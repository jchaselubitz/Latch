import LatchMobileKit
import LatchTransportNative
import Network
import SwiftUI
import UserNotifications

@main
struct LatchMobileApp: App {
    @UIApplicationDelegateAdaptor(PushDelegate.self) private var pushDelegate

    // The reporter is built first because both halves need the same one: the
    // transport writes the selected path into it, and the model reads it for
    // the Settings indicator.
    @State private var model: AppModel = {
        let pathReporter = RemotePathReporter()
        return AppModel(
            linkConnector: NativeRemoteLinkConnector(pathReporter: pathReporter),
            pathReporter: pathReporter
        )
    }()
    @State private var pairing = PairingModel(
        enrollmentProvider: NativeRemoteEnrollmentProvider()
    )
    /// One runner for the whole app so a launch argument from the USB
    /// harness can start it before Settings is ever shown.
    @State private var diagnostics = DiagnosticsRunner()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .environment(pairing)
                .environment(diagnostics)
                .task {
                    pushDelegate.pairing = pairing
                    await pairing.restore()
                    // The Mac may have changed this grant while the phone was
                    // closed. Read it before the paired route snapshots the
                    // record, so the first session tap is never based on the
                    // permission saved at pairing time.
                    await pairing.refreshPermission()
                    let launch = DiagnosticsLaunchOptions.current
                    if let skip = launch.skipLAN { await model.setDiagnosticsSkipLAN(skip) }
                    await model.connectPairedDevice(pairing.record)
                    await PushDelegate.requestRegistration()
                    if launch.autoRuns, pairing.record != nil {
                        diagnostics.settings = launch.applied(to: diagnostics.settings)
                        diagnostics.run(subject: model)
                    }
                }
                .onChange(of: diagnostics.isRunning) { _, running in
                    // A measured run needs the screen on; the phone is on USB
                    // for these runs, so the idle timer is the only thing that
                    // would end it early.
                    UIApplication.shared.isIdleTimerDisabled = running
                }
        }
    }
}

/// Receives the APNs device token and hands it to the pairing model, which
/// registers it with the paired device credential. Notifications carry no
/// content; opening one simply brings the app to the foreground, where the
/// ordinary resume path refreshes real state over the authenticated link.
final class PushDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    @MainActor var pairing: PairingModel?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    /// Asks for alert permission, then for a token. Both are best effort: a
    /// refusal leaves the app exactly as functional, only quieter.
    @MainActor
    static func requestRegistration() async {
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined:
            guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else { return }
        case .denied:
            return
        default:
            break
        }
        UIApplication.shared.registerForRemoteNotifications()
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in await pairing?.pushTokenReceived(deviceToken) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // Best effort by design. Nothing about the link depends on push.
    }

    /// A generic alert while the app is in front is noise: the screens are
    /// already live. Suppress the banner there; the notification still lands
    /// in the list when the app is backgrounded.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        []
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
    @State private var pathMonitor = NetworkPathObserver()

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
        .task {
            // A real path change is one immediate retry through the same
            // owner: a backoff is cut short, a live link is probed. There is
            // no second reconnect loop here.
            for await _ in pathMonitor.changes() {
                guard !suspended else { continue }
                await model.networkPathChanged()
            }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                // Background means nothing is held: the loopback adapter and
                // its capability, the native link, conversation sockets, and
                // any terminal surface all go. iOS suspends a quiet process;
                // push is not a way to keep any of them alive.
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
                    // A revoke or a permission change happens on the Mac while
                    // the phone is away, so returning to the foreground re-reads
                    // it before rebuilding the paired route. Otherwise that
                    // route snapshots the stale, pre-suspension permission.
                    await pairing.refreshPermission()
                    await pairing.registerPushIfPossible()
                    await model.resumeAfterSuspension()
                }
            @unknown default:
                break
            }
        }
        .onChange(of: pairing.record) { _, record in
            // A permission-only update needs no transport teardown. Requests
            // are authorized again by the Mac; this only updates what the UI
            // may offer immediately.
            if !model.applyPairedDeviceRecord(record) {
                Task {
                    await model.connectPairedDevice(record)
                    await pairing.registerPushIfPossible()
                }
            }
        }
    }
}

/// Network path changes as an async sequence, coalesced. Interface changes
/// (Wi-Fi to cellular, a VPN coming up) are hints for the one link owner,
/// never proof that the gateway is reachable.
@MainActor
final class NetworkPathObserver {
    private let monitor = NWPathMonitor()
    private var lastStatus: NWPath.Status?
    private var lastInterfaces: Set<NWInterface.InterfaceType> = []

    func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            monitor.pathUpdateHandler = { [weak self] path in
                Task { @MainActor in
                    guard let self else { return }
                    let interfaces = Set(path.availableInterfaces.map(\.type))
                    let changed = self.lastStatus != nil
                        && (self.lastStatus != path.status || self.lastInterfaces != interfaces)
                    self.lastStatus = path.status
                    self.lastInterfaces = interfaces
                    if changed, path.status == .satisfied { continuation.yield(()) }
                }
            }
            monitor.start(queue: DispatchQueue(label: "dev.cooperativ.latch.network-path"))
            continuation.onTermination = { [monitor] _ in monitor.cancel() }
        }
    }
}
