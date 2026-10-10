import Foundation
import Security

struct ConnectionKeychain {
    private let service: String
    private let account = "paired-computer"

    init(service: String = "dev.openade.remote.connection") {
        self.service = service
    }

    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    func load() throws -> ConnectionConfiguration? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw ConnectionError.keychain(status)
        }
        let stored = try JSONDecoder().decode(ConnectionConfiguration.self, from: data)
        // Validate persisted values too; never silently allow an insecure endpoint.
        return try ConnectionConfiguration(address: stored.displayAddress, token: stored.token)
    }

    func save(_ configuration: ConnectionConfiguration) throws {
        let data = try JSONEncoder().encode(configuration)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updated == errSecItemNotFound {
            var request = query
            attributes.forEach { request[$0.key] = $0.value }
            let status = SecItemAdd(request as CFDictionary, nil)
            guard status == errSecSuccess else { throw ConnectionError.keychain(status) }
        } else if updated != errSecSuccess {
            throw ConnectionError.keychain(updated)
        }
    }

    func delete() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw ConnectionError.keychain(status)
        }
    }

    private struct PendingEnvelope: Codable {
        let connection: ConnectionConfiguration
        let actions: [String: PendingAction]
    }

    func loadPendingActions(for configuration: ConnectionConfiguration) throws -> [String: PendingAction] {
        var request = query
        request[kSecAttrAccount as String] = "pending-actions"
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return [:] }
        guard status == errSecSuccess, let data = result as? Data else {
            throw ConnectionError.keychain(status)
        }
        let envelope = try JSONDecoder().decode(PendingEnvelope.self, from: data)
        // A replacement credential can never replay an older device/host's work,
        // even if updating one of the two Keychain records was interrupted.
        return envelope.connection == configuration ? envelope.actions : [:]
    }

    func savePendingActions(_ actions: [String: PendingAction], for configuration: ConnectionConfiguration? = nil) throws {
        var request = query
        request[kSecAttrAccount as String] = "pending-actions"
        if actions.isEmpty {
            let deleted = SecItemDelete(request as CFDictionary)
            guard deleted == errSecSuccess || deleted == errSecItemNotFound else {
                throw ConnectionError.keychain(deleted)
            }
            return
        }
        guard let configuration else { throw ConnectionError.disconnected }
        let data = try JSONEncoder().encode(PendingEnvelope(connection: configuration, actions: actions))
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        let status = SecItemUpdate(request as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            attributes.forEach { request[$0.key] = $0.value }
            let added = SecItemAdd(request as CFDictionary, nil)
            guard added == errSecSuccess else { throw ConnectionError.keychain(added) }
        } else if status != errSecSuccess {
            throw ConnectionError.keychain(status)
        }
    }

    func loadLibrary() throws -> ConnectionLibrary? {
        var request = query
        request[kSecAttrAccount as String] = "computer-library-v2"
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw ConnectionError.keychain(status) }
        return try JSONDecoder().decode(ConnectionLibrary.self, from: data).validated()
    }

    func saveLibrary(_ library: ConnectionLibrary) throws {
        let validated = try library.validated()
        var request = query
        request[kSecAttrAccount as String] = "computer-library-v2"
        let attributes: [String: Any] = [
            kSecValueData as String: try JSONEncoder().encode(validated),
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        let status = SecItemUpdate(request as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            attributes.forEach { request[$0.key] = $0.value }
            let added = SecItemAdd(request as CFDictionary, nil)
            guard added == errSecSuccess else { throw ConnectionError.keychain(added) }
        } else if status != errSecSuccess { throw ConnectionError.keychain(status) }
    }

    func deleteLibrary() throws {
        var request = query
        request[kSecAttrAccount as String] = "computer-library-v2"
        let status = SecItemDelete(request as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw ConnectionError.keychain(status) }
    }
}
