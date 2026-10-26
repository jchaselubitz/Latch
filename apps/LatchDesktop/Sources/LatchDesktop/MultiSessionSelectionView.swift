import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct MultiSessionSelectionView: View {
    @ObservedObject var store: SessionStore
    @State private var showingStop = false

    private var selectedSessions: [SessionSummary] {
        store.sessions.filter { store.selection.contains($0.id) }
    }

    private var liveCount: Int {
        selectedSessions.filter(\.state.isLive).count
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("\(store.selection.count) sessions selected")
                .font(.title2)
                .fontWeight(.semibold)
            Text(summary)
                .foregroundStyle(.secondary)
            if liveCount > 0 {
                Button("Stop Selected…", role: .destructive) {
                    showingStop = true
                }
                .buttonStyle(.bordered)
            }
            List(selectedSessions) { session in
                VStack(alignment: .leading, spacing: 4) {
                    Text(session.name)
                        .fontWeight(.medium)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(session.displaySubtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(session.state.rawValue.capitalized)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            }
            .listStyle(.inset)
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .confirmationDialog(
            "Stop \(liveCount) session\(liveCount == 1 ? "" : "s")?",
            isPresented: $showingStop,
            titleVisibility: .visible
        ) {
            Button("Stop", role: .destructive) {
                Task { await store.stopSessions(store.selectedLiveSessionIDs, force: false) }
            }
            Button("Force Stop", role: .destructive) {
                Task { await store.stopSessions(store.selectedLiveSessionIDs, force: true) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Stopping ends each child process but retains its final screen for later inspection.")
        }
    }

    private var summary: String {
        let running = selectedSessions.filter { $0.state == .running }.count
        let exited = selectedSessions.filter { $0.state == .exited }.count
        let lost = selectedSessions.filter { $0.state == .lost }.count
        var parts: [String] = []
        if running > 0 { parts.append("\(running) running") }
        if exited > 0 { parts.append("\(exited) exited") }
        if lost > 0 { parts.append("\(lost) lost") }
        return parts.joined(separator: ", ")
    }
}
