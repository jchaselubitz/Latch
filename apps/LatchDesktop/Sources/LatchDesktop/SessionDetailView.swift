import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionDetailView: View {
    @ObservedObject var store: SessionStore
    let session: InspectReport
    @State private var renameValue = ""
    @State private var showingRename = false
    @State private var showingResize = false
    @State private var destructiveAction: DestructiveAction?

    enum DestructiveAction: String, Identifiable {
        case stop, remove, forceRemove
        var id: String { rawValue }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header

                OpenSessionButton(
                    store: store,
                    sessionID: session.id,
                    isEnabled: session.state.isAttachable && store.canAttachSessions
                )

                HStack(alignment: .top, spacing: 4) {
                    CaptionedIconButton(title: "Rename", systemImage: "pencil") {
                        renameValue = session.name
                        showingRename = true
                    }
                    CaptionedIconButton(title: "Resize", systemImage: "arrow.up.left.and.arrow.down.right") {
                        showingResize = true
                    }
                    .disabled(session.state != .running)
                    CaptionedIconButton(title: "Copy", systemImage: "doc.on.doc") {
                        Task { await store.copyAttachCommand(session.id) }
                    }
                    .help("Copy Attach Command")
                    CaptionedIconButton(title: "Stop", systemImage: "stop.circle", role: .destructive) {
                        destructiveAction = .stop
                    }
                    .disabled(!session.state.isLive)
                }

                VStack(spacing: 0) {
                    DetailRow(label: "Directory", value: (session.cwd as NSString).abbreviatingWithTildeInPath, monospaced: true)
                    DetailRow(label: "Command", value: session.commandLabel, monospaced: true)
                    if let kernel = session.kernel {
                        DetailRow(label: "Kernel", value: kernel)
                    }
                    DetailRow(label: "Size", value: sizeDescription)
                    DetailRow(label: "Created", value: Self.displayTime(session.createdAt))
                    if let lastActivity = summary?.lastActivityAt {
                        DetailRow(label: "Last Activity", value: Self.displayTime(lastActivity))
                    }
                    if let exit = session.exit {
                        DetailRow(label: "Exit", value: exitDescription(exit))
                    }
                    if let attached = session.attached {
                        DetailRow(label: "Attached", value: attached == 1 ? "1 client" : "\(attached) clients")
                    }
                    DetailRow(label: "Session ID", value: session.id, monospaced: true, isLast: true)
                }
                .latchCard()
                .textSelection(.enabled)

                Button(role: .destructive) {
                    destructiveAction = session.state.isLive ? .forceRemove : .remove
                } label: {
                    Label("Remove Session…", systemImage: "trash")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.red)
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(16)
        }
        .alert("Rename Session", isPresented: $showingRename) {
            TextField("Name", text: $renameValue)
            Button("Cancel", role: .cancel) {}
            Button("Rename") { Task { await store.rename(session.id, to: renameValue) } }
        }
        .sheet(isPresented: $showingResize) {
            ResizeSessionView(
                store: store,
                session: session,
                isPresented: $showingResize
            )
        }
        .confirmationDialog(
            destructiveTitle,
            isPresented: Binding(
                get: { destructiveAction != nil },
                set: { if !$0 { destructiveAction = nil } }
            ),
            titleVisibility: .visible
        ) {
            destructiveButtons
            Button("Cancel", role: .cancel) { destructiveAction = nil }
        } message: {
            Text(destructiveMessage)
        }
    }

    @ViewBuilder private var destructiveButtons: some View {
        switch destructiveAction {
        case .stop:
            Button("Stop", role: .destructive) { Task { await store.stop(session.id, force: false) } }
            Button("Force Stop", role: .destructive) { Task { await store.stop(session.id, force: true) } }
        case .remove:
            Button("Remove", role: .destructive) { Task { await store.remove(session.id, force: false) } }
        case .forceRemove:
            Button("Stop and Remove", role: .destructive) { Task { await store.remove(session.id, force: true) } }
        case nil:
            EmptyView()
        }
    }

    private var destructiveTitle: String {
        switch destructiveAction {
        case .stop: return "Stop \(session.name)?"
        case .remove, .forceRemove: return "Remove \(session.name)?"
        case nil: return "Manage Session"
        }
    }

    private var destructiveMessage: String {
        switch destructiveAction {
        case .stop:
            return "Stopping ends the child process but retains its final screen for later inspection."
        case .remove:
            return "This permanently deletes the retained screen and session metadata."
        case .forceRemove:
            return "This stops the live child process, then permanently deletes its retained screen and metadata."
        case nil: return ""
        }
    }

    private var summary: SessionSummary? {
        store.sessions.first { $0.id == session.id }
    }

    private var header: some View {
        HStack(spacing: 12) {
            AgentTile(agent: SessionAgent(commandLabel: session.commandLabel), size: 44)
            VStack(alignment: .leading, spacing: 4) {
                Text(session.name)
                    .font(.title3.weight(.bold))
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 6) {
                    StatePill(state: session.state)
                    Text(headerCaption)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                if let title = session.title {
                    Text(SessionSummary.displaySubtitle(fromTitle: title))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var headerCaption: String {
        let agent = SessionAgent(commandLabel: session.commandLabel).title
        guard let idle = summary?.displayIdleLabel else { return agent }
        return "\(agent) · \(idle)"
    }

    /// CLI timestamps are RFC 3339; anything unparseable is shown as sent.
    static func displayTime(_ timestamp: String) -> String {
        let parser = ISO8601DateFormatter()
        parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = parser.date(from: timestamp)
            ?? ISO8601DateFormatter().date(from: timestamp) else { return timestamp }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        formatter.doesRelativeDateFormatting = true
        return formatter.string(from: date)
    }

    private var sizeDescription: String {
        let size = session.size ?? session.initialSize
        return "\(size.cols) × \(size.rows)"
    }

    private func exitDescription(_ exit: ExitRecord) -> String {
        if let code = exit.code { return "Code \(code), \(Self.displayTime(exit.exitedAt))" }
        return "\(exit.signal ?? "signal"), \(Self.displayTime(exit.exitedAt))"
    }
}

struct DetailRow: View {
    let label: String
    let value: String
    var monospaced = false
    var isLast = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(label)
                    .foregroundStyle(.secondary)
                    .frame(width: 84, alignment: .leading)
                Text(value)
                    .font(monospaced ? .caption.monospaced() : .callout)
                    .multilineTextAlignment(.trailing)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .font(.callout)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            if !isLast {
                Divider().padding(.leading, 12)
            }
        }
        .accessibilityElement(children: .combine)
    }
}
