import SwiftUI
import AppKit

@main
struct LatchDesktopApp: App {
    @StateObject private var store = SessionStore()
    @StateObject private var updates = UpdateController()
    @Environment(\.openWindow) private var openWindow

    var body: some Scene {
        WindowGroup("Latch", id: "sessions") {
            SessionsView(store: store, updates: updates)
                .frame(minWidth: 760, minHeight: 480)
                .task {
                    store.start()
                    updates.startAutomaticChecks()
                }
        }
        .defaultSize(width: 940, height: 620)
        .commands {
            // The App menu is where macOS users look for this, and it is the
            // one command that has to work when no session is selected and the
            // CLI is missing entirely.
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") {
                    openSessionsWindow()
                    Task { await updates.check(userInitiated: true) }
                }
            }

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
        }

        MenuBarExtra {
            MenuBarSessionsView(store: store, updates: updates) {
                openWindow(id: "sessions")
                NSApp.activate(ignoringOtherApps: true)
            } openSettings: {
                openSettingsWindow()
            } checkForUpdates: {
                openWindow(id: "sessions")
                NSApp.activate(ignoringOtherApps: true)
                Task { await updates.check(userInitiated: true) }
            }
        } label: {
            Label {
                Text("Latch \(store.runningCount)")
            } icon: {
                Image(nsImage: Self.menuBarImage)
                    .renderingMode(.template)
            }
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView(store: store, updates: updates)
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

    private static let menuBarImage: NSImage = {
        guard let url = Bundle.main.url(
            forResource: "latch-menubar-template@2x",
            withExtension: "png"
        ), let image = NSImage(contentsOf: url) else {
            return NSImage(systemSymbolName: "rectangle.stack", accessibilityDescription: "Latch")
                ?? NSImage()
        }

        image.size = NSSize(width: 18, height: 18)
        image.isTemplate = true
        return image
    }()
}
