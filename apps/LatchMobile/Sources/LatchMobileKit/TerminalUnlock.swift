import Foundation
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif

/// The device-owner check the phone runs before it takes a Mac's terminal.
///
/// A protocol rather than a direct `LAContext` call so the lifecycle rules
/// around it can be asserted without a biometric prompt, and so a platform
/// without LocalAuthentication is a compile-time substitution rather than a
/// runtime branch.
public protocol DeviceOwnerAuthenticating: Sendable {
    /// Whether this device can ask the owner at all. False on a phone with no
    /// passcode, biometric or otherwise.
    func canAuthenticate() -> Bool
    /// Asks. Returns false when the owner cancelled or failed the check;
    /// throws only when the check could not be run.
    func authenticate(reason: String) async throws -> Bool
}

/// Errors this side raises, as opposed to the ones LocalAuthentication does.
public enum DeviceOwnerAuthenticationError: Error, Equatable, Sendable {
    /// The device has no passcode, so there is no owner check to run.
    case unavailable
}

#if canImport(LocalAuthentication)
/// `LAContext` with the device-owner policy: Face ID or Touch ID where the
/// device has it, and the passcode everywhere else and as the fallback. The
/// biometric-only policy is deliberately not used — a failed or unenrolled
/// biometric should ask for the passcode, not refuse the terminal.
public struct LocalDeviceOwnerAuthenticator: DeviceOwnerAuthenticating {
    public init() {}

    public func canAuthenticate() -> Bool {
        LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
    }

    public func authenticate(reason: String) async throws -> Bool {
        // A fresh context per evaluation. Reusing one carries its own
        // successful-evaluation state, which is exactly the caching this
        // class means to hold itself, on its own clock.
        let context = LAContext()
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else {
            throw DeviceOwnerAuthenticationError.unavailable
        }
        do {
            return try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
        } catch let error as LAError where error.code == .userCancel
            || error.code == .userFallback
            || error.code == .appCancel
            || error.code == .systemCancel
            || error.code == .authenticationFailed
        {
            // A refusal is an answer, not a malfunction: the terminal simply
            // does not open, and no error banner is warranted.
            return false
        }
    }
}
#endif

/// Whether this phone may open a terminal right now.
///
/// Chat is not gated: reading a conversation and sending a message are what a
/// paired phone is for. A terminal is different — it runs commands on the Mac
/// and takes the session's one surface from whoever is looking at it — so it
/// is held behind the device owner, with a short grace window so a person
/// working in a session is not asked on every reattach.
@MainActor
public final class TerminalUnlock {
    /// How long one successful check lasts. Short enough that a phone left on
    /// a desk does not stay open to a terminal, long enough that attaching,
    /// backgrounding to read something, and reattaching is one prompt.
    public nonisolated static let defaultGrace: TimeInterval = 5 * 60

    private let authenticator: any DeviceOwnerAuthenticating
    private let grace: TimeInterval
    private let now: @Sendable () -> Date
    private var unlockedUntil: Date?
    /// Why the last check did not open the terminal, when it is worth saying.
    /// A cancelled prompt leaves this nil: the person already knows.
    public private(set) var failure: String?

    public init(
        authenticator: any DeviceOwnerAuthenticating,
        grace: TimeInterval = TerminalUnlock.defaultGrace,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.authenticator = authenticator
        self.grace = grace
        self.now = now
    }

    #if canImport(LocalAuthentication)
    public convenience init(grace: TimeInterval = TerminalUnlock.defaultGrace) {
        self.init(authenticator: LocalDeviceOwnerAuthenticator(), grace: grace)
    }
    #endif

    /// Whether a check has been passed recently enough to still count.
    public var isUnlocked: Bool {
        guard let unlockedUntil else { return false }
        return now() < unlockedUntil
    }

    /// Passes the owner check, or reports why the terminal stays shut.
    ///
    /// Inside the grace window this answers immediately without a prompt.
    public func unlock(reason: String) async -> Bool {
        if isUnlocked {
            failure = nil
            return true
        }
        guard authenticator.canAuthenticate() else {
            failure = "Set a passcode on this device to open a terminal."
            return false
        }
        do {
            guard try await authenticator.authenticate(reason: reason) else {
                failure = nil
                return false
            }
        } catch {
            failure = "This device could not confirm it is you, so the terminal stayed closed."
            return false
        }
        unlockedUntil = now().addingTimeInterval(grace)
        failure = nil
        return true
    }

    /// Ends the grace window. Used when the link itself goes away, so a phone
    /// relinked to another Mac starts from a fresh check.
    public func lock() {
        unlockedUntil = nil
        failure = nil
    }
}
