import Foundation
import Observation

/// The app's link to one computer, shared by both tabs.
///
/// Discovery lives here rather than in each screen: it is the mandatory first
/// step, its answer decides what the session screen may offer, and repeating it
/// per screen would turn one contract question into several.
@MainActor
@Observable
public final class AppModel {
    public enum LinkState: Equatable {
        case unlinked
        case connecting
        case linked(GatewayCapabilities)
        case failed(String)
    }

    public private(set) var linkState: LinkState = .unlinked
    public private(set) var link: GatewayLink?
    public private(set) var gateway: LatchGateway?
    public private(set) var sessions: [SessionSummary] = []
    public private(set) var sessionsError: String?
    public private(set) var isLoadingSessions = false

    private let storage: LinkStorage
    private let sessionFactory: @Sendable (GatewayLink) -> LatchGateway

    public init(
        storage: LinkStorage = KeychainLinkStorage(),
        sessionFactory: @escaping @Sendable (GatewayLink) -> LatchGateway = { LatchGateway(link: $0) }
    ) {
        self.storage = storage
        self.sessionFactory = sessionFactory
    }

    /// What discovery permits on the session screen.
    public var surface: SessionSurface {
        guard case .linked(let capabilities) = linkState else {
            return SessionSurface(chat: false, composer: false, interactionControls: false)
        }
        return GatewayCompatibility.sessionSurface(for: capabilities)
    }

    /// The gateway's product version, for Settings.
    public var productVersion: String? {
        guard case .linked(let capabilities) = linkState else { return nil }
        return capabilities.productVersion.isEmpty ? nil : capabilities.productVersion
    }

    /// Restores a saved link at launch and connects to it.
    public func restore() async {
        guard link == nil, let saved = try? storage.load() else { return }
        await connect(to: saved, persist: false)
    }

    /// Links to a computer and runs discovery.
    public func link(address: String, token: String) async {
        do {
            let link = try GatewayLink(address: address, token: token)
            await connect(to: link, persist: true)
        } catch let error as LatchError {
            linkState = .failed(error.message)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
    }

    /// Forgets the computer and everything fetched from it.
    public func unlink() {
        try? storage.clear()
        link = nil
        gateway = nil
        sessions = []
        sessionsError = nil
        linkState = .unlinked
    }

    private func connect(to link: GatewayLink, persist: Bool) async {
        linkState = .connecting
        let gateway = sessionFactory(link)
        do {
            let capabilities = try await gateway.discover()
            self.link = link
            self.gateway = gateway
            linkState = .linked(capabilities)
            if persist {
                try? storage.save(link)
            }
            await refreshSessions()
        } catch let error as LatchError {
            linkState = .failed(error.message)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
    }

    /// Reloads the session list.
    public func refreshSessions() async {
        guard let gateway else { return }
        isLoadingSessions = true
        defer { isLoadingSessions = false }
        do {
            sessions = try await gateway.listSessions()
            sessionsError = nil
        } catch let error as LatchError {
            sessionsError = error.message
        } catch {
            sessionsError = error.localizedDescription
        }
    }

    /// Repeats discovery after the connection was interrupted.
    ///
    /// The contract requires this before the app resumes application traffic
    /// on a reconnected path: capabilities may have changed while the phone was
    /// away, and carrying the old answers across would assume a feature the
    /// gateway no longer offers.
    public func rediscover() async {
        guard let gateway, let link else { return }
        await gateway.invalidateDiscovery()
        await connect(to: link, persist: false)
    }
}
