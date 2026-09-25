import Foundation

/// The `latch://` links other apps use to open one session on the paired Mac.
///
/// The only shape is `latch://sessions/<session id>`. Overlord Mobile builds it
/// from a mission's terminal session so a person can jump from the mission to
/// the live session without copying `latch attach` into an SSH shell.
///
/// A link names a session and nothing else: no gateway address, token, or
/// command travels in it. It opens the same screen a tap on that row would,
/// under the same grant, so a link can never reach further than the list can.
public enum SessionDeepLink {
    public static let scheme = "latch"
    static let sessionsHost = "sessions"
    /// Session ids are short opaque tokens (`ses_1a0d7323104aa9e0`). Anything
    /// longer or outside this alphabet is not one, and is refused rather than
    /// sent to the Mac as a lookup.
    static let maxSessionIDLength = 128

    /// The link that opens `sessionID`, or nil when the id is not one.
    public static func url(forSession sessionID: String) -> URL? {
        guard isValidSessionID(sessionID) else { return nil }
        return URL(string: "\(scheme)://\(sessionsHost)/\(sessionID)")
    }

    /// The session a link names, or nil when the link is not a session link.
    public static func sessionID(from url: URL) -> String? {
        guard url.scheme?.lowercased() == scheme,
              url.host?.lowercased() == sessionsHost else { return nil }
        let components = url.pathComponents.filter { $0 != "/" }
        guard components.count == 1, let sessionID = components.first,
              isValidSessionID(sessionID) else { return nil }
        return sessionID
    }

    static func isValidSessionID(_ value: String) -> Bool {
        guard !value.isEmpty, value.count <= maxSessionIDLength else { return false }
        return value.unicodeScalars.allSatisfy { scalar in
            scalar.isASCII && (CharacterSet.alphanumerics.contains(scalar) || scalar == "_" || scalar == "-")
        }
    }
}
