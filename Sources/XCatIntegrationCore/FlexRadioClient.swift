#if canImport(Network)
@preconcurrency import Network
private typealias FlexConnection = NWConnection
#elseif os(Windows)
private typealias FlexConnection = WindowsTCPConnection
#endif
import Foundation

enum FlexCommandResponseParser {
    struct Response: Equatable {
        let sequence: Int
        let code: UInt32
        let message: String
    }

    static func parse(_ line: String) -> Response? {
        guard line.first == "R" else { return nil }
        let fields = line.dropFirst().split(separator: "|", maxSplits: 3, omittingEmptySubsequences: false)
        guard fields.count >= 2,
              let sequence = Int(fields[0]),
              let code = UInt32(fields[1], radix: 16) else { return nil }
        return Response(sequence: sequence, code: code, message: fields.count >= 3 ? String(fields[2]) : "")
    }
}

/// A transmitter status is distinct from a successful command response. Only
/// this field can advance the RF-power readback revision.
enum FlexRFPowerStatusParser {
    static func parse(_ line: String) -> Int? {
        guard line.first == "S", let separator = line.firstIndex(of: "|") else { return nil }
        let fields = line[line.index(after: separator)...].split(separator: " ")
        guard fields.first == "transmit" else { return nil }
        let matches = fields.dropFirst().filter { $0.hasPrefix("rfpower=") }
        guard matches.count == 1 else { return nil }
        let raw = matches[0].dropFirst("rfpower=".count)
        guard !raw.isEmpty, raw.utf8.allSatisfy({ (48...57).contains($0) }),
              let watts = Int(raw), (0...100).contains(watts) else { return nil }
        return watts
    }
}

public actor FlexRadioClient: RadioControlling {
    public typealias StateHandler = @Sendable (RadioConnectionState, String) -> Void
    public typealias SnapshotHandler = @Sendable (RadioSnapshot) -> Void

    public struct TransmitObservation: Sendable {
        public let revision: UInt64
        public let isTransmitting: Bool
        public let isConnected: Bool

        public init(revision: UInt64, isTransmitting: Bool, isConnected: Bool) {
            self.revision = revision
            self.isTransmitting = isTransmitting
            self.isConnected = isConnected
        }
    }

    public struct RFPowerObservation: Sendable {
        public let revision: UInt64
        public let level: Int
        public let isConnected: Bool

        public init(revision: UInt64, level: Int, isConnected: Bool) {
            self.revision = revision
            self.level = level
            self.isConnected = isConnected
        }
    }

    private struct SliceState: Sendable {
        var id: Int
        var frequencyMHz: Double?
        var mode: String?
        var clientHandle: String?
        var indexLetter: String?
        var daxChannel: Int?
        var active = false
        var transmit = false
        var inUse = true
    }

    private struct PendingCommand {
        let continuation: CheckedContinuation<String, any Error>
        let timeoutTask: Task<Void, Never>
    }

    private let host: String
    private let port: UInt16
    private let stationName: String
    private let sliceLetter: String
    private let wanValidationHandle: String?
    private let certificateIdentity: String?
    private let onState: StateHandler
    private let onSnapshot: SnapshotHandler
    private let queue = DispatchQueue(label: "com.oliverbross.OM0RXXCat.radio")

    private var connection: FlexConnection?
    private let digitalPTTOwner = DigitalPTTPermit.OwnerIdentity()
    private var generation = 0
    private var shouldRun = false
    private var reconnectAttempt = 0
    private var reconnectTask: Task<Void, Never>?
    private var lastHeartbeat: Date?
    public func confirmedHeartbeat() -> Date? { state == .ready && value.isConnected ? lastHeartbeat : nil }
    private var pingTask: Task<Void, Never>?
    private var inboundBuffer = Data()
    private var commandSequence = 1
    private var cwx = FlexCWX()
    private var state: RadioConnectionState = .stopped
    private var value: RadioSnapshot
    private var interlockKnown = false
    private var interlockObservationRevision: UInt64 = 0
    private var rfPowerKnown = false
    private var rfPowerObservationRevision: UInt64 = 0
    private var slices: [Int: SliceState] = [:]
    private var selectedSliceID: Int?
    private var followedClientHandle: String?
    private var followedClientID: String?
    private var clientHandleReceived = false
    private var transportReady = false
    private var pendingCommands: [Int: PendingCommand] = [:]
    private var wordsPerMinute = 25
    private var stationsByHandle: [String: String] = [:]
    private var clientIDsByHandle: [String: String] = [:]
    private var reportedClientHandles: Set<String> = []
    private var reportedSliceIDs: Set<Int> = []
    private var ownClientHandle:UInt32?

    public init(
        host: String,
        port: UInt16 = 4992,
        stationName: String,
        sliceLetter: String = "A",
        wanValidationHandle: String? = nil,
        certificateIdentity: String? = nil,
        onState: @escaping StateHandler = { _, _ in },
        onSnapshot: @escaping SnapshotHandler = { _ in }
    ) {
        self.host = host
        self.port = port
        self.stationName = stationName
        self.sliceLetter = sliceLetter.uppercased()
        self.wanValidationHandle = wanValidationHandle
        self.certificateIdentity = certificateIdentity
        self.onState = onState
        self.onSnapshot = onSnapshot
        value = RadioSnapshot(isConnected: false, slice: sliceLetter.uppercased(), station: stationName)
    }

    public func start() {
        guard !shouldRun else { return }
        shouldRun = true
        reconnectAttempt = 0
        connect()
    }

    public func stop() async {
        shouldRun = false
        reconnectTask?.cancel()
        reconnectTask = nil
        pingTask?.cancel()
        pingTask = nil
        if transportReady {
            try? await sendCommand("transmit tune 0", timeout: .seconds(1))
            try? await sendCommand("cwx clear", timeout: .seconds(1))
            try? await sendCommand("xmit 0", timeout: .seconds(1))
        }
        await stopTransportOnly()
    }

    /// Close this client without changing the radio's transmit state. Owners
    /// with explicit RF obligations release and observe those before calling it.
    public func stopTransportOnly() async {
        cwx.retire()
        cwx.invalidate("CWX connection closed; pending text was retired.")
        shouldRun = false
        reconnectTask?.cancel()
        reconnectTask = nil
        pingTask?.cancel()
        pingTask = nil
        generation += 1
        connection?.cancel()
        connection = nil
        failAllPending(XCatError.notConnected)
        inboundBuffer.removeAll(keepingCapacity: false)
        interlockKnown = false
        rfPowerKnown = false
        value.rfPowerLevel = nil
        value.isConnected = false
        publishSnapshot()
        transition(.stopped, "Stopped")
    }

    public func reconnectNow() {
        guard shouldRun else {
            start()
            return
        }
        reconnectAttempt = 0
        generation += 1
        connection?.cancel()
        connection = nil
        // Explicit reconnect must withdraw the old session's apparent live
        // readback before any new transport or slice is discovered.
        value.isConnected = false
        rfPowerKnown = false
        value.rfPowerLevel = nil
        publishSnapshot()
        connect()
    }

    public func snapshot() -> RadioSnapshot {
        value
    }

    /// Only an interlock status, never an unrelated slice update, can prove
    /// whether FLEX transmit is on or off for this transport session.
    public func transmitObservation() -> TransmitObservation? {
        guard interlockKnown, state == .ready else { return nil }
        return TransmitObservation(revision: interlockObservationRevision,
                                   isTransmitting: value.isTransmitting,
                                   isConnected: value.isConnected)
    }

    public func rfPowerObservation() -> RFPowerObservation? {
        guard rfPowerKnown, state == .ready, let level = value.rfPowerLevel else { return nil }
        return RFPowerObservation(revision: rfPowerObservationRevision, level: level,
                                  isConnected: value.isConnected)
    }

    public func primaryFrequency() -> Int64? {
        guard let frequencyMHz = stationSlice(letter: "A")?.frequencyMHz else { return nil }
        return Int64((frequencyMHz * 1_000_000).rounded())
    }

    public func setPrimaryFrequency(_ frequencyHz: Int64) async throws {
        guard frequencyHz > 0 else { throw XCatError.invalidFrequency }
        guard let slice = stationSlice(letter: "A") else { throw XCatError.notConnected }
        try await tuneSlice(slice.id, frequencyHz: frequencyHz)
    }

    /// Set the mode on the station-owned VFO A slice. Unlike `setMode`, this
    /// never follows the mutable selected slice and is therefore suitable for
    /// a command admitted against an explicit VFO-A observation.
    public func setPrimaryMode(_ mode: String) async throws {
        try await setSliceMode(mode, letter: "A")
    }
    /// Exact station-owned logical slice, independent of GUI active selection.
    public func setSliceMode(_ mode: String, letter: String) async throws {
        guard ["A","B"].contains(letter) else { throw XCatError.unsupported }
        let normalized = try Self.validatedMode(mode)
        guard let slice = stationSlice(letter: letter) else { throw XCatError.notConnected }
        try await sendCommand("slice s \(slice.id) mode=\(normalized)")
    }

    public func setFrequency(_ frequencyHz: Int64) async throws {
        guard frequencyHz > 0 else { throw XCatError.invalidFrequency }
        guard let selectedSliceID else { throw XCatError.notConnected }
        try await tuneSlice(selectedSliceID, frequencyHz: frequencyHz)
    }

    public func setMode(_ mode: String) async throws {
        let normalized = try Self.validatedMode(mode)
        guard let selectedSliceID else { throw XCatError.notConnected }
        try await sendCommand("slice s \(selectedSliceID) mode=\(normalized)")
    }

    public func secondaryFrequency() -> Int64? {
        guard let frequencyMHz = stationSlice(letter: "B")?.frequencyMHz else { return nil }
        return Int64((frequencyMHz * 1_000_000).rounded())
    }

    public func setSecondaryFrequency(_ frequencyHz: Int64) async throws {
        guard frequencyHz > 0 else { throw XCatError.invalidFrequency }
        guard let slice = stationSlice(letter: "B") else { throw XCatError.unsupported }
        try await tuneSlice(slice.id, frequencyHz: frequencyHz)
    }

    private func tuneSlice(_ id: Int, frequencyHz: Int64) async throws {
        let session = generation
        try await sendCommand("slice t \(id) \(Self.formatMHz(frequencyHz))")
        guard generation == session else { throw XCatError.notConnected }
        // The FLEX-8400 can acknowledge a tune without echoing its changed
        // frequency to the originating client. Request authoritative readback.
        try await sendCommand("sub slice \(id)")
    }

    public func setSplitTransmit(_ enabled: Bool) async throws {
        let letter = enabled ? "B" : "A"
        guard let slice = stationSlice(letter: letter) else { throw XCatError.unsupported }
        try await sendCommand("slice s \(slice.id) tx=1")
    }

    public func setReceiveVFO(_ vfo: Int) async throws {
        guard vfo == 0 || vfo == 1 else { throw XCatError.unsupported }
        let letter = vfo == 0 ? "A" : "B"
        guard let slice = stationSlice(letter: letter) else { throw XCatError.unsupported }
        try await sendCommand("slice s \(slice.id) active=1")
    }

    public func setTransmit(_ enabled: Bool) async throws {
        guard transportReady else { throw XCatError.notConnected }
        if enabled {
            guard state == .ready, value.isConnected, selectedSliceID != nil else { throw XCatError.notConnected }
        }
        if enabled {
            try await sendCommand("xmit 1")
            return
        }
        // Both releases are safety-critical. A failed tune-off reply must not
        // prevent the independent PTT-off command from being attempted.
        async let pttOff: Void = sendCommand("xmit 0")
        var releaseError: Error?
        do { try await sendCommand("transmit tune 0") } catch { releaseError = error }
        do { try await pttOff } catch {
            if releaseError == nil { releaseError = error }
        }
        if let releaseError { throw releaseError }
    }

    public nonisolated var supportsDigitalPTTLease: Bool { true }
    public func makeDigitalPTTPermit(deadline: ContinuousClock.Instant) throws -> DigitalPTTPermit {
        guard transportReady, state == .ready, value.isConnected else { throw XCatError.notConnected }
        return try DigitalPTTPermit(owner: digitalPTTOwner, connectionGeneration: generation, deadline: deadline)
    }
    /// The returned baseline is captured on the same actor immediately before
    /// send initiation. OFF does not wait for tune-off, an ON ACK or an owner RPC.
    public func setDigitalPTT(_ enabled: Bool, permit: DigitalPTTPermit) async throws -> UInt64 {
        try permit.checkRelease(owner: digitalPTTOwner, connectionGeneration: generation)
        let before = interlockObservationRevision
        _ = try await sendReply(enabled ? "xmit 1" : "xmit 0", permit: enabled ? permit : nil)
        try permit.checkRelease(owner: digitalPTTOwner, connectionGeneration: generation)
        return before
    }

    public func setRFPower(_ watts: Int) async throws {
        guard (0...100).contains(watts) else {
            throw XCatError.server("RF power must be 0 through 100.")
        }
        guard state == .ready, value.isConnected else { throw XCatError.notConnected }
        try await sendCommand("transmit set rfpower=\(watts)")
    }

    public func cwSpeed() -> Int { wordsPerMinute }

    public func setCWSpeed(_ wordsPerMinute: Int) async throws {
        guard (5...100).contains(wordsPerMinute) else { throw XCatError.server("CW speed must be 5 through 100 WPM.") }
        try await sendCommand("cw wpm \(wordsPerMinute)")
        self.wordsPerMinute = wordsPerMinute
    }

    public func sendCW(_ text: String) async throws {
        let safeText = text.unicodeScalars.map { scalar -> Character in
            (scalar.value >= 0x20 && scalar.value <= 0x7E && scalar != "|") ? Character(scalar) : " "
        }
        let sanitized = String(safeText)
        guard !sanitized.trimmingCharacters(in: .whitespaces).isEmpty, sanitized.count <= 128 else {
            throw XCatError.server("CW text must contain 1 through 128 printable characters.")
        }
        // This retained whole-message path has no indexed allocation. Its late
        // status must never be credited to a later live composition on this TCP
        // session, particularly on firmware that reuses indices after clear.
        cwx.invalidate("Unindexed whole-message CWX was used on this connection. Reconnect before live typing; whole-message Send remains available.")
        try await sendCommand("cwx send \(sanitized)")
    }

    public func clearCW() async throws {
        cwx.retire()
        try await sendCommand("cwx clear")
        try? await setTransmit(false)
    }

    public func compositionSnapshot() -> FlexCWXSnapshot { cwx.snapshot }
    public func retireCWComposition(generation expected: Int? = nil, job: String? = nil) -> Bool {
        guard expected == nil || expected == generation else { return false }
        cwx.retire(job: job); return true
    }
    public func clearCWComposition(generation expected: Int) async throws {
        guard expected == generation else { throw XCatError.notConnected }
        try await clearCW()
    }

    public func appendCW(generation expected: Int, job: String, ordinal: Int, text: String, finish: Bool) async throws -> FlexCWXSnapshot {
        guard state == .ready, transportReady, expected == generation else { throw XCatError.notConnected }
        guard let append = try cwx.prepare(generation: expected, job: job, ordinal: ordinal, text: text, finish: finish) else { return cwx.snapshot }
        do {
            let reply = try await sendReply(append.command)
            try cwx.acknowledge(append, reply: reply)
            return cwx.snapshot
        } catch {
            // Never requeue an uncertain command, including after Stop/reconnect.
            if generation == expected, cwx.snapshot.job == job, cwx.snapshot.available { cwx.invalidate("CWX append outcome is uncertain. Use Stop; do not retry.") }
            throw error
        }
    }

    public func addSpot(_ spot: FlexSpot) async throws {
        try await sendCommand("spot add " + Self.spotFields(spot, source: spot.source))
    }

    private static func spotFields(_ spot: FlexSpot, source: String) -> String {
        func encoded(_ value: String) -> String { value.replacingOccurrences(of: " ", with: "\u{7F}") }
        let frequency = String(format: "%.6f", Double(spot.frequencyHz) / 1_000_000)
        return [
            "rx_freq=\(frequency)",
            "callsign=\(encoded(spot.callsign))",
            "mode=\(encoded(spot.mode))",
            "source=\(encoded(source))",
            "spotter_callsign=\(encoded(spot.spotter))",
            "timestamp=\(spot.timestamp)",
            "lifetime_seconds=\(spot.lifetimeSeconds)",
            "priority=\(spot.priority)",
            "comment=\(encoded(spot.comment))",
            "color=\(spot.colorHex)",
        ].joined(separator: " ")
    }

    #if os(Windows)
    private struct SpotIdentity: Equatable {
        let callsign: String
        let hz: Int64
    }
    private var spotSource: String?
    private var ownedSpots: [Int: SpotIdentity] = [:]
    private var spotCommandActive = false

    /// The worker supplies the durable publisher ID. Re-subscribing on a new
    /// transport rebuilds indexes from this radio's current ownership reports.
    public func publishSpot(_ spot: FlexSpot, source: String, prior: Bool,
                            beforeUpdate: @escaping @Sendable () async throws -> Void) async throws {
        try await changeSpot(callsign: spot.callsign, frequencyHz: spot.frequencyHz, source: source, prior: prior, spot: spot, beforeUpdate: beforeUpdate)
    }

    public func withdrawSpot(callsign: String, frequencyHz: Int64, source: String, prior: Bool) async throws {
        try await changeSpot(callsign: callsign, frequencyHz: frequencyHz, source: source, prior: prior, spot: nil)
    }

    private func changeSpot(callsign: String, frequencyHz: Int64, source: String, prior: Bool, spot: FlexSpot?,
                            beforeUpdate: (@Sendable () async throws -> Void)? = nil) async throws {
        guard state == .ready, value.isConnected, !spotCommandActive else { throw XCatError.notConnected }
        spotCommandActive = true
        defer { spotCommandActive = false }
        let session = generation
        if spotSource != source {
            spotSource = source
            ownedSpots.removeAll()
            do { try await sendCommand("sub spot all") }
            catch { if generation == session { spotSource = nil }; throw error }
        }
        guard generation == session, state == .ready else { throw XCatError.notConnected }
        let identity = SpotIdentity(callsign: callsign, hz: frequencyHz)
        // SmartSDR can report a spot a few Hz away after converting MHz to
        // single precision. Carry the exact requested Hz across reconnects.
        let taggedSource = "\(source)-\(frequencyHz)"
        // The subscription ACK can precede the inventory by many packets.
        // A retained attempt requires positive observation before index use.
        if prior {
            _ = try await waitForSpot(identity, session: session)
        }
        let matches = ownedSpots.filter { $0.value == identity }
        guard matches.count <= 1 else { throw XCatError.server("Ambiguous FLEX spot ownership.") }
        if let (index, _) = matches.first {
            if let spot {
                try await beforeUpdate?()
                guard generation == session, ownedSpots[index] == identity else { throw XCatError.notConnected }
                try await sendCommand("spot set \(index) " + Self.spotFields(spot, source: taggedSource))
            } else {
                try await sendCommand("spot remove \(index)")
                let deadline = ContinuousClock.now + .seconds(4)
                while ownedSpots[index] == identity {
                    guard generation == session, ContinuousClock.now < deadline else { throw XCatError.notConnected }
                    try await Task.sleep(for: .milliseconds(20))
                }
            }
        } else if let spot {
            let reply = try await sendReply("spot add " + Self.spotFields(spot, source: taggedSource))
            guard let id = Int(reply), id >= 0, try await waitForSpot(identity, session: session) == id else { throw XCatError.server("Missing FLEX spot index.") }
        }
        guard generation == session else { throw XCatError.notConnected }
    }

    private func waitForSpot(_ identity: SpotIdentity, session: Int) async throws -> Int {
        let deadline = ContinuousClock.now + .seconds(4)
        while generation == session && ContinuousClock.now < deadline {
            if let entry = ownedSpots.first(where: { $0.value == identity }) { return entry.key }
            try await Task.sleep(for: .milliseconds(20))
        }
        throw XCatError.server("The radio has not confirmed this publisher’s spot.")
    }

    private func recordSpot(_ index: String?, properties: [String: String], removed: Bool) {
        guard let raw = index, let id = Int(raw), id >= 0 else { return }
        if removed { ownedSpots.removeValue(forKey: id); return }
        guard let source = properties["source"] else { return }
        guard let spotSource, source.hasPrefix(spotSource + "-"),
              let hz = Int64(source.dropFirst(spotSource.count + 1)), (100_000...60_000_000).contains(hz),
              let call = properties["callsign"], let mhz = properties["rx_freq"].flatMap(Double.init),
              abs(mhz * 1_000_000 - Double(hz)) <= 4 else {
            ownedSpots.removeValue(forKey: id)
            return
        }
        ownedSpots[id] = SpotIdentity(callsign: call.uppercased(), hz: hz)
    }
    #endif

    public func setTransmit(_ mode: CATTransmitMode) async throws {
        guard state == .ready, value.isConnected, selectedSliceID != nil else { throw XCatError.notConnected }
        switch mode {
        case .microphone, .data:
            try await setTransmit(true)
        case .tune:
            try await sendCommand("transmit tune 1")
        }
    }

    private func connect() {
        guard shouldRun, connection == nil else { return }
        generation += 1
        let currentGeneration = generation
        resetSessionState()
        transition(.connecting, "Connecting to \(host):\(port)")

        #if os(Windows)
        guard wanValidationHandle == nil, port != 0 else {
            transition(.failed, "Direct LAN address and port required")
            return
        }
        let newConnection = WindowsTCPConnection(host: host, port: port, queue: queue)
        #else
        guard let endpointPort = NWEndpoint.Port(rawValue: port) else {
            transition(.failed, "Invalid radio port")
            return
        }
        let parameters: NWParameters
        if wanValidationHandle != nil {
            let tls = NWProtocolTLS.Options()
            let pinStore = SmartLinkCertificatePinStore()
            let pinIdentity = certificateIdentity ?? host
            sec_protocol_options_set_verify_block(tls.securityProtocolOptions, { _, trust, complete in
                let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
                guard let certificates = SecTrustCopyCertificateChain(secTrust) as? [SecCertificate],
                      let certificate = certificates.first else {
                    complete(false)
                    return
                }
                let data = SecCertificateCopyData(certificate) as Data
                complete(pinStore.verifyOrTrustFirstUse(identity: pinIdentity, certificateData: data))
            }, queue)
            parameters = NWParameters(tls: tls, tcp: NWProtocolTCP.Options())
        } else {
            parameters = .tcp
        }
        let newConnection = NWConnection(host: NWEndpoint.Host(host), port: endpointPort, using: parameters)
        #endif
        connection = newConnection
        newConnection.stateUpdateHandler = { [weak self] networkState in
            Task { await self?.handle(networkState, generation: currentGeneration) }
        }
        #if os(Windows)
        newConnection.start()
        #else
        newConnection.start(queue: queue)
        #endif
    }

    private func handle(_ networkState: FlexConnection.State, generation currentGeneration: Int) async {
        guard currentGeneration == generation else { return }
        switch networkState {
        case .ready:
            transportReady = true
            transition(.connecting, wanValidationHandle == nil ? "Connected; starting radio session" : "Secure transport connected; validating SmartLink session")
            receive(generation: currentGeneration)
            do {
                if let wanValidationHandle {
                    try await sendCommand("wan validate handle=\(wanValidationHandle)")
                    try await waitForWANValidation()
                }
                try await bootstrap()
                guard currentGeneration == generation else { return }
                transition(.ready, "Connected; waiting for \(stationName) slice \(sliceLetter)")
                // Initial slice, power and interlock reports may arrive while
                // bootstrap is still waiting for the final command reply.
                publishSnapshot()
                startPingLoop(generation: currentGeneration)
                // Optional advanced CW capability must not disable ordinary CAT
                // or delay readiness/whole-message Send on older firmware.
                do {
                    try await sendCommand("sub cwx all")
                    guard currentGeneration == generation else { return }
                    cwx.subscriptionAccepted()
                } catch { /* CWX live append remains unavailable. */ }
            } catch {
                await connectionFailed("Bootstrap failed: \(error.localizedDescription)", generation: currentGeneration)
            }
        #if !os(Windows)
        case let .waiting(error):
            await connectionFailed("Network waiting: \(error.localizedDescription)", generation: currentGeneration)
        #endif
        case let .failed(error):
            await connectionFailed("Connection failed: \(error.localizedDescription)", generation: currentGeneration)
        #if !os(Windows)
        case .cancelled:
            if shouldRun, currentGeneration == generation {
                await connectionFailed("Connection closed", generation: currentGeneration)
            }
        default:
            break
        #endif
        }
    }

    private func bootstrap() async throws {
        for command in [
            "sub client all",
            "sub slice all",
            "sub radio all",
            "sub tx all",
            "keepalive enable",
            "slice list",
            "ping",
        ] {
            do {
                try await sendCommand(command)
            } catch {
                throw XCatError.server("\(command): \(error.localizedDescription)")
            }
        }
    }

    private func startPingLoop(generation currentGeneration: Int) {
        pingTask?.cancel()
        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled else { return }
                do {
                    try await self?.confirmHeartbeat(generation: currentGeneration)
                } catch {
                    await self?.connectionFailed("Radio heartbeat failed", generation: currentGeneration)
                    return
                }
            }
        }
    }

    private func confirmHeartbeat(generation expected: Int) async throws {
        try await sendCommand("ping")
        if generation == expected && transportReady { lastHeartbeat = Date() }
    }

    private func receive(generation currentGeneration: Int) {
        guard currentGeneration == generation, let connection else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16_384) { [weak self] data, _, complete, error in
            Task {
                guard let self else { return }
                if let data, !data.isEmpty {
                    await self.consume(data, generation: currentGeneration)
                }
                if complete || error != nil {
                    await self.connectionFailed(
                        error.map { "Receive failed: \($0.localizedDescription)" } ?? "Radio closed the connection",
                        generation: currentGeneration
                    )
                } else {
                    await self.receive(generation: currentGeneration)
                }
            }
        }
    }

    private func consume(_ data: Data, generation currentGeneration: Int) {
        guard currentGeneration == generation else { return }
        inboundBuffer.append(data)
        if inboundBuffer.count > 1_048_576 {
            inboundBuffer.removeAll(keepingCapacity: true)
            return
        }
        while let newline = inboundBuffer.firstIndex(of: 0x0A) {
            let lineData = inboundBuffer[..<newline]
            inboundBuffer.removeSubrange(...newline)
            guard let line = String(data: lineData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !line.isEmpty else { continue }
            parse(line)
        }
    }

    private func parse(_ line: String) {
        if line.first == "R" {
            completeCommand(from: line)
            return
        }
        if line.first == "H" {
            clientHandleReceived = true
            ownClientHandle=Self.protocolHandle(String(line.dropFirst()))
            cwx.identify(ownClientHandle)
            return
        }
        guard line.first == "S" else { return }
        let payload = line.dropFirst()
        let statusParts = payload.split(separator: "|", maxSplits: 1, omittingEmptySubsequences: false)
        guard statusParts.count == 2 else { return }
        let body = String(statusParts[1])
        cwx.status(handle: Self.protocolHandle(String(statusParts[0])), body: body, generation: generation)
        let tokens = body.split(separator: " ")
        guard let object = tokens.first else { return }

        var identifier: String?
        var propertyStart = 1
        if tokens.count > 1, !tokens[1].contains("=") {
            identifier = String(tokens[1])
            propertyStart = 2
        }
        var properties: [String: String] = [:]
        for token in tokens.dropFirst(propertyStart) {
            let pair = token.split(separator: "=", maxSplits: 1).map(String.init)
            if pair.count == 2 { properties[pair[0]] = pair[1] }
        }

        switch object {
        #if os(Windows)
        case "spot":
            recordSpot(identifier, properties: properties, removed: tokens.dropFirst(2).first == "removed")
        #endif
        case "client", "gui_client":
            applyClient(identifier: identifier, properties: properties)
        case "slice":
            applySlice(identifier: identifier, properties: properties)
        case "interlock":
            if let interlock = properties["state"]?.uppercased() {
                // Transitional, fault, and unfamiliar values cannot prove an
                // RF-off state. Only explicit settled RX or keyed TX does.
                switch interlock {
                case "TRANSMITTING":
                    value.isTransmitting = true
                    interlockKnown = true
                    interlockObservationRevision &+= 1
                case "PTT_REQUESTED":
                    value.isTransmitting = true
                    interlockKnown = false
                case "RECEIVE", "READY":
                    value.isTransmitting = false
                    interlockKnown = true
                    interlockObservationRevision &+= 1
                default:
                    interlockKnown = false
                }
                publishSnapshot()
            }
        case "transmit":
            if let watts = FlexRFPowerStatusParser.parse(line) {
                value.rfPowerLevel = watts
                rfPowerKnown = true
                rfPowerObservationRevision &+= 1
                publishSnapshot()
            }
        case "cwx":
            if let speed = properties["wpm"].flatMap(Int.init), (5...100).contains(speed) {
                wordsPerMinute = speed
            }
        default:
            break
        }
    }

    static func protocolHandle(_ text: String) -> UInt32? {
        let value = text.lowercased().hasPrefix("0x") ? String(text.dropFirst(2)) : text
        guard (1...8).contains(value.utf8.count), value.utf8.allSatisfy({
            (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
        }) else { return nil }
        return UInt32(value, radix: 16)
    }

    private func applyClient(identifier: String?, properties: [String: String]) {
        if let handle = Self.normalizedHandle(identifier ?? properties["handle"]),
           reportedClientHandles.insert(handle).inserted {
            let observedStation = properties["station"].map(Self.displayStation) ?? "unnamed"
            let program = properties["program"] ?? "unknown program"
            onState(.connecting, "Found station \(observedStation) (\(program))")
        }
        guard let handle = Self.normalizedHandle(identifier ?? properties["handle"]) else { return }
        if properties.keys.contains("removed") {
            stationsByHandle.removeValue(forKey: handle)
            clientIDsByHandle.removeValue(forKey: handle)
            if handle == followedClientHandle {
                followedClientHandle = nil
                followedClientID = nil
                selectedSliceID = nil
                value.isConnected = false
                value.vfos = []
            }
        } else {
            if let raw = properties["station"] {
                let name = Self.displayStation(raw)
                if !name.isEmpty && name.utf8.count <= 64 && name.utf8.allSatisfy({ (32...126).contains($0) && $0 != 124 }) {
                    stationsByHandle[handle] = name
                }
            }
            if let id = properties["client_id"] { clientIDsByHandle[handle] = id }
        }
        value.availableStations = Array(Set(stationsByHandle.values)).sorted()
        publishSnapshot()
        selectBestSlice()
    }

    private func followStation(_ handle: String) {
        let newClientID = clientIDsByHandle[handle]
        let shouldBind = newClientID != nil && newClientID != followedClientID
        followedClientHandle = handle
        followedClientID = newClientID
        if shouldBind, let followedClientID {
            let bindingGeneration=generation
            Task {
                do {
                    try await sendCommand("client bind client_id=\(followedClientID)")
                    guard generation==bindingGeneration,self.followedClientID==followedClientID else{return}
                } catch { /* CAT observation remains available while Station binding fails. */ }
            }
        }
    }

    private func applySlice(identifier: String?, properties: [String: String]) {
        guard let identifier, let id = Int(identifier) else { return }
        if properties["removed"] == "1" || properties.keys.contains("removed") {
            slices.removeValue(forKey: id)
            if selectedSliceID == id { selectedSliceID = nil }
            selectBestSlice()
            return
        }
        var slice = slices[id] ?? SliceState(id: id)
        if let frequency = properties["RF_frequency"].flatMap(Double.init) ?? properties["freq"].flatMap(Double.init),
           frequency.isFinite, (0.1...1_000).contains(frequency) {
            slice.frequencyMHz = frequency
        }
        if let mode = properties["mode"] { slice.mode = mode.uppercased() }
        if let handle = properties["client_handle"] { slice.clientHandle = Self.normalizedHandle(handle) }
        if let letter = properties["index_letter"] { slice.indexLetter = letter.uppercased() }
        if let dax = properties["dax"] { slice.daxChannel = FlexDAXAssignment.parse(dax) }
        if let active = properties["active"] { slice.active = active == "1" || active.uppercased() == "T" }
        if let transmit = properties["tx"] { slice.transmit = transmit == "1" || transmit.uppercased() == "T" }
        if let inUse = properties["in_use"] { slice.inUse = inUse == "1" || inUse.uppercased() == "T" }
        slices[id] = slice
        if reportedSliceIDs.insert(id).inserted {
            let letter = slice.indexLetter ?? "?"
            let frequency = slice.frequencyMHz.map { String(format: "%.6f MHz", $0) } ?? "unknown frequency"
            let roles = [slice.active ? "RX" : nil, slice.transmit ? "TX" : nil].compactMap { $0 }.joined(separator: "+")
            onState(.ready, "Found VFO \(letter) at \(frequency)\(roles.isEmpty ? "" : " (\(roles))")")
        }
        selectBestSlice()
    }

    private func selectBestSlice() {
        let automatic = stationName.isEmpty || (stationName == "OM0RX-xLog" && !stationsByHandle.values.contains(stationName))
        let handles = stationsByHandle.keys.filter { handle in
            if !automatic { return Self.normalizedStation(stationsByHandle[handle]) == Self.normalizedStation(stationName) }
            return slices.values.contains { $0.clientHandle == handle && $0.inUse && $0.frequencyMHz != nil }
        }.sorted()
        guard let handle = followedClientHandle.flatMap({ handles.contains($0) ? $0 : nil }) ?? handles.first else {
            if value.isConnected { value.isConnected = false; value.vfos = []; publishSnapshot() }
            return
        }
        followStation(handle)
        let followedClientHandle = handle
        let candidates = slices.values.filter { slice in
            slice.clientHandle == followedClientHandle && slice.inUse && slice.frequencyMHz != nil
        }
        let selected = candidates.sorted { lhs, rhs in
            if lhs.active != rhs.active { return lhs.active }
            if lhs.indexLetter == sliceLetter, rhs.indexLetter != sliceLetter { return true }
            if lhs.indexLetter != sliceLetter, rhs.indexLetter == sliceLetter { return false }
            if lhs.transmit != rhs.transmit { return lhs.transmit }
            return lhs.id < rhs.id
        }.first
        guard let selected, let frequencyMHz = selected.frequencyMHz else {
            selectedSliceID = nil
            if value.isConnected {
                value.isConnected = false
                publishSnapshot()
            }
            return
        }
        selectedSliceID = selected.id
        value.frequencyHz = Int64((frequencyMHz * 1_000_000).rounded())
        if let mode = selected.mode { value.mode = mode }
        value.slice = selected.indexLetter ?? sliceLetter
        let transmit = candidates.filter(\.transmit).sorted { lhs, rhs in
            if lhs.indexLetter != rhs.indexLetter {
                return (lhs.indexLetter ?? "~") < (rhs.indexLetter ?? "~")
            }
            return lhs.id < rhs.id
        }.first ?? selected
        value.transmitSlice = transmit.indexLetter ?? value.slice
        value.transmitFrequencyHz = transmit.frequencyMHz
            .map { Int64(($0 * 1_000_000).rounded()) }
            ?? value.frequencyHz
        value.vfos = candidates.compactMap { slice in
            guard let frequencyMHz = slice.frequencyMHz,
                  let letter = slice.indexLetter else { return nil }
            return RadioVFO(
                letter: letter,
                frequencyHz: Int64((frequencyMHz * 1_000_000).rounded()),
                mode: slice.mode ?? "?",
                isReceive: slice.active,
                isTransmit: slice.transmit,
                flexSliceID: slice.id,
                flexClientHandle: slice.clientHandle,
                daxChannel: slice.daxChannel
            )
        }.sorted { $0.letter < $1.letter }
        value.station = stationsByHandle[handle] ?? stationName
        value.isConnected = true
        reconnectAttempt = 0
        publishSnapshot()
    }

    private func stationSlice(letter: String) -> SliceState? {
        guard let followedClientHandle else { return nil }
        return slices.values.first { slice in
            slice.clientHandle == followedClientHandle && slice.indexLetter == letter && slice.inUse
        }
    }

    private func publishSnapshot() {
        onSnapshot(value)
    }

    private func sendCommand(_ body: String, timeout: Duration = .seconds(5)) async throws {
        _ = try await sendReply(body,timeout:timeout)
    }

    private func sendReply(_ body:String,timeout:Duration = .seconds(5),permit:DigitalPTTPermit? = nil) async throws -> String {
        guard transportReady, let connection else { throw XCatError.notConnected }
        let commandGeneration = generation
        let sequence = commandSequence
        commandSequence += 1
        let command = "C\(sequence)|\(body)\n"
        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, any Error>) in
            let timeoutTask = Task { [weak self] in
                try? await Task.sleep(for: timeout)
                guard !Task.isCancelled else { return }
                await self?.commandTimedOut(sequence, generation: commandGeneration)
            }
            pendingCommands[sequence] = PendingCommand(continuation: continuation, timeoutTask: timeoutTask)
            let completion: @Sendable (Error?) -> Void = { error in
                if let error {
                    Task { [self] in await self.failCommand(sequence, generation: commandGeneration, error: error) }
                }
            }
            do {
                if let permit {
                    #if os(Windows) && !canImport(Network)
                    // Windows has a userland send queue; guard its eventual
                    // nonblocking write too, not just enqueue admission.
                    try permit.withSendPermission(owner: digitalPTTOwner, connectionGeneration: commandGeneration) {
                        connection.send(content: Data(command.utf8), completion: .contentProcessed(completion),
                                        permit: permit, permitOwner: digitalPTTOwner, permitGeneration: commandGeneration)
                    }
                    #else
                    try permit.withSendPermission(owner: digitalPTTOwner, connectionGeneration: commandGeneration) {
                        connection.send(content: Data(command.utf8), completion: .contentProcessed(completion))
                    }
                    #endif
                } else { connection.send(content: Data(command.utf8), completion: .contentProcessed(completion)) }
            } catch {
                pendingCommands.removeValue(forKey: sequence)?.timeoutTask.cancel()
                continuation.resume(throwing: error)
            }
        }
    }

    private func completeCommand(from line: String) {
        guard let response = FlexCommandResponseParser.parse(line),
              let pending = pendingCommands.removeValue(forKey: response.sequence) else { return }
        pending.timeoutTask.cancel()
        if response.code == 0 {
            pending.continuation.resume(returning:response.message)
        } else {
            let message = !response.message.isEmpty
                ? response.message
                : String(format: "Radio rejected command (0x%08X).", response.code)
            pending.continuation.resume(throwing: XCatError.server(message))
        }
    }

    private func commandTimedOut(_ sequence: Int, generation commandGeneration: Int) {
        guard generation == commandGeneration else { return }
        guard let pending = pendingCommands.removeValue(forKey: sequence) else { return }
        pending.continuation.resume(throwing: XCatError.server("Radio command \(sequence) timed out."))
    }

    private func failCommand(_ sequence: Int, generation commandGeneration: Int, error: any Error) {
        guard generation == commandGeneration else { return }
        guard let pending = pendingCommands.removeValue(forKey: sequence) else { return }
        pending.timeoutTask.cancel()
        pending.continuation.resume(throwing: error)
    }

    private func failAllPending(_ error: any Error) {
        let pending = pendingCommands.values
        pendingCommands.removeAll(keepingCapacity: true)
        for command in pending {
            command.timeoutTask.cancel()
            command.continuation.resume(throwing: error)
        }
    }

    private func connectionFailed(_ message: String, generation currentGeneration: Int) async {
        guard currentGeneration == generation else { return }
        cwx.retire()
        cwx.invalidate("CWX connection lost; pending text was retired without replay.")
        pingTask?.cancel()
        pingTask = nil
        generation += 1
        connection?.cancel()
        connection = nil
        failAllPending(XCatError.notConnected)
        interlockKnown = false
        rfPowerKnown = false
        value.rfPowerLevel = nil
        value.isConnected = false
        publishSnapshot()
        guard shouldRun else {
            transition(.stopped, "Stopped")
            return
        }
        scheduleReconnect(message)
    }

    private func scheduleReconnect(_ message: String) {
        reconnectTask?.cancel()
        reconnectAttempt += 1
        let base = min(pow(2.0, Double(max(0, reconnectAttempt - 1))), 30.0)
        let jitter = Double.random(in: 0...(base * 0.25))
        let delay = base + jitter
        transition(.retrying, "\(message). Retrying in \(String(format: "%.1f", delay)) s")
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            await self?.connect()
        }
    }

    private func resetSessionState() {
        lastHeartbeat = nil
        #if os(Windows)
        spotSource = nil
        ownedSpots.removeAll()
        #endif
        cwx.reset(generation: generation)
        ownClientHandle=nil
        pingTask?.cancel()
        pingTask = nil
        inboundBuffer.removeAll(keepingCapacity: true)
        commandSequence = 1
        slices.removeAll(keepingCapacity: true)
        selectedSliceID = nil
        followedClientHandle = nil
        followedClientID = nil
        clientHandleReceived = false
        stationsByHandle.removeAll()
        clientIDsByHandle.removeAll()
        value.availableStations = []
        reportedClientHandles.removeAll(keepingCapacity: true)
        reportedSliceIDs.removeAll(keepingCapacity: true)
        transportReady = false
        interlockKnown = false
        rfPowerKnown = false
        value.rfPowerLevel = nil
        failAllPending(XCatError.notConnected)
    }

    private func transition(_ newState: RadioConnectionState, _ description: String) {
        state = newState
        onState(newState, description)
    }


    private static func normalizedHandle(_ value: String?) -> String? {
        guard var value = value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(), !value.isEmpty else {
            return nil
        }
        if value.hasPrefix("0x") { value.removeFirst(2) }
        let normalized = value.drop(while: { $0 == "0" })
        return normalized.isEmpty ? "0" : String(normalized)
    }

    private static func normalizedStation(_ value: String?) -> String {
        displayStation(value ?? "")
            .lowercased()
    }

    private static func displayStation(_ value: String) -> String {
        value
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "\u{7F}", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func formatMHz(_ hertz: Int64) -> String {
        String(format: "%.6f", Double(hertz) / 1_000_000)
    }

    private static func validatedMode(_ mode: String) throws -> String {
        let normalized = mode.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
        guard !normalized.isEmpty,
              normalized.allSatisfy({ $0.isLetter || $0.isNumber || $0 == "-" }) else {
            throw XCatError.invalidMode
        }
        return normalized
    }

    private func waitForWANValidation() async throws {
        for _ in 0..<80 {
            if clientHandleReceived { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        throw XCatError.server("The remote radio did not validate the SmartLink session.")
    }
}
