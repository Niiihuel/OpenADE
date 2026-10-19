import Foundation

/// A task as the PC's sidebar shows it: native, ACP or a CLI in a terminal.
struct RemoteTask: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    let project: String
    let agent: String
    let backend: String
    let updatedAtMS: Int64
    let pinned: Bool
    let unread: Bool
    /// `idle`, `running` or `waiting` (for the person).
    let state: String
    let queued: Int
    /// False for CLI tasks: they run in a terminal on the PC and have no chat here.
    let chat: Bool

    enum CodingKeys: String, CodingKey {
        case id, title, project, agent, backend, pinned, unread, state, queued, chat
        case updatedAtMS = "updated_at_ms"
    }

    var isWaiting: Bool { state == "waiting" }
    var isRunning: Bool { state == "running" || state == "waiting" }
    var updatedAt: Date { Date(timeIntervalSince1970: Double(updatedAtMS) / 1_000) }
    var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Nueva tarea" : trimmed
    }

    func with(title: String? = nil, pinned: Bool? = nil, unread: Bool? = nil) -> RemoteTask {
        RemoteTask(id: id, title: title ?? self.title, project: project, agent: agent, backend: backend,
                   updatedAtMS: updatedAtMS, pinned: pinned ?? self.pinned, unread: unread ?? self.unread,
                   state: state, queued: queued, chat: chat)
    }
}

struct RemoteProject: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let path: String
}

/// An agent a new task can be started with.
struct RemoteAgent: Codable, Identifiable, Hashable, Sendable {
    let backend: String
    let name: String
    var id: String { backend }
}

struct ServerInfo: Codable, Hashable, Sendable {
    let name: String
    let serverID: String
    let routes: [String]
    let push: Bool
    enum CodingKeys: String, CodingKey {
        case name, routes, push
        case serverID = "server_id"
    }
}

/// What the stream's `overview` event carries, and what is cached for offline reading.
struct TaskListSnapshot: Codable, Equatable, Sendable {
    var tasks: [RemoteTask] = []
    var projects: [RemoteProject] = []
    var agents: [RemoteAgent] = []
}

struct OverviewResponse: Decodable, Sendable {
    let server: ServerInfo
    let tasks: [RemoteTask]
    let projects: [RemoteProject]
    let agents: [RemoteAgent]
    var snapshot: TaskListSnapshot { TaskListSnapshot(tasks: tasks, projects: projects, agents: agents) }
}

/// `GET /.well-known/openade`: answered without a token, so the phone can
/// check it reached its own PC before it sends one. It names nothing else.
struct ServerIdentity: Decodable, Sendable {
    let api: Int
    let serverID: String
    let fingerprint: String
    enum CodingKeys: String, CodingKey {
        case api, fingerprint
        case serverID = "server_id"
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

    subscript(key: String) -> JSONValue? {
        if case .object(let object) = self { return object[key] }
        return nil
    }
    var stringValue: String? { if case .string(let text) = self { return text }; return nil }
    var arrayValue: [JSONValue]? { if case .array(let items) = self { return items }; return nil }
    var boolValue: Bool? { if case .bool(let bool) = self { return bool }; return nil }

    /// Compact JSON with sorted keys: what an answer string or a test compares.
    var compactText: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return (try? encoder.encode(self)).map { String(decoding: $0, as: UTF8.self) } ?? ""
    }

    var displayText: String {
        if case .string(let text) = self { return text }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(self) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }
}

/// One public journal event. The same shape v1 projected, now streamed.
struct RemoteEvent: Codable, Equatable, Sendable {
    let seq: Int64
    let timestampMS: Int64
    let type: String
    var text: String? = nil
    var turnID: String? = nil
    var messageID: String? = nil
    var requestID: String? = nil
    var toolCallID: String? = nil
    var toolName: String? = nil
    var arguments: JSONValue? = nil
    var decision: String? = nil
    var isError: Bool? = nil

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

/// `GET /v2/tasks/{id}/history`: a window back to the last few prompts.
struct TaskHistory: Codable, Sendable {
    let events: [RemoteEvent]
    let pending: [PendingRequest]
    /// The journal's last sequence: where the stream goes on from.
    let cursor: Int64
    let hasMore: Bool
    enum CodingKeys: String, CodingKey {
        case events, pending, cursor
        case hasMore = "has_more"
    }
}

struct StreamEventsPayload: Decodable { let task: String; let events: [RemoteEvent] }
struct StreamPendingPayload: Decodable { let task: String; let pending: [PendingRequest] }
struct StreamTaskPayload: Decodable { let task: String }

struct UploadedFile: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let filename: String
    let sizeBytes: Int64
    enum CodingKeys: String, CodingKey {
        case id, filename
        case sizeBytes = "size_bytes"
    }
}

enum PermissionDecision: String, Sendable {
    /// This once.
    case allow
    /// This tool for the rest of the task.
    case allowSession = "allow_session"
    /// The exact command prefix the PC named in `always`, from now on.
    case allowAlways = "allow_always"
    case deny
}

enum MessageDelivery: String, Sendable { case auto, queue }

extension String {
    /// Without control or bidirectional-format characters, which can make a
    /// name read as another one.
    var displaySafe: String {
        var scalars = String.UnicodeScalarView()
        scalars.append(contentsOf: unicodeScalars.filter {
            $0.properties.generalCategory != .control && $0.properties.generalCategory != .format
        })
        return String(scalars).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

extension Date {
    /// "14:30": the app speaks Spanish whatever the iPhone's language.
    var spanishTime: String { formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: Locale(identifier: "es"))) }
}
