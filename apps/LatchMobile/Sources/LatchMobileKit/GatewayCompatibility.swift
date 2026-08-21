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
            interactionControls: conversation
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

    public init(chat: Bool, composer: Bool, interactionControls: Bool) {
        self.chat = chat
        self.composer = composer
        self.interactionControls = interactionControls
    }

    public func restricted(to permission: DevicePermission?) -> SessionSurface {
        guard let permission, !permission.permits(.interact) else { return self }
        return SessionSurface(chat: chat, composer: false, interactionControls: false)
    }
}
