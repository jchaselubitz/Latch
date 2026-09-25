import Foundation

/// What the terminal screen's status chip says, reduced from the connection's
/// state to the few facts a person acts on: whether this phone holds the
/// terminal, whether it is on its way back, and whether something else took it.
public enum TerminalChromeStatus: Equatable, Sendable {
    case attaching
    case attached
    /// This phone does not hold the terminal; the Mac or nothing does.
    case detached
    /// The link dropped under a held surface and a resume is in flight.
    case reconnecting
    /// Another surface attached and this phone lost the terminal to it.
    case capturedElsewhere
    /// The program exited; there is nothing left to attach to.
    case exited

    public init(state: TerminalSessionState?) {
        switch state {
        case .attached:
            self = .attached
        case .connecting:
            self = .attaching
        case .interrupted(let resumable):
            self = resumable ? .reconnecting : .detached
        case .closed(.stolen):
            self = .capturedElsewhere
        case .closed(.sessionExited):
            self = .exited
        case .closed, .failed, .idle, nil:
            self = .detached
        }
    }

    public var label: String {
        switch self {
        case .attaching: "Attaching…"
        case .attached: "Attached"
        case .detached: "Detached"
        case .reconnecting: "Reconnecting"
        case .capturedElsewhere: "Captured elsewhere"
        case .exited: "Exited"
        }
    }

    /// Whether the session-details sheet leads with Reattach. Only when this
    /// phone does not hold the terminal, is not already getting it back, and
    /// there is still a program to attach to.
    public var offersReattach: Bool {
        switch self {
        case .detached, .capturedElsewhere: true
        case .attaching, .attached, .reconnecting, .exited: false
        }
    }
}

public extension SessionConnector {
    /// The connector as the session-details sheet names it. Kept here, with
    /// the composer placeholder, so the chat screen never spells a provider.
    var detailLabel: String {
        switch self {
        case .none:
            return "None (shell)"
        case .unknown:
            return "Not reported"
        case .named(let name):
            if let agent = SessionAgent(rawValue: name) { return agent.displayName }
            return name.isEmpty ? "Not reported" : name
        }
    }
}
