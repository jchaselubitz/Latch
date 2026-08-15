import SwiftUI
import AppKit
import UniformTypeIdentifiers
import CoreImage
import CoreImage.CIFilterBuiltins

/// Settings surface for the Remote Access lifecycle.
///
/// Everything here is explicit: remote access is off until the user turns it
/// on, every paired device shows the permission it actually holds, and the
/// audit trail is visible in the same place the switches are.
struct RemoteAccessSettingsView: View {
    @ObservedObject var controller: RemoteAccessController

    var body: some View {
        Form {
            Section {
                Toggle("Allow remote access", isOn: Binding(
                    get: { controller.isEnabled },
                    set: { enabled in Task { await controller.setEnabled(enabled) } }
                ))
                .disabled(controller.isBusy)

                LabeledContent("Status") {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(statusColor)
                            .frame(width: 8, height: 8)
                        Text(statusText)
                    }
                }

                if let deviceID = controller.status.deviceID {
                    LabeledContent("This Mac") {
                        Text(deviceID)
                            .font(.system(.callout, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }
            } header: {
                SettingsSectionHeader("Remote Access")
            } footer: {
                SettingsFootnote(
                    "Latch supervises a private gateway on a loopback-only port and never publishes it. Phones reach it through an authenticated, encrypted transport that ends inside Latch."
                )
            }

            Section {
                if controller.activeDevices.isEmpty {
                    Text("No devices are paired.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(controller.activeDevices) { device in
                        RemoteDeviceRow(device: device, controller: controller)
                    }
                }
                Button("Pair a Device…") {
                    Task { await controller.createPairing() }
                }
                .disabled(!controller.isEnabled)
            } header: {
                SettingsSectionHeader("Paired Devices")
            } footer: {
                SettingsFootnote(
                    "Observe can watch. Interact can also send messages and answer prompts. Control can also type into the terminal. Latch checks this on every request before anything reaches a session."
                )
            }

            if !controller.revokedDevices.isEmpty {
                Section {
                    ForEach(controller.revokedDevices) { device in
                        LabeledContent(device.name) {
                            Text("Revoked").foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    SettingsSectionHeader("Revoked")
                }
            }

            Section {
                Toggle("Allow encrypted relay fallback", isOn: Binding(
                    get: { controller.status.relayEnabled },
                    set: { enabled in Task { await controller.setRelayEnabled(enabled) } }
                ))
                .disabled(!controller.isEnabled)

                Button("Export Diagnostics…") { exportDiagnostics() }
            } header: {
                SettingsSectionHeader("Connectivity")
            } footer: {
                SettingsFootnote(
                    "Turning the relay off keeps same-network and direct connections working. Diagnostics contain counts and switch states only — no names, addresses, keys, or session content — and are never uploaded."
                )
            }

            Section {
                RemoteAuditList(events: controller.securityEvents, empty: "No security events recorded.")
            } header: {
                SettingsSectionHeader("Security Events")
            }

            Section {
                RemoteAuditList(events: controller.connectionEvents, empty: "No connections recorded.")
            } header: {
                SettingsSectionHeader("Connections")
            }

            if let message = controller.errorMessage {
                Section {
                    Label(message, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .formStyle(.grouped)
        .task { await controller.restoreIfEnabled() }
        .sheet(item: Binding(
            get: { controller.pendingPairing },
            set: { if $0 == nil { controller.dismissPairing() } }
        )) { material in
            RemotePairingSheet(material: material) { controller.dismissPairing() }
        }
    }

    private func exportDiagnostics() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "latch-remote-access-diagnostics.json"
        panel.allowedContentTypes = [.json]
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await controller.exportDiagnostics(to: url) }
    }

    private var statusText: String {
        switch controller.phase {
        case .off: return "Off"
        case .starting: return "Starting…"
        case .online(let listener): return "Online on \(listener)"
        case .failed(let message): return "Stopped — \(message)"
        }
    }

    private var statusColor: Color {
        switch controller.phase {
        case .off: return .secondary
        case .starting: return .yellow
        case .online: return .green
        case .failed: return .red
        }
    }
}

private struct RemoteDeviceRow: View {
    let device: RemoteDevice
    @ObservedObject var controller: RemoteAccessController

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(device.name)
                Text(device.permission.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Picker("", selection: Binding(
                get: { device.permission },
                set: { permission in
                    Task { await controller.grant(device, permission: permission) }
                }
            )) {
                ForEach(DevicePermission.allCases) { permission in
                    Text(permission.label).tag(permission)
                }
            }
            .labelsHidden()
            .frame(width: 120)

            Button("Revoke", role: .destructive) {
                Task { await controller.revoke(device) }
            }
        }
    }
}

private struct RemoteAuditList: View {
    let events: [RemoteAuditEvent]
    let empty: String

    var body: some View {
        if events.isEmpty {
            Text(empty).foregroundStyle(.secondary)
        } else {
            ForEach(events.prefix(12)) { event in
                HStack {
                    Text(event.summary)
                    Spacer()
                    if event.result != "ok" {
                        Text(event.result)
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Text(event.date, style: .time)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// The pairing sheet exists for as long as the one-time secret is valid. The
/// secret is never written to disk by the app.
private struct RemotePairingSheet: View {
    let material: PairingMaterial
    let dismiss: () -> Void

    /// The one-time document is rendered from the material already in memory.
    /// A failure here means the material could not be re-encoded at all, which
    /// is worth saying rather than showing an empty box.
    private var document: String {
        (try? material.pairingDocument()) ?? "This pairing code could not be displayed. Create a new one."
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Pair a Device")
                .font(.title2)
                .fontWeight(.semibold)
            Text("Scan this with the device you are pairing. It works once and expires \(material.expiryDate, style: .relative) from now.")
                .fixedSize(horizontal: false, vertical: true)
            if let code = QRCode.image(for: document, side: 220) {
                HStack {
                    Spacer()
                    Image(nsImage: code)
                        .interpolation(.none)
                        .accessibilityLabel("Pairing QR code")
                    Spacer()
                }
            }
            Text("Or enter this manually:")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(document)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
            Text("New devices start with Interact. Change or revoke that at any time in Remote Access settings.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("Done", action: dismiss).keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 460)
    }
}

/// Renders pairing material as a QR code the phone's scanner can read.
///
/// The code is generated in-process from a string already in memory: nothing is
/// written to disk and no service is contacted, so the one-time secret never
/// leaves the app.
enum QRCode {
    static func image(for contents: String, side: CGFloat) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(contents.utf8)
        // Pairing material is short, so the highest correction level costs
        // nothing and survives a poor camera angle.
        filter.correctionLevel = "H"
        guard let output = filter.outputImage else { return nil }
        let scale = side / max(output.extent.width, 1)
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext()
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: side, height: side))
    }
}
