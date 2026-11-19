import Foundation
#if canImport(XLogCore)
import XLogCore
#else
import XLogPortableCore
#endif
import XClusterIntegrationCore

/// Display projection only. Durable edits retain their existing operation IDs
/// and revision checks; no rows or award evidence are invented from a page.
enum CompanionHistoryProjection {
    static func exactMode(mode: String, submode: String = "") -> String {
        let submode = submode.trimmingCharacters(in: .whitespacesAndNewlines)
        let value = SpotLogic.normalizedMode(submode.isEmpty ? mode : submode)
        switch value { case "CWR": return "CW"; case "RTTYR": return "RTTY"; default: return value }
    }
    private static func band(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }
    private static func millis(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }
    static func boundedComment(_ value: String) -> (text: String, complete: Bool) {
        guard value.utf8.count > 4096 else { return (value, true) }
        let suffix = "\n[Truncated for companion display; open xLog for the complete contact.]"
        let budget = 4096 - suffix.utf8.count
        var prefix = "", bytes = 0
        // A Character can contain arbitrarily many combining scalars. Budget
        // actual UTF-8, retaining valid scalar boundaries and room for the notice.
        for scalar in value.unicodeScalars {
            let width = String(scalar).utf8.count
            guard bytes + width <= budget else { break }
            prefix.unicodeScalars.append(scalar); bytes += width
        }
        return (prefix + suffix, false)
    }
    private static func contact(_ current: LogbookContactSnapshot) -> [String: Any] {
        let qso = current.qso, comment = boundedComment(current.qso.comment)
        return ["contact_id":qso.id.uuidString.lowercased(), "callsign":qso.call,
            "band":qso.band, "mode":qso.mode, "submode":qso.submode,
            "exact_mode":exactMode(mode:qso.mode, submode:qso.submode),
            "started_at_unix_ms":millis(qso.start), "frequency_hz":qso.frequencyHz as Any? ?? NSNull(),
            "rst_sent":qso.rstSent, "rst_received":qso.rstReceived, "grid":qso.gridSquare,
            "comment":comment.text,
            "contact_revision":comment.complete ? current.revision as Any : NSNull(), "details_complete":comment.complete]
    }
    static func history(_ read: CompanionLogbookRead, call: String, generation: String,
                        requestedBand: String? = nil, requestedMode: String? = nil, now: Date = Date()) -> [String: Any] {
        let rows = read.historyRows
        // Reuse the owner's normal Desk confirmation policy, not a client rule.
        let local = LocalContactHistory.project(rows:rows, call:call, adif:nil)
        let bands = Set(rows.map { band($0.band) }).sorted()
        let modes = Set(rows.map { exactMode(mode:$0.mode, submode:$0.submode) }).sorted()
        let requestedBand = requestedBand.map(band), requestedMode = requestedMode.map { exactMode(mode:$0) }
        let bandWorked: Bool? = requestedBand.map { b in rows.contains { band($0.band) == b } }
        let exactWorked: Bool? = requestedBand.flatMap { b in requestedMode.map { m in
            rows.contains { band($0.band) == b && exactMode(mode:$0.mode, submode:$0.submode) == m }
        } }
        // Preserve the existing matrix's raw SUBMODE precedence and Y/V rule.
        // Its legacy semantics differ from the explicitly labelled summary policy.
        var legacy: [String: (band:String, mode:String, confirmed:Bool)] = [:]
        for row in rows {
            let mode = row.submode.isEmpty ? row.mode : row.submode, key = "\(row.band)|\(row.submode.isEmpty ? row.mode : row.submode)"
            let confirmed = [row.qslReceived, row.lotwReceived].contains { ["Y", "V"].contains($0.uppercased()) }
            legacy[key] = (row.band, mode, confirmed || (legacy[key]?.confirmed ?? false))
        }
        let matrix = legacy.values.sorted { $0.band == $1.band ? $0.mode < $1.mode : $0.band < $1.band }
        return ["callsign":call, "generation":generation, "logbook_revision":read.revision,
            "contacts":read.historyContacts.map(contact), "contacts_limit":25, "contacts_complete":rows.count <= 25,
            "matrix":matrix.prefix(256).map { ["band":$0.band,"mode":$0.mode,"worked":true,"confirmed":$0.confirmed] as [String:Any] },
            "matrix_complete":matrix.count <= 256, "generated_at_unix_ms":millis(now), "max_age_ms":5000,
            "history_summary":["schema_version":1, "scope":"normal_log_base_callsign", "complete":true,
                "total":local["previous_count"] ?? NSNull(), "worked_before":!rows.isEmpty,
                "confirmed_count":local["previous_confirmed_count"] ?? NSNull(),
                "confirmation_policy":"xlog_cluster_paper_or_lotw_v1", "confirmation_sources":["paper_qsl","lotw"],
                "last_started_at_unix_ms":rows.first.map { millis($0.start) } as Any? ?? NSNull(),
                "bands":Array(bands.prefix(128)), "bands_complete":bands.count <= 128,
                "modes":Array(modes.prefix(128)), "modes_complete":modes.count <= 128,
                "requested_band":requestedBand as Any? ?? NSNull(), "requested_exact_mode":requestedMode as Any? ?? NSNull(),
                "requested_band_worked":bandWorked as Any? ?? NSNull(), "requested_band_exact_mode_worked":exactWorked as Any? ?? NSNull()]]
    }
    static func recent(_ read: CompanionLogbookRead, generation: String, now: Date = Date()) -> [String:Any] {
        ["schema_version":1, "generation":generation, "logbook_revision":read.revision,
         "contacts":read.recentContacts.map(contact), "limit":5, "total":read.logbookCount,
         "complete":read.logbookCount <= 5, "ordering":"qso_start_desc_contact_id_asc", "generated_at_unix_ms":millis(now), "max_age_ms":5000]
    }
}
