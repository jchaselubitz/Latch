import Foundation
import Combine
import AppKit

@MainActor
final class SessionStore: ObservableObject {
    @Published private(set) var sessions: [SessionSummary] = []
    @Published private(set) var details: InspectReport?
    @Published private(set) var savedChats: [String: SavedChatSummary] = [:]
    @Published private(set) var resumingSessionIDs = Set<String>()
    private var retainedSessionIDs = Set<String>()
    private var resumeRequests: [String: UUID] = [:]
    @Published var selection = Set<String>()
    @Published var search = ""
    @Published var filter: SessionFilter = .all
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
            filter.includes(session) &&
            (search.isEmpty || [session.name, session.title ?? "", session.commandLabel, session.cwd]
                .contains { $0.localizedCaseInsensitiveContains(search) })
        }
    }

    var runningCount: Int { sessions.filter { $0.state == .running }.count }

    func count(for filter: SessionFilter) -> Int {
        sessions.filter(filter.includes).count
    }

    /// Working directories of the newest sessions, most recent first, so a
    /// new session can start where recent work happened.
    func recentDirectories(limit: Int = 4) -> [String] {
        var seen = Set<String>()
        return sessions
            .sorted { $0.createdAt > $1.createdAt }
            .map(\.cwd)
            .filter { seen.insert($0).inserted }
            .prefix(limit)
            .map { $0 }
    }
    var canCreateSessions: Bool { cliCapabilities?.capabilities.create == true }
    var canAttachSessions: Bool { cliCapabilities?.capabilities.localAttach == true }

    /// What a stop confirmation tells the user about the sessions it will end.
    nonisolated static func stopExplanation(count: Int) -> String {
        count == 1
            ? "Stopping ends the child process but retains its final screen for later inspection."
            : "Stopping ends each child process but retains its final screen for later inspection."
    }

    /// Whether a session in `state` can be opened in a terminal right now.
    func canOpen(_ state: SessionState) -> Bool { state.isAttachable && canAttachSessions }

    func isSavedOnly(_ id: String) -> Bool {
        savedChats[id] != nil && !retainedSessionIDs.contains(id)
    }

    func canResume(_ id: String) -> Bool {
        savedChats[id]?.canResume == true && canAttachSessions && !resumingSessionIDs.contains(id)
    }

    func resumeExplanation(_ id: String) -> String {
        if let reason = savedChats[id]?.reason { return reason }
        if cliCapabilities?.capabilities.extensions.contains("saved-chats") != true {
            return "Update the Latch CLI to resume saved chats."
        }
        if savedChats[id] == nil { return "This session has no verified saved conversation." }
        return "Resume the saved conversation and open its terminal."
    }

    /// Live sessions in the current sidebar selection, in list order.
    var selectedLiveSessionIDs: [String] {
        sessions
            .filter { selection.contains($0.id) && $0.state.isLive }
            .map(\.id)
    }

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
                try await refreshCapabilities()
            } catch {
                report(error)
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
            report(error)
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
        savedChats = [:]
        resumeRequests = [:]
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
                let catalog = if cliCapabilities?.capabilities.extensions.contains("saved-chats") == true {
                    try await client.savedChats().chats
                } else {
                    [SavedChatSummary]()
                }
                retainedSessionIDs = Set(report.sessions.map(\.id))
                let nextChats = Dictionary(catalog.map { ($0.sessionId, $0) }, uniquingKeysWith: { first, _ in first })
                if nextChats != savedChats { savedChats = nextChats }
                applySessionList(report.sessions + catalog.filter { isSavedOnly($0.sessionId) }.map(\.session))
                consecutiveRefreshFailures = 0
                clearError()
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
        guard selection.count == 1, let id = selection.first, !isSavedOnly(id) else {
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
        report(error)
    }

    func create(_ request: NewSessionRequest, openAfterCreation: Bool) async {
        await perform {
            let report = try await client.create(request)
            selection = [report.session.id]
            await refresh()
            if openAfterCreation { await open(report.session.id) }
        }
    }

    func resume(_ sessionID: String) async {
        guard canResume(sessionID), let chat = savedChats[sessionID] else { return }
        let operation = resumeRequests[chat.chatId] ?? UUID()
        resumeRequests[chat.chatId] = operation
        resumingSessionIDs.insert(sessionID)
        defer { resumingSessionIDs.remove(sessionID) }
        do {
            let report = try await client.resume(chat.chatId, requestID: operation)
            resumeRequests[chat.chatId] = nil
            await refresh()
            selection = [report.session.id]
            await open(report.session.id)
        } catch let error as LatchClientError {
            // A lost process response can be retried with the same operation.
            if case .commandFailed = error { resumeRequests[chat.chatId] = nil }
            report(error)
        } catch {
            report(error)
        }
    }

    func open(_ id: String, behavior: TerminalOpenBehavior? = nil) async {
        await perform {
            let command = await client.attachmentCommand(for: id)
            try TerminalLauncher.open(
                command: command,
                in: preferredTerminal,
                behavior: behavior ?? terminalOpenBehavior,
                customExecutable: customTerminalExecutable,
                customTemplate: customTerminalTemplate,
                openInBackground: terminalOpenInBackground
            )
        }
    }

    /// Puts the shell command that attaches to a session on the pasteboard.
    func copyAttachCommand(_ id: String) async {
        let command = await client.attachmentCommand(for: id)
            .map(TerminalLauncher.shellQuote)
            .joined(separator: " ")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(command, forType: .string)
    }

    func stop(_ id: String, force: Bool) async {
        await stopSessions([id], force: force)
    }

    func stopSessions(_ ids: [String], force: Bool) async {
        guard !ids.isEmpty else { return }
        await perform {
            for id in ids {
                _ = try await client.stop(id, force: force)
            }
            await refresh()
        }
    }

    func stopAll() async {
        await perform { _ = try await client.stopAll(); await refresh() }
    }

    /// Live session ids "Stop Other Sessions" would end, leaving `id` as-is.
    func otherLiveSessionIDs(keeping id: String) -> [String] {
        SessionSummary.otherLiveIDs(in: sessions, keeping: id)
    }

    func stopOthers(keeping id: String, force: Bool) async {
        await stopSessions(otherLiveSessionIDs(keeping: id), force: force)
    }

    func rename(_ id: String, to name: String) async {
        await perform { _ = try await client.rename(id, to: name); await refresh() }
    }

    func resize(_ id: String, request: ResizeSessionRequest) async {
        await perform { _ = try await client.resize(id, request: request); await refresh() }
    }

    func remove(_ id: String, force: Bool) async {
        await perform { _ = try await client.remove(id, force: force); await refresh() }
    }

    func previewPrune() async {
        await perform { prunePreview = try await client.previewPrune() }
    }

    func pruneAll() async {
        await perform { _ = try await client.pruneAll(); prunePreview = nil; await refresh() }
    }

    /// Runs a user-initiated session action, surfacing any failure in the
    /// error banner. Each action refreshes the list itself when it succeeds.
    private func perform(_ operation: () async throws -> Void) async {
        do { try await operation() } catch { report(error) }
    }

    /// Shows `error` in the banner, skipping the publish when it is already shown.
    private func report(_ error: Error) {
        if errorMessage != error.localizedDescription {
            errorMessage = error.localizedDescription
        }
    }

    /// Re-validates the selected CLI, publishing only when its report changed.
    private func refreshCapabilities() async throws {
        let capabilities = try await client.validateCompatibility()
        if cliCapabilities != capabilities {
            cliCapabilities = capabilities
        }
    }

    /// Clears a shown error without publishing when there is none.
    private func clearError() {
        if errorMessage != nil {
            errorMessage = nil
        }
    }

    func refreshCLIDiagnostics() async {
        do {
            try await refreshCapabilities()
            cliDoctorReport = try await client.doctor()
            clearError()
            syncChrome()
        } catch {
            report(error)
        }
    }

    func checkForCLIUpdate() async {
        guard !isCheckingCLIUpdate, !isUpdatingCLI else { return }
        isCheckingCLIUpdate = true
        defer { isCheckingCLIUpdate = false }
        do {
            cliUpdateReport = try await client.checkForUpdate()
            clearError()
        } catch {
            report(error)
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
            try await refreshCapabilities()
            cliDoctorReport = try await client.doctor()
            clearError()
            syncChrome()
            await refresh()
        } catch {
            report(error)
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
