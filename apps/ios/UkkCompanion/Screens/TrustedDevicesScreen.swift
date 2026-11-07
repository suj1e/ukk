import SwiftUI

/// Trusted-device management shell. Mutation is local fixture state only; the
/// real revocation round-trip belongs to the Host protocol (S1/S2).
struct TrustedDevicesScreen: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            FixtureBanner()
            List {
                Section {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: "exclamationmark.shield")
                            .foregroundStyle(Color.orange)
                            .accessibilityHidden(true)
                        Text("Pairing and revocation are a local UI shell only. This build sends no Host request and holds no trust material.")
                            .font(.caption)
                    }
                    .accessibilityIdentifier("pairingNotImplementedNote")
                }

                Section {
                    ForEach(model.deviceStore.sortedForDisplay) { device in
                        deviceRow(device)
                    }
                } header: {
                    Text("Trusted devices")
                } footer: {
                    Text("Each capability is a separate grant. Pairing a device grants nothing by itself.")
                }

                Section {
                    NavigationLink {
                        QRPairingScreen(model: model)
                    } label: {
                        Label("Pair a device", systemImage: "qrcode.viewfinder")
                    }
                    .accessibilityIdentifier("pairDeviceRow")
                } footer: {
                    Text("Scanning presents the QR UI shell. No invitation is redeemed and no key is stored in this build.")
                }

                if let notice = model.deviceActionNotice {
                    Section {
                        Text(notice)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("deviceActionNotice")
                    }
                }
            }
            .listStyle(.insetGrouped)
        }
        .navigationTitle("Devices")
        .navigationBarTitleDisplayMode(.inline)
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

            if let lastSeen = device.lastSeenAt {
                Text("Last seen \(lastSeen, format: .dateTime.hour().minute()) · fixture")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }

            if device.presence != .revoked && !device.isCurrentDevice {
                Button("Revoke") {
                    model.revokeDevice(device.id)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(.red)
                .accessibilityIdentifier("revokeButton-\(device.id)")
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("trustedDeviceRow-\(device.id)")
    }
}
