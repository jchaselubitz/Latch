import Foundation
import IOKit.pwr_mgt

/// Keeps this Mac awake while a phone is actually connected to it.
///
/// A remote terminal is only useful if the machine serving it is running, and
/// idle sleep will otherwise take the Mac out from under a person typing into
/// it from across town. The assertion is deliberately the narrow one:
/// `PreventUserIdleSystemSleep` stops the *idle* timer only. Closing the lid,
/// choosing Sleep from the menu, and a critically low battery all still put the
/// Mac to sleep, because those are the owner saying so and this is not a
/// mechanism for overruling them. The display is left alone entirely — a screen
/// that stays lit is not what a remote session needs.
///
/// It is held only while remote access is on *and* the helper reports at least
/// one authenticated stream, so a Mac nobody is using goes to sleep on its
/// ordinary schedule.
protocol SleepPreventing: Sendable {
    /// Starts preventing idle sleep, returning the assertion handle. Called
    /// only when nothing is currently held.
    func create(reason: String) -> UInt32?
    /// Releases a handle from `create`.
    func release(_ assertion: UInt32)
}

struct IOPMSleepPreventer: SleepPreventing {
    func create(reason: String) -> UInt32? {
        var assertion: IOPMAssertionID = 0
        let result = IOPMAssertionCreateWithName(
            kIOPMAssertionTypePreventUserIdleSystemSleep as CFString,
            IOPMAssertionLevel(kIOPMAssertionLevelOn),
            reason as CFString,
            &assertion
        )
        guard result == kIOReturnSuccess else { return nil }
        return assertion
    }

    func release(_ assertion: UInt32) {
        IOPMAssertionRelease(assertion)
    }
}

/// Holds at most one assertion and matches it to a desired state.
///
/// The desired state is recomputed from status on every poll rather than
/// toggled from events, so a missed connection-closed event cannot leave a Mac
/// awake forever: the next status that reports no connections releases it.
@MainActor
final class SleepAssertionHolder {
    /// What macOS shows next to the assertion in `pmset -g assertions`. It
    /// names the reason rather than the app, because the owner deserves to see
    /// why their Mac is not sleeping.
    static let reason = "Latch is serving a connected phone"

    private let preventer: any SleepPreventing
    private var assertion: UInt32?

    /// Whether idle sleep is being prevented right now.
    var isHeld: Bool { assertion != nil }

    nonisolated init(preventer: any SleepPreventing = IOPMSleepPreventer()) {
        self.preventer = preventer
    }

    deinit {
        // `deinit` cannot hop actors, and the holder is owned by the
        // controller for the life of the app; `apply(false)` on teardown is
        // the path that actually runs. A leaked assertion would outlive only
        // the process, which releases it.
        if let assertion { preventer.release(assertion) }
    }

    /// Raises or drops the assertion to match `shouldPrevent`. Idempotent: a
    /// repeated poll of the same state does nothing.
    func apply(_ shouldPrevent: Bool) {
        switch (shouldPrevent, assertion) {
        case (true, nil):
            assertion = preventer.create(reason: Self.reason)
        case (false, .some(let held)):
            preventer.release(held)
            assertion = nil
        default:
            break
        }
    }
}
