import Foundation

/// How the session list and the open session share the window.
public enum SessionBrowserLayout: Equatable, Sendable {
    /// The list fills the window. Opening a session covers it, and Back
    /// returns to the list. This is the phone, and any iPad window narrow
    /// enough that two columns would not fit.
    case stack
    /// The list stays on screen beside the open session. Back is only for a
    /// screen pushed over that session, such as the terminal taken from chat.
    case columns
}

public enum SessionBrowserLayoutPolicy {
    /// Two columns when the window is wide and there is a session list to
    /// put in one of them. A narrow window, and every full-screen link
    /// state (unpaired, connecting, revoked), keeps the single column.
    public static func layout(isRegularWidth: Bool, showsSessionList: Bool) -> SessionBrowserLayout {
        isRegularWidth && showsSessionList ? .columns : .stack
    }

    /// Back returns to the list. Beside an open list that control is
    /// redundant, except where another screen has been pushed over the session.
    public static func showsSessionBackButton(
        layout: SessionBrowserLayout,
        isPushedOverSession: Bool
    ) -> Bool {
        switch layout {
        case .stack: true
        case .columns: isPushedOverSession
        }
    }
}
