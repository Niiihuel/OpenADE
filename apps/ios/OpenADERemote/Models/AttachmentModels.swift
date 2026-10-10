import Foundation
import CryptoKit

struct RemoteAttachment: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let filename: String
    let mediaType: String
    let sizeBytes: Int64
    let sha256: String
    enum CodingKeys: String, CodingKey {
        case id, filename, sha256
        case mediaType = "media_type"
        case sizeBytes = "size_bytes"
    }
    var isImage: Bool { mediaType.hasPrefix("image/") }
}

struct AttachmentUpload: Sendable {
    let filename: String
    let mediaType: String
    let data: Data

    init(filename: String, mediaType: String, data: Data) throws {
        guard !filename.isEmpty, filename.utf8.count <= 255, filename != ".", filename != "..",
              !filename.contains("/"), !filename.contains("\\"),
              !filename.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw AttachmentError.invalidName
        }
        if mediaType == "text/plain" {
            guard data.count <= 256 * 1024 else { throw AttachmentError.textTooLarge }
            guard let text = String(data: data, encoding: .utf8),
                  !text.unicodeScalars.contains(where: {
                      CharacterSet.controlCharacters.contains($0) && ![9, 10, 13].contains(Int($0.value))
                  }) else { throw AttachmentError.invalidText }
        } else {
            guard ["image/jpeg", "image/png", "image/webp", "image/gif"].contains(mediaType),
                  !data.isEmpty, data.count <= 4 * 1024 * 1024 else { throw AttachmentError.imageTooLarge }
        }
        self.filename = filename; self.mediaType = mediaType; self.data = data
    }

    var body: [String: JSONValue] {
        ["filename": .string(filename), "media_type": .string(mediaType),
         "data_base64": .string(data.base64EncodedString()),
         "sha256": .string(SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined())]
    }
}

enum AttachmentError: LocalizedError {
    case invalidName, invalidText, textTooLarge, imageTooLarge, unreadableImage, limit, pendingUpload
    var errorDescription: String? {
        switch self {
        case .invalidName: "El nombre del adjunto no es válido."
        case .invalidText: "Elegí un archivo de texto UTF-8, sin contenido binario."
        case .textTooLarge: "Cada archivo de texto puede tener hasta 256 KB."
        case .imageTooLarge: "La imagen debe ocupar hasta 4 MB."
        case .unreadableImage: "No se pudo leer esta imagen."
        case .limit: "Podés adjuntar hasta 8 archivos y 1 MB de texto por mensaje."
        case .pendingUpload: "Confirmá el adjunto pendiente antes de agregar otro."
        }
    }
}
