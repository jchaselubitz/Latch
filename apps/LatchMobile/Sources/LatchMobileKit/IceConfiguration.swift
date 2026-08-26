import Foundation

/// Holds the ICE configuration across the channels one route opens.
///
/// The transport opens a fresh channel — and so a fresh ICE agent — for every
/// loopback request. Asking the control plane to mint TURN credentials each
/// time would put a round trip on each of those critical paths and write a
/// credential row per request for a credential that is valid for minutes.
/// Reusing the unexpired one costs nothing in exposure: it is the same
/// short-lived credential the same device would have been issued again.
public actor IceConfiguration {
    /// Discard a credential slightly before it expires, so a channel opened at
    /// the boundary does not gather against a TURN server that stops
    /// answering mid-allocation.
    private static let expiryMargin: TimeInterval = 15
    /// STUN is static configuration rather than a credential, but it is not
    /// immutable, so it is held for a bounded window rather than forever.
    private static let stunLifetime: TimeInterval = 300

    private let now: @Sendable () -> Date
    private var stunServers: [IceServer] = []
    private var stunFetchedAt: Date?
    private var relayServers: [IceServer] = []
    private var relayExpiresAt: Date?

    /// - Parameter now: injected so the expiry rules can be tested without
    ///   waiting out a credential lifetime.
    public init(now: @escaping @Sendable () -> Date = Date.init) {
        self.now = now
    }

    public func stun(
        _ fetch: () async throws -> [IceServer]
    ) async throws -> [IceServer] {
        if let stunFetchedAt, now().timeIntervalSince(stunFetchedAt) < Self.stunLifetime {
            return stunServers
        }
        let servers = try await fetch()
        stunServers = servers
        stunFetchedAt = now()
        return servers
    }

    /// Relay servers, and whether their absence was a refusal.
    ///
    /// A missing relay is never fatal. `relayDisabled` is the account kill
    /// switch answering, and a transport or service failure still leaves a
    /// direct attempt worth making, so both degrade to a direct-only
    /// gathering rather than to an error the person has to read. The two are
    /// distinguished because only the second is worth asking about again.
    /// Neither is cached: the switch can be turned back on, and a service that
    /// was down can come back, and the next channel should find that out.
    public func relay(
        _ fetch: () async throws -> TurnCredentials
    ) async -> (servers: [IceServer], refused: Bool) {
        if let relayExpiresAt, relayExpiresAt > now() {
            return (relayServers, false)
        }
        do {
            let credentials = try await fetch()
            relayServers = credentials.iceServers
            relayExpiresAt = Date(timeIntervalSince1970: TimeInterval(credentials.expiresAt))
                .addingTimeInterval(-Self.expiryMargin)
            return (relayServers, false)
        } catch ControlPlaneError.relayDisabled {
            return ([], true)
        } catch {
            return ([], false)
        }
    }
}
