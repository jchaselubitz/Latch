import Foundation

/// An arbitrary JSON document.
///
/// The harness event contract deliberately leaves reduced tool `input` and
/// `output` unconstrained, so the client cannot know their shape ahead of time.
/// Decoding them into a value tree rather than `Any` keeps `HarnessEvent`
/// `Sendable` and `Equatable`, which the transcript store relies on.
public enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "value is not JSON"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    /// The value at `key`, when this is an object.
    public subscript(key: String) -> JSONValue? {
        guard case .object(let fields) = self else { return nil }
        return fields[key]
    }

    /// A one-line rendering for a transcript row.
    ///
    /// Tool payloads are already reduced by the connector, but they can still
    /// be long enough to dominate a phone screen, so this stays deliberately
    /// terse and lets the detail view show more.
    public var summary: String {
        switch self {
        case .null: return ""
        case .bool(let value): return value ? "true" : "false"
        case .string(let value): return value
        case .number(let value):
            return value == value.rounded() && abs(value) < 1e15
                ? String(Int(value))
                : String(value)
        case .array(let values):
            return values.map(\.summary).joined(separator: ", ")
        case .object(let fields):
            // Prefer the fields a person recognizes a tool call by.
            for key in ["command", "description", "file_path", "path", "pattern", "prompt"] {
                if let value = fields[key], case .string(let text) = value {
                    return text
                }
            }
            return fields.keys.sorted().joined(separator: ", ")
        }
    }
}
