/// SmartSDR slice `dax` subscription field. Zero explicitly disables DAX.
/// Missing values are retained by the subscription owner; malformed updates
/// clear the previous assignment rather than silently keeping a stale route.
enum FlexDAXAssignment {
    static func parse(_ raw: String) -> Int? {
        guard raw.count == 1, let value = Int(raw), (0...8).contains(value) else { return nil }
        return value
    }
}
