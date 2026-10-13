import Foundation

// MARK: - Git

extension AppModel {
    /// Whether the linked Mac serves the Git routes, independent of this
    /// phone's grant. A Mac that predates them advertises none, and no
    /// repository entry point appears. The last discovery counts while the
    /// link recovers, so an entry point does not flicker away on a blip.
    public var advertisesGit: Bool {
        let capabilities = linkState.cachedCapabilities
        return [GatewayEndpointsName.gitRepositories, .gitResolve, .gitStatus, .gitDiff]
            .allSatisfy { GatewayCompatibility.supports(endpoint: $0, capabilities: capabilities) }
    }

    /// The Git changes this Mac serves, each advertised separately so a
    /// phone shows only what the Mac it is linked to can do.
    public var gitMutationSupport: GitMutationSupport {
        GitMutationSupport(endpoints: linkState.cachedCapabilities?.endpoints)
    }

    /// Whether this phone may read repositories right now: the routes exist,
    /// the link is up, and the grant is control, the same grant the folder
    /// browser needs.
    public var canUseGit: Bool {
        guard case .linked = linkState else { return false }
        return advertisesGit && pairedDevice?.permission.permits(.control) == true
    }

    /// Why the advertised entry points cannot be used, or nil when they can
    /// (or when the link, not the grant, is what is missing).
    public var gitUnavailableExplanation: String? {
        guard advertisesGit, linkState.isUsable, !canUseGit else { return nil }
        return """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """
    }

    /// The Repositories sheet's model, built only after the device-owner
    /// check, so no repository name is fetched before the person is confirmed.
    public func gitRepositoryList() async -> GitRepositoryListModel? {
        guard canUseGit, let macID = pairedDevice?.mac.deviceId else { return nil }
        guard await unlockRemoteAccess(reason: "Show repositories on your Mac.") else { return nil }
        let model = GitRepositoryListModel(
            listing: { [weak self] in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitRepositories()
            },
            hasAccess: { [weak self] in self?.canUseGit == true && self?.pairedDevice?.mac.deviceId == macID },
            isOnline: { [weak self] in self?.linkState.isUsable == true && self?.pairedDevice?.mac.deviceId == macID }
        )
        await model.load()
        return model
    }

    /// One repository's screen model, behind the same owner check. Opening a
    /// repository never attaches to a terminal.
    public func gitRepository(folder: String) async -> GitRepositoryModel? {
        guard canUseGit, let macID = pairedDevice?.mac.deviceId else { return nil }
        guard await unlockRemoteAccess(reason: "Open a repository on your Mac.") else { return nil }
        let key = "\(macID)\0\(folder)"
        if let existing = gitRepositoryModels[key] {
            await existing.reconnected()
            return existing
        }
        let model = GitRepositoryModel(
            folder: folder,
            read: { [weak self] path in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitStatus(path: path)
            },
            readDiff: { [weak self] path, key, area, cursor in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitDiff(path: path, fileKey: key, area: area, cursor: cursor)
            },
            hasAccess: { [weak self] in self?.canUseGit == true && self?.pairedDevice?.mac.deviceId == macID },
            isOnline: { [weak self] in self?.linkState.isUsable == true && self?.pairedDevice?.mac.deviceId == macID },
            support: gitMutationSupport,
            mutate: { [weak self] action in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitMutation(action)
            },
            lookupReceipt: { [weak self] requestID in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitReceipt(requestID: requestID)
            },
            readBranches: { [weak self] path in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitBranches(path: path)
            },
            readSync: { [weak self] path, cursor in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitSync(path: path, cursor: cursor)
            },
            readPush: { [weak self] path, selection, cursor in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitPushPreview(
                    path: path, remote: selection?.remote, destination: selection?.destination, cursor: cursor
                )
            },
            checkRemote: { [weak self] requestID in
                guard let self else { throw GitAccessError.unavailable }
                return try await self.currentGitGateway(macID: macID).gitPushReconcile(requestId: requestID)
            }
        )
        await model.refresh()
        let canonicalKey = "\(macID)\0\(model.target)"
        if let existing = gitRepositoryModels[canonicalKey] {
            gitRepositoryModels[key] = existing
            await existing.reconnected()
            return existing
        }
        gitRepositoryModels[key] = model
        gitRepositoryModels[canonicalKey] = model
        return model
    }
    /// A reconnect replaces the local adapter and its capability. Resolve
    /// the current gateway for every call, while retaining the Mac binding.
    private func currentGitGateway(macID: String) throws -> LatchGateway {
        guard canUseGit, pairedDevice?.mac.deviceId == macID, let gateway else {
            throw GitAccessError.unavailable
        }
        return gateway
    }

}
