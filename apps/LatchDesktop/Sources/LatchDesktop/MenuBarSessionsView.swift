import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct MenuBarSessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    let openMainWindow: () -> Void
    let openSettings: () -> Void
    let checkForUpdates: () -> Void

    var body: some View {
        ForEach(store.sessions.prefix(12)) { session in
            Menu {
                Text(MenuTextWrapper.wrap(session.displaySubtitle))
                Divider()
                Button("Open") {
                    Task { await store.open(session.id) }
                }
                .disabled(!session.state.isAttachable || !store.canAttachSessions)
                Button("Stop", role: .destructive) {
                    Task { await store.stop(session.id, force: false) }
                }
                .disabled(!session.state.isLive)
            } label: {
                Label(session.name, systemImage: session.state == .running ? "circle.fill" : "circle")
            }
        }
        if store.sessions.isEmpty { Text("No sessions") }
        Button("Stop All Sessions", role: .destructive) {
            Task { await store.stopAll() }
        }
        .disabled(!store.sessions.contains(where: { $0.state.isLive }))
        if let update = updates.pendingUpdate {
            Button("Update to Latch \(update.version.description)…") { checkForUpdates() }
        }
        Divider()
        Button {
            store.shouldPresentNewSession = true
            openMainWindow()
        } label: {
            Label("New Session…", systemImage: "plus")
        }
        .disabled(!store.canCreateSessions)
        Button {
            openSettings()
        } label: {
            Label("Settings…", systemImage: "gear")
        }
        Button("Quit Latch") { NSApp.terminate(nil) }
        Divider()
        Button("Open Latch") { openMainWindow() }
    }
}
