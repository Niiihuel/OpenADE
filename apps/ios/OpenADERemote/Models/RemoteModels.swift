import Foundation

struct RemoteProject: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let path: String
}

struct RemoteSession: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let title: String
    let status: String
    let updatedAtMS: Int64
    let pendingPermissions: [RemotePermission]
    let activeTurnID: String?
    let archived: Bool?
    let favorite: Bool?
    let origin: String?
    let canStartTurn: Bool?
    let canControlActive: Bool?

    enum CodingKeys: String, CodingKey {
        case id, title, status, archived, favorite
        case projectID = "project_id"
        case updatedAtMS = "updated_at_ms"
        case pendingPermissions = "pending_permissions"
        case activeTurnID = "active_turn_id"
        case origin
        case canStartTurn = "can_start_turn"
        case canControlActive = "can_control_active"
    }

    var isBusy: Bool { status == "running" || status == "waiting_permission" }
    var updatedAt: Date { Date(timeIntervalSince1970: Double(updatedAtMS) / 1_000) }
    var statusTitle: String {
        switch status {
        case "idle": "Lista"
        case "running": "Trabajando"
        case "waiting_permission": "Necesita aprobación"
        case "completed": "Completada"
        case "failed": "Con error"
        case "cancelled": "Detenida"
        case "interrupted": "Interrumpida"
        default: "Estado desconocido"
        }
    }
}

struct RemotePermission: Codable, Identifiable, Hashable, Sendable {
    let requestID: String
    let toolCallID: String
    let toolName: String
    let arguments: JSONValue
    let argumentsTruncated: Bool?
    var id: String { requestID }

    enum CodingKeys: String, CodingKey {
        case arguments
        case requestID = "request_id"
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case argumentsTruncated = "arguments_truncated"
    }
}

enum JSONValue: Codable, Hashable, Sendable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let bool = try? value.decode(Bool.self) { self = .bool(bool) }
        else if let string = try? value.decode(String.self) { self = .string(string) }
        else if let number = try? value.decode(Double.self) { self = .number(number) }
        else if let object = try? value.decode([String: JSONValue].self) { self = .object(object) }
        else { self = .array(try value.decode([JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .string(let text): try value.encode(text)
        case .number(let number): try value.encode(number)
        case .bool(let bool): try value.encode(bool)
        case .object(let object): try value.encode(object)
        case .array(let array): try value.encode(array)
        case .null: try value.encodeNil()
        }
    }

    var displayText: String {
        if case .string(let text) = self { return text }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(self) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }
}

struct RemoteEvent: Decodable, Sendable {
    let seq: Int64
    let timestampMS: Int64
    let type: String
    let text: String?
    let turnID: String?

    let messageID: String?
    let requestID: String?
    let toolCallID: String?
    let toolName: String?
    let arguments: JSONValue?
    let decision: String?
    let isError: Bool?

    enum CodingKeys: String, CodingKey {
        case seq, type, text, arguments, decision
        case timestampMS = "timestamp_ms"
        case turnID = "turn_id"
        case messageID = "message_id"
        case requestID = "request_id"
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case isError = "is_error"
    }
}

struct EventPage: Decodable, Sendable {
    let events: [RemoteEvent]
    let nextCursor: Int64
    let hasMore: Bool
    enum CodingKeys: String, CodingKey {
        case events
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }
}

struct ProjectDiff: Decodable, Sendable {
    let diff: String
    let truncated: Bool
    let includesUntracked: Bool?
    enum CodingKeys: String, CodingKey {
        case diff, truncated
        case includesUntracked = "includes_untracked"
    }
}

enum TurnMode: String, Codable, CaseIterable, Identifiable, Sendable {
    case plan, build
    var id: String { rawValue }
    var title: String { self == .plan ? "Planear" : "Construir" }
}

struct PendingAction: Codable, Sendable {
    enum Kind: String, Codable, Sendable { case createSession, sendTurn, steerTurn, cancelTurn }
    let requestID: String
    let kind: Kind
    let resourceID: String
    let prompt: String?
    let mode: TurnMode?
    let turnID: String?
    var attachmentIDs: [String]? = nil
    var options: RemoteTurnOptions? = nil

    var key: String { "\(kind.rawValue):\(resourceID)" }
}

enum PendingActionRetentionPolicy {
    static func mayDiscardAfterRejection(status: Int, code: String) -> Bool {
        // A conflict may describe a durable operation whose outcome is unknown.
        // Keep its request ID unless the host proves there was no accepted effect.
        [400, 404, 422].contains(status) || (status == 413 && code == "invalid_request")
    }
}
