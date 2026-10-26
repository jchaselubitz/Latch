import SwiftUI
import AppKit

/// The window's sidebar: the filters that narrow the session list, the paired
/// devices, and a status footer for the selected CLI.
struct SessionsSidebar: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var remoteAccess: RemoteAccessController
    let prune: () -> Void

    var body: some View {
        List(selection: filterSelection) {
            Section("Sessions") {
                row(.all, systemImage: "square.stack", tint: .accentColor)
                ForEach(SessionState.allCases, id: \.self) { state in
                    row(.state(state), systemImage: state.symbolName, tint: state.color)
                }
            }
            Section("Agents") {
                ForEach([SessionAgent.claude, .codex, .shell]) { agent in
                    row(.agent(agent), systemImage: agent.symbolName, tint: agent.tint)
                }
            }
            if remoteAccess.phase != .off, !remoteAccess.activeDevices.isEmpty {
                Section("Remote") {
                    ForEach(remoteAccess.activeDevices) { device in
                        deviceRow(device)
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            SidebarStatus(store: store, prune: prune)
                .padding(10)
        }
    }

    /// Clearing the selection (Command-click) falls back to every session
    /// rather than leaving the list unfiltered with nothing highlighted.
    private var filterSelection: Binding<SessionFilter?> {
        Binding(
            get: { store.filter },
            set: { store.filter = $0 ?? .all }
        )
    }

    private func row(_ filter: SessionFilter, systemImage: String, tint: Color) -> some View {
        Label {
            Text(filter.title)
        } icon: {
            Image(systemName: systemImage).foregroundStyle(tint)
        }
        .badge(store.count(for: filter))
        .tag(filter as SessionFilter?)
    }

    private func deviceRow(_ device: RemoteDevice) -> some View {
        let link = device.controlPlaneDeviceID.flatMap { remoteAccess.linkStatuses[$0] }
        let connected = link == .ready
        return HStack {
            Label(device.name, systemImage: "iphone")
            Spacer(minLength: 4)
            Circle()
                .fill(connected ? Color.green : Color.secondary.opacity(0.5))
                .frame(width: 7, height: 7)
        }
        .help(link.map(RemoteAccessSettingsView.label) ?? "Not connected")
        .accessibilityElement(children: .combine)
        .accessibilityValue(connected ? "Connected" : "Not connected")
    }
}

/// Health of the selected CLI, with refresh and prune close at hand.
private struct SidebarStatus: View {
    @ObservedObject var store: SessionStore
    let prune: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(statusColor)
                .frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 1) {
                // One line each: a footer whose height follows the column's
                // width makes the split view re-measure the sidebar forever.
                Text(headline)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.tail)
                if let version = store.cliCapabilities?.productVersion {
                    Text(version)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help("latch \(version)")
                }
            }
            Spacer(minLength: 0)
            Button {
                Task { await store.refresh(showsProgress: true) }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(store.isRefreshing)
            .help("Refresh Sessions")
            Menu {
                Button("Prune Finished Sessions…", action: prune)
            } label: {
                Label("More", systemImage: "ellipsis.circle")
            }
            .menuIndicator(.hidden)
            .fixedSize()
            .help("More")
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .menuStyle(.borderlessButton)
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .latchCard()
        .accessibilityElement(children: .contain)
    }

    private var hasErrors: Bool {
        store.cliDoctorReport?.findings.contains { $0.severity == "error" } == true
    }

    private var headline: String {
        if store.cliCapabilities == nil { return "Latch CLI unavailable" }
        return hasErrors ? "Latch needs attention" : "Latch is ready"
    }

    private var statusColor: Color {
        if store.cliCapabilities == nil { return .orange }
        return hasErrors ? .orange : .green
    }
}
