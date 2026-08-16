import Foundation
import LatchMobileKit

/// Opens one shared-Rust data channel for each loopback request.
///
/// The direct attempt receives only the STUN configuration returned by
/// `GET /v1/ice-servers`. TURN credentials are not even requested until that
/// connect fails, so the ICE agent cannot silently select relay first.
public final class NativeRemoteChannelProvider: RemoteNoiseChannelProvider, @unchecked Sendable {
    private let record: PairedDeviceRecord
    private let signaling: any SignalingClient
    private let lanFallback: (any RemoteNoiseChannelProvider)?
    private let policy = RemoteTransportPolicy()
    private var pathChangeHandler: (@Sendable () async -> Void)?

    public init(
        record: PairedDeviceRecord,
        signaling: any SignalingClient,
        lanFallback: (any RemoteNoiseChannelProvider)? = nil,
        pathChangeHandler: (@Sendable () async -> Void)? = nil
    ) {
        self.record = record
        self.signaling = signaling
        self.lanFallback = lanFallback
        self.pathChangeHandler = pathChangeHandler
    }

    public func openChannel() async throws -> any RemoteNoiseChannel {
        do {
            return try await openNativeChannel()
        } catch {
            guard let lanFallback else { throw error }
            return try await lanFallback.openChannel()
        }
    }

    private func openNativeChannel() async throws -> any RemoteNoiseChannel {
        let stun = try await signaling.iceServers(for: record)
        let credentials = Self.credentials()
        let transport = try await RemoteTransport.gather(
            credentials: credentials,
            servers: stun.flatMap(Self.nativeServers)
        )
        do {
            return try await connect(
                transport: transport,
                local: transport.localDescription(),
                expectedPath: .direct
            )
        } catch let failure as DirectConnectivityFailure {
            // This is the sole issuance point for TURN credentials. Noise is
            // above this provider, so an identity mismatch never reaches it.
            guard transport.relayRetryAllowed() else { throw failure.underlying }
            await policy.recordDirectFailure()
            try await policy.authorizeRelayRetry()
            let turn = try await signaling.turnCredentials(for: record)
            let servers = (stun + turn.iceServers).flatMap(Self.nativeServers)
            let local = try await transport.retryWithRelay(servers: servers)
            do {
                return try await connect(
                    transport: transport,
                    local: local,
                    expectedPath: .relay
                )
            } catch let failure as DirectConnectivityFailure {
                throw failure.underlying
            }
        }
    }

    private func connect(
        transport: RemoteTransport,
        local: LocalDescription,
        expectedPath: SelectedPath
    ) async throws -> NativeRemoteNoiseChannel {
        let answer = try await signaling.offerRendezvous(
            for: record,
            candidates: local.candidates.map(Self.signalingCandidate),
            iceUfrag: local.credentials.ufrag,
            icePwd: local.credentials.password
        )
        guard let remoteUfrag = answer.iceUfrag, let remotePassword = answer.icePwd else {
            throw ControlPlaneError.malformedResponse("The Mac did not return ICE credentials.")
        }
        let selected: SelectedPath
        do {
            selected = try await transport.connect(
                remote: RemoteDescription(
                    credentials: IceCredentials(ufrag: remoteUfrag, password: remotePassword),
                    candidates: try answer.candidates.map(Self.nativeCandidate)
                ),
                role: .initiator
            )
        } catch {
            throw DirectConnectivityFailure(underlying: error)
        }
        guard selected == expectedPath || (expectedPath == .direct && selected == .direct) else {
            try? await transport.close()
            throw ControlPlaneError.transport("The ICE agent selected a path outside the direct-first policy.")
        }
        await record(path: selected)
        return NativeRemoteNoiseChannel(
            transport: transport,
            pathObserver: { [weak self] path in await self?.record(path: path) }
        )
    }

    private func record(path: SelectedPath) async {
        let policyPath: RemoteTransportPath = path == .relay ? .relay : .direct
        let changed = await policy.recordSelectedPath(policyPath)
        if changed, let pathChangeHandler { await pathChangeHandler() }
    }

    private static func credentials() -> IceCredentials {
        // UUID entropy is produced by the system CSPRNG. The strings satisfy
        // RFC 8445 and the control plane's explicit length/character bounds.
        IceCredentials(
            ufrag: UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased(),
            password: (UUID().uuidString + UUID().uuidString)
                .replacingOccurrences(of: "-", with: "")
                .lowercased()
        )
    }

    private static func nativeServers(_ server: LatchMobileKit.IceServer) -> [IceServer] {
        server.urls.map {
            IceServer(
                url: $0,
                username: server.username ?? "",
                credential: server.credential ?? ""
            )
        }
    }

    private static func signalingCandidate(_ candidate: TransportCandidate) -> LatchMobileKit.TransportCandidate {
        LatchMobileKit.TransportCandidate(
            address: candidate.address,
            expiresAt: UInt64(Date().timeIntervalSince1970) + SignalingWindows.rendezvousTTL,
            type: candidate.candidateType,
            priority: candidate.priority,
            foundation: candidate.foundation,
            component: Int(candidate.component),
            protocol: candidate.protocol,
            relatedAddress: candidate.relatedAddress,
            relatedPort: candidate.relatedPort.map(Int.init),
            tcpType: candidate.tcpType
        )
    }

    private static func nativeCandidate(_ candidate: LatchMobileKit.TransportCandidate) throws -> TransportCandidate {
        guard let type = candidate.type,
              let priority = candidate.priority,
              let foundation = candidate.foundation,
              let component = candidate.component,
              let proto = candidate.protocol,
              let component16 = UInt16(exactly: component)
        else {
            throw ControlPlaneError.malformedResponse("The Mac returned an incomplete ICE candidate.")
        }
        return TransportCandidate(
            candidateType: type,
            priority: priority,
            foundation: foundation,
            component: component16,
            protocol: proto,
            address: candidate.address,
            relatedAddress: candidate.relatedAddress,
            relatedPort: candidate.relatedPort.flatMap(UInt16.init(exactly:)),
            tcpType: candidate.tcpType
        )
    }
}

private struct DirectConnectivityFailure: Error {
    let underlying: any Error
}

private final class NativeRemoteNoiseChannel: RemoteNoiseChannel, @unchecked Sendable {
    private let transport: RemoteTransport
    private let pathObserver: @Sendable (SelectedPath) async -> Void

    init(
        transport: RemoteTransport,
        pathObserver: @escaping @Sendable (SelectedPath) async -> Void
    ) {
        self.transport = transport
        self.pathObserver = pathObserver
    }

    func readFrame() async throws -> Data {
        let data = try await transport.read()
        try await observePath()
        return data
    }

    func writeFrame(_ frame: Data) async throws {
        try await transport.write(record: frame)
        try await observePath()
    }

    func close() async {
        try? await transport.close()
    }

    private func observePath() async throws {
        await pathObserver(try await transport.selectedPath())
    }
}

public enum NativePairedGatewayFactory {
    /// Production paired path used by the iOS app. The manual HTTPS path and
    /// Bonjour/TCP fallback remain in LatchMobileKit and are not replaced.
    public static func make(
        identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore()
    ) -> @Sendable (PairedDeviceRecord) async throws -> LatchGateway {
        { record in
            let lanTarget = try await BonjourMacDiscovery()
                .candidates(matching: record.mac.publicKey)
                .first
            let lanFallback = lanTarget.map(LANRemoteNoiseChannelProvider.init)
            guard let controlPlane = record.controlPlane else {
                guard let lanTarget else { throw NoiseTunnelError.macNotReachable }
                let transport = try await NoiseTunnelGatewayTransport.start(
                    target: lanTarget,
                    pairedDevice: record,
                    identityStore: identityStore
                )
                return LatchGateway(transport: transport)
            }
            let signaling = HTTPControlPlaneClient(baseURL: controlPlane)
            let rediscovery = GatewayRediscovery()
            let provider = NativeRemoteChannelProvider(
                record: record,
                signaling: signaling,
                lanFallback: lanFallback,
                pathChangeHandler: { await rediscovery.run() }
            )
            let transport = try await NoiseTunnelGatewayTransport.start(
                channelProvider: provider,
                pairedDevice: record,
                identityStore: identityStore
            )
            let gateway = LatchGateway(transport: transport)
            await rediscovery.install(gateway)
            return gateway
        }
    }
}

private actor GatewayRediscovery {
    private var gateway: LatchGateway?

    func install(_ gateway: LatchGateway) {
        self.gateway = gateway
    }

    /// A selected-path change invalidates and completes discovery before the
    /// provider releases the triggering channel to application traffic.
    func run() async {
        guard let gateway else { return }
        await gateway.invalidateDiscovery()
        _ = try? await gateway.discover()
    }
}

private extension NSLock {
    func withLock<T>(_ body: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try body()
    }
}
