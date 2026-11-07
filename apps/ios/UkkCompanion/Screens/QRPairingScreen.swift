import SwiftUI

/// QR pairing presentation. The scanner is real AVFoundation code, but this
/// build never redeems an invitation, never performs a handshake and never
/// stores trust material. The camera is unavailable in the simulator, which is
/// surfaced explicitly instead of failing silently.
struct QRPairingScreen: View {
    @ObservedObject var model: AppModel

    @State private var scanner = QRScannerController()

    private let availability = CameraAvailability.current()

    var body: some View {
        VStack(spacing: 0) {
            FixtureBanner()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    switch availability {
                    case .available:
                        scannerSection
                    case .unavailable(let reason):
                        unavailableSection(reason)
                    }

                    payloadSection

                    if let message = model.pairingParseMessage {
                        resultSection(message)
                    }

                    securitySection
                }
                .padding(16)
            }
        }
        .navigationTitle("Pair Device")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                FixtureTag()
            }
        }
        .onAppear {
            scanner.onPayload = { payload in
                model.handleScannedCode(payload)
            }
            if availability.isAvailable {
                scanner.start()
            }
        }
        .onDisappear {
            scanner.stop()
        }
    }

    private var scannerSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack {
                QRScannerView(controller: scanner)
                    .frame(height: 300)
                    .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.9), lineWidth: 3)
                    .frame(width: 180, height: 180)
                    .accessibilityHidden(true)
            }
            Text("Point the camera at the Desktop pairing QR code. The first code read stops the capture session.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("qrScannerContainer")
    }

    private func unavailableSection(_ reason: CameraAvailability.UnavailableReason) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Camera unavailable", systemImage: "camera.fill")
                .font(.headline)
            Text(reason.message)
                .font(.callout)
                .foregroundStyle(.secondary)
            Text("You can still validate the shape of an invite payload below. Validation is local and performs no network request.")
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
            Text("Validate an invite payload")
                .font(.headline)
            TextEditor(text: $model.pairingPayloadDraft)
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
            Button("Validate payload (no network)") {
                model.validatePairingDraft()
            }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("validatePayloadButton")
        }
    }

    private func resultSection(_ message: String) -> some View {
        Text(message)
            .font(.callout)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(
                Color(.secondarySystemGroupedBackground),
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .accessibilityIdentifier("pairingResult")
    }

    private var securitySection: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "lock.shield")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text("Pairing is not implemented. This screen presents the scanner and validates invitation shape only; scanned values never leave this device, and the invite secret is never displayed or logged.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("pairingSecurityNote")
    }
}
