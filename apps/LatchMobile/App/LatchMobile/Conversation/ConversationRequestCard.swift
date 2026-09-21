import LatchMobileKit
import SwiftUI

/// A request as it sits in the transcript.
///
/// While it is the one the host waits on, the row is only a marker: the full
/// prompt and its choices are in the controls that replace the composer, so
/// the text is not shown twice. Once settled, the row keeps the prompt, how it
/// settled, and what became of an answer this phone gave that did not apply.
struct ConversationRequestCard: View, Equatable {
    let request: ConversationRequestPresentation

    var body: some View {
        if request.isAwaitingAnswer {
            HStack(spacing: 6) {
                Image(systemName: request.kind.symbol)
                Text(request.kind.title)
                Text("· Answer below").foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            .font(.caption.weight(.medium))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.yellow.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("conversation.request.marker")
        } else {
            VStack(alignment: .leading, spacing: 4) {
                Label(request.kind.title, systemImage: request.kind.symbol)
                    .font(.caption.weight(.medium))
                Text(request.prompt)
                    .font(.callout)
                    .textSelection(.enabled)
                if let outcome = request.status.outcome {
                    Text(outcome)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if let answer = request.answer, answer.explainsSettledRequest {
                    ConversationAnswerStatus(answer: answer)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.yellow.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityIdentifier("conversation.request.card")
        }
    }
}

/// The input surface while the host waits on a request. It offers exactly the
/// choices the host sent — no synthesized pair, no free text — and every
/// answer targets the request's exact `requestId`. An answer on its way or
/// applied is not offered again; any other outcome needs a new explicit tap.
struct ConversationRequestControls: View {
    let request: ConversationRequestPresentation
    let canResolve: Bool
    let reason: String?
    let resolve: (_ requestID: String, _ choice: String) -> Void

    @State private var showsFullPrompt = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(request.kind.title, systemImage: request.kind.symbol)
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            Text(request.prompt)
                .font(.callout)
                .lineLimit(showsFullPrompt ? nil : 8)
                .textSelection(.enabled)
                .accessibilityIdentifier("conversation.request.prompt")
            if isLongPrompt {
                Button(showsFullPrompt ? "Show less" : "Show full request") { showsFullPrompt.toggle() }
                    .font(.caption)
                    .accessibilityIdentifier("conversation.request.expand")
            }
            if let answer = request.answer {
                ConversationAnswerStatus(answer: answer)
            }
            if request.answer?.allowsAnotherAnswer ?? true {
                choices
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial)
        .accessibilityIdentifier("conversation.request.controls")
    }

    @ViewBuilder
    private var choices: some View {
        if request.choices.isEmpty {
            // Nothing the host can confirm submitting; say so rather than
            // inventing a yes/no pair it might not recognize.
            Text("This request offers no choices that can be answered from this phone.")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            VStack(spacing: 6) {
                ForEach(request.choices, id: \.self) { choice in
                    Button {
                        resolve(request.requestId, choice)
                    } label: {
                        Text(choice)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.bordered)
                    .disabled(!canResolve)
                    .accessibilityIdentifier("conversation.request.choice")
                }
            }
            if let reason, !canResolve {
                Label(reason, systemImage: "hourglass")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var isLongPrompt: Bool {
        request.prompt.count > 400 || request.prompt.filter(\.isNewline).count >= 8
    }
}

/// What happened to an answer. A refusal is an ordinary outcome, drawn in
/// the same quiet style as the rest of the card rather than as an alert.
struct ConversationAnswerStatus: View {
    let answer: ConversationRequestAnswerPresentation

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if case .submitting = answer.outcome {
                ProgressView().controlSize(.mini)
            } else {
                Image(systemName: symbol)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(answer.title).fontWeight(.medium)
                if let detail = answer.detail {
                    Text(detail).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
        }
        .font(.caption)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.request.answer")
    }

    private var symbol: String {
        switch answer.outcome {
        case .submitting, .submitted: "checkmark.circle"
        case .refused: "arrow.uturn.backward.circle"
        case .uncertain: "questionmark.circle"
        case .notSent: "wifi.slash"
        }
    }
}

extension ConversationRequestKind {
    var symbol: String {
        self == .permission ? "lock.shield" : "questionmark.bubble"
    }
}
