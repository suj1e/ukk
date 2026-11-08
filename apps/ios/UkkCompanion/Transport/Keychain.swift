import Foundation
import Security

/// Minimal secret storage abstraction so trust and identity handling are
/// testable without the Keychain, and so no code path ever writes secrets to
/// UserDefaults or the filesystem.
protocol SecretStore: AnyObject {
    func read(account: String) throws -> Data?
    func write(_ data: Data, account: String) throws
    func delete(account: String) throws
}

enum SecretStoreError: Error, Equatable {
    case keychain(OSStatus)
    case malformedRecord

    var message: String {
        switch self {
        case .keychain(let status):
            return "Keychain operation failed (\(status))."
        case .malformedRecord:
            return "Stored trust material is malformed; pair again."
        }
    }
}

/// Keychain-backed store. Items are available after first unlock and are never
/// migrated to another device (`ThisDeviceOnly`, no iCloud synchronisation).
final class KeychainSecretStore: SecretStore {
    private let service: String

    init(service: String = "dev.ukk.companion.remote") {
        self.service = service
    }

    private func baseQuery(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false
        ]
    }

    func read(account: String) throws -> Data? {
        var query = baseQuery(account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else { throw SecretStoreError.malformedRecord }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw SecretStoreError.keychain(status)
        }
    }

    func write(_ data: Data, account: String) throws {
        let query = baseQuery(account: account)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]

        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else {
            throw SecretStoreError.keychain(updateStatus)
        }

        var insert = query
        insert.merge(attributes) { _, new in new }
        let addStatus = SecItemAdd(insert as CFDictionary, nil)
        guard addStatus == errSecSuccess else {
            throw SecretStoreError.keychain(addStatus)
        }
    }

    func delete(account: String) throws {
        let status = SecItemDelete(baseQuery(account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw SecretStoreError.keychain(status)
        }
    }
}

/// Deterministic in-memory store used by previews and unit tests. It is never
/// selected by the app: `IdentityVault.live` always uses the Keychain.
final class InMemorySecretStore: SecretStore, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String: Data] = [:]

    init() {}

    func read(account: String) throws -> Data? {
        lock.lock()
        defer { lock.unlock() }
        return items[account]
    }

    func write(_ data: Data, account: String) throws {
        lock.lock()
        defer { lock.unlock() }
        items[account] = data
    }

    func delete(account: String) throws {
        lock.lock()
        defer { lock.unlock() }
        items.removeValue(forKey: account)
    }

    var accounts: [String] {
        lock.lock()
        defer { lock.unlock() }
        return items.keys.sorted()
    }
}

// MARK: - Records

struct IdentityRecord: Codable, Equatable, Sendable {
    var version: Int = 1
    let edSeed: Data
    let xPrivate: Data
    let edPublic: Data
    let xPublic: Data
    let deviceID: String
    let createdAtMS: Int64

    var identity: DeviceIdentity {
        get throws {
            try DeviceIdentity(edSeed: edSeed, xPrivate: xPrivate)
        }
    }

    /// The device id is stable and non-secret; it is what the relay sees.
    static func makeDeviceID(edPublic: Data) -> String {
        let digest = HandshakeCrypto.sha256(edPublic)
        return Base64URL.encode(Data(digest.prefix(16)))
    }
}

struct HostTrustRecord: Codable, Equatable, Sendable {
    var version: Int = 1
    let hostID: String
    let hostEd: Data
    let hostX: Data
    let relayURL: URL
    let deviceID: String
    let label: String
    let pairedAtMS: Int64

    var pinnedIdentity: HostPinnedIdentity {
        get throws {
            try HostPinnedIdentity(hostID: hostID, hostEd: hostEd, hostX: hostX, relayURL: relayURL)
        }
    }
}

// MARK: - Vault

/// Owns the device identity and the pinned Host. The invite secret is never
/// accepted by this type, so it cannot be persisted by accident.
final class IdentityVault {
    private let store: SecretStore
    private static let identityAccount = "identity.v1"
    private static let trustAccount = "host-trust.v1"

    init(store: SecretStore) {
        self.store = store
    }

    static func live() -> IdentityVault {
        IdentityVault(store: KeychainSecretStore())
    }

    func loadOrCreateIdentity() throws -> IdentityRecord {
        if let existing = try loadIdentity() { return existing }
        let identity = try DeviceIdentity.generate()
        let record = IdentityRecord(
            edSeed: identity.edSeed,
            xPrivate: identity.xPrivate,
            edPublic: identity.edPublic,
            xPublic: identity.xPublic,
            deviceID: IdentityRecord.makeDeviceID(edPublic: identity.edPublic),
            createdAtMS: Int64(Date().timeIntervalSince1970 * 1000)
        )
        let encoded = try JSONEncoder().encode(record)
        try store.write(encoded, account: Self.identityAccount)
        return record
    }

    func loadIdentity() throws -> IdentityRecord? {
        guard let data = try store.read(account: Self.identityAccount) else { return nil }
        let record: IdentityRecord
        do {
            record = try JSONDecoder().decode(IdentityRecord.self, from: data)
        } catch {
            throw SecretStoreError.malformedRecord
        }
        guard record.version == 1,
              record.edSeed.count == 32,
              record.xPrivate.count == 32,
              record.edPublic.count == 32,
              record.xPublic.count == 32 else {
            throw SecretStoreError.malformedRecord
        }
        _ = try record.identity
        return record
    }

    func loadTrust() throws -> HostTrustRecord? {
        guard let data = try store.read(account: Self.trustAccount) else { return nil }
        let record: HostTrustRecord
        do {
            record = try JSONDecoder().decode(HostTrustRecord.self, from: data)
        } catch {
            throw SecretStoreError.malformedRecord
        }
        guard record.version == 1,
              record.hostEd.count == 32,
              record.hostX.count == 32,
              record.relayURL.scheme?.lowercased() == "wss" else {
            throw SecretStoreError.malformedRecord
        }
        return record
    }

    func saveTrust(_ record: HostTrustRecord) throws {
        let encoded = try JSONEncoder().encode(record)
        try store.write(encoded, account: Self.trustAccount)
    }

    /// Only call after the user explicitly confirms dropping the pinned Host.
    func clearTrust() throws {
        try store.delete(account: Self.trustAccount)
    }
}
