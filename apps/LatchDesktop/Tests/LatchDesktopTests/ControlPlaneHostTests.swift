import XCTest
@testable import LatchDesktop

/// A control plane that answers from memory, so host enrollment and pairing
/// registration can be driven without a deployed service.
private final class StubControlPlaneHostAPI: ControlPlaneHostAPI, @unchecked Sendable {
    struct Enrollment: Sendable { let name: String; let publicKey: String }
    struct Rotation: Sendable { let deviceID: String; let publicKey: String }
    struct Pairing: Sendable {
        let pairingID: String
        let secretDigest: String
        let permission: DevicePermission
        let deviceToken: String
    }
    struct PermissionMirror: Sendable {
        let clientDeviceID: String
        let permission: DevicePermission
        let deviceToken: String
    }
    struct Presence: Sendable {
        let deviceToken: String
        let candidates: [ControlPlaneCandidate]
        let ice: ControlPlaneIceCredentials?
    }

    struct Recorded: Sendable {
        var accounts: [String] = []
        var enrollments: [Enrollment] = []
        var rotations: [Rotation] = []
        var pairings: [Pairing] = []
        var permissionMirrors: [PermissionMirror] = []
        var presences: [Presence] = []
        var presenceClears: [String] = []
        var relaySettings: [Bool] = []
    }

    private let lock = NSLock()
    private var recorded = Recorded()
    var devices: [ControlPlaneDevice] = []
    var offers: [ControlPlaneRendezvousOffer] = []
    var failure: ControlPlaneHostError?
    /// Refused once, then cleared, so a recovery path can be driven without
    /// leaving the stub permanently broken.
    var refuseOnce: ControlPlaneHostError?

    private func takeRefusal() -> ControlPlaneHostError? {
        lock.lock()
        defer { lock.unlock() }
        let refusal = refuseOnce
        refuseOnce = nil
        return refusal
    }

    var log: Recorded {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func createAccount(label: String) async throws -> String {
        lock.lock()
        recorded.accounts.append(label)
        lock.unlock()
        if let failure { throw failure }
        return "acct_token"
    }

    func enrollHost(
        accountToken: String,
        name: String,
        publicKey: String
    ) async throws -> (deviceID: String, deviceToken: String) {
        lock.lock()
        recorded.enrollments.append(Enrollment(name: name, publicKey: publicKey))
        let issued = recorded.enrollments.count
        lock.unlock()
        if let failure { throw failure }
        // The first enrollment keeps the names the other tests pin; a second
        // one is distinguishable, which is the whole point of re-enrolling.
        return issued == 1 ? ("dev_mac", "dev_token") : ("dev_mac_\(issued)", "dev_token_\(issued)")
    }

    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws {
        lock.lock()
        recorded.rotations.append(Rotation(deviceID: deviceID, publicKey: publicKey))
        lock.unlock()
        if let refusal = takeRefusal() { throw refusal }
        if let failure { throw failure }
    }

    func openPairingRequest(
        deviceToken: String,
        pairingID: String,
        secretDigest: String,
        permission: DevicePermission
    ) async throws {
        lock.lock()
        recorded.pairings.append(
            Pairing(
                pairingID: pairingID,
                secretDigest: secretDigest,
                permission: permission,
                deviceToken: deviceToken
            )
        )
        lock.unlock()
        if let refusal = takeRefusal() { throw refusal }
        if let failure { throw failure }
    }

    func setPairingPermission(
        deviceToken: String,
        clientDeviceID: String,
        permission: DevicePermission
    ) async throws {
        lock.lock()
        recorded.permissionMirrors.append(
            PermissionMirror(
                clientDeviceID: clientDeviceID,
                permission: permission,
                deviceToken: deviceToken
            )
        )
        lock.unlock()
        if let refusal = takeRefusal() { throw refusal }
        if let failure { throw failure }
    }

    func devices(deviceToken: String) async throws -> [ControlPlaneDevice] {
        if let failure { throw failure }
        return devices
    }

    func publishPresence(
        deviceToken: String,
        candidates: [ControlPlaneCandidate],
        ice: ControlPlaneIceCredentials?
    ) async throws -> ControlPlanePresence {
        lock.lock()
        recorded.presences.append(
            Presence(deviceToken: deviceToken, candidates: candidates, ice: ice)
        )
        lock.unlock()
        if let failure { throw failure }
        return ControlPlanePresence(expiresAt: 1_700_000_090, ttlSeconds: 90)
    }

    func clearPresence(deviceToken: String) async throws {
        lock.lock()
        recorded.presenceClears.append(deviceToken)
        lock.unlock()
        if let failure { throw failure }
    }

    func rendezvousOffers(deviceToken: String) async throws -> [ControlPlaneRendezvousOffer] {
        if let failure { throw failure }
        return offers
    }

    func setRelayEnabled(accountToken: String, enabled: Bool) async throws {
        lock.lock()
        recorded.relaySettings.append(enabled)
        lock.unlock()
        if let failure { throw failure }
    }
}

@MainActor
final class ControlPlaneHostTests: XCTestCase {
    private let macKey = String(repeating: "1", count: 64)
    private static let suite = "co.cooperativ.latch.desktop.tests"

    /// A private suite, emptied first, so a test never reads or writes the
    /// address the app itself uses.
    private func freshDefaults() -> UserDefaults {
        let defaults = UserDefaults(suiteName: Self.suite)!
        defaults.removePersistentDomain(forName: Self.suite)
        return defaults
    }

    private func material(secret: String = String(repeating: "a", count: 64)) -> PairingMaterial {
        PairingMaterial(
            formatVersion: 1,
            pairingID: "0123456789abcdef0123456789abcdef",
            secret: secret,
            macPublicKey: macKey,
            expiresAt: 1_700_000_300,
            controlPlane: nil,
            macName: nil
        )
    }

    private func makeHost(
        api: StubControlPlaneHostAPI,
        store: MemoryHostEnrollmentStore = MemoryHostEnrollmentStore()
    ) -> ControlPlaneHost {
        ControlPlaneHost(store: store, defaults: freshDefaults(), apiFactory: { _ in api })
    }

    // MARK: - Address handling

    func testAddressIsNormalizedAndOnlyHTTPIsAccepted() throws {
        XCTAssertEqual(try ControlPlaneHost.normalize("control.example").absoluteString, "https://control.example")
        XCTAssertEqual(try ControlPlaneHost.normalize(" https://control.example/ ").absoluteString, "https://control.example")
        XCTAssertEqual(try ControlPlaneHost.normalize("http://127.0.0.1:8080").absoluteString, "http://127.0.0.1:8080")

        for bad in ["", "   ", "ftp://control.example", "https://"] {
            XCTAssertThrowsError(try ControlPlaneHost.normalize(bad), "\(bad) must be refused")
        }
    }

    func testABlankAddressClearsTheSetting() throws {
        let host = makeHost(api: StubControlPlaneHostAPI())
        try host.setAddress("control.example")
        XCTAssertTrue(host.isConfigured)
        try host.setAddress("  ")
        XCTAssertFalse(host.isConfigured)
        XCTAssertNil(host.address)
    }

    // MARK: - The secret never leaves this Mac

    /// The digest is the whole reason a control-plane breach cannot answer a
    /// scan: it must match `credentials.ts`'s domain-separated SHA-256 exactly,
    /// so this pins the vector rather than re-deriving it.
    func testSecretDigestMatchesTheControlPlaneContract() {
        XCTAssertEqual(
            ControlPlaneHost.secretDigest(String(repeating: "a", count: 64)),
            "7ce44e5edf1de426e374a315547d001dea55440ab1daf61357c51c90cc90b2a1"
        )
    }

    func testOpeningAPairingRegistersTheDigestAndNeverTheSecret() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")

        let addressed = try await host.openPairing(
            material(),
            publicKey: macKey,
            macName: "Studio Mac"
        )

        let registered = try XCTUnwrap(api.log.pairings.first)
        XCTAssertEqual(registered.pairingID, "0123456789abcdef0123456789abcdef")
        XCTAssertEqual(registered.permission, .interact)
        XCTAssertNotEqual(registered.secretDigest, String(repeating: "a", count: 64))
        XCTAssertEqual(
            registered.secretDigest,
            ControlPlaneHost.secretDigest(String(repeating: "a", count: 64))
        )

        // The code the phone scans is the one that now says where to enroll.
        XCTAssertEqual(addressed.controlPlane, "https://control.example")
        XCTAssertEqual(addressed.macName, "Studio Mac")
        XCTAssertTrue(addressed.carriesAddress)
        XCTAssertEqual(addressed.secret, material().secret)
    }

    func testPairingWithoutAConfiguredControlPlaneIsRefusedRatherThanGuessed() async {
        let host = makeHost(api: StubControlPlaneHostAPI())
        do {
            _ = try await host.openPairing(material(), publicKey: macKey, macName: "Studio Mac")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ControlPlaneHostError, .notConfigured)
        }
    }

    // MARK: - Host enrollment

    func testThisMacEnrolsOnceAndReusesItsCredentials() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")

        let first = try await host.enrollment(publicKey: macKey, name: "Studio Mac")
        let second = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        XCTAssertEqual(first, second)
        XCTAssertEqual(api.log.accounts.count, 1)
        XCTAssertEqual(api.log.enrollments.count, 1)
        XCTAssertEqual(api.log.enrollments.first?.publicKey, macKey)
        XCTAssertEqual(try store.load()?.deviceID, "dev_mac")
    }

    /// A locally rotated Mac identity is carried to the control plane rather
    /// than re-enrolled, because re-enrolling would strand this Mac's pairings.
    func testARotatedMacKeyRotatesInPlace() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        let rotated = String(repeating: "2", count: 64)
        let updated = try await host.enrollment(publicKey: rotated, name: "Studio Mac")

        XCTAssertEqual(updated.publicKey, rotated)
        XCTAssertEqual(updated.deviceID, "dev_mac")
        XCTAssertEqual(api.log.rotations.map(\.publicKey), [rotated])
        XCTAssertEqual(api.log.enrollments.count, 1)
        XCTAssertEqual(try store.load()?.publicKey, rotated)
    }

    /// A grant change made on this Mac is restated in the directory the phone
    /// reads its own permission from, at the pairing row rather than as a new
    /// pairing.
    func testAGrantChangeIsMirroredIntoTheDirectory() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        try await host.mirrorPermission(clientDeviceID: "dev_phone", permission: .control)

        XCTAssertEqual(api.log.permissionMirrors.map(\.clientDeviceID), ["dev_phone"])
        XCTAssertEqual(api.log.permissionMirrors.map(\.permission), [.control])
        XCTAssertTrue(api.log.pairings.isEmpty)
    }

    /// A Mac with no control plane has no directory to mirror into. The local
    /// grant is still the one that decides, so this refuses rather than
    /// silently enrolling to satisfy a UI action.
    func testMirroringAGrantWithoutAControlPlaneIsRefused() async {
        let host = makeHost(api: StubControlPlaneHostAPI(), store: MemoryHostEnrollmentStore())
        do {
            try await host.mirrorPermission(clientDeviceID: "dev_phone", permission: .control)
            XCTFail("an unconfigured Mac must not mirror a grant")
        } catch {
            XCTAssertEqual(error as? ControlPlaneHostError, .notConfigured)
        }
    }

    /// Credentials issued by one deployment name nothing in another, so a
    /// changed address must enroll afresh instead of reusing a stale token.
    func testCredentialsFromAnotherControlPlaneAreNotReused() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore(
            HostEnrollment(
                address: "https://old.example",
                accountToken: "stale",
                deviceID: "dev_old",
                deviceToken: "stale",
                publicKey: macKey
            )
        )
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")

        let enrollment = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        XCTAssertEqual(enrollment.address, "https://control.example")
        XCTAssertEqual(enrollment.deviceToken, "dev_token")
        XCTAssertEqual(api.log.accounts.count, 1)
        XCTAssertTrue(api.log.rotations.isEmpty)
    }

    // MARK: - Names the control plane will store

    /// macOS names a Mac with the typographic apostrophe, which the control
    /// plane's label set does not contain, so an ordinary default-named Mac
    /// used to be refused on the very first call of a pairing.
    func testAMacNameMacOSGivesIsEnrolledRatherThanRefused() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")

        _ = try await host.enrollment(publicKey: macKey, name: "Jake\u{2019}s MacBook Pro")

        XCTAssertEqual(api.log.accounts, ["Jake's MacBook Pro"])
        XCTAssertEqual(api.log.enrollments.map(\.name), ["Jake's MacBook Pro"])
    }

    func testANameIsReducedToTheLabelSetTheServiceAccepts() {
        // Folded rather than dropped, so the name stays a name.
        XCTAssertEqual(ControlPlaneLabel.enrollable("Jake\u{2019}s MacBook Pro"), "Jake's MacBook Pro")
        XCTAssertEqual(ControlPlaneLabel.enrollable("Studio \u{2014} Mac"), "Studio - Mac")
        // Anything else becomes a space, and runs of spaces collapse.
        XCTAssertEqual(ControlPlaneLabel.enrollable("Mac \u{1F5A5}\u{FE0F} Studio"), "Mac Studio")
        XCTAssertEqual(ControlPlaneLabel.enrollable("  Studio   Mac  "), "Studio Mac")
        // The service takes at most 64 code points.
        XCTAssertEqual(ControlPlaneLabel.enrollable(String(repeating: "a", count: 100)).count, 64)
        // A decomposed accent is a combining mark, which the service's letter
        // class does not match, so the name is composed before it is reduced.
        XCTAssertEqual(ControlPlaneLabel.enrollable("Ana\u{0301}s Mac"), "An\u{00E1}s Mac")
        // A name that survives as nothing still has to be something.
        XCTAssertEqual(ControlPlaneLabel.enrollable("\u{1F5A5}\u{FE0F}"), ControlPlaneLabel.fallback)
    }

    // MARK: - Credentials the control plane no longer knows

    /// A redeployed or reset control plane has no row for the device this Mac
    /// enrolled as, and it issues no replacement. Without this the Keychain
    /// would keep a credential that can only be refused, and every pairing
    /// from then on would fail with nothing the person could do about it.
    func testCredentialsTheControlPlaneNoLongerKnowsAreReplaced() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        api.refuseOnce = .http(status: 401, path: "/v1/pairings/requests", reason: "unauthorized")
        let addressed = try await host.openPairing(material(), publicKey: macKey, macName: "Studio Mac")

        XCTAssertEqual(api.log.enrollments.count, 2)
        XCTAssertEqual(api.log.pairings.map(\.deviceToken), ["dev_token", "dev_token_2"])
        XCTAssertEqual(try store.load()?.deviceToken, "dev_token_2")
        XCTAssertTrue(addressed.carriesAddress)
    }

    /// A refusal that is not about the credential is reported rather than
    /// answered by enrolling this Mac a second time.
    func testAnOrdinaryRefusalDoesNotReEnrollThisMac() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        api.failure = .http(status: 403, path: "/v1/pairings/requests", reason: "too many pending pairing requests")
        do {
            _ = try await host.openPairing(material(), publicKey: macKey, macName: "Studio Mac")
            XCTFail("expected the refusal to be reported")
        } catch {
            XCTAssertEqual(error as? ControlPlaneHostError, api.failure)
        }
        XCTAssertEqual(api.log.enrollments.count, 1)
    }

    // MARK: - Reading back the phones

    func testOnlyLivePhonesAreReportedAsPaired() async throws {
        let api = StubControlPlaneHostAPI()
        api.devices = [
            ControlPlaneDevice(
                deviceID: "dev_mac",
                name: "Studio Mac",
                role: "host",
                publicKey: macKey,
                revoked: false,
                permission: nil
            ),
            ControlPlaneDevice(
                deviceID: "dev_old",
                name: "Old phone",
                role: "client",
                publicKey: String(repeating: "3", count: 64),
                revoked: true,
                permission: .observe
            ),
            ControlPlaneDevice(
                deviceID: "dev_phone",
                name: "Jake's iPhone",
                role: "client",
                publicKey: String(repeating: "4", count: 64),
                revoked: false,
                permission: .interact
            ),
        ]
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        let phones = try await host.pairedClients()
        XCTAssertEqual(phones.map(\.deviceID), ["dev_phone"])
    }

    // MARK: - Presence and rendezvous

    func testPresenceUsesTheListenerOnlyAndRefreshesInsideTheServiceWindow() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")

        let presence = try await host.publishPresence(
            publicKey: macKey,
            macName: "Studio Mac",
            listenerAddress: "192.168.1.20:49221",
            now: Date(timeIntervalSince1970: 1_700_000_000)
        )

        XCTAssertEqual(presence.ttlSeconds, 90)
        let published = try XCTUnwrap(api.log.presences.first)
        XCTAssertEqual(published.deviceToken, "dev_token")
        XCTAssertEqual(published.candidates.count, 1)
        let candidate = try XCTUnwrap(published.candidates.first)
        XCTAssertEqual(candidate.address, "192.168.1.20:49221")
        XCTAssertEqual(candidate.expiresAt, 1_700_000_090)
        // The listener is a TCP listener, so it is published as one rather
        // than as a UDP candidate no peer could reach.
        XCTAssertEqual(candidate.type, "host")
        XCTAssertEqual(candidate.protocol, "tcp")
        XCTAssertEqual(candidate.tcpType, "passive")
        XCTAssertEqual(candidate.component, 1)
        XCTAssertNotNil(candidate.priority)
        XCTAssertNotNil(candidate.foundation)
        XCTAssertNil(candidate.relatedAddress)
    }

    /// The ICE credentials belong to the agent, not to a presence window. A
    /// phone that reads them at the start of a window and begins connectivity
    /// checks at the end of it must still authenticate.
    func testIceCredentialsAreCarriedUnchangedAcrossPresenceRefreshes() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        let ice = ControlPlaneIceCredentials.generate()

        for offset in [0, 30, 60] {
            _ = try await host.publishPresence(
                publicKey: macKey,
                macName: "Studio Mac",
                listenerAddress: "192.168.1.20:49221",
                ice: ice,
                now: Date(timeIntervalSince1970: 1_700_000_000 + TimeInterval(offset))
            )
        }

        let published = api.log.presences
        XCTAssertEqual(published.count, 3)
        XCTAssertEqual(published.map(\.ice), Array(repeating: ice, count: 3))
        // The foundation is derived from type, transport, and base address, so
        // it is stable too: a peer must not see the candidate re-form.
        XCTAssertEqual(Set(published.compactMap(\.candidates.first?.foundation)).count, 1)
        XCTAssertTrue(ice.ufrag.count >= 4 && ice.pwd.count >= 22)
        XCTAssertNotEqual(ice, ControlPlaneIceCredentials.generate())
    }

    /// Once the ICE agent supplies candidates, a partially-specified one is
    /// refused here rather than costing a round trip and a 400.
    func testPresenceRefusesAgentCandidatesMissingRequiredIceFields() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        let malformed = [
            // A type with no priority, foundation, component, or protocol.
            ControlPlaneCandidate(address: "203.0.113.7:51000", expiresAt: 1_700_000_060, type: "srflx"),
            // Related address without its port.
            ControlPlaneCandidate(
                address: "203.0.113.7:51000",
                expiresAt: 1_700_000_060,
                type: "srflx",
                priority: 1_694_498_815,
                foundation: "abc",
                component: 1,
                protocol: "udp",
                relatedAddress: "192.168.1.20"
            ),
            // A loopback related address would leak nothing useful and is
            // exactly what the address rule exists to keep out.
            ControlPlaneCandidate(
                address: "203.0.113.7:51000",
                expiresAt: 1_700_000_060,
                type: "srflx",
                priority: 1_694_498_815,
                foundation: "abc",
                component: 1,
                protocol: "udp",
                relatedAddress: "127.0.0.1",
                relatedPort: 49_221
            ),
            // tcpType on a UDP candidate.
            ControlPlaneCandidate(
                address: "203.0.113.7:51000",
                expiresAt: 1_700_000_060,
                type: "srflx",
                priority: 1_694_498_815,
                foundation: "abc",
                component: 1,
                protocol: "udp",
                tcpType: "passive"
            ),
        ]

        for candidate in malformed {
            await XCTAssertThrowsErrorAsync {
                _ = try await host.publishPresence(
                    publicKey: self.macKey,
                    macName: "Studio Mac",
                    listenerAddress: "192.168.1.20:49221",
                    agentCandidates: [candidate],
                    now: Date(timeIntervalSince1970: 1_700_000_000)
                )
            }
        }
        XCTAssertTrue(api.log.presences.isEmpty)
    }

    func testPresenceCarriesTheAgentsGatheredCandidatesAlongsideTheHostOne() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        let reflexive = ControlPlaneCandidate(
            address: "203.0.113.7:51000",
            expiresAt: 1_700_000_060,
            type: "srflx",
            priority: 1_694_498_815,
            foundation: "abc",
            component: 1,
            protocol: "udp",
            relatedAddress: "192.168.1.20",
            relatedPort: 49_221
        )

        _ = try await host.publishPresence(
            publicKey: macKey,
            macName: "Studio Mac",
            listenerAddress: "192.168.1.20:49221",
            agentCandidates: [reflexive],
            now: Date(timeIntervalSince1970: 1_700_000_000)
        )

        let published = try XCTUnwrap(api.log.presences.first)
        XCTAssertEqual(published.candidates.map(\.type), ["host", "srflx"])
        XCTAssertEqual(published.candidates.last, reflexive)
    }

    func testPresenceRefusesLoopbackHostnamesAndInvalidPortsBeforePublishing() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")

        for address in ["127.0.0.1:49221", "[::1]:49221", "mac.example:49221", "192.168.1.20:0"] {
            await XCTAssertThrowsErrorAsync {
                _ = try await host.publishPresence(
                    publicKey: self.macKey,
                    macName: "Studio Mac",
                    listenerAddress: address
                )
            }
        }
        XCTAssertTrue(api.log.presences.isEmpty)
    }

    func testPresenceCanBeClearedWithoutReenrolling() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        await host.clearPresence()

        XCTAssertEqual(api.log.presenceClears, ["dev_token"])
        XCTAssertEqual(api.log.enrollments.count, 1)
    }

    func testRendezvousOffersAndRelaySwitchUseHostCredentials() async throws {
        let api = StubControlPlaneHostAPI()
        api.offers = [
            ControlPlaneRendezvousOffer(
                requestID: "request-123",
                peerDeviceID: "dev_0123456789abcdef0123456789abcdef",
                peerIdentityKey: String(repeating: "2", count: 64),
                candidates: [ControlPlaneCandidate(address: "10.0.0.4:4000", expiresAt: 1_700_000_050)],
                expiresAt: 1_700_000_050
            ),
        ]
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        let collected = try await host.rendezvousOffers()
        XCTAssertEqual(collected, api.offers)
        try await host.setRelayEnabled(false)
        XCTAssertEqual(api.log.relaySettings, [false])
    }
}

private func XCTAssertThrowsErrorAsync(
    _ expression: @escaping () async throws -> Void,
    file: StaticString = #filePath,
    line: UInt = #line
) async {
    do {
        try await expression()
        XCTFail("expected an error", file: file, line: line)
    } catch {}
}
