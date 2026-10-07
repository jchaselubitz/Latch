import Foundation

// MARK: - SessionCreation

extension AppModel {
    /// Directory data and process creation are control-granted operations.
    private var hasNewSessionControlGrant: Bool {
        pairedDevice?.permission.permits(.control) == true
    }

    public var canBrowseNewSessionFolders: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return hasNewSessionControlGrant
            && GatewayCompatibility.supports(endpoint: .browseDirectories, capabilities: capabilities)
    }

    public var canCreateNewSession: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return canBrowseNewSessionFolders
            && GatewayCompatibility.supports(endpoint: .createSession, capabilities: capabilities)
    }

    /// Agent kinds the linked Mac will launch from the create route, in the
    /// order discovery listed them. Empty on a Mac that predates agent
    /// creation, which then offers shells only. Independent of this phone's
    /// grant, like the rest of what a Mac advertises.
    public var availableSessionAgents: [SessionAgent] {
        guard case .linked(let capabilities) = linkState else { return [] }
        guard GatewayCompatibility.supports(endpoint: .createSession, capabilities: capabilities)
        else { return [] }
        return capabilities.features.sessionAgents
    }

    /// Whether this phone may start `agent` right now: creation itself must
    /// be allowed, and the Mac must have said it launches that kind.
    public func canCreateNewSession(agent: SessionAgent?) -> Bool {
        guard canCreateNewSession else { return false }
        guard let agent else { return true }
        return availableSessionAgents.contains(agent)
    }

    /// Whether the linked Mac lists models for its agents and accepts one at
    /// creation. A Mac that predates model choice starts every agent on its
    /// own default, and the picker shows no model row.
    public var offersAgentModelChoice: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: .agentModels, capabilities: capabilities)
    }

    /// Whether the linked Mac serves both new-session routes, independent of
    /// this phone's grant. The control is shown but disabled when the Mac can
    /// serve the flow and this phone may not use it, so the reason can be
    /// said rather than left as a missing button.
    public var advertisesNewSessionCreation: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: .browseDirectories, capabilities: capabilities)
            && GatewayCompatibility.supports(endpoint: .createSession, capabilities: capabilities)
    }

    /// Why the advertised new-session flow cannot be used right now, or nil
    /// when it can. Only a grant can hold it back once the routes exist.
    public var newSessionUnavailableExplanation: String? {
        guard advertisesNewSessionCreation, !canCreateNewSession else { return nil }
        return """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """
    }

    /// Re-reads the store after the browser saved a selection.
    public func reloadDefaultNewSessionFolder() {
        defaultNewSessionFolder = newSessionFolderStore.load()
    }

    /// Forgets the saved default so the next picker opens at the Mac's home
    /// directory.
    public func clearDefaultNewSessionFolder() {
        newSessionFolderStore.save(nil)
        defaultNewSessionFolder = nil
    }

    /// Authenticates before constructing the browser so no directory state is
    /// fetched or exposed to someone who has not passed the owner check.
    public func newSessionFolderBrowser(
        mode: FolderBrowserMode
    ) async -> FolderBrowserModel? {
        let available = mode.isCreate
            ? canCreateNewSession(agent: mode.agent)
            : canBrowseNewSessionFolders
        guard available, let gateway else { return nil }
        let reason: String
        switch mode {
        case .create:
            reason = "Browse folders and start a new session on your Mac."
        case .createAgent(let agent):
            reason = "Browse folders and start \(agent.displayName) on your Mac."
        case .chooseDefault:
            reason = "Browse folders on your Mac and choose a default."
        }
        guard await unlockRemoteAccess(reason: reason) else { return nil }

        // Read when the picker opens rather than at discovery, so the list is
        // the one the agent holds now; a Mac without the route gets no row.
        var listModels: FolderBrowserModel.ListingModels?
        if let agent = mode.agent, offersAgentModelChoice {
            listModels = {
                guard self.canCreateNewSession(agent: agent) else {
                    throw NewSessionAccessError.unavailable
                }
                return try await gateway.agentModels(for: agent)
            }
        }
        let browser = FolderBrowserModel(
            mode: mode,
            initialPath: newSessionFolderStore.load(),
            folderStore: newSessionFolderStore,
            browse: { path, cursor in
                guard self.canBrowseNewSessionFolders else {
                    throw NewSessionAccessError.unavailable
                }
                return try await gateway.browseDirectories(path: path, cursor: cursor)
            },
            create: { requestID, cwd, model in
                guard self.canCreateNewSession(agent: mode.agent) else {
                    throw NewSessionAccessError.unavailable
                }
                return try await gateway.createSession(
                    requestID: requestID,
                    cwd: cwd,
                    agent: mode.agent,
                    model: model
                )
            },
            hasAccess: {
                mode.isCreate
                    ? self.canCreateNewSession(agent: mode.agent)
                    : self.canBrowseNewSessionFolders
            },
            didCreate: { sessionID in
                await self.refreshSessions()
                self.highlightedSessionID = sessionID
            },
            listModels: listModels,
            modelStore: agentModelStore
        )
        await browser.load()
        return browser
    }

    public func clearNewSessionHighlight() {
        highlightedSessionID = nil
    }
}
