import Foundation

/// Source-only by-ID projection. No provider, transport, write or device access.
/// A host must still bind this owner read to authenticated server/workspace/worker
/// identity and its original read lease before advertising a detail capability.
public struct CompanionDetailField<T: Encodable & Sendable>: Encodable, Sendable {
    public enum State: String, Encodable, Sendable { case value, empty, unavailable }
    public let state: State
    public let value: T?
    public let reason: String?
    public static func known(_ value: T?) -> Self {
        Self(state: value == nil ? .empty : .value, value: value, reason: nil)
    }
    public static func unavailable(_ reason: String = "unsupported") -> Self {
        Self(state: .unavailable, value: nil, reason: reason)
    }
    private enum CodingKeys: String, CodingKey { case state, value, reason }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(state, forKey: .state)
        if let value { try c.encode(value, forKey: .value) } else { try c.encodeNil(forKey: .value) }
        if let reason { try c.encode(reason, forKey: .reason) } else { try c.encodeNil(forKey: .reason) }
    }
}

public struct CompanionContactDetailRead: Sendable {
    public enum Status: String, Encodable, Sendable { case found, deleted, not_found, unavailable }
    public let contactID: UUID
    public let logRevision: Int64
    public let contactRevision: Int64?
    public let status: Status
    public let reason: String?
    public let snapshot: LogbookContactSnapshot?
    public init(contactID: UUID, logRevision: Int64, contactRevision: Int64?, status: Status,
                reason: String?, snapshot: LogbookContactSnapshot?) {
        self.contactID = contactID; self.logRevision = logRevision; self.contactRevision = contactRevision
        self.status = status; self.reason = reason; self.snapshot = snapshot
    }
}

public struct CompanionContactDetail: Encodable, Sendable {
    public typealias Text = CompanionDetailField<String>
    public typealias Integer = CompanionDetailField<Int64>
    public typealias Number = CompanionDetailField<Double>
    public struct Contact: Encodable, Sendable {
        public let callsign, band, mode, submode: Text
        public let started_at_unix_ms, ended_at_unix_ms: Integer
        public let stored_time_unit, original_start_precision, original_end_precision: Text
        public let raw_qso_date, raw_time_on, raw_qso_date_off, raw_time_off: Text
        public let rst_sent, rst_received, grid, comment, notes: Text
    }
    public struct Radio: Encodable, Sendable {
        public let frequency_hz, receive_frequency_hz: Integer
        public let receive_band, tx_power_raw: Text
        public let tx_power_w: Number
        public let propagation_mode, antenna_path, satellite_name: Text
    }
    public struct Station: Encodable, Sendable {
        public let name, qth, state, county, country: Text
        public let dxcc: Integer
        public let continent, cq_zone, itu_zone: Text
    }
    public struct OwnStation: Encodable, Sendable {
        public let operator_call, callsign, grid, recorded_rig, radio_label: Text
        public let station_profile_id: Integer
        public let station_location: Text
    }
    public struct ReferenceSide: Encodable, Sendable {
        // Raw values may contain multiple references. No guessed split or merge.
        public let pota, sota, wwff, iota, sig, sig_info: Text
    }
    public struct References: Encodable, Sendable {
        public let their, my: ReferenceSide
        public let dok: Text
    }
    public struct Confirmation: Encodable, Sendable {
        public let source, evidence_kind: Text
        public let sent_status, sent_date, sent_via: Text
        public let received_status, received_date, received_via: Text
        public let match_age_seconds: Number
    }
    public struct Confirmations: Encodable, Sendable {
        public let paper, lotw, eqsl, qrz: Confirmation
    }
    public struct Awards: Encodable, Sendable {
        public let credit_granted, credit_submitted, authority: Text
    }
    public struct Upload: Encodable, Sendable {
        public let source, state: Text
        public let receipt_revision, last_attempt_at_unix_ms: Integer
        public let is_current_revision: CompanionDetailField<Bool>
    }
    public struct Uploads: Encodable, Sendable {
        public let lotw, eqsl, qrz, clublog: Upload
    }
    public struct Contest: Encodable, Sendable {
        public let contest_id, stx, srx, stx_string, srx_string: Text
    }
    public struct Fields: Encodable, Sendable {
        public let contact: Contact
        public let radio: Radio
        public let station: Station
        public let own_station: OwnStation
        public let references: References
        public let confirmations: Confirmations
        public let awards: Awards
        public let uploads: Uploads
        public let contest: Contest
    }
    public struct Editability: Encodable, Sendable {
        public let allowed: Bool
        public let reason: String
        public let allowed_fields: [String]
        public let base: CompanionQSO?
        private enum CodingKeys: String, CodingKey { case allowed, reason, allowed_fields, base }
        private enum BaseKeys: String, CodingKey {
            case contact_id, callsign, band, mode, started_at_unix_ms, frequency_hz, rst_sent, rst_received, grid, comment
        }
        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(allowed, forKey: .allowed); try c.encode(reason, forKey: .reason)
            try c.encode(allowed_fields, forKey: .allowed_fields)
            if let base {
                // This is a complete edit base: nullable frequency must be
                // explicit, unlike synthesized Codable's encodeIfPresent.
                var b = c.nestedContainer(keyedBy: BaseKeys.self, forKey: .base)
                try b.encode(base.contact_id, forKey: .contact_id)
                try b.encode(base.callsign, forKey: .callsign); try b.encode(base.band, forKey: .band)
                try b.encode(base.mode, forKey: .mode); try b.encode(base.started_at_unix_ms, forKey: .started_at_unix_ms)
                if let hz = base.frequency_hz { try b.encode(hz, forKey: .frequency_hz) }
                else { try b.encodeNil(forKey: .frequency_hz) }
                try b.encode(base.rst_sent, forKey: .rst_sent); try b.encode(base.rst_received, forKey: .rst_received)
                try b.encode(base.grid, forKey: .grid); try b.encode(base.comment, forKey: .comment)
            } else { try c.encodeNil(forKey: .base) }
        }
    }
    public let schema_version = 1
    public let contact_id: String
    public let status: CompanionContactDetailRead.Status
    public let reason: String?
    public let logbook_revision: Int64
    public let contact_revision: Int64?
    public let fields: Fields?
    public let editability: Editability
    public let locate: Text = .unavailable("paged_logbook_not_supported")
    private enum CodingKeys: String, CodingKey {
        case schema_version, contact_id, status, reason, logbook_revision, contact_revision, fields, editability, locate
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(schema_version, forKey: .schema_version); try c.encode(contact_id, forKey: .contact_id)
        try c.encode(status, forKey: .status); try c.encode(logbook_revision, forKey: .logbook_revision)
        if let reason { try c.encode(reason, forKey: .reason) } else { try c.encodeNil(forKey: .reason) }
        if let contact_revision { try c.encode(contact_revision, forKey: .contact_revision) } else { try c.encodeNil(forKey: .contact_revision) }
        if let fields { try c.encode(fields, forKey: .fields) } else { try c.encodeNil(forKey: .fields) }
        try c.encode(editability, forKey: .editability); try c.encode(locate, forKey: .locate)
    }
    private static func text(_ value: String?, cap: Int = 4096) -> Text {
        guard let value, !value.isEmpty else { return .known(nil) }
        return value.utf8.count <= cap ? .known(value) : .unavailable("oversize")
    }
    private static func millis(_ value: Date?) -> Integer {
        guard let value else { return .known(nil) }
        let raw = value.timeIntervalSince1970 * 1000
        guard raw.isFinite, abs(raw) <= 9_007_199_254_740_990 else { return .unavailable("invalid") }
        return .known(Int64(raw.rounded()))
    }
    private static func power(_ raw: String) -> Number {
        guard !raw.isEmpty else { return .known(nil) }
        guard raw.utf8.count <= 64,
              raw.range(of: #"^[0-9]+(\.[0-9]+)?$"#, options: .regularExpression) != nil,
              let value = Double(raw), value.isFinite, value >= 0 else { return .unavailable("invalid") }
        return .known(value)
    }
    private static func frequency(_ raw: Int64?) -> Integer {
        guard let raw else { return .known(nil) }
        // Retain representable historical facts even when they cannot be used
        // for an edit or tuning. CompanionQSO separately validates editability.
        return (-9_007_199_254_740_990...9_007_199_254_740_990).contains(raw) ? .known(raw) : .unavailable("invalid")
    }
    private static func absent(_ read: CompanionContactDetailRead, reason: String? = nil) -> Self {
        Self(contact_id: read.contactID.uuidString.lowercased(), status: read.status,
             reason: reason ?? read.reason, logbook_revision: read.logRevision,
             contact_revision: read.contactRevision, fields: nil,
             editability: .init(allowed: false, reason: reason ?? read.reason ?? read.status.rawValue, allowed_fields: [], base: nil))
    }
    /// `allowed` means owner core admissibility only, not host/lease authority.
    /// Host must AND it with current authenticated bindings and controls policy.
    public static func project(_ read: CompanionContactDetailRead) -> Self {
        guard read.status == .found else { return absent(read) }
        guard let current = read.snapshot, current.qso.id == read.contactID,
              read.contactRevision == current.revision, current.revision >= 0, read.logRevision >= 0,
              current.qso.deletedAt == nil else {
            return absent(.init(contactID: read.contactID, logRevision: read.logRevision,
                contactRevision: nil, status: .unavailable, reason: "invalid_read_binding", snapshot: nil))
        }
        let q = current.qso
        func extra(_ key: String) -> Text { text(q.extraFields[key]) }
        func confirmation(_ source: String, sent: String?, received: String?, sentDate: String,
                          receivedDate: String, sentVia: String?, receivedVia: String?) -> Confirmation {
            .init(source: .known(source), evidence_kind: .known("retained_contact_field"),
                  sent_status: text(sent), sent_date: extra(sentDate), sent_via: source == "paper" ? text(sentVia) : .unavailable(),
                  received_status: text(received), received_date: extra(receivedDate), received_via: source == "paper" ? text(receivedVia) : .unavailable(),
                  match_age_seconds: .unavailable("not_recorded"))
        }
        let unsupportedConfirmation = Confirmation(source: .known("qrz"), evidence_kind: .unavailable(),
            sent_status: .unavailable(), sent_date: .unavailable(), sent_via: .unavailable(),
            received_status: .unavailable(), received_date: .unavailable(), received_via: .unavailable(), match_age_seconds: .unavailable())
        func upload(_ source: String) -> Upload {
            // No receipt projection yet: a retained ADIF sent flag is NOT an upload receipt.
            .init(source: .known(source), state: .unavailable("receipt_not_projected"),
                  receipt_revision: .unavailable("receipt_not_projected"), last_attempt_at_unix_ms: .unavailable("receipt_not_projected"),
                  is_current_revision: .unavailable("receipt_not_projected"))
        }
        let start = millis(q.start), end = millis(q.end)
        let fields = Fields(contact: .init(callsign: text(q.call), band: text(q.band), mode: text(q.mode), submode: text(q.submode),
            started_at_unix_ms: start, ended_at_unix_ms: end, stored_time_unit: .known("unix_milliseconds"),
            original_start_precision: .unavailable("not_recorded"), original_end_precision: .unavailable("not_recorded"),
            raw_qso_date: extra("QSO_DATE"), raw_time_on: extra("TIME_ON"), raw_qso_date_off: extra("QSO_DATE_OFF"), raw_time_off: extra("TIME_OFF"),
            rst_sent: text(q.rstSent), rst_received: text(q.rstReceived), grid: text(q.gridSquare), comment: text(q.comment), notes: text(q.notes, cap: 16384)),
            radio: .init(frequency_hz: frequency(q.frequencyHz), receive_frequency_hz: frequency(q.receiveFrequencyHz),
                receive_band: text(q.receiveBand), tx_power_raw: text(q.txPower), tx_power_w: power(q.txPower),
                propagation_mode: text(q.propagationMode), antenna_path: text(q.antennaPath), satellite_name: extra("SAT_NAME")),
            station: .init(name: text(q.name), qth: text(q.qth), state: text(q.state), county: text(q.county), country: text(q.country),
                dxcc: .known(q.dxcc.map(Int64.init)), continent: text(q.continent), cq_zone: text(q.cqZone), itu_zone: text(q.ituZone)),
            own_station: .init(operator_call: text(q.operatorCall), callsign: text(q.myCall), grid: text(q.myGrid),
                recorded_rig: q.extraFields["MY_RIG"] == nil ? .unavailable("not_recorded") : extra("MY_RIG"),
                radio_label: text(q.radio), station_profile_id: .known(q.stationProfileID.map(Int64.init)), station_location: text(q.stationLocation)),
            references: .init(their: .init(pota: text(q.potaReference), sota: text(q.sotaReference), wwff: text(q.wwffReference),
                iota: text(q.iota), sig: text(q.sig), sig_info: text(q.sigInfo)),
                my: .init(pota: extra("MY_POTA_REF"), sota: extra("MY_SOTA_REF"), wwff: extra("MY_WWFF_REF"), iota: extra("MY_IOTA"), sig: extra("MY_SIG"), sig_info: extra("MY_SIG_INFO")), dok: text(q.dok)),
            confirmations: .init(paper: confirmation("paper", sent: q.qslSent, received: q.qslReceived, sentDate: "QSLSDATE", receivedDate: "QSLRDATE",
                sentVia: q.qslMethod, receivedVia: q.extraFields["QSL_RCVD_VIA"]),
                lotw: confirmation("lotw", sent: q.lotwSent, received: q.lotwReceived, sentDate: "LOTW_QSLSDATE", receivedDate: "LOTW_QSLRDATE", sentVia: nil, receivedVia: nil),
                eqsl: confirmation("eqsl", sent: q.extraFields["EQSL_QSL_SENT"], received: q.extraFields["EQSL_QSL_RCVD"], sentDate: "EQSL_QSLSDATE", receivedDate: "EQSL_QSLRDATE", sentVia: nil, receivedVia: nil), qrz: unsupportedConfirmation),
            awards: .init(credit_granted: extra("CREDIT_GRANTED"), credit_submitted: extra("CREDIT_SUBMITTED"), authority: .unavailable("sponsor_authority_unknown")),
            uploads: .init(lotw: upload("lotw"), eqsl: upload("eqsl"), qrz: upload("qrz"), clublog: upload("clublog")),
            contest: .init(contest_id: extra("CONTEST_ID"), stx: extra("STX"), srx: extra("SRX"), stx_string: extra("STX_STRING"), srx_string: extra("SRX_STRING")))
        var base: CompanionQSO?
        if current.revision > 0, let ms = start.value {
            let candidate = CompanionQSO(contact_id: q.id, callsign: q.call, band: q.band, mode: q.mode,
                started_at_unix_ms: ms, frequency_hz: q.frequencyHz, rst_sent: q.rstSent, rst_received: q.rstReceived,
                grid: q.gridSquare, comment: q.comment)
            // Validate exact complete core and prove an unchanged roundtrip would
            // not normalize any stored value or erase fractional time precision.
            if (try? candidate.validate()) != nil, candidate.applying(to: q) == q { base = candidate }
        }
        return Self(contact_id: q.id.uuidString.lowercased(), status: .found, reason: nil, logbook_revision: read.logRevision,
            contact_revision: current.revision, fields: fields,
            editability: .init(allowed: base != nil, reason: base != nil ? "complete_core" : current.revision == 0 ? "legacy_revision_zero" : "core_not_lossless",
                allowed_fields: base == nil ? [] : ["started_at_unix_ms", "frequency_hz", "rst_sent", "rst_received", "grid", "comment"], base: base))
    }
    public static func encodeBounded(_ read: CompanionContactDetailRead) throws -> Data {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(project(read))
        if data.count <= 120 * 1024 { return data } // reserve 8 KiB for bound host envelope
        return try encoder.encode(absent(.init(contactID: read.contactID, logRevision: read.logRevision,
            contactRevision: read.contactRevision, status: .unavailable, reason: "detail_too_large", snapshot: nil)))
    }
}
