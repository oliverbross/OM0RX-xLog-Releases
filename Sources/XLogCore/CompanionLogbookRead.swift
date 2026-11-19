import Foundation

/// One committed SQLite snapshot. Thin full-call rows remain inside the owner;
/// only bounded contact pages and scalar summaries cross the companion bridge.
public struct CompanionLogbookRead: Sendable {
    public let revision: Int64
    public let historyRows: [ContactHistoryRow]
    public let historyContacts: [LogbookContactSnapshot]
    public let recentContacts: [LogbookContactSnapshot]
    public let logbookCount: Int
}
