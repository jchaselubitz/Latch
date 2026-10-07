import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct CLISetupView: View {
    @ObservedObject var store: SessionStore

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Connect the Latch CLI")
                .font(.title2)
                .fontWeight(.semibold)
            if store.isDiscoveringCLI {
                ProgressView("Running `where latch`…")
            } else if store.discoveredLatchExecutables.isEmpty {
                Text("Latch Desktop does not bundle the CLI, and `where latch` returned no paths. Run this verified-release installer in Terminal:")
                Text(store.cliInstallCommand)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(10)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                HStack {
                    Button("Run in Terminal…") { store.runCLIInstaller() }
                        .buttonStyle(.borderedProminent)
                    Button("Search Again") {
                        Task { await store.discoverCLI() }
                    }
                }
            } else {
                Text("`where latch` found the following executables. Choose the one Latch Desktop should use:")
                ForEach(store.discoveredLatchExecutables, id: \.self) { path in
                    Button {
                        store.selectCLI(path)
                    } label: {
                        HStack {
                            Image(systemName: "terminal")
                            Text(path).textSelection(.enabled)
                            Spacer()
                            Text("Use").foregroundStyle(.secondary)
                        }
                    }
                    .buttonStyle(.bordered)
                }
            }
            if !store.discoveredLatchExecutables.isEmpty {
                Text("To install or replace an outdated CLI, run:")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(store.cliInstallCommand)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                Button("Run Installer in Terminal…") { store.runCLIInstaller() }
            }
            HStack {
                Spacer()
                Button("Not Now") { store.shouldPresentCLISetup = false }
            }
        }
        .padding(24)
        .frame(width: 540)
    }
}
