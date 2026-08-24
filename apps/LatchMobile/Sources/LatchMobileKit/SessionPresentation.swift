import Foundation

/// Which screen a tap on a session lands on, when the session offers a choice.
///
/// This is a *default presentation*, not a fallback rule: a phone set to
/// Terminal opens a Claude session's terminal too. Either screen can still be
/// impossible for a given session, which is what `SessionRoute.route` is for.
public enum SessionPresentation: String, CaseIterable, Codable, Sendable {
    case terminal
    case chat

    public static let `default` = SessionPresentation.terminal

    public var label: String {
        switch self {
        case .terminal: "Terminal"
        case .chat: "Chat"
        }
    }
}

/// Where the preferred session view is kept between launches.
///
/// This mirrors `ControlPlaneAddressStoring` deliberately — protocol,
/// `UserDefaults` for the app, in-memory for tests — because it is the same
/// kind of thing: a preference, not a credential, and no new storage idiom
/// should appear for it.
public protocol SessionPresentationStoring: Sendable {
    func load() -> SessionPresentation
    func save(_ presentation: SessionPresentation)
}

/// `UserDefaults`-backed storage, used by the app.
public struct UserDefaultsSessionPresentationStore: SessionPresentationStoring {
    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "sessionPresentation") {
        self.defaults = defaults
        self.key = key
    }

    /// An unreadable or unrecognised value falls back to the default rather
    /// than refusing to open a session: this preference decides a destination,
    /// so it must always produce one.
    public func load() -> SessionPresentation {
        guard let raw = defaults.string(forKey: key),
              let presentation = SessionPresentation(rawValue: raw)
        else { return .default }
        return presentation
    }

    public func save(_ presentation: SessionPresentation) {
        defaults.set(presentation.rawValue, forKey: key)
    }
}

/// In-memory storage, for tests and previews.
public final class MemorySessionPresentationStore: SessionPresentationStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var presentation: SessionPresentation

    public init(_ presentation: SessionPresentation = .default) {
        self.presentation = presentation
    }

    public func load() -> SessionPresentation {
        lock.lock()
        defer { lock.unlock() }
        return presentation
    }

    public func save(_ presentation: SessionPresentation) {
        lock.lock()
        defer { lock.unlock() }
        self.presentation = presentation
    }
}

/// The grid the phone attaches at.
///
/// Geometry is a choice, not a measurement: `cols`/`rows` are query parameters
/// on the terminal socket and nothing requires them to describe the phone's
/// screen. `matchMac` attaches at the pane's current size so it does not
/// resize at all — no `SIGWINCH`, no reflow, and a paused prompt that cannot
/// repaint transfers exactly as it stands.
public enum TerminalSize: String, CaseIterable, Codable, Sendable {
    case matchMac
    case readable
    case fixed80x24
    case fixed100x30

    public static let `default` = TerminalSize.matchMac

    public var label: String {
        switch self {
        case .matchMac: "Match the Mac"
        case .readable: "Readable"
        case .fixed80x24: "80 × 24"
        case .fixed100x30: "100 × 30"
        }
    }

    /// The fixed grid this choice names, or `nil` when the grid is derived —
    /// from the preview for `matchMac`, from the viewport for `readable`.
    public var fixedGrid: (cols: Int, rows: Int)? {
        switch self {
        case .matchMac, .readable: nil
        case .fixed80x24: (80, 24)
        case .fixed100x30: (100, 30)
        }
    }
}

/// Where the preferred terminal grid is kept between launches. Same idiom as
/// `SessionPresentationStoring`, for the same reason.
public protocol TerminalSizeStoring: Sendable {
    func load() -> TerminalSize
    func save(_ size: TerminalSize)
}

/// `UserDefaults`-backed storage, used by the app.
public struct UserDefaultsTerminalSizeStore: TerminalSizeStoring {
    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "terminalSize") {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> TerminalSize {
        guard let raw = defaults.string(forKey: key),
              let size = TerminalSize(rawValue: raw)
        else { return .default }
        return size
    }

    public func save(_ size: TerminalSize) {
        defaults.set(size.rawValue, forKey: key)
    }
}

/// In-memory storage, for tests and previews.
public final class MemoryTerminalSizeStore: TerminalSizeStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var size: TerminalSize

    public init(_ size: TerminalSize = .default) {
        self.size = size
    }

    public func load() -> TerminalSize {
        lock.lock()
        defer { lock.unlock() }
        return size
    }

    public func save(_ size: TerminalSize) {
        lock.lock()
        defer { lock.unlock() }
        self.size = size
    }
}

/// Where a tap on a session row goes.
public enum SessionRoute: Equatable, Sendable {
    /// The terminal screen. `autoAttach` decides whether arriving there takes
    /// the session's exclusive surface from the Mac immediately, or waits for
    /// the person to ask.
    case terminal(autoAttach: Bool)
    case chat
    case unavailable(SessionRouteBlock)
}

/// Why neither screen can be opened. Each case is a different sentence on the
/// screen, so they are kept apart rather than collapsed into one failure.
public enum SessionRouteBlock: Equatable, Sendable {
    /// Paired at observe or interact: a terminal is a control surface.
    case needsControlGrant
    /// The Mac predates the terminal route, or it is switched off.
    case noTerminalEndpoint
    /// No connector for chat, and no terminal to fall back to either.
    case noConversation
}

extension SessionRoute {
    /// Resolves a tap. Pure, so the table below can be tested directly rather
    /// than through navigation.
    ///
    /// Preference alone cannot decide, because either screen can be
    /// impossible: chat needs a connector and the `conversation` endpoint,
    /// and a terminal needs the `terminal` endpoint and the `control` grant.
    public static func route(
        preference: SessionPresentation,
        connector: SessionConnector,
        surface: SessionSurface,
        isRunning: Bool
    ) -> SessionRoute {
        // Chat needs both a connector to drive it and a Hub to talk to.
        let chatPossible: Bool = {
            guard surface.chat else { return false }
            switch connector {
            case .named, .unknown: return true
            case .none: return false
            }
        }()

        switch preference {
        case .terminal:
            if surface.terminal {
                // Auto-attach only for a running session. There is nothing to
                // steal from an exited one, and a spawned attach against a
                // dead pane closes with `session_exited` and reads as failure.
                return .terminal(autoAttach: isRunning)
            }
            if chatPossible { return .chat }
            return .unavailable(block(surface: surface))

        case .chat:
            // `.unknown` still opens chat: an older Mac that omits the
            // connector field must keep behaving exactly as it does today,
            // including `ChatView`'s existing "connector is null" screen.
            if chatPossible { return .chat }
            // No connector, and the person asked for chat. They get the
            // terminal instead — but *without* auto-attaching. The steal is
            // not implied by a tap that asked for something else.
            if surface.terminal { return .terminal(autoAttach: false) }
            return .unavailable(block(surface: surface))
        }
    }

    /// Which explanation to show when nothing can be opened. A phone whose
    /// gateway advertises the route but whose grant forbids it is a different
    /// problem from a Mac that has no route at all, and says so.
    private static func block(surface: SessionSurface) -> SessionRouteBlock {
        // Advertised but cleared: the gateway has the route and this device's
        // grant is what forbids it. That is a pairing answer, not an update.
        if surface.terminalAdvertised { return .needsControlGrant }
        // The Hub is there, so the Mac is merely older than the terminal
        // route; this session just has no connector to chat with.
        if surface.chat { return .noTerminalEndpoint }
        return .noConversation
    }
}
