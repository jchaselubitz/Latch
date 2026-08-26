import CryptoKit
import Foundation
import Network

public enum RemoteTransportPath: Sendable {
    case direct
    case relay
}

public enum RemoteTransportPolicyError: Error, Equatable, Sendable {
    /// A relay attempt was assembled without a relay server to attempt it with.
    case missingTurnServer
}

/// Small platform-independent state machine mirrored by the Rust boundary.
/// Keeping it in the kit makes relay issuance and capability invalidation
/// testable without an XCFramework or simulator.
///
/// Relay servers are allowed into the first attempt. Preferring a direct path
/// is ICE's own job — host and reflexive candidates outrank relay candidates
/// in the pair priority formula — so withholding TURN never made direct more
/// likely, it only guaranteed a second round trip for every phone that could
/// not get there directly. Refusing relay outright is still possible, but it
/// is enforced where the credentials are minted: the control plane returns 403
/// for an account with relay disabled, and this policy never manufactures a
/// server the control plane declined to issue.
public actor RemoteTransportPolicy {
    private var selectedPath: RemoteTransportPath?

    public init() {}

    /// Checks that a relay attempt actually has a relay server behind it.
    ///
    /// This is the recovery path for an attempt that ran direct-only because
    /// credential issuance failed, not a gate on issuance itself.
    public func authorizeRelayAttempt(servers: [IceServer]) throws {
        guard servers.contains(where: \.isTurn) else {
            throw RemoteTransportPolicyError.missingTurnServer
        }
    }

    /// Returns true exactly when capabilities must be rediscovered.
    public func recordSelectedPath(_ path: RemoteTransportPath) -> Bool {
        defer { selectedPath = path }
        return selectedPath.map { $0 != path } ?? false
    }
}

/// The mechanism that makes the gateway URL usable.
///
/// Gateway clients deliberately stay HTTP/WebSocket clients.
/// A manual link supplies an HTTPS (or local HTTP) URL directly; the paired
/// route supplies a private loopback URL whose listener carries bytes through
/// a pinned Noise session. Keeping this seam here prevents two implementations
/// of the gateway protocol and its WebSocket framing.
public protocol GatewayTransport: Sendable {
    var gatewayLink: GatewayLink { get }
}

/// The pre-existing `latch serve` path a person configures in Settings.
public struct HTTPSGatewayTransport: GatewayTransport, Sendable {
    public let gatewayLink: GatewayLink

    public init(link: GatewayLink) {
        gatewayLink = link
    }
}

/// A TCP target for the Mac's authenticated remote listener.
///
/// This is intentionally a transport address, never a gateway URL or
/// credential. The gateway itself remains loopback-only on the Mac.
public struct NoiseTunnelTarget: @unchecked Sendable {
    fileprivate let endpoint: NWEndpoint

    public init(host: String, port: UInt16) throws {
        let trimmed = host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, port != 0, let port = NWEndpoint.Port(rawValue: port) else {
            throw NoiseTunnelError.invalidTarget
        }
        endpoint = .hostPort(host: NWEndpoint.Host(trimmed), port: port)
    }

    fileprivate init(endpoint: NWEndpoint) {
        self.endpoint = endpoint
    }
}

/// One reliable ordered record channel produced by the shared Rust core.
///
/// Each channel carries exactly one Noise session. The Rust layer owns ICE,
/// DTLS, SCTP, consent freshness, and path selection; this Swift layer still
/// owns the Noise handshake and verifies the pairing-record pin.
public protocol RemoteNoiseChannel: NoiseFrameChannel {
    func close() async
}

/// Opens a fresh remote record channel for one loopback HTTP/WebSocket socket.
public protocol RemoteNoiseChannelProvider: Sendable {
    func openChannel() async throws -> any RemoteNoiseChannel
}

/// A plain TCP channel to the Mac's authenticated listener.
///
/// Two routes share it: a Bonjour result on the local network, and a host
/// address the Mac published as presence — which is how a tailnet works with no
/// ICE involved at all, because a Tailscale address is just another interface
/// the listener is bound on. Nothing is trusted more for having come from one
/// of those than the other; the pinned Noise handshake runs over both.
///
/// `openChannel` waits for the connection to be established rather than
/// returning an optimistic one. A caller that falls through to another target
/// needs to learn here that this one failed: handing back a channel whose
/// connect is still in flight moves the failure into the Noise handshake, where
/// there is no other target left to try.
public struct LANRemoteNoiseChannelProvider: RemoteNoiseChannelProvider, @unchecked Sendable {
    /// Long enough for a tailnet address that has to bring a tunnel up, short
    /// enough that a list of interface addresses — most of which this phone
    /// cannot route to — does not take a minute to walk.
    public static let defaultConnectTimeout = Duration.seconds(4)

    private let target: NoiseTunnelTarget
    private let connectTimeout: Duration
    private let queue = DispatchQueue(label: "dev.cooperativ.latch.lan-noise-channel")

    public init(
        target: NoiseTunnelTarget,
        connectTimeout: Duration = LANRemoteNoiseChannelProvider.defaultConnectTimeout
    ) {
        self.target = target
        self.connectTimeout = connectTimeout
    }

    public func openChannel() async throws -> any RemoteNoiseChannel {
        let connection = NWConnection(to: target.endpoint, using: .tcp)
        do {
            try await Self.start(connection, on: queue, within: connectTimeout)
        } catch {
            connection.cancel()
            throw error
        }
        return NWConnectionNoiseChannel(connection: connection)
    }

    private static func start(
        _ connection: NWConnection,
        on queue: DispatchQueue,
        within timeout: Duration
    ) async throws {
        let completion = ContinuationGuard()
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    connection.stateUpdateHandler = { state in
                        switch state {
                        case .ready:
                            guard completion.markCompleted() else { return }
                            continuation.resume()
                        case .failed(let error):
                            guard completion.markCompleted() else { return }
                            continuation.resume(throwing: NoiseError.transport(error.localizedDescription))
                        case .cancelled:
                            guard completion.markCompleted() else { return }
                            continuation.resume(throwing: NoiseTunnelError.macNotReachable)
                        default:
                            break
                        }
                    }
                    connection.start(queue: queue)
                }
            }
            group.addTask {
                try await Task.sleep(for: timeout)
                // Cancelling drives the handler above, so the waiting task is
                // resumed by the same path a real failure takes rather than by
                // a second continuation nobody owns.
                connection.cancel()
                throw NoiseTunnelError.macNotReachable
            }
            defer { group.cancelAll() }
            try await group.next()
        }
    }
}

/// Installs an `NWListener` handler before the listener starts, while allowing
/// the transport that owns accepted connections to be created after the
/// listener has selected its ephemeral port.
///
/// Network.framework requires a connection handler at `start()` time. The
/// gateway link cannot be constructed until the listener becomes ready and
/// exposes that port, so this router bridges the initialization cycle without
/// dropping an early connection or starting the listener in an invalid state.
final class DeferredConnectionHandler<Connection>: @unchecked Sendable {
    typealias Handler = @Sendable (Connection) -> Void

    private let lock = NSLock()
    private var pending: [Connection] = []
    private var handler: Handler?

    func receive(_ connection: Connection) {
        lock.lock()
        guard let handler else {
            pending.append(connection)
            lock.unlock()
            return
        }
        lock.unlock()
        handler(connection)
    }

    func install(_ handler: @escaping Handler) {
        lock.lock()
        precondition(self.handler == nil, "the deferred connection handler was installed twice")
        self.handler = handler
        let queued = pending
        pending.removeAll(keepingCapacity: false)
        lock.unlock()

        queued.forEach(handler)
    }
}

/// User-facing local errors from the paired tunnel.
public enum NoiseTunnelError: Error, Equatable, Sendable, LocalizedError {
    case invalidTarget
    case macNotReachable
    case listenerUnavailable
    case callerSuppliedCredential
    case malformedRequest
    case requestHeaderTooLarge
    case closed

    public var errorDescription: String? { message }

    public var message: String {
        switch self {
        case .invalidTarget:
            return "This Mac did not provide a usable remote-access address."
        case .macNotReachable:
            return "This Mac is not reachable on this network."
        case .listenerUnavailable:
            return "The phone could not start its local secure connection."
        case .callerSuppliedCredential:
            return "This secure connection refuses credentials supplied by the phone."
        case .malformedRequest:
            return "The local secure connection received an incomplete HTTP request."
        case .requestHeaderTooLarge:
            return "The local secure connection received an HTTP request header that is too large."
        case .closed:
            return "The secure connection closed before the request completed."
        }
    }
}

/// Guards a continuation against being resumed twice. `stateUpdateHandler`
/// runs serially on the queue passed to `start`, but that guarantee is not
/// visible to the Swift 6 concurrency checker, so the flag is kept behind
/// a lock rather than as a captured `var`.
final class ContinuationGuard: @unchecked Sendable {
    private let lock = NSLock()
    private var completed = false

    /// Returns `true` the first time it is called, `false` after that.
    func markCompleted() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !completed else { return false }
        completed = true
        return true
    }
}

/// A listener-backed paired transport.
///
/// There is one fresh Noise session for every inbound loopback connection.
/// That mirrors the Mac's `authorize_and_inject` boundary: ordinary HTTP
/// requests are one authorized operation and the sole long-lived exception is
/// the WebSocket connection URLSession itself keeps open.
public final class NoiseTunnelGatewayTransport: GatewayTransport, @unchecked Sendable {
    /// Marks a 502 the phone wrote itself. No gateway sends this code.
    static let tunnelFailureCode = "tunnel_unreachable"

    public let gatewayLink: GatewayLink

    private let listener: NWListener
    private let target: NoiseTunnelTarget?
    private let channelProvider: (any RemoteNoiseChannelProvider)?
    private let staticKey: Curve25519.KeyAgreement.PrivateKey
    private let pinnedMacPublicKey: String
    private let queue = DispatchQueue(label: "dev.cooperativ.latch.noise-tunnel")

    private init(
        listener: NWListener,
        target: NoiseTunnelTarget?,
        channelProvider: (any RemoteNoiseChannelProvider)?,
        staticKey: Curve25519.KeyAgreement.PrivateKey,
        pinnedMacPublicKey: String,
        gatewayLink: GatewayLink
    ) {
        self.listener = listener
        self.target = target
        self.channelProvider = channelProvider
        self.staticKey = staticKey
        self.pinnedMacPublicKey = pinnedMacPublicKey
        self.gatewayLink = gatewayLink
    }

    deinit {
        listener.cancel()
    }

    /// Starts a listener bound to `127.0.0.1` on an ephemeral port.
    ///
    /// The paired record supplies the pin; no rendezvous-provided identity or
    /// Bonjour TXT value is accepted as authority for the handshake.
    public static func start(
        target: NoiseTunnelTarget,
        pairedDevice: PairedDeviceRecord,
        identityStore: any DeviceIdentityStoring
    ) async throws -> NoiseTunnelGatewayTransport {
        guard pairedDevice.isActive else {
            throw NoiseTunnelError.closed
        }
        let staticKey = try identityStore.privateKey()
        let pin = try pairedDevice.pinnedMacPublicKey()
        let listenerAndLink = try await makeLoopbackListener()
        let transport = NoiseTunnelGatewayTransport(
            listener: listenerAndLink.listener,
            target: target,
            channelProvider: nil,
            staticKey: staticKey,
            pinnedMacPublicKey: pin,
            gatewayLink: listenerAndLink.link
        )
        listenerAndLink.router.install { [weak transport] connection in
            guard let transport else {
                connection.cancel()
                return
            }
            Task { await transport.accept(connection) }
        }
        // The loopback URL is not exposed until after this handler is set, so
        // the app cannot race its own first URLSession request.
        return transport
    }

    /// Starts the same loopback shim over the shared Rust ICE transport.
    ///
    /// The provider is asked for a fresh reliable ordered channel per inbound
    /// connection, preserving the Mac's one-authorized-request boundary.
    public static func start(
        channelProvider: any RemoteNoiseChannelProvider,
        pairedDevice: PairedDeviceRecord,
        identityStore: any DeviceIdentityStoring
    ) async throws -> NoiseTunnelGatewayTransport {
        guard pairedDevice.isActive else { throw NoiseTunnelError.closed }
        let staticKey = try identityStore.privateKey()
        let pin = try pairedDevice.pinnedMacPublicKey()
        let listenerAndLink = try await makeLoopbackListener()
        let transport = NoiseTunnelGatewayTransport(
            listener: listenerAndLink.listener,
            target: nil,
            channelProvider: channelProvider,
            staticKey: staticKey,
            pinnedMacPublicKey: pin,
            gatewayLink: listenerAndLink.link
        )
        listenerAndLink.router.install { [weak transport] connection in
            guard let transport else {
                connection.cancel()
                return
            }
            Task { await transport.accept(connection) }
        }
        return transport
    }

    /// Stops accepting new loopback requests and closes the local listener.
    /// iOS suspension tears these sockets down too; callers create a fresh
    /// transport on resume rather than treating an old link as live.
    public func stop() {
        listener.cancel()
    }

    private static func makeLoopbackListener() async throws -> (
        listener: NWListener,
        link: GatewayLink,
        router: DeferredConnectionHandler<NWConnection>
    ) {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(
            host: .ipv4(IPv4Address("127.0.0.1")!),
            port: .any
        )
        let listener = try NWListener(using: parameters, on: .any)
        let router = DeferredConnectionHandler<NWConnection>()
        // Network.framework requires this before `start()`. The transport is
        // installed into the router immediately after the selected port is
        // available and before the loopback URL is exposed to URLSession.
        listener.newConnectionHandler = { connection in router.receive(connection) }
        try await waitUntilReady(listener)
        guard let port = listener.port else { throw NoiseTunnelError.listenerUnavailable }
        return (
            listener,
            GatewayLink(url: URL(string: "http://127.0.0.1:\(port.rawValue)")!, token: ""),
            router
        )
    }

    private static func waitUntilReady(_ listener: NWListener) async throws {
        try await withCheckedThrowingContinuation { continuation in
            let completion = ContinuationGuard()
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    guard completion.markCompleted() else { return }
                    continuation.resume(returning: ())
                case .failed:
                    guard completion.markCompleted() else { return }
                    continuation.resume(throwing: NoiseTunnelError.listenerUnavailable)
                case .cancelled:
                    guard completion.markCompleted() else { return }
                    continuation.resume(throwing: NoiseTunnelError.listenerUnavailable)
                default:
                    break
                }
            }
            listener.start(queue: DispatchQueue(label: "dev.cooperativ.latch.noise-listener"))
        }
    }


    private func accept(_ loopback: NWConnection) async {
        loopback.start(queue: queue)
        do {
            var validator = TunnelRequestValidator()
            while true {
                let bytes = try await loopback.receiveData()
                if let firstRequest = try validator.append(bytes) {
                    try await bridge(loopback: loopback, firstRequest: firstRequest)
                    return
                }
            }
        } catch let error as NoiseTunnelError {
            await reject(loopback, reason: error.message)
        } catch let error as NoiseError {
            await reject(loopback, reason: error.message)
        } catch {
            await reject(loopback, reason: error.localizedDescription)
        }
    }

    private func bridge(loopback: NWConnection, firstRequest: Data) async throws {
        let channel: any RemoteNoiseChannel
        if let channelProvider {
            channel = try await channelProvider.openChannel()
        } else if let target {
            let remote = NWConnection(to: target.endpoint, using: .tcp)
            remote.start(queue: queue)
            channel = NWConnectionNoiseChannel(connection: remote)
        } else {
            throw NoiseTunnelError.invalidTarget
        }
        defer {
            Task { await channel.close() }
            loopback.cancel()
        }
        let noise = try await NoiseXX.connect(
            channel: channel,
            staticKey: staticKey,
            pinnedPeerPublicKey: pinnedMacPublicKey
        )

        // The two tasks each own a Noise direction. NoiseSession serializes
        // each cipher state internally, but no task is allowed to hold a
        // transport lock while waiting on socket I/O.
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask {
                try await Self.copyLoopbackToNoise(
                    loopback,
                    firstRequest: firstRequest,
                    channel: channel,
                    session: noise
                )
            }
            group.addTask {
                try await Self.copyNoiseToLoopback(loopback, channel: channel, session: noise)
            }
            do {
                _ = try await group.next()
            } catch {
                // Task cancellation does not itself interrupt an outstanding
                // Network receive. Closing both sockets does, so the sibling
                // exits promptly instead of keeping a completed HTTP request
                // or WebSocket teardown alive indefinitely.
                await channel.close()
                loopback.cancel()
                group.cancelAll()
                throw error
            }
            await channel.close()
            loopback.cancel()
            group.cancelAll()
        }
    }

    private static func copyLoopbackToNoise(
        _ loopback: NWConnection,
        firstRequest: Data,
        channel: any NoiseFrameChannel,
        session: NoiseSession
    ) async throws {
        try await channel.writeFrame(try session.encrypt(firstRequest))
        while !Task.isCancelled {
            let bytes = try await loopback.receiveData()
            try await channel.writeFrame(try session.encrypt(bytes))
        }
    }

    private static func copyNoiseToLoopback(
        _ loopback: NWConnection,
        channel: any NoiseFrameChannel,
        session: NoiseSession
    ) async throws {
        while !Task.isCancelled {
            let ciphertext = try await channel.readFrame()
            try await loopback.sendData(try session.decrypt(ciphertext))
        }
    }

    private func reject(_ connection: NWConnection, reason: String) async {
        // This is intentionally a response rather than silently stripping a
        // header: URLSession gets a useful local failure and a future caller
        // cannot accidentally leak a credential into the paired transport.
        //
        // It is sent in the gateway's own error shape, tagged with a code no
        // gateway uses, so the client can tell "this phone could not reach
        // your Mac" from "your Mac answered with an error" and show the
        // sentence rather than a status line.
        let body = (try? JSONSerialization.data(withJSONObject: [
            "error": Self.tunnelFailureCode,
            "reason": reason
        ])).flatMap { String(data: $0, encoding: .utf8) } ?? reason
        let response = "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: \(body.utf8.count)\r\n\r\n\(body)"
        try? await connection.sendData(Data(response.utf8))
        connection.cancel()
    }
}

/// Buffers just enough of a loopback HTTP request to enforce the tunnel's
/// credential boundary before the first byte is sent to the Mac.
struct TunnelRequestValidator {
    private static let maxHeaderBytes = 64 * 1024
    private var pending = Data()
    private var accepted = false

    mutating func append(_ bytes: Data) throws -> Data? {
        guard !accepted else { return bytes }
        pending.append(bytes)
        guard pending.count <= Self.maxHeaderBytes else {
            throw NoiseTunnelError.requestHeaderTooLarge
        }
        guard let end = pending.range(of: Data("\r\n\r\n".utf8)) else { return nil }
        let header = pending[..<end.lowerBound]
        guard let text = String(data: header, encoding: .utf8) else {
            throw NoiseTunnelError.malformedRequest
        }
        let lines = text.components(separatedBy: "\r\n")
        guard !lines.isEmpty else { throw NoiseTunnelError.malformedRequest }
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = line[..<colon].trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if name == "authorization" || name == "proxy-authorization" {
                throw NoiseTunnelError.callerSuppliedCredential
            }
        }
        accepted = true
        defer { pending.removeAll(keepingCapacity: false) }
        return pending
    }
}

/// Frames a Network connection for the existing Noise implementation.
actor NWConnectionNoiseChannel: RemoteNoiseChannel {
    private let connection: NWConnection
    private var buffered = Data()

    init(connection: NWConnection) {
        self.connection = connection
    }

    func readFrame() async throws -> Data {
        while true {
            if let decoded = try NoiseFraming.decode(from: buffered) {
                buffered = decoded.rest
                return decoded.frame
            }
            buffered.append(try await connection.receiveData())
        }
    }

    func writeFrame(_ frame: Data) async throws {
        try await connection.sendData(try NoiseFraming.encode(frame))
    }

    func close() {
        connection.cancel()
    }
}

private extension NWConnection {
    func receiveData(maximumLength: Int = NoiseWire.maxRecord) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            receive(
                minimumIncompleteLength: 1,
                maximumLength: maximumLength
            ) { data, _, complete, error in
                if let error {
                    continuation.resume(throwing: NoiseError.transport(error.localizedDescription))
                } else if let data, !data.isEmpty {
                    continuation.resume(returning: data)
                } else if complete {
                    continuation.resume(throwing: NoiseTunnelError.closed)
                } else {
                    continuation.resume(throwing: NoiseTunnelError.closed)
                }
            }
        }
    }

    func sendData(_ data: Data) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            send(content: data, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: NoiseError.transport(error.localizedDescription))
                } else {
                    continuation.resume()
                }
            })
        }
    }
}

/// Bonjour is only a reachability optimization. A matching identity TXT hint
/// is preferred and an explicit mismatch is ignored, but iOS may initially
/// surface a result before its TXT metadata. Such an unknown result is still
/// safe to try because the Noise handshake verifies the paired key before any
/// application bytes are forwarded.
public final class BonjourMacDiscovery: @unchecked Sendable {
    public static let serviceType = "_latch-remote._tcp"

    public init() {}

    /// Browses briefly rather than returning on the browser's initial empty
    /// result set. Bonjour commonly reports that empty set before multicast
    /// responses arrive, and treating it as a completed search would turn a
    /// discoverable Mac into a misleading offline result.
    public func candidates(
        matching pinnedMacPublicKey: String,
        for duration: Duration = .seconds(5)
    ) async throws -> [NoiseTunnelTarget] {
        let pin = try NoiseXX.normalizedPin(pinnedMacPublicKey)
        let parameters = NWParameters.tcp
        parameters.includePeerToPeer = true
        let browser = NWBrowser(for: .bonjour(type: Self.serviceType, domain: nil), using: parameters)
        let collector = BonjourCandidateCollector()
        browser.browseResultsChangedHandler = { results, _ in
            let classified = results.map { ($0, Self.identityKey(in: $0)) }
            let targets = classified
                .filter { Self.shouldAttempt(advertisedIdentityKey: $0.1, normalizedPin: pin) }
                .sorted { left, right in
                    // A cryptographically matching hint wins over an unknown
                    // one. Noise remains authoritative for both.
                    (left.1 == pin ? 0 : 1) < (right.1 == pin ? 0 : 1)
                }
                .map { NoiseTunnelTarget(endpoint: $0.0.endpoint) }
            Task { await collector.replace(with: targets) }
        }
        browser.start(queue: DispatchQueue(label: "dev.cooperativ.latch.bonjour"))
        defer { browser.cancel() }
        try? await Task.sleep(for: duration)
        return await collector.values
    }

    static func shouldAttempt(advertisedIdentityKey: String?, normalizedPin: String) -> Bool {
        advertisedIdentityKey == nil || advertisedIdentityKey == normalizedPin
    }

    private static func identityKey(in result: NWBrowser.Result) -> String? {
        guard case let .bonjour(txtRecord) = result.metadata,
              let key = txtRecord["identityKey"]
        else {
            return nil
        }
        return try? NoiseXX.normalizedPin(key)
    }
}

private actor BonjourCandidateCollector {
    private(set) var values: [NoiseTunnelTarget] = []

    func replace(with values: [NoiseTunnelTarget]) {
        self.values = values
    }
}
