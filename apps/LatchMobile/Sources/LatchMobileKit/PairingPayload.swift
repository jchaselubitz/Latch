import Foundation

/// QR-only inputs for Remote Link v1 enrollment. The admission code selects
/// one provisional service record; the independent 256-bit secret binds the
/// Noise transcript and is never sent to that service.
public struct PairingPayload: Equatable, Sendable {
    public static let supportedFormatVersion = 1
    static let maximumLifetime: TimeInterval = 5 * 60
    static let clockSkew: TimeInterval = 60

    public let version: Int
    public let controlPlane: URL
    public let enrollmentId: String
    public let hostPublicKey: String
    public let admissionCode: String
    public let enrollmentSecret: String
    public let expiresAt: Date
    public let macName: String

    public func remainingLifetime(now: Date = Date()) -> TimeInterval {
        expiresAt.timeIntervalSince(now)
    }

    public var redactedDescription: String {
        "enrollment \(HexCoding.abbreviate(enrollmentId)) for Mac \(HexCoding.abbreviate(hostPublicKey))"
    }
}

public enum PairingPayloadError: Error, Equatable, Sendable {
    case notPairingMaterial
    case unsupportedFormat(found: Int, supported: Int)
    case malformedField(String)
    case expired(by: TimeInterval)
    case implausibleLifetime

    public var message: String {
        switch self {
        case .notPairingMaterial: return "That is not a Latch Remote Link code."
        case .unsupportedFormat(let found, let supported):
            return "This code is version \(found); this app understands \(supported). Update Latch on both devices."
        case .malformedField(let field):
            return "This code is damaged: \(field) is missing or invalid."
        case .expired(let by):
            return "This code expired \(max(Int(by.rounded()), 1)) seconds ago. Show a new one on your Mac."
        case .implausibleLifetime:
            return "This code lasts longer than Remote Link allows. Do not use it."
        }
    }
}

extension PairingPayload {
    public static func parse(_ scanned: String, now: Date = Date()) throws -> PairingPayload {
        let trimmed = scanned.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let data = trimmed.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { throw PairingPayloadError.notPairingMaterial }
        guard let version = object["version"] as? Int else {
            throw PairingPayloadError.notPairingMaterial
        }
        guard version == supportedFormatVersion else {
            throw PairingPayloadError.unsupportedFormat(found: version, supported: supportedFormatVersion)
        }
        guard let address = object["controlPlane"] as? String,
              let controlPlane = URL(string: address),
              let scheme = controlPlane.scheme?.lowercased(),
              scheme == "https" || (scheme == "http" && Self.isLoopback(controlPlane.host)),
              controlPlane.host != nil
        else { throw PairingPayloadError.malformedField("controlPlane") }
        let enrollmentId = try requiredString(object, "enrollmentId")
        guard enrollmentId.range(of: #"^enr_[0-9a-f]{32}$"#, options: .regularExpression) != nil else {
            throw PairingPayloadError.malformedField("enrollmentId")
        }
        let hostPublicKey = try hex(object, "hostPublicKey", bytes: 32)
        let admissionCode = try requiredString(object, "admissionCode")
        guard admissionCode.range(of: #"^enr_[0-9a-f]{32}\.[0-9a-f]{64}$"#, options: .regularExpression) != nil,
              admissionCode.hasPrefix(enrollmentId + ".") else {
            throw PairingPayloadError.malformedField("admissionCode")
        }
        let enrollmentSecret = try hex(object, "enrollmentSecret", bytes: 32)
        guard let expiry = object["expiresAt"] as? NSNumber else {
            throw PairingPayloadError.malformedField("expiresAt")
        }
        let rawName = try requiredString(object, "macName")
        let macName = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !macName.isEmpty, macName.count <= 80,
              macName.rangeOfCharacter(from: .controlCharacters) == nil else {
            throw PairingPayloadError.malformedField("macName")
        }
        let payload = PairingPayload(
            version: version,
            controlPlane: controlPlane,
            enrollmentId: enrollmentId,
            hostPublicKey: hostPublicKey,
            admissionCode: admissionCode,
            enrollmentSecret: enrollmentSecret,
            expiresAt: Date(timeIntervalSince1970: expiry.doubleValue),
            macName: macName
        )
        try payload.checkLifetime(now: now)
        return payload
    }

    public func checkLifetime(now: Date = Date()) throws {
        let remaining = remainingLifetime(now: now)
        guard remaining > 0 else { throw PairingPayloadError.expired(by: -remaining) }
        guard remaining <= Self.maximumLifetime + Self.clockSkew else {
            throw PairingPayloadError.implausibleLifetime
        }
    }

    private static func hex(_ object: [String: Any], _ name: String, bytes: Int) throws -> String {
        guard let value = object[name] as? String, HexCoding.isKey(value, bytes: bytes) else {
            throw PairingPayloadError.malformedField(name)
        }
        return value.lowercased()
    }

    private static func requiredString(_ object: [String: Any], _ name: String) throws -> String {
        guard let value = object[name] as? String else {
            throw PairingPayloadError.malformedField(name)
        }
        return value
    }

    private static func isLoopback(_ host: String?) -> Bool {
        host == "127.0.0.1" || host == "localhost" || host == "::1"
    }
}
