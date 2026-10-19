import Foundation
import CryptoKit
import Security

/// One `https://host:port` address of a PC.
enum RemoteRoute {
    static func url(_ value: String) throws -> URL {
        guard var components = URLComponents(string: value.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.scheme?.lowercased() == "https",
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else { throw ConnectionError.invalidRoute }
        components.path = ""
        guard let url = components.url else { throw ConnectionError.invalidRoute }
        return url
    }

    /// "192.168.1.229:7787" for a person to read.
    static func display(_ value: String) -> String {
        value.replacingOccurrences(of: "https://", with: "")
    }
}

/// The PC's certificate is its own, so the pin is the only trust anchor:
/// SHA-256 of the leaf certificate's DER bytes, lowercase hex.
enum CertificatePin {
    static func fingerprint(of der: Data) -> String {
        SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    }

    static func isValid(_ pin: String) -> Bool {
        pin.utf8.count == 64 && pin.allSatisfy { $0.isASCII && $0.isHexDigit }
    }

    static func matches(der: Data, pinned: String) -> Bool {
        let expected = pinned.lowercased()
        guard isValid(expected) else { return false }
        let actual = fingerprint(of: der)
        // Constant time: every byte is compared whatever the first mismatch.
        return zip(actual.utf8, expected.utf8).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }

    static func matches(trust: SecTrust, pinned: String) -> Bool {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first else { return false }
        return matches(der: SecCertificateCopyData(leaf) as Data, pinned: pinned)
    }

    /// "ABCD-1234-EF56-7890": what the PC's Settings page shows to compare by eye.
    static func short(_ pin: String) -> String {
        let characters = Array(pin.uppercased().prefix(16))
        return stride(from: 0, to: characters.count, by: 4)
            .map { String(characters[$0..<min($0 + 4, characters.count)]) }
            .joined(separator: "-")
    }
}

/// Accepts a TLS handshake only from the pinned certificate, ignoring
/// hostname and CA validation, and never follows a redirect.
final class PinnedSessionDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    let fingerprint: String
    private let lock = NSLock()
    private var rejected = 0

    init(fingerprint: String) { self.fingerprint = fingerprint }

    /// How many handshakes presented another certificate.
    var rejections: Int { lock.withLock { rejected } }

    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        if CertificatePin.matches(trust: trust, pinned: fingerprint) {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            lock.withLock { rejected += 1 }
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// One pinned session per certificate, reused for every request and stream.
enum PinnedSessions {
    private static let lock = NSLock()
    private static var sessions: [String: (URLSession, PinnedSessionDelegate)] = [:]

    static func session(for fingerprint: String) -> (URLSession, PinnedSessionDelegate) {
        lock.withLock {
            if let existing = sessions[fingerprint] { return existing }
            let settings = URLSessionConfiguration.ephemeral
            settings.requestCachePolicy = .reloadIgnoringLocalCacheData
            settings.urlCache = nil
            settings.httpCookieStorage = nil
            settings.urlCredentialStorage = nil
            settings.timeoutIntervalForRequest = 20
            // The stream stays open for as long as the app is in the foreground.
            settings.timeoutIntervalForResource = 7 * 24 * 3_600
            settings.waitsForConnectivity = false
            let delegate = PinnedSessionDelegate(fingerprint: fingerprint.lowercased())
            let created = (URLSession(configuration: settings, delegate: delegate, delegateQueue: nil), delegate)
            sessions[fingerprint] = created
            return created
        }
    }
}

struct RemoteAPIError: LocalizedError, Sendable {
    let status: Int
    let code: String
    let message: String
    var errorDescription: String? { message }
}

enum ConnectionError: LocalizedError, Equatable {
    case invalidRoute, unauthorized, invalidResponse, notConnected, streamEnded
    case http(Int)
    case keychain(Int32)

    var errorDescription: String? {
        switch self {
        case .invalidRoute: "La dirección de la PC no es válida."
        case .unauthorized: "Acceso revocado; emparejá de nuevo desde openADE."
        case .invalidResponse: "La PC devolvió una respuesta que esta versión de la app no reconoce."
        case .notConnected: "Sin conexión con la PC. Probá de nuevo cuando vuelva."
        case .streamEnded: "Se cortó la conexión con la PC."
        case .http(let status): "La PC no pudo completar la solicitud (HTTP \(status))."
        case .keychain(let status): "No se pudo acceder al llavero del iPhone (código \(status))."
        }
    }

    /// Words for a network failure, never the system's English.
    static func readable(_ error: Error) -> String {
        if let network = error as? URLError {
            switch network.code {
            case .notConnectedToInternet: return "El iPhone está sin conexión."
            case .timedOut: return "La PC tardó demasiado en responder."
            case .cannotConnectToHost, .cannotFindHost, .networkConnectionLost, .dnsLookupFailed:
                return "No se pudo contactar a tu PC. Revisá que openADE esté abierto."
            case .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateHasUnknownRoot,
                 .serverCertificateNotYetValid, .secureConnectionFailed, .cancelled:
                return "No se pudo verificar la conexión segura con la PC."
            default: return "No se pudo completar la conexión. Volvé a intentarlo."
            }
        }
        return error.localizedDescription
    }
}
