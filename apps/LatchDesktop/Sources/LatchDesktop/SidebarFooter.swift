import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SidebarFooter: View {
    let canStopSelected: Bool
    let stopSelected: () -> Void
    let prune: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 8) {
                Button("Stop Selected…", role: .destructive, action: stopSelected)
                    .disabled(!canStopSelected)
                Button("Prune…", action: prune)
                Spacer(minLength: 0)
            }
            .controlSize(.small)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .background(.bar)
    }
}
