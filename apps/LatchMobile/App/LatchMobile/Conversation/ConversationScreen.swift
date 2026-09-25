import LatchMobileKit
import SwiftUI

/// Everything the chat screen draws, read from the store once per update.
///
/// Components below take these values and plain closures rather than the
/// store, so each one can be previewed in any state without a connection.
struct ConversationScreenContent {
    var viewState: ConversationViewState
    var transcript: ConversationTranscriptPresentation
    /// Hub phase plus the newest live tool, when the phase is working.
    var statusLine: String?
    /// The newest rendered item, whose change means something was appended.
    var tailItemID: String?
    var prependAnchor: String?
    var paging: ConversationPaging
    /// Sends whose outcome needs the person; still-sending ones are omitted.
    var operations: [ConversationOperationPresentation]
    var composer: ConversationComposerPresentation
    var canResolve: Bool
    var resolveReason: String?
    var connectionError: String?
    /// Updates that could not be placed at all; placeholders cover the rest.
    var skippedUpdates: Int
}

struct ConversationPaging: Equatable {
    var hasEarlierRendered = false
    var hasMoreBefore = false
    var isHistoryLimitReached = false
    var hasNewerRendered = false
}

struct ConversationScreenActions {
    var loadOlder: () -> Void = {}
    var showNewer: () -> Void = {}
    var send: (String) -> Void = { _ in }
    /// Sends an operation's text again as a new operation.
    var retry: (String) -> Void = { _ in }
    /// Returns an operation's text to the draft.
    var edit: (String) -> Void = { _ in }
    var dismiss: (String) -> Void = { _ in }
    var resolve: (_ requestID: String, _ choice: String) -> Void = { _, _ in }
}

extension ConversationScreenContent {
    /// Reading the store here is what subscribes the screen to its changes.
    @MainActor
    init(store: ConversationStore) {
        let diagnostics = store.decodeDiagnostics
        let viewState = ConversationViewState(store: store)
        self.init(
            viewState: viewState,
            transcript: ConversationProjection.project(
                items: store.items,
                state: store.state,
                operations: store.operations,
                resolveAttempts: store.resolveAttempts
            ),
            statusLine: nil,
            tailItemID: store.items.last?.id,
            prependAnchor: store.prependAnchor,
            paging: ConversationPaging(
                hasEarlierRendered: store.hasEarlierRendered,
                hasMoreBefore: store.hasMoreBefore,
                isHistoryLimitReached: store.isHistoryLimitReached,
                hasNewerRendered: store.hasNewerRendered
            ),
            operations: ConversationOperationPresentation.rows(for: store.operations),
            composer: .derive(
                viewState: viewState,
                state: store.state,
                canSend: store.canSend,
                sendReason: store.sendReason,
                connectionError: store.connectionError
            ),
            canResolve: store.canResolve,
            resolveReason: store.resolveReason,
            connectionError: store.connectionError,
            skippedUpdates: diagnostics.droppedItems + diagnostics.undecodableFrames
        )
        statusLine = viewState.statusLine(in: transcript)
    }
}

extension ConversationViewState {
    @MainActor
    init(store: ConversationStore) {
        self = .derive(
            socketState: store.socketState,
            connectionError: store.connectionError,
            state: store.state,
            hasItems: !store.items.isEmpty
        )
    }
}

extension ConversationScreenActions {
    @MainActor
    init(store: ConversationStore) {
        self.init(
            loadOlder: { store.loadOlder() },
            showNewer: { store.showNewer() },
            send: { store.send(text: $0) },
            retry: { store.retry($0) },
            edit: { store.editOperation($0) },
            dismiss: { store.dismissOperation($0) },
            resolve: { store.resolve(requestID: $0, choice: $1) }
        )
    }
}

/// The conversation itself: transcript, delivery outcomes, and one input
/// surface, with connection status pinned above.
///
/// The input surface floats over the bottom of the transcript as a safe-area
/// inset rather than taking a slot beneath it, so the transcript scrolls
/// under it and its newest row always settles just above the field.
///
/// While the host waits on a request, its controls replace the composer: the
/// request is the one thing to answer, and it appears in full only there. The
/// draft is kept underneath and comes back with the composer.
struct ConversationScreen: View {
    let content: ConversationScreenContent
    let actions: ConversationScreenActions
    @Binding var draft: String
    @FocusState.Binding var composerFocused: Bool
    var placeholder = "Message"
    var sessionActions = ConversationSessionActions()
    var attachments = ConversationAttachmentControls()

    /// Autofocus is decided once, the first time the screen knows enough.
    @State private var consideredFocus = false

    var body: some View {
        ConversationTranscript(
            transcript: content.transcript,
            viewState: content.viewState,
            tailItemID: content.tailItemID,
            prependAnchor: content.prependAnchor,
            paging: content.paging,
            loadOlder: actions.loadOlder,
            showNewer: actions.showNewer
        )
        .safeAreaInset(edge: .top, spacing: 0) {
            ConversationConnectionBanner(error: content.connectionError, skippedUpdates: content.skippedUpdates)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            inputSurface
        }
        .onAppear(perform: considerFocus)
        .onChange(of: content.viewState) { _, _ in considerFocus() }
    }

    private var inputSurface: some View {
        VStack(spacing: 0) {
            if !content.operations.isEmpty {
                ConversationOperationRows(operations: content.operations, actions: actions)
            }
            if let request = content.transcript.pendingRequest {
                ConversationRequestControls(
                    request: request,
                    canResolve: content.canResolve,
                    reason: content.resolveReason,
                    resolve: actions.resolve
                )
            } else {
                ConversationComposer(
                    draft: $draft,
                    focused: $composerFocused,
                    presentation: content.composer,
                    placeholder: placeholder,
                    sessionActions: sessionActions,
                    attachments: attachments,
                    send: actions.send
                )
                // A soft fade rather than a bar: the field floats, and the
                // transcript passing beneath it stays out of the way.
                .background {
                    LinearGradient(
                        stops: [
                            .init(color: Color(.systemBackground).opacity(0), location: 0),
                            .init(color: Color(.systemBackground).opacity(0.92), location: 0.45),
                        ],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .ignoresSafeArea(edges: .bottom)
                }
            }
        }
    }

    /// Raises the keyboard on arrival only when the next move is the
    /// person's; a conversation still loading waits until it has loaded.
    private func considerFocus() {
        guard !consideredFocus, content.viewState != .loading else { return }
        consideredFocus = true
        guard content.transcript.pendingRequest == nil,
              ConversationComposerChrome.shouldFocusOnOpen(
                  canSend: content.composer.canSend,
                  transcript: content.transcript
              ) else { return }
        composerFocused = true
    }
}
