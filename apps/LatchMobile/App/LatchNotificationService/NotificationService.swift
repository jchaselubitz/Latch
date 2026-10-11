import LatchNotificationKit
import UserNotifications

/// Opens a sealed attention alert and says which session it is about.
///
/// The control plane delivers the same generic sentence for every alert,
/// marked mutable and carrying a blob the Mac sealed to this phone's
/// notification key. This extension opens it — locally, while the phone may
/// be locked — and replaces the sentence with the session's name and what it
/// needs. Anything it cannot open is shown exactly as delivered: the generic
/// alert is always the fallback, never a blank or an error.
final class NotificationService: UNNotificationServiceExtension {
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var delivered: UNNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        delivered = request.content
        finish(with: Self.rewrite(request.content) ?? request.content)
    }

    /// Out of time before finishing: show the alert as delivered.
    override func serviceExtensionTimeWillExpire() {
        if let delivered { finish(with: delivered) }
    }

    private func finish(with content: UNNotificationContent) {
        guard let contentHandler else { return }
        self.contentHandler = nil
        contentHandler(content)
    }

    private static func rewrite(_ content: UNNotificationContent) -> UNNotificationContent? {
        guard let fields = AttentionNotification.sealedFields(in: content.userInfo),
              let key = NotificationKeyStore.keychain(accessGroup: accessGroup).load(),
              let payload = try? AttentionPayload.open(sealedHex: fields.sealed, eventID: fields.eventID, privateKey: key),
              let mutable = content.mutableCopy() as? UNMutableNotificationContent
        else { return nil }
        let notification = AttentionNotification(payload)
        mutable.title = notification.title
        mutable.body = notification.body
        mutable.threadIdentifier = notification.threadIdentifier
        var userInfo = mutable.userInfo
        // The blob has served its purpose; what the app needs is the session.
        userInfo.removeValue(forKey: "latch")
        userInfo[AttentionNotification.sessionIDKey] = notification.sessionID
        mutable.userInfo = userInfo
        return mutable
    }

    private static var accessGroup: String? {
        Bundle.main.object(forInfoDictionaryKey: "LatchKeychainAccessGroup") as? String
    }
}
