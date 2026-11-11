#if os(macOS)
import Foundation
import CoreFoundation
import XCatIntegrationCore

/// A rotor transport has its own lifetime, independent of either radio owner.
/// Closing it is a queue barrier, not proof that the antenna has stopped moving.
protocol RotorTransport: AnyObject, Sendable {
    func start()
    func stopAndDrain() async
    func turn(to bearing: Double) async throws
    func stopRotation() async throws
}

extension DCU1RotatorClient: RotorTransport {}

/// Network.framework delivers state and headings on one serial queue. Reduce
/// them synchronously there so bursts stay ordered without a task per sample.
private final class RotorTelemetry: @unchecked Sendable {
    struct Value {
        var state: DCU1RotatorClient.State = .stopped
        var azimuth: Double?
        var observedAt: Date?
        var observation = RotorObservation()
        var commandRevision: UInt64 = 0
    }
    private let lock = NSLock()
    private var value = Value()
    func withValue<T>(_ body: (inout Value) throws -> T) rethrows -> T {
        try lock.withLock { try body(&value) }
    }
    func update(_ state: DCU1RotatorClient.State) {
        withValue {
            $0.state = state
            if state != .connected {
                $0.commandRevision &+= 1
                $0.azimuth = nil
                $0.observedAt = nil
                $0.observation = RotorObservation()
            }
        }
    }
    func observe(_ azimuth: Double) {
        guard azimuth.isFinite, (0...360).contains(azimuth) else { return }
        withValue {
            guard $0.state == .connected else { return }
            $0.azimuth = azimuth
            $0.observedAt = Date()
            $0.observation.observe(azimuth)
        }
    }
}

actor LiveRotorSession {
    static let shared = LiveRotorSession()
    typealias Factory = @Sendable (String, UInt16, @escaping DCU1RotatorClient.StateHandler,
                                   @escaping DCU1RotatorClient.AzimuthHandler) -> any RotorTransport

    private let factory: Factory
    private var client: (any RotorTransport)?
    private var telemetry = RotorTelemetry()
    private var name = "Rotor"
    private var retirement: Task<Void, Never>?
    private var terminalShutdown = false

    init(factory: @escaping Factory = { host, port, onState, onAzimuth in
        DCU1RotatorClient(host: host, port: port, pollMilliseconds: 500,
                          onState: onState, onAzimuth: onAzimuth)
    }) {
        self.factory = factory
    }

    func handle(_ method: String, params: [String: Any]) async throws -> [String: Any] {
        try requireActive()
        switch method {
        case "station_rotor_snapshot":
            try requireKeys(params, allowed: ["provider"])
            return projection()
        case "station_rotor_connect":
            try requireKeys(params, allowed: ["provider", "host", "port", "name"])
            let host = try LiveStationSession.validatedRotorHost(params["host"])
            guard let port = try Self.integer(params["port"]), (1...65_535).contains(port) else {
                throw Self.invalidParams()
            }
            let requestedName = (params["name"] as? String ?? "Rotor").trimmingCharacters(in: .whitespacesAndNewlines)
            guard params["name"] == nil || params["name"] is String,
                  (1...64).contains(requestedName.utf8.count) else { throw Self.invalidParams() }
            guard client == nil, retirement == nil else {
                throw WorkerFailure("station_busy", "Disconnect the current DCU-1 rotor before connecting again.")
            }
            let captured = RotorTelemetry()
            captured.update(.connecting)
            let next = factory(host, UInt16(port),
                { newState, _ in captured.update(newState) },
                { value in captured.observe(value) })
            telemetry = captured
            client = next
            name = requestedName
            next.start()
            return projection()
        case "station_rotor_disconnect":
            try requireKeys(params, allowed: ["provider"])
            await retire()
            return projection()
        case "station_rotor_goto":
            try requireKeys(params, allowed: ["provider", "bearing", "label"])
            guard let number = params["bearing"] as? NSNumber,
                  CFGetTypeID(number) != CFBooleanGetTypeID(),
                  number.doubleValue.isFinite, (0...360).contains(number.doubleValue),
                  params["label"] == nil || params["label"] is String else { throw Self.invalidParams() }
            let (client, captured, revision) = try beginCommand()
            do { try await client.turn(to: number.doubleValue) }
            catch { throw WorkerFailure("station_rejected", "The rotor rejected the turn command.") }
            let label = (params["label"] as? String).map { String($0.prefix(64)) } ?? "Manual bearing"
            try completeCommand(captured, revision) { $0.request(number.doubleValue, label: label) }
            return ["accepted": true, "confirmed": false, "action": "rotor_goto"]
        case "station_rotor_stop":
            try requireKeys(params, allowed: ["provider"])
            let (client, captured, revision) = try beginCommand()
            do { try await client.stopRotation() }
            catch { throw WorkerFailure("station_rejected", "The rotor rejected the stop command.") }
            try completeCommand(captured, revision) { $0.stopRequested() }
            return ["accepted": true, "confirmed": false, "action": "rotor_stop"]
        default:
            throw WorkerFailure("method_not_allowed", "Rotor method is not available.")
        }
    }

    func projection() -> [String: Any] {
        let latest = telemetry.withValue { $0 }
        let observation = latest.observation
        return ["connected": latest.state == .connected, "name": name,
                "status": latest.state == .connected ? observation.status : latest.state == .stopped ? "Disconnected" : latest.state.rawValue.capitalized,
                "azimuth": Self.optional(latest.azimuth), "observed_at": Self.optional(latest.observedAt?.timeIntervalSince1970),
                "target_bearing": Self.optional(observation.target), "target_label": observation.label,
                "moving": Self.optional(observation.moving), "motion_direction": Self.optional(observation.direction)]
    }

    func isAttached() -> Bool { client != nil }

    func terminate() async {
        terminalShutdown = true
        await retire()
    }

    private func retire() async {
        if let retirement {
            await retirement.value
            return
        }
        let previous = client
        client = nil
        telemetry = RotorTelemetry()
        let drain = Task<Void, Never> { if let previous { await previous.stopAndDrain() } }
        retirement = drain
        await drain.value
        retirement = nil
    }

    private func beginCommand() throws -> (any RotorTransport, RotorTelemetry, UInt64) {
        try telemetry.withValue {
            guard let client, $0.state == .connected else {
                throw WorkerFailure("station_unavailable", "The rotor is not connected.")
            }
            $0.commandRevision &+= 1
            return (client, telemetry, $0.commandRevision)
        }
    }

    private func completeCommand(_ captured: RotorTelemetry, _ revision: UInt64,
                                 update: (inout RotorObservation) -> Void) throws {
        try captured.withValue {
            guard captured === telemetry, revision == $0.commandRevision else { throw changed() }
            update(&$0.observation)
        }
    }

    private func requireActive() throws {
        guard !terminalShutdown else {
            throw WorkerFailure("worker_stopping", "The rotor service is shutting down; no new command was started.")
        }
    }

    private func requireKeys(_ params: [String: Any], allowed: Set<String>) throws {
        guard Set(params.keys).isSubset(of: allowed), params["provider"] as? String == "direct_agent" else {
            throw Self.invalidParams()
        }
    }

    private static func integer(_ raw: Any?) throws -> Int64? {
        guard let raw else { return nil }
        guard let value = raw as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID(),
              value.doubleValue.isFinite, value.doubleValue.rounded() == value.doubleValue,
              value.doubleValue >= Double(Int64.min), value.doubleValue < Double(Int64.max) else {
            throw invalidParams()
        }
        return value.int64Value
    }

    private static func optional(_ value: Any?) -> Any { value ?? NSNull() }
    private static func invalidParams() -> WorkerFailure {
        WorkerFailure("invalid_params", "Invalid direct rotor parameters.")
    }
    private func changed() -> WorkerFailure {
        WorkerFailure("station_changed", "The rotor owner changed during the request; no outcome is confirmed.")
    }
}
#endif
