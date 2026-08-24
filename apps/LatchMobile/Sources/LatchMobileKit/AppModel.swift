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
    private let presentationStore: any SessionPresentationStoring
    private let terminalSizeStore: any TerminalSizeStoring
    private let storage: LinkStorage
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
        presentationStore: any SessionPresentationStoring = UserDefaultsSessionPresentationStore(),
        terminalSizeStore: any TerminalSizeStoring = UserDefaultsTerminalSizeStore(),
        terminalConnector: TerminalConnecting? = nil
    ) {
        self.terminalConnector = terminalConnector
        self.presentationStore = presentationStore
        self.terminalSizeStore = terminalSizeStore
        self.sessionPresentation = presentationStore.load()
        self.terminalSize = terminalSizeStore.load()
        self.storage = storage
        self.sessionFactory = sessionFactory
        self.pairedGatewayFactory = pairedGatewayFactory ?? { record in
            let candidates = try await BonjourMacDiscovery().candidates(
                matching: record.mac.publicKey
            )
            guard let target = candidates.first else {
                throw NoiseTunnelError.macNotReachable
            }
            let transport = try await NoiseTunnelGatewayTransport.start(
                target: target,
                pairedDevice: record,
                identityStore: identityStore
            )
            return LatchGateway(transport: transport)
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

    /// Returns the one terminal connection for this session, or nil when this
    /// device may not open one — gated on `surface.terminal`, the way
    /// `conversationStore(for:)` is gated on the conversation endpoint.
    public func terminalSession(for session: SessionSummary) -> TerminalSession? {
        guard let gateway, surface.terminal else { return nil }
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

    /// Releases every held surface before the app is suspended.
    ///
    /// The sessions themselves are kept, so foregrounding returns to
    /// `.closed(.detached)` with a Reattach button rather than silently taking
    /// the surface back from whoever is now using it.
    public func suspendTerminals() {
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
    public func detachAllTerminals() {
        terminalSessions.values.forEach { $0.detach() }
        terminalSessions = [:]
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
