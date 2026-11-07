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

    func validatePairingDraft(now: Date = Date()) {
        let trimmed = pairingPayloadDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            pairingParseMessage = "Paste a pairing invite payload first."
            return
        }
        switch PairingInviteParser.parse(payload: trimmed, now: now) {
        case .success(let invite):
            pairingParseMessage = "Invite parsed for host \(invite.hostID.uuidString). Host confirmation is still required; this build does not connect."
        case .failure(let error):
            pairingParseMessage = error.message
        }
    }

    func handleScannedCode(_ payload: String) {
        pairingPayloadDraft = payload
        validatePairingDraft()
    }
}
