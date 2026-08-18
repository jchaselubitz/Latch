import Foundation
import Combine
import AppKit

@MainActor
final class SessionStore: ObservableObject {
    @Published private(set) var sessions: [SessionSummary] = []
    @Published private(set) var details: InspectReport?
    @Published var selection = Set<String>()
    @Published var search = ""
    @Published var stateFilter: SessionState?
    @Published var isRefreshing = false
    @Published var errorMessage: String?
    @Published var prunePreview: PruneReport?
    @Published var shouldPresentNewSession = false
    @Published var shouldPresentPrune = false
    @Published var shouldPresentCLISetup = false
    @Published private(set) var discoveredLatchExecutables: [String] = []
    @Published private(set) var isDiscoveringCLI = false
    @Published private(set) var cliCapabilities: CapabilitiesReport?
    @Published private(set) var cliDoctorReport: DoctorReport?
    @Published private(set) var cliUpdateReport: CLIUpdateReport?
    @Published private(set) var isCheckingCLIUpdate = false
    @Published private(set) var isUpdatingCLI = false
    @Published private(set) var activeCLIPath: String
    @Published var customTerminalExecutable: String {
        didSet { UserDefaults.standard.set(customTerminalExecutable, forKey: "customTerminalExecutable") }
    }
    @Published var customTerminalTemplate: String {
        didSet { UserDefaults.standard.set(customTerminalTemplate, forKey: "customTerminalTemplate") }
    }
    @Published var terminalOpenInBackground: Bool {
        didSet { UserDefaults.standard.set(terminalOpenInBackground, forKey: "terminalOpenInBackground") }
    }
    @Published var latchExecutablePath: String {
        didSet {
            if latchExecutablePath.isEmpty {
                LatchClient.preferences.removeObject(forKey: "latchExecutablePath")
            } else {
                LatchClient.preferences.set(latchExecutablePath, forKey: "latchExecutablePath")
            }
            client = LatchClient()
            activeCLIPath = client.executableURL.path
        }
    }

    @Published private var preferredTerminalRaw: String
    @Published private var terminalOpenBehaviorRaw: String

    private var client: LatchClient
    private var refreshRequested = false
    /// Coalesces overlapping refresh work without publishing `isRefreshing` on
    /// every background poll — that publish alone was enough to rebuild menus.
    private var refreshInFlight = false
    /// Background polls tolerate one bad answer before they say anything: a
    /// single slow or half-answered query is normal on a busy Mac, and a
    /// banner that appears and clears itself five seconds later reads as a
    /// fault in Latch rather than the passing hiccup it is.
    private var consecutiveRefreshFailures = 0
    private static let refreshFailuresBeforeReporting = 2
    private static let activePollNanoseconds: UInt64 = 5_000_000_000
    private static let idlePollNanoseconds: UInt64 = 60_000_000_000
    private var pollingTask: Task<Void, Never>?
    private var activationCancellable: AnyCancellable?
    private weak var chrome: AppChromeState?
    private var companionRefreshers: [@MainActor () async -> Void] = []

    var cliInstallCommand: String { LatchClient.installCommand }
    var selectedCLIIsExecutable: Bool {
        !latchExecutablePath.isEmpty &&
            FileManager.default.isExecutableFile(atPath: latchExecutablePath)
    }

    init(client: LatchClient = LatchClient()) {
        self.client = client
        activeCLIPath = client.executableURL.path
        preferredTerminalRaw = UserDefaults.standard.string(forKey: "preferredTerminal")
            ?? PreferredTerminal.terminal.rawValue
        terminalOpenBehaviorRaw = UserDefaults.standard.string(forKey: "terminalOpenBehavior")
            ?? TerminalOpenBehavior.newWindow.rawValue
        customTerminalExecutable = UserDefaults.standard.string(forKey: "customTerminalExecutable") ?? ""
        customTerminalTemplate = UserDefaults.standard.string(forKey: "customTerminalTemplate")
            ?? "-e {latch} attach {session}"
        terminalOpenInBackground = UserDefaults.standard.bool(forKey: "terminalOpenInBackground")
        latchExecutablePath = LatchClient.preferences.string(forKey: "latchExecutablePath") ?? ""
    }

    func attachChrome(_ chrome: AppChromeState) {
        self.chrome = chrome
        syncChrome()
    }

    /// Extra work that should ride the same cadence as session polling (for
    /// example remote-access status) instead of running a second 5s timer.
    func addCompanionRefresh(_ refresh: @escaping @MainActor () async -> Void) {
        companionRefreshers.append(refresh)
    }

    var preferredTerminal: PreferredTerminal {
        get { PreferredTerminal(rawValue: preferredTerminalRaw) ?? .terminal }
        set {
            preferredTerminalRaw = newValue.rawValue
            UserDefaults.standard.set(newValue.rawValue, forKey: "preferredTerminal")
        }
    }

    /// The launch shape the plain Open action uses; a terminal that cannot honour it
    /// still opens the session in a new window.
    var terminalOpenBehavior: TerminalOpenBehavior {
        get { TerminalOpenBehavior(rawValue: terminalOpenBehaviorRaw) ?? .newWindow }
        set {
            terminalOpenBehaviorRaw = newValue.rawValue
            UserDefaults.standard.set(newValue.rawValue, forKey: "terminalOpenBehavior")
        }
    }

    /// What the default Open action will really do in the currently preferred terminal.
    var effectiveOpenBehavior: TerminalOpenBehavior {
        preferredTerminal.resolvedOpenBehavior(terminalOpenBehavior)
    }

    var filteredSessions: [SessionSummary] {
        sessions.filter { session in
            (stateFilter == nil || session.state == stateFilter) &&
            (search.isEmpty || [session.name, session.title ?? "", session.commandLabel, session.cwd]
                .contains { $0.localizedCaseInsensitiveContains(search) })
        }
    }

    var runningCount: Int { sessions.filter { $0.state == .running }.count }
    var canCreateSessions: Bool { cliCapabilities?.capabilities.create == true }
    var canAttachSessions: Bool { cliCapabilities?.capabilities.localAttach == true }

    /// Live sessions in the current sidebar selection, in list order.
    var selectedLiveSessionIDs: [String] {
        sessions
            .filter { selection.contains($0.id) && $0.state.isLive }
            .map(\.id)
    }

    var hasSelectedLiveSessions: Bool { !selectedLiveSessionIDs.isEmpty }

    func start() {
        guard pollingTask == nil else { return }
        activationCancellable = NotificationCenter.default
            .publisher(for: NSApplication.didBecomeActiveNotification)
            .sink { [weak self] _ in
                Task { @MainActor in
                    guard self?.cliCapabilities != nil else { return }
                    await self?.refresh()
                }
            }
        pollingTask = Task {
            await discoverCLI(
                present: !LatchClient.preferences.bool(forKey: "cliSetupCompleted")
            )
            guard FileManager.default.isExecutableFile(
                atPath: activeCLIPath
            ) else {
                shouldPresentCLISetup = true
                return
            }
            do {
                let capabilities = try await client.validateCompatibility()
                if cliCapabilities != capabilities {
                    cliCapabilities = capabilities
                }
            } catch {
                if errorMessage != error.localizedDescription {
                    errorMessage = error.localizedDescription
                }
                shouldPresentCLISetup = true
                return
            }
            cliDoctorReport = try? await client.doctor()
            await refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: pollIntervalNanoseconds)
                if !Task.isCancelled { await refresh() }
            }
        }
    }

    func discoverCLI(present: Bool = false) async {
        isDiscoveringCLI = true
        let paths = await Task.detached(priority: .userInitiated) {
            (try? LatchClient.discoverExecutablePaths()) ?? []
        }.value
        discoveredLatchExecutables = paths
        isDiscoveringCLI = false
        if present || (paths.isEmpty && !selectedCLIIsExecutable) {
            shouldPresentCLISetup = true
        }
    }

    func selectCLI(_ path: String) {
        latchExecutablePath = path
        LatchClient.preferences.set(true, forKey: "cliSetupCompleted")
        shouldPresentCLISetup = false
        restartPolling()
    }

    func runCLIInstaller() {
        do { try TerminalLauncher.runInTerminal(cliInstallCommand) }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func useConfiguredCLI() {
        guard selectedCLIIsExecutable else { return }
        LatchClient.preferences.set(true, forKey: "cliSetupCompleted")
        shouldPresentCLISetup = false
        restartPolling()
    }

    private func restartPolling() {
        pollingTask?.cancel()
        pollingTask = nil
        cliCapabilities = nil
        cliDoctorReport = nil
        cliUpdateReport = nil
        syncChrome()
        start()
    }

    func refresh(showsProgress: Bool = false) async {
        if refreshInFlight {
            refreshRequested = true
            return
        }
        refreshInFlight = true
        if showsProgress {
            isRefreshing = true
        }
        defer {
            refreshInFlight = false
            if isRefreshing {
                isRefreshing = false
            }
        }
        repeat {
            refreshRequested = false
            do {
                let report = try await client.list()
                applySessionList(report.sessions)
                consecutiveRefreshFailures = 0
                if errorMessage != nil {
                    errorMessage = nil
                }
                await loadDetails()
                for companion in companionRefreshers {
                    await companion()
                }
            } catch {
                // Preserve the last good snapshot; only the diagnostic changes.
                reportRefreshFailure(error)
            }
        } while refreshRequested
    }

    func loadDetails() async {
        guard selection.count == 1, let id = selection.first else {
            if details != nil {
                details = nil
            }
            return
        }
        do {
            let report = try await client.inspect(id)
            if details != report {
                details = report
            }
            consecutiveRefreshFailures = 0
        } catch {
            reportRefreshFailure(error)
        }
    }

    /// Surfaces a polling failure only once it has repeated, so the banner
    /// describes a condition the user can still act on when they read it.
    private func reportRefreshFailure(_ error: Error) {
        consecutiveRefreshFailures += 1
        guard consecutiveRefreshFailures >= Self.refreshFailuresBeforeReporting else { return }
        if errorMessage != error.localizedDescription {
            errorMessage = error.localizedDescription
        }
    }

    func create(_ request: NewSessionRequest, openAfterCreation: Bool) async {
        do {
            let report = try await client.create(request)
            selection = [report.session.id]
            await refresh()
            if openAfterCreation { await open(report.session.id) }
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func open(_ id: String, behavior: TerminalOpenBehavior? = nil) async {
        do {
            let command = await client.attachmentCommand(for: id)
            try TerminalLauncher.open(
                command: command,
                in: preferredTerminal,
                behavior: behavior ?? terminalOpenBehavior,
                customExecutable: customTerminalExecutable,
                customTemplate: customTerminalTemplate,
                openInBackground: terminalOpenInBackground
            )
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func stop(_ id: String, force: Bool) async {
        await stopSessions([id], force: force)
    }

    func stopSessions(_ ids: [String], force: Bool) async {
        guard !ids.isEmpty else { return }
        do {
            for id in ids {
                _ = try await client.stop(id, force: force)
            }
            await refresh()
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func stopAll() async {
        do { _ = try await client.stopAll(); await refresh() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func rename(_ id: String, to name: String) async {
        do { _ = try await client.rename(id, to: name); await refresh() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func resize(_ id: String, request: ResizeSessionRequest) async {
        do { _ = try await client.resize(id, request: request); await refresh() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func remove(_ id: String, force: Bool) async {
        do { _ = try await client.remove(id, force: force); await refresh() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func previewPrune() async {
        do { prunePreview = try await client.previewPrune() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func pruneAll() async {
        do { _ = try await client.pruneAll(); prunePreview = nil; await refresh() }
        catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func refreshCLIDiagnostics() async {
        do {
            let capabilities = try await client.validateCompatibility()
            if cliCapabilities != capabilities {
                cliCapabilities = capabilities
            }
            cliDoctorReport = try await client.doctor()
            if errorMessage != nil {
                errorMessage = nil
            }
            syncChrome()
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func checkForCLIUpdate() async {
        guard !isCheckingCLIUpdate, !isUpdatingCLI else { return }
        isCheckingCLIUpdate = true
        defer { isCheckingCLIUpdate = false }
        do {
            cliUpdateReport = try await client.checkForUpdate()
            if errorMessage != nil {
                errorMessage = nil
            }
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    func updateCLI() async {
        guard cliCapabilities?.capabilities.selfUpdate == true,
              !isCheckingCLIUpdate,
              !isUpdatingCLI else { return }
        isUpdatingCLI = true
        defer { isUpdatingCLI = false }
        do {
            cliUpdateReport = try await client.update()
            let capabilities = try await client.validateCompatibility()
            if cliCapabilities != capabilities {
                cliCapabilities = capabilities
            }
            cliDoctorReport = try await client.doctor()
            if errorMessage != nil {
                errorMessage = nil
            }
            syncChrome()
            await refresh()
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    private var pollIntervalNanoseconds: UInt64 {
        shouldPollFrequently ? Self.activePollNanoseconds : Self.idlePollNanoseconds
    }

    /// Keep a tight loop only while Latch is in front with something on screen.
    /// Closing the window (or deactivating) backs the poll off; becoming active
    /// already triggers an immediate refresh via `didBecomeActiveNotification`.
    private var shouldPollFrequently: Bool {
        guard NSApp.isActive else { return false }
        return NSApp.windows.contains { $0.isVisible && !$0.isMiniaturized }
    }

    private func applySessionList(_ next: [SessionSummary]) {
        let sessionsChanged = !sessions.elementsEqual(next, by: { $0.isDisplayEqual(to: $1) })
        if sessionsChanged {
            sessions = next
        }

        let validIDs = Set(next.map(\.id))
        var nextSelection = selection.intersection(validIDs)
        if nextSelection.isEmpty {
            nextSelection = Set(next.prefix(1).map(\.id))
        }
        if nextSelection != selection {
            selection = nextSelection
        }

        if sessionsChanged {
            syncChrome()
        }
    }

    private func syncChrome() {
        chrome?.update(runningCount: runningCount, canCreateSessions: canCreateSessions)
    }
}
