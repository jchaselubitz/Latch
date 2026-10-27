import SwiftUI
import AppKit

/// Shared visual vocabulary for sessions: the state pill and the Liquid Glass
/// helpers that fall back to materials before macOS 26. The agents' logos
/// live in AgentLogo.swift.

extension SessionState {
    var label: String { rawValue.capitalized }

    var color: Color {
        switch self {
        case .running: return .green
        case .exited: return .gray
        case .lost: return .red
        }
    }

    var symbolName: String {
        switch self {
        case .running: return "play.circle"
        case .exited: return "stop.circle"
        case .lost: return "exclamationmark.triangle"
        }
    }
}

struct StatePill: View {
    let state: SessionState

    var body: some View {
        HStack(spacing: 4) {
            Circle().frame(width: 6, height: 6)
            Text(state.label)
        }
        .font(.caption2.weight(.semibold))
        .foregroundStyle(state.color)
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .background(state.color.opacity(0.14), in: Capsule())
    }
}

/// An icon button with its caption underneath, used for the inspector's
/// row of session actions.
struct CaptionedIconButton: View {
    let title: String
    let systemImage: String
    var role: ButtonRole?
    let action: () -> Void

    var body: some View {
        VStack(spacing: 5) {
            Button(role: role, action: action) {
                Label(title, systemImage: systemImage)
                    .labelStyle(.iconOnly)
                    .font(.system(size: 14, weight: .medium))
                    .frame(width: 22, height: 22)
            }
            .latchCircleButtonShape()
            .controlSize(.large)
            .latchGlassButtonStyle()
            .tint(role == .destructive ? .red : nil)
            .help(title)
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        }
        .frame(maxWidth: .infinity)
    }
}

extension View {
    /// Liquid Glass on macOS 26, a material on earlier systems.
    @ViewBuilder func latchGlass<S: Shape>(in shape: S) -> some View {
        if #available(macOS 26.0, *) {
            glassEffect(.regular, in: shape)
        } else {
            background(.regularMaterial, in: shape)
        }
    }

    @ViewBuilder func latchGlassButtonStyle() -> some View {
        if #available(macOS 26.0, *) {
            buttonStyle(.glass)
        } else {
            buttonStyle(.bordered)
        }
    }

    @ViewBuilder func latchProminentButtonStyle() -> some View {
        if #available(macOS 26.0, *) {
            buttonStyle(.glassProminent)
        } else {
            buttonStyle(.borderedProminent)
        }
    }

    /// A tinted toolbar button on macOS 26; earlier toolbars keep their
    /// standard buttons, where a filled button looks out of place.
    @ViewBuilder func latchProminentToolbarButton() -> some View {
        if #available(macOS 26.0, *) {
            buttonStyle(.glassProminent)
        } else {
            self
        }
    }

    @ViewBuilder func latchCircleButtonShape() -> some View {
        if #available(macOS 14.0, *) {
            buttonBorderShape(.circle)
        } else {
            self
        }
    }

    /// A quiet grouped surface for content inside glass panels.
    func latchCard(cornerRadius: CGFloat = 12) -> some View {
        background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
    }
}
