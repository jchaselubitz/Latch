import Foundation

/// The short-lived material a Latch Desktop QR code carries.
///
/// This is the wire shape of the Mac's `PairingMaterial`: the same camelCase
/// keys `latch remote-access pair` emits. It is one-time, expires five minutes
/// after the Mac made it, and the secret in it is authority to enroll a device,
/// so it is parsed defensively and never logged or described in full.
public struct PairingPayload: Equatable, Sendable {
    /// The only format this build understands.
    public static let supportedFormatVersion = 1
    /// Length in bytes of the identifiers and keys in the payload.
    static let pairingIdBytes = 16
    static let secretBytes = 32
    static let keyBytes = 32
    /// The Mac gives pairing five minutes. Anything claiming materially more
    /// than that did not come from a Latch of this contract version.
    static let maximumLifetime: TimeInterval = 5 * 60
    /// Slack for the phone and the Mac disagreeing about the time.
    static let clockSkew: TimeInterval = 60

    public let formatVersion: Int
    /// Opaque one-time pairing identifier, hex.
    public let pairingId: String
    /// The one-time secret. Held only as long as the enrollment call needs it.
    public let secret: String
    /// The Mac's pinned public identity, hex. Every later connection verifies
    /// the peer against this value, so it is the field pairing exists to move.
    public let macPublicKey: String
    /// When confirmation stops being accepted.
    public let expiresAt: Date
    /// Where to enroll. Absent on a Mac that pairs over the local network only,
    /// in which case the caller supplies the address it already has.
    public let controlPlane: URL?
    /// What the Mac calls itself, for the confirmation screen. Advisory: the
    /// key, not the name, is what is pinned.
    public let macName: String?

    /// How long the code is still good for, relative to `now`.
    public func remainingLifetime(now: Date = Date()) -> TimeInterval {
        expiresAt.timeIntervalSince(now)
    }

    /// A safe rendering: the identifier and Mac key are shortened, and the
    /// secret does not appear at all.
    public var redactedDescription: String {
        "pairing \(HexCoding.abbreviate(pairingId)) for Mac \(HexCoding.abbreviate(macPublicKey))"
    }
}

/// Why a scanned code was refused.
///
/// These are separate cases rather than one string because the scanner screen
/// says something different for each: an expired code is re-generated on the
/// Mac, a foreign QR code is simply the wrong code, and a version mismatch is
/// an update.
public enum PairingPayloadError: Error, Equatable, Sendable {
    /// Not JSON, or not a JSON object.
    case notPairingMaterial
    /// A Latch payload, but from a contract this build does not implement.
    case unsupportedFormat(found: Int, supported: Int)
    /// A field is missing or malformed. The detail names the field only.
    case malformedField(String)
    /// The five minutes are up.
    case expired(by: TimeInterval)
    /// The expiry is further out than the Mac is allowed to grant, which means
    /// the payload was not produced by the pairing flow it claims to be.
    case implausibleLifetime

    public var message: String {
        switch self {
        case .notPairingMaterial:
            return "That is not a Latch pairing code."
        case .unsupportedFormat(let found, let supported):
            return """
            This pairing code is version \(found); this app understands \(supported). \
            Update the app or the `latch` CLI so both sides agree.
            """
        case .malformedField(let field):
            return "This pairing code is damaged: \(field) is missing or invalid."
        case .expired(let by):
            let seconds = Int(by.rounded())
            return """
            This pairing code expired \(seconds < 60 ? "\(max(seconds, 1))s" : "\(seconds / 60)m") ago. \
            Show a new one on your Mac.
            """
        case .implausibleLifetime:
            return "This pairing code claims to last longer than Latch allows. It was not made by your Mac."
        }
    }
}

extension PairingPayload {
    /// Parses and validates one scanned string.
    ///
    /// Validation happens here rather than at use because the scanner is a
    /// firehose: it re-reads the same code many times a second, and every one
    /// of those reads is untrusted input from whatever was in front of the
    /// camera. `now` is injected so expiry is testable.
    public static func parse(_ scanned: String, now: Date = Date()) throws -> PairingPayload {
        let trimmed = scanned.trimmingCharacters(in: .whitespacesAndNewlines)
        guard
            let data = trimmed.data(using: .utf8),
            let root = try? JSONSerialization.jsonObject(with: data),
            let object = root as? [String: Any]
        else {
            throw PairingPayloadError.notPairingMaterial
        }

        // The version gate comes before every other field: a future format may
        // reuse these names with different meanings, and guessing at that is
        // exactly what the contract rules forbid.
        guard let version = object["formatVersion"] as? Int else {
            throw PairingPayloadError.notPairingMaterial
        }
        guard version == supportedFormatVersion else {
            throw PairingPayloadError.unsupportedFormat(
                found: version,
                supported: supportedFormatVersion
            )
        }

        let pairingId = try key(object, "pairingId", bytes: pairingIdBytes)
        let secret = try key(object, "secret", bytes: secretBytes)
        let macPublicKey = try key(object, "macPublicKey", bytes: keyBytes)

        guard let expiry = object["expiresAt"] as? NSNumber else {
            throw PairingPayloadError.malformedField("expiresAt")
        }
        let expiresAt = Date(timeIntervalSince1970: expiry.doubleValue)

        var controlPlane: URL?
        if let address = object["controlPlane"] as? String, !address.isEmpty {
            guard
                let url = URL(string: address),
                let scheme = url.scheme?.lowercased(),
                scheme == "https" || scheme == "http",
                url.host != nil
            else {
                throw PairingPayloadError.malformedField("controlPlane")
            }
            controlPlane = url
        }

        var macName: String?
        if let name = object["macName"] as? String {
            let cleaned = name.trimmingCharacters(in: .whitespacesAndNewlines)
            // A name from a QR code is attacker-controlled text destined for
            // the confirmation screen. Bound it, and keep control characters
            // out of it, before it is ever rendered.
            guard cleaned.count <= 80, !cleaned.unicodeScalars.contains(where: \.properties.isDefaultIgnorableCodePoint),
                  cleaned.rangeOfCharacter(from: .controlCharacters) == nil
            else {
                throw PairingPayloadError.malformedField("macName")
            }
            macName = cleaned.isEmpty ? nil : cleaned
        }

        let payload = PairingPayload(
            formatVersion: version,
            pairingId: pairingId,
            secret: secret,
            macPublicKey: macPublicKey,
            expiresAt: expiresAt,
            controlPlane: controlPlane,
            macName: macName
        )
        try payload.checkLifetime(now: now)
        return payload
    }

    /// Rechecks expiry. The scan and the enrollment call are separated by a
    /// confirmation the user has to read, and five minutes is short enough
    /// that the code can lapse in between.
    public func checkLifetime(now: Date = Date()) throws {
        let remaining = remainingLifetime(now: now)
        guard remaining > 0 else {
            throw PairingPayloadError.expired(by: -remaining)
        }
        guard remaining <= Self.maximumLifetime + Self.clockSkew else {
            throw PairingPayloadError.implausibleLifetime
        }
    }

    private static func key(_ object: [String: Any], _ name: String, bytes: Int) throws -> String {
        guard let value = object[name] as? String, HexCoding.isKey(value, bytes: bytes) else {
            throw PairingPayloadError.malformedField(name)
        }
        return value.lowercased()
    }
}
