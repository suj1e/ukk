import Foundation
import SwiftUI

/// App-level state for the fixture-first shell. It owns presentation state only:
/// no Host transport, no key material, no Agent process.
@MainActor
final class AppModel: ObservableObject {
    let catalog: FixtureCatalog

    @Published var navigation = NavigationState()
    @Published private(set) var connection: ConnectionStateMachine
    @Published private(set) var deviceStore: TrustedDeviceStore
    @Published private(set) var deviceActionNotice: String?
    @Published var pairingPayloadDraft = ""
    @Published private(set) var pairingParseMessage: String?
    @Published var isShowingTrustedDevices = false

    init(
        catalog: FixtureCatalog = .fixture,
        connection: ConnectionStateMachine = ConnectionStateMachine(),
        deviceStore: TrustedDeviceStore? = nil
    ) {
        self.catalog = catalog
        self.connection = connection
        self.deviceStore = deviceStore ?? TrustedDeviceStore(devices: catalog.devices)
    }

    func handleScenePhase(_ phase: ScenePhase) {
        switch phase {
        case .background:
            connection.handle(.appEnteredBackground)
        case .active:
            connection.handle(.appEnteredForeground)
        case .inactive:
            break
        @unknown default:
            break
        }
    }

    func retryConnection() {
        connection.handle(.userRequestedRetry)
    }

    func revokeDevice(_ id: DeviceID) {
        deviceActionNotice = deviceStore.revoke(id).message
    }

    /// Validates the payload the user pasted into the visible field.
    func validatePairingDraft(now: Date = Date()) {
        validatePairing(payload: pairingPayloadDraft, now: now)
    }

    /// Validates an invite payload without copying it into the visible draft.
    /// Scanned QR payloads contain `invite_secret`, so they must never be bound
    /// to the always-visible editor.
    @discardableResult
    func validatePairing(payload: String, now: Date = Date()) -> String {
        let trimmed = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        let message: String
        if trimmed.isEmpty {
            message = "Paste a pairing invite payload first."
        } else {
            switch PairingInviteParser.parse(payload: trimmed, now: now) {
            case .success(let invite):
                message = "Invite parsed for host \(invite.hostID.uuidString). Host confirmation is still required; this build does not connect."
            case .failure(let error):
                message = error.message
            }
        }
        pairingParseMessage = message
        return message
    }

    /// Scanned payloads are validated in memory only; they never reach
    /// `pairingPayloadDraft`, the UI, logs or disk.
    func handleScannedCode(_ payload: String) {
        validatePairing(payload: payload)
    }
}
