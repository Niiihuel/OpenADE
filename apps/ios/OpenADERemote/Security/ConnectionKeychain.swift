import Foundation
import Security

/// Paired PCs, their tokens and pins, in one Keychain item for this device only.
struct ConnectionKeychain {
    private let service: String
    private let account = "computer-library-v3"
    /// Records of the old daemon's protocol: their tokens open nothing now.
    private let legacyAccounts = ["paired-computer", "pending-actions", "computer-library-v2"]

    init(service: String = "dev.openade.remote.connection") {
        self.service = service
    }

    private func query(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    func loadLibrary() throws -> ConnectionLibrary? {
        var request = query(account)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw ConnectionError.keychain(status) }
        // Validate what was stored too; never silently use a malformed route or pin.
        return try JSONDecoder().decode(ConnectionLibrary.self, from: data).validated()
    }

    func saveLibrary(_ library: ConnectionLibrary) throws {
        let data = try JSONEncoder().encode(library.validated())
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        let status = SecItemUpdate(query(account) as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var request = query(account)
            attributes.forEach { request[$0.key] = $0.value }
            let added = SecItemAdd(request as CFDictionary, nil)
            guard added == errSecSuccess else { throw ConnectionError.keychain(added) }
        } else if status != errSecSuccess {
            throw ConnectionError.keychain(status)
        }
    }

    func deleteLibrary() throws {
        let status = SecItemDelete(query(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw ConnectionError.keychain(status) }
    }

    func deleteLegacyRecords() {
        for legacy in legacyAccounts { SecItemDelete(query(legacy) as CFDictionary) }
    }
}
