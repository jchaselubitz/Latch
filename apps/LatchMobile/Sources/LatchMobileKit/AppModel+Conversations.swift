import Foundation

// MARK: - Conversations

extension AppModel {
    /// The largest file the composer may attach, or nil when this phone may
    /// not attach files at all: the Mac does not serve the route, or this
    /// device's grant does not reach the composer.
    public var attachmentLimit: Int? {
        guard case .linked(let capabilities) = linkState, surface.attachments else { return nil }
        return capabilities.features.attachmentMaxBytes
    }

    /// Returns the one persistent conversation store for this session. This
    /// consumes discovery already performed during link setup; it never makes
    /// a separate interaction-capabilities preflight.
    public func conversationStore(for session: SessionSummary) -> ConversationStore? {
        guard let gateway,
              case .linked(let capabilities) = linkState,
              GatewayCompatibility.supports(endpoint: .conversation, capabilities: capabilities)
        else { return nil }
        if let existing = conversationStores[session.id] { return existing }
        let store = ConversationStore(
            sessionID: session.id,
            gateway: gateway,
            operationRetentionSeconds: capabilities.operationRetentionSeconds
        )
        conversationStores[session.id] = store
        return store
    }

    /// The socket is intentionally stopped before suspension: iOS can reclaim
    /// the underlying connection without delivering a close callback. Stores
    /// keep their cache and resume tuple, so foreground does not need a replay.
    public func suspendConversations() {
        conversationStores.values.forEach { $0.stop() }
    }

    public func resumeConversations() {
        conversationStores.values.forEach { $0.start() }
    }
}
