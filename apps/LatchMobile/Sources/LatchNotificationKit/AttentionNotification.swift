import Foundation

/// The words and routing for one attention alert.
///
/// Shared by the notification service extension, which rewrites the generic
/// alert with it, and the app, which reads the session id back when the
/// alert is tapped. Kept free of UserNotifications so it is testable on any
/// platform.
public struct AttentionNotification: Equatable, Sendable {
    /// `userInfo` key carrying the session id a tap opens.
    public static let sessionIDKey = "latchSessionID"
    /// The category the control plane sets on sealed alerts.
    public static let category = "LATCH_ATTENTION"

    public let title: String
    public let body: String
    /// Groups a session's alerts together in Notification Center.
    public let threadIdentifier: String
    public let sessionID: String

    public init(_ payload: AttentionPayload) {
        let title = payload.title?.trimmingCharacters(in: .whitespacesAndNewlines)
        self.title = (title?.isEmpty == false ? title : nil) ?? payload.name
        switch payload.kind {
        case .approval: body = "Needs your input"
        case .completion: body = "Finished working"
        }
        threadIdentifier = "latch-session-\(payload.sessionID)"
        sessionID = payload.sessionID
    }

    /// Where the extension finds the sealed blob and its event id in a
    /// remote notification's `userInfo`: `{"latch": {"e": ..., "c": ...}}`.
    public static func sealedFields(in userInfo: [AnyHashable: Any]) -> (eventID: String, sealed: String)? {
        guard let latch = userInfo["latch"] as? [String: Any],
              let eventID = latch["e"] as? String,
              let sealed = latch["c"] as? String,
              eventID.count == 32,
              eventID.allSatisfy(\.isHexDigit)
        else { return nil }
        return (eventID, sealed)
    }

    /// The session a delivered alert should open, if the extension named one.
    public static func sessionID(in userInfo: [AnyHashable: Any]) -> String? {
        userInfo[sessionIDKey] as? String
    }
}
