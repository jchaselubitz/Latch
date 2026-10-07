import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct ResizeSessionView: View {
    @ObservedObject var store: SessionStore
    let session: InspectReport
    @Binding var isPresented: Bool
    @State private var request: ResizeSessionRequest

    init(store: SessionStore, session: InspectReport, isPresented: Binding<Bool>) {
        self.store = store
        self.session = session
        _isPresented = isPresented
        let size = session.size ?? session.initialSize
        _request = State(initialValue: ResizeSessionRequest(cols: size.cols, rows: size.rows))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Resize \(session.name)")
                .font(.title2)
                .fontWeight(.semibold)
            Form {
                TextField("Columns", value: $request.cols, format: .number)
                TextField("Rows", value: $request.rows, format: .number)
                Toggle("Pin this size", isOn: $request.pin)
                Text("Without pinning, an attached terminal can change the session size later. Pinning keeps the requested session geometry.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            HStack {
                Button("Cancel", role: .cancel) { isPresented = false }
                Spacer()
                Button("Resize") {
                    let submitted = request
                    isPresented = false
                    Task { await store.resize(session.id, request: submitted) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(request.cols == 0 || request.rows == 0)
            }
        }
        .padding(24)
        .frame(width: 420)
    }
}
