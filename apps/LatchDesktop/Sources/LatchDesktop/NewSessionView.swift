import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct NewSessionView: View {
    @ObservedObject var store: SessionStore
    @Binding var isPresented: Bool
    @State private var request = NewSessionRequest()
    @State private var advanced = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("New Session").font(.title2).fontWeight(.semibold)
            Form {
                TextField("Name (optional)", text: $request.name)
                TextField("Title (optional)", text: $request.title)
                TextField("Working directory", text: $request.cwd)
                Picker("Start", selection: $request.agent) {
                    ForEach(availableAgents) { agent in
                        Text(agent.title).tag(agent)
                    }
                }
                if !store.canLaunchAgents {
                    Text("Update the Latch CLI to start Claude Code or Codex so the session can open in Chat.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                DisclosureGroup("Advanced", isExpanded: $advanced) {
                    if request.agent == .shell {
                        TextField("Command (optional)", text: $request.command)
                    }
                    HStack {
                        TextField("Columns", value: $request.cols, format: .number)
                        TextField("Rows", value: $request.rows, format: .number)
                    }
                }
            }
            HStack {
                Button("Cancel", role: .cancel) { isPresented = false }
                Spacer()
                Button("Create") { submit(open: false) }
                Button("Create and Open") { submit(open: true) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(24)
        .frame(width: 480)
    }

    /// Agents are offered only when the selected CLI can launch them with
    /// their identity; a shell is always available.
    private var availableAgents: [SessionAgent] {
        store.canLaunchAgents ? SessionAgent.allCases : [.shell]
    }

    private func submit(open: Bool) {
        var submitted = request
        if submitted.agent != .shell { submitted.command = "" }
        isPresented = false
        Task { await store.create(submitted, openAfterCreation: open) }
    }
}
