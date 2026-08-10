import Foundation
import Combine
import AppKit

@MainActor
final class SessionStore: ObservableObject {
    @Published private(set) var sessions: [SessionSummary] = []
    @Published private(set) var details: InspectReport?
    @Published var selection: String?
    @Published var search = ""
    @Published var stateFilter: SessionState?
    @Published var isRefreshing = false
    @Published var errorMessage: String?
    @Published var prunePreview: PruneReport?
    @Published var shouldPresentNewSession = false
    @Published var shouldPresentPrune = false
    @Published var customTerminalExecutable: String {
        didSet { UserDefaults.standard.set(customTerminalExecutable, forKey: "customTerminalExecutable") }
    }
    @Published var customTerminalTemplate: String {
        didSet { UserDefaults.standard.set(customTerminalTemplate, forKey: "customTerminalTemplate") }
    }
    @Published var latchExecutablePath: String {
        didSet {
            if latchExecutablePath.isEmpty {
                LatchClient.preferences.removeObject(forKey: "latchExecutablePath")
            } else {
                LatchClient.preferences.set(latchExecutablePath, forKey: "latchExecutablePath")
            }
            client = LatchClient()
        }
    }

    @Published private var preferredTerminalRaw: String

    private var client: LatchClient
    private var refreshRequested = false
    private var pollingTask: Task<Void, Never>?
    private var activationCancellable: AnyCancellable?

    init(client: LatchClient = LatchClient()) {
        self.client = client
        preferredTerminalRaw = UserDefaults.standard.string(forKey: "preferredTerminal")
            ?? PreferredTerminal.terminal.rawValue
        customTerminalExecutable = UserDefaults.standard.string(forKey: "customTerminalExecutable") ?? ""
        customTerminalTemplate = UserDefaults.standard.string(forKey: "customTerminalTemplate")
            ?? "-e {latch} attach {session}"
        latchExecutablePath = LatchClient.preferences.string(forKey: "latchExecutablePath") ?? ""
    }

    var preferredTerminal: PreferredTerminal {
        get { PreferredTerminal(rawValue: preferredTerminalRaw) ?? .terminal }
        set {
            preferredTerminalRaw = newValue.rawValue
            UserDefaults.standard.set(newValue.rawValue, forKey: "preferredTerminal")
        }
    }

    var filteredSessions: [SessionSummary] {
        sessions.filter { session in
            (stateFilter == nil || session.state == stateFilter) &&
            (search.isEmpty || [session.name, session.title ?? "", session.commandLabel, session.cwd]
                .contains { $0.localizedCaseInsensitiveContains(search) })
        }
    }

    var runningCount: Int { sessions.filter { $0.state == .running }.count }

    func start() {
        guard pollingTask == nil else { return }
        activationCancellable = NotificationCenter.default
            .publisher(for: NSApplication.didBecomeActiveNotification)
            .sink { [weak self] _ in
                Task { @MainActor in await self?.refresh() }
            }
        pollingTask = Task {
            do { _ = try await client.validateCompatibility() }
            catch { errorMessage = error.localizedDescription }
            await refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                if !Task.isCancelled { await refresh() }
            }
        }
    }

    func refresh() async {
        if isRefreshing {
            refreshRequested = true
            return
        }
        isRefreshing = true
        defer { isRefreshing = false }
        repeat {
            refreshRequested = false
            do {
                let report = try await client.list()
                sessions = report.sessions
                errorMessage = nil
                if let selection, !sessions.contains(where: { $0.id == selection }) {
                    self.selection = sessions.first?.id
                } else if selection == nil {
                    selection = sessions.first?.id
                }
                await loadDetails()
            } catch {
                // Preserve the last good snapshot; only the diagnostic changes.
                errorMessage = error.localizedDescription
            }
        } while refreshRequested
    }

    func loadDetails() async {
        guard let selection else {
            details = nil
            return
        }
        do { details = try await client.inspect(selection) }
        catch { errorMessage = error.localizedDescription }
    }

    func create(_ request: NewSessionRequest, openAfterCreation: Bool) async {
        do {
            let report = try await client.create(request)
            selection = report.session.id
            await refresh()
            if openAfterCreation { await open(report.session.id) }
        } catch { errorMessage = error.localizedDescription }
    }

    func open(_ id: String) async {
        do {
            let command = await client.attachmentCommand(for: id)
            try TerminalLauncher.open(
                command: command,
                in: preferredTerminal,
                customExecutable: customTerminalExecutable,
                customTemplate: customTerminalTemplate
            )
        } catch { errorMessage = error.localizedDescription }
    }

    func stop(_ id: String, force: Bool) async {
        do { _ = try await client.stop(id, force: force); await refresh() }
        catch { errorMessage = error.localizedDescription }
    }

    func rename(_ id: String, to name: String) async {
        do { _ = try await client.rename(id, to: name); await refresh() }
        catch { errorMessage = error.localizedDescription }
    }

    func remove(_ id: String, force: Bool) async {
        do { _ = try await client.remove(id, force: force); await refresh() }
        catch { errorMessage = error.localizedDescription }
    }

    func previewPrune() async {
        do { prunePreview = try await client.previewPrune() }
        catch { errorMessage = error.localizedDescription }
    }

    func pruneAll() async {
        do { _ = try await client.pruneAll(); prunePreview = nil; await refresh() }
        catch { errorMessage = error.localizedDescription }
    }
}
