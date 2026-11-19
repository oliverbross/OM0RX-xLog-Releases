#if os(macOS) || os(Windows)
import Foundation
import XClusterIntegrationCore

/// Independent implementation of the documented big-endian schema-2/3 fields.
/// WSJT-X starts at schema 2; this receive-only listener never negotiates an upgrade.
/// https://github.com/WSJTX/wsjtx/blob/master/Network/NetworkMessage.hpp
/// No command builder or sending capability exists in this receive model.
/// The station a WSJT-X-compatible engine (WSJT-X, Nexus) is working, from its
/// Status message: who it is calling or answering, and whether it is keyed.
struct WSJTXWorking: Equatable, Sendable {
    var call = ""
    var grid = ""
    var txEnabled = false
    var transmitting = false

    /// A plausible callsign and grid, or empty when the field is free text.
    static func sanitized(call rawCall: String, grid rawGrid: String) -> (String, String) {
        let call = rawCall.trimmingCharacters(in: .whitespaces).uppercased()
        let validCall = (3...32).contains(call.utf8.count)
            && call.utf8.allSatisfy { (65...90).contains($0) || (48...57).contains($0) || $0 == 47 || $0 == 45 }
            && call.utf8.contains { (48...57).contains($0) }
        let grid = rawGrid.trimmingCharacters(in: .whitespaces).uppercased()
        let validGrid = grid.range(of: #"^[A-R]{2}[0-9]{2}([A-X]{2})?$"#, options: .regularExpression) != nil
        return (validCall ? call : "", validGrid ? grid : "")
    }
}

enum WSJTXActivityMessage {
    case heartbeat(String)
    case status(String, UInt64, String, WSJTXWorking)
    case decode(String, Bool, UInt32, UInt32, String, Bool, Bool)
    case close(String)
    case clear(String)

    var instance: String {
        switch self {
        case .heartbeat(let id), .status(let id, _, _, _), .decode(let id, _, _, _, _, _, _), .close(let id), .clear(let id): id
        }
    }

    static func parse(_ data: Data) throws -> Self? {
        var cursor = Reader(data: data)
        guard try cursor.uint32() == 0xadbccbda else { throw WSJTXMessageError.invalid }
        let schema = try cursor.uint32(), type = try cursor.uint32()
        guard [0, 1, 2, 3, 6].contains(type) else { return nil }
        guard [2, 3].contains(schema), data.count <= 16_384 else { throw WSJTXMessageError.invalid }
        let id = try cursor.text(128)
        guard !id.isEmpty, !id.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw WSJTXMessageError.invalid }
        switch type {
        case 0:
            guard try cursor.uint32() >= 3 else { throw WSJTXMessageError.invalid }
            _ = try cursor.text(128); _ = try cursor.text(128)
            return .heartbeat(id)
        case 1:
            let frequency = try cursor.uint64(), mode = try cursor.text(24)
            let dxCall = try cursor.text(32); _ = try cursor.text(32); _ = try cursor.text(24)
            let txEnabled = try cursor.boolean(), transmitting = try cursor.boolean(); _ = try cursor.boolean()
            _ = try cursor.uint32(); _ = try cursor.uint32()
            _ = try cursor.text(32); _ = try cursor.text(12); let dxGrid = try cursor.text(12)
            guard frequency <= 10_000_000_000, !mode.isEmpty, mode.utf8.allSatisfy({ $0 >= 32 && $0 < 127 }) else { throw WSJTXMessageError.invalid }
            let (call, grid) = WSJTXWorking.sanitized(call: dxCall, grid: dxGrid)
            return .status(id, frequency, mode, WSJTXWorking(call: call, grid: call.isEmpty ? "" : grid,
                                                            txEnabled: txEnabled, transmitting: transmitting))
        case 2:
            let new = try cursor.boolean(), time = try cursor.uint32()
            _ = try cursor.uint32() // signed SNR is not needed by this passive projection
            let delta = Double(bitPattern: try cursor.uint64())
            let offset = try cursor.uint32()
            _ = try cursor.text(16) // decode marker, not the operating mode
            let message = try cursor.text(256), low = try cursor.boolean(), offAir = try cursor.boolean()
            guard time < 86_400_000, delta.isFinite, abs(delta) <= 60, offset <= 100_000,
                  !message.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw WSJTXMessageError.invalid }
            return .decode(id, new, time, offset, message, low, offAir)
        case 3: return .clear(id)
        default: return .close(id)
        }
    }

    private struct Reader {
        let data: Data
        var offset = 0
        mutating func uint32() throws -> UInt32 { UInt32(try number(4)) }
        mutating func uint64() throws -> UInt64 { try number(8) }
        mutating func number(_ size: Int) throws -> UInt64 {
            guard offset <= data.count - size else { throw WSJTXMessageError.invalid }
            defer { offset += size }
            return data[offset..<(offset + size)].reduce(0) { ($0 << 8) | UInt64($1) }
        }
        mutating func boolean() throws -> Bool {
            let value = try number(1)
            guard value <= 1 else { throw WSJTXMessageError.invalid }
            return value == 1
        }
        mutating func text(_ maximum: Int) throws -> String {
            let count = try uint32()
            // QDataStream null byte arrays represent absent optional text.
            if count == UInt32.max { return "" }
            guard count <= UInt32(maximum), offset <= data.count - Int(count),
                  let result = String(data: data[offset..<(offset + Int(count))], encoding: .utf8) else { throw WSJTXMessageError.invalid }
            offset += Int(count)
            return result
        }
    }
}

struct WSJTXActivityClassificationInput: Sendable {
    let id: String
    let callsign: String
    let band: String
    let mode: String
    let receivedAt: UInt64
}

struct WSJTXActivityClassification: Sendable {
    let callStatus: String
    let dxccStatus: String
}

struct WSJTXActivityClassificationSnapshot: Sendable {
    let key: String
    let spots: [String: WSJTXActivityClassification]
}

struct WSJTXActivity {
    private struct Session {
        var frequency: UInt64?
        var mode = ""
        var statusAt: UInt64 = 0
        var lastSeen: UInt64
        var working = WSJTXWorking()
        var dialHz: UInt64 = 0
    }
    private struct CQ {
        let id: String
        let session: String
        let callsign: String
        let frequency: Double
        let band: String
        let mode: String
        let comment: String
        let receivedAt: UInt64
    }
    private var sessions: [String: Session] = [:]
    private var spots: [CQ] = []
    private var seen: Set<String> = []
    private var seenOrder: [String] = []
    private(set) var revision: UInt64 = 0
    private(set) var epoch = UUID().uuidString

    mutating func reset() { self = WSJTXActivity() }

    mutating func invalidate(sender: String) {
        let prefix = sender + "\u{1f}"
        sessions = sessions.filter { !$0.key.hasPrefix(prefix) }
        spots.removeAll { $0.session.hasPrefix(prefix) }
        revision += 1
    }

    mutating func expire(now: UInt64) {
        let old = spots.count
        sessions = sessions.filter { now >= $0.value.lastSeen && now - $0.value.lastSeen <= 180_000 }
        spots.removeAll { now < $0.receivedAt || now - $0.receivedAt > 60_000 || sessions[$0.session] == nil }
        if old != spots.count { revision += 1 }
    }

    mutating func receive(_ message: WSJTXActivityMessage, sender: String, now: UInt64) {
        expire(now: now)
        guard sender.utf8.count <= 80, !sender.isEmpty else { return }
        let key = sender + "\u{1f}" + message.instance
        if case .close = message {
            sessions.removeValue(forKey: key); spots.removeAll { $0.session == key }; revision += 1; return
        }
        if case .clear = message {
            spots.removeAll { $0.session == key }; revision += 1; return
        }
        guard sessions[key] != nil || sessions.count < 8 else { return }
        var session = sessions[key] ?? Session(lastSeen: now)
        session.lastSeen = now
        switch message {
        case .status(_, let frequency, let mode, let working):
            if session.frequency != frequency || session.mode != mode { spots.removeAll { $0.session == key } }
            session.frequency = frequency > 0 && ["FT8", "FT4"].contains(mode) ? frequency : nil
            session.mode = mode; session.statusAt = now
            session.working = working; session.dialHz = frequency
        case .decode(_, let new, let time, let offset, let text, let low, let offAir):
            guard new, !low, !offAir, let frequency = session.frequency,
                  now >= session.statusAt, now - session.statusAt <= 120_000,
                  let call = Self.cqCall(text) else { sessions[key] = session; return }
            let day = now / 86_400_000 * 86_400_000
            let today = Int64(day) + Int64(time)
            let candidates: [Int64] = [today, today - 86_400_000, today + 86_400_000]
            let decodeTime = candidates
                .min(by: { abs($0 - Int64(now)) < abs($1 - Int64(now)) })!
            guard abs(decodeTime - Int64(now)) <= 60_000 else { sessions[key] = session; return }
            let identity = key + "\u{1f}\(frequency)\u{1f}\(decodeTime)\u{1f}\(offset)\u{1f}" + text
            guard seen.insert(identity).inserted else { sessions[key] = session; return }
            seenOrder.append(identity)
            if seenOrder.count > 4096 { seen.remove(seenOrder.removeFirst()) }
            let khz = Double(frequency + UInt64(offset)) / 1000
            let band = SpotLogic.band(for: khz)
            guard !band.isEmpty else { sessions[key] = session; return }
            spots.append(CQ(id: UUID().uuidString, session: key, callsign: call, frequency: khz,
                            band: band, mode: session.mode, comment: text, receivedAt: now))
            if spots.count > 200 { spots.removeFirst(spots.count - 200) }
        default: break
        }
        sessions[key] = session; revision += 1
    }

    mutating func snapshot(now: UInt64, available: Bool) -> [String: Any] {
        snapshot(now: now, available: available, classificationRevision: 0, classifications: [:])
    }

    mutating func classificationInputs(now: UInt64) -> [WSJTXActivityClassificationInput] {
        expire(now: now)
        return spots.map { WSJTXActivityClassificationInput(id: $0.id, callsign: $0.callsign,
            band: $0.band, mode: $0.mode, receivedAt: $0.receivedAt) }
    }

    mutating func snapshot(now: UInt64, available: Bool, classificationRevision: UInt64,
                           classifications: [String: WSJTXActivityClassification]) -> [String: Any] {
        expire(now: now)
        return ["epoch": epoch, "revision": revision, "available": available, "received_at_ms": sessions.values.map(\.lastSeen).max() ?? 0,
                "working": working(now: now),
                "classification_revision": classificationRevision, "session_count": sessions.count, "ttl_ms": 60_000,
                "spots": spots.map { spot in
                    let classification = classifications[spot.id]
                    return ["id": spot.id, "callsign": spot.callsign, "frequencyKHz": spot.frequency,
                    "band": spot.band, "mode": spot.mode, "comment": spot.comment,
                    "callStatus": classification?.callStatus ?? "Unknown",
                    "dxccStatus": classification?.dxccStatus ?? "Unknown", "specialTags": [String](),
                    "receivedAtMs": spot.receivedAt] as [String: Any]
                }]
    }

    /// The most recently reported station being worked, from a Status seen in
    /// the last 60 s, or NSNull when no engine is working anyone.
    private func working(now: UInt64) -> Any {
        guard let (key, session) = sessions.filter({ !$0.value.working.call.isEmpty && now >= $0.value.statusAt
                && now - $0.value.statusAt <= 60_000 }).max(by: { $0.value.statusAt < $1.value.statusAt }) else { return NSNull() }
        let parts = key.split(separator: "\u{1f}", maxSplits: 1).map(String.init)
        return ["call": session.working.call, "grid": session.working.grid, "tx_enabled": session.working.txEnabled,
                "transmitting": session.working.transmitting, "dial_hz": session.dialHz, "mode": session.mode,
                "sender": parts.first ?? "", "instance": parts.count > 1 ? parts[1] : "", "updated_ms": session.statusAt] as [String: Any]
    }

    private static func cqCall(_ message: String) -> String? {
        let tokens = message.uppercased().split(separator: " ", omittingEmptySubsequences: true).map(String.init)
        guard tokens.first == "CQ", (2...4).contains(tokens.count) else { return nil }
        // Accept CQ CALL [GRID] and CQ qualifier CALL [GRID], never infer a call from free text.
        let position = tokens.count >= 3 && !validCall(tokens[1]) ? 2 : 1
        guard position < tokens.count, validCall(tokens[position]),
              tokens.count == position + 1 || tokens.count == position + 2 && tokens.last!.range(of: "^[A-R]{2}[0-9]{2}([A-X]{2})?$", options: .regularExpression) != nil else { return nil }
        return tokens[position]
    }
    private static func validCall(_ value: String) -> Bool {
        value.utf8.count <= 24 && value.range(of: "^(?:[A-Z0-9]{1,4}/)?[A-Z0-9]{1,4}[0-9][A-Z]{1,4}(?:/[A-Z0-9]{1,4})?$", options: .regularExpression) != nil
    }
}
#endif
