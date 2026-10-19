import Foundation

/// What the PC's QR code carries: where it answers, the certificate the phone
/// pins, and a secret good once for ten minutes.
struct PairingPayload: Codable, Equatable, Identifiable, Sendable {
    let version: Int
    let routes: [String]
    let fingerprint: String
    let serverID: String
    let pairingID: String
    let secret: String
    let expiresAt: Int64
    let label: String
    var id: String { pairingID }
    var shortFingerprint: String { CertificatePin.short(fingerprint) }

    enum CodingKeys: String, CodingKey {
        case version, routes, fingerprint, secret, label
        case serverID = "server_id"
        case pairingID = "pairing_id"
        case expiresAt = "expires_at"
    }

    static func parse(_ source: String, now: Date = .now) throws -> PairingPayload {
        guard source.utf8.count <= 12_000 else { throw PairingError.invalidCode }
        let source = source.trimmingCharacters(in: .whitespacesAndNewlines)
        let data: Data
        if source.hasPrefix("{") { data = Data(source.utf8) }
        else {
            guard let url = URLComponents(string: source), url.scheme == "openade-remote", url.host == "pair",
                  url.query == nil, url.user == nil, url.password == nil, url.path.isEmpty || url.path == "/",
                  let fragment = url.fragment, !fragment.isEmpty else { throw PairingError.invalidCode }
            var base64 = fragment.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
            guard let decoded = Data(base64Encoded: base64) else { throw PairingError.invalidCode }
            data = decoded
        }
        struct Version: Decodable { let version: Int }
        guard let version = try? JSONDecoder().decode(Version.self, from: data) else { throw PairingError.invalidCode }
        guard version.version == 2 else { throw PairingError.unsupportedVersion }
        guard let payload = try? JSONDecoder().decode(PairingPayload.self, from: data) else { throw PairingError.invalidCode }
        func lowercaseHex(_ value: String) -> Bool { value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) } }
        guard (1...8).contains(payload.routes.count), payload.routes.allSatisfy({ (try? RemoteRoute.url($0)) != nil }),
              lowercaseHex(payload.fingerprint), lowercaseHex(payload.secret),
              UUID(uuidString: payload.serverID) != nil, UUID(uuidString: payload.pairingID) != nil,
              !payload.label.isEmpty, payload.label.utf8.count <= 256 else { throw PairingError.invalidCode }
        guard Double(payload.expiresAt) > now.timeIntervalSince1970 else { throw PairingError.expired }
        return payload
    }
}

/// `POST /v2/pair`: this phone's own token, and what the PC is.
struct PairingResult: Decodable, Sendable {
    let deviceID: String
    let deviceName: String
    let token: String
    let serverID: String
    let name: String
    let fingerprint: String
    let routes: [String]
    enum CodingKeys: String, CodingKey {
        case token, name, fingerprint, routes
        case deviceID = "device_id"
        case deviceName = "device_name"
        case serverID = "server_id"
    }
}

enum PairingError: LocalizedError, Equatable {
    case invalidCode, expired, unsupportedVersion, cameraUnavailable, unreachable, wrongComputer, certificateMismatch, changedCertificate
    var errorDescription: String? {
        switch self {
        case .invalidCode: "El código no es un emparejamiento válido de openADE."
        case .expired: "El código venció. Generá otro desde openADE en tu PC."
        case .unsupportedVersion: "Este código es de otra versión de openADE. Actualizá openADE en tu PC y esta app."
        case .cameraUnavailable: "No se pudo usar la cámara. Permití el acceso en Ajustes o pegá el enlace de emparejamiento."
        case .unreachable: "No encontramos tu PC en ninguna de sus direcciones. Revisá que openADE esté abierto y que el iPhone esté en la misma red."
        case .wrongComputer: "La PC que respondió no es la del código. Por seguridad no se emparejó."
        case .certificateMismatch: "Una dirección respondió con otro certificado. Por seguridad no se emparejó."
        case .changedCertificate: "Ya tenés esta PC emparejada con otro certificado. Si la PC lo cambió, olvidala en Ajustes y volvé a emparejar."
        }
    }
}
