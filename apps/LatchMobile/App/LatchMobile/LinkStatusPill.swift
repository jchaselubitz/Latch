import LatchMobileKit
import SwiftUI

/// An outlined pill: a colored dot for the link state and, when the link is
/// live, the name of the Mac on the other end of it.
struct LinkStatusPill: View {
    let status: SessionListLinkStatus
    let deviceName: String?

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
            Text(status.pillText(deviceName: deviceName))
                .lineLimit(1)
        }
        .font(.footnote.weight(.medium))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .overlay(
            Capsule(style: .continuous)
                .strokeBorder(.secondary.opacity(0.35), lineWidth: 1)
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityIdentifier("sessions.linkStatus")
    }

    private var accessibilityLabel: String {
        var parts = [status.label]
        if status == .connected, let deviceName, !deviceName.isEmpty { parts.append("to \(deviceName)") }
        if let detail = status.accessibilityDetail { parts.append(detail) }
        return parts.joined(separator: ". ")
    }

    private var color: Color {
        switch status {
        case .connected: .green
        case .reconnecting: .orange
        case .macUnavailable: .secondary
        }
    }
}
