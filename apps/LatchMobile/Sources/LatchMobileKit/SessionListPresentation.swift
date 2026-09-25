import Foundation

/// The two groups the session list reads as. Latch has no notion of a recent
/// session, only whether its program is still there, so the grouping says
/// exactly that and nothing more.
public enum SessionListSection: String, CaseIterable, Sendable {
    case running
    case stopped

    public var title: String {
        switch self {
        case .running: "Running"
        case .stopped: "Stopped"
        }
    }

    /// Where a session belongs. A session on its way up or down is still
    /// running: its program exists, and Stop may still be offered or pending.
    public init(state: String) {
        switch state {
        case "running", "creating", "stopping": self = .running
        default: self = .stopped
        }
    }
}

/// One non-empty group of the session list, in the order the Mac listed it.
public struct SessionListGroup: Equatable, Identifiable, Sendable {
    public let section: SessionListSection
    public let sessions: [SessionSummary]

    public var id: SessionListSection { section }

    /// Groups `sessions` into Running then Stopped, keeping the Mac's order
    /// inside each group, dropping sessions `query` does not match, and
    /// leaving out a group that ends up empty.
    public static func grouped(_ sessions: [SessionSummary], matching query: String = "") -> [SessionListGroup] {
        let matching = sessions.filter { $0.matches(searchQuery: query) }
        return SessionListSection.allCases.compactMap { section in
            let members = matching.filter { SessionListSection(state: $0.state) == section }
            return members.isEmpty ? nil : SessionListGroup(section: section, sessions: members)
        }
    }
}

public extension SessionSummary {
    /// True while the Mac is starting or stopping the session, the one state
    /// a row still marks with a dot now that the section says the rest.
    var isTransitioning: Bool {
        state == "creating" || state == "stopping"
    }

    /// Whether a search for `query` finds this session: by title, folder
    /// name, or agent. Case and diacritics are ignored; whitespace alone
    /// matches everything, so an empty search field hides nothing.
    func matches(searchQuery query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return true }
        return searchableText.contains { $0.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
    }

    private var searchableText: [String] {
        var fields = [displayName, name, directoryName]
        switch connector {
        case .none:
            fields.append("Shell")
        case .unknown:
            break
        case .named(let raw):
            fields.append(raw)
            if let agent = SessionAgent(rawValue: raw) { fields.append(agent.displayName) }
        }
        return fields
    }

    /// Idle time at the precision a person reads at a glance.
    static func idleLabel(milliseconds: Int) -> String {
        let seconds = max(0, milliseconds) / 1000
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m" }
        if seconds < 86_400 { return "\(seconds / 3600)h" }
        return "\(seconds / 86_400)d"
    }

    /// The trailing idle label, for a running row only. A stopped session's
    /// idle time only grows, so it says nothing worth the space.
    var idleLabel: String? {
        guard state == "running", let idleMs else { return nil }
        return Self.idleLabel(milliseconds: idleMs)
    }
}

/// A session's title split the way the list reads it: what the session is
/// about first, and the handle it was launched under beneath.
///
/// Sessions started by an orchestrator arrive titled `coo:1056.wpbv — Align
/// UI colors`, a ticket handle and a description joined by a dash. On a phone
/// the handle is the least useful part of that line and it came first, so the
/// description was what got truncated. The split puts the description on top
/// and demotes the handle to a secondary line; a title with no dash stays a
/// single line.
public struct SessionHeadline: Equatable, Sendable {
    /// The line that identifies the session: the description when the title
    /// has one, otherwise the whole title.
    public let primary: String
    /// The handle the title led with, when it had one.
    public let secondary: String?

    /// The dashes a title may be joined by, tried in order; the em dash is
    /// what Overlord writes and the others cover a title typed by hand.
    static let separators = [" — ", " – ", " - "]

    public init(title: String) {
        for separator in Self.separators {
            guard let range = title.range(of: separator) else { continue }
            let handle = title[..<range.lowerBound].trimmingCharacters(in: .whitespaces)
            let description = title[range.upperBound...].trimmingCharacters(in: .whitespaces)
            guard !handle.isEmpty, !description.isEmpty else { break }
            primary = description
            secondary = handle
            return
        }
        primary = title
        secondary = nil
    }
}

public extension SessionSummary {
    /// The row's two lines, cut from `displayName`.
    var headline: SessionHeadline {
        SessionHeadline(title: displayName)
    }
}

/// One piece of a row's secondary line. The row joins the pieces with a dot,
/// so a session that has no handle, or no agent, simply has a shorter line.
public enum SessionSubtitleField: Equatable, Sendable {
    /// The ticket handle the title led with.
    case handle(String)
    /// The folder a shell was started in; a shell has no agent to name.
    case shellFolder(String)
    /// The agent the session runs, as a person knows it.
    case agent(String)
    /// How long the session has been idle, `2m` at a glance.
    case idle(String)

    public var text: String {
        switch self {
        case .handle(let text), .shellFolder(let text), .agent(let text), .idle(let text): text
        }
    }
}

public extension SessionSummary {
    /// The agent's name for a row: the product name when this build knows the
    /// connector, the raw name when it does not, and nothing for a shell or a
    /// gateway that did not say.
    var agentLabel: String? {
        guard case .named(let raw) = connector, !raw.isEmpty else { return nil }
        return SessionAgent(rawValue: raw)?.displayName ?? raw
    }

    /// The row's secondary line in reading order: handle, then agent, then
    /// how long it has been idle. `showingIdle` is false while a stop is
    /// pending, when the row says so instead and idle time means nothing.
    func subtitleFields(showingIdle: Bool = true) -> [SessionSubtitleField] {
        var fields: [SessionSubtitleField] = []
        if let handle = headline.secondary { fields.append(.handle(handle)) }
        if connector == .none { fields.append(.shellFolder(directoryName)) }
        if let agent = agentLabel { fields.append(.agent(agent)) }
        if showingIdle, let idle = idleLabel { fields.append(.idle(idle)) }
        return fields
    }
}

/// The pill beside the session list's title. It covers the link states in
/// which the list itself stays on screen; every other state takes the whole
/// screen with its own explanation.
public enum SessionListLinkStatus: Equatable, Sendable {
    case connected
    case reconnecting
    case macUnavailable

    public init?(_ state: AppModel.LinkState) {
        switch state {
        case .linked: self = .connected
        case .interrupted: self = .reconnecting
        case .macOffline: self = .macUnavailable
        case .unlinked, .connecting, .revoked, .pairingRequired, .incompatible, .failed: return nil
        }
    }

    public var label: String {
        switch self {
        case .connected: "Connected"
        case .reconnecting: "Reconnecting…"
        case .macUnavailable: "Mac unavailable"
        }
    }

    /// What the pill says. A live link is named by the Mac it reaches, since
    /// the dot already says "connected"; the other states say what is wrong,
    /// because the Mac's name would only be reassuring.
    public func pillText(deviceName: String?) -> String {
        switch self {
        case .connected:
            if let deviceName, !deviceName.isEmpty { return deviceName }
            return label
        case .reconnecting, .macUnavailable:
            return label
        }
    }

    /// What VoiceOver adds after the label, so the stale rows below it are
    /// not read as current.
    public var accessibilityDetail: String? {
        switch self {
        case .connected: nil
        case .reconnecting: "Showing the last known sessions."
        case .macUnavailable: "Your Mac is not reachable through the relay. Showing the last known sessions."
        }
    }
}
