import LatchMobileKit
import SwiftUI

/// The status letter in its small rounded chip. The letter and the row's
/// VoiceOver value carry the meaning; the colour only repeats it.
struct GitStatusBadge: View {
    let entry: GitStatusEntry

    var body: some View {
        Text(entry.badge)
            .font(.caption.monospaced().weight(.bold))
            .foregroundStyle(isConflict ? Color.white : color)
            .frame(minWidth: 22, minHeight: 22)
            .background {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(isConflict ? color : color.opacity(0.14))
            }
            .accessibilityHidden(true)
    }

    private var isConflict: Bool { entry.change == .conflicted }

    private var color: Color {
        switch entry.change {
        case .modified, .typeChanged: .blue
        case .added, .copied: .green
        case .deleted, .conflicted: Color(red: 0.92, green: 0.30, blue: 0.20)
        case .renamed: .purple
        case .untracked: .gray
        }
    }
}

/// An outlined pill, as the link status pill draws one.
struct GitOutlinedPill: View {
    let text: String
    var dot: Color?
    var systemImage: String?

    var body: some View {
        HStack(spacing: 6) {
            if let dot {
                Circle().fill(dot).frame(width: 8, height: 8)
            }
            if let systemImage {
                Image(systemName: systemImage).imageScale(.small)
            }
            Text(text).lineLimit(1)
        }
        .font(.footnote.weight(.medium))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .overlay(
            Capsule(style: .continuous)
                .strokeBorder(.secondary.opacity(0.35), lineWidth: 1)
        )
    }
}

/// The reconnecting pill shown over a status that is no longer live.
struct GitStalePill: View {
    let text: String

    var body: some View {
        GitOutlinedPill(text: text, dot: .orange)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(text)
            .accessibilityIdentifier("git.stale")
    }
}

private struct OpenSessionKey: EnvironmentKey {
    static let defaultValue: ((String) -> Void)? = nil
}

extension EnvironmentValues {
    /// Opens a session by id over whatever is on screen. Set by the session
    /// browser, so a chip on the repository screen can go to its session.
    var openSession: ((String) -> Void)? {
        get { self[OpenSessionKey.self] }
        set { self[OpenSessionKey.self] = newValue }
    }
}

/// The bottom bar's soft fade, the mirror of the chrome bar's, so rows
/// scrolling under the floating controls do not show through their caption.
struct BottomFade: View {
    var body: some View {
        LinearGradient(
            stops: [
                .init(color: Color(.systemBackground).opacity(0), location: 0),
                .init(color: Color(.systemBackground).opacity(0.92), location: 0.45),
            ],
            startPoint: .top,
            endPoint: .bottom
        )
        .ignoresSafeArea(edges: .bottom)
        .allowsHitTesting(false)
    }
}
