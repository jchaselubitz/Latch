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
        case manual
        case paired
    }

    public enum LinkState: Equatable {
        case unlinked
        case connecting
        case linked(GatewayCapabilities)
        /// The computer answered, and this build cannot speak to it. Kept
        /// separate from `failed` because it is not a connection problem and
        /// must not be reported as one: the remedy is an update, on a named
        /// side, and the saved link stays valid.
        case incompatible(ProtocolMismatch)
        case failed(String)
    }

    public private(set) var linkState: LinkState = .unlinked
    public private(set) var link: GatewayLink?
    public private(set) var gateway: LatchGateway?
    public private(set) var linkSource: LinkSource?
    public private(set) var sessions: [SessionSummary] = []
    /// The network path the paired route is running over, once one is open.
    /// Nil for a manual link, which is whatever tunnel the person configured.
    public private(set) var remotePath: RemotePath?
    /// How this phone's paired connections have resolved, across launches.
    /// Read during a field run; it leaves the device only if a person reads it
    /// off the screen.
    public private(set) var remotePathTally = RemotePathTally()
    public private(set) var sessionsError: String?
    public private(set) var isLoadingSessions = false
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
    private let storage: LinkStorage
    /// Where the transport writes the path it selected, so Settings can say
    /// whether this session is on the local network, direct, or relayed.
    private let pathReporter: RemotePathReporter
    private let sessionFactory: @Sendable (GatewayLink) -> LatchGateway
    private let pairedGatewayFactory: @Sendable (PairedDeviceRecord) async throws -> LatchGateway
    private var pairedDevice: PairedDeviceRecord?
    /// Invalidates an in-flight paired connect when its process-local route is
    /// torn down. A backgrounded factory must never resurrect a connection.
    private var pairedConnectionGeneration = 0

    public init(
        storage: LinkStorage = KeychainLinkStorage(),
        sessionFactory: @escaping @Sendable (GatewayLink) -> LatchGateway = { LatchGateway(link: $0) },
        identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore(),
        pairedGatewayFactory: (@Sendable (PairedDeviceRecord) async throws -> LatchGateway)? = nil,
        pathReporter: RemotePathReporter = RemotePathReporter(),
        presentationStore: any SessionPresentationStoring = UserDefaultsSessionPresentationStore(),
        terminalSizeStore: any TerminalSizeStoring = UserDefaultsTerminalSizeStore(),
        terminalConnector: TerminalConnecting? = nil,
        terminalUnlock: TerminalUnlock? = nil
    ) {
        self.pathReporter = pathReporter
        self.terminalUnlock = terminalUnlock ?? TerminalUnlock()
        self.terminalConnector = terminalConnector
        self.presentationStore = presentationStore
        self.terminalSizeStore = terminalSizeStore
        self.sessionPresentation = presentationStore.load()
        self.terminalSize = terminalSizeStore.load()
        self.storage = storage
        self.sessionFactory = sessionFactory
        // The default route is the local network only. The app injects the
        // full one — Bonjour, then presence and ICE — because the ICE stack
        // lives in `LatchTransportNative`, which sits above this module.
        self.pairedGatewayFactory = pairedGatewayFactory ?? PairedGatewayRoute.factory(
            identityStore: identityStore,
            pathReporter: pathReporter
        )
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
            .restricted(to: linkSource == .paired ? pairedDevice?.permission : nil)
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

    /// Restores a saved link at launch and connects to it.
    public func restore() async {
        guard link == nil, let saved = try? storage.load() else { return }
        await connect(to: saved, persist: false)
    }

    /// Links to a computer and runs discovery.
    public func link(address: String, token: String) async {
        do {
            let link = try GatewayLink(address: address, token: token)
            await connect(to: link, persist: true)
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
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
        if linkSource == .manual {
            try? storage.clear()
        }
        link = nil
        gateway = nil
        linkSource = nil
        pairedDevice = nil
        pairedConnectionGeneration &+= 1
        sessions = []
        sessionsError = nil
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

    /// Asks the device owner to confirm before a terminal is opened.
    ///
    /// Called by the terminal screen ahead of `terminalSession(for:)`. Inside
    /// the grace window it answers without prompting, so attaching, reading
    /// something else, and reattaching is one Face ID check rather than three.
    @discardableResult
    public func unlockTerminal() async -> Bool {
        guard surface.terminal else { return false }
        return await terminalUnlock.unlock(
            reason: "Open a terminal on your Mac and run commands on it."
        )
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
        let created = TerminalSession(sessionID: id) { cols, rows in
            if let connector {
                return try await connector(id, cols, rows)
            }
            return try await gateway.openTerminal(sessionID: id, cols: cols, rows: rows)
        }
        terminalSessions[id] = created
        return created
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

    private func connect(to link: GatewayLink, persist: Bool) async {
        linkState = .connecting
        let gateway = sessionFactory(link)
        await finishConnecting(
            gateway: gateway,
            link: link,
            source: .manual,
            pairedDevice: nil,
            persist: persist
        )
    }

    /// Establishes the paired LAN route. A manual link remains coequal and is
    /// never replaced by pairing: it is the route that also works when no
    /// control plane or Bonjour service exists.
    public func connectPairedDevice(_ record: PairedDeviceRecord?) async {
        guard let record, record.isActive else {
            if linkSource == .paired { unlink() }
            return
        }
        guard linkSource != .manual else { return }
        // Keep the paired identity even if the current network route cannot
        // be established. It is the authority to retry on foreground, while
        // the listener and Noise sockets themselves are strictly ephemeral.
        pairedDevice = record
        pairedConnectionGeneration &+= 1
        let generation = pairedConnectionGeneration
        linkState = .connecting
        do {
            let pairedGateway = try await pairedGatewayFactory(record)
            // A manual link could have completed while Bonjour was browsing.
            // It wins because it is the explicit route the person configured.
            guard linkSource != .manual, generation == pairedConnectionGeneration else { return }
            let pairedLink = await pairedGateway.gateway
            await finishConnecting(
                gateway: pairedGateway,
                link: pairedLink,
                source: .paired,
                pairedDevice: record,
                persist: false,
                pairedConnectionGeneration: generation
            )
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
        } catch let error as NoiseTunnelError {
            linkState = .failed(error.message)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
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

    private func finishConnecting(
        gateway: LatchGateway,
        link: GatewayLink,
        source: LinkSource,
        pairedDevice: PairedDeviceRecord?,
        persist: Bool,
        pairedConnectionGeneration: Int? = nil
    ) async {
        do {
            let capabilities = try await gateway.discover()
            guard pairedConnectionGeneration == nil
                || pairedConnectionGeneration == self.pairedConnectionGeneration
            else { return }
            self.link = link
            self.gateway = gateway
            linkSource = source
            self.pairedDevice = pairedDevice
            linkState = .linked(capabilities)
            conversationStores.values.forEach {
                $0.reconnect(using: gateway, operationRetentionSeconds: capabilities.operationRetentionSeconds)
            }
            if persist {
                try? storage.save(link)
            }
            await refreshSessions()
        } catch let error as LatchError {
            // A saved control-plane URL must not stay as "the computer":
            // restore would keep winning over pairing on every launch.
            if error == .notAGateway, source == .manual {
                try? storage.clear()
                if let existing = self.pairedDevice {
                    await connectPairedDevice(existing)
                    if case .linked = linkState { return }
                }
            }
            linkState = Self.linkFailure(error)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
    }

    /// Reloads the session list.
    public func refreshSessions() async {
        guard let gateway else { return }
        isLoadingSessions = true
        defer { isLoadingSessions = false }
        do {
            sessions = try await gateway.listSessions()
            sessionsError = nil
        } catch let error as LatchError {
            sessionsError = error.message
        } catch {
            sessionsError = error.localizedDescription
        }
    }

    /// Repeats discovery after the connection was interrupted.
    ///
    /// The contract requires this before the app resumes application traffic
    /// on a reconnected path: capabilities may have changed while the phone was
    /// away, and carrying the old answers across would assume a feature the
    /// gateway no longer offers.
    public func rediscover() async {
        if linkSource == .paired, let pairedDevice {
            // A paired transport owns a process-local listener and sockets;
            // those cannot be assumed to survive backgrounding. Start a new
            // route rather than rediscovering through a stale listener.
            gateway = nil
            link = nil
            linkSource = nil
            self.pairedDevice = nil
            await connectPairedDevice(pairedDevice)
            return
        }
        guard let gateway, let link else { return }
        await gateway.invalidateDiscovery()
        await connect(to: link, persist: false)
    }

    /// Releases a paired route before the app is suspended.
    ///
    /// iOS may terminate the loopback listener, its TCP connection, and any
    /// future ICE allocation while the app is in the background. Retaining a
    /// linked state across that boundary would let UI code treat stale
    /// capabilities as live. The paired record remains so `reconnectPaired`
    /// can make a fresh route and repeat discovery on foreground.
    public func suspendPairedTransport() {
        guard linkSource != .manual, pairedDevice != nil else { return }
        pairedConnectionGeneration &+= 1
        gateway = nil
        link = nil
        linkSource = nil
        sessions = []
        sessionsError = nil
        // The path belongs to the torn-down route. Leaving it on screen would
        // report a live connection the phone no longer has.
        pathReporter.clear()
        linkState = .unlinked
    }

    /// Re-establishes a paired route after suspension or a path change.
    /// Discovery is part of reconnection, not an optional refresh: the Mac's
    /// capabilities and the granted permission may have changed while the
    /// phone was away.
    public func reconnectPairedTransport() async {
        // A currently linked paired route is handled by `rediscover()`.
        // This entry point is only for a route suspension/path teardown.
        guard linkSource == nil, let pairedDevice else { return }
        await connectPairedDevice(pairedDevice)
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
        if linkSource == nil, pairedDevice != nil {
            await reconnectPairedTransport()
        } else {
            await rediscover()
        }
        guard case .linked = linkState else { return }
        resumeConversations()
    }
}
