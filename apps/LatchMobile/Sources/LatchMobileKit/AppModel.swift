import Foundation
import Observation

/// The app's link to one computer, shared by both tabs.
///
/// Discovery lives here rather than in each screen: it is the mandatory first
/// step, its answer decides what the session screen may offer, and repeating it
/// per screen would turn one contract question into several.
@MainActor
@Observable
public final class AppModel {
    public enum LinkSource: Equatable, Sendable {
        case paired
    }

    public enum LinkState: Equatable {
        case unlinked
        case connecting
        case linked(GatewayCapabilities)
        /// The link was up and is recovering on the coordinator's schedule.
        /// Cached results stay on screen, marked stale; nothing new is fetched
        /// until the link is ready again.
        case interrupted(RemoteLinkState, GatewayCapabilities?)
        /// The relay admitted this phone but the Mac is not there: asleep,
        /// offline, or with remote access off. Retrying continues.
        case macOffline(GatewayCapabilities?)
        /// The control plane no longer recognises this pairing. No retry.
        case revoked(String)
        /// The Mac refused this phone's identity. Re-pair. No retry.
        case pairingRequired(String)
        /// The computer answered, and this build cannot speak to it. Kept
        /// separate from `failed` because it is not a connection problem and
        /// must not be reported as one: the remedy is an update, on a named
        /// side, and the saved link stays valid.
        case incompatible(ProtocolMismatch)
        case failed(String)

        /// Capabilities last discovered on this link, if any survive.
        public var cachedCapabilities: GatewayCapabilities? {
            switch self {
            case .linked(let capabilities): return capabilities
            case .interrupted(_, let capabilities), .macOffline(let capabilities): return capabilities
            default: return nil
            }
        }

        /// Whether the gateway is usable right now.
        public var isUsable: Bool {
            if case .linked = self { return true }
            return false
        }
    }

    public private(set) var linkState: LinkState = .unlinked
    /// The link owner's latest immutable snapshot, for Settings.
    public private(set) var linkSnapshot = RemoteLinkSnapshot()
    /// The session list is from before the current interruption.
    public private(set) var sessionsStale = false
    /// Content-free stage timings from the most recent link and requests,
    /// newest last, bounded.
    public private(set) var recentStages: [LinkStageSample] = []
    /// Whether this phone may keep retrying on its own. False in terminal
    /// states so the UI can say what to do instead of showing a spinner.
    public var canRetryAutomatically: Bool {
        switch linkState {
        case .revoked, .pairingRequired, .incompatible: return false
        default: return true
        }
    }
    public private(set) var link: GatewayLink?
    public private(set) var gateway: LatchGateway?
    public private(set) var linkSource: LinkSource?
    public private(set) var sessions: [SessionSummary] = []
    /// The network path the paired route is running over, once one is open.
    public private(set) var remotePath: RemotePath?
    /// How this phone's paired connections have resolved, across launches.
    /// Read during a field run; it leaves the device only if a person reads it
    /// off the screen.
    public private(set) var remotePathTally = RemotePathTally()
    public private(set) var sessionsError: String?
    public private(set) var isLoadingSessions = false
    /// The session most recently created from the folder browser. The view may
    /// highlight it without treating creation as permission to open or attach.
    public private(set) var highlightedSessionID: String?

    /// Sessions this phone has asked the Mac to stop and has not yet heard
    /// back about. A stop can take several seconds — the Mac waits out its own
    /// grace period before escalating — so the rows have to be able to say
    /// that the request is in flight rather than look ignored.
    public private(set) var stoppingSessionIDs: Set<String> = []
    /// Session stores are retained here rather than by a navigation view, so a
    /// pushed chat can reconnect from its cached revision instead of replaying
    /// the conversation after every back-navigation.
    private var conversationStores: [String: ConversationStore] = [:]
    /// Terminal connections are retained here for the same reason, plus one
    /// more: while attached, the phone holds the session's only surface, so
    /// something outside the pushed screen must be able to release it.
    private var terminalSessions: [String: TerminalSession] = [:]

    /// The screen a tap on a session lands on, when the session offers a
    /// choice. Read from the store at init and written back on change, so a
    /// person who set it once is not asked again.
    public var sessionPresentation: SessionPresentation {
        didSet {
            guard sessionPresentation != oldValue else { return }
            presentationStore.save(sessionPresentation)
        }
    }

    /// The grid the phone attaches a terminal at.
    public var terminalSize: TerminalSize {
        didSet {
            guard terminalSize != oldValue else { return }
            terminalSizeStore.save(terminalSize)
        }
    }

    /// Opens one terminal connection for a session at a declared grid.
    ///
    /// A seam for tests, in the same shape as `sessionFactory`: the lifecycle
    /// rules — backgrounding detaches, foregrounding does not reattach — are
    /// properties of this model, and asserting them should not require a
    /// WebSocket listener.
    public typealias TerminalConnecting =
        @Sendable (String, Int, Int) async throws -> any TerminalSocketConnection

    private let terminalConnector: TerminalConnecting?
    /// The device-owner check standing in front of the terminal. Chat has no
    /// equivalent and deliberately so: a phone that may read and reply is
    /// doing what it was paired for, while a terminal runs commands on the
    /// Mac and takes the session's one surface.
    private let terminalUnlock: TerminalUnlock
    private let presentationStore: any SessionPresentationStoring
    private let terminalSizeStore: any TerminalSizeStoring
    private let newSessionFolderStore: any NewSessionFolderStoring
    /// Where the transport writes the path it selected, so Settings can say
    /// whether this session is on the local network, direct, or relayed.
    private let pathReporter: RemotePathReporter
    /// Builds the gateway client over the capability-protected loopback
    /// adapter. One per link generation; the adapter is stopped on suspend.
    public typealias GatewayFactory = @Sendable (
        PairedDeviceRecord, any AuthenticatedGatewayChannelProvider, LinkStageRecorder?
    ) async throws -> LatchGateway
    private let gatewayFactory: GatewayFactory
    /// The one link owner. Screens never open connections.
    public let coordinator: RemoteLinkCoordinator
    private var coordinatorObservation: Task<Void, Never>?
    private var pairedDevice: PairedDeviceRecord?
    /// The loopback adapter behind the current gateway, stopped on suspend so
    /// its capability dies with it.
    private var transport: (any GatewayTransport)?
    /// Which link generation discovery last ran for. Discovery runs once per
    /// new authenticated link, never per screen.
    private var discoveredGeneration = 0
    /// The gateway instance discovery last saw, so a restarted gateway is
    /// noticed and its per-instance state (conversation sockets) re-based.
    private var gatewayInstanceID: String?
    /// Invalidates an in-flight connect when its process-local route is torn
    /// down. A backgrounded factory must never resurrect a connection.
    private var pairedConnectionGeneration = 0
    private var diagnosticsOptions = RemoteLinkConnectOptions()
    /// The owner's suspension in flight. Suspension is requested from a
    /// synchronous scene-phase handler, so it runs as a task; every resume
    /// waits for it first. Without that a fast background/foreground (or a
    /// diagnostics cycle) could resume the old supervisor and then have the
    /// late suspension tear the fresh link down with nobody left to retry.
    private var pendingSuspension: Task<Void, Never>?
    /// Armed only by the USB harness's launch argument; writes one
    /// `cold_open` record for this process and then goes quiet.
    public let coldOpen: ColdOpenRecorder

    public init(
        linkConnector: (any RemoteLinkConnecting)? = nil,
        gatewayFactory: GatewayFactory? = nil,
        pairedGatewayFactory: (@Sendable (PairedDeviceRecord) async throws -> LatchGateway)? = nil,
        pathReporter: RemotePathReporter = RemotePathReporter(),
        presentationStore: any SessionPresentationStoring = UserDefaultsSessionPresentationStore(),
        terminalSizeStore: any TerminalSizeStoring = UserDefaultsTerminalSizeStore(),
        newSessionFolderStore: any NewSessionFolderStoring = UserDefaultsNewSessionFolderStore(),
        terminalConnector: TerminalConnecting? = nil,
        terminalUnlock: TerminalUnlock? = nil,
        coldOpen: ColdOpenRecorder? = nil
    ) {
        self.coldOpen = coldOpen ?? ColdOpenRecorder()
        self.pathReporter = pathReporter
        self.terminalUnlock = terminalUnlock ?? TerminalUnlock()
        self.terminalConnector = terminalConnector
        self.presentationStore = presentationStore
        self.terminalSizeStore = terminalSizeStore
        self.newSessionFolderStore = newSessionFolderStore
        self.defaultNewSessionFolder = newSessionFolderStore.load()
        self.sessionPresentation = presentationStore.load()
        self.terminalSize = terminalSizeStore.load()
        // The native application injects the Rust Remote Link connector. The
        // kit cannot silently fall back to a second transport stack; tests
        // inject a fake connector and a stubbed gateway instead.
        let connector: any RemoteLinkConnecting = linkConnector
            ?? (pairedGatewayFactory != nil ? AlwaysReadyLinkConnector() : UnavailableLinkConnector())
        self.coordinator = RemoteLinkCoordinator(connector: connector)
        if let gatewayFactory {
            self.gatewayFactory = gatewayFactory
        } else if let pairedGatewayFactory {
            self.gatewayFactory = { record, _, _ in try await pairedGatewayFactory(record) }
        } else {
            self.gatewayFactory = { record, provider, recorder in
                LatchGateway(transport: try await RemoteLinkGatewayTransport.start(
                    authenticatedProvider: provider, pairedDevice: record, recorder: recorder
                ))
            }
        }
        pathReporter.observe { [weak self] path in
            Task { @MainActor in self?.remotePath = path }
        }
        pathReporter.observeTally { [weak self] tally in
            Task { @MainActor in self?.remotePathTally = tally }
        }
    }

    /// What discovery permits on the session screen.
    public var surface: SessionSurface {
        guard case .linked(let capabilities) = linkState else {
            return SessionSurface(chat: false, composer: false, interactionControls: false)
        }
        return GatewayCompatibility.sessionSurface(for: capabilities)
            .restricted(to: pairedDevice?.permission)
    }

    /// Where a tap on this session row goes.
    public func route(for session: SessionSummary) -> SessionRoute {
        SessionRoute.route(
            preference: sessionPresentation,
            connector: session.connector,
            surface: surface,
            isRunning: session.isRunning
        )
    }

    /// The gateway's product version, for Settings.
    public var productVersion: String? {
        guard case .linked(let capabilities) = linkState else { return nil }
        return capabilities.productVersion.isEmpty ? nil : capabilities.productVersion
    }

    /// Directory data and process creation are control-granted operations.
    private var hasNewSessionControlGrant: Bool {
        pairedDevice?.permission.permits(.control) == true
    }

    public var canBrowseNewSessionFolders: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return hasNewSessionControlGrant
            && GatewayCompatibility.supports(endpoint: .browseDirectories, capabilities: capabilities)
    }

    public var canCreateNewSession: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return canBrowseNewSessionFolders
            && GatewayCompatibility.supports(endpoint: .createSession, capabilities: capabilities)
    }

    /// Whether the linked Mac serves both new-session routes, independent of
    /// this phone's grant. The control is shown but disabled when the Mac can
    /// serve the flow and this phone may not use it, so the reason can be
    /// said rather than left as a missing button.
    public var advertisesNewSessionCreation: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: .browseDirectories, capabilities: capabilities)
            && GatewayCompatibility.supports(endpoint: .createSession, capabilities: capabilities)
    }

    /// Why the advertised new-session flow cannot be used right now, or nil
    /// when it can. Only a grant can hold it back once the routes exist.
    public var newSessionUnavailableExplanation: String? {
        guard advertisesNewSessionCreation, !canCreateNewSession else { return nil }
        return """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """
    }

    /// The saved default folder, observed so Settings updates the moment one
    /// is chosen. The store remains the source of truth; this mirrors it.
    public private(set) var defaultNewSessionFolder: String?

    /// Re-reads the store after the browser saved a selection.
    public func reloadDefaultNewSessionFolder() {
        defaultNewSessionFolder = newSessionFolderStore.load()
    }

    /// Forgets the saved default so the next picker opens at the Mac's home
    /// directory.
    public func clearDefaultNewSessionFolder() {
        newSessionFolderStore.save(nil)
        defaultNewSessionFolder = nil
    }

    /// Clears the path counters.
    ///
    /// Unlinking deliberately does not: the counters describe this phone's
    /// networks, not its relationship with one Mac, and a field run that
    /// re-pairs between scenarios should not silently lose its own evidence.
    public func resetRemotePathTally() {
        pathReporter.resetTally()
    }

    /// Forgets the computer and everything fetched from it.
    public func unlink() {
        coordinatorObservation?.cancel()
        coordinatorObservation = nil
        Task { [coordinator] in await coordinator.stop() }
        transport?.stop()
        transport = nil
        link = nil
        gateway = nil
        linkSource = nil
        pairedDevice = nil
        pairedConnectionGeneration &+= 1
        discoveredGeneration = 0
        gatewayInstanceID = nil
        sessions = []
        sessionsStale = false
        sessionsError = nil
        highlightedSessionID = nil
        stoppingSessionIDs = []
        conversationStores.values.forEach { $0.stop() }
        conversationStores = [:]
        detachAllTerminals()
        pathReporter.clear()
        linkState = .unlinked
    }

    /// Returns the one persistent conversation store for this session. This
    /// consumes discovery already performed during link setup; it never makes
    /// a separate interaction-capabilities preflight.
    public func conversationStore(for session: SessionSummary) -> ConversationStore? {
        guard let gateway,
              case .linked(let capabilities) = linkState,
              GatewayCompatibility.supports(endpoint: .conversation, capabilities: capabilities)
        else { return nil }
        if let existing = conversationStores[session.id] { return existing }
        let store = ConversationStore(
            sessionID: session.id,
            gateway: gateway,
            operationRetentionSeconds: capabilities.operationRetentionSeconds
        )
        conversationStores[session.id] = store
        return store
    }

    /// Whether the terminal is open to this device right now: it holds the
    /// Mac's grant *and* has passed the owner check recently enough.
    public var isTerminalUnlocked: Bool { surface.terminal && terminalUnlock.isUnlocked }

    /// Why the last owner check did not open the terminal, when there is
    /// something worth saying. A cancelled prompt leaves this nil.
    public var terminalUnlockFailure: String? { terminalUnlock.failure }

    /// Why the shared owner check could not authorize a protected action.
    public var ownerAuthenticationFailure: String? { terminalUnlock.failure }

    /// Asks the device owner to confirm before a terminal is opened.
    ///
    /// Called by the terminal screen ahead of `terminalSession(for:)`. Inside
    /// the grace window it answers without prompting, so attaching, reading
    /// something else, and reattaching is one Face ID check rather than three.
    @discardableResult
    public func unlockTerminal() async -> Bool {
        guard surface.terminal else { return false }
        return await unlockRemoteAccess(
            reason: "Open a terminal on your Mac and run commands on it."
        )
    }

    /// One owner-authentication grace window covers terminal access, revealing
    /// remote folder names, and creating a process. Capability and grant checks
    /// still run independently for every operation.
    @discardableResult
    public func unlockRemoteAccess(reason: String) async -> Bool {
        await terminalUnlock.unlock(reason: reason)
    }

    /// Authenticates before constructing the browser so no directory state is
    /// fetched or exposed to someone who has not passed the owner check.
    public func newSessionFolderBrowser(
        mode: FolderBrowserMode
    ) async -> FolderBrowserModel? {
        let available = mode == .create ? canCreateNewSession : canBrowseNewSessionFolders
        guard available, let gateway else { return nil }
        let reason = mode == .create
            ? "Browse folders and start a new session on your Mac."
            : "Browse folders on your Mac and choose a default."
        guard await unlockRemoteAccess(reason: reason) else { return nil }

        let browser = FolderBrowserModel(
            mode: mode,
            initialPath: newSessionFolderStore.load(),
            folderStore: newSessionFolderStore,
            browse: { path, cursor in
                guard self.canBrowseNewSessionFolders else {
                    throw NewSessionAccessError.unavailable
                }
                return try await gateway.browseDirectories(path: path, cursor: cursor)
            },
            create: { requestID, cwd in
                guard self.canCreateNewSession else {
                    throw NewSessionAccessError.unavailable
                }
                return try await gateway.createSession(requestID: requestID, cwd: cwd)
            },
            hasAccess: {
                mode == .create ? self.canCreateNewSession : self.canBrowseNewSessionFolders
            },
            didCreate: { sessionID in
                await self.refreshSessions()
                self.highlightedSessionID = sessionID
            }
        )
        await browser.load()
        return browser
    }

    public func clearNewSessionHighlight() {
        highlightedSessionID = nil
    }

    /// Whether the linked Mac serves the stop route at all, independent of
    /// this phone's grant. A Mac that predates the route advertises nothing,
    /// and there is then no control to show and nothing to explain.
    public var advertisesSessionStop: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: .stopSession, capabilities: capabilities)
    }

    /// Whether this phone may stop a session right now. Ending what is running
    /// is a control operation, held to the same grant as the terminal.
    public var canStopSessions: Bool {
        advertisesSessionStop && pairedDevice?.permission.permits(.control) == true
    }

    /// Why the advertised stop control cannot be used, or nil when it can.
    /// Only a grant can hold it back once the route exists.
    public var sessionStopUnavailableExplanation: String? {
        guard advertisesSessionStop, !canStopSessions else { return nil }
        return """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """
    }

    /// Asks the Mac to stop one session, then re-reads the list.
    ///
    /// Stopping is not removing: the Mac keeps the session's record and its
    /// dead pane, so the row stays and turns `exited` rather than vanishing.
    /// Any terminal this phone holds for the session is dropped first — the
    /// surface is about to end underneath it, and a connection kept past that
    /// only produces a socket close nobody is watching.
    ///
    /// The request is safe to repeat, so a lost response costs a retry and
    /// nothing else. Returns whether the Mac confirmed the stop.
    @discardableResult
    public func stopSession(_ session: SessionSummary) async -> Bool {
        guard let gateway, canStopSessions else {
            // Reached when the grant or the link changed between the row
            // offering Stop and the confirmation coming back, so the reason
            // has to name which of the two it was.
            sessionsError = sessionStopUnavailableExplanation
                ?? (linkState.isUsable
                    ? "This Mac cannot stop sessions from a phone. Update Latch on the Mac."
                    : "This phone is not connected to your Mac right now.")
            return false
        }
        // A second tap on a row already waiting is not a second stop.
        guard stoppingSessionIDs.insert(session.id).inserted else { return false }
        defer { stoppingSessionIDs.remove(session.id) }
        discardTerminal(for: session)
        do {
            _ = try await gateway.stopSession(sessionID: session.id)
            sessionsError = nil
            await refreshSessions()
            return true
        } catch {
            let failure = (error as? LatchError)?.message ?? error.localizedDescription
            // The stop may still have landed before the answer was lost, so
            // the list is re-read either way rather than left showing a stale
            // row. The reason the stop failed is restored afterwards: a clean
            // refresh must not quietly clear the only account of it.
            await refreshSessions()
            sessionsError = failure
            return false
        }
    }

    /// Returns the one terminal connection for this session, or nil when this
    /// device may not open one — gated on `surface.terminal`, the way
    /// `conversationStore(for:)` is gated on the conversation endpoint, and on
    /// a current owner check, which `unlockTerminal()` is what obtains.
    public func terminalSession(for session: SessionSummary) -> TerminalSession? {
        guard let gateway, surface.terminal, terminalUnlock.isUnlocked else { return nil }
        if let existing = terminalSessions[session.id] { return existing }
        let id = session.id
        let connector = terminalConnector
        let created = TerminalSession(sessionID: id) { [weak self] cols, rows, resume in
            if let connector {
                return try await connector(id, cols, rows)
            }
            // The gateway of the moment, not the one captured at creation: a
            // resume after transport loss goes through the replacement link.
            guard let current = await self?.gateway else {
                throw LatchError.transport("Not linked to a computer.")
            }
            return try await current.openTerminal(sessionID: id, cols: cols, rows: rows, resume: resume)
        }
        terminalSessions[id] = created
        return created
    }

    /// After a link comes back: terminals whose attach was interrupted and
    /// whose bounded resume capability is still valid are resumed; the
    /// gateway refuses if anyone else has attached since. Every other
    /// interrupted terminal stays put with a Reconnect button. Nothing typed
    /// is replayed.
    @discardableResult
    public func resumeInterruptedTerminals() -> Int {
        var resumed = 0
        for terminal in terminalSessions.values where terminal.canResume {
            if terminal.resume() { resumed += 1 }
        }
        return resumed
    }

    /// Transport loss under held terminals: they become interrupted rather
    /// than silently closed, and the person is told input may be missing.
    private func interruptTerminals() {
        for terminal in terminalSessions.values where terminal.holdsSurface {
            terminal.interrupt()
        }
    }

    /// Reads the pane without attaching, so nothing is taken from the Mac.
    ///
    /// This is the first thing the terminal screen does, and it is deliberately
    /// not gated on `surface.terminal`: the route needs only `observe`, so a
    /// phone that may never attach can still see what it cannot type at.
    public func previewSession(
        for session: SessionSummary,
        scrollbackLines: Int = 0
    ) async throws -> SessionPreview {
        guard let gateway else { throw LatchError.transport("Not linked to a computer.") }
        return try await gateway.previewSession(
            sessionID: session.id,
            scrollbackLines: scrollbackLines
        )
    }

    /// How long a held terminal survives with no input once the app stops
    /// being the thing on screen.
    ///
    /// Backgrounding proper releases the surface at once — a phone in a pocket
    /// is not using a terminal. This covers the other case: an app that is on
    /// screen but not frontmost, which is what a pulled-down notification
    /// centre, an incoming call banner, the app switcher, and the Face ID
    /// prompt itself all produce. Tearing the terminal down for those would
    /// make the phone unusable; holding it forever would leave the Mac's one
    /// surface parked on a phone nobody is looking at.
    public nonisolated static let terminalIdleTimeout: TimeInterval = 2 * 60

    /// How often the countdown checks. It bounds how late a release can be, so
    /// the surface comes back within a quarter-minute of the deadline rather
    /// than only when the app is next touched.
    private nonisolated static let terminalIdleTick: Duration = .seconds(15)

    private var terminalIdleWatch: Task<Void, Never>?

    /// Starts releasing idle terminals while the app is not frontmost.
    ///
    /// Idempotent: a scene phase that flickers does not restart the clock,
    /// because the clock is `lastInputAt` on each session rather than a
    /// countdown this task owns.
    public func beginTerminalIdleCountdown(
        timeout: TimeInterval = AppModel.terminalIdleTimeout
    ) {
        guard terminalIdleWatch == nil else { return }
        terminalIdleWatch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: AppModel.terminalIdleTick)
                guard !Task.isCancelled, let self else { return }
                self.releaseIdleTerminals(timeout: timeout)
            }
        }
    }

    /// Stops the countdown. Called when the app comes back to the front, and
    /// when backgrounding releases every surface outright.
    public func cancelTerminalIdleCountdown() {
        terminalIdleWatch?.cancel()
        terminalIdleWatch = nil
    }

    /// Releases every held surface that has had no input for `timeout`.
    ///
    /// The owner check goes with it. A terminal that was given up because
    /// nobody was typing at it should be reopened deliberately, and the grace
    /// window outlasts this timeout otherwise.
    @discardableResult
    public func releaseIdleTerminals(
        timeout: TimeInterval = AppModel.terminalIdleTimeout,
        now: Date = Date()
    ) -> Int {
        var released = 0
        for session in terminalSessions.values where session.holdsSurface {
            guard now.timeIntervalSince(session.lastInputAt) >= timeout else { continue }
            session.detach()
            released += 1
        }
        if released > 0 { terminalUnlock.lock() }
        return released
    }

    /// Releases every held surface before the app is suspended.
    ///
    /// The sessions themselves are kept, so foregrounding returns to
    /// `.closed(.detached)` with a Reattach button rather than silently taking
    /// the surface back from whoever is now using it.
    public func suspendTerminals() {
        cancelTerminalIdleCountdown()
        terminalSessions.values.forEach { $0.detach() }
    }

    /// Releases one session's surface and forgets the connection.
    ///
    /// This is what back-navigation uses rather than `detach()` alone: nothing
    /// displays the connection's state once the screen is gone, and a screen
    /// re-entered later re-reads the pane through the preview anyway. Keeping
    /// it would also leave a second reader on an output stream that only ever
    /// has one.
    public func discardTerminal(for session: SessionSummary) {
        guard let existing = terminalSessions.removeValue(forKey: session.id) else { return }
        existing.detach()
    }

    /// Releases every held surface and forgets the connections.
    ///
    /// This is the teardown path — unlinking, or a link that failed — so the
    /// grace window ends with it. A phone relinked to another Mac starts from
    /// a fresh owner check rather than inheriting one.
    public func detachAllTerminals() {
        cancelTerminalIdleCountdown()
        terminalSessions.values.forEach { $0.detach() }
        terminalSessions = [:]
        terminalUnlock.lock()
    }

    /// Establishes the paired route: starts the one link owner for this
    /// record, builds the capability-protected adapter and gateway, and lets
    /// the owner's snapshots drive discovery and every later state.
    public func connectPairedDevice(_ record: PairedDeviceRecord?) async {
        guard let record, record.isActive else {
            if linkSource == .paired { unlink() }
            return
        }
        // Keep the paired identity even if the current network route cannot
        // be established. It is the authority to retry on foreground, while
        // the listener and Noise sockets themselves are strictly ephemeral.
        pairedDevice = record
        await settleSuspension()
        pairedConnectionGeneration &+= 1
        let generation = pairedConnectionGeneration
        linkState = .connecting
        await coordinator.setOptions(diagnosticsOptions)
        await coordinator.setProbe { [weak self] in
            await self?.probeGateway() ?? false
        }
        do {
            transport?.stop()
            let provider = CoordinatorChannelProvider(coordinator: coordinator)
            let recorder: LinkStageRecorder = { [weak self] sample in
                Task { @MainActor in self?.record(sample) }
            }
            let gateway = try await gatewayFactory(record, provider, recorder)
            guard generation == pairedConnectionGeneration else { return }
            self.gateway = gateway
            self.link = await gateway.gateway
            self.transport = nil
            linkSource = .paired
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
            return
        } catch let error as RemoteLinkTransportError {
            linkState = .failed(error.message)
            return
        } catch {
            linkState = .failed(error.localizedDescription)
            return
        }
        observeCoordinator()
        await coordinator.start(record: record)
        // Callers get a settled answer: linked, or a typed reason it is not.
        // Later changes keep arriving through the owner's snapshots.
        await waitUntilSettled()
    }

    /// Applies a permission-only refresh without rebuilding a healthy route.
    ///
    /// The host checks the current local grant on every request, so the saved
    /// record is only the phone UI's projection. Replacing that projection in
    /// place makes a Mac-side terminal toggle visible as soon as it is read
    /// from the control plane, while identity, endpoint, or revocation changes
    /// still take the full reconnect path.
    @discardableResult
    public func applyPairedDeviceRecord(_ record: PairedDeviceRecord?) -> Bool {
        guard linkSource == .paired,
              let current = pairedDevice,
              let record,
              record.isActive,
              current.updating(permission: record.permission) == record
        else { return false }
        pairedDevice = record
        // A downgrade during recovery closes what the lesser grant no longer
        // covers before anything is fetched on the reconnected link.
        if !record.permission.permits(.control) {
            detachAllTerminals()
        }
        return true
    }

    /// Takes the session's terminal while its conversation is open.
    ///
    /// A chat still drives the Conversation Hub, but it now owns the same
    /// exclusive session surface as the terminal screen. The preview supplies
    /// the Mac's current grid so claiming it does not resize or reflow the
    /// agent. The caller must consume `output` and discard the terminal when
    /// the chat disappears.
    public func claimTerminalForChat(for session: SessionSummary) async -> TerminalSession? {
        guard session.isRunning, surface.terminal else { return nil }
        let preview = try? await previewSession(for: session)
        guard await unlockTerminal(), let terminal = terminalSession(for: session) else {
            return nil
        }
        let grid = TerminalGeometry.grid(
            for: terminalSize,
            preview: preview,
            viewport: .zero
        )
        terminal.attach(cols: grid.cols, rows: grid.rows)
        return terminal
    }

    /// Classifies a discovery failure. A protocol disagreement is the one
    /// failure where the computer is fine, so it gets its own state rather
    /// than a string the UI cannot tell apart from a dead network.
    private static func linkFailure(_ error: LatchError) -> LinkState {
        if let mismatch = error.protocolMismatch { return .incompatible(mismatch) }
        return .failed(error.message)
    }

    // MARK: - Link owner

    private func observeCoordinator() {
        coordinatorObservation?.cancel()
        let coordinator = self.coordinator
        coordinatorObservation = Task { [weak self] in
            for await snapshot in await coordinator.snapshots() {
                guard !Task.isCancelled, let self else { return }
                await self.apply(snapshot)
            }
        }
    }

    /// One place turns owner snapshots into screen state.
    private func apply(_ snapshot: RemoteLinkSnapshot) async {
        LinkTrace.shared.mark("app.apply.\(RemoteLinkCoordinator.traceWord(snapshot.state))")
        linkSnapshot = snapshot
        coldOpen.observe(path: snapshot.path)
        coldOpen.observe(snapshot.state)
        switch snapshot.state {
        case .ready:
            if let timings = snapshot.timings {
                record(LinkStageSample(stage: .admission, milliseconds: timings.admissionMs))
                record(LinkStageSample(stage: .connect, milliseconds: timings.connectMs))
                record(LinkStageSample(stage: .peerWait, milliseconds: timings.peerWaitMs))
                record(LinkStageSample(stage: .authenticate, milliseconds: timings.authenticateMs))
                record(LinkStageSample(stage: .linkReady, milliseconds: timings.linkReadyMs))
            }
            if let path = snapshot.path { pathReporter.report(path) }
            if snapshot.generation != discoveredGeneration {
                discoveredGeneration = snapshot.generation
                // Discovery is network work. It must not run inline here: the
                // owner's snapshots are applied one after another, and a
                // discovery (or session refresh) that outlives its link, for
                // example one cut off by a suspend, would hold every later
                // state transition hostage until the request timed out. The
                // generation guards inside discard a stale result.
                Task { [weak self] in await self?.discoverOnFreshLink() }
            }
        case .connecting(let attempt):
            if attempt == 0, case .connecting = linkState { return }
            if let capabilities = linkState.cachedCapabilities {
                becomeInterrupted(.interrupted(snapshot.state, capabilities))
            } else if !linkState.isUsable, linkState != .connecting {
                linkState = .connecting
            }
        case .backoff:
            if linkState.cachedCapabilities != nil || linkState.isUsable {
                becomeInterrupted(.interrupted(snapshot.state, linkState.cachedCapabilities))
            } else {
                linkState = .interrupted(snapshot.state, nil)
            }
        case .macOffline:
            becomeInterrupted(.macOffline(linkState.cachedCapabilities))
        case .suspended:
            becomeInterrupted(.interrupted(.suspended, linkState.cachedCapabilities))
        case .revoked(let reason):
            becomeInterrupted(.revoked(reason))
            detachAllTerminals()
        case .pairingRequired(let reason):
            becomeInterrupted(.pairingRequired(reason))
            detachAllTerminals()
        case .disabled:
            break
        }
    }

    /// The link is not usable. Keep what was on screen, mark it stale, stop
    /// the sockets that cannot survive, and turn held terminals into
    /// interrupted ones (never silently closed, never replayed).
    private func becomeInterrupted(_ state: LinkState) {
        if linkState.isUsable {
            sessionsStale = !sessions.isEmpty
            pathReporter.clear()
            conversationStores.values.forEach { $0.stop() }
            interruptTerminals()
        }
        linkState = state
    }

    /// Discovery once per authenticated link. Capabilities may have changed
    /// while the phone was away, and a restarted gateway instance re-bases
    /// every conversation socket.
    private func discoverOnFreshLink() async {
        LinkTrace.shared.mark("app.discovery.begin")
        defer { LinkTrace.shared.mark("app.discovery.end") }
        guard let gateway, let pairedDevice else { return }
        let generation = pairedConnectionGeneration
        do {
            let started = Date()
            let capabilities = try await gateway.discover()
            record(LinkStageSample(stage: .discovery, milliseconds: Self.millis(since: started)))
            guard generation == pairedConnectionGeneration else { return }
            let instanceChanged = gatewayInstanceID != nil && gatewayInstanceID != capabilities.gatewayInstanceId
            gatewayInstanceID = capabilities.gatewayInstanceId
            self.pairedDevice = pairedDevice
            linkSource = .paired
            linkState = .linked(capabilities)
            if let timings = linkSnapshot.timings {
                record(LinkStageSample(
                    stage: .applicationReady,
                    milliseconds: timings.linkReadyMs + Self.millis(since: started)
                ))
            }
            conversationStores.values.forEach {
                $0.reconnect(using: gateway, operationRetentionSeconds: capabilities.operationRetentionSeconds)
            }
            if instanceChanged {
                // A new gateway process has no receipts from the old one in
                // memory beyond its journal; sockets already re-based above.
                highlightedSessionID = nil
            }
            resumeInterruptedTerminals()
            await refreshSessions()
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
    }

    /// The owner's health probe for a link that looks alive after a network
    /// change: one bounded discovery request.
    private func probeGateway() async -> Bool {
        guard let gateway else { return false }
        return (try? await gateway.discover()) != nil
    }

    private func record(_ sample: LinkStageSample) {
        coldOpen.observe(sample)
        recentStages.append(sample)
        if recentStages.count > 64 { recentStages.removeFirst(recentStages.count - 64) }
    }

    private static func millis(since date: Date) -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince(date) * 1000))
    }

    /// Reloads the session list.
    public func refreshSessions() async {
        guard let gateway, linkState.isUsable else { return }
        isLoadingSessions = true
        defer { isLoadingSessions = false }
        do {
            let started = Date()
            sessions = try await gateway.listSessions()
            record(LinkStageSample(stage: .sessionList, milliseconds: Self.millis(since: started)))
            sessionsStale = false
            sessionsError = nil
        } catch let error as LatchError {
            sessionsError = error.message
        } catch {
            sessionsError = error.localizedDescription
        }
    }

    /// Repeats discovery on a usable link, or cuts a backoff short.
    ///
    /// The contract requires discovery before the app resumes application
    /// traffic on a reconnected path; that happens automatically per link
    /// generation. This is the person's "Check again".
    public func rediscover() async {
        guard pairedDevice != nil else { return }
        if linkState.isUsable {
            await discoverOnFreshLink()
        } else {
            await coordinator.retryImmediately()
        }
    }

    /// A real network path change. One immediate attempt through the same
    /// owner: a backoff is cut short, a live link is probed and replaced if
    /// the probe fails. No second retry loop.
    public func networkPathChanged() async {
        guard pairedDevice != nil else { return }
        await coordinator.retryImmediately()
    }

    /// Releases the route before the app is suspended.
    ///
    /// The loopback adapter and its capability die here, the native link is
    /// closed, and conversation sockets stop. The paired record remains so
    /// `resumeAfterSuspension` can make a fresh adapter with a fresh
    /// capability and repeat discovery on foreground.
    public func suspendPairedTransport() {
        guard pairedDevice != nil else { return }
        LinkTrace.shared.mark("app.suspend")
        pairedConnectionGeneration &+= 1
        let coordinator = self.coordinator
        let previous = pendingSuspension
        pendingSuspension = Task {
            await previous?.value
            await coordinator.suspend()
        }
        transport?.stop()
        transport = nil
        if let gateway {
            Task { await gateway.stopTransport() }
        }
        gateway = nil
        link = nil
        highlightedSessionID = nil
        pathReporter.clear()
        becomeInterrupted(.interrupted(.suspended, linkState.cachedCapabilities))
    }

    /// Re-establishes the route after suspension: a new adapter and
    /// capability, then the same owner resumes and discovery follows.
    public func reconnectPairedTransport() async {
        guard let pairedDevice else { return }
        await settleSuspension()
        pairedConnectionGeneration &+= 1
        let generation = pairedConnectionGeneration
        do {
            let provider = CoordinatorChannelProvider(coordinator: coordinator)
            let recorder: LinkStageRecorder = { [weak self] sample in
                Task { @MainActor in self?.record(sample) }
            }
            let gateway = try await gatewayFactory(pairedDevice, provider, recorder)
            guard generation == pairedConnectionGeneration else { return }
            self.gateway = gateway
            self.link = await gateway.gateway
            linkSource = .paired
        } catch {
            linkState = .failed(error.localizedDescription)
            return
        }
        if coordinatorObservation == nil { observeCoordinator() }
        await coordinator.resume()
    }

    /// The socket is intentionally stopped before suspension: iOS can reclaim
    /// the underlying connection without delivering a close callback. Stores
    /// keep their cache and resume tuple, so foreground does not need a replay.
    public func suspendConversations() {
        conversationStores.values.forEach { $0.stop() }
    }

    public func resumeConversations() {
        conversationStores.values.forEach { $0.start() }
    }

    /// Restores a usable route and repeats discovery before any conversation
    /// socket is allowed to resume application traffic.
    public func resumeAfterSuspension() async {
        guard pairedDevice != nil else { return }
        LinkTrace.shared.mark("app.resume.begin")
        defer { LinkTrace.shared.mark("app.resume.end") }
        await settleSuspension()
        LinkTrace.shared.mark("app.resume.suspensionSettled")
        if gateway == nil {
            await reconnectPairedTransport()
        } else {
            await coordinator.retryImmediately()
        }
        // Discovery runs when the owner reports the link ready; conversation
        // sockets resume only once that produced a usable gateway.
        await waitUntilSettled()
        guard case .linked = linkState else { return }
        resumeConversations()
    }

    /// Lets an in-flight suspension finish before the owner is resumed, so
    /// suspend and resume always apply in the order they were requested.
    private func settleSuspension() async {
        if let pendingSuspension {
            await pendingSuspension.value
            if self.pendingSuspension == pendingSuspension { self.pendingSuspension = nil }
        }
    }

    /// Waits for the current attempt to reach a usable or non-retrying state,
    /// bounded so a caller never hangs on a Mac that is offline.
    public func waitUntilSettled(timeout: Duration = .seconds(20)) async {
        let deadline = Date().addingTimeInterval(
            Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
        )
        while Date() < deadline {
            switch linkState {
            case .linked, .revoked, .pairingRequired, .incompatible, .failed, .macOffline, .unlinked:
                return
            case .connecting, .interrupted:
                try? await Task.sleep(for: .milliseconds(25))
            }
        }
    }

    /// Diagnostics-only: whether the LAN attempt is skipped so the relay path
    /// is the one measured. Applies from the next connection attempt.
    public func setDiagnosticsSkipLAN(_ skip: Bool) async {
        diagnosticsOptions.skipLAN = skip
        coldOpen.observe(skipLAN: skip)
        await coordinator.setOptions(diagnosticsOptions)
    }
}

// MARK: - Diagnostics subject

extension AppModel: DiagnosticsSubject {
    public func diagnosticsRecoveryCycle(skipLAN: Bool) async throws -> (path: String?, stages: [LinkStageSample]) {
        await setDiagnosticsSkipLAN(skipLAN)
        let started = Date()
        suspendPairedTransport()
        let before = recentStages.count
        await resumeAfterSuspension()
        guard case .linked = linkState else {
            throw LatchError.transport("The link did not become usable after the cycle.")
        }
        var stages = Array(recentStages.dropFirst(before))
        stages.append(LinkStageSample(stage: .applicationReady, milliseconds: Self.millis(since: started)))
        return (linkSnapshot.path?.rawValue, stages)
    }

    public func diagnosticsSessionList() async throws -> LinkStageSample {
        guard let gateway else { throw LatchError.transport("Not linked to a computer.") }
        let (list, sample) = try await measureStage(.sessionList) { try await gateway.listSessions() }
        sessions = list
        return sample
    }

    public func diagnosticsPreview() async throws -> LinkStageSample? {
        guard let gateway, let session = sessions.first else { return nil }
        let (_, sample) = try await measureStage(.preview) {
            try await gateway.previewSession(sessionID: session.id, scrollbackLines: 0)
        }
        return sample
    }

    public func diagnosticsTerminalFirstOutput() async throws -> LinkStageSample? {
        guard surface.terminal, terminalUnlock.isUnlocked,
              let session = sessions.first(where: \.isRunning),
              let terminal = terminalSession(for: session)
        else { return nil }
        defer { discardTerminal(for: session) }
        let started = Date()
        terminal.attach(cols: 80, rows: 24)
        let deadline = Date().addingTimeInterval(10)
        for await _ in terminal.output {
            return LinkStageSample(stage: .terminalFirstOutput, milliseconds: Self.millis(since: started))
        }
        _ = deadline
        throw LatchError.transport("No terminal output arrived.")
    }
}

/// Bridges the loopback adapter's channel requests to the one link owner.
final class CoordinatorChannelProvider: AuthenticatedGatewayChannelProvider, @unchecked Sendable {
    private let coordinator: RemoteLinkCoordinator

    init(coordinator: RemoteLinkCoordinator) { self.coordinator = coordinator }

    func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
        try await coordinator.openGatewayChannel()
    }
}

/// The kit's refusal to invent a transport: a build without the native
/// connector fails clearly instead of pretending to connect.
struct UnavailableLinkConnector: RemoteLinkConnecting {
    func connect(record: PairedDeviceRecord, options: RemoteLinkConnectOptions) async throws -> any RemoteLinkConnection {
        throw RemoteLinkFailure.authentication("This build does not include the native Remote Link transport.")
    }
}

/// For tests that stub the gateway over HTTP and only need the owner to
/// report a ready link that never drops.
final class AlwaysReadyLinkConnector: RemoteLinkConnecting, @unchecked Sendable {
    final class Connection: RemoteLinkConnection, @unchecked Sendable {
        let path: RemotePath = .local
        let grantRevision: UInt64 = 1
        let timings = RemoteLinkStageTimings()
        private let closed = AsyncStream<Void>.makeStream()
        func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
            throw RemoteLinkTransportError.listenerUnavailable
        }
        func waitClosed() async { for await _ in closed.stream {} }
        func close() async { closed.continuation.finish() }
    }

    func connect(record: PairedDeviceRecord, options: RemoteLinkConnectOptions) async throws -> any RemoteLinkConnection {
        Connection()
    }
}
