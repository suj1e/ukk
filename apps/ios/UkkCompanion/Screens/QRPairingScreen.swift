import SwiftUI

/// Real pairing: scan or paste a v1 invite, complete the handshake, show the
/// six-digit code and require explicit confirmation before trust is stored.
struct QRPairingScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject private var pairing: PairingCoordinator

    @StateObject private var scanner = QRScannerController()
    @State private var availability = CameraAvailability.current()
    @State private var pasteDraft = ""
    @Environment(\.dismiss) private var dismiss

    init(model: AppModel) {
        self.model = model
        self._pairing = ObservedObject(wrappedValue: model.pairing)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                switch availability {
                case .available:
                    scannerSection
                case .requestingPermission:
                    requestingSection
                case .unavailable(let reason):
                    unavailableSection(reason)
                }

                if model.trust == nil {
                    payloadSection
                }
                stateSection
                securitySection
            }
            .padding(16)
        }
        .navigationTitle("Pair Device")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            scanner.onPayload = { payload in
                model.beginPairing(payload: payload)
                pasteDraft = ""
                scanner.stop()
            }
        }
        .task {
            let resolved = await CameraAvailability.resolve()
            availability = resolved
            if resolved.isAvailable {
                scanner.start()
            }
        }
        .onChange(of: availability) { _, newValue in
            if newValue.isAvailable, model.trust == nil {
                scanner.start()
            } else {
                scanner.stop()
            }
        }
        .onDisappear {
            scanner.stop()
        }
    }

    private var scannerSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let error = scanner.configurationError {
                configurationErrorCard(error)
            } else {
                ZStack {
                    QRScannerView(controller: scanner)
                        .frame(height: 300)
                        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.9), lineWidth: 3)
                        .frame(width: 180, height: 180)
                        .accessibilityHidden(true)
                }
                Text("Point the camera at the Desktop pairing QR code. The first code read starts the handshake and stops the capture session.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityIdentifier("qrScannerContainer")
    }

    private var requestingSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                ProgressView()
                Text("Requesting camera access…")
                    .font(.headline)
            }
            Text("QR scanning needs camera permission. You can paste the invite JSON instead.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(
            Color(.secondarySystemGroupedBackground),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("pairingCameraRequesting")
    }

    private func configurationErrorCard(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Camera could not start", systemImage: "exclamationmark.triangle.fill")
                .font(.headline)
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
            Text("You can paste the invite payload below instead.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(
            Color.orange.opacity(0.10),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("qrConfigurationError")
    }

    private func unavailableSection(_ reason: CameraAvailability.UnavailableReason) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Camera unavailable", systemImage: "camera.fill")
                .font(.headline)
            Text(reason.message)
                .font(.callout)
                .foregroundStyle(.secondary)
            Text("You can paste the invite payload below instead.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(
            Color.orange.opacity(0.10),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("pairingCameraUnavailable")
    }

    private var payloadSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Pairing invite")
                .font(.headline)
            TextEditor(text: $pasteDraft)
                .font(.caption.monospaced())
                .frame(minHeight: 110)
                .scrollContentBackground(.hidden)
                .padding(6)
                .background(
                    Color(.secondarySystemGroupedBackground),
                    in: RoundedRectangle(cornerRadius: 10, style: .continuous)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Color(.separator), lineWidth: 0.5)
                )
                .accessibilityIdentifier("pairingPayloadField")
            Button("Begin pairing") {
                let payload = pasteDraft
                pasteDraft = ""
                model.beginPairing(payload: payload)
            }
            .buttonStyle(.borderedProminent)
            .disabled(pairing.isBusy || pasteDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityIdentifier("beginPairingButton")
        }
    }

    @ViewBuilder
    private var stateSection: some View {
        switch pairing.state {
        case .idle, .cancelled:
            Text("Scan the Desktop QR code or paste its invite JSON to start pairing.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier("pairingIdle")

        case .connecting:
            stateCard {
                HStack(spacing: 8) {
                    ProgressView()
                    Text("Contacting the Host…")
                        .font(.headline)
                }
                Text("The invite is being used once. It is not stored.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .accessibilityIdentifier("pairingConnecting")

        case .awaitingConfirmation(let code, let hostID):
            stateCard {
                Text("Compare this code with the Desktop")
                    .font(.headline)
                Text(code)
                    .font(.system(size: 44, weight: .bold, design: .monospaced))
                    .tracking(8)
                    .accessibilityIdentifier("pairingCode")
                Text("Host \(hostID.prefix(12))…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Text("Only confirm when the Desktop displays the same six digits. Confirming sends the proof that completes pairing.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack(spacing: 10) {
                    Button("The codes match — pair") {
                        pairing.confirm()
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("confirmPairingButton")

                    Button("Cancel", role: .cancel) {
                        model.cancelPairing()
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("cancelPairingButton")
                }
            }
            .accessibilityIdentifier("pairingConfirmation")

        case .finishing:
            stateCard {
                HStack(spacing: 8) {
                    ProgressView()
                    Text("Finishing pairing…")
                        .font(.headline)
                }
                Text("Verifying the encrypted channel with the Host.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .accessibilityIdentifier("pairingFinishing")

        case .paired(let trust):
            stateCard {
                Label("Paired", systemImage: "checkmark.seal.fill")
                    .font(.headline)
                    .foregroundStyle(Color.green)
                Text(trust.label)
                    .font(.callout)
                Text("Host keys are pinned in the Keychain. Sessions load over the encrypted channel.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("pairingDoneButton")
            }
            .accessibilityIdentifier("pairingPaired")

        case .failed(let message):
            stateCard {
                Label("Pairing failed", systemImage: "xmark.octagon.fill")
                    .font(.headline)
                    .foregroundStyle(Color.red)
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("pairingFailureMessage")
                Button("Try again") {
                    model.cancelPairing()
                }
                .buttonStyle(.bordered)
            }
            .accessibilityIdentifier("pairingFailed")
        }
    }

    private func stateCard<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8, content: content)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(
                Color(.secondarySystemGroupedBackground),
                in: RoundedRectangle(cornerRadius: 12, style: .continuous)
            )
    }

    private var securitySection: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "lock.shield")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text("The invite secret stays in memory for this one attempt, is never displayed or logged, and is never written to disk. Only the device key seeds and the pinned Host public keys are stored, in the Keychain with ThisDeviceOnly protection.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("pairingSecurityNote")
    }
}
