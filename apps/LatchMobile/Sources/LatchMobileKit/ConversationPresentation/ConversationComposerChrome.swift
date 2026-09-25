import Foundation

/// How the composer behaves on arrival, apart from whether sending is
/// allowed. What the empty field says depends on the session's connector and
/// so lives with the session (`SessionConnector.composerPlaceholder`); the
/// presentation layer names no provider.
public enum ConversationComposerChrome {
    /// Whether opening the conversation should raise the keyboard.
    ///
    /// Only when a send could go through, and only when the next move is the
    /// person's: a fresh conversation, or one whose newest turn is their own
    /// words still waiting on nothing. Arriving at an agent's reply, the
    /// person is there to read, and a keyboard would cover what they came for.
    public static func shouldFocusOnOpen(canSend: Bool, transcript: ConversationTranscriptPresentation) -> Bool {
        guard canSend else { return false }
        guard let last = transcript.turns.last else { return true }
        if let entry = last.entries.last {
            if case .message(let message) = entry { return message.role == .user }
            return false
        }
        return last.prompt?.role == .user
    }
}
