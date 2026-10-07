import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SettingsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @ObservedObject var remoteAccess: RemoteAccessController

    private enum Tab: String, Hashable {
        case terminal
        case remoteAccess
        case cli
        case updates
    }

    @State private var selection: Tab = .terminal

    var body: some View {
        TabView(selection: $selection) {
            terminalTab
                .tabItem { Label("Terminal", systemImage: "terminal") }
                .tag(Tab.terminal)
            RemoteAccessSettingsView(controller: remoteAccess)
                .tabItem { Label("Remote Access", systemImage: "iphone.and.arrow.forward") }
                .tag(Tab.remoteAccess)
            cliTab
                .tabItem { Label("Latch CLI", systemImage: "wrench.and.screwdriver") }
                .tag(Tab.cli)
            updatesTab
                .tabItem { Label("Updates", systemImage: "arrow.down.circle") }
                .tag(Tab.updates)
        }
        .frame(width: 580, height: 500)
    }

    // MARK: - Terminal

    private var terminalTab: some View {
        Form {
            Section {
                Picker("Preferred terminal", selection: Binding(
                    get: { store.preferredTerminal },
                    set: { store.preferredTerminal = $0 }
                )) {
                    ForEach(PreferredTerminal.allCases) { terminal in
                        Text(terminal.rawValue + (TerminalLauncher.isInstalled(terminal) ? "" : " (not installed)"))
                            .tag(terminal)
                    }
                }

                Picker("Open sessions in", selection: Binding(
                    get: { store.terminalOpenBehavior },
                    set: { store.terminalOpenBehavior = $0 }
                )) {
                    ForEach(TerminalOpenBehavior.allCases) { behavior in
                        Text(behavior.label + (store.preferredTerminal.supports(behavior) ? "" : " (unavailable)"))
                            .tag(behavior)
                    }
                }
                .disabled(store.preferredTerminal.supportedOpenBehaviors.isEmpty)

                Toggle("Open in background", isOn: $store.terminalOpenInBackground)
            } header: {
                SettingsSectionHeader("Opening Sessions")
            } footer: {
                SettingsFootnote(openBehaviorFootnote)
            }

            if store.preferredTerminal == .custom {
                Section {
                    LabeledContent("Application") {
                        Button("Choose Terminal Application…") {
                            chooseCustomTerminalApplication()
                        }
                    }
                    TextField("Executable", text: $store.customTerminalExecutable)
                    TextField("Argument template", text: $store.customTerminalTemplate)
                } header: {
                    SettingsSectionHeader("Custom Terminal")
                } footer: {
                    SettingsFootnote(
                        "Choose any .app not listed above, then adjust its launch arguments if needed. Required placeholders: {latch} and {session}. Arguments are parsed directly and never passed to a shell."
                    )
                }
            }

            Section {
                SettingsFootnote(
                    "This is the default app Latch uses when opening a session. Closing Latch never stops sessions."
                )
            }
        }
        .formStyle(.grouped)
    }

    // MARK: - CLI

    private var cliTab: some View {
        Form {
            Section {
                TextField("Executable path", text: $store.latchExecutablePath)
                LabeledContent("Currently using") {
                    Text(store.activeCLIPath)
                        .textSelection(.enabled)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                HStack(spacing: 10) {
                    Button(store.isDiscoveringCLI ? "Searching…" : "Run `where latch`") {
                        Task { await store.discoverCLI() }
                    }
                    .disabled(store.isDiscoveringCLI)
                    Button("Diagnose") {
                        Task { await store.refreshCLIDiagnostics() }
                    }
                    Spacer()
                    Button("Use This CLI") { store.useConfiguredCLI() }
                        .disabled(!store.selectedCLIIsExecutable)
                }
            } header: {
                SettingsSectionHeader("Executable")
            } footer: {
                SettingsFootnote(
                    "Latch Desktop uses the independently installed CLI selected here. CLI updates replace the command, the remote-access helper, and the pinned latchd payload."
                )
            }

            discoverySection
            statusSection
            cliUpdatesSection
        }
        .formStyle(.grouped)
    }

    @ViewBuilder private var discoverySection: some View {
        if !store.discoveredLatchExecutables.isEmpty {
            Section {
                ForEach(store.discoveredLatchExecutables, id: \.self) { path in
                    Button {
                        store.selectCLI(path)
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "terminal")
                                .foregroundStyle(.secondary)
                            Text(path)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            Text(path == store.activeCLIPath ? "In use" : "Use")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(path == store.activeCLIPath)
                }
            } header: {
                SettingsSectionHeader("Found by `where latch`")
            }
        } else {
            Section {
                Text(store.cliInstallCommand)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                Button("Run Install Command in Terminal…") { store.runCLIInstaller() }
            } header: {
                SettingsSectionHeader("Install the CLI")
            } footer: {
                SettingsFootnote("No standalone Latch CLI was found on this Mac.")
            }
        }
    }

    @ViewBuilder private var statusSection: some View {
        if store.cliCapabilities != nil || store.cliDoctorReport != nil {
            Section {
                if let capabilities = store.cliCapabilities {
                    LabeledContent("CLI version", value: capabilities.productVersion)
                    LabeledContent("Protocol", value: String(capabilities.protocolVersion))
                    if !capabilities.capabilities.extensions.isEmpty {
                        LabeledContent(
                            "Extensions",
                            value: capabilities.capabilities.extensions.joined(separator: ", ")
                        )
                    }
                }
                if let doctor = store.cliDoctorReport {
                    if let kernel = doctor.kernel {
                        LabeledContent("Selected kernel", value: kernel)
                    }
                    ForEach(doctor.findings) { finding in
                        Label(
                            finding.message,
                            systemImage: finding.severity == "error"
                                ? "exclamationmark.octagon"
                                : "exclamationmark.triangle"
                        )
                        .font(.caption)
                        .foregroundStyle(finding.severity == "error" ? Color.red : Color.secondary)
                    }
                }
            } header: {
                SettingsSectionHeader("Status")
            }
        }
    }

    private var cliUpdatesSection: some View {
        Section {
            HStack(spacing: 10) {
                Button(store.isCheckingCLIUpdate ? "Checking…" : "Check for CLI Update") {
                    Task { await store.checkForCLIUpdate() }
                }
                .disabled(store.isCheckingCLIUpdate || store.isUpdatingCLI)
                if store.cliUpdateReport?.status == .available,
                   store.cliCapabilities?.capabilities.selfUpdate == true {
                    Button(store.isUpdatingCLI ? "Updating…" : "Install CLI Update") {
                        Task { await store.updateCLI() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(store.isUpdatingCLI)
                }
                Spacer()
            }
            cliUpdateStatus
        } header: {
            SettingsSectionHeader("CLI Updates")
        }
    }

    // MARK: - App updates

    private var updatesTab: some View {
        Form {
            Section {
                Toggle("Check for updates automatically", isOn: $updates.automaticChecks)
                LabeledContent("Installed version") {
                    HStack(spacing: 12) {
                        Text(updates.installedVersionLabel)
                            .foregroundStyle(.secondary)
                        Button("Check Now") {
                            Task { await updates.check(userInitiated: true) }
                        }
                    }
                }
            } header: {
                SettingsSectionHeader("Desktop App Updates")
            } footer: {
                SettingsFootnote(
                    "Latch installs updates from its signed GitHub releases and verifies that each one is signed by the same developer before replacing itself."
                )
            }
        }
        .formStyle(.grouped)
    }

    private var openBehaviorFootnote: String {
        let background =
            "Open in background attaches the session without bringing the terminal to the front."
        switch store.preferredTerminal {
        case .custom:
            return "Your argument template decides how a custom terminal opens, so the window-or-tab setting does not apply. \(background)"
        case .ghostty:
            return "Ghostty cannot be told to open a tab from another app, so sessions always open in a new window. \(background)"
        case .terminal:
            return "The Open button uses the window-or-tab setting; its menu can still override it per session. Opening a Terminal tab sends Command-T, so macOS asks Latch to control System Events once. If that is refused, Latch opens a new window instead. \(background) A Terminal tab may flash briefly so the keystroke can run."
        case .iTerm:
            return "The Open button uses the window-or-tab setting; its menu can still override it per session. \(background)"
        }
    }

    @ViewBuilder private var cliUpdateStatus: some View {
        if let report = store.cliUpdateReport {
            switch report.status {
            case .current:
                SettingsFootnote("Latch CLI \(report.currentVersion) is current.")
            case .available:
                VStack(alignment: .leading, spacing: 8) {
                    Text("Latch CLI \(report.latestVersion ?? "update") is available; you have \(report.currentVersion).")
                    if let releaseURL = report.releaseURL {
                        Link("CLI release notes", destination: releaseURL)
                    }
                    if store.cliCapabilities?.capabilities.selfUpdate != true {
                        Text("This CLI cannot replace itself. Use its package manager or run:")
                        Text(store.cliInstallCommand)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                        Button("Run Installer in Terminal…") { store.runCLIInstaller() }
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            case .installed:
                SettingsFootnote(
                    "Updated the CLI from \(report.currentVersion) to \(report.latestVersion ?? "the latest release")."
                )
            }
        }
    }

    private func chooseCustomTerminalApplication() {
        let panel = NSOpenPanel()
        panel.title = "Choose Terminal Application"
        panel.message = "Select the application Latch should use to open session attachments."
        panel.prompt = "Choose"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.applicationBundle]

        guard panel.runModal() == .OK, let applicationURL = panel.url else { return }
        do {
            store.customTerminalExecutable = try TerminalLauncher.executablePath(forApplicationURL: applicationURL)
        } catch {
            store.errorMessage = error.localizedDescription
        }
    }
}
