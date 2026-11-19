import LatchMobileKit
import SwiftUI

/// An outlined pill: a colored dot for the link state and, when the link is
/// live, the name of the Mac on the other end of it.
struct LinkStatusPill: View {
    let status: SessionListLinkStatus
    let deviceName: String?

    var body: some View {
        OutlinedPill(text: status.pillText(deviceName: deviceName), dot: color)
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
