import Foundation

/// How the composer behaves on arrival, apart from whether sending is
/// allowed. What the empty field says depends on the session's connector and
/// so lives with the session (`SessionConnector.composerPlaceholder`); the
/// presentation layer names no provider.
public enum ConversationComposerChrome {
    /// Slash commands use the terminal path even when ordinary messages can
    /// be queued through the bridge. Only a confirmed idle host can take one.
    public static func canUseSlashCommands(
        viewState: ConversationViewState, state: ConversationState?, canSend: Bool
    ) -> Bool {
        canSend && state?.phase == "idle" && state?.pendingRequest == nil
            && (viewState == .ready || viewState == .empty)
    }

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

/// Suggestions come only from the live catalog. Once the person starts
/// command arguments, leave the draft alone and stop offering completions.
public struct ConversationCommandPickerPresentation: Equatable, Sendable {
    public let commands: [AdvertisedCommand]
    public let canSelect: Bool

    public init(draft: String, catalog: [AdvertisedCommand], canSelect: Bool) {
        self.canSelect = canSelect
        guard draft.hasPrefix("/"), !draft.contains(where: { $0.isWhitespace }) else {
            commands = []
            return
        }
        let prefix = draft.dropFirst()
        commands = catalog.filter { $0.name.lowercased().hasPrefix(prefix.lowercased()) }
    }

    /// Re-check the current suggestions and availability on tap so a catalog
    /// disappearing or a turn starting cannot apply an outdated selection.
    public func inserting(_ command: AdvertisedCommand) -> String? {
        guard canSelect, commands.contains(command) else { return nil }
        return "/" + command.name + " "
    }
}
