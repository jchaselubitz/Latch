import Foundation
import LatchMobileKit

public struct NativeRemoteEnrollmentProvider: RemoteEnrollmentProviding {
    private let identityStore: any DeviceIdentityStoring

    public init(identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore()) {
        self.identityStore = identityStore
    }

    public func prepare(
        payload: PairingPayload,
        deviceName: String,
        permission: DevicePermission
    ) async throws -> any RemoteEnrollmentSession {
        try payload.checkLifetime()
        let key = try identityStore.privateKey()
        let privateKey = key.rawRepresentation
        let publicKey = key.publicKey.rawRepresentation
        let publicKeyHex = Self.hex(publicKey)
        let client = HTTPControlPlaneClient(baseURL: payload.controlPlane)
        let claim = try await client.claimRemoteEnrollment(
            enrollmentId: payload.enrollmentId,
            admissionCode: payload.admissionCode,
            name: deviceName,
            publicKey: publicKeyHex
        )
        guard claim.version == 1, claim.enrollmentId == payload.enrollmentId else {
            throw ControlPlaneError.malformedResponse("enrollment claim did not match the scanned code")
        }
        let link = try await RemoteLink.connectWss(
            url: claim.relayUrl.absoluteString,
            admission: claim.controllerAdmission,
            purpose: .enrollment,
            role: .controller,
            localPrivateKey: privateKey,
            localPublicKey: publicKey,
            expectedRemotePublicKey: try Self.bytes(payload.hostPublicKey),
            enrollmentId: payload.enrollmentId,
            enrollmentSecret: try Self.bytes(payload.enrollmentSecret),
            grantRevision: 0,
            peerWaitMs: 30_000
        )
        do {
            let stream = try await link.openService(service: .enrollment, grantRevision: 0)
            let proposal = EnrollmentProposalWire(
                enrollmentId: payload.enrollmentId,
                provisionalDeviceId: claim.provisionalDeviceId,
                controllerPublicKey: publicKeyHex,
                name: deviceName,
                permission: permission
            )
            var message = try JSONEncoder().encode(proposal)
            message.append(0x0a)
            try await stream.write(bytes: message)
            let comparison = try link.enrollmentComparison(
                enrollmentId: payload.enrollmentId,
                controllerPublicKey: publicKey,
                permission: permission.rawValue
            )
            return NativeRemoteEnrollmentSession(
                link: link,
                stream: stream,
                client: client,
                payload: payload,
                claim: claim,
                deviceName: deviceName,
                controllerPublicKey: publicKeyHex,
                permission: permission,
                comparison: comparison
            )
        } catch {
            try? await link.close()
            throw error
        }
    }

    private struct EnrollmentProposalWire: Encodable {
        let type = "enrollment_proposal"
        let version = 1
        let enrollmentId: String
        let provisionalDeviceId: String
        let controllerPublicKey: String
        let name: String
        let permission: DevicePermission
    }

    fileprivate static func bytes(_ value: String) throws -> Data {
        guard value.count == 64 else {
            throw ControlPlaneError.malformedResponse("Remote Link key material is malformed.")
        }
        var bytes = Data(capacity: 32)
        var index = value.startIndex
        while index < value.endIndex {
            let next = value.index(index, offsetBy: 2)
            guard let byte = UInt8(value[index..<next], radix: 16) else {
                throw ControlPlaneError.malformedResponse("Remote Link key material is malformed.")
            }
            bytes.append(byte)
            index = next
        }
        return bytes
    }

    fileprivate static func hex(_ value: Data) -> String {
        value.map { String(format: "%02x", $0) }.joined()
    }
}

private final class NativeRemoteEnrollmentSession: RemoteEnrollmentSession, @unchecked Sendable {
    private struct Receipt: Decodable {
        let type: String
        let version: Int
        let enrollmentId: String
        let hostPublicKey: String
        let controllerPublicKey: String
        let permission: DevicePermission
        let grantRevision: UInt64
    }

    let comparison: String
    private let link: RemoteLink
    private let stream: RemoteStream
    private let client: HTTPControlPlaneClient
    private let payload: PairingPayload
    private let claim: RemoteEnrollmentClaim
    private let deviceName: String
    private let controllerPublicKey: String
    private let permission: DevicePermission

    init(
        link: RemoteLink,
        stream: RemoteStream,
        client: HTTPControlPlaneClient,
        payload: PairingPayload,
        claim: RemoteEnrollmentClaim,
        deviceName: String,
        controllerPublicKey: String,
        permission: DevicePermission,
        comparison: String
    ) {
        self.link = link
        self.stream = stream
        self.client = client
        self.payload = payload
        self.claim = claim
        self.deviceName = deviceName
        self.controllerPublicKey = controllerPublicKey
        self.permission = permission
        self.comparison = comparison
    }

    func awaitApprovedRecord() async throws -> PairedDeviceRecord {
        let receipt: Receipt
        do {
            receipt = try await readReceipt()
            guard receipt.type == "pairing_approved", receipt.version == 1,
                  receipt.enrollmentId == payload.enrollmentId,
                  receipt.hostPublicKey == payload.hostPublicKey,
                  receipt.controllerPublicKey == controllerPublicKey,
                  receipt.permission == permission,
                  receipt.grantRevision > 0 else {
                throw ControlPlaneError.rejected("The encrypted enrollment receipt did not match this phone and grant.")
            }
            var descriptor: RemoteLinkDirectoryEntry?
            for attempt in 0..<12 {
                descriptor = try await client.remoteLinks(accessToken: claim.provisionalToken)
                    .first { $0.peerPublicKey == payload.hostPublicKey }
                if descriptor != nil { break }
                if attempt < 11 { try await Task.sleep(for: .milliseconds(250)) }
            }
            guard let descriptor,
                  descriptor.version == 1,
                  descriptor.permission == permission.rawValue,
                  descriptor.grantRevision == receipt.grantRevision else {
                throw ControlPlaneError.rejected("The current Remote Link directory does not match the encrypted receipt.")
            }
            try? await stream.close()
            try? await link.close()
            return PairedDeviceRecord(
                deviceId: claim.provisionalDeviceId,
                name: deviceName,
                devicePublicKey: controllerPublicKey,
                mac: PairedMac(
                    deviceId: descriptor.peerDeviceId,
                    publicKey: payload.hostPublicKey,
                    name: payload.macName
                ),
                permission: permission,
                comparison: comparison,
                controlPlane: payload.controlPlane,
                accessToken: claim.provisionalToken
            )
        } catch {
            try? await stream.close()
            try? await link.close()
            throw error
        }
    }

    func close() async {
        try? await stream.close()
        try? await link.close()
    }

    private func readReceipt() async throws -> Receipt {
        var buffer = Data()
        while buffer.count <= 16 * 1024 {
            let chunk = try await stream.read()
            guard !chunk.isEmpty else { throw RemoteLinkTransportError.closed }
            buffer.append(chunk)
            if let newline = buffer.firstIndex(of: 0x0a) {
                guard newline == buffer.index(before: buffer.endIndex) else {
                    throw ControlPlaneError.malformedResponse("enrollment receipt had trailing bytes")
                }
                return try JSONDecoder().decode(Receipt.self, from: buffer[..<newline])
            }
        }
        throw ControlPlaneError.malformedResponse("enrollment receipt exceeded its bound")
    }
}

/// Establishes one authenticated link over the shared Rust core: verifies the
/// current control-plane directory entry against the locally pinned Mac
/// identity, then tries the LAN entry point of the protocol before the relay,
/// under one owner. Lease renewal lives inside the connection so it dies with
/// the link.
public final class NativeRemoteLinkConnector: RemoteLinkConnecting, @unchecked Sendable {
    private let identityStore: any DeviceIdentityStoring
    private let pathReporter: RemotePathReporter
    private let signalingFactory: @Sendable (URL) -> any SignalingClient

    public init(
        identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore(),
        pathReporter: RemotePathReporter = RemotePathReporter(),
        signalingFactory: @escaping @Sendable (URL) -> any SignalingClient = { HTTPControlPlaneClient(baseURL: $0) }
    ) {
        self.identityStore = identityStore
        self.pathReporter = pathReporter
        self.signalingFactory = signalingFactory
    }

    public func connect(
        record: PairedDeviceRecord,
        options: RemoteLinkConnectOptions
    ) async throws -> any RemoteLinkConnection {
        let signaling = signalingFactory(record.controlPlane)
        let accessToken = try record.signalingAccessToken()
        let macDeviceID = try record.signalingMacDeviceId()
        LinkTrace.shared.mark("connector.directory.begin")
        let directory = try await signaling.remoteLinks(accessToken: accessToken)
        LinkTrace.shared.mark("connector.directory.end")
        guard let descriptor = directory.first(where: { $0.peerDeviceId == macDeviceID }),
              descriptor.version == 1,
              descriptor.peerPublicKey == record.mac.publicKey,
              descriptor.permission == record.permission.rawValue,
              descriptor.grantRevision > 0
        else {
            throw RemoteLinkFailure.revoked(
                "The Remote Link directory does not match this locally pinned pairing. Pair again from a new code on your Mac."
            )
        }
        let key = try identityStore.privateKey()
        let pin = try Self.bytes(record.mac.publicKey)
        let privateKey = key.rawRepresentation
        let publicKey = key.publicKey.rawRepresentation
        let revision = descriptor.grantRevision
        let peerWaitMs = UInt64(max(1, options.peerWait.components.seconds)) * 1000
        let pathReporter = self.pathReporter

        // LAN first, then the relay: not a race. A Rust connect cannot be
        // cancelled from Swift once it is in flight, and two links from the
        // same phone replace each other on the Mac, so a losing attempt that
        // finished its handshake a moment later would tear down the winner.
        // The browse returns on the first matching Mac and is bounded for the
        // "not here" case; that bound is the only cost off the LAN.
        if !options.skipLAN {
            let targets = await BonjourMacDiscovery()
                .remoteLinkTargets(matching: record.mac.publicKey, for: Self.lanBrowseWindow)
            LinkTrace.shared.mark(targets.isEmpty ? "connector.bonjour.none" : "connector.bonjour.found")
            if !targets.isEmpty, let link = await Self.connectFirstLanTarget(
                targets, privateKey: privateKey, publicKey: publicKey, pin: pin, revision: revision
            ) {
                pathReporter.report(.local)
                let stage = link.stageTimings()
                return NativeRemoteLinkConnection(
                    link: link, path: .local, revision: revision,
                    timings: LatchMobileKit.RemoteLinkStageTimings(
                        admissionMs: 0, connectMs: stage.connectMs,
                        peerWaitMs: stage.peerWaitMs, authenticateMs: stage.authenticateMs
                    ),
                    signaling: signaling, accessToken: accessToken
                )
            }
        }

        let result: NativeLinkAttempt
        do {
            let admissionStarted = Date()
            LinkTrace.shared.mark("connector.admission.begin")
            let admission = try await signaling.relayAdmission(for: record)
            LinkTrace.shared.mark("connector.admission.end")
            let admissionMs = UInt64(max(0, Date().timeIntervalSince(admissionStarted) * 1000))
            guard admission.version == 1 else {
                throw RemoteLinkFailure.transient("unsupported relay admission")
            }
            LinkTrace.shared.mark("connector.wss.begin")
            let link = try await RemoteLink.connectWss(
                url: admission.relayUrl.absoluteString, admission: admission.admission,
                purpose: .session, role: .controller,
                localPrivateKey: privateKey, localPublicKey: publicKey,
                expectedRemotePublicKey: pin, enrollmentId: nil, enrollmentSecret: nil,
                grantRevision: revision, peerWaitMs: peerWaitMs
            )
            LinkTrace.shared.mark("connector.wss.end")
            if Task.isCancelled {
                try? await link.close()
                result = .failed(RemoteLinkFailure.transient("cancelled"))
            } else {
                result = .connected(link, .relay, admissionMs: admissionMs)
            }
        } catch {
            result = .failed(Self.classify(error))
        }
        guard case let .connected(link, path, admissionMs) = result else {
            pathReporter.reportFailure()
            if case .failed(let failure) = result { throw failure }
            throw RemoteLinkFailure.transient("The Mac could not be reached.")
        }
        pathReporter.report(path)
        let stage = link.stageTimings()
        return NativeRemoteLinkConnection(
            link: link,
            path: path,
            revision: revision,
            timings: LatchMobileKit.RemoteLinkStageTimings(
                admissionMs: admissionMs,
                connectMs: stage.connectMs,
                peerWaitMs: stage.peerWaitMs,
                authenticateMs: stage.authenticateMs
            ),
            signaling: signaling,
            accessToken: accessToken
        )
    }

    /// How long the phone waits for a matching Mac to appear in the Bonjour
    /// browse before going to the relay. The browse returns early on the
    /// first match; this is the bound for the Mac not being on this network.
    static let lanBrowseWindow: Duration = .milliseconds(400)

    /// Tries the published LAN targets in order and keeps the first link that
    /// authenticates. Sequential on purpose: two LAN links to the same Mac
    /// would replace each other there.
    /// The whole LAN phase, across every published target, is bounded so a
    /// Mac whose LAN listener just moved (helper restart) or whose published
    /// addresses are stale costs at most this before the relay is tried.
    static let lanPhaseBudget: Duration = .milliseconds(1500)

    static func connectFirstLanTarget(
        _ targets: [RemoteLinkLanTarget], privateKey: Data, publicKey: Data, pin: Data, revision: UInt64
    ) async -> RemoteLink? {
        let deadline = ContinuousClock.now + lanPhaseBudget
        for target in targets.prefix(6) {
            if Task.isCancelled || ContinuousClock.now >= deadline { return nil }
            LinkTrace.shared.mark("connector.lan.begin")
            do {
                let link = try await RemoteLink.connectLan(
                    host: target.host, port: target.port, purpose: .session, role: .controller,
                    localPrivateKey: privateKey, localPublicKey: publicKey,
                    expectedRemotePublicKey: pin, enrollmentId: nil, enrollmentSecret: nil,
                    grantRevision: revision
                )
                LinkTrace.shared.mark("connector.lan.end")
                return link
            } catch {
                LinkTrace.shared.mark("connector.lan.failed")
                if case .authentication = Self.classify(error) { return nil }
            }
        }
        return nil
    }

    /// The failure worth reporting when both entry points failed: an
    /// authentication refusal beats "no LAN peer", and "Mac unavailable
    /// through the relay" beats a generic transport error.
    static func worst(of failures: [RemoteLinkFailure]) -> RemoteLinkFailure {
        if let authentication = failures.first(where: { if case .authentication = $0 { return true }; return false }) {
            return authentication
        }
        if let revoked = failures.first(where: { if case .revoked = $0 { return true }; return false }) {
            return revoked
        }
        if failures.contains(.macOffline) { return .macOffline }
        return failures.first ?? .transient("The Mac could not be reached.")
    }

    static func classify(_ error: Error) -> RemoteLinkFailure {
        if let failure = error as? RemoteLinkFailure { return failure }
        if let error = error as? TransportError {
            switch error {
            case .PeerUnavailable: return .macOffline
            case .Authentication(let message): return .authentication(message)
            case .Timeout: return .transient("The secure link timed out.")
            case .InvalidState: return .transient("The secure link is not ready.")
            case .Failure(let message): return .transient(message)
            }
        }
        if let error = error as? ControlPlaneError {
            switch error {
            case .rejected(let reason): return .revoked(reason)
            default: return .transient(error.message)
            }
        }
        return .transient(error.localizedDescription)
    }

    fileprivate static func bytes(_ value: String) throws -> Data {
        guard value.count == 64 else {
            throw ControlPlaneError.malformedResponse("The stored Mac key is malformed.")
        }
        var bytes = Data(capacity: 32)
        var index = value.startIndex
        while index < value.endIndex {
            let next = value.index(index, offsetBy: 2)
            guard let byte = UInt8(value[index..<next], radix: 16) else {
                throw ControlPlaneError.malformedResponse("The stored Mac key is malformed.")
            }
            bytes.append(byte)
            index = next
        }
        return bytes
    }
}

private enum NativeLinkAttempt: @unchecked Sendable {
    case connected(RemoteLink, RemotePath, admissionMs: UInt64)
    case failed(RemoteLinkFailure)
}

/// One authenticated link plus the relay lease renewal that keeps it admitted.
/// Closing cancels the renewal and the Rust task tree together.
private final class NativeRemoteLinkConnection: RemoteLinkConnection, @unchecked Sendable {
    let path: RemotePath
    let grantRevision: UInt64
    let timings: LatchMobileKit.RemoteLinkStageTimings
    private let link: RemoteLink
    private let leaseTask: Task<Void, Never>

    init(
        link: RemoteLink,
        path: RemotePath,
        revision: UInt64,
        timings: LatchMobileKit.RemoteLinkStageTimings,
        signaling: any SignalingClient,
        accessToken: String
    ) {
        self.link = link
        self.path = path
        self.grantRevision = revision
        self.timings = timings
        self.leaseTask = Task {
            while !Task.isCancelled, let event = await link.nextRelayEvent() {
                guard event.kind == "lease_started",
                      let leaseID = event.leaseId,
                      var expiry = event.expiresAt
                else { continue }
                while !Task.isCancelled {
                    let now = UInt64(Date().timeIntervalSince1970)
                    let delay = expiry > now + 300 ? expiry - now - 300 : 0
                    if delay > 0 {
                        try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                        if Task.isCancelled { return }
                    }
                    do {
                        let extensionValue = try await signaling.renewRelayLease(
                            leaseId: leaseID,
                            accessToken: accessToken
                        )
                        guard extensionValue.leaseId == leaseID else { return }
                        try await link.extendLease(claim: extensionValue.claim)
                        expiry = extensionValue.expiresAt
                    } catch {
                        // Fail closed at the already-enforced relay deadline.
                        return
                    }
                }
            }
        }
    }

    deinit {
        leaseTask.cancel()
        let link = link
        Task { try? await link.close() }
    }

    func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
        do {
            let stream = try await link.openService(service: .gateway, grantRevision: grantRevision)
            return NativeRemoteLinkChannel(stream: stream)
        } catch {
            throw NativeRemoteLinkConnector.classify(error)
        }
    }

    func waitClosed() async {
        await link.waitClosed()
    }

    func close() async {
        LinkTrace.shared.mark("link.close.begin")
        leaseTask.cancel()
        try? await link.close()
        LinkTrace.shared.mark("link.close.end")
    }
}

private final class NativeRemoteLinkChannel: AuthenticatedGatewayChannel, @unchecked Sendable {
    private let stream: RemoteStream

    init(stream: RemoteStream) { self.stream = stream }

    func read() async throws -> Data {
        let bytes = try await stream.read()
        guard !bytes.isEmpty else { throw RemoteLinkTransportError.closed }
        return bytes
    }

    func write(_ bytes: Data) async throws {
        try await stream.write(bytes: bytes)
    }

    func close() async {
        try? await stream.close()
    }
}
