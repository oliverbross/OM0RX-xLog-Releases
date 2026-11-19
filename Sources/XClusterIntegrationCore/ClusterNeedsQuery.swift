import Foundation

public struct ClusterNeedsQuery: Sendable {
    public let id: String
    public let call: String
    public let frequencyHz: Int64
    public let mode: String
    public init(id: String, call: String, frequencyHz: Int64, mode: String) {
        self.id = id; self.call = call; self.frequencyHz = frequencyHz; self.mode = mode
    }
}

public struct ClusterNeedsResult: Sendable {
    public let query: ClusterNeedsQuery
    public let band: String
    public let country: String?
    public let adif: Int?
    public let resolution: String
    public let callStatus: String
    public let dxccStatus: String
    public let need: String
    public let score: Int?
    public let reasons: [String]
    public let reasonCode: String?
}

public struct ClusterNeedsBatch: Sendable {
    public let revision: UInt64
    public let reference: String?
    public let results: [ClusterNeedsResult]
}
