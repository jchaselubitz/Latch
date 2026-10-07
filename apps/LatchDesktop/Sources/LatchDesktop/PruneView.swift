import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct PruneView: View {
    @ObservedObject var store: SessionStore
    @Binding var isPresented: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Prune Sessions").font(.title2).fontWeight(.semibold)
            if let report = store.prunePreview {
                if report.reclaimed.isEmpty {
                    Text("There are no exited or lost sessions to reclaim.")
                } else {
                    Text("The retained screen and metadata for \(report.reclaimed.count) session(s) will be permanently deleted:")
                    List(report.reclaimed, id: \.self) { Text($0).textSelection(.enabled) }
                        .frame(height: 180)
                }
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { isPresented = false }
                if store.prunePreview?.reclaimed.isEmpty == false {
                    Button("Prune", role: .destructive) {
                        isPresented = false
                        Task { await store.pruneAll() }
                    }
                }
            }
        }
        .padding(24)
        .frame(width: 520)
    }
}
