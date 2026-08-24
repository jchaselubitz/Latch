import LatchMobileKit
import SwiftUI

/// Hub-owned conversation rendering. The view does not fold transcript records
/// or decide whether an interaction is allowed; those answers arrive in the
/// store's pushed state over the one v2 socket.
struct ChatView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var appModel
    @State private var store: ConversationStore?
    @State private var draft = ""
    @FocusState private var composerFocused: Bool

    var body: some View {
        Group {
            if let store {
                conversation(store)
            } else if appModel.surface.chat {
                ProgressView("Opening conversation…")
            } else {
                terminalFallback(
                    title: "Conversation unavailable",
                    detail: "This Mac does not offer the v2 Conversation Hub."
                )
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .task {
            guard store == nil else { return }
            store = appModel.conversationStore(for: session)
            store?.start()
        }
    }

    @ViewBuilder
    private func conversation(_ store: ConversationStore) -> some View {
        if store.state?.connector == nil, store.socketState == .open {
            terminalFallback(
                title: "Conversation unsupported",
                detail: "This session's connector cannot provide a conversation."
            )
        } else {
            VStack(spacing: 0) {
                ConversationList(store: store)

                if let request = store.pendingRequest {
                    RequestControls(request: request, store: store)
                }

                if !store.operations.isEmpty {
                    OperationNotices(store: store)
                }

                Composer(store: store, draft: $draft, focused: $composerFocused)
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                ConnectionStatus(store: store)
            }
        }
    }

    /// Both dead ends keep their explanation and gain a way out.
    ///
    /// Telling someone to walk to their Mac, on a session that is live,
    /// reachable and already authenticated, is what this feature exists to
    /// remove — so the button appears whenever this device's grant and the
    /// Mac's routes actually allow a terminal, and the old sentence about
    /// attaching on the Mac stays only for the case where they do not.
    @ViewBuilder
    private func terminalFallback(title: String, detail: String) -> some View {
        if appModel.surface.terminal {
            ContentUnavailableView {
                Label(title, systemImage: "terminal")
            } description: {
                Text(detail + " The session's terminal can be opened here instead.")
            } actions: {
                NavigationLink("Open terminal") {
                    // Never auto-attach from here. The user arrived asking for
                    // chat; the steal is not implied by a tap that asked for
                    // something else.
                    TerminalView(session: session, autoAttach: false)
                }
                .buttonStyle(.borderedProminent)
            }
        } else {
            ContentUnavailableView(
                title,
                systemImage: "terminal",
                description: Text(detail + " Use `latch attach` on the Mac for this session.")
            )
        }
    }
}

private struct ConversationList: View {
    let store: ConversationStore

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    if store.hasMoreBefore {
                        Button("Load earlier messages") { store.loadOlder() }
                            .buttonStyle(.bordered)
                            .frame(maxWidth: .infinity)
                    }
                    ForEach(store.items) { item in
                        ConversationRow(item: item)
                            .id(item.id)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .onChange(of: store.prependAnchor) { _, anchor in
                // Restoring the old first row after a history prepend keeps the
                // reader's viewport stable instead of jumping toward the past.
                guard let anchor else { return }
                proxy.scrollTo(anchor, anchor: .top)
            }
            .onChange(of: store.items.last?.id) { _, id in
                guard let id else { return }
                withAnimation(.easeOut(duration: 0.18)) {
                    proxy.scrollTo(id, anchor: .bottom)
                }
            }
        }
    }
}

private struct ConversationRow: View {
    let item: ConversationItem

    var body: some View {
        switch item.kind {
        case .message(let role, let text, _):
            HStack {
                if role == "user" { Spacer(minLength: 38) }
                Text(text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(
                        role == "user" ? AnyShapeStyle(Color.accentColor.opacity(0.16)) : AnyShapeStyle(.quaternary),
                        in: RoundedRectangle(cornerRadius: 16, style: .continuous)
                    )
                if role != "user" { Spacer(minLength: 38) }
            }
        case .tool(let name, let summary, let status, _):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: status == "complete" ? "checkmark.circle" : "circle.dashed")
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.caption.weight(.medium))
                    if !summary.isEmpty { Text(summary).font(.caption2).foregroundStyle(.secondary).lineLimit(2) }
                }
                Spacer(minLength: 0)
            }
            .foregroundStyle(.secondary)
        case .request(_, let type, let prompt, _, let status):
            VStack(alignment: .leading, spacing: 4) {
                Label(type == "permission" ? "Permission requested" : "Question", systemImage: type == "permission" ? "lock.shield" : "questionmark.bubble")
                    .font(.caption.weight(.medium))
                Text(prompt).font(.callout)
                if status != "pending" {
                    Text(status.replacingOccurrences(of: "_", with: " "))
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.yellow.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }
}

private struct RequestControls: View {
    let request: ConversationItem
    let store: ConversationStore

    var body: some View {
        Group {
            if case .request(let requestID, _, let prompt, let choices, _) = request.kind {
                VStack(alignment: .leading, spacing: 8) {
                    Text(prompt).font(.footnote).lineLimit(3)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(choices.isEmpty ? ["yes", "no"] : choices, id: \.self) { choice in
                                Button(choice) { store.resolve(requestID: requestID, choice: choice) }
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .disabled(!store.canResolve)
                            }
                        }
                    }
                    if let reason = store.resolveReason, !store.canResolve {
                        Text(reason).font(.caption2).foregroundStyle(.secondary)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.thinMaterial)
            }
        }
    }
}

private struct OperationNotices: View {
    let store: ConversationStore

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(store.operations) { operation in
                if operation.status != .sending {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: operation.status == .ambiguous ? "questionmark.diamond" : "exclamationmark.triangle")
                        Text(operation.reason ?? "Message needs review.")
                        Spacer(minLength: 0)
                        Button("Retry") { store.retry(operation.id) }
                            .buttonStyle(.bordered)
                    }
                    .font(.caption)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.thinMaterial)
    }
}

private struct Composer: View {
    let store: ConversationStore
    @Binding var draft: String
    @FocusState.Binding var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let reason = store.sendReason, !store.canSend {
                Text(reason).font(.caption2).foregroundStyle(.secondary).padding(.horizontal, 4)
            }
            HStack(spacing: 8) {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.plain)
                    .focused($focused)
                    .disabled(!store.canSend)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(.quaternary, in: Capsule())
                Button {
                    let text = draft
                    draft = ""
                    store.send(text: text)
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title2)
                }
                .disabled(!store.canSend || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

private struct ConnectionStatus: View {
    let store: ConversationStore

    var body: some View {
        if let error = store.connectionError {
            HStack(spacing: 6) {
                Image(systemName: "antenna.radiowaves.left.and.right")
                Text(error).lineLimit(2)
                Spacer(minLength: 0)
            }
            .font(.caption2)
            .padding(.horizontal, 14)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.thinMaterial)
        }
    }
}
