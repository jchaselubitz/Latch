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
            VStack(alignment: .leading, spacing: 22) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(session.name).font(.largeTitle).fontWeight(.semibold)
                        if let title = session.title { Text(title).foregroundStyle(.secondary) }
                    }
                    Spacer()
                    Text(session.state.rawValue.capitalized)
                        .padding(.horizontal, 9).padding(.vertical, 4)
                        .background(.quaternary, in: Capsule())
                }

                OpenSessionButton(
                    store: store,
                    sessionID: session.id,
                    isEnabled: session.state.isAttachable && store.canAttachSessions
                )

                Grid(alignment: .leading, horizontalSpacing: 22, verticalSpacing: 10) {
                    DetailRow(label: "Command", value: session.commandLabel)
                    if let kernel = session.kernel {
                        DetailRow(label: "Kernel", value: kernel)
                    }
                    DetailRow(label: "Directory", value: session.cwd)
                    DetailRow(label: "Created", value: session.createdAt)
                    if let summary = store.sessions.first(where: { $0.id == session.id }),
                       let lastActivity = summary.lastActivityAt {
                        DetailRow(label: "Last Activity", value: lastActivity)
                    }
                    DetailRow(label: "Size", value: sizeDescription)
                    DetailRow(label: "Session ID", value: session.id)
                    if let exit = session.exit {
                        DetailRow(label: "Exit", value: exitDescription(exit))
                    }
                    if let attached = session.attached {
                        DetailRow(label: "Attached", value: "\(attached)")
                    }
                }
                .textSelection(.enabled)

                HStack {
                    Button("Rename…") {
                        renameValue = session.name
                        showingRename = true
                    }
                    if session.state == .running {
                        Button("Resize…") { showingResize = true }
                    }
                    if session.state.isLive {
                        Button("Stop…") { destructiveAction = .stop }
                    }
                    Spacer()
                    Button("Remove…", role: .destructive) {
                        destructiveAction = session.state.isLive ? .forceRemove : .remove
                    }
                }
            }
            .padding(28)
        }
        .navigationTitle(session.name)
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

    private var sizeDescription: String {
        let size = session.size ?? session.initialSize
        return "\(size.cols) × \(size.rows)"
    }

    private func exitDescription(_ exit: ExitRecord) -> String {
        if let code = exit.code { return "Code \(code) at \(exit.exitedAt)" }
        return "\(exit.signal ?? "signal") at \(exit.exitedAt)"
    }
}

struct DetailRow: View {
    let label: String
    let value: String

    var body: some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
