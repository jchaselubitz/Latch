import LatchMobileKit
import SwiftUI

/// The settings tab: linking this phone to a computer, and what that link can do.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing
    @State private var address = ""
    @State private var token = ""
    @State private var confirmingUnlink = false

    var body: some View {
        NavigationStack {
            Form {
                remoteAccessSection

                switch model.linkState {
                case .linked:
                    linkedSections
                default:
                    linkForm
                }
            }
            .navigationTitle("Settings")
        }
    }

    // MARK: - Remote access

    /// The pairing entry point. It sits alongside the gateway link rather than
    /// replacing it: a tunnel to `latch serve` and a paired identity are two
    /// different ways to reach the same computer, and this build supports both.
    @ViewBuilder
    private var remoteAccessSection: some View {
        Section {
            NavigationLink {
                PairingView()
            } label: {
                LabeledContent("Remote access", value: pairingSummary)
            }
        } footer: {
            Text("""
            Pairing links this phone to your Mac's identity directly, using a code \
            your Mac shows for five minutes.
            """)
        }
    }

    private var pairingSummary: String {
        switch pairing.state {
        case .paired(let record): return record.mac.displayName
        case .revoked: return "Revoked"
        case .confirming, .enrolling, .scanning: return "Pairing…"
        case .idle, .failed: return "Not paired"
        }
    }

    /// Pairing is the usual remote-access path. A typed `latch serve` tunnel
    /// remains available, but it is easy to paste the Mac's control-plane URL
    /// into this field by mistake — that service enrolls phones, it does not
    /// list sessions.
    private var manualLinkHeader: String {
        if case .paired = pairing.state {
            return "Optional latch serve tunnel"
        }
        return "Your computer"
    }

    private var manualLinkFooter: String {
        if case .paired(let record) = pairing.state {
            return """
            You're already paired with \(record.mac.displayName). That connection does \
            not use this address. Only fill this in for a separate `latch serve` tunnel — \
            never the control-plane URL from Mac Remote Access settings.
            """
        }
        return """
        Run `latch serve` on your computer and `latch serve token` for the token. \
        The gateway listens on loopback only, so the address here is a tunnel to it — \
        an SSH forward, a Tailscale address, or a reverse proxy that terminates TLS. \
        This is not the control-plane URL shown in Mac Remote Access settings.
        """
    }

    // MARK: - Not linked

    @ViewBuilder
    private var linkForm: some View {
        Section {
            TextField("https://…", text: $address)
                .textContentType(.URL)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            SecureField("Gateway token", text: $token)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        } header: {
            Text(manualLinkHeader)
        } footer: {
            Text(manualLinkFooter)
        }

        Section {
            Button {
                Task { await model.link(address: address, token: token) }
            } label: {
                if case .connecting = model.linkState {
                    HStack {
                        ProgressView()
                        Text("Connecting…")
                    }
                } else {
                    Text("Link this computer")
                }
            }
            .disabled(address.isEmpty || token.isEmpty || model.linkState == .connecting)
        }

        if case .failed(let reason) = model.linkState {
            Section {
                Label(reason, systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
        }
    }

    // MARK: - Linked

    @ViewBuilder
    private var linkedSections: some View {
        Section("Linked computer") {
            if model.linkSource == .paired {
                LabeledContent("Connection", value: "Secure paired connection")
            } else {
                LabeledContent("Address", value: model.link?.url.absoluteString ?? "")
                    .lineLimit(1)
            }
            if let version = model.productVersion {
                LabeledContent("Latch", value: version)
            }
            LabeledContent("Protocol", value: "v\(LatchContract.protocolVersion)")
        }

        // What discovery said this gateway can do. It is shown rather than
        // hidden because it explains why a screen is missing a control: the
        // app never probes an endpoint to find out, so this is the answer.
        Section {
            ForEach(GatewayEndpointsName.allCases, id: \.self) { endpoint in
                CapabilityRow(
                    name: Self.label(endpoint),
                    available: available(endpoint)
                )
            }
            ForEach(GatewayFeaturesName.allCases, id: \.self) { feature in
                CapabilityRow(
                    name: Self.label(feature),
                    available: available(feature)
                )
            }
        } header: {
            Text("What this gateway offers")
        } footer: {
            Text("""
            Reported by the gateway's discovery document. Anything switched off \
            here is missing from the app on purpose.
            """)
        }

        Section {
            Button("Check again") {
                Task { await model.rediscover() }
            }
            Button(model.linkSource == .paired ? "Disconnect" : "Unlink", role: .destructive) {
                confirmingUnlink = true
            }
            .confirmationDialog(
                model.linkSource == .paired ? "Disconnect from this Mac?" : "Unlink this computer?",
                isPresented: $confirmingUnlink,
                titleVisibility: .visible
            ) {
                Button(model.linkSource == .paired ? "Disconnect" : "Unlink", role: .destructive) {
                    model.unlink()
                    address = ""
                    token = ""
                }
            } message: {
                Text(
                    model.linkSource == .paired
                        ? "This closes the current secure connection. Your pairing stays on this phone."
                        : "The saved address and token are removed from this phone."
                )
            }
        }
    }

    private func available(_ endpoint: GatewayEndpointsName) -> Bool {
        guard case .linked(let capabilities) = model.linkState else { return false }
        return GatewayCompatibility.supports(endpoint: endpoint, capabilities: capabilities)
    }

    private func available(_ feature: GatewayFeaturesName) -> Bool {
        guard case .linked(let capabilities) = model.linkState else { return false }
        return GatewayCompatibility.supports(feature: feature, capabilities: capabilities)
    }

    static func label(_ endpoint: GatewayEndpointsName) -> String {
        switch endpoint {
        case .sessions: return "Session list"
        case .sessionCapabilities: return "Per-session capabilities"
        case .terminal: return "Terminal"
        case .events: return "Chat transcript"
        case .send: return "Sending messages"
        }
    }

    static func label(_ feature: GatewayFeaturesName) -> String {
        switch feature {
        case .idempotencyKeys: return "Safe retries"
        case .readOnlyTerminal: return "Read-only terminal"
        }
    }
}

private struct CapabilityRow: View {
    let name: String
    let available: Bool

    var body: some View {
        HStack {
            Text(name)
            Spacer()
            Image(systemName: available ? "checkmark.circle.fill" : "minus.circle")
                .foregroundStyle(available ? Color.green : Color.secondary)
                .accessibilityLabel(available ? "available" : "unavailable")
        }
    }
}
