import Foundation

/// One exact operating-callsign row and its revision, read on the existing owner.
/// Historical coordinates are not a current station-position observation.
public struct CompanionBearingGridRead: Sendable {
    public struct Contact: Sendable {
        public let id: UUID
        public let revision: Int64
        public let call: String
        public let grid: String
        public let startedAt: Date
        public init(id: UUID, revision: Int64, call: String, grid: String, startedAt: Date) {
            self.id = id; self.revision = revision; self.call = call
            self.grid = grid; self.startedAt = startedAt
        }
    }
    public let logRevision: Int64
    public let contact: Contact?
    public let unavailableReason: String?
    public init(logRevision: Int64, contact: Contact?, unavailableReason: String? = nil) {
        self.logRevision = logRevision; self.contact = contact; self.unavailableReason = unavailableReason
    }
}

public enum CompanionBearingError: Error { case invalidCall, invalidRevision, oversized }

/// Passive geometry only. Its result never authorizes a turn or transmission.
public struct CompanionBearing: Encodable, Sendable {
    public struct Origin: Encodable, Sendable {
        public let source = "saved_station_settings"
        public let grid: String
        public let latitude, longitude: Double
    }
    public struct Target: Encodable, Sendable {
        public let source: String
        public let grid: String?
        public let latitude, longitude: Double
        public let precision: String
        public let contactID: UUID?
        public let contactRevision, contactStartedAtUnixMS: Int64?
        public let referenceRevision, matchedReferenceRevision, matchedRule, countryStatus: String?
        enum CodingKeys: String, CodingKey {
            case source, grid, latitude, longitude, precision
            case contactID = "contact_id", contactRevision = "contact_revision"
            case contactStartedAtUnixMS = "contact_started_at_unix_ms"
            case referenceRevision = "reference_revision", matchedReferenceRevision = "matched_reference_revision"
            case matchedRule = "matched_rule", countryStatus = "country_status"
        }
        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(source, forKey: .source); try c.encode(grid, forKey: .grid)
            try c.encode(latitude, forKey: .latitude); try c.encode(longitude, forKey: .longitude)
            try c.encode(precision, forKey: .precision)
            try c.encode(contactID?.uuidString.lowercased(), forKey: .contactID)
            try c.encode(contactRevision, forKey: .contactRevision)
            try c.encode(contactStartedAtUnixMS, forKey: .contactStartedAtUnixMS)
            try c.encode(referenceRevision, forKey: .referenceRevision)
            try c.encode(matchedReferenceRevision, forKey: .matchedReferenceRevision)
            try c.encode(matchedRule, forKey: .matchedRule); try c.encode(countryStatus, forKey: .countryStatus)
        }
    }
    public struct Geometry: Encodable, Sendable {
        public let shortPathDegrees, longPathDegrees, distanceKilometres: Double
        enum CodingKeys: String, CodingKey {
            case shortPathDegrees = "short_path_deg", longPathDegrees = "long_path_deg"
            case distanceKilometres = "distance_km"
        }
    }
    public let requestedCallsign, normalizedCallsign: String
    public let status: String
    public let reason: String?
    public let origin: Origin?
    public let target: Target?
    public let geometry: Geometry?
    public let logbookRevision: Int64
    public var referenceRevision: String? { target?.referenceRevision }
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", requestedCallsign = "requested_callsign"
        case normalizedCallsign = "normalized_callsign", status, reason, origin, target, geometry
        case logbookRevision = "logbook_revision", referenceRevision = "reference_revision"
        case motionAuthority = "motion_authority", transmitAuthority = "transmit_authority"
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(1, forKey: .schemaVersion)
        try c.encode(requestedCallsign, forKey: .requestedCallsign)
        try c.encode(normalizedCallsign, forKey: .normalizedCallsign)
        try c.encode(status, forKey: .status); try c.encode(reason, forKey: .reason)
        try c.encode(origin, forKey: .origin); try c.encode(target, forKey: .target)
        try c.encode(geometry, forKey: .geometry); try c.encode(logbookRevision, forKey: .logbookRevision)
        try c.encode(referenceRevision, forKey: .referenceRevision)
        try c.encode(false, forKey: .motionAuthority); try c.encode(false, forKey: .transmitAuthority)
    }
    public static func normalizedCall(_ raw: String) -> String? {
        let bytes = Array(raw.utf8)
        guard (3...32).contains(bytes.count), bytes.allSatisfy({
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 47
        }), bytes.contains(where: { (65...90).contains($0) || (97...122).contains($0) }),
            bytes.contains(where: { (48...57).contains($0) }),
            bytes.first != 47, bytes.last != 47, !raw.contains("//") else { return nil }
        return raw.uppercased()
    }
    public static func normalizedGrid(_ grid: String) -> String? {
        coordinate(grid).map { _ in grid.uppercased() }
    }
    private static func coordinate(_ grid: String) -> GeoCoordinate? {
        guard [4, 6, 8].contains(grid.utf8.count), grid.utf8.allSatisfy({ $0 < 128 }) else { return nil }
        let bytes = Array(grid.uppercased().utf8)
        guard [4, 6, 8].contains(bytes.count), bytes.enumerated().allSatisfy({ i, b in
            switch i {
            case 0, 1: return (65...82).contains(b)
            case 4, 5: return (65...88).contains(b)
            default: return (48...57).contains(b)
            }
        }), let value = MaidenheadLocator.coordinate(grid) else { return nil }
        // The shared legacy locator helper resolves through six characters.
        // Preserve the extra precision of an eight-character saved locator.
        let latitude = value.latitude + (bytes.count == 8 ? (Double(bytes[7] - 48) - 4.5) / 240 : 0)
        let longitude = value.longitude + (bytes.count == 8 ? (Double(bytes[6] - 48) - 4.5) / 120 : 0)
        let result = GeoCoordinate(latitude: latitude, longitude: longitude)
        return valid(result) ? result : nil
    }
    private static func valid(_ point: GeoCoordinate) -> Bool {
        point.latitude.isFinite && point.longitude.isFinite && (-90...90).contains(point.latitude)
            && (-180...180).contains(point.longitude)
    }
    public static func project(call: String, originGrid: String, read: CompanionBearingGridRead,
                               resolver: CTYResolver?, at date: Date) throws -> Self {
        guard let normalized = normalizedCall(call) else { throw CompanionBearingError.invalidCall }
        let safe: Int64 = 9_007_199_254_740_990
        guard (0...safe).contains(read.logRevision) else { throw CompanionBearingError.invalidRevision }
        let originPoint = coordinate(originGrid)
        let origin = originPoint.map { Origin(grid: originGrid.uppercased(), latitude: $0.latitude, longitude: $0.longitude) }
        func result(_ reason: String?, _ target: Target? = nil, _ geometry: Geometry? = nil) -> Self {
            .init(requestedCallsign: call, normalizedCallsign: normalized, status: geometry == nil ? "unavailable" : "available",
                reason: reason, origin: origin, target: target, geometry: geometry, logbookRevision: read.logRevision)
        }
        guard let originPoint else { return result("origin_unavailable") }
        if let reason = read.unavailableReason {
            return result(["record_too_large", "invalid_record"].contains(reason) ? reason : "logbook_unavailable")
        }
        if let contact = read.contact {
            let stamp = contact.startedAt.timeIntervalSince1970 * 1000
            guard normalizedCall(contact.call) == normalized, (0...read.logRevision).contains(contact.revision),
                  stamp.isFinite, stamp >= -Double(safe), stamp <= Double(safe) else { return result("invalid_record") }
        }
        let target: Target
        if let contact = read.contact, let destination = coordinate(contact.grid) {
            let stamp = contact.startedAt.timeIntervalSince1970 * 1000
            target = .init(source: "exact_logbook_grid", grid: contact.grid.uppercased(),
                latitude: destination.latitude, longitude: destination.longitude, precision: "historical_grid_cell_center",
                contactID: contact.id, contactRevision: contact.revision, contactStartedAtUnixMS: Int64(stamp.rounded(.down)),
                referenceRevision: nil, matchedReferenceRevision: nil, matchedRule: nil, countryStatus: nil)
        } else {
            guard let resolver else { return result("reference_unavailable") }
            let facts = resolver.facts(normalized, at: date)
            guard facts.status == .resolved else {
                let reason: String
                switch facts.status {
                case .ambiguous: reason = "country_ambiguous"
                case .knownNoEntity: reason = "country_known_no_entity"
                case .invalid: reason = "country_invalid"
                case .missingDate: reason = "country_missing_date"
                default: reason = "country_unknown"
                }
                return result(reason)
            }
            guard let country = facts.country, let lat = country.latitude, let lon = country.longitude,
                  valid(.init(latitude: lat, longitude: lon)) else { return result("target_coordinates_unavailable") }
            guard facts.referenceRevision.utf8.count <= 2048,
                  (facts.matchedReferenceRevision?.utf8.count ?? 0) <= 2048,
                  (facts.matchedRule?.utf8.count ?? 0) <= 128 else { return result("reference_metadata_invalid") }
            target = .init(source: "cty_entity_approximation", grid: nil, latitude: lat, longitude: lon,
                precision: "entity_approximation", contactID: nil, contactRevision: nil, contactStartedAtUnixMS: nil,
                referenceRevision: facts.referenceRevision, matchedReferenceRevision: facts.matchedReferenceRevision,
                matchedRule: facts.matchedRule, countryStatus: facts.status.rawValue)
        }
        let destination = GeoCoordinate(latitude: target.latitude, longitude: target.longitude)
        let lat1 = originPoint.latitude * .pi / 180, lat2 = destination.latitude * .pi / 180
        let delta = (destination.longitude - originPoint.longitude) * .pi / 180
        let dot = sin(lat1) * sin(lat2) + cos(lat1) * cos(lat2) * cos(delta)
        guard 1 - dot > 1e-12 else { return result("coincident", target) }
        guard 1 + dot > 1e-12 else { return result("antipodal", target) }
        let value = GreatCircle.solve(from: originPoint, to: destination)
        guard value.distanceKilometres.isFinite, value.distanceKilometres >= 0,
              value.shortPathBearing.isFinite, (0..<360).contains(value.shortPathBearing),
              value.longPathBearing.isFinite, (0..<360).contains(value.longPathBearing) else { return result("invalid_geometry", target) }
        return result(nil, target, .init(shortPathDegrees: value.shortPathBearing,
            longPathDegrees: value.longPathBearing, distanceKilometres: value.distanceKilometres))
    }
    public func encoded() throws -> Data {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(self)
        guard data.count <= 16 * 1024 else { throw CompanionBearingError.oversized }
        return data
    }
}
