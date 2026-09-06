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

public enum FolderBrowserMode: Equatable, Sendable {
    case create
    case chooseDefault
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
    public typealias Creating = (UUID, String) async throws -> CreateReport
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

    private let initialPath: String?
    private let folderStore: any NewSessionFolderStoring
    private let browse: Browsing
    private let create: Creating
    private let hasAccess: AccessChecking
    private let didCreate: Created

    public init(
        mode: FolderBrowserMode,
        initialPath: String?,
        folderStore: any NewSessionFolderStoring,
        browse: @escaping Browsing,
        create: @escaping Creating,
        hasAccess: @escaping AccessChecking = { true },
        didCreate: @escaping Created = { _ in }
    ) {
        self.mode = mode
        self.initialPath = initialPath
        self.folderStore = folderStore
        self.browse = browse
        self.create = create
        self.hasAccess = hasAccess
        self.didCreate = didCreate
    }

    public func load() async {
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

    public func navigateToParent() async {
        guard let parent = currentPage?.parent else { return }
        await navigate(to: parent)
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
        guard mode == .chooseDefault, hasAccess(), let path = currentPage?.path else {
            if !hasAccess() { error = NewSessionAccessError.unavailable.message }
            return false
        }
        folderStore.save(path)
        return true
    }

    public func startSession() async {
        guard mode == .create, let cwd = currentPage?.path else { return }
        guard hasAccess() else {
            error = NewSessionAccessError.unavailable.message
            return
        }
        let requestID = pendingCreationRequestID ?? UUID()
        pendingCreationRequestID = requestID
        isLoading = true
        defer { isLoading = false }
        do {
            let report = try await create(requestID, cwd)
            createdSessionID = report.session.id
            pendingCreationRequestID = nil
            error = nil
            await didCreate(report.session.id)
        } catch {
            self.error = Self.message(for: error)
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
