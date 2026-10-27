import SwiftUI

/// Provider credentials belong to the Mac gateway, not the paired phone.
struct RepositorySettingsView: View {
    let cliPath: String
    @State private var key = ""
    @State private var status = "Checking…"
    @State private var busy = false
    @State private var message: String?
    @State private var confirmRemoval = false

    var body: some View {
        Form {
            Section {
                LabeledContent("Status", value: status)
                SecureField("Gemini API key", text: $key)
                    .disabled(busy)
                if !key.isEmpty && !validKey {
                    Text("Enter a 16–512 character key using letters, numbers, hyphens or underscores.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                HStack {
                    Button("Save Key") { save() }
                        .disabled(busy || !validKey)
                    Button("Remove Key", role: .destructive) { confirmRemoval = true }
                        .disabled(busy)
                    Button("Refresh Status") { refresh() }
                        .disabled(busy)
                }
                if let message {
                    Text(message).font(.caption).foregroundStyle(.secondary)
                }
            } header: {
                SettingsSectionHeader("AI Commit Messages")
            } footer: {
                SettingsFootnote("The key is stored in this Mac’s Keychain using the selected Latch CLI. Generating a draft sends selected staged changes to Google. Status checks local access only; they do not verify the key with Google.")
            }
        }
        .formStyle(.grouped)
        .task(id: cliPath) { key = ""; refresh() }
        .onDisappear { key = "" }
        .alert("Remove Gemini API key?", isPresented: $confirmRemoval) {
            Button("Remove", role: .destructive) { remove() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("AI commit drafts will be unavailable until a key is saved again. Manual commits remain available.")
        }
    }

    private var validKey: Bool {
        (16...512).contains(key.utf8.count) && key.utf8.allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95
        }
    }

    private func save() {
        let value = key
        key = ""
        perform(success: "Key saved.") { client in try await client.setGeminiKey(value) }
    }

    private func remove() {
        key = ""
        perform(success: "Key removed.") { client in try await client.removeGeminiKey() }
    }

    private func perform(success: String, _ operation: @escaping @Sendable (LatchClient) async throws -> Void) {
        let path = cliPath
        busy = true
        message = nil
        Task {
            let result = await Task.detached { () -> Bool in
                do { try await operation(LatchClient(executableURL: URL(fileURLWithPath: path))); return true }
                catch { return false }
            }.value
            busy = false
            message = result ? success : "Could not update the key. Check that the selected CLI supports AI commit messages and that your login Keychain is unlocked."
            refresh()
        }
    }

    private func refresh() {
        let path = cliPath
        busy = true
        Task {
            let result = await Task.detached { () -> String in
                do { return try await LatchClient(executableURL: URL(fileURLWithPath: path)).geminiKeyStatus() }
                catch { return "Unable to check; update or select a compatible Latch CLI." }
            }.value
            guard path == cliPath else { return }
            switch result {
            case let value where value.hasPrefix("ready"): status = "Ready on this Mac"
            case "unconfigured": status = "No key configured"
            case let value where value.hasPrefix("unavailable"): status = "Keychain or model unavailable"
            default: status = "Unable to check; update or select a compatible Latch CLI."
            }
            busy = false
        }
    }
}
