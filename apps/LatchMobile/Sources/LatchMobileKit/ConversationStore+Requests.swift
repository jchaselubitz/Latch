import Foundation

// MARK: - Request answers and turn cancellation

extension ConversationStore {
    private static let maximumResolveAttempts = 20

    public var canCancelTurn: Bool {
        state?.cancelTurn?.enabled == true && operationEpoch != nil && socketState == .open && cancelAttempt?.isInFlight != true
    }

    public func cancelTurn() {
        guard canCancelTurn, let operationEpoch else { return }
        let attempt = ConversationResolveAttempt(id: UUID().uuidString, requestId: "cancel_turn", choice: "Stop")
        cancelAttempt = attempt
        let socket = socket
        Task {
            do {
                guard let socket else { throw ConversationSocketError.notConnected }
                try await socket.send(.cancelTurn(operationEpoch: operationEpoch, operationId: attempt.id))
            } catch let error as ConversationSocketError where error == .notConnected {
                guard cancelAttempt?.id == attempt.id, cancelAttempt?.status == .sending else { return }
                cancelAttempt?.status = .notSent
                cancelAttempt?.reason = "The conversation is not connected."
            } catch {
                guard cancelAttempt?.id == attempt.id, cancelAttempt?.status == .sending else { return }
                cancelAttempt?.status = .ambiguous
                cancelAttempt?.reason = error.localizedDescription
            }
        }
    }

    public var canResolve: Bool { state?.resolveRequest.enabled == true && operationEpoch != nil }

    public var resolveReason: String? { state?.resolveRequest.reason }

    public var pendingRequest: ConversationItem? {
        guard let requestID = state?.pendingRequest else { return nil }
        return transcript.items.last { item in
            if case .request(let id, _, _, _, _, _) = item.kind { return id == requestID }
            return false
        }
    }

    public func resolveAttempt(for requestID: String) -> ConversationResolveAttempt? {
        resolveAttempts.last { $0.requestId == requestID }
    }

    /// Answers the exact request the host names as pending. Each answer is a
    /// new operation. A request whose answer is in flight or already accepted
    /// is not answered again; after a refusal or an uncertain outcome only
    /// another explicit choice answers it.
    public func resolve(requestID: String, choice: String) {
        resolve(requestID: requestID, choice: choice, answers: nil)
    }

    public func resolve(requestID: String, answers: [String: String]) {
        guard case .request(_, _, _, _, _, let questions) = pendingRequest?.kind,
              !questions.isEmpty,
              Set(questions.map(\.question)).count == questions.count,
              Set(answers.keys) == Set(questions.map(\.question)),
              answers.values.allSatisfy({ !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty })
        else { return }
        let summary = questions.map { "\($0.question): \(answers[$0.question] ?? "")" }.joined(separator: "\n")
        resolve(requestID: requestID, choice: summary, answers: answers)
    }

    private func resolve(requestID: String, choice: String, answers: [String: String]?) {
        guard canResolve,
              let operationEpoch,
              state?.pendingRequest == requestID,
              resolveAttempt(for: requestID)?.isInFlight != true
        else { return }
        let attempt = ConversationResolveAttempt(id: UUID().uuidString, requestId: requestID, choice: choice)
        resolveAttempts.removeAll { $0.requestId == requestID }
        resolveAttempts.append(attempt)
        if resolveAttempts.count > Self.maximumResolveAttempts {
            resolveAttempts.removeFirst(resolveAttempts.count - Self.maximumResolveAttempts)
        }
        let socket = socket
        Task {
            guard let socket else {
                sendFailed(attempt.id, status: .notSent, reason: "The conversation is not connected.")
                return
            }
            do {
                try await socket.send(.resolveRequest(
                    operationEpoch: operationEpoch,
                    operationId: attempt.id,
                    requestId: requestID,
                    choice: answers == nil ? choice : nil,
                    answers: answers
                ))
            } catch let error as ConversationSocketError where error == .notConnected {
                sendFailed(attempt.id, status: .notSent, reason: "The conversation is not connected.")
            } catch {
                // A failed write may still have left the phone.
                sendFailed(attempt.id, status: .ambiguous, reason: error.localizedDescription)
            }
        }
    }

    /// A local send failure never overrides an outcome the host already gave.
    private func sendFailed(_ operationID: String, status: ConversationResolveStatus, reason: String) {
        guard resolveAttempts.first(where: { $0.id == operationID })?.status == .sending else { return }
        updateResolveAttempt(operationID, status: status, reason: reason)
    }

    private func updateResolveAttempt(_ operationID: String, status: ConversationResolveStatus, reason: String?) {
        guard let index = resolveAttempts.firstIndex(where: { $0.id == operationID }) else { return }
        resolveAttempts[index].status = status
        resolveAttempts[index].reason = reason
    }

    /// An accepted answer to a request the host no longer names as pending
    /// has done its job; the transcript row now says how it settled. Refused
    /// and uncertain answers stay so the settled row can still explain them.
    func pruneSettledResolveAttempts() {
        if state?.cancelTurn?.enabled != true && cancelAttempt?.status == .accepted { cancelAttempt = nil }
        let pending = state?.pendingRequest
        resolveAttempts.removeAll { $0.status == .accepted && $0.requestId != pending }
    }

    /// A refusal to apply an answer is an expected outcome: the request moved
    /// on, or the choice is no longer on screen. It is recorded, not raised.
    func applyResolveResult(operationID: String, status: String, reason: String?) {
        switch status {
        case "accepted":
            updateResolveAttempt(operationID, status: .accepted, reason: nil)
            pruneSettledResolveAttempts()
        case "refused":
            updateResolveAttempt(operationID, status: .refused, reason: reason ?? "The host did not apply this answer.")
        case "ambiguous":
            updateResolveAttempt(operationID, status: .ambiguous, reason: reason ?? "It is unknown whether the host applied this answer.")
        default:
            updateResolveAttempt(operationID, status: .ambiguous, reason: reason ?? "The host has no record of this answer.")
        }
    }
}
