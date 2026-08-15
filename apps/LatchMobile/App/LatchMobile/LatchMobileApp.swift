import LatchMobileKit
import SwiftUI

@main
struct LatchMobileApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .task { await model.restore() }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
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
            // Coming back from the background is a reconnect. The contract
            // requires repeating discovery before resuming application
            // traffic, because capabilities may have changed while away.
            guard phase == .active else { return }
            Task { await model.rediscover() }
        }
    }
}
