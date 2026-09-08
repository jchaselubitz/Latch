import Foundation
import Observation

#if canImport(AVFoundation)
import AVFoundation
#endif

/// Whether the app may use the camera, which is the one iOS permission the
/// pairing screen depends on.
///
/// This is modeled rather than read inline so the screen can say what is wrong
/// and what to do about it: a first-run prompt, a trip to Settings, and a
/// device without a usable camera are three different dead ends.
public enum CameraPermission: Equatable, Sendable {
    case notDetermined
    case authorized
    case denied
    case restricted
    /// No camera at all — the simulator, mostly.
    case unavailable

    public var allowsScanning: Bool { self == .authorized }

    public var explanation: String? {
        switch self {
        case .authorized, .notDetermined:
            return nil
        case .denied:
            return """
            Latch needs the camera to read the pairing code on your Mac. \
            Turn it on in Settings › Latch › Camera, or paste the Remote Link code.
            """
        case .restricted:
            return """
            Camera access is restricted on this phone, so the code cannot be scanned. \
            Paste the Remote Link code instead.
            """
        case .unavailable:
            return "This device has no camera. Paste the Remote Link code instead."
        }
    }
}

/// The camera authorization calls, behind a protocol so the pairing flow is
/// testable without a camera or a permission prompt.
public protocol CameraAuthorizing: Sendable {
    func current() -> CameraPermission
    func request() async -> CameraPermission
}

/// The real thing.
public struct SystemCameraAuthorization: CameraAuthorizing {
    public init() {}

    public func current() -> CameraPermission {
        #if canImport(AVFoundation)
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: return .authorized
        case .denied: return .denied
        case .restricted: return .restricted
        case .notDetermined: return .notDetermined
        @unknown default: return .denied
        }
        #else
        return .unavailable
        #endif
    }

    public func request() async -> CameraPermission {
        #if canImport(AVFoundation)
        guard current() == .notDetermined else { return current() }
        _ = await AVCaptureDevice.requestAccess(for: .video)
        return current()
        #else
        return .unavailable
        #endif
    }
}

/// A camera permission fixed at construction, for tests and previews.
public struct StubCameraAuthorization: CameraAuthorizing {
    private let permission: CameraPermission

    public init(_ permission: CameraPermission) {
        self.permission = permission
    }

    public func current() -> CameraPermission { permission }
    public func request() async -> CameraPermission { permission }
}

/// A validated Remote Link enrollment code.
public struct PairingProposal: Equatable, Sendable {
    public let payload: PairingPayload
    /// This phone's identity, as it will be enrolled.
    public let devicePublicKey: String

    /// The Mac as the confirmation screen should name it.
    public var macDisplayName: String {
        if !payload.macName.isEmpty { return payload.macName }
        return "Mac \(HexCoding.abbreviate(payload.hostPublicKey))"
    }

    public var macFingerprint: String {
        HexCoding.abbreviate(payload.hostPublicKey)
    }
}

/// Prepared native enrollment. The comparison is derived only after the WSS
/// carrier and Noise XX transcript exist. The durable record arrives only
/// after the Mac's explicit approval and encrypted receipt.
public protocol RemoteEnrollmentSession: Sendable {
    var comparison: String { get }
    func awaitApprovedRecord() async throws -> PairedDeviceRecord
    func close() async
}

public protocol RemoteEnrollmentProviding: Sendable {
    func prepare(
        payload: PairingPayload,
        deviceName: String,
        permission: DevicePermission
    ) async throws -> any RemoteEnrollmentSession
}

public struct UnavailableRemoteEnrollmentProvider: RemoteEnrollmentProviding {
    public init() {}
    public func prepare(
        payload: PairingPayload,
        deviceName: String,
        permission: DevicePermission
    ) async throws -> any RemoteEnrollmentSession {
        throw ControlPlaneError.rejected("This build does not include the native Remote Link enrollment provider.")
    }
}

/// Where pairing is.
public enum PairingState: Equatable, Sendable {
    /// No pairing, and none in progress.
    case idle
    /// The camera is live and no code has been accepted yet.
    case scanning
    /// A code validated. Connecting creates the authenticated comparison.
    case confirming(PairingProposal)
    /// Noise authenticated; compare this transcript-derived code on the Mac.
    case comparing(PairingProposal, String)
    /// Completing encrypted enrollment.
    case enrolling
    /// Paired and usable.
    case paired(PairedDeviceRecord)
    /// Paired once, then revoked. The record is kept so the app can say so.
    case revoked(PairedDeviceRecord)
    /// The attempt failed. The message is already user-facing.
    case failed(String)
}

/// The pairing flow: identity, scan, enroll, confirm, persist, revoke.
///
/// Every step that can fail resolves to a `PairingState` with a sentence in
/// it, because the whole screen is a security decision the person has to be
/// able to read. Nothing here logs the pairing secret.
@MainActor
@Observable
public final class PairingModel {
    public private(set) var state: PairingState = .idle
    public private(set) var cameraPermission: CameraPermission = .notDetermined
    /// This phone's identity, created on first use.
    public private(set) var identity: DeviceIdentity?
    /// The saved pairing, when there is one.
    public private(set) var record: PairedDeviceRecord?
    /// Set while a control-plane call is in flight.
    public private(set) var isBusy = false

    /// What this phone asks to be called on the Mac's device list.
    public var deviceName: String
    private let identityStore: DeviceIdentityStoring
    private let deviceStore: PairedDeviceStoring
    private let camera: CameraAuthorizing
    private let clientFactory: @Sendable (URL) -> ControlPlaneClient
    private let enrollmentProvider: any RemoteEnrollmentProviding
    private var enrollmentSession: (any RemoteEnrollmentSession)?
    /// The last string the scanner handed over, so the same code re-read many
    /// times a second does not restart the flow or flash an error repeatedly.
    private var lastScanned: String?

    public init(
        identityStore: DeviceIdentityStoring = KeychainDeviceIdentityStore(),
        deviceStore: PairedDeviceStoring = KeychainPairedDeviceStore(),
        camera: CameraAuthorizing = SystemCameraAuthorization(),
        deviceName: String = "",
        enrollmentProvider: any RemoteEnrollmentProviding = UnavailableRemoteEnrollmentProvider(),
        clientFactory: @escaping @Sendable (URL) -> ControlPlaneClient = { HTTPControlPlaneClient(baseURL: $0) }
    ) {
        self.identityStore = identityStore
        self.deviceStore = deviceStore
        self.camera = camera
        self.deviceName = deviceName
        self.enrollmentProvider = enrollmentProvider
        self.clientFactory = clientFactory
    }

    /// The phone's own name, which is what a person expects to recognize in
    /// the Mac's device list.
    public static var defaultDeviceName: String {
        #if canImport(UIKit) && !os(macOS)
        return UIDeviceName.current
        #else
        return "iPhone"
        #endif
    }

    /// Loads the saved pairing and this phone's identity at launch.
    public func restore() async {
        cameraPermission = camera.current()
        if deviceName.isEmpty { deviceName = Self.defaultDeviceName }
        do {
            identity = try identityStore.loadOrCreate()
        } catch let error as DeviceIdentityError {
            state = .failed(error.message)
            return
        } catch {
            state = .failed(error.localizedDescription)
            return
        }
        guard let saved = try? deviceStore.load() else { return }
        record = saved
        state = saved.revoked ? .revoked(saved) : .paired(saved)
    }

    // MARK: - Scanning

    /// Opens the scanner, asking for the camera if this is the first time.
    public func beginScanning() async {
        guard record == nil else { return }
        lastScanned = nil
        cameraPermission = camera.current()
        if cameraPermission == .notDetermined {
            cameraPermission = await camera.request()
        }
        state = .scanning
    }

    /// Leaves the flow without pairing.
    public func cancel() {
        if let enrollmentSession {
            Task { await enrollmentSession.close() }
        }
        enrollmentSession = nil
        lastScanned = nil
        if let record {
            state = record.revoked ? .revoked(record) : .paired(record)
        } else {
            state = .idle
        }
    }

    /// Handles one string from the scanner or the manual-entry field.
    ///
    /// The scanner delivers the same code repeatedly, so a string identical to
    /// the last one is dropped: without that, a code that fails validation
    /// would re-report its failure many times a second.
    public func scanned(_ value: String, now: Date = Date()) {
        // A scan is only meaningful before there is a pairing, and only while
        // the flow is waiting for one. A repeat during confirmation must not
        // restart the screen the person is in the middle of reading.
        guard record == nil else { return }
        switch state {
        case .idle, .scanning, .failed:
            break
        case .confirming, .comparing, .enrolling, .paired, .revoked:
            return
        }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != lastScanned else { return }
        lastScanned = trimmed
        guard let identity else {
            state = .failed("This phone has no device identity yet.")
            return
        }
        do {
            let payload = try PairingPayload.parse(trimmed, now: now)
            state = .confirming(
                PairingProposal(
                    payload: payload,
                    devicePublicKey: identity.publicKey
                )
            )
        } catch let error as PairingPayloadError {
            state = .failed(error.message)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    // MARK: - Enrolling

    /// Opens the native WSS/Noise enrollment link.
    ///
    /// Expiry is rechecked here rather than trusted from the scan.
    public func confirm(now: Date = Date()) async {
        guard case .confirming(let proposal) = state else { return }
        guard identity != nil else {
            state = .failed("This phone has no device identity yet.")
            return
        }
        do {
            try proposal.payload.checkLifetime(now: now)
        } catch let error as PairingPayloadError {
            state = .failed(error.message)
            return
        } catch {
            state = .failed(error.localizedDescription)
            return
        }

        state = .enrolling
        isBusy = true
        defer { isBusy = false }
        do {
            let session = try await enrollmentProvider.prepare(
                payload: proposal.payload,
                deviceName: Self.enrollableName(deviceName),
                permission: .control
            )
            enrollmentSession = session
            state = .comparing(proposal, session.comparison)
        } catch let error as ControlPlaneError {
            state = .failed(error.message)
        } catch let error as DeviceIdentityError {
            state = .failed(error.message)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    /// Continues only after the person confirms the transcript code matches
    /// the Mac. Persistence happens after the encrypted receipt and current
    /// directory grant both verify in the native provider.
    public func confirmComparison() async {
        guard case .comparing = state, let enrollmentSession else { return }
        state = .enrolling
        isBusy = true
        defer { isBusy = false }
        do {
            let record = try await enrollmentSession.awaitApprovedRecord()
            try deviceStore.save(record)
            self.record = record
            self.enrollmentSession = nil
            state = record.revoked ? .revoked(record) : .paired(record)
        } catch let error as ControlPlaneError {
            state = .failed(error.message)
        } catch let error as DeviceIdentityError {
            state = .failed(error.message)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    /// This phone's name, reduced to what the control plane will accept.
    ///
    /// The service takes letters, digits, spaces, and `. _ ' ( ) -`, up to 64
    /// characters, and answers anything else with a 400. A phone called
    /// "Jake’s iPhone" — with the typographic apostrophe iOS puts there —
    /// would otherwise fail pairing with a message about the control plane
    /// rather than about the name, so the name is fixed here instead.
    ///
    /// Typographic punctuation is folded to its ASCII equivalent before
    /// anything is dropped, so the name survives as a name: that phone enrolls
    /// as "Jake's iPhone" rather than losing the character to a space. The name
    /// is composed first for the same reason: the service matches letters, and
    /// a decomposed accent is a combining mark rather than part of one.
    static func enrollableName(_ raw: String) -> String {
        let allowed = CharacterSet.letters
            .union(.decimalDigits)
            .union(CharacterSet(charactersIn: " ._'()-"))
        let folded = String(
            raw.precomposedStringWithCanonicalMapping.map { Self.punctuationFolding[$0] ?? $0 }
        )
        let cleaned = String(
            String.UnicodeScalarView(folded.unicodeScalars.map { allowed.contains($0) ? $0 : " " })
        )
        let collapsed = cleaned
            .split(separator: " ", omittingEmptySubsequences: true)
            .joined(separator: " ")
        let bounded = String(collapsed.prefix(64)).trimmingCharacters(in: .whitespaces)
        return bounded.isEmpty ? defaultDeviceName : bounded
    }

    /// Punctuation the platforms put in device names that the control plane's
    /// label set does not accept, mapped to the character it stands for.
    private static let punctuationFolding: [Character: Character] = [
        "\u{2018}": "'", "\u{2019}": "'", "\u{02BC}": "'", "\u{00B4}": "'", "`": "'",
        "\u{2013}": "-", "\u{2014}": "-", "\u{2212}": "-",
    ]

    // MARK: - Permission state and revocation

    /// Re-reads the device record from the control plane.
    ///
    /// This is how the phone learns that the Mac revoked it or changed what it
    /// may do. It is called when the pairing screen appears and when the app
    /// returns to the foreground, in the same spirit as re-running gateway
    /// discovery: state that was decided elsewhere is not assumed to hold.
    public func refreshPermission() async {
        guard let saved = record, let token = saved.accessToken else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            let confirmation = try await clientFactory(saved.controlPlane)
                .device(deviceId: saved.deviceId, accessToken: token)
            if confirmation.device.revoked {
                // A server-confirmed revoke is terminal. Use the same path as
                // an invalidated credential so the now-useless token is
                // removed instead of being persisted with the revoked record.
                markRevoked()
                return
            }
            var updated = saved
            updated.permission = confirmation.device.permission
            guard updated != saved else { return }
            try deviceStore.save(updated)
            record = updated
            state = updated.revoked ? .revoked(updated) : .paired(updated)
        } catch let error as ControlPlaneError {
            switch error {
            case .rejected:
                // The control plane no longer recognizes this device or its
                // token. That is a revocation from the phone's point of view,
                // and saying so is more honest than leaving a paired screen
                // that cannot work.
                markRevoked()
            default:
                break
            }
        } catch {
            // A refresh is advisory. A network failure must not make a working
            // pairing look revoked.
        }
    }

    // MARK: - Attention notifications

    /// The opaque APNs token iOS handed the app, hex encoded, kept so it can be
    /// registered as soon as a pairing exists and re-registered if it changes.
    public private(set) var pushToken: String?
    /// Whether the control plane currently holds this phone's token.
    public private(set) var pushRegistered = false

    /// iOS delivered (or refreshed) the device token. Registration needs a
    /// paired device credential; before one exists the token is only kept.
    public func pushTokenReceived(_ token: Data) async {
        let hex = token.map { String(format: "%02x", $0) }.joined()
        guard hex != pushToken || !pushRegistered else { return }
        pushToken = hex
        pushRegistered = false
        await registerPushIfPossible()
    }

    /// Registers the retained token with the paired device credential. Best
    /// effort: a failure leaves `pushRegistered` false and the next foreground
    /// tries again. Nothing else is sent.
    public func registerPushIfPossible() async {
        guard let pushToken, !pushRegistered, let saved = record, saved.isActive,
              let token = saved.accessToken else { return }
        do {
            try await clientFactory(saved.controlPlane).registerPush(token: pushToken, accessToken: token)
            pushRegistered = true
        } catch {
            pushRegistered = false
        }
    }

    /// Revokes this phone, from this phone.
    ///
    /// The local record and the device identity go first and unconditionally:
    /// if the control plane cannot be reached, the phone must still stop being
    /// able to connect. The Mac's own revoke stays authoritative for the
    /// server-side half.
    public func revoke() async {
        guard let saved = record else { return }
        isBusy = true
        defer { isBusy = false }
        if let token = saved.accessToken {
            // Revocation removes the registration server-side as well; this is
            // the explicit half in case the revoke call itself fails.
            try? await clientFactory(saved.controlPlane).unregisterPush(accessToken: token)
            try? await clientFactory(saved.controlPlane).revoke(deviceId: saved.deviceId, accessToken: token)
        }
        pushRegistered = false
        forget()
    }

    /// Drops the pairing and the identity it was made with.
    public func forget() {
        try? deviceStore.clear()
        try? identityStore.destroy()
        record = nil
        lastScanned = nil
        identity = try? identityStore.loadOrCreate()
        state = .idle
    }

    private func markRevoked() {
        guard var updated = record, !updated.revoked else { return }
        updated.revoked = true
        // The token is useless once revoked; keeping it would be one more
        // credential on disk for no reason.
        updated.accessToken = nil
        try? deviceStore.save(updated)
        record = updated
        state = .revoked(updated)
    }
}

#if canImport(UIKit) && !os(macOS)
import UIKit

/// `UIDevice.name` is main-actor-isolated; this keeps that isolation in one
/// place instead of spreading it through the model.
enum UIDeviceName {
    @MainActor
    static var current: String {
        let name = UIDevice.current.name
        return name.isEmpty ? UIDevice.current.model : name
    }
}
#endif
