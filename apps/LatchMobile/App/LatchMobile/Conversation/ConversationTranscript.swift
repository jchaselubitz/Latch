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
            // Centred in what the floating chrome and composer leave visible,
            // not at the top of the scroll content.
            .overlay {
                if transcript.turns.isEmpty {
                    ConversationEmptyTranscript(viewState: viewState)
                        .allowsHitTesting(false)
                }
            }
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

/// What an empty transcript shows depends on why it is empty. Only the
/// states with something to explain say anything; an ordinary empty
/// conversation is a faint Latch mark, and the composer below it is the
/// invitation.
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
            case .starting, .empty, .ready, .working, .awaitingInput, .interrupted:
                LatchMark()
                    .foregroundStyle(.quaternary)
                    .frame(width: 52, height: 72)
                    .accessibilityElement()
                    .accessibilityLabel("No messages yet")
            }
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The Latch "L": a tall rounded bar with a shorter one along its foot,
/// drawn as a shape so it takes any tint and stays sharp at any size.
struct LatchMark: Shape {
    func path(in rect: CGRect) -> Path {
        // Proportions of the app mark: the stem is ~38% of the width, the
        // foot ~22% of the height.
        let stem = CGRect(x: rect.minX, y: rect.minY, width: rect.width * 0.38, height: rect.height)
        let footHeight = rect.height * 0.22
        let foot = CGRect(x: rect.minX, y: rect.maxY - footHeight, width: rect.width, height: footHeight)
        var path = Path()
        path.addRoundedRect(in: stem, cornerSize: CGSize(width: stem.width / 2, height: stem.width / 2))
        path.addRoundedRect(in: foot, cornerSize: CGSize(width: footHeight / 2, height: footHeight / 2))
        return path
    }
}
