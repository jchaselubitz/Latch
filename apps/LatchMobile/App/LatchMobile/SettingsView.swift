import LatchMobileKit
import SwiftUI

/// The settings tab: linking this phone to a computer, and what that link can do.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var address = ""
    @State private var token = ""
    @State private var confirmingUnlink = false

    var body: some View {
        NavigationStack {
            Form {
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
            Text("Your computer")
        } footer: {
            Text("""
            Run `latch serve` on your computer and `latch serve token` for the token. \
            The gateway listens on loopback only, so the address here is a tunnel to it — \
            an SSH forward, a Tailscale address, or a reverse proxy that terminates TLS.
            """)
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
            LabeledContent("Address", value: model.link?.url.absoluteString ?? "")
                .lineLimit(1)
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
            Button("Unlink", role: .destructive) {
                confirmingUnlink = true
            }
            .confirmationDialog(
                "Unlink this computer?",
                isPresented: $confirmingUnlink,
                titleVisibility: .visible
            ) {
                Button("Unlink", role: .destructive) {
                    model.unlink()
                    address = ""
                    token = ""
                }
            } message: {
                Text("The saved address and token are removed from this phone.")
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
