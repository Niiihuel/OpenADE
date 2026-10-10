import Foundation

struct ConnectionConfiguration: Codable, Equatable, Sendable {
    let baseURL: URL
    let token: String

    init(address: String, token: String) throws {
        guard var components = URLComponents(string: address.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.scheme?.lowercased() == "https",
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else { throw ConnectionError.invalidAddress }
        let secret = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !secret.isEmpty, secret.unicodeScalars.allSatisfy({ (33...126).contains($0.value) }) else {
            throw ConnectionError.missingToken
        }
        components.path = ""
        guard let url = components.url else { throw ConnectionError.invalidAddress }
        baseURL = url
        self.token = secret
    }

    var displayAddress: String { baseURL.absoluteString }
}

enum ConnectionError: LocalizedError {
    case invalidAddress
    case missingToken
    case keychain(OSStatusCode)
    case invalidResponse
    case unauthorized
    case http(Int)
    case disconnected
    case unsupportedVersion

    var errorDescription: String? {
        switch self {
        case .invalidAddress:
            return "Usá una dirección HTTPS de tu PC, sin rutas ni datos de acceso."
        case .missingToken:
            return "Ingresá el token de conexión que muestra openADE en tu PC."
        case .keychain(let status):
            return "No se pudo acceder al llavero del iPhone (código \(status))."
        case .invalidResponse:
            return "La PC devolvió una respuesta que esta versión de la app no reconoce."
        case .unauthorized:
            return "El acceso fue revocado o el token venció. Volvé a conectar tu PC."
        case .http(let status):
            return "La PC no pudo completar la solicitud (HTTP \(status))."
        case .disconnected:
            return "Se perdió la conexión con la PC."
        case .unsupportedVersion:
            return "La versión de openADE en tu PC no es compatible con esta app."
        }
    }
}

// Keep security error codes portable to Foundation-only test targets.
typealias OSStatusCode = Int32
