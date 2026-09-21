import Foundation

/// Whether the transcript viewport should follow new items at the tail.
///
/// Pure state: the view reports what the reader did and what the store
/// published, and applies the returned action. A reader inspecting earlier
/// history is never moved by new output; Jump to latest restores following.
public struct ConversationTailFollowState: Equatable, Sendable {
    public enum Action: Equatable, Sendable {
        case none
        case scrollToTail(animated: Bool)
        /// Keep the row that was first on screen before a history prepend.
        case restoreAnchor(String)
    }

    /// Points from the bottom still counted as "at the tail".
    public static let defaultThreshold: Double = 64

    public let threshold: Double
    public private(set) var isFollowing = true
    /// Items arrived while follow was released.
    public private(set) var hasUnseenItems = false

    public init(threshold: Double = Self.defaultThreshold) {
        self.threshold = threshold
    }

    /// Jump to latest is offered exactly when follow is released.
    public var showsJumpToLatest: Bool { !isFollowing }

    /// The viewport moved. Only a reader's own scroll releases follow; a
    /// programmatic scroll or a layout change never does. Returning within
    /// the threshold resumes following, however it got there.
    public mutating func viewportMoved(distanceFromBottom: Double, isUserDriven: Bool) {
        if distanceFromBottom <= threshold {
            isFollowing = true
            hasUnseenItems = false
        } else if isUserDriven {
            isFollowing = false
        }
    }

    /// New items were published at the tail. `tailIsRendered` is false when
    /// the store is showing an older window, where there is no tail to scroll
    /// to.
    public mutating func itemsAppended(tailIsRendered: Bool = true, reduceMotion: Bool = false) -> Action {
        guard isFollowing, tailIsRendered else {
            hasUnseenItems = true
            return .none
        }
        return .scrollToTail(animated: !reduceMotion)
    }

    /// A history page was placed before the current first row. The viewport
    /// stays on what the reader was looking at, and following is unchanged.
    public mutating func historyPrepended(anchorID: String?) -> Action {
        guard let anchorID else { return .none }
        return .restoreAnchor(anchorID)
    }

    public mutating func jumpToLatest(reduceMotion: Bool = false) -> Action {
        isFollowing = true
        hasUnseenItems = false
        return .scrollToTail(animated: !reduceMotion)
    }
}
