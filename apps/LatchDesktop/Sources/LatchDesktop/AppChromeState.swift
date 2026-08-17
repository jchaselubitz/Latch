import Foundation

/// App-level chrome that the menu bar label and `.commands` block may observe.
///
/// Session list churn must never invalidate this object: SwiftUI rebuilds the
/// macOS main menu on every App-body invalidation, and AppKit accumulates
/// NSKeyValueDependency records on each rebuild.
@MainActor
final class AppChromeState: ObservableObject {
    @Published private(set) var runningCount = 0
    @Published private(set) var canCreateSessions = false

    func update(runningCount: Int, canCreateSessions: Bool) {
        if self.runningCount != runningCount {
            self.runningCount = runningCount
        }
        if self.canCreateSessions != canCreateSessions {
            self.canCreateSessions = canCreateSessions
        }
    }
}

/// Owns long-lived controllers without making `App.body` observe them.
@MainActor
final class DesktopRuntime {
    let store = SessionStore()
    let updates = UpdateController()
    let remoteAccess = RemoteAccessController()
}
