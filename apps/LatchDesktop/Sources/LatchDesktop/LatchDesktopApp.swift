import SwiftUI
import AppKit

@main
struct LatchDesktopApp: App {
    /// Menu bar label + `.commands` observe only this small object so session
    /// list churn cannot reach `-[NSApplication setMainMenu:]`.
    @StateObject private var chrome = AppChromeState()
    /// Controllers live here without being `@StateObject`s on `App`, so their
    /// `@Published` traffic does not invalidate the scene graph at the root.
    @State private var runtime = DesktopRuntime()
    @Environment(\.openWindow) private var openWindow

    var body: some Scene {
        WindowGroup("Latch", id: "sessions") {
            SessionsView(store: runtime.store, updates: runtime.updates)
                .frame(minWidth: 760, minHeight: 480)
                .task {
                    let store = runtime.store
                    let remoteAccess = runtime.remoteAccess
                    store.attachChrome(chrome)
                    store.addCompanionRefresh { await remoteAccess.refresh() }
                    store.start()
                    runtime.updates.startAutomaticChecks()
                    // Remote access never turns itself on: this only resumes
                    // supervision when the user already left it enabled.
                    // Polling is owned by SessionStore's companion refresh.
                    await remoteAccess.restoreIfEnabled()
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
                    Task { await runtime.updates.check(userInitiated: true) }
                }
            }

            // Replacing the default New Window item gives Latch an explicit File
            // menu with the action users actually need to start their work.
            CommandGroup(replacing: .newItem) {
                Button("New Session…") {
                    runtime.store.shouldPresentNewSession = true
                    openSessionsWindow()
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(!chrome.canCreateSessions)
            }

            CommandMenu("Session") {
                Button("Refresh") {
                    Task { await runtime.store.refresh(showsProgress: true) }
                }
                .keyboardShortcut("r", modifiers: .command)

                Button("Prune…") {
                    runtime.store.shouldPresentPrune = true
                    openSessionsWindow()
                }
            }
        }

        MenuBarExtra {
            MenuBarSessionsView(store: runtime.store, updates: runtime.updates) {
                openWindow(id: "sessions")
                NSApp.activate(ignoringOtherApps: true)
            } openSettings: {
                openSettingsWindow()
            } checkForUpdates: {
                openWindow(id: "sessions")
                NSApp.activate(ignoringOtherApps: true)
                Task { await runtime.updates.check(userInitiated: true) }
            }
        } label: {
            Label {
                Text("Latch \(chrome.runningCount)")
            } icon: {
                Image(nsImage: Self.menuBarImage)
                    .renderingMode(.template)
            }
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView(
                store: runtime.store,
                updates: runtime.updates,
                remoteAccess: runtime.remoteAccess
            )
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
