import Foundation

struct ConversationEntry: Identifiable, Equatable {
    enum Kind: Equatable { case user, assistant, tool, notice }
    let id: String
    let kind: Kind
    var title: String
    var text: String
    var arguments: String?
    var isError = false
    var isRunning = false
    var textTruncated = false
}

/// Replays ordered host events. Duplicate pages are safe to consume after a reconnect.
struct ConversationProjection {
    private(set) var entries: [ConversationEntry] = []
    private(set) var cursor: Int64 = 0
    private(set) var didTrimHistory = false
    private var indices: [String: Int] = [:]

    mutating func apply(_ events: [RemoteEvent]) {
        for event in events.sorted(by: { $0.seq < $1.seq }) where event.seq > cursor {
            apply(event)
            cursor = event.seq
            trimTranscript()
        }
    }

    mutating func apply(_ page: EventPage) {
        apply(page.events)
        // The host advances past private/unexposed events too. Preserve its cursor,
        // including when a page contains no public events.
        cursor = max(cursor, page.nextCursor)
    }

    private mutating func apply(_ event: RemoteEvent) {
        let turn = event.turnID ?? "event-\(event.seq)"
        switch event.type {
        case "user_message":
            append(ConversationEntry(id: "user-\(event.seq)", kind: .user, title: "Vos", text: event.text ?? ""))
        case "assistant_delta", "assistant_message":
            let key = "assistant-\(event.messageID ?? turn)"
            let index = ensure(ConversationEntry(id: key, kind: .assistant, title: "openADE", text: ""))
            if event.type == "assistant_message" {
                entries[index].text = event.text ?? ""
                entries[index].isRunning = false
            } else {
                entries[index].text += event.text ?? ""
                entries[index].isRunning = true
            }
        case "tool_started", "tool_output", "tool_finished":
            let key = "tool-\(turn)-\(event.toolCallID ?? String(event.seq))"
            let index = ensure(ConversationEntry(id: key, kind: .tool, title: event.toolName ?? "Herramienta", text: ""))
            if let name = event.toolName { entries[index].title = name }
            if let arguments = event.arguments { entries[index].arguments = arguments.displayText }
            if let text = event.text {
                if event.type == "tool_finished" { entries[index].text = text }
                else { entries[index].text += text }
            }
            entries[index].isRunning = event.type != "tool_finished"
            entries[index].isError = event.isError ?? entries[index].isError
        case "turn_failed", "turn_cancelled", "turn_completed":
            for index in entries.indices { entries[index].isRunning = false }
            if event.type != "turn_completed" {
                let fallback = event.type == "turn_cancelled" ? "Turno detenido." : "El turno terminó con un error."
                append(ConversationEntry(id: "notice-\(event.seq)", kind: .notice, title: "Estado", text: event.text ?? fallback, isError: event.type == "turn_failed"))
            }
        default:
            // Permissions come from the authoritative session snapshot. Unknown events
            // still advance the cursor, so a newer host cannot trap the replay loop.
            break
        }
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
            if let arguments = entries[index].arguments, arguments.utf8.count > 64 * 1_024 {
                entries[index].arguments = String(decoding: arguments.utf8.prefix(64 * 1_024), as: UTF8.self) + "\n[Vista recortada]"
            }
        }
        var bytes = entries.reduce(0) { $0 + $1.text.utf8.count + ($1.arguments?.utf8.count ?? 0) }
        while entries.count > 300 || bytes > 2 * 1_024 * 1_024 {
            let index = entries.firstIndex { !$0.isRunning } ?? 0
            let removed = entries.remove(at: index)
            bytes -= removed.text.utf8.count + (removed.arguments?.utf8.count ?? 0)
            didTrimHistory = true
        }
        indices = Dictionary(uniqueKeysWithValues: entries.enumerated().map { ($0.element.id, $0.offset) })
    }
}
