import LatchMobileKit
import SwiftUI
import UIKit

/// The floating chrome the chat and terminal screens share in place of a
/// navigation bar: a circular back button at the leading edge, a compact
/// title and status chip in the centre, and an optional trailing control.
///
/// It sits in a top safe-area inset, so the screen's content scrolls beneath
/// it rather than starting below a bar. Tapping the chip opens the session's
/// details. Every control is a `FloatingControl`: glass on iOS 26, material
/// before it.
struct SessionChromeBar<Trailing: View>: View {
    let title: String
    /// Only what the host reported. Nil draws the title alone.
    let status: String?
    var style: SessionChromeStyle = .standard
    var statusIdentifier = "session.chrome.status"
    let showDetails: () -> Void
    @ViewBuilder var trailing: Trailing

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        HStack(spacing: 10) {
            Button {
                dismiss()
            } label: {
                FloatingControl(systemImage: "chevron.backward")
                    .foregroundStyle(.primary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Back")
            .accessibilityIdentifier("session.chrome.back")

            Spacer(minLength: 0)

            Button(action: showDetails) {
                FloatingControl(shape: .capsule) {
                    VStack(spacing: 0) {
                        Text(title)
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                        if let status {
                            Text(status)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .contentTransition(.opacity)
                        }
                    }
                    .padding(.vertical, 4)
                }
                // The status line changes as the agent works; it settles
                // without motion when Reduce Motion is on.
                .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: status)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityHint("Shows the session's details")
            .accessibilityIdentifier(statusIdentifier)
            .layoutPriority(1)

            Spacer(minLength: 0)

            // The chip stays centred whether or not a trailing control exists.
            // A hidden circle holds the slot at the same scaled size.
            ZStack {
                FloatingControl(systemImage: "ellipsis").hidden()
                trailing
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .padding(.bottom, 10)
        .environment(\.colorScheme, style == .terminal ? .dark : colorScheme)
        // A soft fade rather than a bar, the mirror of the composer's.
        .background {
            LinearGradient(
                stops: [
                    .init(color: style.fade.opacity(0.92), location: 0.55),
                    .init(color: style.fade.opacity(0), location: 1),
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .ignoresSafeArea(edges: .top)
            .allowsHitTesting(false)
        }
    }

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
}

extension SessionChromeBar where Trailing == EmptyView {
    init(
        title: String,
        status: String?,
        style: SessionChromeStyle = .standard,
        statusIdentifier: String = "session.chrome.status",
        showDetails: @escaping () -> Void
    ) {
        self.init(title: title, status: status, style: style, statusIdentifier: statusIdentifier, showDetails: showDetails) {
            EmptyView()
        }
    }
}

enum SessionChromeStyle {
    case standard
    /// Over the terminal's black surface, whatever the system appearance.
    case terminal

    var fade: Color {
        switch self {
        case .standard: Color(.systemBackground)
        case .terminal: .black
        }
    }
}

// MARK: - Details

/// What the details sheet can do beyond describing the session.
struct SessionDetailsActions {
    /// The sheet's leading action, such as Reattach on a detached terminal.
    var primaryTitle: String?
    var primary: (() -> Void)?
    /// Asks to stop; the screen confirms before anything is sent.
    var stop: (() -> Void)?
    var isStopping = false
}

/// Folder, command and connector, plus whatever the screen can do from here.
/// Actions run after the sheet has gone, so a confirmation they raise is not
/// presented over a sheet that is dismissing.
struct SessionDetailsSheet: View {
    let session: SessionSummary
    let status: String?
    let actions: SessionDetailsActions
    /// Receives the chosen action; the presenter runs it on dismissal.
    let choose: (@escaping () -> Void) -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                // First, so it is above the fold at the medium detent.
                if let title = actions.primaryTitle, let primary = actions.primary {
                    Section {
                        Button(title) { pick(primary) }
                            .fontWeight(.semibold)
                            .accessibilityIdentifier("session.details.primary")
                    }
                }
                Section {
                    // Paths and commands run long; they sit under their label
                    // rather than wrapping against the trailing edge.
                    stacked("Folder", session.cwd)
                    stacked("Command", session.commandLabel, monospaced: true)
                    LabeledContent("Connector", value: session.connector.detailLabel)
                    if let status {
                        LabeledContent("Status", value: status)
                    }
                }
                if let stop = actions.stop {
                    Section {
                        Button(role: .destructive) {
                            pick(stop)
                        } label: {
                            if actions.isStopping {
                                Label("Stopping…", systemImage: "hourglass")
                            } else {
                                Label("Stop session", systemImage: "stop.circle")
                            }
                        }
                        .disabled(actions.isStopping)
                        .accessibilityIdentifier("session.details.stop")
                    }
                }
            }
            .navigationTitle(session.displayName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func stacked(_ label: String, _ value: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
            Text(value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }

    private func pick(_ action: @escaping () -> Void) {
        choose(action)
        dismiss()
    }
}

extension View {
    /// Replaces the navigation bar with `bar` floating over the content, and
    /// presents the details sheet when `showingDetails` is set.
    func sessionChrome<Bar: View>(
        session: SessionSummary,
        status: String?,
        showingDetails: Binding<Bool>,
        details: SessionDetailsActions,
        @ViewBuilder bar: () -> Bar
    ) -> some View {
        modifier(SessionChromeModifier(
            session: session,
            status: status,
            showingDetails: showingDetails,
            details: details,
            bar: bar()
        ))
    }
}

private struct SessionChromeModifier<Bar: View>: ViewModifier {
    let session: SessionSummary
    let status: String?
    @Binding var showingDetails: Bool
    let details: SessionDetailsActions
    let bar: Bar

    @State private var chosen: (() -> Void)?

    func body(content: Content) -> some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) { bar }
            .toolbar(.hidden, for: .navigationBar)
            .background(InteractivePopRestorer())
            .sheet(isPresented: $showingDetails, onDismiss: {
                let action = chosen
                chosen = nil
                action?()
            }) {
                SessionDetailsSheet(session: session, status: status, actions: details) { chosen = $0 }
            }
    }
}

// MARK: - Stop

extension View {
    /// The Stop confirmation and the missing-grant explanation, shared by
    /// every screen that offers Stop. Ending the agent's work is never one tap.
    func sessionStopPrompts(
        session: SessionSummary,
        confirming: Binding<Bool>,
        explainingGrant: Binding<Bool>
    ) -> some View {
        modifier(SessionStopPrompts(session: session, confirming: confirming, explainingGrant: explainingGrant))
    }
}

private struct SessionStopPrompts: ViewModifier {
    let session: SessionSummary
    @Binding var confirming: Bool
    @Binding var explainingGrant: Bool

    @Environment(AppModel.self) private var appModel

    func body(content: Content) -> some View {
        content
            .alert("This phone can't stop a session", isPresented: $explainingGrant) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(appModel.sessionStopUnavailableExplanation ?? "")
            }
            .confirmationDialog(
                "Stop \(session.displayName)?",
                isPresented: $confirming,
                titleVisibility: .visible
            ) {
                Button("Stop session", role: .destructive) {
                    Task { await appModel.stopSession(session) }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(
                    """
                    Whatever is running in \(session.directoryName) on your Mac ends. The session \
                    itself stays in your list so you can still read what it left behind.
                    """
                )
            }
    }
}

extension AppModel {
    /// Stop as a screen offers it: nil when the Mac has no stop route or the
    /// session is not running, and still offered without the grant so the
    /// alert can say what to change on the Mac, the same bargain the session
    /// list makes.
    func stopRequest(
        for session: SessionSummary,
        confirm: @escaping () -> Void,
        explainGrant: @escaping () -> Void
    ) -> (() -> Void)? {
        guard advertisesSessionStop, session.isRunning else { return nil }
        let canStop = canStopSessions
        return { canStop ? confirm() : explainGrant() }
    }
}

// MARK: - Swipe back

/// Hiding the navigation bar also stops UIKit's edge swipe from going back,
/// because the pop gesture's own delegate refuses while the bar is hidden.
/// This hands the gesture a delegate that allows it whenever there is a
/// screen to go back to.
private struct InteractivePopRestorer: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> Controller { Controller() }
    func updateUIViewController(_ controller: Controller, context: Context) {}

    final class Controller: UIViewController {
        override func viewDidAppear(_ animated: Bool) {
            super.viewDidAppear(animated)
            guard let navigation = navigationController else { return }
            PopGestureDelegate.shared.navigation = navigation
            navigation.interactivePopGestureRecognizer?.delegate = PopGestureDelegate.shared
        }
    }
}

private final class PopGestureDelegate: NSObject, UIGestureRecognizerDelegate {
    @MainActor static let shared = PopGestureDelegate()
    weak var navigation: UINavigationController?

    func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        (navigation?.viewControllers.count ?? 0) > 1
    }
}
