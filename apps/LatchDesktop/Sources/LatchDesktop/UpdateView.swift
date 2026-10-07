import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// The one place an update is described and accepted.
///
/// It states the version, links the release notes rather than reproducing
/// them, and never installs without being asked — an app that replaces itself
/// while somebody is working in it is worse than an app that is a day old.
struct UpdateView: View {
    @ObservedObject var updates: UpdateController

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(title).font(.title2).fontWeight(.semibold)
            content
            HStack {
                Spacer()
                buttons
            }
        }
        .padding(24)
        .frame(width: 460)
    }

    private var title: String {
        switch updates.phase {
        case .available: return "Update Available"
        case .failed: return "Update Failed"
        case .upToDate: return "Latch Is Up to Date"
        default: return "Checking for Updates"
        }
    }

    @ViewBuilder private var content: some View {
        switch updates.phase {
        case .idle, .checking:
            ProgressView().progressViewStyle(.linear)
        case .upToDate:
            Text("Latch \(updates.installedVersionLabel) is the newest release.")
        case .available(let update):
            VStack(alignment: .leading, spacing: 10) {
                Text("Latch \(update.version.description) is available. You have \(updates.installedVersionLabel).")
                Link("Release notes", destination: update.releasePage)
            }
        case .downloading:
            VStack(alignment: .leading, spacing: 10) {
                Text("Downloading and verifying the update…")
                ProgressView().progressViewStyle(.linear)
            }
        case .failed(let message):
            Text(message).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var buttons: some View {
        switch updates.phase {
        case .available(let update):
            Button("Not Now") { updates.dismiss() }
            Button("Install and Relaunch") {
                Task { await updates.install(update) }
            }
            .buttonStyle(.borderedProminent)
        case .downloading:
            Button("Cancel", role: .cancel) { updates.dismiss() }.disabled(true)
        default:
            Button("OK") { updates.dismiss() }
        }
    }
}
