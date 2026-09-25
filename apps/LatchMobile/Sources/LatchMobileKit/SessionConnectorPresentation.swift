import Foundation

public extension SessionConnector {
    /// What the empty chat composer says. It names who the message goes to;
    /// a shell has no one to message, so it asks for a command instead, and
    /// a connector this build does not know is addressed by its own name
    /// rather than guessed at.
    var composerPlaceholder: String {
        switch self {
        case .none:
            return "Type a command"
        case .unknown:
            return "Message"
        case .named(let name):
            if let agent = SessionAgent(rawValue: name) { return "Message \(agent.displayName)" }
            return name.isEmpty ? "Message" : "Message \(name)"
        }
    }
}
