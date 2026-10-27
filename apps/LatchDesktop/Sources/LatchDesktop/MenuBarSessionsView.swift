import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// The menu-bar extra's window: sessions at a glance, with the actions people
/// reach for without opening the main window.
struct MenuBarSessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @ObservedObject var remoteAccess: RemoteAccessController
    let openMainWindow: () -> Void
    let openSettings: () -> Void
    let checkForUpdates: () -> Void

    @State private var hoveredID: String?

    private var listedSessions: [SessionSummary] {
        let live = store.sessions.filter(\.state.isLive)
        let stopped = store.sessions.filter { !$0.state.isLive }
        return Array((live + stopped).prefix(12))
    }

    private var hiddenCount: Int { store.sessions.count - listedSessions.count }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header

            if store.sessions.isEmpty {
                Text("No sessions")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 18)
            } else {
                VStack(spacing: 2) {
                    ForEach(listedSessions) { session in
                        sessionRow(session)
                    }
                }
                if hiddenCount > 0 {
                    menuRow {
                        dismissThen(openMainWindow)
                    } label: {
                        Text("\(hiddenCount) more in Latch…")
                            .foregroundStyle(.secondary)
                    }
                }
            }

            if remoteAccess.phase != .off, !remoteAccess.activeDevices.isEmpty {
                Divider().padding(.horizontal, 8)
                ForEach(remoteAccess.activeDevices) { device in
                    deviceRow(device)
                }
            }

            Divider().padding(.horizontal, 8)

            menuRow {
                dismissThen(checkForUpdates)
            } label: {
                Label(
                    updates.pendingUpdate == nil ? "Check for Updates…" : "Update Latch",
                    systemImage: updates.pendingUpdate == nil ? "arrow.clockwise.circle" : "arrow.down.circle.fill"
                )
                .foregroundStyle(updates.pendingUpdate == nil ? Color.primary : Color.accentColor)
            }

            HStack(spacing: 8) {
                Button {
                    store.shouldPresentNewSession = true
                    dismissThen(openMainWindow)
                } label: {
                    Label("New Session", systemImage: "plus")
                        .frame(maxWidth: .infinity)
                }
                .latchProminentButtonStyle()
                .disabled(!store.canCreateSessions)
                Button {
                    dismissThen(openMainWindow)
                } label: {
                    Text("Open Latch")
                        .frame(maxWidth: .infinity)
                }
                .latchGlassButtonStyle()
            }
            .controlSize(.large)
            .padding(.top, 4)

            HStack {
                Button("Stop All Sessions", role: .destructive) {
                    Task { await store.stopAll() }
                }
                .foregroundStyle(.red)
                .disabled(!store.sessions.contains(where: { $0.state.isLive }))
                Spacer()
                Button("Quit Latch") { NSApp.terminate(nil) }
                    .keyboardShortcut("q")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .font(.callout)
            .padding(.horizontal, 6)
            .padding(.top, 2)
        }
        .padding(10)
        .frame(width: 330)
    }

    private var header: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Latch").font(.headline)
                Text(runningSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                dismissThen(openSettings)
            } label: {
                Label("Settings…", systemImage: "gearshape")
                    .labelStyle(.iconOnly)
                    .font(.system(size: 14))
            }
            .buttonStyle(.borderless)
            .help("Settings")
        }
        .padding(.horizontal, 8)
        .padding(.top, 4)
        .padding(.bottom, 4)
    }

    private var runningSummary: String {
        let running = store.runningCount
        return running == 0 ? "No running sessions" : "\(running) running"
    }

    private func sessionRow(_ session: SessionSummary) -> some View {
        let canOpen = session.state.isAttachable && store.canAttachSessions
        let hovered = hoveredID == session.id
        return HStack(spacing: 9) {
            Button {
                dismissThen { Task { await store.open(session.id) } }
            } label: {
                HStack(spacing: 9) {
                    AgentLogo(agent: session.agent, size: 16)
                        .opacity(session.state.isLive ? 1 : 0.6)
                    Text(session.name)
                        .fontWeight(.medium)
                        .foregroundStyle(session.state.isLive ? Color.primary : Color.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 6)
                    if hovered && canOpen {
                        Image(systemName: "arrow.up.forward.app")
                            .foregroundStyle(Color.accentColor)
                    } else if session.state.isLive {
                        Text(session.displayIdleLabel?.replacingOccurrences(of: " idle", with: "") ?? "")
                            .font(.caption)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                    } else {
                        Text(session.state.label)
                            .font(.caption)
                            .foregroundStyle(session.state.color)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!canOpen)
            .help(canOpen ? "Open in \(store.preferredTerminal.rawValue)" : session.displaySubtitle)
            .accessibilityLabel("Open \(session.name)")

            if hovered && session.state.isLive {
                Button {
                    Task { await store.stop(session.id, force: false) }
                } label: {
                    Label("Stop \(session.name)", systemImage: "stop.circle")
                        .labelStyle(.iconOnly)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.red)
                .help("Stop")
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 30)
        .background(
            hovered ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .onHover { inside in
            if inside {
                hoveredID = session.id
            } else if hoveredID == session.id {
                hoveredID = nil
            }
        }
    }

    private func deviceRow(_ device: RemoteDevice) -> some View {
        let link = device.controlPlaneDeviceID.flatMap { remoteAccess.linkStatuses[$0] }
        let connected = link == .ready
        return HStack(spacing: 9) {
            Image(systemName: "iphone")
                .frame(width: 20)
            Text(device.name).lineLimit(1)
            Spacer()
            HStack(spacing: 5) {
                Circle().frame(width: 6, height: 6)
                Text(link.map(RemoteAccessSettingsView.label) ?? "Not connected")
            }
            .font(.caption)
            .foregroundStyle(connected ? Color.green : Color.secondary)
        }
        .padding(.horizontal, 8)
        .frame(height: 28)
    }

    private func menuRow<Label: View>(action: @escaping () -> Void, @ViewBuilder label: () -> Label) -> some View {
        Button(action: action) {
            label()
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 8)
                .frame(height: 28)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// The extra's window stays up until it loses focus, so it is closed
    /// first; otherwise it would sit over the window an action brings forward.
    private func dismissThen(_ action: @escaping () -> Void) {
        if let window = NSApp.keyWindow, window is NSPanel {
            window.close()
        }
        action()
    }
}
