import LatchMobileKit
import SwiftUI

/// The scrolling transcript: turns, history paging, and tail following.
///
/// Whether to move the viewport is decided by `ConversationTailFollowState`;
/// this view only reports events to it and applies the action it returns.
struct ConversationTranscript: View {
    let transcript: ConversationTranscriptPresentation
    let viewState: ConversationViewState
    let tailItemID: String?
    let prependAnchor: String?
    let paging: ConversationPaging
    let loadOlder: () -> Void
    let showNewer: () -> Void

    @State private var follow = ConversationTailFollowState()
    @State private var viewportHeight: CGFloat = 0
    @State private var userIsDragging = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    earlierControl
                    if transcript.turns.isEmpty {
                        ConversationEmptyTranscript(viewState: viewState)
                    }
                    ForEach(transcript.turns) { turn in
                        if let prompt = turn.prompt {
                            // Equatable rows let a tail update re-render only
                            // the row whose content changed.
                            ConversationMessageRow(message: prompt)
                                .equatable()
                                .id(prompt.id)
                        }
                        ForEach(turn.entries) { entry in
                            ConversationEntryRow(entry: entry)
                                .equatable()
                                .id(entry.id)
                        }
                    }
                    if paging.hasNewerRendered {
                        Button("Show newer messages", action: showNewer)
                            .buttonStyle(.bordered)
                            .frame(maxWidth: .infinity)
                    }
                    Color.clear
                        .frame(height: 1)
                        .background {
                            GeometryReader { geometry in
                                Color.clear.preference(
                                    key: ConversationTailPositionKey.self,
                                    value: geometry.frame(in: .named("conversation.transcript.scroll")).maxY
                                )
                            }
                        }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .coordinateSpace(name: "conversation.transcript.scroll")
            .background {
                GeometryReader { geometry in
                    Color.clear
                        .onAppear { viewportHeight = geometry.size.height }
                        .onChange(of: geometry.size.height) { _, height in viewportHeight = height }
                }
            }
            .onPreferenceChange(ConversationTailPositionKey.self) { bottom in
                guard let bottom, viewportHeight > 0 else { return }
                follow.viewportMoved(
                    distanceFromBottom: max(0, Double(bottom - viewportHeight)),
                    isUserDriven: userIsDragging
                )
            }
            .simultaneousGesture(
                DragGesture(minimumDistance: 5)
                    .onChanged { _ in userIsDragging = true }
                    .onEnded { _ in userIsDragging = false }
            )
            .accessibilityIdentifier("conversation.transcript")
            .onAppear {
                guard !paging.hasNewerRendered,
                      let row = tailItemID.flatMap(transcript.rowID(containing:))
                else { return }
                proxy.scrollTo(row, anchor: .bottom)
            }
            .overlay(alignment: .bottomTrailing) {
                if follow.showsJumpToLatest, tailItemID != nil {
                    Button {
                        apply(follow.jumpToLatest(reduceMotion: reduceMotion), proxy)
                    } label: {
                        Label("Jump to latest", systemImage: "arrow.down")
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityLabel("Jump to latest")
                    .accessibilityIdentifier("conversation.jumpToLatest")
                    .padding(16)
                }
            }
            .onChange(of: prependAnchor) { _, anchor in
                // Restoring the old first row after a history prepend keeps the
                // reader's viewport stable instead of jumping toward the past.
                apply(follow.historyPrepended(anchorID: anchor.flatMap(transcript.rowID(containing:))), proxy)
            }
            .onChange(of: tailItemID) { _, id in
                guard id != nil else { return }
                apply(follow.itemsAppended(tailIsRendered: !paging.hasNewerRendered, reduceMotion: reduceMotion), proxy)
            }
        }
    }

    @ViewBuilder
    private var earlierControl: some View {
        if paging.hasEarlierRendered || paging.hasMoreBefore {
            Button(paging.hasEarlierRendered ? "Show earlier messages" : "Load earlier messages", action: loadOlder)
                .buttonStyle(.bordered)
                .frame(maxWidth: .infinity)
        } else if paging.isHistoryLimitReached {
            Text("History limit reached on this device")
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
        }
    }

    private func apply(_ action: ConversationTailFollowState.Action, _ proxy: ScrollViewProxy) {
        switch action {
        case .none:
            return
        case .restoreAnchor(let row):
            proxy.scrollTo(row, anchor: .top)
        case .scrollToTail(let animated):
            guard let row = tailItemID.flatMap(transcript.rowID(containing:)) else { return }
            if animated {
                withAnimation(.easeOut(duration: 0.18)) { proxy.scrollTo(row, anchor: .bottom) }
            } else {
                proxy.scrollTo(row, anchor: .bottom)
            }
        }
    }
}

private struct ConversationTailPositionKey: PreferenceKey {
    static var defaultValue: CGFloat? = nil

    static func reduce(value: inout CGFloat?, nextValue: () -> CGFloat?) {
        value = nextValue() ?? value
    }
}

/// One entry in a turn.
struct ConversationEntryRow: View, Equatable {
    let entry: ConversationTurnEntry

    var body: some View {
        switch entry {
        case .message(let message):
            ConversationMessageRow(message: message)
        case .activity(let group):
            ConversationActivityDisclosure(group: group)
        case .request(let request):
            ConversationRequestCard(request: request)
        case .unrecognized:
            ConversationUnrecognizedRow()
        }
    }
}

/// What an empty transcript says depends on why it is empty.
private struct ConversationEmptyTranscript: View {
    let viewState: ConversationViewState

    var body: some View {
        Group {
            switch viewState {
            case .loading:
                ProgressView("Opening conversation…")
            case .failed:
                Label("This conversation is unavailable.", systemImage: "exclamationmark.bubble")
            case .disconnected:
                Label("Waiting for the connection to return.", systemImage: "antenna.radiowaves.left.and.right")
            case .empty, .ready, .working, .awaitingInput, .interrupted:
                Text("No messages yet.")
            }
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity)
        .padding(.top, 40)
    }
}
