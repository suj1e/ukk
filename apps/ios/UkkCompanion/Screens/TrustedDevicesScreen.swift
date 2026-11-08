import SwiftUI

/// Trusted devices loaded from the Host (`remote.status`) and revoked through
/// `remote.revoke`. Revoking this device ends the connection and requires a new
/// pairing.
struct TrustedDevicesScreen: View {
    @ObservedObject var model: AppModel

    @State private var pendingRevoke: TrustedDevice?
    @State private var confirmForget = false

    var body: some View {
        VStack(spacing: 0) {
            StatusBanner(model: model)
            List {
                if let trust = model.trust {
                    Section("Paired Host") {
                        LabeledContent("Host") {
                            Text(trust.label)
                        }
                        LabeledContent("Host id") {
                            Text(trust.hostID)
                                .font(.caption.monospaced())
                                .textSelection(.enabled)
                        }
                        LabeledContent("Relay") {
                            Text(trust.relayURL.absoluteString)
                                .font(.caption2.monospaced())
                                .lineLimit(1)
                        }
                        Button("Forget Host", role: .destructive) {
                            confirmForget = true
                        }
                        .accessibilityIdentifier("forgetHostButton")
                    }
                }

                Section {
                    if model.trust == nil {
                        Text("No Host is paired. Pairing displays a six-digit code that must match the Desktop before any trust is stored.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("pairingNotPairedNote")
                    } else if model.devices.isEmpty {
                        Text(model.connection.isLiveClaim
                             ? "No devices were reported by the Host."
                             : "Device grants load only while the Host connection is live.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.devices) { device in
                            deviceRow(device)
                        }
                    }
                } header: {
                    Text("Trusted devices")
                } footer: {
                    Text("Each capability is a separate grant. Pairing a device grants nothing by itself.")
                }

                Section {
                    if model.trust == nil {
                        NavigationLink {
                            QRPairingScreen(model: model)
                        } label: {
                            Label("Pair a device", systemImage: "qrcode.viewfinder")
                        }
                        .accessibilityIdentifier("pairDeviceRow")
                    } else {
                        Label("Forget the current Host before pairing another", systemImage: "qrcode.viewfinder")
                            .foregroundStyle(.secondary)
                    }
                } footer: {
                    Text("Pairing stores only this device's key seeds and the pinned Host public keys in the Keychain. The invite secret is never stored.")
                }

                if let error = model.lastError {
                    Section {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(Color.red)
                            .accessibilityIdentifier("deviceErrorLabel")
                    }
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await model.loadDevices() }
        }
        .navigationTitle("Devices")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.loadDevices() }
        .confirmationDialog(
            pendingRevoke.map { "Revoke \($0.label)?" } ?? "Revoke device?",
            isPresented: Binding(
                get: { pendingRevoke != nil },
                set: { if !$0 { pendingRevoke = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let device = pendingRevoke {
                Button("Revoke", role: .destructive) {
                    Task { await model.revokeDevice(device) }
                    pendingRevoke = nil
                }
                Button("Cancel", role: .cancel) { pendingRevoke = nil }
            }
        } message: {
            if let device = pendingRevoke {
                Text(device.isCurrentDevice
                     ? "This is the current device. Revoking ends this session immediately and requires pairing again."
                     : "The Host will drop this device's grants.")
            }
        }
        .confirmationDialog(
            "Forget this Host?",
            isPresented: $confirmForget,
            titleVisibility: .visible
        ) {
            Button("Forget Host", role: .destructive) {
                model.forgetHost()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The pinned keys and the last-selected Session are removed from this device. The Desktop keeps its own trust list.")
        }
    }

    private func deviceRow(_ device: TrustedDevice) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text(device.label)
                    .font(.headline)
                if device.isCurrentDevice {
                    Text("This device")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                StatusChip(text: device.presence.label, tone: device.presence.tone)
            }

            Text("\(device.platform) · \(device.role.label)")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(device.scopesSummary)
                .font(.caption2)
                .foregroundStyle(.secondary)

            if let until = device.grantedUntil {
                Text("Grants expire \(until, format: .dateTime.hour().minute())")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }

            if device.presence != .revoked {
                Button(device.isCurrentDevice ? "Revoke this device" : "Revoke") {
                    pendingRevoke = device
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(.red)
                .disabled(!model.connection.isLiveClaim)
                .accessibilityIdentifier("revokeButton-\(device.id)")
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("trustedDeviceRow-\(device.id)")
    }
}
