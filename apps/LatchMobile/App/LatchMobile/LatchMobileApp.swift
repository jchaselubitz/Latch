import LatchMobileKit
import LatchTransportNative
import SwiftUI

@main
struct LatchMobileApp: App {
    @State private var model = AppModel(
        pairedGatewayFactory: NativePairedGatewayFactory.make()
    )
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
            if phase == .inactive || phase == .background {
                // A paired route owns a loopback listener and live sockets.
                // Drop them before suspension rather than advertising the
                // prior path as live when iOS has already reclaimed it.
                model.suspendPairedTransport()
            }
            // Coming back from the background is a reconnect. The contract
            // requires repeating discovery before resuming application
            // traffic, because capabilities may have changed while away.
            guard phase == .active else { return }
            Task { await model.reconnectPairedTransport() }
            Task { await model.rediscover() }
            // A revoke or a permission change happens on the Mac while the
            // phone is away, so returning to the foreground re-reads it for
            // the same reason discovery is repeated: state decided elsewhere
            // is not assumed to have held.
            Task { await pairing.refreshPermission() }
        }
        .onChange(of: pairing.record) { _, record in
            Task { await model.connectPairedDevice(record) }
        }
    }
}
