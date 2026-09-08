import SwiftUI
import AppKit
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
                HStack {
                    TextField("https://…", text: $controller.controlPlaneAddress)
                        .textFieldStyle(.roundedBorder)
                    Button("Save") { controller.saveControlPlaneAddress() }
                }
                HStack {
                    SecureField("One-use owner invitation", text: $controller.ownerInvitation)
                        .textFieldStyle(.roundedBorder)
                    Button("Use Invitation") { controller.saveOwnerInvitation() }
                        .disabled(controller.ownerInvitation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                LabeledContent("Pairing codes") {
                    Text(controller.isControlPlaneConfigured ? "Carry this address" : "Carry no address")
                        .foregroundStyle(controller.isControlPlaneConfigured ? Color.secondary : Color.orange)
                }
            } header: {
                SettingsSectionHeader("Control Plane")
            } footer: {
                SettingsFootnote(
                    "A new Mac also needs an operator-minted, one-use owner invitation. It is held only in memory and consumed during first enrollment. A phone enrolls against this address after scanning a pairing code; no session content or gateway credential reaches the control plane."
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
                Button {
                    Task { await controller.createPairing() }
                } label: {
                    HStack(spacing: 6) {
                        if controller.isPairing {
                            ProgressView().controlSize(.small)
                        }
                        Text(controller.isPairing ? "Preparing a Code…" : "Pair a Device…")
                    }
                }
                .disabled(!controller.isEnabled || controller.isPairing)
            } header: {
                SettingsSectionHeader("Paired Devices")
            } footer: {
                SettingsFootnote(
                    "Observe can read sessions and conversations. Control can also send messages and answer prompts. Allow terminal additionally lets a device open a session's terminal and run commands on this Mac, which takes that terminal from whatever is currently showing it. New devices start with terminal access allowed; you can switch it off at any time. Latch checks this on every request before anything reaches a session, and turning the terminal off closes one a device is already holding."
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
                LabeledContent("Internet path") { Text("Encrypted WSS relay") }
                LabeledContent("Local path") { Text("Authenticated Remote Link") }
                ForEach(controller.activeDevices) { device in
                    if let directoryID = device.controlPlaneDeviceID,
                       let link = controller.linkStatuses[directoryID] {
                        LabeledContent(device.name) { Text(Self.label(link)) }
                    }
                }
            } header: {
                SettingsSectionHeader("Connectivity")
            } footer: {
                SettingsFootnote(
                    "Both paths use the same pinned Noise identity and bounded logical streams. The relay forwards opaque frames only; it never receives device keys, grants, gateway credentials, or session content. A waiting relay socket is not a connection; a phone is connected only once it has authenticated."
                )
            }

            Section {
                Toggle("Keep this Mac awake while a phone is connected", isOn: $controller.keepAwakeWhilePluggedIn)
                LabeledContent("Power") {
                    Text(controller.isOnExternalPower ? "Plugged in" : "On battery")
                }
                LabeledContent("Idle sleep") {
                    Text(controller.isPreventingSleep ? "Prevented while connected" : "Not prevented")
                }
            } header: {
                SettingsSectionHeader("Sleep")
            } footer: {
                SettingsFootnote(
                    "Off by default. When on, Latch prevents idle sleep only while a phone is authenticated and this Mac is on external power. Closing the lid or choosing Sleep still sleeps the Mac, and an asleep Mac is reported to the phone as offline until it wakes."
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
            RemotePairingSheet(
                material: material,
                progress: controller.pairingProgress,
                approve: { controller.approveEnrollment() },
                reject: { controller.rejectEnrollment() }
            ) { controller.dismissPairing() }
        }
        // A failed attempt shows no code at all, so it is raised here rather
        // than left to the error row at the end of the form, which is below
        // the fold in a window this size.
        .alert(
            "Latch could not create a pairing code",
            isPresented: Binding(
                get: { controller.pairingFailure != nil },
                set: { presented in if !presented { controller.pairingFailure = nil } }
            ),
            presenting: controller.pairingFailure
        ) { _ in
            Button("OK", role: .cancel) { controller.pairingFailure = nil }
        } message: { reason in
            Text(reason)
        }
    }

    private var statusText: String {
        switch controller.phase {
        case .off: return "Off"
        case .starting: return "Starting…"
        case .onlineRelay(let peers):
            return peers == 0 ? "Waiting for a phone" : "Connected (\(peers) device\(peers == 1 ? "" : "s"))"
        case .failed(let message): return "Stopped — \(message)"
        }
    }

    static func label(_ status: HelperLinkStatus) -> String {
        switch status {
        case .lanReady, .connecting: return "Connecting to relay"
        case .waitingForPeer: return "Waiting for phone"
        case .authenticating: return "Authenticating"
        case .ready: return "Connected"
        case .linkClosed: return "Disconnected"
        case .offline: return "Relay unavailable"
        }
    }

    private var statusColor: Color {
        switch controller.phase {
        case .off: return .secondary
        case .starting: return .yellow
        case .onlineRelay: return .green
        case .failed: return .red
        }
    }
}

/// One paired phone, with the two decisions that are actually separate: how
/// much of the conversation it can take part in, and whether it may hold this
/// Mac's terminal.
///
/// The terminal is deliberately its own switch rather than the top notch of a
/// severity dropdown. It is the one grant that hands a phone the ability to
/// run commands here, and taking a session's single terminal surface is
/// visible to whoever was using it, so it should be turned on by an act that
/// reads as turning it on.
private struct RemoteDeviceRow: View {
    let device: RemoteDevice
    @ObservedObject var controller: RemoteAccessController

    /// The permission the device holds when the terminal is not allowed. It is
    /// tracked here because `control` erases it: a device switched to the
    /// terminal and back should return to Control or Observe as it was, not
    /// to a default.
    @State private var baseAccess: DevicePermission

    init(device: RemoteDevice, controller: RemoteAccessController) {
        self.device = device
        self.controller = controller
        _baseAccess = State(initialValue: device.permissionWithoutTerminal)
    }

    /// Everything below `control`, in ladder order. `control` is not offered
    /// here because it is the terminal toggle.
    private static let baseChoices: [DevicePermission] = [.observe, .interact]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(device.name)
                    Text(device.permission.detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Picker("", selection: Binding(
                    get: { baseAccess },
                    set: { permission in
                        baseAccess = permission
                        // Allowing the terminal already implies everything
                        // below it, so this only takes effect once the
                        // terminal is off again.
                        guard !device.allowsTerminal else { return }
                        Task { await controller.grant(device, permission: permission) }
                    }
                )) {
                    ForEach(Self.baseChoices) { permission in
                        Text(permission.label).tag(permission)
                    }
                }
                .labelsHidden()
                .frame(width: 120)

                Button("Revoke", role: .destructive) {
                    Task { await controller.revoke(device) }
                }
            }
            Toggle("Allow terminal", isOn: Binding(
                get: { device.allowsTerminal },
                set: { allowed in
                    Task {
                        await controller.grant(
                            device,
                            permission: allowed ? .control : baseAccess
                        )
                    }
                }
            ))
            .toggleStyle(.switch)
            .controlSize(.small)
            .font(.callout)
        }
        // A device revoked and re-granted, or changed from another window,
        // must not leave a stale base behind the toggle.
        .onChange(of: device.permission) { permission in
            if permission != .control {
                baseAccess = permission
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
    let material: RemoteEnrollmentMaterial
    let progress: RemotePairingProgress
    let approve: () -> Void
    let reject: () -> Void
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
            Text("Scan this with the device you are pairing. Scanning alone grants nothing; approval happens after both screens show the same comparison code. It works once and expires \(material.expiryDate, style: .relative) from now.")
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
            progressLabel
            Text("New devices start with Control and terminal access allowed. Change either setting, or revoke the device, at any time in Remote Access settings.")
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

    /// What the sheet is waiting for. The comparison is surfaced before the
    /// exact key and grant are approved and committed.
    @ViewBuilder
    private var progressLabel: some View {
        switch progress {
        case .idle:
            EmptyView()
        case .waiting:
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Waiting for the phone to scan this…")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        case .comparing(let name, let permission, let code):
            VStack(alignment: .leading, spacing: 10) {
                Text("Confirm \(name) requests \(permission.label), and check that the phone shows exactly:")
                Text(code)
                    .font(.system(.title3, design: .monospaced).weight(.semibold))
                    .textSelection(.enabled)
                HStack {
                    Button("Reject", role: .destructive, action: reject)
                    Spacer()
                    Button("Approve This Device", action: approve)
                        .keyboardShortcut(.defaultAction)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        case .enrolled(let name):
            VStack(alignment: .leading, spacing: 4) {
                Label("\(name) is paired.", systemImage: "checkmark.circle")
                    .foregroundStyle(.green)
                Text("The phone received its encrypted receipt after the local grant and service mirror completed.")
            }
            .font(.caption)
            .fixedSize(horizontal: false, vertical: true)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle")
                .font(.caption)
                .foregroundStyle(.orange)
                .fixedSize(horizontal: false, vertical: true)
        }
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
