import Foundation
import Observation

/// Shared owner-unlock and authenticated link checks for scoped reads.
@MainActor
@Observable
final class ScopedReadAuthorization {
    private weak var app: AppModel?
    private let generation: Int
    private let ownerDeadline: Date
    private let ownerGeneration: Int
    private var invalidated = false
    private var expired = false
    @ObservationIgnored private var expiration: Task<Void, Never>?

    init(app: AppModel, generation: Int, ownerDeadline: Date, ownerGeneration: Int) {
        self.app = app
        self.generation = generation
        self.ownerDeadline = ownerDeadline
        self.ownerGeneration = ownerGeneration
        let remaining = app.terminalUnlock.timeRemaining(until: ownerDeadline)
        expiration = Task { [weak self] in
            do {
                try await Task.sleep(for: remaining)
                self?.expired = true
            } catch {
                return
            }
        }
    }

    deinit { expiration?.cancel() }

    func invalidate() {
        invalidated = true
        expiration?.cancel()
    }

    func revokeAccess() {
        app?.scopedReadGeneration &+= 1
        app?.terminalUnlock.lock()
    }

    func currentGateway(for endpoint: GatewayEndpointsName) throws -> LatchGateway {
        guard !invalidated, let app, generation == app.scopedReadGeneration else {
            throw ScopedReadAccessError.invalidated
        }
        guard !expired, app.terminalUnlock.isCurrent(deadline: ownerDeadline, generation: ownerGeneration) else {
            throw ScopedReadAccessError.ownerAuthorizationExpired
        }
        return try app.currentScopedReadGateway(for: endpoint)
    }

    func read<Value>(
        _ endpoint: GatewayEndpointsName, operation: (LatchGateway) async throws -> Value
    ) async throws -> Value {
        try Task.checkCancellation()
        try await app?.checkScopedReadLink()
        let gateway = try currentGateway(for: endpoint)
        let result: Result<Value, Error>
        do {
            result = .success(try await operation(gateway))
        } catch {
            result = .failure(error)
        }
        // The coordinator may advance while a response is in flight.
        try await app?.checkScopedReadLink()
        _ = try currentGateway(for: endpoint)
        try Task.checkCancellation()
        return try result.get()
    }
}

enum ScopedReadAccessError: Error {
    case ownerCancelled
    case ownerAuthenticationUnavailable
    case ownerAuthorizationExpired
    case accessDenied
    case transportUnavailable
    case invalidated
    case unsupported(GatewayEndpointsName)
}

extension AppModel {
    func authorizeScopedRead(
        endpoint: GatewayEndpointsName, reason: String
    ) async throws -> ScopedReadAuthorization {
        try Task.checkCancellation()
        _ = try currentScopedReadGateway(for: endpoint)
        let generation = scopedReadGeneration
        try await checkScopedReadLink()
        guard generation == scopedReadGeneration else { throw ScopedReadAccessError.invalidated }
        try Task.checkCancellation()
        let unlocked = await unlockRemoteAccess(reason: reason)
        try await checkScopedReadLink()
        guard generation == scopedReadGeneration else { throw ScopedReadAccessError.invalidated }
        _ = try currentScopedReadGateway(for: endpoint)
        try Task.checkCancellation()
        guard unlocked, let deadline = terminalUnlock.unlockedUntil else {
            if ownerAuthenticationFailure != nil { throw ScopedReadAccessError.ownerAuthenticationUnavailable }
            throw ScopedReadAccessError.ownerCancelled
        }
        return ScopedReadAuthorization(
            app: self, generation: generation, ownerDeadline: deadline, ownerGeneration: terminalUnlock.lockGeneration
        )
    }

    /// The coordinator can advance before its observation reaches AppModel.
    /// Check the authenticated link directly at a scoped read boundary.
    func checkScopedReadLink() async throws {
        let generation = scopedReadGeneration
        let current = await coordinator.current
        guard generation == scopedReadGeneration, current.state == .ready,
              current.generation == scopedReadReadyGeneration else {
            throw ScopedReadAccessError.invalidated
        }
        if let permission = current.permission, permission != .control {
            throw ScopedReadAccessError.accessDenied
        }
    }

    func currentScopedReadGateway(for endpoint: GatewayEndpointsName) throws -> LatchGateway {
        switch linkState {
        case .revoked, .pairingRequired, .unlinked: throw ScopedReadAccessError.accessDenied
        default: break
        }
        guard let pairedDevice, pairedDevice.isActive, pairedDevice.permission == .control else {
            throw ScopedReadAccessError.accessDenied
        }
        guard linkState.isUsable, linkSnapshot.state == .ready,
              scopedReadReadyGeneration == linkSnapshot.generation, let gateway else {
            throw ScopedReadAccessError.transportUnavailable
        }
        guard GatewayCompatibility.supports(endpoint: endpoint, capabilities: linkState.cachedCapabilities) else {
            throw ScopedReadAccessError.unsupported(endpoint)
        }
        return gateway
    }
}
