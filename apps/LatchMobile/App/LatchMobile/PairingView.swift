import LatchMobileKit
import SwiftUI

#if canImport(UIKit)
import UIKit
#endif

/// Pairing this phone with a Mac, and what the pairing looks like afterwards.
///
/// Every state on this screen is a security decision the person has to be able
/// to read: which Mac, which transcript comparison, what this phone may do, and how to take
/// it back. `PairingModel` already resolves each failure to a sentence, so the
/// view shows what it is given rather than inventing its own wording.
struct PairingView: View {
    @Environment(PairingModel.self) private var model
    @State private var confirmingRevoke = false

    var body: some View {
        @Bindable var model = model
        Form {
            switch model.state {
            case .idle:
                unpairedSections(name: $model.deviceName)
            case .scanning:
                scanningSections(name: $model.deviceName)
            case .confirming(let proposal):
                confirmingSections(proposal)
            case .comparing(let proposal, let code):
                comparisonSections(proposal, code: code)
            case .enrolling:
                busySection("Pairing with your Mac…")
            case .paired(let record):
                pairedSections(record, revoked: false)
            case .revoked(let record):
                pairedSections(record, revoked: true)
            case .failed(let reason):
                failureSection(reason)
            }
        }
        .navigationTitle("Remote access")
        .task {
            await model.restore()
            // The Mac may have revoked this phone or narrowed what it may do
            // while the app was closed, so the stored answer is re-read rather
            // than trusted.
            await model.refreshPermission()
        }
    }

    // MARK: - Not paired

    @ViewBuilder
    private func unpairedSections(name: Binding<String>) -> some View {
        Section {
            TextField("This phone's name", text: name)
                .textInputAutocapitalization(.words)
                .autocorrectionDisabled()
        } header: {
            Text("How this phone appears on your Mac")
        }

        Section {
            Button("Scan the pairing code") {
                Task { await model.beginScanning() }
            }
        } header: {
            Text("Pair with your Mac")
        } footer: {
            Text("""
            On your Mac, open Remote Access and choose Pair a phone. The code it shows \
            lasts five minutes and can be used once.
            """)
        }

        if let identity = model.identity {
            identitySection(identity)
        }
    }

    @ViewBuilder
    private func identitySection(_ identity: DeviceIdentity) -> some View {
        Section {
            LabeledContent("Identity", value: identity.shortFingerprint)
                .monospaced()
            LabeledContent("Protected by", value: identity.protection.label)
        } header: {
            Text("This phone")
        } footer: {
            Text("""
            Your Mac pins this key when it pairs. It is created on this phone and never \
            leaves it.
            """)
        }
    }

    // MARK: - Scanning

    @ViewBuilder
    private func scanningSections(name: Binding<String>) -> some View {
        // A camera the app cannot use is a dead end, not a slow start: the
        // scanner is skipped entirely and the code is typed instead.
        if let explanation = model.cameraPermission.explanation {
            Section {
                Label(explanation, systemImage: "camera.fill")
                    .font(.footnote)
                if let settings = URL(string: UIApplication.openSettingsURLString) {
                    Link("Open Settings", destination: settings)
                }
            } header: {
                Text("Camera")
            }
        } else {
            Section {
                QRScannerView { code in
                    model.scanned(code)
                }
                .frame(height: 260)
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .listRowInsets(EdgeInsets())
            } footer: {
                Text("Point the camera at the pairing code on your Mac.")
            }
        }

        ManualCodeEntry { code in
            model.scanned(code)
        }

        Section {
            Button("Cancel", role: .cancel) { model.cancel() }
        }
    }

    // MARK: - Confirming the scanned Mac

    @ViewBuilder
    private func confirmingSections(_ proposal: PairingProposal) -> some View {
        Section {
            LabeledContent("Mac", value: proposal.macDisplayName)
            LabeledContent("Identity", value: proposal.macFingerprint)
                .monospaced()
            LabeledContent("This phone", value: model.deviceName)
        }

        Section {
            Button("Connect securely") {
                Task { await model.confirm() }
            }
            .disabled(model.isBusy)
            Button("Cancel", role: .cancel) { model.cancel() }
        } footer: {
            Text("This starts an encrypted Remote Link. Nothing is granted until you compare the code that appears next.")
        }
    }

    @ViewBuilder
    private func comparisonSections(_ proposal: PairingProposal, code: String) -> some View {
        Section {
            Text(code)
                .font(.title2.monospaced())
                .fontWeight(.semibold)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 8)
                .accessibilityLabel("Enrollment comparison code: \(code)")
        } header: {
            Text("Check this code matches your Mac")
        } footer: {
            Text("The code binds this phone's exact key, requested permission, and encrypted handshake. Reject on both devices if it differs.")
        }
        Section {
            LabeledContent("Mac", value: proposal.macDisplayName)
            LabeledContent("Access requested", value: DevicePermission.control.label)
            Button("The codes match — wait for Mac approval") {
                Task { await model.confirmComparison() }
            }
            Button("Codes do not match", role: .destructive) { model.cancel() }
        }
    }

    // MARK: - Paired

    @ViewBuilder
    private func pairedSections(_ record: PairedDeviceRecord, revoked: Bool) -> some View {
        if revoked {
            Section {
                Label(
                    """
                    \(record.mac.displayName) revoked this phone. It can no longer connect. \
                    Pair again from your Mac to restore access.
                    """,
                    systemImage: "hand.raised.fill"
                )
                .font(.footnote)
                .foregroundStyle(.orange)
            }
        }

        Section {
            LabeledContent("Name", value: record.mac.displayName)
            LabeledContent("Identity", value: record.mac.shortFingerprint)
                .monospaced()
            LabeledContent("Confirmed with", value: record.comparison)
                .font(.footnote.monospaced())
            LabeledContent("Paired", value: record.pairedAt.formatted(date: .abbreviated, time: .shortened))
        } header: {
            Text("Paired Mac")
        }

        Section {
            LabeledContent("This phone", value: record.name)
            LabeledContent("Access", value: revoked ? "Revoked" : record.permission.label)
            Button("Check for changes") {
                Task { await model.refreshPermission() }
            }
            .disabled(model.isBusy)
        } header: {
            Text("What this phone may do")
        } footer: {
            // The grant is stated even when it is narrow, because it is the
            // reason a control is missing elsewhere in the app.
            Text(revoked ? "Access is revoked." : record.permission.explanation)
        }

        Section {
            if revoked {
                Button("Remove from this phone", role: .destructive) {
                    model.forget()
                }
            } else {
                Button("Unpair this phone", role: .destructive) {
                    confirmingRevoke = true
                }
                .disabled(model.isBusy)
                .confirmationDialog(
                    "Unpair this phone?",
                    isPresented: $confirmingRevoke,
                    titleVisibility: .visible
                ) {
                    Button("Unpair", role: .destructive) {
                        Task { await model.revoke() }
                    }
                } message: {
                    Text("""
                    This phone stops being able to connect, and its identity key is \
                    destroyed. Pairing again enrolls it as a new device.
                    """)
                }
            }
        } footer: {
            Text("Your Mac can also revoke this phone with `latch remote-access revoke`.")
        }
    }

    // MARK: - Waiting and failure

    @ViewBuilder
    private func busySection(_ title: String) -> some View {
        Section {
            HStack {
                ProgressView()
                Text(title)
            }
        }
    }

    @ViewBuilder
    private func failureSection(_ reason: String) -> some View {
        Section {
            Label(reason, systemImage: "exclamationmark.triangle")
                .font(.footnote)
                .foregroundStyle(.red)
        }
        Section {
            Button("Try again") {
                Task { await model.beginScanning() }
            }
            Button("Cancel", role: .cancel) { model.cancel() }
        }
    }
}

/// Typing the code instead of scanning it.
///
/// The scanner is the normal path, but a phone whose camera is denied or
/// restricted is not a phone that cannot pair — the code is a short document,
/// and pasting it is a supported way in.
private struct ManualCodeEntry: View {
    let onSubmit: (String) -> Void
    @State private var text = ""

    var body: some View {
        Section {
            TextField("Paste the pairing code", text: $text, axis: .vertical)
                .lineLimit(1...4)
                .font(.footnote.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Use this code") {
                onSubmit(text)
                text = ""
            }
            .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        } header: {
            Text("Or enter it by hand")
        }
    }
}
