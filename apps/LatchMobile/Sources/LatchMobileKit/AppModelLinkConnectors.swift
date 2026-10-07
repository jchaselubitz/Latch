import Foundation

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
        let permission: DevicePermission
        let timings = RemoteLinkStageTimings()
        private let closed = AsyncStream<Void>.makeStream()
        init(permission: DevicePermission) { self.permission = permission }
        func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
            throw RemoteLinkTransportError.listenerUnavailable
        }
        func waitClosed() async { for await _ in closed.stream {} }
        func close() async { closed.continuation.finish() }
    }

    func connect(record: PairedDeviceRecord, options: RemoteLinkConnectOptions) async throws -> any RemoteLinkConnection {
        Connection(permission: record.permission)
    }
}
