import LatchMobileKit
import SwiftUI
import UIKit

/// What the composer's "+" menu can do to the session itself. Each action is
/// nil when this phone cannot offer it, and the menu leaves it out rather
/// than showing something that would only fail.
struct ConversationSessionActions {
    var takeTerminal: (() -> Void)?
    /// The `latch://` link that reopens this session from another app.
    var sessionLink: URL?
    /// Asks to stop; the caller confirms before anything is sent.
    var stop: (() -> Void)?
    var isStopping = false

    var isEmpty: Bool { takeTerminal == nil && sessionLink == nil && stop == nil }
}

/// The ordinary composer, and the one thing the chat screen is built around.
///
/// The field stays editable whatever the host says, including while offline;
/// only the send button follows availability, and the reason it is off reads
/// as a quiet line above the field, not an error.
struct ConversationComposer: View {
    @Binding var draft: String
    @FocusState.Binding var focused: Bool
    let presentation: ConversationComposerPresentation
    var placeholder = "Message"
    var sessionActions = ConversationSessionActions()
    var attachments = ConversationAttachmentControls()
    let send: (String) -> Void

    /// The picker the "+" menu asked for, presented by the modifier below.
    @State private var attachmentSource: ConversationAttachmentSource?

    /// The field's resting height; the circular buttons match it. It grows
    /// with Dynamic Type and never falls below the 44pt hit target.
    @ScaledMetric(relativeTo: .body) private var scaledFieldHeight: CGFloat = 48
    /// Capped where the floating buttons stop growing, so the field keeps
    /// its width between them; longer text still grows the field upward.
    private var fieldHeight: CGFloat { min(scaledFieldHeight, 48 * FloatingControlMetrics.largestScale) }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let notice = presentation.notice {
                ConversationSendNoticeView(notice: notice)
                    .padding(.horizontal, fieldHeight + 8)
            }
            if !attachments.items.isEmpty || attachments.phase != .idle {
                ConversationAttachmentStrip(controls: attachments)
                    .padding(.leading, fieldHeight + 8)
            }
            HStack(alignment: .bottom, spacing: 8) {
                moreMenu
                field
                sendButton
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 8)
        .conversationAttachmentPickers(source: $attachmentSource, add: attachments.add)
    }

    private var field: some View {
        TextField(placeholder, text: $draft, axis: .vertical)
            .lineLimit(1...6)
            .textFieldStyle(.plain)
            .focused($focused)
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .frame(minHeight: fieldHeight)
            .background(Color(.systemBackground), in: RoundedRectangle(cornerRadius: fieldHeight / 2, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: fieldHeight / 2, style: .continuous)
                    .strokeBorder(focused ? Color.accentColor.opacity(0.6) : Color(.separator), lineWidth: 1)
            }
            .shadow(color: .black.opacity(0.08), radius: 8, y: 2)
            // The padding is part of the target: a tap anywhere in the
            // capsule raises the keyboard. Simultaneous, so cursor placement
            // and selection inside the text still work.
            .contentShape(Rectangle())
            .simultaneousGesture(TapGesture().onEnded { focused = true })
            .accessibilityIdentifier("conversation.composer.field")
    }

    private var moreMenu: some View {
        Menu {
            // Attachments lead: they are about the message being written.
            // A Mac without the route, or a phone without the composer's
            // grant, gets no attachment items at all rather than dead ones.
            if attachments.isAvailable {
                Section {
                    Button { attachmentSource = .photoLibrary } label: {
                        Label("Photo library", systemImage: "photo.on.rectangle")
                    }
                    if ConversationAttachmentSource.cameraAvailable {
                        Button { attachmentSource = .camera } label: {
                            Label("Take photo", systemImage: "camera")
                        }
                    }
                    Button { attachmentSource = .file } label: {
                        Label("File", systemImage: "doc")
                    }
                }
                .disabled(attachments.isUploading)
            }
            if let takeTerminal = sessionActions.takeTerminal {
                Button(action: takeTerminal) {
                    Label("Take terminal", systemImage: "terminal")
                }
                .accessibilityHint("Opens the live terminal here and detaches it from your Mac.")
            }
            if let link = sessionActions.sessionLink {
                Button {
                    UIPasteboard.general.url = link
                } label: {
                    Label("Copy session link", systemImage: "link")
                }
                .accessibilityHint("Copies a latch:// link that opens this session.")
            }
            if let stop = sessionActions.stop {
                Divider()
                Button(role: .destructive, action: stop) {
                    Label(sessionActions.isStopping ? "Stopping…" : "Stop session", systemImage: "stop.circle")
                }
                .disabled(sessionActions.isStopping)
            }
        } label: {
            FloatingControl(systemImage: "plus", size: 48)
                .foregroundStyle(.primary)
        }
        // A menu label otherwise takes the accent colour.
        .tint(.primary)
        .disabled(sessionActions.isEmpty && !attachments.isAvailable)
        .accessibilityLabel("More")
        .accessibilityIdentifier("conversation.composer.more")
    }

    private var sendButton: some View {
        Button {
            let text = draft
            draft = ""
            send(text)
        } label: {
            // Accent-filled only when a send would go through; otherwise
            // the plain floating fill with a quiet glyph.
            FloatingControl(shape: .circle, size: 48, tint: canSubmit ? .accentColor : nil) {
                if attachments.isUploading {
                    ProgressView()
                } else {
                    FloatingControlGlyph(systemImage: "arrow.up")
                }
            }
            .foregroundStyle(canSubmit ? Color.white : Color.secondary)
        }
        .buttonStyle(.plain)
        .disabled(!canSubmit)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: canSubmit)
        .accessibilityLabel(attachments.isUploading ? "Sending attachments" : "Send")
        .accessibilityHint(presentation.notice?.title ?? "")
        .accessibilityIdentifier("conversation.composer.send")
    }

    /// A file alone is a message; text is not required alongside it.
    private var canSubmit: Bool {
        presentation.canSend
            && !attachments.isUploading
            && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.items.isEmpty)
    }
}

/// Why sending is off, as one small line. Agent states are quiet and
/// informational; only a lost connection or an unavailable host uses a
/// warning symbol.
struct ConversationSendNoticeView: View {
    let notice: ConversationSendNotice
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            Image(systemName: symbol)
            Group {
                if let detail = notice.detail {
                    Text(notice.title).fontWeight(.medium) + Text(" · ") + Text(detail)
                } else {
                    Text(notice.title).fontWeight(.medium)
                }
            }
            // More room at accessibility sizes, where two lines hold a few words.
            .lineLimit(dynamicTypeSize.isAccessibilitySize ? 5 : 2)
            Spacer(minLength: 0)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.composer.notice")
    }

    private var symbol: String {
        switch notice.kind {
        case .agentState: "hourglass"
        case .connection: "wifi.slash"
        case .unavailable: "nosign"
        }
    }
}
