import Foundation

struct SessionPage: Decodable, Sendable {
    let sessions: [RemoteSession]
    let nextCursor: String?
    var partial: Bool? = nil
    enum CodingKeys: String, CodingKey { case sessions, partial; case nextCursor = "next_cursor" }
}

struct RemoteFileEntry: Decodable, Identifiable, Hashable, Sendable {
    let path: String
    let name: String
    let kind: String
    let sizeBytes: Int64?
    var id: String { path }
    var isDirectory: Bool { kind == "directory" }
    enum CodingKeys: String, CodingKey { case path, name, kind; case sizeBytes = "size_bytes" }
}

struct RemoteDirectory: Decodable, Sendable {
    let path: String
    let entries: [RemoteFileEntry]
    let truncated: Bool
}

struct RemoteFile: Decodable, Sendable {
    let path: String
    let content: String
    let revision: String
    let language: String?
    let truncated: Bool
}

struct RemoteGitFile: Decodable, Identifiable, Hashable, Sendable {
    let path: String
    let indexStatus: String
    let worktreeStatus: String
    let staged: Bool
    let untracked: Bool
    var id: String { path }
    enum CodingKeys: String, CodingKey {
        case path, staged, untracked
        case indexStatus = "index_status"
        case worktreeStatus = "worktree_status"
    }
    var statusTitle: String {
        if untracked { return "Nuevo" }
        if worktreeStatus.trimmingCharacters(in: .whitespaces).isEmpty { return "Preparado" }
        return staged ? "Preparado y modificado" : "Modificado"
    }
}

struct RemoteGitStatus: Decodable, Sendable {
    let revision: String
    let branch: String?
    let files: [RemoteGitFile]
    let clean: Bool
    let commit: String?
}

struct RemoteFileDiff: Decodable, Sendable {
    let path: String
    let diff: String
    let truncated: Bool
}

struct RemoteModel: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let provider: String
    var reasoningEfforts: [String]? = nil
    var supportsImages: Bool? = nil
    enum CodingKeys: String, CodingKey {
        case id, name, provider
        case reasoningEfforts = "reasoning_efforts"
        case supportsImages = "supports_images"
    }
}

struct RemoteModelCatalog: Decodable, Sendable {
    let models: [RemoteModel]
    let defaultModelID: String?
    var selectionEnabled: Bool? = nil
    enum CodingKeys: String, CodingKey {
        case models
        case defaultModelID = "default_model_id"
        case selectionEnabled = "selection_enabled"
    }
}

enum RemoteApprovalPolicy: String, Codable, CaseIterable, Hashable, Sendable {
    case ask, auto
    case fullAccess = "full_access"
}

/// Frozen before transmission. Missing approval policy keeps the host's Ask default.
struct RemoteTurnOptions: Codable, Hashable, Sendable {
    var modelID: String? = nil
    var reasoningEffort: String? = nil
    var approvalPolicy: RemoteApprovalPolicy? = nil
}

/// Stored before sending; retries reuse the exact method, path, payload and ID.
struct DurableMutation: Codable, Identifiable, Sendable {
    let requestID: String
    let key: String
    let label: String
    let method: String
    let path: [String]
    let body: [String: JSONValue]
    var rejectionCode: String?
    var id: String { requestID }
    var canDiscard: Bool {
        rejectionCode == "file_conflict" || rejectionCode == "git_conflict"
    }
    var previewText: String {
        var preview = body
        if case .string(let encoded)? = preview["data_base64"] {
            preview["data_base64"] = .string("[Contenido adjunto guardado: \(encoded.utf8.count) bytes codificados]")
        }
        for (key, value) in preview {
            if case .string(let text) = value, text.count > 8_000 {
                preview[key] = .string(String(text.prefix(8_000)) + "\n[Vista recortada; el envío original permanece completo]")
            }
        }
        return JSONValue.object(preview).displayText
    }
}

struct SavedComputer: Codable, Identifiable, Sendable {
    let id: String
    var name: String
    let configuration: ConnectionConfiguration
    var lastUsedAt: Date
    var pendingActions: [String: PendingAction]
    var pendingMutations: [String: DurableMutation]
    var queuedPrompts: [QueuedPrompt]
    var drafts: [String: String]? = nil
    var deviceID: String? = nil
    var notificationsEnabled: Bool? = nil
    var attachments: [String: [RemoteAttachment]]? = nil
}

struct ConnectionLibrary: Codable, Sendable {
    var computers: [SavedComputer] = []
    var activeID: String?
    var pendingPairing: PendingPairing? = nil

    func validated() throws -> ConnectionLibrary {
        guard computers.count <= 20, Set(computers.map(\.id)).count == computers.count else {
            throw ConnectionError.invalidResponse
        }
        for computer in computers {
            _ = try ConnectionConfiguration(address: computer.configuration.displayAddress, token: computer.configuration.token)
        }
        return self
    }
}

struct QueuedPrompt: Codable, Identifiable, Sendable {
    let id: String
    let sessionID: String
    let text: String
    let mode: TurnMode
    let createdAt: Date
    var options: RemoteTurnOptions? = nil
}

enum SessionListFilter: String, CaseIterable, Identifiable {
    case active, favorites, attention, archived
    var id: String { rawValue }
    var title: String {
        switch self {
        case .active: "Recientes"
        case .favorites: "Favoritas"
        case .attention: "Necesitan atención"
        case .archived: "Archivadas"
        }
    }
    func includes(_ session: RemoteSession) -> Bool {
        switch self {
        case .active: session.archived != true
        case .favorites: session.favorite == true && session.archived != true
        case .attention: !session.pendingPermissions.isEmpty && session.archived != true
        case .archived: session.archived == true
        }
    }
}
