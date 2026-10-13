#if DEBUG
import LatchMobileKit
import SwiftUI

/// View-state previews for the chat screen: one per `ConversationViewState`.
///
/// Items are decoded from wire JSON, the same way the socket receives them,
/// so a preview cannot show a shape the contract does not allow. The prose is
/// abbreviated from the captured fixture corpus.
enum ConversationPreviewFixtures {
    static func content(_ viewState: ConversationViewState, envelopes: Bool = false) -> ConversationScreenContent {
        let items: [ConversationItem]
        var pending: String?
        var operations: [ConversationOperation] = []
        var attempts: [ConversationResolveAttempt] = []
        var connectionError: String?
        var canSend = true
        var sendReason: String?

        switch viewState {
        case .loading, .starting, .empty:
            items = []
            canSend = viewState != .loading
            sendReason = viewState == .loading ? "Waiting for conversation state" : nil
        case .ready:
            items = envelopes ? envelopeConversation : conversation
            operations = envelopes ? [] : [
                ConversationOperation(id: "op-1", text: "retry this", operationEpoch: "e", status: .refused, reason: "The agent is working; try again when it is idle."),
                ConversationOperation(id: "op-2", text: "and this", operationEpoch: "e", status: .ambiguous, reason: "The Mac did not confirm delivery."),
            ]
        case .working:
            items = conversation + [tool("t9", 20, name: "Bash", status: "running")]
            canSend = false
            sendReason = "agent is working"
        case .awaitingInput:
            // One earlier request settled after this phone's answer was
            // refused; the pending one shows only a marker in the transcript.
            items = conversation + [
                request("r0", 18, requestId: "req-0", status: "dismissed"),
                request("r1", 20, requestId: "req-1"),
            ]
            pending = "req-1"
            attempts = [ConversationResolveAttempt(
                id: "op-r0", requestId: "req-0", choice: "Yes", status: .refused,
                reason: "the requested prompt is no longer visible"
            )]
            canSend = false
            sendReason = "resolve the pending request first"
        case .interrupted:
            items = conversation
            canSend = false
            sendReason = "The agent has exited."
        case .disconnected:
            items = conversation
            connectionError = "The secure connection closed. Reconnecting…"
        case .failed:
            items = conversation
            canSend = false
            sendReason = "This session's conversation is unavailable."
        }

        let state = viewState == .loading ? nil : state(pendingRequest: pending)
        let transcript = ConversationProjection.project(
            items: items, state: state, operations: operations, resolveAttempts: attempts
        )
        return ConversationScreenContent(
            viewState: viewState,
            transcript: transcript,
            statusLine: viewState.statusLine(in: transcript),
            tailItemID: items.last?.id,
            prependAnchor: nil,
            paging: ConversationPaging(hasMoreBefore: !items.isEmpty),
            operations: ConversationOperationPresentation.rows(for: operations),
            composer: .derive(
                viewState: viewState, state: state, canSend: canSend,
                sendReason: sendReason, connectionError: connectionError
            ),
            canResolve: pending != nil,
            resolveReason: pending == nil ? "no pending request" : nil,
            connectionError: connectionError,
            skippedUpdates: 0
        )
    }

    static var conversation: [ConversationItem] {
        [
            message("u1", 1, role: "user", text: "the desktop app says it has paired, but the phone says the connection closed. Expected?"),
            message("a1", 2, role: "assistant", text: """
            Found it. The backend's startup log is unambiguous:

            ```
            [webapp] web server listening on :::8080
            ```

            - `:::8080` means it **is** bound dual-stack, so binding was never the problem.
            - The port it listens on is **8080**, not **4310**.

            ## The fix

            | Service | Variable | Value |
            | --- | --- | --- |
            | gateway | `BACKEND_URL` | `:8080` |

            ```bash
            node -e "fetch('http://backend.example.internal:8080/').then(r=>console.log('OK',r.status)).catch(e=>console.error('ERR',e.cause?.code||e))"
            ```
            """),
            tool("t1", 3, name: "Read"),
            tool("t2", 4, name: "Read"),
            tool("t3", 5, name: "Bash", status: "failed", summary: "Exit code 1"),
            message("a2", 6, role: "assistant", text: "Both files read. Want me to re-check the deploy logs once you've redeployed?"),
            message("u2", 7, role: "user", text: "yes please", status: "submitted"),
        ]
    }

    /// Harness markup the agent files as user turns, as it arrives: each is
    /// drawn as a card, never as raw tags in the person's bubble.
    static var envelopeConversation: [ConversationItem] {
        [
            message("e1", 1, role: "user", text: "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>"),
            message("e2", 2, role: "user", text: "<local-command-stdout>Set model to opus</local-command-stdout>"),
            message("e3", 3, role: "user", text: "split the refactor into three forks and run them in the background"),
            tool("t1", 4, name: "Agent", status: "running", summary: "P6b fork C: shifts/orders/ranges"),
            tool("t2", 5, name: "mcp__issue-tracker__create_comment", summary: "Posted progress"),
            message("e4", 6, role: "user", text: """
            <task-notification>
            <task-id>a753f1967a1f2b6d3</task-id>
            <tool-use-id>toolu_018ig2PzS7YeFtNz8K1zGKKA</tool-use-id>
            <output-file>/private/tmp/tasks/a753f1967a1f2b6d3.output</output-file>
            <status>failed</status>
            <summary>Agent "P6b fork C: shifts/orders/ranges" failed: Agent terminated early due to an API error: You've hit your session limit · resets 4pm (error type rate_limit, HTTP 429)</summary>
            <note>A task-notification fires each time this agent stops with no live background children of its own.</note>
            <result>Fork C: shifts/orders/stats range pickers, hydration, lead chip, routes (items 1-11).</result>
            </task-notification>
            """),
            message("a1", 7, role: "assistant", text: "Fork C hit the session limit. I'll restart it after the reset."),
            message("e5", 8, role: "user", text: """
            <task-notification>
            <task-id>br0hdlnq7</task-id>
            <status>completed</status>
            <summary>Background command "Run the test suite" completed (exit code 0)</summary>
            </task-notification>
            """),
            message("e6", 9, role: "user", text: "<teammate-message teammate_id=\"researcher\" summary=\"Root cause found\">The range picker reads the stale store; see `RangeStore.swift:42`.</teammate-message>"),
            message("e7", 10, role: "user", text: "<bash-input>git status --short</bash-input><bash-stdout> M Sources/RangeStore.swift</bash-stdout><bash-stderr></bash-stderr>"),
        ]
    }

    private static func message(_ id: String, _ ordinal: Int, role: String, text: String, status: String = "complete") -> ConversationItem {
        item(id, ordinal, ["type": "message", "role": role, "text": text, "status": status])
    }

    private static func tool(_ id: String, _ ordinal: Int, name: String, status: String = "succeeded", summary: String = "completed") -> ConversationItem {
        item(id, ordinal, ["type": "tool", "name": name, "summary": summary, "status": status])
    }

    private static func request(_ id: String, _ ordinal: Int, requestId: String, status: String = "pending") -> ConversationItem {
        item(id, ordinal, [
            "type": "request",
            "requestId": requestId,
            "requestType": "permission",
            "prompt": "Allow Bash to run `cargo test -p latch`?",
            "choices": ["Yes", "Yes, and don't ask again", "No"],
            "status": status,
        ])
    }

    private static func item(_ id: String, _ ordinal: Int, _ kind: [String: Any]) -> ConversationItem {
        decode(["id": id, "ordinal": ordinal, "createdAt": "2026-09-21T00:00:00Z", "kind": kind])
    }

    private static func state(pendingRequest: String?) -> ConversationState {
        decode([
            "phase": pendingRequest == nil ? "idle" : "awaiting_input",
            "sendMessage": ["enabled": pendingRequest == nil],
            "resolveRequest": ["enabled": pendingRequest != nil],
            "pendingRequest": pendingRequest.map { $0 as Any } ?? NSNull(),
            "connector": ["id": "preview", "version": "1"],
        ])
    }

    private static func decode<T: Decodable>(_ object: [String: Any]) -> T {
        let data = try! JSONSerialization.data(withJSONObject: object)
        return try! JSONDecoder().decode(T.self, from: data)
    }
}

/// A stand-alone screen for one state, with its own draft and focus.
private struct ConversationStatePreview: View {
    let viewState: ConversationViewState
    var attachmentPhase: ConversationAttachmentPhase?
    var showsEnvelopes = false
    @State private var draft: String
    @State private var attachments: [ConversationAttachment]
    @FocusState private var focused: Bool

    init(
        viewState: ConversationViewState,
        draft: String = "Half-written follow-up kept across states",
        attachmentPhase: ConversationAttachmentPhase? = nil,
        showsEnvelopes: Bool = false
    ) {
        self.viewState = viewState
        self.showsEnvelopes = showsEnvelopes
        self.attachmentPhase = attachmentPhase
        _draft = State(initialValue: draft)
        _attachments = State(initialValue: attachmentPhase == nil ? [] : [
            ConversationAttachment(name: "screenshot.png", data: Data(count: 4), kind: .image),
            ConversationAttachment(name: "crash-report.txt", data: Data(count: 4), kind: .file),
        ])
    }

    /// Attachment items appear in the "+" menu; the chips appear only in the
    /// states that pass a phase.
    private var attachmentControls: ConversationAttachmentControls {
        ConversationAttachmentControls(
            items: attachments,
            phase: attachmentPhase ?? .idle,
            add: { attachments.append($0) },
            remove: { id in attachments.removeAll { $0.id == id } }
        )
    }

    private var content: ConversationScreenContent {
        ConversationPreviewFixtures.content(viewState, envelopes: showsEnvelopes)
    }

    var body: some View {
        NavigationStack {
            ConversationScreen(
                content: content,
                actions: ConversationScreenActions(),
                draft: $draft,
                composerFocused: $focused,
                // Chat components name no provider, previews included; the
                // real placeholder comes from the session's connector.
                placeholder: "Message the agent",
                sessionActions: ConversationSessionActions(
                    takeTerminal: {},
                    sessionLink: SessionDeepLink.url(forSession: "ses_preview"),
                    stop: {}
                ),
                attachments: attachmentControls
            )
            .safeAreaInset(edge: .top, spacing: 0) {
                SessionChromeBar(
                    title: "latch-mobile",
                    status: content.statusLine,
                    statusIdentifier: "conversation.toolbar.status",
                    showDetails: {}
                )
            }
            .toolbar(.hidden, for: .navigationBar)
        }
    }
}

#Preview("Loading") { ConversationStatePreview(viewState: .loading) }
#Preview("Empty") { ConversationStatePreview(viewState: .empty) }
#Preview("Ready") { ConversationStatePreview(viewState: .ready) }
#Preview("Ready, no draft") { ConversationStatePreview(viewState: .ready, draft: "") }
#Preview("Attachments") { ConversationStatePreview(viewState: .ready, draft: "What does this crash mean?", attachmentPhase: .idle) }
#Preview("Attachments uploading") { ConversationStatePreview(viewState: .ready, draft: "", attachmentPhase: .uploading) }
#Preview("Attachment failed") {
    ConversationStatePreview(
        viewState: .ready,
        attachmentPhase: .failed("The attachment did not reach your Mac. The connection was lost.")
    )
}
#Preview("Agent envelopes") { ConversationStatePreview(viewState: .ready, draft: "", showsEnvelopes: true) }
#Preview("Working") { ConversationStatePreview(viewState: .working) }
#Preview("Awaiting input") { ConversationStatePreview(viewState: .awaitingInput) }
#Preview("Interrupted") { ConversationStatePreview(viewState: .interrupted) }
#Preview("Disconnected") { ConversationStatePreview(viewState: .disconnected) }
#Preview("Failed") { ConversationStatePreview(viewState: .failed) }
#endif
