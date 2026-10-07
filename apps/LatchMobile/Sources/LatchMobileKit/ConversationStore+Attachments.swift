import Foundation

// MARK: - Attachments

extension ConversationStore {
    public var isUploadingAttachments: Bool { attachmentPhase == .uploading }

    /// Adds a file to the next message. A file larger than the Mac accepts is
    /// refused here, with the reason in `attachmentPhase`, rather than
    /// uploaded only to be turned away.
    @discardableResult
    public func addAttachment(_ attachment: ConversationAttachment, maximumBytes: Int?) -> Bool {
        guard !isUploadingAttachments else { return false }
        if let maximumBytes, attachment.byteCount > maximumBytes {
            let limit = ByteCountFormatter.string(fromByteCount: Int64(maximumBytes), countStyle: .file)
            attachmentPhase = .failed("\(attachment.name) is larger than the \(limit) your Mac accepts.")
            return false
        }
        attachments.append(attachment)
        attachmentPhase = .idle
        return true
    }

    /// Takes a file back out of the next message. Not while it is uploading.
    public func removeAttachment(_ id: UUID) {
        guard !isUploadingAttachments else { return }
        attachments.removeAll { $0.id == id }
        if case .failed = attachmentPhase { attachmentPhase = .idle }
    }

    /// Uploads every file the Mac does not already have, then sends the text
    /// with their paths appended as one ordinary message.
    ///
    /// The order matters: the agent must never read a path before the file is
    /// there. So nothing is sent until every upload has succeeded. Any failure
    /// sends nothing, returns the text to the draft, and keeps the files —
    /// with the receipts of those that did arrive, so a retry uploads only
    /// what is missing.
    func sendWithAttachments(text: String) {
        guard !isUploadingAttachments, canSend else {
            restoreDraft(text)
            return
        }
        attachmentPhase = .uploading
        let uploader: any ConversationAttachmentUploading = attachmentUploader ?? gateway
        let sessionID = sessionID
        attachmentSendTask = Task { [weak self] in
            guard let self else { return }
            // Adding and removing are refused while uploading, so these
            // indices stay valid across each await.
            for index in attachments.indices where attachments[index].receipt == nil {
                let attachment = attachments[index]
                do {
                    attachments[index].receipt = try await uploader.uploadAttachment(
                        sessionID: sessionID,
                        name: attachment.name,
                        data: attachment.data
                    )
                } catch {
                    failAttachmentSend(text: text, reason: Self.uploadFailureReason(error))
                    return
                }
            }
            // The agent may have become busy while the files uploaded. They
            // stay on the Mac and in the composer; the next send reuses them.
            guard canSend, let operationEpoch else {
                failAttachmentSend(
                    text: text,
                    reason: sendReason ?? "The agent cannot take a message right now."
                )
                return
            }
            let message = ConversationAttachmentMessage.compose(
                text: text,
                paths: attachments.compactMap { $0.receipt?.path }
            )
            attachments = []
            attachmentPhase = .idle
            enqueue(text: message, operationEpoch: operationEpoch)
        }
    }

    private func failAttachmentSend(text: String, reason: String) {
        attachmentPhase = .failed(reason)
        restoreDraft(text)
    }

    /// Puts unsent text back in front of anything typed since.
    private func restoreDraft(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? text
            : text + "\n\n" + draft
    }

    private static func uploadFailureReason(_ error: Error) -> String {
        if let error = error as? LatchError {
            if case .refused(let reason) = error { return reason }
            return "The attachment did not reach your Mac. \(error.message)"
        }
        return "The attachment did not reach your Mac. \(error.localizedDescription)"
    }
}
