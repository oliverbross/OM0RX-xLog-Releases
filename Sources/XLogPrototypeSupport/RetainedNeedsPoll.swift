import Foundation

/// The optional lookup has an already-running deadline. Do not spend that
/// budget validating the same authority for an empty metadata batch first.
/// Both callbacks are the existing authoritative producer, not cached facts.
func retainedNeedsPoll(
    isolation: isolated (any Actor)? = #isolation,
    lookup: () async -> [String: Any]?,
    base: () async -> [String: Any]
) async -> (metadata: [String: Any], reply: [String: Any]?) {
    let reply = await lookup()
    var metadata: [String: Any]
    if let result = reply?["result"] as? [String: Any], result["readiness"] as? String == "ready" {
        // Preserve the exact validated authority, generated time and TTL.
        metadata = result
    } else {
        // An expired/malformed/foreign optional query cannot make ordinary
        // metadata unavailable. This callback independently checks authority.
        metadata = await base()
    }
    metadata.removeValue(forKey: "results")
    return (metadata, reply)
}
