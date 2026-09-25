import Foundation

/// One file waiting in the composer to go out with the next message.
///
/// Attachments use workspace file handoff: the file is uploaded to the Mac,
/// placed in the session's working directory, and the message names it by
/// path. The agent reads it from disk like any other file; nothing about the
/// conversation channel changes. Held in memory only, like the draft.
public struct ConversationAttachment: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case image
        case file
    }

    public let id: UUID
    /// The name suggested to the Mac, already reduced to its safe alphabet.
    public let name: String
    public let data: Data
    public let kind: Kind
    /// A small encoded image for the composer chip, made once when the file
    /// is added so the chip never decodes the full file.
    public let thumbnail: Data?
    /// Set once the Mac has the file, so a send that fails after uploading
    /// does not upload it again when retried.
    public internal(set) var receipt: AttachmentReceipt?

    public init(id: UUID = UUID(), name: String, data: Data, kind: Kind, thumbnail: Data? = nil) {
        self.id = id
        self.name = Self.suggestedName(name)
        self.data = data
        self.kind = kind
        self.thumbnail = thumbnail
    }

    public var byteCount: Int { data.count }

    /// Reduces a file name to `[A-Za-z0-9._-]` with no runs of `.` or `-`,
    /// the same shape the Mac keeps. Doing it here as well keeps the request
    /// target free of `..`, which the paired link refuses outright.
    public static func suggestedName(_ raw: String) -> String {
        let last = raw.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
        var mapped = ""
        for scalar in last.unicodeScalars {
            let next: Character
            if scalar.isASCII, CharacterSet.alphanumerics.contains(scalar) || scalar == "_" {
                next = Character(scalar)
            } else if scalar == "." {
                next = "."
            } else {
                next = "-"
            }
            if (next == "." || next == "-"), mapped.last == next { continue }
            mapped.append(next)
        }
        let trimmed = mapped.trimmingCharacters(in: CharacterSet(charactersIn: ".-"))
        return trimmed.isEmpty ? "attachment" : String(trimmed.prefix(80))
    }
}

/// What the composer shows about attachments beyond the chips themselves.
public enum ConversationAttachmentPhase: Equatable, Sendable {
    case idle
    /// Files are going to the Mac; the message follows when they land.
    case uploading
    /// Nothing was sent. The reason is a sentence; the draft and the files
    /// are still in the composer.
    case failed(String)
}

/// Places a file in a session's workspace. `LatchGateway` is the real one;
/// tests substitute their own to control the sequence.
public protocol ConversationAttachmentUploading: Sendable {
    func uploadAttachment(sessionID: String, name: String, data: Data) async throws -> AttachmentReceipt
}

extension LatchGateway: ConversationAttachmentUploading {}

/// How a message names the files that went with it.
///
/// Plain text an agent reads the way a person would: the message first, then
/// the absolute path of each file on its own line, so the agent can open it
/// with its ordinary file tools from whatever directory it is in.
public enum ConversationAttachmentMessage {
    public static func compose(text: String, paths: [String]) -> String {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !paths.isEmpty else { return text }
        let reference: String
        if paths.count == 1 {
            reference = "Attached file: \(paths[0])"
        } else {
            reference = (["Attached files:"] + paths.map { "- \($0)" }).joined(separator: "\n")
        }
        return text.isEmpty ? reference : text + "\n\n" + reference
    }
}
