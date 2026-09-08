import LatchMobileKit
import SwiftUI

/// The settings tab: linking this phone to a computer, and what that link can do.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing
    @State private var confirmingUnlink = false
    @State private var choosingDefaultFolder = false
    @Environment(DiagnosticsRunner.self) private var diagnostics

    var body: some View {
        NavigationStack {
            Form {
                sessionViewSection
                newSessionSection
                remoteAccessSection

                switch model.linkState {
                case .linked:
                    linkedSections
                default:
                    linkStatusSection
                }
                if pairing.record != nil {
                    diagnosticsSection
                }
            }
            .navigationTitle("Settings")
        }
    }

    // MARK: - Session view

    /// Placed above *Remote access* because it is the setting a person looks
    /// for first: it decides what happens when they tap a session.
    @ViewBuilder
    private var sessionViewSection: some View {
        @Bindable var model = model
        Section {
            Picker("Session view", selection: $model.sessionPresentation) {
                ForEach(SessionPresentation.allCases, id: \.self) { presentation in
                    Text(presentation.label).tag(presentation)
                }
            }
            .pickerStyle(.inline)
        } header: {
            Text("Session view")
        } footer: {
            Text("""
            Terminal opens the session's live terminal and takes it from whatever is \
            attached on your Mac. Sessions without a Claude or Codex connector — every \
            plain shell — always open in the terminal.
            """)
        }

        Section {
            Picker("Terminal size", selection: $model.terminalSize) {
                ForEach(TerminalSize.allCases, id: \.self) { size in
                    Text(size.label).tag(size)
                }
            }
        } footer: {
            Text("""
            Match the Mac attaches at the size the pane already has, so nothing on \
            your Mac resizes or reflows. The other choices set the grid here and fit \
            the text to it.
            """)
        }
    }

    // MARK: - New sessions

    /// Only shown when the linked Mac serves the folder browser. A saved path
    /// is a preference, not a credential: it names a folder on the Mac and
    /// grants nothing on its own.
    @ViewBuilder
    private var newSessionSection: some View {
        if model.advertisesNewSessionCreation {
            Section {
                Button {
                    choosingDefaultFolder = true
                } label: {
                    LabeledContent("Default folder", value: defaultFolderSummary)
                }
                .disabled(!model.canBrowseNewSessionFolders)
                .accessibilityHint(
                    model.canBrowseNewSessionFolders
                        ? "Browse your Mac and choose where new sessions start"
                        : "Unavailable until this phone has control of your Mac"
                )
                if model.defaultNewSessionFolder != nil {
                    Button("Use my Mac home folder", role: .destructive) {
                        model.clearDefaultNewSessionFolder()
                    }
                }
            } header: {
                Text("New sessions")
            } footer: {
                Text(newSessionFooter)
            }
            .sheet(isPresented: $choosingDefaultFolder) {
                FolderPickerView(mode: .chooseDefault)
            }
        }
    }

    private var defaultFolderSummary: String {
        model.defaultNewSessionFolder ?? "Mac home folder"
    }

    private var newSessionFooter: String {
        if let explanation = model.newSessionUnavailableExplanation {
            return explanation
        }
        return """
        Where the folder picker starts. Choosing somewhere else for one session does not \
        change it. Starting a session creates a plain shell on your Mac — it does not open \
        the session or run an agent.
        """
    }

    // MARK: - Remote access

    /// The sole Remote Link enrollment entry point.
    @ViewBuilder
    private var remoteAccessSection: some View {
        Section {
            NavigationLink {
                PairingView()
            } label: {
                LabeledContent("Remote access", value: pairingSummary)
            }
        } footer: {
            Text("""
            Pairing links this phone to your Mac's identity directly, using a code \
            your Mac shows for five minutes.
            """)
        }
    }

    private var pairingSummary: String {
        switch pairing.state {
        case .paired(let record): return record.mac.displayName
        case .revoked: return "Revoked"
        case .confirming, .comparing, .enrolling, .scanning: return "Pairing…"
        case .idle, .failed: return "Not paired"
        }
    }

    // MARK: - Link owner

    /// The owner's typed state when the gateway is not usable, so a person
    /// can tell "your Mac is asleep" from "pair again" without a debugger.
    @ViewBuilder
    private var linkStatusSection: some View {
        Section {
            LabeledContent("Connection", value: Self.describe(model.linkState))
            if let detail = Self.detail(model.linkState) {
                Text(detail).font(.footnote).foregroundStyle(.secondary)
            }
            if model.canRetryAutomatically, pairing.record != nil {
                Button("Try now") { Task { await model.rediscover() } }
            }
        } header: {
            Text("Linked computer")
        }
    }

    static func describe(_ state: AppModel.LinkState) -> String {
        switch state {
        case .unlinked: return "Not connected"
        case .connecting: return "Connecting…"
        case .linked: return "Connected"
        case .interrupted(.suspended, _): return "Reconnecting…"
        case .interrupted(.backoff, _): return "Connection lost"
        case .interrupted: return "Reconnecting…"
        case .macOffline: return "Mac offline"
        case .revoked: return "Unpaired"
        case .pairingRequired: return "Pair again"
        case .incompatible: return "Update needed"
        case .failed: return "Failed"
        }
    }

    static func detail(_ state: AppModel.LinkState) -> String? {
        switch state {
        case .interrupted(.backoff(_, _, let reason), _): return reason
        case .macOffline: return "Your Mac is not connected to the relay. It may be asleep or have remote access turned off."
        case .revoked(let reason), .pairingRequired(let reason), .failed(let reason): return reason
        case .incompatible(let mismatch): return mismatch.detail
        default: return nil
        }
    }

    // MARK: - Diagnostics

    /// Opt-in. Runs real suspend/resume cycles through the one link owner and
    /// records content-free stage timings for the physical matrix.
    @ViewBuilder
    private var diagnosticsSection: some View {
        @Bindable var diagnostics = diagnostics
        Section {
            Toggle("Skip local network attempt", isOn: $diagnostics.settings.skipLANAttempt)
                .onChange(of: diagnostics.settings.skipLANAttempt) { _, skip in
                    Task { await model.setDiagnosticsSkipLAN(skip) }
                }
            Toggle("Include a terminal attach", isOn: $diagnostics.settings.includeTerminal)
            Stepper("Cycles: \(diagnostics.settings.cycles)", value: $diagnostics.settings.cycles, in: 1...500)
            if diagnostics.isRunning {
                HStack {
                    ProgressView()
                    Text("Cycle \(diagnostics.attempts.count + 1) of \(diagnostics.settings.cycles)")
                    Spacer()
                    Button("Stop", role: .destructive) { diagnostics.cancel() }
                }
            } else {
                Button("Run reconnect cycles") { diagnostics.run(subject: model) }
                    .disabled(pairing.record == nil)
            }
            if !diagnostics.attempts.isEmpty {
                LabeledContent("Succeeded", value: "\(diagnostics.successCount) of \(diagnostics.attempts.count)")
                if let p95 = diagnostics.p95(.applicationReady) {
                    LabeledContent("p95 to usable gateway", value: "\(p95) ms")
                }
                if let p95 = diagnostics.p95(.linkReady) {
                    LabeledContent("p95 to link ready", value: "\(p95) ms")
                }
                if let location = diagnostics.location {
                    LabeledContent("Saved as", value: location)
                }
            }
            if let error = diagnostics.lastError {
                Text(error).font(.footnote).foregroundStyle(.red)
            }
            ForEach(model.recentStages.suffix(6).reversed(), id: \.self) { sample in
                LabeledContent(sample.stage.rawValue, value: "\(sample.milliseconds) ms")
                    .font(.footnote)
            }
        } header: {
            Text("Diagnostics")
        } footer: {
            Text("""
            Each cycle drops the secure connection the way backgrounding does, reconnects through the             same owner, lists sessions, and reads one preview. Timings are written to Files › Latch ›             latch-diagnostics and contain no session names, prompts, paths, or output. Skipping the             local network attempt measures the relay path from a network where your Mac is also nearby.
            """)
        }
    }

    // MARK: - Linked

    @ViewBuilder
    private var linkedSections: some View {
        Section {
            LabeledContent("Connection", value: "Secure paired connection")
            if let path = model.remotePath {
                LabeledContent("Path", value: path.label)
                    .accessibilityHint(path.detail)
            }
            if let summary = model.remotePathTally.summary {
                LabeledContent("Paths so far", value: summary)
                Button("Reset path counters", role: .destructive) {
                    model.resetRemotePathTally()
                }
            }
            if let version = model.productVersion {
                LabeledContent("Latch", value: version)
            }
            LabeledContent("Protocol", value: "v\(LatchContract.protocolVersion)")
        } header: {
            Text("Linked computer")
        } footer: {
            if let path = model.remotePath {
                Text(path.detail)
            }
        }

        // What discovery said this gateway can do. It is shown rather than
        // hidden because it explains why a screen is missing a control: the
        // app never probes an endpoint to find out, so this is the answer.
        Section {
            ForEach(GatewayEndpointsName.allCases, id: \.self) { endpoint in
                CapabilityRow(
                    name: Self.label(endpoint),
                    available: available(endpoint)
                )
            }
            ForEach(GatewayFeaturesName.allCases, id: \.self) { feature in
                CapabilityRow(
                    name: Self.label(feature),
                    available: available(feature)
                )
            }
        } header: {
            Text("What this gateway offers")
        } footer: {
            Text("""
            Reported by the gateway's discovery document. Anything switched off \
            here is missing from the app on purpose.
            """)
        }

        Section {
            Button("Check again") {
                Task { await model.rediscover() }
            }
            Button("Disconnect", role: .destructive) {
                confirmingUnlink = true
            }
            .confirmationDialog(
                "Disconnect from this Mac?",
                isPresented: $confirmingUnlink,
                titleVisibility: .visible
            ) {
                Button("Disconnect", role: .destructive) {
                    model.unlink()
                }
            } message: {
                Text("This closes the current secure connection. Your pairing stays on this phone.")
            }
        }
    }

    private func available(_ endpoint: GatewayEndpointsName) -> Bool {
        guard case .linked(let capabilities) = model.linkState else { return false }
        return GatewayCompatibility.supports(endpoint: endpoint, capabilities: capabilities)
    }

    private func available(_ feature: GatewayFeaturesName) -> Bool {
        guard case .linked(let capabilities) = model.linkState else { return false }
        return GatewayCompatibility.supports(feature: feature, capabilities: capabilities)
    }

    static func label(_ endpoint: GatewayEndpointsName) -> String {
        switch endpoint {
        case .sessions: return "Session list"
        case .preview: return "Screen preview"
        case .terminal: return "Terminal"
        case .conversation: return "Conversation"
        case .browseDirectories: return "Folder browser"
        case .createSession: return "Session creation"
        }
    }

    static func label(_ feature: GatewayFeaturesName) -> String {
        switch feature {
        case .exclusiveTerminal: return "Exclusive terminal"
        }
    }
}

private struct CapabilityRow: View {
    let name: String
    let available: Bool

    var body: some View {
        HStack {
            Text(name)
            Spacer()
            Image(systemName: available ? "checkmark.circle.fill" : "minus.circle")
                .foregroundStyle(available ? Color.green : Color.secondary)
                .accessibilityLabel(available ? "available" : "unavailable")
        }
    }
}
