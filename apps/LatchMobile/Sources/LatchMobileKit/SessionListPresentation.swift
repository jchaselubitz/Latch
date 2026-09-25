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

/// The one quiet line under the session list's title. It covers the link
/// states in which the list itself stays on screen; every other state takes
/// the whole screen with its own explanation.
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
