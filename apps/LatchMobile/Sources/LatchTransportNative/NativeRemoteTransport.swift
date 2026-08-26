import Foundation
import LatchMobileKit

/// Opens one shared-Rust data channel for each loopback request.
///
/// The first attempt receives STUN from `GET /v1/ice-servers` and, when the
/// account allows relay, TURN from `POST /v1/turn-credentials` requested at
/// the same time. Preferring a direct path is ICE's own job: host and
/// reflexive candidates outrank relay candidates in the pair priority
/// formula. Withholding TURN until a direct attempt failed never made direct
/// more likely — it only charged every phone behind a symmetric NAT or a
/// UDP-blocking network a full failed attempt before the relay one could
/// start. A phone whose account has relay disabled gets a 403 here and runs
/// direct-only; the refusal is the control plane's, not this type's.
///
/// Choosing between this and the local network is not this type's job: the
/// route in `PairedGatewayRoute` tries Bonjour first and only reaches here on
/// a miss or a failed LAN connect.
public final class NativeRemoteChannelProvider: RemoteNoiseChannelProvider, @unchecked Sendable {
    private let record: PairedDeviceRecord
    private let signaling: any SignalingClient
    private let pathReporter: RemotePathReporter
    private let policy = RemoteTransportPolicy()
    private let iceConfiguration = IceConfiguration()
    private var pathChangeHandler: (@Sendable () async -> Void)?

    public convenience init(context: RemoteChannelContext) {
        self.init(
            record: context.record,
            signaling: context.signaling,
            pathReporter: context.pathReporter,
            pathChangeHandler: context.onPathChange
        )
    }

    public init(
        record: PairedDeviceRecord,
        signaling: any SignalingClient,
        pathReporter: RemotePathReporter = RemotePathReporter(),
        pathChangeHandler: (@Sendable () async -> Void)? = nil
    ) {
        self.record = record
        self.signaling = signaling
        self.pathReporter = pathReporter
        self.pathChangeHandler = pathChangeHandler
    }

    public func openChannel() async throws -> any RemoteNoiseChannel {
        async let stunTask = iceConfiguration.stun {
            try await self.signaling.iceServers(for: self.record)
        }
        async let relayTask = iceConfiguration.relay {
            try await self.signaling.turnCredentials(for: self.record)
        }
        let stun = try await stunTask
        let relay = await relayTask
        let transport = try await RemoteTransport.gather(
            credentials: Self.credentials(),
            servers: Self.nativeServers(stun + relay.servers)
        )
        do {
            return try await connect(transport: transport, local: transport.localDescription())
        } catch let failure as ConnectivityFailure {
            // The only retry left is the one relay was meant to cover: the
            // first attempt ran direct-only because credential issuance was
            // unavailable. A refusal is not retried — the control plane has
            // already said no, and asking again just spends a round trip on a
            // path that is already failing.
            guard relay.servers.isEmpty, !relay.refused, transport.connectivityFailed() else {
                throw failure.underlying
            }
            let retry = await iceConfiguration.relay {
                try await self.signaling.turnCredentials(for: self.record)
            }
            guard !retry.servers.isEmpty else { throw failure.underlying }
            try await policy.authorizeRelayAttempt(servers: retry.servers)
            let local = try await transport.retryWithRelay(
                servers: Self.nativeServers(stun + retry.servers)
            )
            do {
                return try await connect(transport: transport, local: local)
            } catch let failure as ConnectivityFailure {
                throw failure.underlying
            }
        }
    }

    private func connect(
        transport: RemoteTransport,
        local: LocalDescription
    ) async throws -> NativeRemoteNoiseChannel {
        let candidates = LatchMobileKit.TransportCandidate.preferredForPublication(
            local.candidates.map(Self.signalingCandidate)
        )
        let answer = try await signaling.offerRendezvous(
            for: record,
            candidates: candidates,
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
            throw ConnectivityFailure(underlying: error)
        }
        // Whichever pair ICE nominated is the answer: relay is a legitimate
        // outcome of the first attempt now, and the same pinned Noise session
        // runs over it either way. The path is reported, not judged.
        await record(path: selected)
        return NativeRemoteNoiseChannel(
            transport: transport,
            pathObserver: { [weak self] path in await self?.record(path: path) }
        )
    }

    private func record(path: SelectedPath) async {
        let policyPath: RemoteTransportPath = path == .relay ? .relay : .direct
        pathReporter.report(policyPath == .relay ? .relay : .direct)
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

    /// Flattens the control plane's entries into one URL-per-server list.
    ///
    /// Cloudflare returns its STUN URL inside the TURN entry as well, so the
    /// combined list arrives with duplicates. Handing the same URL to the
    /// agent twice gathers the same reflexive candidate twice, which costs a
    /// round trip and publishes a redundant candidate in a list capped at
    /// eight.
    private static func nativeServers(_ servers: [LatchMobileKit.IceServer]) -> [IceServer] {
        var seen: Set<String> = []
        return servers.flatMap { server in
            server.urls.compactMap { url in
                guard seen.insert(url).inserted else { return nil }
                return IceServer(
                    url: url,
                    username: server.username ?? "",
                    credential: server.credential ?? ""
                )
            }
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

private struct ConnectivityFailure: Error {
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
    /// Production paired path used by the iOS app: the local network first,
    /// then presence and ICE through the control plane. The manual HTTPS link
    /// remains a separate, coequal route in LatchMobileKit.
    public static func make(
        identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore(),
        pathReporter: RemotePathReporter = RemotePathReporter()
    ) -> @Sendable (PairedDeviceRecord) async throws -> LatchGateway {
        PairedGatewayRoute.factory(
            identityStore: identityStore,
            pathReporter: pathReporter,
            remoteProvider: { context in NativeRemoteChannelProvider(context: context) }
        )
    }
}
