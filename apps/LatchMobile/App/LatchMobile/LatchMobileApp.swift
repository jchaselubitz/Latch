import LatchMobileKit
import LatchTransportNative
import SwiftUI

@main
struct LatchMobileApp: App {
    // The reporter is built first because both halves need the same one: the
    // transport writes the selected path into it, and the model reads it for
    // the Settings indicator.
    @State private var model: AppModel = {
        let pathReporter = RemotePathReporter()
        return AppModel(
            pairedGatewayFactory: NativePairedGatewayFactory.make(pathReporter: pathReporter),
            pathReporter: pathReporter
        )
    }()
    @State private var pairing = PairingModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .environment(pairing)
                .task {
                    await pairing.restore()
                    await model.restore()
                    await model.connectPairedDevice(pairing.record)
                }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing
    @Environment(\.scenePhase) private var scenePhase
    @State private var selection = Tab.sessions
    /// Whether the last phase change actually suspended the app, so returning
    /// to the front only reconnects when there is something to reconnect.
    @State private var suspended = false

    enum Tab {
        case sessions
        case settings
    }

    var body: some View {
        TabView(selection: $selection) {
            SessionsView()
                .tabItem { Label("Sessions", systemImage: "bubble.left.and.bubble.right") }
                .tag(Tab.sessions)

            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(Tab.settings)
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                // A paired route owns a loopback listener and live sockets.
                // Drop them before suspension rather than advertising the
                // prior path as live when iOS has already reclaimed it.
                suspended = true
                model.suspendPairedTransport()
                model.suspendConversations()
                // While attached, the phone holds the session's only surface.
                // A phone suspended with the socket open holds it hostage from
                // a locked pocket; the gateway's 4408 slow-client eviction
                // bounds the damage, but relying on being evicted is not a
                // design.
                model.suspendTerminals()
            case .inactive:
                // Not the same thing as backgrounded. A pulled-down
                // notification centre, an incoming call banner, the app
                // switcher, and the Face ID prompt in front of the terminal
                // itself all land here, and tearing the route down for each of
                // them would make the phone reconnect constantly. The surface
                // is held, but on a clock: no input for a couple of minutes and
                // it goes back to the Mac.
                model.beginTerminalIdleCountdown()
            case .active:
                model.cancelTerminalIdleCountdown()
                // Terminals are deliberately absent from what comes back.
                // `resumeAfterSuspension` resumes conversations because that is
                // free; reattaching is another steal, so it returns to
                // `.closed(.detached)` with a Reattach button the user presses.
                //
                // Only after a real suspension: repeating discovery every time
                // the app merely regains focus would rebuild the whole route
                // behind every Face ID prompt.
                guard suspended else { return }
                suspended = false
                Task {
                    await model.resumeAfterSuspension()
                    // A revoke or a permission change happens on the Mac while
                    // the phone is away, so returning to the foreground re-reads
                    // it for the same reason discovery is repeated: state
                    // decided elsewhere is not assumed to have held.
                    await pairing.refreshPermission()
                }
            @unknown default:
                break
            }
        }
        .onChange(of: pairing.record) { _, record in
            Task { await model.connectPairedDevice(record) }
        }
    }
}
