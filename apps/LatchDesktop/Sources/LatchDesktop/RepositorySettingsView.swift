import SwiftUI

enum CommitMessageProvider: String, Codable, CaseIterable, Identifiable, Sendable {
    case gemini
    case openrouter

    var id: String { rawValue }

    var label: String {
        switch self {
        case .gemini: return "Google Gemini"
        case .openrouter: return "OpenRouter"
        }
    }

    var keyLabel: String {
        switch self {
        case .gemini: return "Gemini API key"
        case .openrouter: return "OpenRouter API key"
        }
    }

    var defaultModel: String {
        switch self {
        case .gemini: return "gemini-3.5-flash-lite"
        case .openrouter: return "google/gemini-3.5-flash-lite"
        }
    }
}

/// `latch commit-message status --json`. Never carries key material.
struct CommitMessageStatus: Decodable, Equatable, Sendable {
    struct ProviderState: Decodable, Equatable, Sendable {
        /// `present`, `missing` or `unavailable`.
        let key: String
        /// Nil when the saved model cannot be read.
        let model: String?
        let defaultModel: String
        let customModel: Bool
    }

    struct Routing: Decodable, Equatable, Sendable {
        let order: [String]
        let allowFallbacks: Bool
    }

    let state: String
    /// Nil when the saved settings cannot be read.
    let provider: CommitMessageProvider?
    let providers: [String: ProviderState]
    let openrouterRouting: Routing?

    func state(for provider: CommitMessageProvider) -> ProviderState? {
        providers[provider.rawValue]
    }
}

/// Provider credentials belong to the Mac gateway, not the paired phone.
struct RepositorySettingsView: View {
    let cliPath: String
    @State private var provider: CommitMessageProvider = .gemini
    @State private var key = ""
    @State private var model = ""
    @State private var routingOrder = ""
    @State private var allowFallbacks = true
    @State private var configuration: CommitMessageStatus?
    @State private var status = "Checking…"
    @State private var busy = false
    @State private var message: String?
    @State private var confirmRemoval = false

    var body: some View {
        Form {
            Section {
                LabeledContent("Status", value: status)
                Picker("Provider", selection: providerBinding) {
                    ForEach(CommitMessageProvider.allCases) { Text($0.label).tag($0) }
                }
                .disabled(busy)
            } header: {
                SettingsSectionHeader("AI Commit Messages")
            } footer: {
                SettingsFootnote(providerFootnote)
            }

            Section {
                if let state = configuration?.state(for: provider) {
                    LabeledContent("Saved key", value: Self.keyDescription(state.key))
                }
                SecureField(provider.keyLabel, text: $key)
                    .disabled(busy)
                if !key.isEmpty && !validKey {
                    Text("Enter a 16–512 character key using letters, numbers, periods, hyphens or underscores.")
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
                SettingsSectionHeader("API Key")
            } footer: {
                SettingsFootnote("The key is stored in this Mac’s Keychain using the selected Latch CLI. Each provider keeps its own key. Status checks local access only; it does not verify the key with the provider.")
            }

            Section {
                TextField("Model", text: $model, prompt: Text(provider.defaultModel))
                    .disabled(busy)
                if !model.isEmpty && !validModel {
                    Text(modelHint).font(.caption).foregroundStyle(.secondary)
                }
                HStack {
                    Button("Save Model") { saveModel() }
                        .disabled(busy || !validModel || model == savedModel)
                    Button("Use Default") { resetModel() }
                        .disabled(busy || configuration?.state(for: provider)?.customModel != true)
                }
            } header: {
                SettingsSectionHeader("Model")
            } footer: {
                SettingsFootnote("Leave empty to use the default, \(provider.defaultModel). \(modelHint)")
            }

            if provider == .openrouter {
                Section {
                    TextField("Providers", text: $routingOrder, prompt: Text("Automatic"))
                        .disabled(busy)
                    if !validRouting {
                        Text("List up to 8 provider slugs, such as google-vertex, separated by commas.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Toggle("Allow fallback providers", isOn: $allowFallbacks)
                        .disabled(busy)
                    Button("Save Routing") { saveRouting() }
                        .disabled(busy || !validRouting || !routingChanged)
                } header: {
                    SettingsSectionHeader("OpenRouter Provider Routing")
                } footer: {
                    SettingsFootnote("Leave Providers empty to let OpenRouter choose. Otherwise OpenRouter tries the listed providers in order; turn off fallbacks to use only those providers. Slugs appear on each model’s page at openrouter.ai.")
                }
            }
        }
        .formStyle(.grouped)
        .task(id: cliPath) { key = ""; refresh() }
        .onDisappear { key = "" }
        .alert("Remove \(provider.keyLabel)?", isPresented: $confirmRemoval) {
            Button("Remove", role: .destructive) { remove() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("AI commit drafts using \(provider.label) will be unavailable until a key is saved again. Manual commits remain available.")
        }
    }

    private var providerFootnote: String {
        switch provider {
        case .gemini:
            return "Generating a draft sends selected staged changes to Google."
        case .openrouter:
            return "Generating a draft sends selected staged changes to OpenRouter, which forwards them to the model provider it routes to. Your OpenRouter privacy settings apply."
        }
    }

    /// Switching provider is saved immediately: it decides where drafts go.
    private var providerBinding: Binding<CommitMessageProvider> {
        Binding(
            get: { provider },
            set: { value in
                guard value != provider else { return }
                provider = value
                key = ""
                loadFields(for: value)
                perform(success: "Drafts now use \(value.label).", failure: "Could not change the provider.") { client in
                    try await client.setCommitMessageProvider(value)
                }
            }
        )
    }

    private var validKey: Bool { Self.isValidKey(key) }

    private var savedModel: String {
        guard let state = configuration?.state(for: provider), state.customModel else { return "" }
        return state.model ?? ""
    }

    private var validModel: Bool { Self.isValidModel(model, provider: provider) }

    private var modelHint: String {
        switch provider {
        case .gemini: return "Use a Gemini model identifier beginning with gemini-."
        case .openrouter: return "Use an OpenRouter model slug such as anthropic/claude-haiku-5.5."
        }
    }

    private var parsedRoutingOrder: [String] {
        Self.parseRouting(routingOrder)
    }

    private var validRouting: Bool { Self.isValidRouting(parsedRoutingOrder) }

    private var routingChanged: Bool {
        let saved = configuration?.openrouterRouting
        return parsedRoutingOrder != (saved?.order ?? []) || allowFallbacks != (saved?.allowFallbacks ?? true)
    }

    static func keyDescription(_ state: String) -> String {
        switch state {
        case "present": return "Saved in Keychain"
        case "missing": return "None"
        default: return "Keychain unavailable"
        }
    }

    /// ASCII letters and digits plus `-`, `.` and `_`: the alphabet shared by
    /// keys, model names and routing slugs.
    private static func isIdentifierByte(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
            || byte == 45 || byte == 46 || byte == 95
    }

    static func isValidKey(_ key: String) -> Bool {
        (16...512).contains(key.utf8.count) && key.utf8.allSatisfy(isIdentifierByte)
    }

    /// A front-line check; the CLI applies the authoritative validation.
    static func isValidModel(_ model: String, provider: CommitMessageProvider) -> Bool {
        switch provider {
        case .gemini:
            return (1...96).contains(model.utf8.count) && model.hasPrefix("gemini-")
                && model.utf8.allSatisfy(isIdentifierByte)
        case .openrouter:
            let parts = model.split(separator: "/", omittingEmptySubsequences: false)
            return (3...128).contains(model.utf8.count)
                && parts.count >= 2 && parts.allSatisfy { !$0.isEmpty }
                && !model.contains("..")
                && model.utf8.allSatisfy { isIdentifierByte($0) || $0 == 47 || $0 == 58 || $0 == 126 }
        }
    }

    static func parseRouting(_ text: String) -> [String] {
        text.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }

    static func isValidRouting(_ order: [String]) -> Bool {
        order.count <= 8 && order.allSatisfy { slug in
            (1...64).contains(slug.utf8.count) && !slug.contains("..") && !slug.hasPrefix("/") && !slug.hasPrefix(".")
                && !slug.hasPrefix("-") && !slug.hasPrefix("_")
                && slug.utf8.allSatisfy { isIdentifierByte($0) || $0 == 47 }
        }
    }

    private func loadFields(for provider: CommitMessageProvider) {
        let state = configuration?.state(for: provider)
        model = state?.customModel == true ? (state?.model ?? "") : ""
        let routing = configuration?.openrouterRouting
        routingOrder = (routing?.order ?? []).joined(separator: ", ")
        allowFallbacks = routing?.allowFallbacks ?? true
    }

    private func save() {
        let value = key
        let target = provider
        key = ""
        perform(success: "Key saved.", failure: keyFailure) { client in
            try await client.setCommitMessageKey(value, provider: target)
        }
    }

    private func remove() {
        let target = provider
        key = ""
        perform(success: "Key removed.", failure: keyFailure) { client in
            try await client.removeCommitMessageKey(provider: target)
        }
    }

    private func saveModel() {
        let value = model
        let target = provider
        perform(success: "Model saved.", failure: "Could not save the model. Check the identifier.") { client in
            try await client.setCommitMessageModel(value, provider: target)
        }
    }

    private func resetModel() {
        let target = provider
        model = ""
        perform(success: "Using the default model.", failure: "Could not reset the model.") { client in
            try await client.setCommitMessageModel(nil, provider: target)
        }
    }

    private func saveRouting() {
        let order = parsedRoutingOrder
        let fallbacks = allowFallbacks
        perform(success: "Routing saved.", failure: "Could not save routing. Check the provider slugs.") { client in
            try await client.setOpenRouterRouting(order: order, allowFallbacks: fallbacks)
        }
    }

    private var keyFailure: String {
        "Could not update the key. Check that the selected CLI supports AI commit messages and that your login Keychain is unlocked."
    }

    private func perform(success: String, failure: String, _ operation: @escaping @Sendable (LatchClient) async throws -> Void) {
        let path = cliPath
        busy = true
        message = nil
        Task {
            let result = await Task.detached { () -> Bool in
                do { try await operation(LatchClient(executableURL: URL(fileURLWithPath: path))); return true }
                catch { return false }
            }.value
            busy = false
            message = result ? success : failure
            refresh()
        }
    }

    private func refresh() {
        let path = cliPath
        busy = true
        Task {
            let fetched = await Task.detached { () -> CommitMessageStatus? in
                try? await LatchClient(executableURL: URL(fileURLWithPath: path)).commitMessageStatus()
            }.value
            guard path == cliPath else { return }
            let state: String
            if let fetched {
                let firstLoad = configuration == nil
                configuration = fetched
                if let saved = fetched.provider { provider = saved }
                if firstLoad || !busyEditing { loadFields(for: provider) }
                state = fetched.state
            } else {
                state = ""
            }
            switch state {
            case let value where value.hasPrefix("ready"): status = "Ready on this Mac"
            case "unconfigured": status = "No key configured"
            case let value where value.hasPrefix("unavailable"): status = "Keychain or model unavailable"
            default: status = "Unable to check; update or select a compatible Latch CLI."
            }
            busy = false
        }
    }

    /// Keep unsaved model or routing text when a background refresh lands.
    private var busyEditing: Bool {
        model != savedModel || routingChanged
    }
}
