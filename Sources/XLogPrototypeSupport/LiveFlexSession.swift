#if os(macOS) || os(Windows)
#if os(Windows)
import WinSDK
#else
import Darwin
#endif
import Foundation
import XCatIntegrationCore

protocol FlexSessionRadio: RadioControlling {
    nonisolated var supportsDigitalPTTLease: Bool { get }
    func makeDigitalPTTPermit(deadline: ContinuousClock.Instant) async throws -> DigitalPTTPermit
    func setDigitalPTT(_ enabled: Bool, permit: DigitalPTTPermit) async throws -> UInt64
    func start() async
    func reconnectNow() async
    func stopTransportOnly() async
    func setPrimaryMode(_ mode: String) async throws
    func setSliceMode(_ mode: String, letter: String) async throws
    func transmitObservation() async -> FlexRadioClient.TransmitObservation?
    func rfPowerObservation() async -> FlexRadioClient.RFPowerObservation?
    func cwCompositionSnapshot() async -> FlexCWXSnapshot?
    func appendCW(generation: Int, job: String, ordinal: Int, text: String, finish: Bool) async throws -> FlexCWXSnapshot
    func retireCWComposition(generation: Int?, job: String?) async -> Bool
    func clearCWComposition(generation: Int) async throws
}

extension FlexSessionRadio {
    nonisolated var supportsDigitalPTTLease: Bool { false }
    func makeDigitalPTTPermit(deadline: ContinuousClock.Instant) async throws -> DigitalPTTPermit { throw XCatError.unsupported }
    func setDigitalPTT(_ enabled: Bool, permit: DigitalPTTPermit) async throws -> UInt64 { throw XCatError.unsupported }
    func cwCompositionSnapshot() async -> FlexCWXSnapshot? { nil }
    func appendCW(generation: Int, job: String, ordinal: Int, text: String, finish: Bool) async throws -> FlexCWXSnapshot { throw XCatError.notConnected }
    func retireCWComposition(generation: Int?, job: String?) async -> Bool { generation == nil }
    func clearCWComposition(generation: Int) async throws { throw XCatError.notConnected }
}
extension FlexRadioClient: FlexSessionRadio {
    // Optional return in the protocol permits older/test adapters to fail closed.
    func cwCompositionSnapshot() -> FlexCWXSnapshot? { compositionSnapshot() }
}

/// Explicit direct-IP FLEX owner. A command ACK is never promoted to observed
/// success, and disconnect only sends RF release commands for this session's
/// own ambiguous or confirmed transmit attempts.
actor LiveFlexSession {
    struct Configuration: Sendable {
        let host: String
        let port: UInt16
        let stationName: String
        let allowTransmit: Bool
    }
    typealias Factory = @Sendable (
        Configuration,
        @escaping FlexRadioClient.StateHandler,
        @escaping FlexRadioClient.SnapshotHandler
    ) -> any FlexSessionRadio

    static let shared = LiveFlexSession()
    private let factory: Factory
    private let readbackTimeout: Duration
    private var radio: (any FlexSessionRadio)?
    private var configuration: Configuration?
    private var observed: RadioSnapshot?
    private var observedRevision: UInt64 = 0
    private var observedAt: Date?
    private var observedTransmit: Bool?
    private var observedPower: Int?
    private var transportState: RadioConnectionState = .stopped
    private var connectionGeneration: UInt64 = 0
    private var operationEpoch: UInt64 = 0
    private var activeControl = false
    private var carrierNativeIntent = StationCarrierIntent()
    private var releaseInProgress = false
    private var transitioning = false
    private var terminal = false
    private var catServers: EmbeddedCATServers?
    private var catConfig = EmbeddedCATServers.Configuration()
    private var catOwner: Foundation.UUID?
    private var catEpoch: UInt64 = 0
    private var ownsPTT = false
    private var guiPTT = StationGUIPTT()
    private var guiPTTAfterRevision: UInt64?
    private var guiPTTLatestRevision: UInt64 = 0
    private var guiPTTDesired: Bool?
    private var guiPTTOnPending = 0
    private var guiPTTReleaseOverlappedPendingOn = false
    private var ownsCW = false
    private var compositionRoute: String?
    private var ownsTune = false
    private struct DigitalLease {
        let id: String
        let owner: String
        let generation: UInt64
        let transmitSlice: String
        let permit: DigitalPTTPermit
        let radio: any FlexSessionRadio
        var expiresUnixMs: Int64
    }
    private var digitalLease: DigitalLease?
    private var digitalLeaseTask: Task<Void, Never>?
    private var digitalReleaseUncertain = false

    init(readbackTimeout: Duration = .seconds(4), factory: @escaping Factory = { config, onState, onSnapshot in
        FlexRadioClient(host: config.host, port: config.port, stationName: config.stationName,
                        sliceLetter: "A", onState: onState, onSnapshot: onSnapshot)
    }) {
        self.readbackTimeout = readbackTimeout
        self.factory = factory
    }

    func handle(_ method: String, params: [String: Any]) async throws -> [String: Any] {
        guard params["provider"] as? String == "direct_flex_lan" else { throw invalid() }
        if terminal {
            // Retained release uncertainty permits retirement only. No receive
            // mutation, reconnect or transmit acquisition can reopen this owner.
            guard method == "station_disconnect", radio != nil else {
                throw WorkerFailure("worker_stopping", "The direct FLEX session is shutting down.")
            }
            try keys(params, ["provider"])
            try await disconnect()
            return snapshot()
        }
        switch method {
        case "station_cat_servers":
            try keys(params, ["provider", "cat_port", "rigctl_port", "allow_transmit"])
            func port(_ key: String) throws -> UInt16? {
                guard let value = params[key], !(value is NSNull) else { return nil }
                guard let numeric = Self.integer(value), let port = UInt16(exactly: numeric), port > 0 else { throw invalid() }
                return port
            }
            guard !transitioning, !activeControl, !releaseInProgress else { throw busy() }
            let next = try EmbeddedCATServers.Configuration(catPort: port("cat_port"), rigCtlPort: port("rigctl_port"),
                allowTransmit: Self.bool(params["allow_transmit"]) ?? false)
            guard !next.allowTransmit || configuration?.allowTransmit == true else { throw unavailable() }
            if next.catPort != nil || next.rigCtlPort != nil {
                guard radio != nil, transportState == .ready, observed?.isConnected == true else { throw unavailable() }
            }
            transitioning = true
            defer { transitioning = false }
            try await stopCAT()
            if next.catPort != nil || next.rigCtlPort != nil {
                let epoch = catEpoch
                let bridge = FlexCATRadio(owner: self, epoch: epoch, client: Foundation.UUID())
                let listeners = EmbeddedCATServers(radio: bridge, configuration: next,
                    clientRadio: { FlexCATRadio(owner: self, epoch: epoch, client: $0) }, authorizeMutation: { _ in true })
                do { try listeners.start() }
                catch { throw WorkerFailure("cat_listener_failed", error.localizedDescription) }
                catServers = listeners
                catConfig = next
            }
            return snapshot()
        #if os(Windows)
        case "station_add_spot", "station_remove_spot":
            guard transportState == .ready, observed?.isConnected == true, let configuration, let client = radio as? FlexRadioClient else {
                throw WorkerFailure("station_unavailable", "Connect the direct FLEX radio before forwarding spots.")
            }
            let epoch = operationEpoch
            guard !activeControl else { throw busy() }
            activeControl = true
            defer { activeControl = false }
            func identity(_ call: String, _ hz: Int64) -> String {
                "\(configuration.host)|\(configuration.port)|\(call)|\(hz)"
            }
            if method == "station_add_spot" {
                try keys(params, ["provider", "spot"])
                let spot = try Self.validatedSpot(params)
                let key = identity(spot.callsign, spot.frequencyHz)
                let reservation = try await SpotForwardBroker.shared.reserveFlexSpot(identity: key, lifetime: spot.lifetimeSeconds)
                try requireEpoch(epoch)
                try await client.publishSpot(spot, source: reservation.source, prior: reservation.prior) {
                    try await SpotForwardBroker.shared.refreshFlexSpot(identity: key, lifetime: spot.lifetimeSeconds)
                }
            } else {
                try keys(params, ["provider", "callsign", "frequency_hz"])
                guard let call = Self.spotText(params["callsign"], maximum: 32, required: true),
                      let hz = Self.integer(params["frequency_hz"]), (100_000...60_000_000).contains(hz) else { throw invalid() }
                let key = identity(call.uppercased(), hz)
                let reservation = try await SpotForwardBroker.shared.reserveFlexSpot(identity: key, lifetime: nil)
                try requireEpoch(epoch)
                try await client.withdrawSpot(callsign: call.uppercased(), frequencyHz: hz, source: reservation.source, prior: reservation.prior)
                try await SpotForwardBroker.shared.finishFlexRemoval(identity: key)
            }
            return ["accepted": true, "source": "direct_flex_lan", "confirmed": false]
        #endif
        case "station_connect":
            let config = try Self.configuration(params)
            try await connect(config)
            return snapshot()
        case "station_disconnect":
            try keys(params, ["provider"])
            try await disconnect()
            return snapshot()
        case "station_reconnect":
            try keys(params, ["provider"])
            guard !transitioning, !activeControl, !releaseInProgress, let radio else { throw busy() }
            guard digitalLease == nil || (!ownsPTT && !digitalReleaseUncertain) else { throw busy() }
            revokeDigitalLease()
            transitioning = true
            defer { transitioning = false }
            try await stopCAT()
            operationEpoch &+= 1
            observed = nil
            observedAt = nil
            observedTransmit = nil
            observedPower = nil
            transportState = .connecting
            await radio.reconnectNow()
            return snapshot()
        case "station_snapshot":
            try keys(params, ["provider"])
            let epoch = connectionGeneration
            let heartbeat = await (radio as? FlexRadioClient)?.confirmedHeartbeat()
            let cw = await radio?.cwCompositionSnapshot()
            var result = snapshot()
            if epoch == connectionGeneration {
                if let heartbeat { result["heartbeat_at"] = heartbeat.timeIntervalSince1970 }
                if let cw { result["cw_composition"] = Self.compositionProjection(cw) }
            }
            return result
        case "station_tune":
            var params = params
            if let raw = params.removeValue(forKey: "expected_generation") {
                guard let expected = Self.integer(raw), expected >= 0, UInt64(expected) == connectionGeneration else {
                    throw WorkerFailure("station_changed", "Radio connection changed; stale tune was not sent.")
                }
            }
            try keys(params, ["provider", "frequency_hz", "mode"])
            guard let hz = Self.integer(params["frequency_hz"]), (100_000...1_000_000_000).contains(hz),
                  let requestedMode = params["mode"] as? String,
                  (1...12).contains(requestedMode.utf8.count),
                  requestedMode.utf8.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) }) else { throw invalid() }
            return try await tune(hz, mode: Self.radioMode(requestedMode, frequencyHz: hz))
        case "station_radio_control":
            return try await control(params)
        case "station_action":
            var params = params
            let hasExpectedGeneration = params["expected_generation"] != nil
            if let raw = params.removeValue(forKey: "expected_generation") {
                guard ["cancel", "tune"].contains(params["action"] as? String ?? ""), let expected = Self.integer(raw), expected >= 0,
                      UInt64(expected) == connectionGeneration else {
                    throw WorkerFailure("station_changed", "Radio connection changed; stale release was not sent.")
                }
            }
            if let raw = params.removeValue(forKey: "expected_native_intent") {
                guard hasExpectedGeneration, ["cancel", "tune"].contains(params["action"] as? String ?? ""),
                      let value = Self.integer(raw), (1...9_007_199_254_740_991).contains(value) else { throw invalid() }
                try carrierNativeIntent.admitNative(UInt64(value))
            }
            try keys(params, ["provider", "action"])
            guard let action = params["action"] as? String else { throw invalid() }
            if action == "tune" { return try await transmit(on: true, tune: true) }
            if action == "cancel" { return try await cancelOwnedTransmit() }
            throw WorkerFailure("station_action_blocked", "Amplifier operate and standby are not available through direct FLEX control.")
        default:
            throw WorkerFailure("method_not_allowed", "Unknown direct FLEX method.")
        }
    }

    @discardableResult func terminate() async -> StationOwnerRetirement {
        terminal = true
        do { try await disconnect(); return .retired }
        catch { return .unconfirmed }
    }

    func isAttached() -> Bool { radio != nil || transitioning }

    private func connect(_ next: Configuration) async throws {
        guard !transitioning else { throw busy() }
        transitioning = true
        defer { transitioning = false }
        if radio != nil { try await retireCurrent() }
        guard !terminal else { throw WorkerFailure("worker_stopping", "The direct FLEX session is shutting down.") }
        connectionGeneration &+= 1
        let generation = connectionGeneration
        let newRadio = factory(next, { [weak self] state, _ in
            Task { await self?.recordState(state, generation: generation) }
        }, { [weak self] snapshot in
            Task { await self?.recordSnapshot(snapshot, generation: generation) }
        })
        radio = newRadio
        configuration = next
        observed = nil
        observedAt = nil
        observedTransmit = nil
        observedPower = nil
        transportState = .connecting
        await newRadio.start()
        guard !terminal, generation == connectionGeneration else {
            await newRadio.stopTransportOnly()
            throw WorkerFailure("station_superseded", "The direct FLEX connection changed before completion.")
        }
    }

    private func disconnect() async throws {
        guard !transitioning else { throw busy() }
        transitioning = true
        defer { transitioning = false }
        try await retireCurrent()
    }

    private func retireCurrent() async throws {
        revokeDigitalLease()
        if let lease=digitalLease,ownsPTT {Task {await self.expireDigitalLease(lease.permit)}}
        operationEpoch &+= 1
        let deadline = ContinuousClock.now.advanced(by: .seconds(6))
        while (activeControl || releaseInProgress) && ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(25))
        }
        guard !activeControl, !releaseInProgress else {
            throw WorkerFailure("station_busy", "A transmit control is still in flight; disconnect was not confirmed.")
        }
        try await stopCAT()
        guard let radio else { return }
        if ownsPTT || ownsCW || ownsTune { try await releaseOwned(radio) }
        await radio.stopTransportOnly()
        connectionGeneration &+= 1
        self.radio = nil
        digitalLease = nil; digitalReleaseUncertain = false
        guiPTT=StationGUIPTT();guiPTTAfterRevision=nil;guiPTTDesired=nil;guiPTTLatestRevision=0
        guiPTTReleaseOverlappedPendingOn=false
        configuration = nil
        observed = nil
        observedAt = nil
        observedTransmit = nil
        observedPower = nil
        transportState = .stopped
    }

    private func releaseOwned(_ radio: any FlexSessionRadio) async throws {
        var failed = false
        var revision = await radio.transmitObservation()?.revision ?? 0
        if ownsCW {
            do { try await radio.clearCW() } catch { failed = true }
        }
        // `setTransmit(false)` also cancels tune in the copied FLEX client.
        do {
            if let lease=digitalLease,ownsPTT {revision=try await radio.setDigitalPTT(false,permit:lease.permit)}
            else {try await radio.setTransmit(false)}
        } catch { failed = true }
        do {
            try await waitForTransmitObservation(radio, after: revision, epoch: nil, on: false)
        } catch { failed = true }
        guard !failed else {
            throw WorkerFailure("release_unconfirmed", "Owned FLEX transmit release was not observed; connection retained for retry.")
        }
        ownsPTT = false; ownsCW = false; ownsTune = false
    }

    private func tune(_ hz: Int64, mode: String) async throws -> [String: Any] {
        let (radio, epoch) = try beginControl(needsTransmit: false)
        defer { activeControl = false }
        guard let before = observed, before.isConnected, before.vfo("A") != nil else {
            throw WorkerFailure("station_unavailable", "Observed FLEX VFO A is unavailable; no tune command was sent.")
        }
        guard !(ownsPTT || ownsCW || ownsTune) else {
            throw WorkerFailure("station_action_blocked", "Stop and confirm transmit release before changing the FLEX VFO.")
        }
        try await requireTransmitIdle(radio)
        try requireEpoch(epoch)
        if Self.confirmsSimplex(before, frequencyHz: hz, mode: mode) {
            return ["accepted": true, "confirmed": true, "state": snapshot()]
        }
        if before.slice.uppercased() != "A" || before.vfo("A")?.isReceive != true {
            try await requireTransmitIdle(radio)
            try requireEpoch(epoch)
            let revision = observedRevision
            try await radio.setReceiveVFO(0)
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.slice.uppercased() == "A" && $0.vfo("A")?.isReceive == true
            }
        }
        try requireEpoch(epoch)
        guard observed?.vfo("A") != nil else { throw unavailable() }
        if observed?.transmitSlice.uppercased() != "A" || observed?.vfo("A")?.isTransmit != true {
            try await requireTransmitIdle(radio)
            try requireEpoch(epoch)
            let revision = observedRevision
            try await radio.setSplitTransmit(false)
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.transmitSlice.uppercased() == "A" && $0.vfo("A")?.isTransmit == true
            }
        }
        try requireEpoch(epoch)
        try await requireTransmitIdle(radio)
        try requireEpoch(epoch)
        let revision = observedRevision
        do {
            try await radio.setFrequencyAndMode(hz, mode: mode)
        } catch {
            throw WorkerFailure("station_rejected", "The direct FLEX tune was rejected or its outcome is uncertain.")
        }
        _ = try await waitForObservation(after: revision, epoch: epoch) {
            Self.confirmsSimplex($0, frequencyHz: hz, mode: mode)
        }
        return ["accepted": true, "confirmed": true, "state": snapshot()]
    }

    private func control(_ params: [String: Any]) async throws -> [String: Any] {
        if params["control"] as? String == "ptt_lease" { return try renewDigitalLease(params) }
        if params["control"] as? String == "ptt", params["source"] as? String == "gui" {return try await guiPTTControl(params)}
        var params = params
        if let raw = params.removeValue(forKey: "expected_generation") {
            guard let expected = Self.integer(raw), expected >= 0, UInt64(expected) == connectionGeneration else {
                throw WorkerFailure("station_changed", "Radio connection changed; stale control was not sent.")
            }
        }
        guard let control = params["control"] as? String else { throw invalid() }
        switch control {
        case "compose_cw":
            try keys(params, ["provider", "control", "job", "append", "text", "finish", "cwx_generation"])
            guard let job = params["job"] as? String, UUID(uuidString: job) != nil,
                  let append = Self.integer(params["append"]), (1...4096).contains(append),
                  let text = params["text"] as? String, let finish = Self.bool(params["finish"]),
                  let expected = Self.integer(params["cwx_generation"]), expected > 0,
                  configuration?.allowTransmit == true, let route = compositionRouteKey() else { throw invalid() }
            let admissionEpoch = operationEpoch
            guard let status = await radio?.cwCompositionSnapshot(), status.available, status.generation == expected,
                  !ownsCW || status.job == job && compositionRoute == route else {
                throw WorkerFailure("station_action_blocked", "Indexed CWX ownership is unavailable; use Stop and staged Send.")
            }
            try requireEpoch(admissionEpoch)
            let first = !ownsCW
            let (radio, epoch) = try beginControl(needsTransmit: first)
            defer { activeControl = false }
            if first { try await requireTransmitIdle(radio); try requireEpoch(epoch) }
            ownsCW = true; compositionRoute = route
            let statusAfter = try await radio.appendCW(generation: Int(expected), job: job, ordinal: Int(append), text: text, finish: finish)
            try requireEpoch(epoch)
            return ["accepted": true, "confirmed": false, "cw_composition": Self.compositionProjection(statusAfter)]
        case "ptt":
            try keys(params, ["provider", "control", "enabled"])
            guard let enabled = Self.bool(params["enabled"]) else { throw invalid() }
            return try await transmit(on: enabled, tune: false)
        case "send_cw":
            try keys(params, ["provider", "control", "text"])
            guard let text = params["text"] as? String, (1...128).contains(text.utf8.count),
                  text.unicodeScalars.allSatisfy({ (32...126).contains($0.value) && $0 != "|" }) else { throw invalid() }
            let (radio, epoch) = try beginControl(needsTransmit: true)
            defer { activeControl = false }
            try await requireTransmitIdle(radio)
            try requireEpoch(epoch)
            ownsCW = true
            do { try await radio.sendCW(text) }
            catch { throw WorkerFailure("station_rejected", "CW delivery was not confirmed; release ownership retained.") }
            try requireEpoch(epoch)
            return ["accepted": true, "confirmed": false, "control": control, "state": snapshot()]
        case "clear_cw":
            let expectedCWX = params.removeValue(forKey: "cwx_generation")
            let retiredJob = params.removeValue(forKey: "cwx_job")
            try keys(params, ["provider", "control"])
            if let retiredJob, (retiredJob as? String).flatMap(UUID.init(uuidString:)) == nil { throw invalid() }
            let expectedCWXGeneration = expectedCWX.flatMap(Self.integer).flatMap(Int.init(exactly:))
            if expectedCWX != nil && (expectedCWXGeneration ?? 0) <= 0 { throw invalid() }
            // Retire first, even if an append is awaiting its radio reply. The
            // shared owner retains release responsibility and retries only Stop.
            operationEpoch &+= 1
            guard await radio?.retireCWComposition(generation: expectedCWXGeneration, job: retiredJob as? String) == true else {
                throw WorkerFailure("station_changed", "CWX connection changed; release was not redirected to a new connection. Check the radio before releasing the guard.")
            }
            guard ownsCW else { throw WorkerFailure("station_action_blocked", "This session has no CW transmission to clear.") }
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            let revision = await radio.transmitObservation()?.revision ?? 0
            if let expectedCWXGeneration { try await radio.clearCWComposition(generation: expectedCWXGeneration) }
            else { try await radio.clearCW() }
            try await waitForTransmitObservation(radio, after: revision, epoch: epoch, on: false)
            ownsCW = false; ownsPTT = false; ownsTune = false
            compositionRoute = nil
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "digital_tune":
            try keys(params,["provider","control","slice","frequency_hz","mode"])
            guard let letter=params["slice"] as? String,["A","B"].contains(letter),
                  let hz=Self.integer(params["frequency_hz"]),(100_000...1_000_000_000).contains(hz),
                  let requestedMode=Self.mode(params["mode"]) else {throw invalid()}
            let mode=Self.radioMode(requestedMode,frequencyHz:hz)
            let(radio,epoch)=try beginControl(needsTransmit:false)
            defer{activeControl=false}
            try await requireSafeReceiveMutation(radio,letter:letter,epoch:epoch)
            let revision=observedRevision
            do {
                if letter=="A" {try await radio.setPrimaryFrequency(hz)} else {try await radio.setSecondaryFrequency(hz)}
                try requireEpoch(epoch)
                try await requireSafeReceiveMutation(radio,letter:letter,epoch:epoch)
                try await radio.setSliceMode(mode,letter:letter)
            } catch {throw rejectedControl()}
            _=try await waitForObservation(after:revision,epoch:epoch) {
                $0.isConnected && $0.vfo(letter)?.frequencyHz==hz && $0.vfo(letter)?.mode.uppercased()==mode
            }
            return ["accepted":true,"confirmed":true,"control":control,"slice":letter,"state":snapshot()]
        case "set_cw_speed":
            try keys(params, ["provider", "control", "words_per_minute"])
            guard let speed = Self.integer(params["words_per_minute"]), (5...100).contains(speed) else { throw invalid() }
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await radio.setCWSpeed(Int(speed))
            try requireEpoch(epoch)
            return ["accepted": true, "confirmed": false, "control": control, "state": snapshot()]
        case "set_frequency_a", "set_frequency_b":
            try keys(params, ["provider", "control", "frequency_hz"])
            guard let hz = Self.integer(params["frequency_hz"]), (100_000...1_000_000_000).contains(hz) else { throw invalid() }
            let letter = control == "set_frequency_a" ? "A" : "B"
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: letter, epoch: epoch)
            let revision = observedRevision
            do {
                if letter == "A" { try await radio.setPrimaryFrequency(hz) }
                else { try await radio.setSecondaryFrequency(hz) }
            } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.vfo(letter)?.frequencyHz == hz
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_mode":
            try keys(params, ["provider", "control", "mode"])
            guard let mode = Self.mode(params["mode"]) else { throw invalid() }
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: observed?.slice.uppercased() ?? "", epoch: epoch)
            let revision = observedRevision
            do { try await radio.setMode(mode) } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.mode.uppercased() == mode
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_mode_a":
            try keys(params, ["provider", "control", "mode"])
            guard let mode = Self.mode(params["mode"]) else { throw invalid() }
            guard observed?.vfo("A")?.isReceive == true else {
                throw WorkerFailure("station_unavailable", "Receive VFO A is not currently observed; no mode control was sent.")
            }
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: "A", epoch: epoch)
            let revision = observedRevision
            do { try await radio.setPrimaryMode(mode) } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.vfo("A")?.isReceive == true && $0.vfo("A")?.mode.uppercased() == mode
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_receive_vfo":
            try keys(params, ["provider", "control", "vfo"])
            guard let letter = params["vfo"] as? String, ["A", "B"].contains(letter) else { throw invalid() }
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: letter, epoch: epoch)
            let revision = observedRevision
            do { try await radio.setReceiveVFO(letter == "A" ? 0 : 1) } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.slice.uppercased() == letter && $0.vfo(letter)?.isReceive == true
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_split":
            try keys(params, ["provider", "control", "enabled"])
            guard let enabled = Self.bool(params["enabled"]) else { throw invalid() }
            let letter = enabled ? "B" : "A"
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: letter, epoch: epoch)
            let revision = observedRevision
            do { try await radio.setSplitTransmit(enabled) } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                $0.isConnected && $0.transmitSlice.uppercased() == letter && $0.vfo(letter)?.isTransmit == true
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_rit", "set_xit":
            try keys(params, ["provider", "control", "enabled", "offset_hz", "vfo"])
            guard let enabled = Self.bool(params["enabled"]),
                  let offset = Self.integer(params["offset_hz"]), (-9_999...9_999).contains(offset),
                  params["vfo"] == nil || ["A", "B"].contains(params["vfo"] as? String ?? "") else { throw invalid() }
            let letter = params["vfo"] as? String ?? observed?.slice.uppercased() ?? ""
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: letter, epoch: epoch)
            let revision = observedRevision
            do {
                if control == "set_rit" { try await radio.setRIT(enabled: enabled, offsetHz: Int(offset), vfo: letter) }
                else { try await radio.setXIT(enabled: enabled, offsetHz: Int(offset), vfo: letter) }
            } catch { throw rejectedControl() }
            _ = try await waitForObservation(after: revision, epoch: epoch) {
                guard let vfo = $0.vfo(letter) else { return false }
                return control == "set_rit" ? vfo.ritEnabled == enabled && vfo.ritOffsetHz == Int(offset)
                    : vfo.xitEnabled == enabled && vfo.xitOffsetHz == Int(offset)
            }
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        case "set_rf_power":
            try keys(params, ["provider", "control", "watts", "expected_tx_frequency_hz", "expected_tx_mode", "expected_tx_vfo"])
            guard let watts = Self.integer(params["watts"]), (0...100).contains(watts) else { throw invalid() }
            func verifyPowerContext() throws {
                guard params["expected_tx_frequency_hz"] != nil || params["expected_tx_mode"] != nil || params["expected_tx_vfo"] != nil else { return }
                guard let frequency = Self.integer(params["expected_tx_frequency_hz"]),
                      let mode = params["expected_tx_mode"] as? String,
                      let letter = params["expected_tx_vfo"] as? String,
                      let current = observed, let tx = current.vfo(current.transmitSlice), tx.isTransmit,
                      tx.letter == letter, tx.frequencyHz == frequency, tx.mode.uppercased() == mode.uppercased() else {
                    throw WorkerFailure("station_changed", "Transmit context changed; power policy was not applied.")
                }
            }
            try verifyPowerContext()
            let (radio, epoch) = try beginControl(needsTransmit: false)
            defer { activeControl = false }
            try await requireSafeReceiveMutation(radio, letter: observed?.slice.uppercased() ?? "", epoch: epoch)
            try verifyPowerContext()
            let revision = await radio.rfPowerObservation()?.revision ?? 0
            try requireEpoch(epoch)
            try verifyPowerContext()
            do { try await radio.setRFPower(Int(watts)) } catch { throw rejectedControl() }
            let power = try await waitForRFPowerObservation(radio, after: revision, epoch: epoch,
                                                             watts: Int(watts))
            observedPower = power.level
            return ["accepted": true, "confirmed": true, "control": control, "state": snapshot()]
        default:
            throw WorkerFailure("station_action_blocked", "This direct FLEX control is not yet supported.")
        }
    }

    private func requireSafeReceiveMutation(_ radio: any FlexSessionRadio, letter: String,
                                            epoch: UInt64) async throws {
        guard let observed, observed.isConnected, observed.vfo(letter) != nil else {
            throw WorkerFailure("station_unavailable", "The requested FLEX VFO is not observed; no control was sent.")
        }
        guard !(ownsPTT || ownsCW || ownsTune) else {
            throw WorkerFailure("station_action_blocked", "Stop and confirm transmit release before changing FLEX radio settings.")
        }
        try await requireTransmitIdle(radio)
        try requireEpoch(epoch)
    }

    private func requireTransmitIdle(_ radio: any FlexSessionRadio) async throws {
        guard let sample = await radio.transmitObservation(), sample.isConnected else {
            throw WorkerFailure("station_unavailable", "FLEX interlock receive state has not been observed.")
        }
        guard !sample.isTransmitting else {
            throw WorkerFailure("station_action_blocked", "FLEX radio is transmitting; receive-side controls are blocked.")
        }
    }

    private func rejectedControl() -> WorkerFailure {
        WorkerFailure("station_rejected", "FLEX control was rejected or its outcome is uncertain.")
    }

    private func transmit(on: Bool, tune: Bool) async throws -> [String: Any] {
        if !on && !(ownsPTT || ownsCW || ownsTune) {
            throw WorkerFailure("station_action_blocked", "This session does not own a transmit control to release.")
        }
        let (radio, epoch) = try beginControl(needsTransmit: on)
        defer { activeControl = false }
        let before = await radio.transmitObservation()
        let revision = before?.revision ?? 0
        if on {
            guard let before, before.isConnected else {
                throw WorkerFailure("station_unavailable", "FLEX interlock transmit state has not been observed.")
            }
            guard !before.isTransmitting else {
                throw WorkerFailure("station_action_blocked", "FLEX radio is already transmitting.")
            }
            try requireEpoch(epoch)
        }
        if on {
            if tune { ownsTune = true } else { ownsPTT = true }
        }
        do {
            if on && tune { try await radio.setTransmit(.tune) }
            else if !on && ownsCW {
                try await radio.clearCW()
                try await radio.setTransmit(false)
            }
            else { try await radio.setTransmit(on) }
        } catch {
            throw WorkerFailure("station_rejected", "FLEX transmit outcome is uncertain; release ownership retained.")
        }
        try await waitForTransmitObservation(radio, after: revision, epoch: epoch, on: on)
        if !on { ownsPTT = false; ownsCW = false; ownsTune = false }
        return ["accepted": true, "confirmed": true, "action": tune ? (on ? "tune" : "cancel") : "ptt",
                "state": snapshot()]
    }

    private func guiPTTControl(_ params:[String:Any]) async throws->[String:Any]{
        try keys(params,["provider","control","enabled","source","ptt_owner_id","expected_generation","expected_native_intent","digital_lease"])
        guard params["source"] as? String == "gui",let on=Self.bool(params["enabled"]),let id=params["ptt_owner_id"] as? String,UUID(uuidString:id) != nil,
              let gen=Self.integer(params["expected_generation"]),gen>=0,UInt64(gen)==connectionGeneration,
              let token=Self.integer(params["expected_native_intent"]),(1...9_007_199_254_740_991).contains(token) else{throw WorkerFailure("station_changed","PTT exact backend generation changed")}
        let requestedLease = try params["digital_lease"].map { try Self.digitalLeaseInput($0) }
        guard on || requestedLease == nil else { throw invalid() }
        try carrierNativeIntent.admitNative(UInt64(token))
        if !on, let lease=digitalLease, lease.owner==id { revokeDigitalLease() }
        if !on{operationEpoch &+= 1}
        let radio:any FlexSessionRadio;let epoch:UInt64
        if on{(radio,epoch)=try beginControl(needsTransmit:true, digitalSlice:requestedLease?.slice)}
        else{guard !transitioning,!releaseInProgress,let current=self.radio,transportState == .ready else{throw unavailable()};radio=current;epoch=operationEpoch;releaseInProgress=true}
        let interruptedOn = !on && guiPTTOnPending > 0
        if interruptedOn { guiPTTReleaseOverlappedPendingOn = true }
        if on{guiPTTOnPending+=1}
        defer{
            if on{
                activeControl=false;guiPTTOnPending-=1;guiPTT.retireAcquisition()
                if guiPTTDesired == false{guiPTTAfterRevision=max(guiPTTAfterRevision ?? 0,guiPTTLatestRevision)}
            }else{
                if interruptedOn{
                    guiPTTAfterRevision=max(guiPTTAfterRevision ?? 0,guiPTTLatestRevision)
                    guiPTTReleaseOverlappedPendingOn=false
                }
                releaseInProgress=false
            }
        }
        if on, let requestedLease {
            guard radio.supportsDigitalPTTLease, digitalLease?.id != requestedLease.id else { throw invalid() }
            let permit = try await radio.makeDigitalPTTPermit(deadline: requestedLease.deadline)
            try requireEpoch(epoch)
            guard observed?.transmitSlice==requestedLease.slice,observed?.vfo(requestedLease.slice)?.isTransmit==true else {permit.revoke();throw unavailable()}
            digitalLease = DigitalLease(id: requestedLease.id, owner: id, generation: connectionGeneration,
                transmitSlice: requestedLease.slice, permit: permit, radio: radio, expiresUnixMs: requestedLease.expires)
            digitalReleaseUncertain = false
            scheduleDigitalLease(permit)
        }
        let retainedOff = !on && digitalLease?.owner == id ? digitalLease : nil
        let before:FlexRadioClient.TransmitObservation?
        if retainedOff != nil {before=nil} else {before=await radio.transmitObservation()}
        var beforeRevision=before?.revision ?? 0
        guiPTTLatestRevision=max(guiPTTLatestRevision,beforeRevision);try requireEpoch(epoch)
        if on{guard let before,before.isConnected,!before.isTransmitting else{throw unavailable()}}
        let owned=guiPTT.ownerID==id
        try guiPTT.begin(id,on:on);guiPTTDesired=on;guiPTTAfterRevision=before?.revision ?? 0
        if on{ownsPTT=true}
        do{if on||owned{
            if let lease=digitalLease,lease.owner==id,lease.generation==connectionGeneration {
                beforeRevision = try await radio.setDigitalPTT(on,permit:lease.permit)
                guiPTTAfterRevision=beforeRevision
            } else { try await radio.setTransmit(on) }
        }}catch{throw rejectedControl()}
        try requireEpoch(epoch);guiPTT.acknowledged(on:on)
        if on||owned{
            try await waitForTransmitObservation(radio,after:beforeRevision,epoch:epoch,on:on)
            if !interruptedOn && (on||guiPTTOnPending==0) {
                guiPTT.confirm(on:on);if !on{ownsPTT=false;digitalReleaseUncertain=false}
            }
        }
        // CW and Tune obligations are intentionally untouched by GUI PTT off.
        return ["accepted":true,"confirmed":guiPTT.confirmed,"control":"ptt","enabled":on,"owner_id":id,"state":snapshot()]
    }

    private static func digitalLeaseInput(_ raw: Any) throws -> (id:String, expires:Int64, deadline:ContinuousClock.Instant, slice:String) {
        guard let fields=raw as? [String:Any],Set(fields.keys)==["lease_id","expires_at_unix_ms","transmit_slice"],
              let slice=fields["transmit_slice"] as? String,["A","B"].contains(slice),
              let id=fields["lease_id"] as? String, let uuid=UUID(uuidString:id),uuid.uuidString.lowercased()==id,
              id != "00000000-0000-0000-0000-000000000000",let expires=integer(fields["expires_at_unix_ms"]),expires<=9_007_199_254_740_991 else {
            throw WorkerFailure("invalid_params","Invalid bounded digital PTT lease")
        }
        let mono=ContinuousClock.now
        let wall=Int64((Date().timeIntervalSince1970*1000).rounded(.down))
        guard expires>wall,expires-wall<=1000 else {throw WorkerFailure("station_changed","Digital PTT lease expired or exceeds one second")}
        return (id,expires,mono.advanced(by:.milliseconds(expires-wall)),slice)
    }
    private func renewDigitalLease(_ params:[String:Any]) throws->[String:Any] {
        try keys(params,["provider","control","source","ptt_owner_id","expected_generation","digital_lease"])
        guard params["source"] as? String=="gui",let owner=params["ptt_owner_id"] as? String,
              let generation=Self.integer(params["expected_generation"]),generation>=0,UInt64(generation)==connectionGeneration,
              var lease=digitalLease,lease.owner==owner,lease.generation==connectionGeneration,
              guiPTT.ownerID==owner,guiPTTDesired==true,!digitalReleaseUncertain,!terminal,
              let raw=params["digital_lease"] else {throw unavailable()}
        let update=try Self.digitalLeaseInput(raw)
        guard update.id==lease.id,update.slice==lease.transmitSlice,update.expires>=lease.expiresUnixMs,
              observed?.transmitSlice==lease.transmitSlice,observed?.vfo(lease.transmitSlice)?.isTransmit==true else {throw invalid()}
        let deadline = update.expires == lease.expiresUnixMs ? min(update.deadline,lease.permit.deadline) : update.deadline
        try lease.permit.renew(deadline:deadline)
        lease.expiresUnixMs=update.expires;digitalLease=lease;scheduleDigitalLease(lease.permit)
        return ["lease_renewed":true,"owner_id":owner,"lease_id":lease.id,"expires_at_unix_ms":update.expires]
    }
    private func revokeDigitalLease() {
        digitalLease?.permit.revoke();digitalLeaseTask?.cancel();digitalLeaseTask=nil
    }
    private func scheduleDigitalLease(_ permit:DigitalPTTPermit) {
        digitalLeaseTask?.cancel()
        digitalLeaseTask=Task.detached(priority:.high) { [weak self,permit] in
            do {try await Task.sleep(until:permit.deadline,clock:.continuous)} catch {return}
            // Revocation happens outside actor/ordinary worker dispatch, before
            // queueing the exact owner's release continuation.
            if permit.expireIfNeeded() {await self?.expireDigitalLease(permit)}
        }
    }
    private func expireDigitalLease(_ permit:DigitalPTTPermit) async {
        guard let lease=digitalLease,lease.permit===permit,lease.generation==connectionGeneration else{return}
        operationEpoch &+= 1
        guard guiPTT.ownerID==lease.owner,ownsPTT else{return}
        digitalReleaseUncertain=true;guiPTTDesired=false;guiPTT.invalidateProof()
        let epoch=operationEpoch
        let alreadyReleasing=releaseInProgress;releaseInProgress=true
        defer {if !alreadyReleasing {releaseInProgress=false}}
        do {
            // No pre-release observation await: the existing radio owner captures
            // its interlock baseline synchronously at send initiation.
            let before=try await lease.radio.setDigitalPTT(false,permit:permit)
            // The OFF was sent: record its baseline before any epoch check. A host OFF arriving
            // now advances the epoch and aborts this readback; the baseline lets a later radio
            // sample still prove the release (see the observation handler).
            guiPTT.acknowledged(on:false);guiPTTAfterRevision=before
            try requireEpoch(epoch)
            try await waitForTransmitObservation(lease.radio,after:before,epoch:epoch,on:false)
            guard digitalLease?.permit===permit else{return}
            ownsPTT=false;guiPTT.confirm(on:false);digitalReleaseUncertain=false
        } catch {guiPTT.invalidateProof()} // Retained ownership blocks new TX.
    }

    /// A release may interrupt an owned acquisition's readback. Its epoch retires
    /// that older operation; its separate flag keeps the older defer from
    /// admitting a new acquisition before release confirmation has finished.
    private func cancelOwnedTransmit() async throws -> [String: Any] {
        revokeDigitalLease()
        // Retire an acquisition still awaiting its initial observation, before
        // it has acquired TX ownership or sent any keying command.
        if activeControl { operationEpoch &+= 1 }
        guard ownsPTT || ownsCW || ownsTune else {
            throw WorkerFailure("station_action_blocked", "This session does not own a transmit control to release.")
        }
        guard !transitioning, !releaseInProgress, let radio, transportState == .ready else { throw unavailable() }
        releaseInProgress = true
        operationEpoch &+= 1
        let epoch = operationEpoch
        defer { releaseInProgress = false }
        var revision = await radio.transmitObservation()?.revision ?? 0
        try requireEpoch(epoch)
        do {
            if ownsCW { try await radio.clearCW(); try requireEpoch(epoch) }
            if let lease=digitalLease,ownsPTT {revision=try await radio.setDigitalPTT(false,permit:lease.permit)}
            else {try await radio.setTransmit(false)}
        } catch {
            throw WorkerFailure("station_rejected", "FLEX transmit outcome is uncertain; release ownership retained.")
        }
        try await waitForTransmitObservation(radio, after: revision, epoch: epoch, on: false)
        ownsPTT = false; ownsCW = false; ownsTune = false
        if digitalLease != nil {digitalReleaseUncertain=false;guiPTT.confirm(on:false)}
        return ["accepted": true, "confirmed": true, "action": "cancel", "state": snapshot()]
    }

    private func stopCAT() async throws {
        catEpoch &+= 1
        let listeners = catServers
        catServers = nil
        catConfig = .init()
        await listeners?.stopAndDrain()
        if catOwner != nil, let radio, ownsCW || ownsTune || ownsPTT { try await releaseOwned(radio) }
        catOwner = nil
    }

    fileprivate func catSnapshot(_ epoch: UInt64) -> RadioSnapshot {
        guard epoch == catEpoch, transportState == .ready, let observed else { return RadioSnapshot(isConnected: false) }
        return observed
    }

    fileprivate func catSpeed(_ epoch: UInt64) async -> Int {
        guard epoch == catEpoch, let radio else { return 0 }
        return await radio.cwSpeed()
    }

    fileprivate func catControl(_ name: String, _ fields: [String: Any], epoch: UInt64, client: Foundation.UUID) async throws {
        guard epoch == catEpoch, !transitioning, catServers != nil else { throw unavailable() }
        let release = name == "clear_cw" || name == "ptt" && fields["enabled"] as? Bool == false
        let acquire = name == "send_cw" || name == "tune" || name == "ptt" && !release
        if release { guard catOwner == client else { throw unavailable() } }
        if acquire {
            guard catConfig.allowTransmit, configuration?.allowTransmit == true, catOwner == nil,
                  !activeControl, !releaseInProgress, !(ownsCW || ownsTune || ownsPTT), guiPTT.ownerID == nil else { throw busy() }
            catOwner = client
        }
        defer { if catOwner == client, !(ownsCW || ownsTune || ownsPTT) { catOwner = nil } }
        if name == "tune" { _ = try await transmit(on: true, tune: true) }
        else if name == "simplex", let hz = fields["frequency_hz"] as? Int64, let mode = fields["mode"] as? String {
            _ = try await tune(hz, mode: mode)
        } else {
            var params = fields
            params["provider"] = "direct_flex_lan"
            params["control"] = name
            _ = try await control(params)
        }
    }

    private func beginControl(needsTransmit: Bool, digitalSlice: String? = nil) throws -> (any FlexSessionRadio, UInt64) {
        guard !transitioning, !activeControl, !releaseInProgress, let radio,
              transportState == .ready, observed?.isConnected == true else { throw unavailable() }
        guard !needsTransmit || digitalSlice != nil || configuration?.allowTransmit == true else {
            throw WorkerFailure("station_action_blocked", "Explicit transmit opt-in is required for direct FLEX control.")
        }
        guard !needsTransmit || !(ownsPTT || ownsCW || ownsTune || guiPTT.ownerID != nil || digitalReleaseUncertain) else {
            throw WorkerFailure("station_action_blocked", "The FLEX radio is already transmitting or has an unresolved release.")
        }
        if needsTransmit, let observed {
            let receiveKnown = digitalSlice != nil || observed.vfo(observed.slice)?.isReceive == true
            let exactTransmit = digitalSlice == nil || observed.transmitSlice == digitalSlice
            guard receiveKnown, exactTransmit,
                  observed.vfo(observed.transmitSlice)?.isTransmit == true else {
                throw WorkerFailure("station_unavailable", "FLEX receive/transmit slice ownership is not explicitly observed.")
            }
        }
        activeControl = true
        return (radio, operationEpoch)
    }

    private func waitForObservation(
        after revision: UInt64, epoch: UInt64?, timeout: Duration? = nil,
        matching: (RadioSnapshot) -> Bool
    ) async throws -> RadioSnapshot {
        let deadline = ContinuousClock.now.advanced(by: timeout ?? readbackTimeout)
        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            if let epoch { try requireEpoch(epoch) }
            if observedRevision > revision, transportState == .ready,
               let observed, matching(observed) { return observed }
            try await Task.sleep(for: .milliseconds(40))
        }
        throw WorkerFailure("station_timeout", "A fresh authoritative FLEX readback did not confirm the control.")
    }

    private func waitForTransmitObservation(_ radio: any FlexSessionRadio, after revision: UInt64,
                                            epoch: UInt64?, on: Bool) async throws {
        let deadline = ContinuousClock.now.advanced(by: readbackTimeout)
        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            if let epoch { try requireEpoch(epoch) }
            let sample = await radio.transmitObservation()
            guiPTTLatestRevision=max(guiPTTLatestRevision,sample?.revision ?? 0)
            if let epoch {try requireEpoch(epoch)}
            if let sample, sample.revision > revision, sample.isConnected,
               sample.isTransmitting == on, transportState == .ready { observedTransmit=on;return }
            try await Task.sleep(for: .milliseconds(40))
        }
        throw WorkerFailure("station_timeout", "A fresh FLEX interlock readback did not confirm transmit state.")
    }

    private func waitForRFPowerObservation(_ radio: any FlexSessionRadio, after revision: UInt64,
                                           epoch: UInt64, watts: Int) async throws -> FlexRadioClient.RFPowerObservation {
        let deadline = ContinuousClock.now.advanced(by: readbackTimeout)
        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            try requireEpoch(epoch)
            let sample = await radio.rfPowerObservation()
            try requireEpoch(epoch)
            if let sample, sample.revision > revision, sample.isConnected,
               sample.level == watts, transportState == .ready { return sample }
            try await Task.sleep(for: .milliseconds(40))
        }
        throw WorkerFailure("station_timeout", "A fresh FLEX RF-power status did not confirm the requested setting.")
    }

    private func requireEpoch(_ epoch: UInt64) throws {
        guard epoch == operationEpoch, !terminal, radio != nil else {
            throw WorkerFailure("station_superseded", "The direct FLEX session changed during the operation.")
        }
    }

    private func recordState(_ state: RadioConnectionState, generation: UInt64) {
        guard generation == connectionGeneration else { return }
        transportState = state
        if state != .ready {
            if let lease=digitalLease {
                revokeDigitalLease()
                Task {await self.expireDigitalLease(lease.permit)}
            }
            operationEpoch &+= 1
            observed = nil
            observedAt = nil
            observedTransmit = nil
            observedPower = nil
        }
    }

    private func recordSnapshot(_ snapshot: RadioSnapshot, generation: UInt64) async {
        let sample = await radio?.transmitObservation()
        let power = await radio?.rfPowerObservation()
        guard generation == connectionGeneration else { return }
        guiPTTLatestRevision=max(guiPTTLatestRevision,sample?.revision ?? 0)
        if let lease=digitalLease,guiPTTDesired==true,
           (snapshot.transmitSlice != lease.transmitSlice || snapshot.vfo(lease.transmitSlice)?.isTransmit != true) {
            revokeDigitalLease()
            Task {await self.expireDigitalLease(lease.permit)}
        }
        observed = snapshot
        observedRevision &+= 1
        observedAt = Date()
        observedTransmit = sample?.isTransmitting
        observedPower = power?.isConnected == true ? power?.level : nil
        // An uncertain digital release (its own readback was aborted, e.g. by a host OFF racing
        // lease expiry at the end of a transmission) is proven only by a fresh interlock sample,
        // newer than the sent OFF, showing the radio unkeyed. Without this the uncertainty never
        // cleared: the host's exact release could not reconcile and every later TX was refused.
        let uncertainReleaseObserved = digitalReleaseUncertain && guiPTTDesired == false && sample?.isTransmitting == false
        if let sample,let after=guiPTTAfterRevision,let desired=guiPTTDesired,sample.revision>after,sample.isConnected,
           !guiPTT.pending,!guiPTTReleaseOverlappedPendingOn,!digitalReleaseUncertain || uncertainReleaseObserved {
            if sample.isTransmitting==desired && (desired || guiPTTOnPending==0){
                guiPTT.confirm(on:desired);if !desired{ownsPTT=false;digitalReleaseUncertain=false}
            }else{guiPTT.invalidateProof()}
        }
    }

    private func snapshot() -> [String: Any] {
        let current = transportState == .ready && observed?.isConnected == true ? observed : nil
        let radioProjection: Any = current.map { value in
            ["observed": true, "connected": true, "frequency_hz": value.frequencyHz,
             "mode": value.mode, "receive_frequency_hz": value.frequencyHz,
             "transmit_frequency_hz": value.transmitFrequencyHz,
             "receive_vfo": value.vfo(value.slice)?.isReceive == true ? value.slice as Any : NSNull(),
             "transmit_vfo": value.vfo(value.transmitSlice)?.isTransmit == true ? value.transmitSlice as Any : NSNull(),
             "split": value.vfo(value.slice)?.isReceive == true && value.vfo(value.transmitSlice)?.isTransmit == true
                 ? (value.slice != value.transmitSlice || value.frequencyHz != value.transmitFrequencyHz) as Any : NSNull(),
             "transmitting": observedTransmit as Any? ?? NSNull(),
              "rf_power_level": observedPower as Any? ?? NSNull(),
             "observed_at": observedAt?.timeIntervalSince1970 as Any? ?? NSNull(),
             "received_at": observedAt?.timeIntervalSince1970 as Any? ?? NSNull(),
             "vfos": value.vfos.map { vfo in
                 ["letter": vfo.letter, "frequency_hz": vfo.frequencyHz, "mode": vfo.mode,
                  "receive": vfo.isReceive, "transmit": vfo.isTransmit,
                  "flex_slice_id": vfo.flexSliceID as Any? ?? NSNull(),
                  "flex_client_handle": vfo.flexClientHandle as Any? ?? NSNull(),
                  "dax_channel": vfo.daxChannel as Any? ?? NSNull()] as [String: Any]
             }] as [String: Any]
        } ?? NSNull()
        return ["source": "direct_flex_lan", "available": current != nil, "generation": connectionGeneration,
                "transport_state": transportState.rawValue, "generated_at": Date().timeIntervalSince1970,
                "available_stations": observed?.availableStations ?? [], "selected_station": current?.station as Any? ?? NSNull(),
                "radio": radioProjection, "station": NSNull(), "rotor": NSNull(),
                "cat_servers": ["cat_port": catConfig.catPort as Any? ?? NSNull(), "rigctl_port": catConfig.rigCtlPort as Any? ?? NSNull(), "allow_transmit": catConfig.allowTransmit] as [String: Any],
                "carrier_tune": ["pending": activeControl, "release_required": ownsTune || releaseInProgress],
                "ptt": guiPTT.projection,
                "digital_ptt_lease": ["supported":radio?.supportsDigitalPTTLease ?? false,"max_age_ms":1000] as [String:Any],
                "propagation": NSNull(), "allow_transmit": configuration?.allowTransmit ?? false,
                "capabilities": ["supported_commands": supportedCommands(current),
                                 "unavailable_commands": observedPower == nil
                                     ? ["set_rit", "set_xit", "set_rf_power"]
                                     : ["set_rit", "set_xit"],
                                 "requires_transmit_opt_in": true] as [String: Any]]
    }

    private func compositionRouteKey() -> String? {
        guard let observed, observed.isConnected, ["CW", "CWR"].contains(observed.mode.uppercased()),
              observed.vfo(observed.slice)?.isReceive == true, observed.vfo(observed.transmitSlice)?.isTransmit == true else { return nil }
        return "\(observed.slice)|\(observed.transmitSlice)|\(observed.frequencyHz)|\(observed.transmitFrequencyHz)|\(observed.mode)"
    }
    private static func compositionProjection(_ value: FlexCWXSnapshot) -> [String: Any] {
        ["generation": value.generation, "available": value.available, "reason": value.reason,
         "job": value.job as Any? ?? NSNull(), "accepted": value.accepted, "reported": value.reported, "closed": value.closed]
    }

    #if os(Windows)
    private static func spotText(_ value: Any?, maximum: Int, required: Bool) -> String? {
        guard let text = value as? String, text.utf8.count <= maximum,
              (!required || !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty),
              text.unicodeScalars.allSatisfy({ (32...126).contains($0.value) && $0 != "|" }) else { return nil }
        return text
    }

    private static func validatedSpot(_ params: [String: Any]) throws -> FlexSpot {
        guard let payload = params["spot"] as? [String: Any],
              Set(payload.keys) == ["frequency_hz", "callsign", "mode", "source", "spotter",
                                    "comment", "timestamp", "lifetime_seconds", "priority", "color_hex"],
              let frequency = Self.integer(payload["frequency_hz"]), (100_000...60_000_000).contains(frequency),
              let timestamp = Self.integer(payload["timestamp"]),
              timestamp >= Int64(Date().timeIntervalSince1970) - 86_400,
              timestamp <= Int64(Date().timeIntervalSince1970) + 300,
              let lifetime = Self.integer(payload["lifetime_seconds"]), (30...86_400).contains(lifetime),
              let priority = Self.integer(payload["priority"]), (1...5).contains(priority),
              let callsign = Self.spotText(payload["callsign"], maximum: 32, required: true),
              let mode = Self.spotText(payload["mode"], maximum: 16, required: true),
              let source = Self.spotText(payload["source"], maximum: 32, required: true),
              let spotter = Self.spotText(payload["spotter"], maximum: 32, required: false),
              let comment = Self.spotText(payload["comment"], maximum: 96, required: false),
              let color = payload["color_hex"] as? String,
              color.range(of: #"^#[0-9A-Fa-f]{6}$"#, options: .regularExpression) != nil,
              let spot = try? FlexSpot(frequencyHz: frequency, callsign: callsign,
                                       mode: Self.radioMode(mode, frequencyHz: frequency),
                                       source: source, spotter: spotter, comment: comment,
                                       timestamp: timestamp, lifetimeSeconds: Int(lifetime),
                                       priority: Int(priority), colorHex: color) else { throw Self.invalid() }
        return spot
    }

    #endif
    private func supportedCommands(_ current: RadioSnapshot?) -> [String] {
        var result = ["station_cat_servers", "station_snapshot", "station_tune", "station_connect", "station_disconnect",
                      "station_reconnect", "set_frequency_a", "set_mode", "set_mode_a", "set_receive_vfo",
                      "set_split", "set_cw_speed", "ptt", "send_cw", "clear_cw", "tune", "cancel"]
        if current?.vfo("B") != nil { result.append("set_frequency_b") }
        if current != nil && observedPower != nil { result.append("set_rf_power") }
        return result.sorted()
    }

    static func confirmsSimplex(_ value: RadioSnapshot, frequencyHz: Int64, mode: String) -> Bool {
        value.isConnected && !value.isTransmitting && value.frequencyHz == frequencyHz && value.transmitFrequencyHz == frequencyHz
            && value.mode.uppercased() == mode && value.slice.uppercased() == "A"
            && value.transmitSlice.uppercased() == "A"
            && value.vfo("A")?.isReceive == true && value.vfo("A")?.isTransmit == true
    }

    private static func radioMode(_ mode: String, frequencyHz: Int64) -> String {
        switch mode {
        case "SSB": frequencyHz < 10_000_000 ? "LSB" : "USB"
        case "FT8", "FT4", "FT2", "RTTY", "JTTY", "PSK", "DIGI": "DIGU"
        default: mode
        }
    }

    private static func mode(_ value: Any?) -> String? {
        guard let mode = value as? String, (1...12).contains(mode.utf8.count),
              mode.utf8.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) }) else { return nil }
        return mode
    }

    private static func configuration(_ params: [String: Any]) throws -> Configuration {
        guard Set(params.keys) == ["provider", "host", "station_name"] ||
              Set(params.keys) == ["provider", "host", "station_name", "port"] ||
              Set(params.keys) == ["provider", "host", "station_name", "allow_transmit"] ||
              Set(params.keys) == ["provider", "host", "station_name", "port", "allow_transmit"],
              let host = params["host"] as? String, privateIP(host),
              let stationName = params["station_name"] as? String,
              stationName.utf8.count <= 64,
              stationName.unicodeScalars.allSatisfy({ (32...126).contains($0.value) && $0 != "|" }),
              let portNumber = params["port"].map(integer) ?? 4992,
              (1...65535).contains(portNumber),
              params["allow_transmit"] == nil || bool(params["allow_transmit"]) != nil else { throw invalid() }
        return Configuration(host: host, port: UInt16(portNumber), stationName: stationName,
                             allowTransmit: bool(params["allow_transmit"]) ?? false)
    }

    private static func privateIP(_ host: String) -> Bool {
        guard !host.isEmpty, host.utf8.count <= 64, !host.contains("%"), !host.utf8.contains(0) else { return false }
        var v4 = in_addr()
        if host.withCString({ inet_pton(AF_INET, $0, &v4) }) == 1 {
            let bytes = withUnsafeBytes(of: &v4) { Array($0) }
            return bytes[0] == 10 || bytes[0] == 127 ||
                (bytes[0] == 172 && (16...31).contains(bytes[1])) ||
                (bytes[0] == 192 && bytes[1] == 168) ||
                (bytes[0] == 169 && bytes[1] == 254) ||
                (bytes[0] == 100 && (64...127).contains(bytes[1]))
        }
        var v6 = in6_addr()
        guard host.withCString({ inet_pton(AF_INET6, $0, &v6) }) == 1 else { return false }
        let bytes = withUnsafeBytes(of: &v6) { Array($0) }
        return (bytes[0] & 0xfe) == 0xfc ||
            (bytes[0] == 0xfe && (bytes[1] & 0xc0) == 0x80) ||
            bytes.dropFirst(15).first == 1 && bytes.prefix(15).allSatisfy({ $0 == 0 })
    }

    private func keys(_ params: [String: Any], _ allowed: Set<String>) throws {
        guard Set(params.keys).isSubset(of: allowed) else { throw Self.invalid() }
    }
    private static func integer(_ value: Any?) -> Int64? {
        guard let number = value as? NSNumber, LiveStationMethods.jsonNumber(number) != nil else { return nil }
        let numeric = number.doubleValue
        guard numeric.isFinite, numeric.rounded() == numeric,
              numeric >= Double(Int64.min), numeric < 9_223_372_036_854_775_808 else { return nil }
        return number.int64Value
    }
    private static func bool(_ value: Any?) -> Bool? {
        guard let number = value as? NSNumber, LiveStationMethods.jsonNumber(number) == nil, number.doubleValue.isFinite else { return nil }
        return number.boolValue
    }
    private static func invalid() -> WorkerFailure { WorkerFailure("invalid_params", "Invalid direct FLEX parameters.") }
    private func invalid() -> WorkerFailure { Self.invalid() }
    private func unavailable() -> WorkerFailure { WorkerFailure("station_unavailable", "Connect and observe the direct FLEX radio first.") }
    private func busy() -> WorkerFailure { WorkerFailure("station_busy", "Another direct FLEX lifecycle operation is in progress.") }
}

/// CAT and RigCtl mutations use the same readback and release paths as Desk controls.
private actor FlexCATRadio: RadioControlling {
    private let owner: LiveFlexSession
    private let epoch: UInt64
    private let client: Foundation.UUID
    init(owner: LiveFlexSession, epoch: UInt64, client: Foundation.UUID) { self.owner = owner; self.epoch = epoch; self.client = client }
    func snapshot() async -> RadioSnapshot { await owner.catSnapshot(epoch) }
    func primaryFrequency() async -> Int64? { await snapshot().vfo("A")?.frequencyHz }
    func secondaryFrequency() async -> Int64? { await snapshot().vfo("B")?.frequencyHz }
    func cwSpeed() async -> Int { await owner.catSpeed(epoch) }
    private func run(_ name: String, _ fields: [String: Any] = [:]) async throws {
        try await owner.catControl(name, fields, epoch: epoch, client: client)
    }
    func setPrimaryFrequency(_ hz: Int64) async throws { try await run("set_frequency_a", ["frequency_hz": hz]) }
    func setSecondaryFrequency(_ hz: Int64) async throws { try await run("set_frequency_b", ["frequency_hz": hz]) }
    func setFrequency(_ hz: Int64) async throws {
        try await run(await snapshot().slice == "B" ? "set_frequency_b" : "set_frequency_a", ["frequency_hz": hz])
    }
    func tuneSimplex(_ hz: Int64, mode: String) async throws { try await run("simplex", ["frequency_hz": hz, "mode": mode]) }
    func setMode(_ mode: String) async throws { try await run("set_mode", ["mode": mode]) }
    func setReceiveVFO(_ vfo: Int) async throws { try await run("set_receive_vfo", ["vfo": vfo == 0 ? "A" : "B"]) }
    func setSplitTransmit(_ enabled: Bool) async throws { try await run("set_split", ["enabled": enabled]) }
    func setTransmit(_ enabled: Bool) async throws { try await run("ptt", ["enabled": enabled]) }
    func setTransmit(_ mode: CATTransmitMode) async throws {
        if case .tune = mode { try await run("tune") } else { try await setTransmit(true) }
    }
    func sendCW(_ text: String) async throws { try await run("send_cw", ["text": text]) }
    func clearCW() async throws { try await run("clear_cw") }
    func setCWSpeed(_ speed: Int) async throws { try await run("set_cw_speed", ["words_per_minute": speed]) }
    func setRFPower(_ watts: Int) async throws { try await run("set_rf_power", ["watts": watts]) }
    func setRIT(enabled: Bool, offsetHz: Int, vfo: String?) async throws {
        try await run("set_rit", ["enabled": enabled, "offset_hz": offsetHz, "vfo": vfo ?? "A"])
    }
    func setXIT(enabled: Bool, offsetHz: Int, vfo: String?) async throws {
        try await run("set_xit", ["enabled": enabled, "offset_hz": offsetHz, "vfo": vfo ?? "A"])
    }
}
#endif
