import Foundation
import Observation

/// The saved canonical folder used to open a new-session browser. `nil` means
/// no preference and deliberately asks the Mac for its user's home directory.
public protocol NewSessionFolderStoring: Sendable {
    func load() -> String?
    func save(_ path: String?)
}

public struct UserDefaultsNewSessionFolderStore: NewSessionFolderStoring {
    private nonisolated(unsafe) let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "newSessionFolder") {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> String? {
        guard let path = defaults.string(forKey: key), !path.isEmpty else { return nil }
        return path
    }

    public func save(_ path: String?) {
        if let path, !path.isEmpty {
            defaults.set(path, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}

public final class MemoryNewSessionFolderStore: NewSessionFolderStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var path: String?

    public init(_ path: String? = nil) { self.path = path }

    public func load() -> String? { lock.withLock { path } }
    public func save(_ path: String?) { lock.withLock { self.path = path } }
}

/// The model last chosen for each agent, so the next new session offers it
/// first. `nil` means the agent's own default on the Mac.
public protocol AgentModelPreferenceStoring: Sendable {
    func load(agent: SessionAgent) -> String?
    func save(_ model: String?, agent: SessionAgent)
}

public struct UserDefaultsAgentModelPreferenceStore: AgentModelPreferenceStoring {
    private nonisolated(unsafe) let defaults: UserDefaults
    private let prefix: String

    public init(defaults: UserDefaults = .standard, prefix: String = "newSessionModel.") {
        self.defaults = defaults
        self.prefix = prefix
    }

    public func load(agent: SessionAgent) -> String? {
        guard let model = defaults.string(forKey: prefix + agent.rawValue), !model.isEmpty else {
            return nil
        }
        return model
    }

    public func save(_ model: String?, agent: SessionAgent) {
        if let model, !model.isEmpty {
            defaults.set(model, forKey: prefix + agent.rawValue)
        } else {
            defaults.removeObject(forKey: prefix + agent.rawValue)
        }
    }
}

public final class MemoryAgentModelPreferenceStore: AgentModelPreferenceStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var models: [SessionAgent: String] = [:]

    public init(_ models: [SessionAgent: String] = [:]) { self.models = models }

    public func load(agent: SessionAgent) -> String? { lock.withLock { models[agent] } }
    public func save(_ model: String?, agent: SessionAgent) {
        lock.withLock { models[agent] = model }
    }
}

public enum FolderBrowserMode: Equatable, Hashable, Sendable, Identifiable {
    /// Start a standard shell in the chosen folder.
    case create
    /// Start the named agent directly in the chosen folder.
    case createAgent(SessionAgent)
    case chooseDefault

    /// Whether choosing a folder starts something on the Mac.
    public var isCreate: Bool {
        switch self {
        case .create, .createAgent: return true
        case .chooseDefault: return false
        }
    }

    /// The agent a creation launches, or nil for a shell or a selection.
    public var agent: SessionAgent? {
        if case .createAgent(let agent) = self { return agent }
        return nil
    }

    /// Lets the picker be presented by value: a sheet bound to the mode
    /// itself always shows the mode that was tapped, never the one before.
    public var id: String {
        switch self {
        case .create: return "create"
        case .createAgent(let agent): return "create." + agent.rawValue
        case .chooseDefault: return "chooseDefault"
        }
    }
}

public extension SessionAgent {
    /// The product name shown on controls and in explanations.
    var displayName: String {
        switch self {
        case .claude: return "Claude Code"
        case .codex: return "Codex"
        }
    }
}

public enum NewSessionAccessError: Error, Equatable, Sendable {
    case unavailable

    public var message: String {
        "Directory browsing and session creation require a current control grant."
    }
}

/// UI-independent state for the remote folder picker. The model deliberately
/// retains a failed creation's UUID: a retry may be answering a lost response,
/// and only the Mac can determine whether that UUID already created a shell.
@MainActor
@Observable
public final class FolderBrowserModel {
    public typealias Browsing = (String?, String?) async throws -> DirectoryPage
    /// Starts a session: request id, folder, and the chosen model (`nil` for
    /// the agent's default, and always `nil` for a shell).
    public typealias Creating = (UUID, String, String?) async throws -> CreateReport
    public typealias ListingModels = () async throws -> AgentModelCatalog
    public typealias AccessChecking = () -> Bool
    public typealias Created = (String) async -> Void

    public let mode: FolderBrowserMode
    public private(set) var currentPage: DirectoryPage?
    public private(set) var entries: [DirectoryEntry] = []
    public private(set) var navigationStack: [String] = []
    public private(set) var isLoading = false
    public private(set) var isLoadingMore = false
    public private(set) var error: String?
    public private(set) var notice: String?
    public private(set) var pendingCreationRequestID: UUID?
    public private(set) var createdSessionID: String?
    /// What the agent lists on the Mac. `nil` for a shell, a Mac that
    /// predates model choice, or until the list arrives.
    public private(set) var modelCatalog: AgentModelCatalog?
    public private(set) var isLoadingModels = false
    /// Why the list could not be read. Starting still works, on the Mac's
    /// default, so this is said rather than blocking the start.
    public private(set) var modelListError: String?
    /// The model the next start names; `nil` is the agent's default on the Mac.
    public private(set) var selectedModelID: String?

    private let initialPath: String?
    private let folderStore: any NewSessionFolderStoring
    private let browse: Browsing
    private let create: Creating
    private let hasAccess: AccessChecking
    private let didCreate: Created
    private let listModels: ListingModels?
    private let modelStore: (any AgentModelPreferenceStoring)?

    public init(
        mode: FolderBrowserMode,
        initialPath: String?,
        folderStore: any NewSessionFolderStoring,
        browse: @escaping Browsing,
        create: @escaping Creating,
        hasAccess: @escaping AccessChecking = { true },
        didCreate: @escaping Created = { _ in },
        listModels: ListingModels? = nil,
        modelStore: (any AgentModelPreferenceStoring)? = nil
    ) {
        self.mode = mode
        self.initialPath = initialPath
        self.folderStore = folderStore
        self.browse = browse
        self.create = create
        self.hasAccess = hasAccess
        self.didCreate = didCreate
        self.listModels = listModels
        self.modelStore = modelStore
    }

    /// Whether this picker offers a model choice at all.
    public var offersModelChoice: Bool { mode.agent != nil && listModels != nil }

    /// The chosen model's display name, or nil while it is the Mac's default.
    public var selectedModelName: String? {
        guard let selectedModelID else { return nil }
        return modelCatalog?.models.first { $0.id == selectedModelID }?.name ?? selectedModelID
    }

    /// The Mac's own default, named from the list when the list has it.
    public var defaultModelName: String? {
        guard let id = modelCatalog?.defaultModel else { return nil }
        return modelCatalog?.models.first { $0.id == id }?.name ?? id
    }

    public func load() async {
        await loadFolders()
        await loadModels()
    }

    /// Reads the agent's models from the Mac and restores the last choice
    /// for this agent when the Mac still lists it.
    public func loadModels() async {
        guard let agent = mode.agent, let listModels, !isLoadingModels else { return }
        guard hasAccess() else { return }
        isLoadingModels = true
        defer { isLoadingModels = false }
        do {
            let catalog = try await listModels()
            modelCatalog = catalog
            modelListError = nil
            let wanted = selectedModelID ?? modelStore?.load(agent: agent)
            let restored = catalog.models.contains { $0.id == wanted } ? wanted : nil
            if restored != selectedModelID {
                selectedModelID = restored
                pendingCreationRequestID = nil
            }
        } catch {
            modelListError = Self.message(for: error)
        }
    }

    /// Chooses the model the next start names, or `nil` for the Mac's
    /// default, and remembers it for this agent's next session.
    public func selectModel(_ id: String?) {
        guard let agent = mode.agent else { return }
        if let id, modelCatalog?.models.contains(where: { $0.id == id }) != true { return }
        guard id != selectedModelID else { return }
        selectedModelID = id
        modelStore?.save(id, agent: agent)
        // A request ID is bound to one model, as it is to one folder: another
        // model is a new intent, not a retry of a possibly-created session.
        pendingCreationRequestID = nil
    }

    private func loadFolders() async {
        guard currentPage == nil else { return }
        if let initialPath {
            do {
                try await loadPage(path: initialPath, rememberCurrent: false)
                return
            } catch let error as LatchError where Self.isUnavailableSavedFolder(error) {
                notice = "Your saved default folder is unavailable. Starting at your Mac home folder."
            } catch {
                self.error = Self.message(for: error)
                return
            }
        }
        do {
            try await loadPage(path: nil, rememberCurrent: false)
        } catch {
            self.error = Self.message(for: error)
        }
    }

    public func navigate(to path: String) async {
        do {
            try await loadPage(path: path, rememberCurrent: true)
        } catch {
            self.error = Self.message(for: error)
        }
    }

    public func retry() async {
        let path = currentPage?.path ?? initialPath
        do {
            try await loadPage(path: path, rememberCurrent: false)
        } catch {
            self.error = Self.message(for: error)
        }
    }

    public func loadMore() async {
        guard !isLoadingMore, let page = currentPage, let cursor = page.nextCursor else { return }
        guard hasAccess() else {
            error = NewSessionAccessError.unavailable.message
            return
        }
        isLoadingMore = true
        defer { isLoadingMore = false }
        do {
            let next = try await browse(page.path, cursor)
            guard next.path == page.path else {
                throw LatchError.malformedResponse("pagination changed directories")
            }
            entries.append(contentsOf: next.entries)
            currentPage = next
            error = nil
        } catch {
            self.error = Self.message(for: error)
        }
    }

    /// Saves only in selection mode. Browsing elsewhere in create mode is a
    /// one-off choice and never mutates the preference.
    @discardableResult
    public func useCurrentAsDefault() -> Bool {
        guard !mode.isCreate, hasAccess(), let path = currentPage?.path else {
            if !hasAccess() { error = NewSessionAccessError.unavailable.message }
            return false
        }
        folderStore.save(path)
        return true
    }

    public func startSession() async {
        guard mode.isCreate, let cwd = currentPage?.path else { return }
        guard hasAccess() else {
            error = NewSessionAccessError.unavailable.message
            return
        }
        let requestID = pendingCreationRequestID ?? UUID()
        pendingCreationRequestID = requestID
        isLoading = true
        defer { isLoading = false }
        let model = mode.agent == nil ? nil : selectedModelID
        do {
            let report = try await create(requestID, cwd, model)
            createdSessionID = report.session.id
            pendingCreationRequestID = nil
            error = nil
            await didCreate(report.session.id)
        } catch {
            self.error = Self.message(for: error)
            // The Mac may have moved on from the model this list showed. A
            // fresh list drops a model it no longer offers, so the next tap
            // is not refused the same way.
            if model != nil {
                await loadModels()
            }
        }
    }

    /// Cancelling an attempt is the point at which its idempotency key stops
    /// being reusable. A later Start is a new user intent and gets a new UUID.
    public func cancelCreation() {
        pendingCreationRequestID = nil
        error = nil
    }

    private func loadPage(path: String?, rememberCurrent: Bool) async throws {
        guard hasAccess() else { throw NewSessionAccessError.unavailable }
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        let page = try await browse(path, nil)
        if rememberCurrent, let current = currentPage?.path, current != page.path {
            navigationStack.append(current)
            // A request ID is bound to one cwd. Choosing another folder is a
            // new intent, not a retry of the possibly-created old one.
            pendingCreationRequestID = nil
        }
        currentPage = page
        entries = page.entries
        error = nil
    }

    private static func isUnavailableSavedFolder(_ error: LatchError) -> Bool {
        guard case .http(let status, let path, _) = error, path == "/v2/directories" else {
            return false
        }
        return status == 400 || status == 403 || status == 404
    }

    private static func message(for error: Error) -> String {
        if let error = error as? LatchError { return error.message }
        if let error = error as? NewSessionAccessError { return error.message }
        return error.localizedDescription
    }
}
