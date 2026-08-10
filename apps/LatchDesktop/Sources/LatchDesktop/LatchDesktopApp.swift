import SwiftUI
import AppKit

@main
struct LatchDesktopApp: App {
    @StateObject private var store = SessionStore()
    @Environment(\.openWindow) private var openWindow

    var body: some Scene {
        WindowGroup("Latch", id: "sessions") {
            SessionsView(store: store)
                .frame(minWidth: 760, minHeight: 480)
                .task { store.start() }
        }
        .defaultSize(width: 940, height: 620)
        .commands {
            // Replacing the default New Window item gives Latch an explicit File
            // menu with the action users actually need to start their work.
            CommandGroup(replacing: .newItem) {
                Button("New Session…") {
                    store.shouldPresentNewSession = true
                    openSessionsWindow()
                }
                .keyboardShortcut("n", modifiers: .command)
            }

            CommandMenu("Session") {
                Button("Refresh") {
                    Task { await store.refresh() }
                }
                .keyboardShortcut("r", modifiers: .command)

                Button("Prune…") {
                    store.shouldPresentPrune = true
                    openSessionsWindow()
                }
            }

            // A Settings scene normally supplies this automatically, but making
            // the command explicit guarantees a standard App-menu entry point.
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") {
                    openSettingsWindow()
                }
                .keyboardShortcut(",", modifiers: .command)
            }
        }

        MenuBarExtra {
            MenuBarSessionsView(store: store) {
                openWindow(id: "sessions")
                NSApp.activate(ignoringOtherApps: true)
            } openSettings: {
                openSettingsWindow()
            }
        } label: {
            Label("Latch \(store.runningCount)", systemImage: "rectangle.stack")
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView(store: store)
        }
    }

    private func openSessionsWindow() {
        openWindow(id: "sessions")
        NSApp.activate(ignoringOtherApps: true)
    }

    private func openSettingsWindow() {
        NSApp.activate(ignoringOtherApps: true)
        NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
    }
}
