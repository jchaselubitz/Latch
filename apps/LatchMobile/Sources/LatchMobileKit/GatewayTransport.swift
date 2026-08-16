import CryptoKit
import Foundation
import Network

public enum RemoteTransportPath: Sendable {
    case direct
    case relay
}

public enum RemoteTransportPolicyError: Error, Equatable, Sendable {
    case relayBeforeDirectFailure
}

/// Small platform-independent state machine mirrored by the Rust boundary.
/// Keeping it in the kit makes relay issuance and capability invalidation
/// testable without an XCFramework or simulator.
public actor RemoteTransportPolicy {
    private var directFailed = false
    private var selectedPath: RemoteTransportPath?

    public init() {}

    public func recordDirectFailure() {
        directFailed = true
    }

    public func authorizeRelayRetry() throws {
        guard directFailed else {
            throw RemoteTransportPolicyError.relayBeforeDirectFailure
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
/// `LatchGateway` and `EventStream` deliberately stay HTTP/WebSocket clients.
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

/// The control-plane-free Bonjour/TCP provider retained alongside ICE.
public struct LANRemoteNoiseChannelProvider: RemoteNoiseChannelProvider, @unchecked Sendable {
    private let target: NoiseTunnelTarget
    private let queue = DispatchQueue(label: "dev.cooperativ.latch.lan-noise-channel")

    public init(target: NoiseTunnelTarget) {
        self.target = target
    }

    public func openChannel() async throws -> any RemoteNoiseChannel {
        let connection = NWConnection(to: target.endpoint, using: .tcp)
        connection.start(queue: queue)
        return NWConnectionNoiseChannel(connection: connection)
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

/// A listener-backed paired transport.
///
/// There is one fresh Noise session for every inbound loopback connection.
/// That mirrors the Mac's `authorize_and_inject` boundary: ordinary HTTP
/// requests are one authorized operation and the sole long-lived exception is
/// the WebSocket connection URLSession itself keeps open.
public final class NoiseTunnelGatewayTransport: GatewayTransport, @unchecked Sendable {
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

        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(
            host: .ipv4(IPv4Address("127.0.0.1")!),
            port: .any
        )
        let listener = try NWListener(using: parameters, on: .any)
        try await waitUntilReady(listener)
        guard let port = listener.port else { throw NoiseTunnelError.listenerUnavailable }
        let link = GatewayLink(
            url: URL(string: "http://127.0.0.1:\(port.rawValue)")!,
            token: ""
        )
        let transport = NoiseTunnelGatewayTransport(
            listener: listener,
            target: target,
            channelProvider: nil,
            staticKey: staticKey,
            pinnedMacPublicKey: pin,
            gatewayLink: link
        )
        listener.newConnectionHandler = { [weak transport] connection in
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
        transport.installHandler()
        return transport
    }

    /// Stops accepting new loopback requests and closes the local listener.
    /// iOS suspension tears these sockets down too; callers create a fresh
    /// transport on resume rather than treating an old link as live.
    public func stop() {
        listener.cancel()
    }

    private static func makeLoopbackListener() async throws -> (listener: NWListener, link: GatewayLink) {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(
            host: .ipv4(IPv4Address("127.0.0.1")!),
            port: .any
        )
        let listener = try NWListener(using: parameters, on: .any)
        try await waitUntilReady(listener)
        guard let port = listener.port else { throw NoiseTunnelError.listenerUnavailable }
        return (
            listener,
            GatewayLink(url: URL(string: "http://127.0.0.1:\(port.rawValue)")!, token: "")
        )
    }

    private func installHandler() {
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else {
                connection.cancel()
                return
            }
            Task { await self.accept(connection) }
        }
    }

    private static func waitUntilReady(_ listener: NWListener) async throws {
        try await withCheckedThrowingContinuation { continuation in
            var completed = false
            listener.stateUpdateHandler = { state in
                guard !completed else { return }
                switch state {
                case .ready:
                    completed = true
                    continuation.resume(returning: ())
                case .failed:
                    completed = true
                    continuation.resume(throwing: NoiseTunnelError.listenerUnavailable)
                case .cancelled:
                    completed = true
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
        let body = reason + "\n"
        let response = "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: \(body.utf8.count)\r\n\r\n\(body)"
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

/// Bonjour is only a reachability optimization. A service must advertise the
/// paired Mac's identity key before it is offered as a candidate, and the
/// subsequent Noise handshake still verifies that exact key cryptographically.
public final class BonjourMacDiscovery: @unchecked Sendable {
    public static let serviceType = "_latch-remote._tcp"

    public init() {}

    /// Browses briefly rather than returning on the browser's initial empty
    /// result set. Bonjour commonly reports that empty set before multicast
    /// responses arrive, and treating it as a completed search would turn a
    /// discoverable Mac into a misleading offline result.
    public func candidates(
        matching pinnedMacPublicKey: String,
        for duration: Duration = .seconds(2)
    ) async throws -> [NoiseTunnelTarget] {
        let pin = try NoiseXX.normalizedPin(pinnedMacPublicKey)
        let parameters = NWParameters.tcp
        parameters.includePeerToPeer = true
        let browser = NWBrowser(for: .bonjour(type: Self.serviceType, domain: nil), using: parameters)
        let collector = BonjourCandidateCollector()
        browser.browseResultsChangedHandler = { results, _ in
            let targets = results.compactMap { result -> NoiseTunnelTarget? in
                guard Self.identityKey(in: result) == pin else { return nil }
                return NoiseTunnelTarget(endpoint: result.endpoint)
            }
            Task { await collector.replace(with: targets) }
        }
        browser.start(queue: DispatchQueue(label: "dev.cooperativ.latch.bonjour"))
        defer { browser.cancel() }
        try? await Task.sleep(for: duration)
        return await collector.values
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
