import LatchMobileKit
import SwiftUI

/// One session's conversation: what the agent is doing, and a way to reply.
struct ChatView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var appModel
    @State private var model: ChatModel?
    @State private var draft = ""
    @FocusState private var composerFocused: Bool

    var body: some View {
        Group {
            if let model {
                content(model)
            } else if appModel.surface.chat {
                ProgressView()
            } else {
                MessageView(
                    icon: "text.bubble",
                    title: "Chat unavailable",
                    detail: """
                    This gateway does not report an events endpoint, so there is no \
                    transcript to show. Update the `latch` CLI on your computer.
                    """
                )
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .task {
            guard model == nil, appModel.surface.chat, let gateway = appModel.gateway else {
                return
            }
            let created = ChatModel(
                session: session,
                gateway: gateway,
                surface: appModel.surface
            )
            model = created
            await created.start()
        }
        .onDisappear { model?.stop() }
    }

    @ViewBuilder
    private func content(_ model: ChatModel) -> some View {
        VStack(spacing: 0) {
            TranscriptList(model: model)

            if let request = model.transcript.pendingRequest, model.canResolve {
                RequestPrompt(request: request) { choice in
                    Task { await model.resolve(requestId: request.requestId, choice: choice) }
                }
            }

            Composer(
                draft: $draft,
                focused: $composerFocused,
                enabled: model.canCompose && !model.isSending,
                reason: model.composerReason
            ) {
                let text = draft
                draft = ""
                Task { await model.send(text) }
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            StatusStrip(model: model)
        }
    }
}

/// The scrolling transcript, pinned to the newest row.
private struct TranscriptList: View {
    let model: ChatModel

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(model.transcript.entries) { entry in
                        TranscriptRow(entry: entry)
                            .id(entry.id)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .onChange(of: model.transcript.entries.last?.id) { _, id in
                guard let id else { return }
                withAnimation(.easeOut(duration: 0.2)) {
                    proxy.scrollTo(id, anchor: .bottom)
                }
            }
        }
    }
}

private struct TranscriptRow: View {
    let entry: TranscriptEntry

    var body: some View {
        switch entry.kind {
        case .user(let text):
            Bubble(text: text, alignment: .trailing)
        case .assistant(let text):
            Bubble(text: text, alignment: .leading)
        case .tool(let name, let detail, let finished):
            ToolRow(name: name, detail: detail, finished: finished)
        case .status(let status):
            NoteRow(icon: "circle.dotted", text: status.replacingOccurrences(of: "_", with: " "))
        case .request(let request):
            RequestRow(request: request)
        case .unrecognized(let type):
            // Additive event types are contract-legal. Showing the type is
            // more honest than pretending the agent did nothing.
            NoteRow(icon: "questionmark.circle", text: "unrecognized event: \(type)")
        }
    }
}

private struct Bubble: View {
    let text: String
    let alignment: HorizontalAlignment

    var body: some View {
        HStack {
            if alignment == .trailing { Spacer(minLength: 40) }
            Text(text)
                .textSelection(.enabled)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(
                    alignment == .trailing
                        ? AnyShapeStyle(Color.accentColor.opacity(0.16))
                        : AnyShapeStyle(.quaternary),
                    in: RoundedRectangle(cornerRadius: 16, style: .continuous)
                )
            if alignment == .leading { Spacer(minLength: 40) }
        }
    }
}

private struct ToolRow: View {
    let name: String
    let detail: String
    let finished: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: finished ? "checkmark.circle" : "circle.dashed")
                .foregroundStyle(finished ? .secondary : .tertiary)
                .font(.caption)
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(.caption.weight(.medium))
                if !detail.isEmpty {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, 2)
    }
}

private struct NoteRow: View {
    let icon: String
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
            Text(text)
            Spacer(minLength: 0)
        }
        .font(.caption2)
        .foregroundStyle(.tertiary)
    }
}

private struct RequestRow: View {
    let request: TranscriptEntry.Request

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(
                request.kind == .permission ? "Permission requested" : "Question",
                systemImage: request.kind == .permission ? "lock.shield" : "questionmark.bubble"
            )
            .font(.caption.weight(.medium))
            Text(request.prompt).font(.callout)
            if let answered = request.answered {
                Text("Answered: \(answered)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.yellow.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

/// Buttons for the request the agent is currently blocked on.
private struct RequestPrompt: View {
    let request: TranscriptEntry.Request
    let choose: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(request.prompt)
                .font(.footnote)
                .lineLimit(3)
            // A harness that supplied no closed set still needs an answer, so
            // fall back to the two the CLI itself offers.
            let choices = request.choices.isEmpty ? ["yes", "no"] : request.choices
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(choices, id: \.self) { choice in
                        Button(choice) { choose(choice) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                    }
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial)
    }
}

private struct Composer: View {
    @Binding var draft: String
    @FocusState.Binding var focused: Bool
    let enabled: Bool
    let reason: String?
    let submit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let reason, !enabled {
                Text(reason)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 4)
            }
            HStack(spacing: 8) {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.plain)
                    .focused($focused)
                    .disabled(!enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(.quaternary, in: Capsule())

                Button(action: submit) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(!enabled || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

/// A thin strip showing why the transcript is not live, when it is not.
private struct StatusStrip: View {
    let model: ChatModel

    var body: some View {
        Group {
            if let ended = model.endedReason {
                strip(text: ended, icon: "xmark.circle")
            } else if let error = model.error {
                strip(text: error, icon: "exclamationmark.triangle")
            } else if model.streamState != .open {
                strip(text: label(model.streamState), icon: "antenna.radiowaves.left.and.right")
            }
        }
    }

    private func strip(text: String, icon: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
            Text(text).lineLimit(2)
            Spacer(minLength: 0)
        }
        .font(.caption2)
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial)
    }

    private func label(_ state: EventStreamState) -> String {
        switch state {
        case .connecting: return "Connecting…"
        case .reconnecting: return "Reconnecting…"
        case .closed: return "Disconnected"
        case .open: return ""
        }
    }
}
