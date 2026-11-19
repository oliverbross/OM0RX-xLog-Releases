import Foundation

/// A revocable lease for ONE existing FLEX connection. The lock is held only
/// around a synchronous write initiation, never an await, reply or readback.
public final class DigitalPTTPermit: @unchecked Sendable {
    public final class OwnerIdentity: Sendable { public init() {} }
    private let owner: OwnerIdentity
    public let connectionGeneration: Int
    private let lock = NSLock()
    private let now: @Sendable () -> ContinuousClock.Instant
    private var until: ContinuousClock.Instant
    private var retired = false

    public init(owner: OwnerIdentity, connectionGeneration: Int, deadline: ContinuousClock.Instant,
                now: @escaping @Sendable () -> ContinuousClock.Instant = { .now }) throws {
        let current = now()
        guard deadline > current, deadline <= current.advanced(by: .seconds(1)) else { throw XCatError.notConnected }
        self.owner = owner; self.connectionGeneration = connectionGeneration; self.until = deadline; self.now = now
    }
    public var deadline: ContinuousClock.Instant { lock.withLock { until } }
    public var isLive: Bool { lock.withLock { !retired && now() < until } }
    public func renew(deadline: ContinuousClock.Instant) throws {
        try lock.withLock {
            let current = now()
            guard !retired, current < until, deadline > current,
                  deadline <= current.advanced(by: .seconds(1)) else { throw XCatError.notConnected }
            until = deadline
        }
    }
    public func revoke() { lock.withLock { retired = true } }
    /// True only for the first expiry of the currently retained deadline.
    public func expireIfNeeded() -> Bool {
        lock.withLock {
            guard !retired, now() >= until else { return false }
            retired = true; return true
        }
    }
    public func checkRelease(owner: OwnerIdentity, connectionGeneration: Int) throws {
        guard owner === self.owner, connectionGeneration == self.connectionGeneration else { throw XCatError.notConnected }
    }
    public func withSendPermission<T>(owner: OwnerIdentity, connectionGeneration: Int, _ send: () throws -> T) throws -> T {
        try lock.withLock {
            guard owner === self.owner, connectionGeneration == self.connectionGeneration, !retired, now() < until else {
                throw XCatError.notConnected
            }
            return try send()
        }
    }
}
