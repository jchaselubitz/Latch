import Foundation
import Observation

/// The app's link to one computer, shared by both tabs.
///
/// Discovery lives here rather than in each screen: it is the mandatory first
/// step, its answer decides what the session screen may offer, and repeating it
/// per screen would turn one contract question into several.
/// Behavior lives in focused AppModel extensions. Shared implementation state
/// is internal so those files can update it; public state stays read-only to
/// kit clients unless it is an explicitly editable preference.
@MainActor
@Observable
public final class AppModel {
    // MARK: - Link types

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

    // MARK: - Observable state

    public internal(set) var linkState: LinkState = .unlinked

    /// The link owner's latest immutable snapshot, for Settings.
    public internal(set) var linkSnapshot = RemoteLinkSnapshot()

    /// The session list is from before the current interruption.
    public internal(set) var sessionsStale = false

    /// Content-free stage timings from the most recent link and requests,
    /// newest last, bounded.
    public internal(set) var recentStages: [LinkStageSample] = []

    public internal(set) var link: GatewayLink?

    public internal(set) var gateway: LatchGateway?

    public internal(set) var linkSource: LinkSource?

    public internal(set) var sessions: [SessionSummary] = []

    /// The network path the paired route is running over, once one is open.
    public private(set) var remotePath: RemotePath?

    /// How this phone's paired connections have resolved, across launches.
    /// Read during a field run; it leaves the device only if a person reads it
    /// off the screen.
    public private(set) var remotePathTally = RemotePathTally()

    public internal(set) var sessionsError: String?

    public internal(set) var isLoadingSessions = false

    /// The session most recently created from the folder browser. The view may
    /// highlight it without treating creation as permission to open or attach.
    public internal(set) var highlightedSessionID: String?

    /// The session a `latch://sessions/<id>` link asked to open, until the
    /// list holds it. Kept here rather than in a view because a link usually
    /// arrives on a cold launch, before the paired route has listed anything.
    public internal(set) var requestedSessionID: String?

    /// Why the last linked session could not be opened, for the view to say.
    public internal(set) var requestedSessionError: String?

    /// Sessions this phone has asked the Mac to stop and has not yet heard
    /// back about. A stop can take several seconds — the Mac waits out its own
    /// grace period before escalating — so the rows have to be able to say
    /// that the request is in flight rather than look ignored.
    public internal(set) var stoppingSessionIDs: Set<String> = []

    /// The saved default folder, observed so Settings updates the moment one
    /// is chosen. The store remains the source of truth; this mirrors it.
    public internal(set) var defaultNewSessionFolder: String?

    // MARK: - Shared implementation state and preferences

    /// Session stores are retained here rather than by a navigation view, so a
    /// pushed chat can reconnect from its cached revision instead of replaying
    /// the conversation after every back-navigation.
    var conversationStores: [String: ConversationStore] = [:]

    /// Terminal connections are retained here for the same reason, plus one
    /// more: while attached, the phone holds the session's only surface, so
    /// something outside the pushed screen must be able to release it.
    var terminalSessions: [String: TerminalSession] = [:]

    var terminalIdleWatch: Task<Void, Never>?

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

    let terminalConnector: TerminalConnecting?

    /// The device-owner check standing in front of the terminal. Chat has no
    /// equivalent and deliberately so: a phone that may read and reply is
    /// doing what it was paired for, while a terminal runs commands on the
    /// Mac and takes the session's one surface.
    let terminalUnlock: TerminalUnlock

    private let presentationStore: any SessionPresentationStoring

    private let terminalSizeStore: any TerminalSizeStoring

    let newSessionFolderStore: any NewSessionFolderStoring

    let agentModelStore: any AgentModelPreferenceStoring

    /// Where the transport writes the path it selected, so Settings can say
    /// whether this session is on the local network, direct, or relayed.
    let pathReporter: RemotePathReporter

    /// Builds the gateway client over the capability-protected loopback
    /// adapter. One per link generation; the adapter is stopped on suspend.
    public typealias GatewayFactory = @Sendable (
        PairedDeviceRecord, any AuthenticatedGatewayChannelProvider, LinkStageRecorder?
    ) async throws -> LatchGateway

    let gatewayFactory: GatewayFactory

    /// The one link owner. Screens never open connections.
    public let coordinator: RemoteLinkCoordinator

    var coordinatorObservation: Task<Void, Never>?

    var pairedDevice: PairedDeviceRecord?

    /// The loopback adapter behind the current gateway, stopped on suspend so
    /// its capability dies with it.
    var transport: (any GatewayTransport)?

    /// Which link generation discovery last ran for. Discovery runs once per
    /// new authenticated link, never per screen.
    var discoveredGeneration = 0

    /// The gateway instance discovery last saw, so a restarted gateway is
    /// noticed and its per-instance state (conversation sockets) re-based.
    var gatewayInstanceID: String?

    /// Invalidates an in-flight connect when its process-local route is torn
    /// down. A backgrounded factory must never resurrect a connection.
    var pairedConnectionGeneration = 0

    var diagnosticsOptions = RemoteLinkConnectOptions()

    /// The owner's suspension in flight. Suspension is requested from a
    /// synchronous scene-phase handler, so it runs as a task; every resume
    /// waits for it first. Without that a fast background/foreground (or a
    /// diagnostics cycle) could resume the old supervisor and then have the
    /// late suspension tear the fresh link down with nobody left to retry.
    var pendingSuspension: Task<Void, Never>?

    /// Armed only by the USB harness's launch argument; writes one
    /// `cold_open` record for this process and then goes quiet.
    public let coldOpen: ColdOpenRecorder

    // MARK: - Initialization

    public init(
        linkConnector: (any RemoteLinkConnecting)? = nil,
        gatewayFactory: GatewayFactory? = nil,
        pairedGatewayFactory: (@Sendable (PairedDeviceRecord) async throws -> LatchGateway)? = nil,
        pathReporter: RemotePathReporter = RemotePathReporter(),
        presentationStore: any SessionPresentationStoring = UserDefaultsSessionPresentationStore(),
        terminalSizeStore: any TerminalSizeStoring = UserDefaultsTerminalSizeStore(),
        newSessionFolderStore: any NewSessionFolderStoring = UserDefaultsNewSessionFolderStore(),
        agentModelStore: any AgentModelPreferenceStoring = UserDefaultsAgentModelPreferenceStore(),
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
        self.agentModelStore = agentModelStore
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
}
