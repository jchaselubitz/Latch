import Foundation

/// Protocol-major-2 discovery rules. There is no legacy fallback or probing.
public enum GatewayCompatibility {
    public static func isControlPlaneUnmatchedRoute(
        status: Int,
        code: String?,
        reason: String
    ) -> Bool {
        status == 404 && code == "not_found" && reason == "no such resource"
    }

    public static func supports(
        endpoint: GatewayEndpointsName,
        capabilities: GatewayCapabilities?
    ) -> Bool {
        guard let capabilities,
              capabilities.protocolVersion == LatchContract.protocolVersion
        else { return false }
        return capabilities.endpoints.isEnabled(endpoint)
    }

    public static func supports(
        feature: GatewayFeaturesName,
        capabilities: GatewayCapabilities?
    ) -> Bool {
        guard let capabilities,
              capabilities.protocolVersion == LatchContract.protocolVersion
        else { return false }
        return capabilities.features.isEnabled(feature)
    }

    public static func validate(_ capabilities: GatewayCapabilities) throws {
        guard capabilities.protocolVersion == LatchContract.protocolVersion else {
            throw LatchError.unsupportedProtocol(
                reported: capabilities.protocolVersion,
                supported: LatchContract.protocolVersion
            )
        }
    }

    public static func sessionSurface(for capabilities: GatewayCapabilities?) -> SessionSurface {
        let conversation = supports(endpoint: .conversation, capabilities: capabilities)
        return SessionSurface(
            chat: conversation,
            composer: conversation,
            interactionControls: conversation,
            terminal: supports(endpoint: .terminal, capabilities: capabilities)
        )
    }
}

public enum ProtocolMismatch: Equatable, Sendable {
    case updatePhone(reported: Int, supported: Int)
    case updateComputer(reported: Int, supported: Int)

    public init(reported: Int, supported: Int) {
        self = reported > supported
            ? .updatePhone(reported: reported, supported: supported)
            : .updateComputer(reported: reported, supported: supported)
    }

    public var reported: Int {
        switch self {
        case .updatePhone(let reported, _), .updateComputer(let reported, _): reported
        }
    }

    public var supported: Int {
        switch self {
        case .updatePhone(_, let supported), .updateComputer(_, let supported): supported
        }
    }

    public var icon: String {
        switch self {
        case .updatePhone: "arrow.down.circle"
        case .updateComputer: "laptopcomputer.trianglebadge.exclamationmark"
        }
    }

    public var title: String {
        switch self {
        case .updatePhone: "Update Latch on this phone"
        case .updateComputer: "Update Latch on your computer"
        }
    }

    public var detail: String {
        switch self {
        case .updatePhone:
            "Your computer is reachable and running a newer version of Latch. Update this app to use it again."
        case .updateComputer:
            "This app is newer than Latch on your computer. Run `latch update` there, then reopen this screen."
        }
    }

    public var summary: String {
        "\(title). Gateway protocol \(reported); this app implements \(supported)."
    }
}

public struct SessionSurface: Equatable, Sendable {
    public let chat: Bool
    public let composer: Bool
    public let interactionControls: Bool
    /// Whether this device may take the session's exclusive terminal surface.
    public let terminal: Bool
    /// Whether the *gateway* offers a terminal route at all, before this
    /// device's grant is applied. It survives `restricted(to:)` on purpose:
    /// an observing phone and a Mac too old to have the route both resolve
    /// `terminal == false`, and the screen explaining the refusal has to tell
    /// them apart — one is answered by pairing again, the other by updating.
    public let terminalAdvertised: Bool

    public init(
        chat: Bool,
        composer: Bool,
        interactionControls: Bool,
        terminal: Bool = false,
        terminalAdvertised: Bool? = nil
    ) {
        self.chat = chat
        self.composer = composer
        self.interactionControls = interactionControls
        self.terminal = terminal
        self.terminalAdvertised = terminalAdvertised ?? terminal
    }

    /// `nil` means unrestricted, and that is not an oversight: a manual
    /// `latch serve` link sends no grant header at all, and `http.rs` grants
    /// loopback requests `Grant::Control`. Only a paired device carries a
    /// permission to narrow by.
    public func restricted(to permission: DevicePermission?) -> SessionSurface {
        guard let permission else { return self }
        // A terminal is a control surface: it sends raw bytes into the pane
        // and resizes the child. Observe and interact grants may not open one,
        // so it is cleared before the interact check below, which returns
        // early for a control grant.
        let terminal = terminal && permission.permits(.control)
        guard !permission.permits(.interact) else {
            return SessionSurface(
                chat: chat,
                composer: composer,
                interactionControls: interactionControls,
                terminal: terminal,
                terminalAdvertised: terminalAdvertised
            )
        }
        return SessionSurface(
            chat: chat,
            composer: false,
            interactionControls: false,
            terminal: terminal,
            terminalAdvertised: terminalAdvertised
        )
    }
}
