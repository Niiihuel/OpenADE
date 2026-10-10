import Foundation

struct PairingPayload: Codable, Equatable, Identifiable, Sendable {
    let version: Int
    let baseURL: String
    let pairingID: String
    let secret: String
    let expiresAt: Int64
    let label: String
    var id: String { pairingID }
    enum CodingKeys: String, CodingKey {
        case version, secret, label
        case baseURL = "base_url"
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
        let payload: PairingPayload
        do { payload = try JSONDecoder().decode(PairingPayload.self, from: data) }
        catch { throw PairingError.invalidCode }
        guard payload.version == 1, UUID(uuidString: payload.pairingID) != nil,
              payload.secret.utf8.count == 64, payload.secret.allSatisfy({ $0.isHexDigit && $0.isASCII }),
              !payload.label.isEmpty, payload.label.utf8.count <= 256 else { throw PairingError.invalidCode }
        _ = try ConnectionConfiguration(address: payload.baseURL, token: "validation")
        guard Double(payload.expiresAt) > now.timeIntervalSince1970 else { throw PairingError.expired }
        return payload
    }
}

struct PendingPairing: Codable, Sendable {
    let payload: PairingPayload
    let requestID: String
    let deviceName: String
}

struct PairingResult: Decodable, Sendable {
    let version: Int
    let baseURL: String
    let deviceID: String
    let deviceName: String
    let token: String
    enum CodingKeys: String, CodingKey {
        case version, token
        case baseURL = "base_url"
        case deviceID = "device_id"
        case deviceName = "device_name"
    }
}

enum PairingError: LocalizedError {
    case invalidCode, expired, pending, cameraUnavailable
    var errorDescription: String? {
        switch self {
        case .invalidCode: "El código no es un emparejamiento válido de openADE."
        case .expired: "El código venció. Generá otro desde tu PC."
        case .pending: "Primero reintentá o descartá el emparejamiento pendiente."
        case .cameraUnavailable: "No se pudo usar la cámara. Permití el acceso en Ajustes o pegá el enlace de emparejamiento."
        }
    }
}
