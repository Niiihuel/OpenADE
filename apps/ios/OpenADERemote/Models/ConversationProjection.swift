import Foundation

struct ConversationEntry: Identifiable, Equatable, Sendable {
    enum Kind: Equatable, Sendable { case user, assistant, tool, notice }
    let id: String
    let kind: Kind
    let turnID: String
    var text: String
    var toolName: String? = nil
    var arguments: JSONValue? = nil
    var isError = false
    var isRunning = false
    var textTruncated = false
}

struct ToolCallItem: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let kind: ToolKind
    let arguments: JSONValue?
    let output: String
    let isError: Bool
    let isRunning: Bool
    var label: String { ToolKind.label(for: self) }

    init(id: String, name: String, arguments: JSONValue? = nil, output: String = "", isError: Bool = false, isRunning: Bool = false) {
        self.id = id; self.name = name; self.kind = ToolKind(toolName: name); self.arguments = arguments
        self.output = output; self.isError = isError; self.isRunning = isRunning
    }

    init(_ entry: ConversationEntry) {
        self.init(id: entry.id, name: entry.toolName ?? "Herramienta", arguments: entry.arguments,
                  output: entry.text, isError: entry.isError, isRunning: entry.isRunning)
    }
}

/// Consecutive tool calls, shown as one row that summarises them by kind.
struct ToolGroup: Identifiable, Equatable, Sendable {
    let id: String
    let calls: [ToolCallItem]
    var isRunning: Bool { calls.contains(where: \.isRunning) }
    var failures: Int { calls.filter(\.isError).count }
    var summary: String { ToolKind.summary(calls) }
}

/// A finished turn's work folded behind one row: "Trabajó 2 min".
struct TurnWork: Identifiable, Equatable, Sendable {
    let id: String
    let durationMS: Int64?
    let items: [ConversationItem]
    let tools: [ToolCallItem]
    var title: String { TurnWork.title(durationMS: durationMS) }
    var summary: String? { tools.isEmpty ? nil : ToolKind.summary(tools) }
    var failures: Int { tools.filter(\.isError).count }

    static func title(durationMS: Int64?) -> String {
        guard let durationMS, durationMS >= 0 else { return "Trabajó" }
        let seconds = Int(durationMS / 1_000)
        if seconds < 60 { return "Trabajó \(max(seconds, 1)) s" }
        let minutes = seconds / 60
        if minutes < 60 { return "Trabajó \(minutes) min" }
        return minutes % 60 == 0 ? "Trabajó \(minutes / 60) h" : "Trabajó \(minutes / 60) h \(minutes % 60) min"
    }
}

enum ConversationItem: Identifiable, Equatable, Sendable {
    case user(ConversationEntry)
    case assistant(ConversationEntry)
    case notice(ConversationEntry)
    case tools(ToolGroup)
    case work(TurnWork)

    var id: String {
        switch self {
        case .user(let entry), .assistant(let entry), .notice(let entry): entry.id
        case .tools(let group): group.id
        case .work(let work): work.id
        }
    }
}

/// Replays ordered host events. Duplicate pages are safe to consume after a reconnect.
struct ConversationProjection: Equatable, Sendable {
    private struct Turn: Equatable, Sendable {
        enum Outcome: Equatable, Sendable { case running, completed, ended }
        var startMS: Int64
        var endMS: Int64?
        var outcome = Outcome.running
    }

    private(set) var entries: [ConversationEntry] = []
    /// What the conversation shows: entries grouped and finished turns folded.
    private(set) var items: [ConversationItem] = []
    private(set) var cursor: Int64 = 0
    /// Changes whenever the visible content does, so a view can follow the end.
    private(set) var revision = 0
    private var indices: [String: Int] = [:]
    private var turns: [String: Turn] = [:]

    /// When the turn still running began, for "Trabajando 1:12".
    var liveTurnStartMS: Int64? { turns.values.filter { $0.outcome == .running }.map(\.startMS).max() }

    mutating func apply(_ events: [RemoteEvent]) {
        var changed = false
        for event in events.sorted(by: { $0.seq < $1.seq }) where event.seq > cursor {
            apply(event)
            cursor = event.seq
            changed = true
        }
        guard changed else { return }
        trimTranscript()
        rebuildItems()
        revision += 1
    }

    private mutating func apply(_ event: RemoteEvent) {
        let turn = event.turnID ?? "event-\(event.seq)"
        switch event.type {
        case "user_message":
            if turns[turn] == nil { turns[turn] = Turn(startMS: event.timestampMS) }
            append(ConversationEntry(id: "user-\(event.seq)", kind: .user, turnID: turn, text: event.text ?? ""))
        case "assistant_delta", "assistant_message":
            let index = ensure(ConversationEntry(id: "assistant-\(event.messageID ?? turn)", kind: .assistant, turnID: turn, text: ""))
            if event.type == "assistant_message" {
                // The committed text replaces the deltas streamed for this step.
                entries[index].text = event.text ?? ""
                entries[index].textTruncated = false
                entries[index].isRunning = false
            } else {
                entries[index].text += event.text ?? ""
                entries[index].isRunning = true
            }
        case "tool_started", "tool_output", "tool_finished":
            let index = ensure(ConversationEntry(id: "tool-\(turn)-\(event.toolCallID ?? String(event.seq))", kind: .tool,
                                                 turnID: turn, text: ""))
            if let name = event.toolName { entries[index].toolName = name }
            if let arguments = event.arguments { entries[index].arguments = arguments }
            if let text = event.text {
                if event.type == "tool_finished" { entries[index].text = text } else { entries[index].text += text }
            }
            entries[index].isRunning = event.type != "tool_finished"
            entries[index].isError = event.isError ?? entries[index].isError
        case "turn_completed", "turn_failed", "turn_cancelled", "turn_superseded":
            for index in entries.indices where entries[index].turnID == turn { entries[index].isRunning = false }
            var state = turns[turn] ?? Turn(startMS: event.timestampMS)
            state.endMS = event.timestampMS
            state.outcome = event.type == "turn_completed" ? .completed : .ended
            turns[turn] = state
            if let text = Self.notice(for: event) {
                append(ConversationEntry(id: "notice-\(event.seq)", kind: .notice, turnID: turn, text: text,
                                         isError: event.type == "turn_failed"))
            }
        case "turn_paused":
            for index in entries.indices where entries[index].turnID == turn { entries[index].isRunning = false }
            append(ConversationEntry(id: "notice-\(event.seq)", kind: .notice, turnID: turn, text: Self.pausedNotice(event.text)))
        default:
            // Permissions come from the pending list. Unknown events still advance
            // the cursor, so a newer host cannot trap the replay loop.
            break
        }
    }

    private static func notice(for event: RemoteEvent) -> String? {
        switch event.type {
        case "turn_failed": return event.text ?? "El turno terminó con un error."
        case "turn_cancelled": return "Turno detenido."
        case "turn_superseded":
            let model = event.text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return model.isEmpty ? "El turno siguió con otro modelo." : "El turno siguió con \(model)."
        default: return nil
        }
    }

    private static func pausedNotice(_ text: String?) -> String {
        let details = text.flatMap { try? JSONDecoder().decode(JSONValue.self, from: Data($0.utf8)) }
        if case .number(let resets)? = details?["resets_at_ms"] {
            return "En pausa por el límite de uso. Sigue a las \(Date(timeIntervalSince1970: resets / 1_000).spanishTime)."
        }
        return "En pausa por el límite de uso."
    }

    private mutating func append(_ entry: ConversationEntry) {
        guard indices[entry.id] == nil else { return }
        indices[entry.id] = entries.count
        entries.append(entry)
    }

    private mutating func ensure(_ entry: ConversationEntry) -> Int {
        if let index = indices[entry.id] { return index }
        append(entry)
        return entries.count - 1
    }

    private mutating func trimTranscript() {
        let perEntryLimit = 128 * 1_024
        for index in entries.indices {
            if entries[index].text.utf8.count > perEntryLimit {
                entries[index].text = String(decoding: entries[index].text.utf8.suffix(perEntryLimit), as: UTF8.self)
                entries[index].textTruncated = true
            }
        }
        var bytes = entries.reduce(0) { $0 + $1.text.utf8.count }
        var removed = false
        while entries.count > 300 || bytes > 2 * 1_024 * 1_024 {
            let index = entries.firstIndex { !$0.isRunning } ?? 0
            bytes -= entries.remove(at: index).text.utf8.count
            removed = true
        }
        if removed {
            indices = Dictionary(uniqueKeysWithValues: entries.enumerated().map { ($0.element.id, $0.offset) })
            let live = Set(entries.map(\.turnID))
            turns = turns.filter { live.contains($0.key) }
        }
    }

    private mutating func rebuildItems() {
        var result: [ConversationItem] = []
        var start = 0
        while start < entries.count {
            let turnID = entries[start].turnID
            var end = start
            while end < entries.count && entries[end].turnID == turnID { end += 1 }
            let slice = Array(entries[start..<end])
            if let turn = turns[turnID], turn.outcome == .completed {
                result += Self.folded(slice, turnID: turnID, durationMS: turn.endMS.map { $0 - turn.startMS })
            } else {
                result += Self.flat(slice)
            }
            start = end
        }
        items = result
    }

    /// Consecutive tool calls fold into one group; everything else stays a row.
    static func flat(_ entries: [ConversationEntry]) -> [ConversationItem] {
        var result: [ConversationItem] = []
        var calls: [ToolCallItem] = []
        func flush() {
            if let first = calls.first { result.append(.tools(ToolGroup(id: "tools-\(first.id)", calls: calls))) }
            calls = []
        }
        for entry in entries {
            switch entry.kind {
            case .tool: calls.append(ToolCallItem(entry))
            case .user: flush(); result.append(.user(entry))
            case .assistant: flush(); result.append(.assistant(entry))
            case .notice: flush(); result.append(.notice(entry))
            }
        }
        flush()
        return result
    }

    /// A completed turn shows its prompt, one "Trabajó" row and its last answer.
    static func folded(_ entries: [ConversationEntry], turnID: String, durationMS: Int64?) -> [ConversationItem] {
        let prompts = entries.prefix { $0.kind == .user }
        var rest = Array(entries.dropFirst(prompts.count))
        var answer: ConversationEntry?
        if let last = rest.lastIndex(where: { $0.kind == .assistant }) { answer = rest.remove(at: last) }
        let notices = rest.filter { $0.kind == .notice && $0.isError }
        rest.removeAll { $0.kind == .notice && $0.isError }
        var result = prompts.map(ConversationItem.user)
        if !rest.isEmpty {
            result.append(.work(TurnWork(id: "work-\(turnID)", durationMS: durationMS, items: flat(rest),
                                         tools: rest.filter { $0.kind == .tool }.map { ToolCallItem($0) })))
        }
        if let answer { result.append(.assistant(answer)) }
        result += notices.map(ConversationItem.notice)
        return result
    }
}

/// A task's conversation on this phone: the raw window it was built from,
/// so an older page can be prepended, and what the task waits on.
struct TaskConversation: Sendable {
    private(set) var events: [RemoteEvent] = []
    private(set) var projection = ConversationProjection()
    var pending: [PendingRequest] = []
    /// Where the stream goes on from: the journal's last sequence.
    private(set) var cursor: Int64 = 0
    private(set) var hasMore = false
    /// History came from the PC in this session, not only from the cache.
    var loaded = false
    var loadingOlder = false

    init() {}

    init(events: [RemoteEvent], pending: [PendingRequest], cursor: Int64, hasMore: Bool) {
        self.pending = pending
        self.hasMore = hasMore
        append(events)
        self.cursor = max(cursor, projection.cursor)
    }

    /// A fresh window from history replaces what was here.
    mutating func replace(with history: TaskHistory) {
        self = TaskConversation(events: history.events, pending: history.pending, cursor: history.cursor, hasMore: history.hasMore)
        loaded = true
    }

    mutating func append(_ new: [RemoteEvent]) {
        let fresh = new.filter { $0.seq > projection.cursor }.sorted { $0.seq < $1.seq }
        guard !fresh.isEmpty else { return }
        projection.apply(fresh)
        events += fresh
        cursor = max(cursor, projection.cursor)
        compact()
    }

    mutating func prepend(_ older: TaskHistory) {
        let first = events.first?.seq ?? .max
        let earlier = older.events.filter { $0.seq < first }.sorted { $0.seq < $1.seq }
        hasMore = older.hasMore
        guard !earlier.isEmpty else { return }
        events = earlier + events
        var rebuilt = ConversationProjection()
        rebuilt.apply(events)
        projection = rebuilt
    }

    /// Committed text replaces its deltas and a finished tool its streamed
    /// output, so the raw window stays about as small as what it shows.
    private mutating func compact() {
        var committed = Set<String>()
        var finished = Set<String>()
        for event in events {
            if event.type == "assistant_message", let id = event.messageID { committed.insert(id) }
            if event.type == "tool_finished", let id = event.toolCallID { finished.insert(id) }
        }
        events.removeAll { event in
            (event.type == "assistant_delta" && event.messageID.map(committed.contains) == true)
                || (event.type == "tool_output" && event.toolCallID.map(finished.contains) == true)
        }
        if events.count > 3_000 {
            events.removeFirst(events.count - 3_000)
            hasMore = true
        }
    }
}
