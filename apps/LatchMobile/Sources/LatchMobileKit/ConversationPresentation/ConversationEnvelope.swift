import Foundation

/// A user-role message the person did not write as prose: the agent's harness
/// wraps background-task results, messages from other agents and sessions,
/// shell escapes, and slash commands in tagged markup and files them as user
/// turns. Shown raw, that markup reads as a wall of XML in the person's own
/// bubble; recognised, it becomes a card that says what happened.
///
/// Recognition keys on the markup in the message text only — never on a
/// connector id — and fails closed: text that does not start with one of the
/// known envelopes (or a single self-closing kebab-case element spanning the
/// whole message) stays an ordinary message.
public struct ConversationEnvelope: Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        /// A background agent, shell command, remote task, or monitor
        /// reporting back (`<task-notification>`).
        case backgroundTask(ConversationBackgroundTaskKind)
        /// Another agent, teammate, session, channel, or coordinator
        /// writing to this one.
        case peerMessage
        /// A shell command the person ran from the composer (`!cmd`).
        case shellCommand
        /// A slash command the person ran (`/model …`).
        case slashCommand
        /// Output a local slash command printed.
        case commandOutput
        /// The agent woke itself on a schedule (`<tick>`).
        case scheduledWake
        /// Any other harness note: hook output, reminders, fetched content.
        case harnessNote
    }

    public enum Tone: Equatable, Sendable {
        case neutral
        case running
        case success
        case failure
        case attention
    }

    /// Who caused the message: the person at the composer, or the agent's
    /// own machinery. A person's envelope stays a trailing prompt; the
    /// agent's is drawn as an event card.
    public enum Origin: Equatable, Sendable {
        case person
        case agent
    }

    public struct Detail: Equatable, Sendable, Identifiable {
        public let label: String
        public let value: String
        public var id: String { label }

        public init(_ label: String, _ value: String) {
            self.label = label
            self.value = value
        }
    }

    public let kind: Kind
    /// The root tag, for tests and accessibility identifiers.
    public let tag: String
    /// Small caps line above the title: what sort of event this is.
    public let eyebrow: String
    /// The subject: an agent's description, a sender, a command line.
    public let title: String
    /// A one-word state badge (Failed, Completed, Stopped…).
    public let status: String?
    public let tone: Tone
    /// One-sentence account of what happened, under the title.
    public let headline: String?
    /// The longer content: an agent's result, a message body, command output.
    public let body: String?
    /// True when `body` is terminal output rather than prose.
    public let bodyIsCode: Bool
    /// Identifiers and bookkeeping, shown behind a disclosure.
    public let details: [Detail]
    public let origin: Origin

    public init(
        kind: Kind,
        tag: String,
        eyebrow: String,
        title: String,
        status: String? = nil,
        tone: Tone = .neutral,
        headline: String? = nil,
        body: String? = nil,
        bodyIsCode: Bool = false,
        details: [Detail] = [],
        origin: Origin = .agent
    ) {
        self.kind = kind
        self.tag = tag
        self.eyebrow = eyebrow
        self.title = title
        self.status = status
        self.tone = tone
        self.headline = headline
        self.body = body
        self.bodyIsCode = bodyIsCode
        self.details = details
        self.origin = origin
    }

    /// An SF Symbol for the card's leading icon.
    public var symbolName: String {
        switch kind {
        case .backgroundTask(let task):
            switch task {
            case .agent: "person.2.circle"
            case .command: "terminal"
            case .remoteTask: "icloud"
            case .monitor: "waveform.path.ecg"
            case .task: "square.stack.3d.up"
            }
        case .peerMessage: "bubble.left.and.bubble.right"
        case .shellCommand: "terminal"
        case .slashCommand: "slash.circle"
        case .commandOutput: "text.alignleft"
        case .scheduledWake: "alarm"
        case .harnessNote: "info.circle"
        }
    }

    /// The text a one-line preview should use instead of the raw markup.
    public var previewText: String {
        [eyebrow, title, status].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

public enum ConversationBackgroundTaskKind: Equatable, Sendable {
    case agent
    case command
    case remoteTask
    case monitor
    case task

    var eyebrow: String {
        switch self {
        case .agent: "Agent"
        case .command: "Background command"
        case .remoteTask: "Remote task"
        case .monitor: "Monitor"
        case .task: "Background task"
        }
    }
}

// MARK: - Parsing

extension ConversationEnvelope {
    /// The envelope `text` is, or nil when it is ordinary prose.
    public static func parse(_ text: String) -> ConversationEnvelope? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("<"), let root = EnvelopeMarkup.leadingTag(in: trimmed) else { return nil }
        switch root {
        case "task-notification":
            return taskNotification(trimmed)
        case "teammate-message", "cross-session-message", "agent-message", "channel",
             "coordinator-relay", "slack-ping", "slack-tag-message", "remote-review":
            return peerMessage(trimmed, tag: root)
        case "bash-input", "bash-stdout", "bash-stderr":
            return shellCommand(trimmed)
        case "command-name", "command-message", "command-args":
            return slashCommand(trimmed)
        case "local-command-stdout", "local-command-stderr":
            return commandOutput(trimmed)
        case "tick":
            return ConversationEnvelope(
                kind: .scheduledWake,
                tag: root,
                eyebrow: "Scheduled",
                title: "Agent woke up",
                body: EnvelopeMarkup.element(root, in: trimmed)?.body.nonEmpty
            )
        default:
            return harnessNote(trimmed, tag: root)
        }
    }

    // `<task-notification>`: one per background task that stopped, failed,
    // checked in, or emitted a monitor event.
    private static func taskNotification(_ text: String) -> ConversationEnvelope? {
        guard let outer = EnvelopeMarkup.element("task-notification", in: text, outermost: true) else { return nil }
        let inner = outer.body
        let field = { (name: String) in EnvelopeMarkup.element(name, in: inner)?.body.nonEmpty }
        let summary = field("summary")
        let subject = TaskSummary(summary)
        let explicitStatus = field("status")
        let (status, tone) = taskStatus(explicitStatus, summary: summary)

        var body = EnvelopeMarkup.element("result", in: inner, outermost: true)?.body.nonEmpty
        if body == nil { body = field("event") }

        var details: [Detail] = []
        if let note = field("note") { details.append(Detail("Note", note)) }
        if let usage = EnvelopeMarkup.element("usage", in: inner)?.body {
            if let tokens = EnvelopeMarkup.element("subagent_tokens", in: usage)?.body.nonEmpty {
                details.append(Detail("Tokens", tokens))
            }
            if let uses = EnvelopeMarkup.element("tool_uses", in: usage)?.body.nonEmpty {
                details.append(Detail("Tool uses", uses))
            }
            if let duration = EnvelopeMarkup.element("duration_ms", in: usage)?.body.nonEmpty,
               let milliseconds = Double(duration) {
                details.append(Detail("Duration", formatDuration(milliseconds / 1000)))
            }
        }
        if let worktree = EnvelopeMarkup.element("worktree", in: inner)?.body {
            if let path = EnvelopeMarkup.element("worktreePath", in: worktree)?.body.nonEmpty {
                details.append(Detail("Worktree", path))
            }
            if let branch = EnvelopeMarkup.element("worktreeBranch", in: worktree)?.body.nonEmpty {
                details.append(Detail("Branch", branch))
            }
        }
        if let taskType = field("task-type") { details.append(Detail("Type", taskType.replacingOccurrences(of: "_", with: " "))) }
        if let taskID = field("task-id") { details.append(Detail("Task", taskID)) }
        if let toolUse = field("tool-use-id") { details.append(Detail("Tool call", toolUse)) }
        if let output = field("output-file") { details.append(Detail("Output file", output)) }
        // Check-ins and stall notices put their guidance after the last tag.
        if let trailing = EnvelopeMarkup.trailingText(in: inner), body == nil {
            details.insert(Detail("Guidance", trailing), at: 0)
        }

        let kind: ConversationBackgroundTaskKind =
            field("task-type") == "remote_agent" ? .remoteTask : subject.kind
        return ConversationEnvelope(
            kind: .backgroundTask(kind),
            tag: "task-notification",
            eyebrow: kind.eyebrow,
            title: subject.name ?? summary ?? kind.eyebrow,
            status: status,
            tone: tone,
            headline: subject.name == nil ? nil : headline(subject.remainder, status: status),
            body: body,
            details: details
        )
    }

    /// The summary after the subject, without the verb the status badge
    /// already says: `failed: API error` → `API error`, `completed (exit
    /// code 0)` → `Exit code 0`.
    static func headline(_ remainder: String?, status: String?) -> String? {
        guard var text = remainder?.trimmingCharacters(in: .whitespaces), !text.isEmpty else { return nil }
        let verbs = ["failed", "completed", "finished", "was stopped", "stopped"]
        if status != nil, let verb = verbs.first(where: { text.lowercased().hasPrefix($0) }) {
            text = String(text.dropFirst(verb.count)).trimmingCharacters(in: CharacterSet(charactersIn: ": "))
            if text.hasPrefix("("), text.hasSuffix(")") { text = String(text.dropFirst().dropLast()) }
        }
        guard !text.isEmpty else { return nil }
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    private static func taskStatus(_ status: String?, summary: String?) -> (String?, Tone) {
        let normalized = status?.lowercased()
        switch normalized {
        case "completed": return ("Completed", .success)
        case "failed": return ("Failed", .failure)
        case "killed", "stopped", "cancelled": return ("Stopped", .attention)
        case "blocked": return ("Blocked", .attention)
        case "running": return ("Running", .running)
        case .some(let other): return (other.replacingOccurrences(of: "_", with: " ").capitalized, .neutral)
        case nil: break
        }
        // Agents and monitors often omit `<status>`; their summary says it.
        let summary = summary?.lowercased() ?? ""
        if summary.contains("\" failed") { return ("Failed", .failure) }
        if summary.contains("\" completed") || summary.contains("\" finished") { return ("Completed", .success) }
        if summary.contains("\" was resumed") { return ("Resumed", .running) }
        if summary.contains(" was stopped") || summary.contains("\" stopped") { return ("Stopped", .attention) }
        if summary.contains("still running") { return ("Still running", .running) }
        if summary.contains("waiting for interactive input") || summary.contains("is blocked") {
            return ("Needs input", .attention)
        }
        if summary.hasPrefix("monitor event:") { return ("Event", .neutral) }
        return (nil, .neutral)
    }

    private static func peerMessage(_ text: String, tag: String) -> ConversationEnvelope? {
        // Several teammate messages can arrive in one turn; show them in order.
        let elements = EnvelopeMarkup.elements(tag, in: text)
        guard let first = elements.first else { return nil }
        let body = elements.map(\.body).filter { !$0.isEmpty }.joined(separator: "\n\n").nonEmpty
        let attributes = first.attributes
        var details: [Detail] = []
        let eyebrow: String
        let title: String
        var headline: String?
        switch tag {
        case "teammate-message":
            eyebrow = elements.count > 1 ? "\(elements.count) teammate messages" : "Teammate"
            title = attributes["teammate_id"] ?? "Teammate"
            headline = attributes["summary"]
        case "cross-session-message":
            eyebrow = "Session message"
            title = attributes["from-name"] ?? attributes["from"] ?? "Another session"
            if let session = attributes["from-session"] { details.append(Detail("Session", session)) }
            if let mode = attributes["from-mode"] { details.append(Detail("Mode", mode)) }
            if let plugin = attributes["from-plugin"] { details.append(Detail("Plugin", plugin)) }
            if let from = attributes["from"], from != title { details.append(Detail("From", from)) }
        case "agent-message":
            eyebrow = "Agent message"
            title = attributes["from"] ?? "Agent"
        case "channel":
            eyebrow = "Channel"
            title = attributes["source"] ?? "Channel"
            for (key, value) in attributes.sorted(by: { $0.key < $1.key }) where key != "source" {
                details.append(Detail(key.replacingOccurrences(of: "_", with: " ").capitalized, value))
            }
        case "coordinator-relay":
            eyebrow = "Coordinator"
            title = "Coordinator session"
        case "remote-review":
            eyebrow = "Remote review"
            title = "Review update"
        default:
            eyebrow = "Slack"
            title = tag == "slack-ping" ? "Slack ping" : "Slack message"
        }
        return ConversationEnvelope(
            kind: .peerMessage,
            tag: tag,
            eyebrow: eyebrow,
            title: title,
            headline: headline,
            body: body,
            details: details
        )
    }

    private static func shellCommand(_ text: String) -> ConversationEnvelope {
        let input = EnvelopeMarkup.element("bash-input", in: text)?.body.nonEmpty
        let stdout = EnvelopeMarkup.element("bash-stdout", in: text)?.body.nonEmpty
        let stderr = EnvelopeMarkup.element("bash-stderr", in: text)?.body.nonEmpty
        let exitCode = EnvelopeMarkup.element("bash-exit-code", in: text)?.body.nonEmpty
        let failed = exitCode.map { $0 != "0" } ?? false
        let output = [stdout, stderr].compactMap { $0 }.joined(separator: "\n").nonEmpty
        return ConversationEnvelope(
            kind: .shellCommand,
            tag: input == nil ? "bash-stdout" : "bash-input",
            eyebrow: input == nil ? "Shell output" : "Shell",
            title: input.map { "$ \($0)" } ?? "Command output",
            status: failed ? "Exit \(exitCode!)" : nil,
            tone: failed ? .failure : .neutral,
            body: output,
            bodyIsCode: true,
            origin: .person
        )
    }

    private static func slashCommand(_ text: String) -> ConversationEnvelope {
        let rawName = EnvelopeMarkup.element("command-name", in: text)?.body.nonEmpty
            ?? EnvelopeMarkup.element("command-message", in: text)?.body.nonEmpty
            ?? "command"
        let name = rawName.hasPrefix("/") ? rawName : "/\(rawName)"
        let args = EnvelopeMarkup.element("command-args", in: text)?.body.nonEmpty
        let stdout = EnvelopeMarkup.element("local-command-stdout", in: text)?.body.nonEmpty
        let stderr = EnvelopeMarkup.element("local-command-stderr", in: text)?.body.nonEmpty
        return ConversationEnvelope(
            kind: .slashCommand,
            tag: "command-name",
            eyebrow: "Command",
            title: [name, args].compactMap { $0 }.joined(separator: " "),
            tone: stderr == nil ? .neutral : .failure,
            body: [stdout, stderr].compactMap { $0 }.joined(separator: "\n").nonEmpty,
            bodyIsCode: true,
            origin: .person
        )
    }

    private static func commandOutput(_ text: String) -> ConversationEnvelope {
        let stdout = EnvelopeMarkup.element("local-command-stdout", in: text)?.body.nonEmpty
        let stderr = EnvelopeMarkup.element("local-command-stderr", in: text)?.body.nonEmpty
        let output = [stdout, stderr].compactMap { $0 }.joined(separator: "\n")
        // Short confirmations ("Set model to …") read best as the title.
        let isShort = !output.contains("\n") && output.count <= 80
        return ConversationEnvelope(
            kind: .commandOutput,
            tag: stdout == nil ? "local-command-stderr" : "local-command-stdout",
            eyebrow: "Command output",
            title: isShort && !output.isEmpty ? output : (stderr == nil ? "Output" : "Error"),
            tone: stderr == nil ? .neutral : .failure,
            body: isShort ? nil : output.nonEmpty,
            bodyIsCode: true,
            origin: .person
        )
    }

    private static func harnessNote(_ text: String, tag: String) -> ConversationEnvelope? {
        let known = EnvelopeMarkup.knownNoteTitles[tag]
        // An unknown tag is only an envelope when it wraps the whole message
        // and is kebab-case like every harness tag; `<div>` typed by a person
        // stays prose.
        guard let element = EnvelopeMarkup.element(tag, in: text, outermost: true) else { return nil }
        if known == nil {
            guard tag.contains("-"), element.isWholeMessage else { return nil }
        }
        let title = known ?? tag.split(separator: "-").joined(separator: " ").capitalized
        return ConversationEnvelope(
            kind: .harnessNote,
            tag: tag,
            eyebrow: "Agent note",
            title: title,
            body: element.body.nonEmpty,
            origin: tag == "user-memory-input" ? .person : .agent
        )
    }

    static func formatDuration(_ seconds: Double) -> String {
        if seconds < 60 { return "\(Int(seconds.rounded()))s" }
        let minutes = Int(seconds / 60)
        let rest = Int(seconds.truncatingRemainder(dividingBy: 60).rounded())
        if minutes < 60 { return rest == 0 ? "\(minutes)m" : "\(minutes)m \(rest)s" }
        return "\(minutes / 60)h \(minutes % 60)m"
    }
}

/// `Agent "name" failed: …` → kind, the quoted name, and what follows it.
private struct TaskSummary {
    var kind: ConversationBackgroundTaskKind = .task
    var name: String?
    var remainder: String?

    init(_ summary: String?) {
        guard let summary else { return }
        let prefixes: [(String, ConversationBackgroundTaskKind)] = [
            ("Agent \"", .agent),
            ("Background command \"", .command),
            ("Remote task \"", .remoteTask),
            ("Monitor event: \"", .monitor),
            ("Task \"", .task),
        ]
        for (prefix, kind) in prefixes where summary.hasPrefix(prefix) {
            self.kind = kind
            let rest = summary.dropFirst(prefix.count)
            // The name ends at the last quote that is followed by a space or
            // the end: descriptions can contain quotes of their own.
            guard let close = Self.closingQuote(in: rest) else { return }
            name = String(rest[..<close]).nonEmpty
            let after = rest[rest.index(after: close)...].trimmingCharacters(in: .whitespaces)
            remainder = after.nonEmpty.map { $0.prefix(1).uppercased() + $0.dropFirst() }
            return
        }
        if summary.hasPrefix("Remote workflow") { kind = .remoteTask }
    }

    /// The name ends at the quote directly before the verb; descriptions
    /// can contain quotes of their own. Failing that, the last quote that
    /// ends a word.
    private static func closingQuote(in text: Substring) -> Substring.Index? {
        let verbs = [" failed", " completed", " finished", " was ", " is ", " appears", " stopped"]
        var fallback: Substring.Index?
        var index = text.startIndex
        while index < text.endIndex {
            if text[index] == "\"" {
                let tail = text[text.index(after: index)...]
                if tail.isEmpty || verbs.contains(where: tail.hasPrefix) { return index }
                if tail.first == " " || tail.first == ":" { fallback = index }
            }
            index = text.index(after: index)
        }
        return fallback
    }
}

// MARK: - Markup scanning

/// A tolerant scanner for the harness's tag vocabulary. It is not an XML
/// parser: bodies are free text that may itself contain `<`, so elements are
/// matched by name, the outermost close tag winning where content can nest.
enum EnvelopeMarkup {
    struct Element {
        let attributes: [String: String]
        let body: String
        let isWholeMessage: Bool
    }

    /// Notes that need a friendlier title than their tag.
    static let knownNoteTitles: [String: String] = [
        "system-reminder": "System reminder",
        "user-prompt-submit-hook": "Prompt hook output",
        "post-tool-use-hook": "Tool hook output",
        "local-command-caveat": "Local command caveat",
        "fetched-web-content": "Fetched web content",
        "persisted-output": "Saved output",
        "user-memory-input": "Saved to memory",
        "fork-boilerplate": "Fork instructions",
        "forked-skill-launch": "Skill launched in a fork",
        "mcp-resource-update": "MCP resource updated",
        "mcp-polling-update": "MCP update",
        "new-diagnostics": "New diagnostics",
        "artifact-type-instructions": "Artifact instructions",
        "artifact-file-content": "Artifact file",
        "artifact-room-event": "Artifact activity",
        "cowritten-artifact-html": "Artifact page",
        "artifact-origin-notes": "Artifact notes",
        "artifact-stored-declaration": "Artifact declaration",
        "remote-review-progress": "Remote review progress",
    ]

    /// The name of the tag `text` opens with, when it is a plausible
    /// harness tag (lowercase, digits, `-` or `_`).
    static func leadingTag(in text: String) -> String? {
        guard text.first == "<" else { return nil }
        var name = ""
        for character in text.dropFirst() {
            if character == ">" || character == " " || character == "\n" { break }
            guard character.isLowercase || character.isNumber || character == "-" || character == "_" else {
                return nil
            }
            name.append(character)
        }
        guard let first = name.first, first.isLetter else { return nil }
        return name
    }

    static func element(_ name: String, in text: String, outermost: Bool = false) -> Element? {
        elements(name, in: text, outermost: outermost, limit: 1).first
    }

    static func elements(_ name: String, in text: String, outermost: Bool = false, limit: Int = .max) -> [Element] {
        var found: [Element] = []
        var searchStart = text.startIndex
        let open = "<\(name)"
        let close = "</\(name)>"
        while found.count < limit, let openRange = text.range(of: open, range: searchStart..<text.endIndex) {
            // The tag name must end here: `<task-id` is not `<task`.
            guard openRange.upperBound < text.endIndex else { break }
            let terminator = text[openRange.upperBound]
            guard terminator == ">" || terminator == " " || terminator == "\n" else {
                searchStart = openRange.upperBound
                continue
            }
            guard let headEnd = text.range(of: ">", range: openRange.upperBound..<text.endIndex) else { break }
            let head = text[openRange.upperBound..<headEnd.lowerBound]
            let closeRange = outermost
                ? text.range(of: close, options: .backwards, range: headEnd.upperBound..<text.endIndex)
                : text.range(of: close, range: headEnd.upperBound..<text.endIndex)
            guard let closeRange else { break }
            let body = String(text[headEnd.upperBound..<closeRange.lowerBound])
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let whole = openRange.lowerBound == text.startIndex
                && text[closeRange.upperBound...].allSatisfy(\.isWhitespace)
            found.append(Element(attributes: attributes(in: head), body: body, isWholeMessage: whole))
            searchStart = closeRange.upperBound
        }
        return found
    }

    /// Free text after the last closing tag of `body`, if any.
    static func trailingText(in body: String) -> String? {
        guard let last = body.range(of: "</", options: .backwards),
              let end = body.range(of: ">", range: last.upperBound..<body.endIndex) else { return nil }
        return String(body[end.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines).nonEmpty
    }

    /// `key="value"` pairs. Values are quoted by the harness, which escapes
    /// `"` inside them, so a simple scan suffices.
    static func attributes(in head: Substring) -> [String: String] {
        var result: [String: String] = [:]
        var index = head.startIndex
        while index < head.endIndex {
            while index < head.endIndex, head[index].isWhitespace { index = head.index(after: index) }
            guard let equals = head[index...].firstIndex(of: "=") else { break }
            let key = head[index..<equals].trimmingCharacters(in: .whitespaces)
            let quoteStart = head.index(after: equals)
            guard quoteStart < head.endIndex, head[quoteStart] == "\"" else { break }
            let valueStart = head.index(after: quoteStart)
            guard let quoteEnd = head[valueStart...].firstIndex(of: "\"") else { break }
            if !key.isEmpty { result[key] = unescape(String(head[valueStart..<quoteEnd])) }
            index = head.index(after: quoteEnd)
        }
        return result
    }

    private static func unescape(_ value: String) -> String {
        value
            .replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&amp;", with: "&")
    }
}

extension String {
    fileprivate var nonEmpty: String? { isEmpty ? nil : self }
}
