#if DEBUG
import LatchMobileKit
import SwiftUI

/// View-state previews for the chat screen: one per `ConversationViewState`.
///
/// Items are decoded from wire JSON, the same way the socket receives them,
/// so a preview cannot show a shape the contract does not allow. The prose is
/// abbreviated from the captured fixture corpus.
enum ConversationPreviewFixtures {
    static func content(_ viewState: ConversationViewState) -> ConversationScreenContent {
        let items: [ConversationItem]
        var pending: String?
        var operations: [ConversationOperation] = []
        var attempts: [ConversationResolveAttempt] = []
        var connectionError: String?
        var canSend = true
        var sendReason: String?

        switch viewState {
        case .loading, .empty:
            items = []
            canSend = viewState == .empty
            sendReason = viewState == .loading ? "Waiting for conversation state" : nil
        case .ready:
            items = conversation
            operations = [
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
    @State private var draft = "Half-written follow-up kept across states"
    @FocusState private var focused: Bool

    private var content: ConversationScreenContent {
        ConversationPreviewFixtures.content(viewState)
    }

    var body: some View {
        NavigationStack {
            ConversationScreen(
                content: content,
                actions: ConversationScreenActions(),
                draft: $draft,
                composerFocused: $focused
            )
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ConversationToolbar(title: "latch-mobile", statusLine: content.statusLine) }
        }
    }
}

#Preview("Loading") { ConversationStatePreview(viewState: .loading) }
#Preview("Empty") { ConversationStatePreview(viewState: .empty) }
#Preview("Ready") { ConversationStatePreview(viewState: .ready) }
#Preview("Working") { ConversationStatePreview(viewState: .working) }
#Preview("Awaiting input") { ConversationStatePreview(viewState: .awaitingInput) }
#Preview("Interrupted") { ConversationStatePreview(viewState: .interrupted) }
#Preview("Disconnected") { ConversationStatePreview(viewState: .disconnected) }
#Preview("Failed") { ConversationStatePreview(viewState: .failed) }
#endif
