import Foundation

/// Lock contention is not evidence that the retained classification changed.
enum RetainedNeedsValidation: Sendable {
    case current
    case busy
    case invalid
}

/// Retry only a busy foreground lock, without holding it across suspension.
/// Neither the caller's wall deadline nor the one-second monotonic budget is
/// renewed. Invalid workspace/revision/reference evidence fails immediately.
func awaitRetainedNeedsValidation(
    expires: Date,
    probe: @Sendable () -> RetainedNeedsValidation
) async -> Bool {
    let remaining = min(1, expires.timeIntervalSinceNow)
    guard remaining > 0 else { return false }
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: .seconds(remaining))
    while !Task.isCancelled, Date() < expires, clock.now < deadline {
        switch probe() {
        case .current:
            return !Task.isCancelled && Date() < expires && clock.now < deadline
        case .invalid:
            return false
        case .busy:
            do { try await clock.sleep(until: min(deadline, clock.now.advanced(by: .milliseconds(5)))) }
            catch { return false }
        }
    }
    return false
}
