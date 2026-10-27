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
            VStack(alignment: .leading, spacing: 3) {
                Text("New Session").font(.title2).fontWeight(.bold)
                Text("Keeps running after you close the terminal.")
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 6) {
                fieldLabel("Start with")
                HStack(spacing: 10) {
                    ForEach(SessionAgent.displayOrder) { agent in
                        AgentChoice(
                            agent: agent,
                            isSelected: request.agent == agent,
                            isAvailable: availableAgents.contains(agent)
                        ) {
                            request.agent = agent
                        }
                    }
                }
                if !store.canLaunchAgents {
                    Text("Update the Latch CLI to start Claude Code or Codex so the session can open in Chat.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                fieldLabel("Working directory")
                HStack(spacing: 8) {
                    TextField("Working directory", text: $request.cwd)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.body, design: .monospaced))
                        .labelsHidden()
                    Button("Choose…", action: chooseDirectory)
                }
                let recents = store.recentDirectories()
                if !recents.isEmpty {
                    HStack(spacing: 6) {
                        Text("Recent")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        ForEach(recents, id: \.self) { directory in
                            Button((directory as NSString).lastPathComponent) {
                                request.cwd = directory
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .help(directory)
                        }
                    }
                }
            }

            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 6) {
                    fieldLabel("Name")
                    TextField("From directory", text: $request.name)
                        .textFieldStyle(.roundedBorder)
                }
                VStack(alignment: .leading, spacing: 6) {
                    fieldLabel("Title")
                    TextField("Optional", text: $request.title)
                        .textFieldStyle(.roundedBorder)
                }
            }

            DisclosureGroup("Advanced", isExpanded: $advanced) {
                VStack(alignment: .leading, spacing: 10) {
                    if request.agent == .shell {
                        TextField("Command (optional)", text: $request.command)
                            .textFieldStyle(.roundedBorder)
                    }
                    HStack {
                        TextField("Columns", value: $request.cols, format: .number)
                            .textFieldStyle(.roundedBorder)
                        Text("×").foregroundStyle(.secondary)
                        TextField("Rows", value: $request.rows, format: .number)
                            .textFieldStyle(.roundedBorder)
                    }
                }
                .padding(.top, 8)
            }

            Divider()

            HStack {
                Button("Cancel", role: .cancel) { isPresented = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Create") { submit(open: false) }
                Button("Create and Open") { submit(open: true) }
                    .keyboardShortcut(.defaultAction)
                    .latchProminentButtonStyle()
            }
            .controlSize(.large)
        }
        .padding(24)
        .frame(width: 540)
    }

    private func fieldLabel(_ text: String) -> some View {
        Text(text)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
    }

    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.directoryURL = URL(fileURLWithPath: request.cwd, isDirectory: true)
        if panel.runModal() == .OK, let url = panel.url {
            request.cwd = url.path
        }
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

private extension SessionAgent {
    /// Agents first, so the common choice sits under the pointer.
    static let displayOrder: [SessionAgent] = [.claude, .codex, .shell]
}

/// One selectable card in the New Session agent picker.
private struct AgentChoice: View {
    let agent: SessionAgent
    let isSelected: Bool
    let isAvailable: Bool
    let select: () -> Void

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        Button(action: select) {
            VStack(alignment: .leading, spacing: 8) {
                AgentLogo(agent: agent, size: 28)
                Text(agent.title)
                    .fontWeight(.semibold)
                    .foregroundStyle(.primary)
                Text(agent.executable ?? "Login shell")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(
                isSelected ? AnyShapeStyle(Color.accentColor.opacity(0.12)) : AnyShapeStyle(.quaternary.opacity(0.5)),
                in: shape
            )
            .overlay(shape.strokeBorder(isSelected ? Color.accentColor : Color.clear, lineWidth: 2))
            .overlay(alignment: .topTrailing) {
                if isSelected {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 16))
                        .foregroundStyle(.white, Color.accentColor)
                        .padding(9)
                }
            }
            .contentShape(shape)
        }
        .buttonStyle(.plain)
        .disabled(!isAvailable)
        .opacity(isAvailable ? 1 : 0.45)
        .accessibilityLabel(agent.title)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}
