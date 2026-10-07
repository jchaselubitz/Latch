import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(stateColor)
                .frame(width: 9, height: 9)
                .accessibilityLabel(session.state.rawValue)
            VStack(alignment: .leading, spacing: 2) {
                Text(session.name).fontWeight(.medium).lineLimit(1)
                Text(session.displaySubtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(session.state.rawValue)
                if let idle = idleDescription {
                    Text(idle)
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }

    private var stateColor: Color {
        switch session.state {
        case .running: return .green
        case .exited: return .secondary
        case .lost: return .red
        }
    }

    private var idleDescription: String? { session.displayIdleLabel }
}
