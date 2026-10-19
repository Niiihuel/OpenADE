import Foundation

/// What a tool call did, counted the way the desktop's activity summary
/// counts it ("leyó 4 archivos, editó 2"). ACP agents report the same names.
enum ToolKind: Int, CaseIterable, Sendable {
    case read, search, run, edit, browse, delegate, other
    /// Following up a command or a subagent already counted is not another action.
    case followUp

    init(toolName: String) {
        let name = (toolName.split(separator: "__").last.map(String.init) ?? toolName).lowercased()
        switch name {
        case "read_file", "batch_read", "get_diagnostics", "list_files", "find_files", "read", "view", "glob", "ls", "list":
            self = .read
        case "search_files", "search_text", "codebase_search", "codebase_explore", "web_search", "grep", "search", "websearch":
            self = .search
        case "run_command", "exec_command", "start_command", "bash", "shell", "execute", "terminal":
            self = .run
        case "write_file", "edit_file", "apply_patch", "delete_file", "rename_file", "edit", "write", "multiedit", "patch", "delete", "move":
            self = .edit
        case "web_fetch", "open_preview", "webfetch", "fetch":
            self = .browse
        case "spawn_agent", "delegate_task", "task":
            self = .delegate
        case "poll_command", "write_stdin", "terminal_send", "terminate_command", "resize_command", "wait_agent",
             "await_subagent", "send_input", "send_message", "message_conversation", "cancel_agent", "cancel_subagent",
             "list_agents", "update_plan", "todowrite", "todoread", "ask_user", "switch_mode":
            // A question or a mode proposal is the card above the composer, not work.
            self = .followUp
        default:
            self = name.hasPrefix("browser_") ? .browse : .other
        }
    }

    var symbol: String {
        switch self {
        case .read: "doc.text"
        case .search: "magnifyingglass"
        case .run: "terminal"
        case .edit: "pencil"
        case .browse: "globe"
        case .delegate: "person.2"
        case .other, .followUp: "wrench.and.screwdriver"
        }
    }

    /// "Leyó 4 archivos · Editó 2 archivos · Ejecutó 1 comando".
    static func summary(_ calls: [ToolCallItem]) -> String {
        var counts: [ToolKind: Int] = [:]
        var edited = Set<String>()
        for call in calls where call.kind != .followUp {
            counts[call.kind, default: 0] += 1
            if call.kind == .edit {
                // The same file edited twice is one file.
                let paths = paths(in: call.arguments)
                if paths.isEmpty { edited.insert("#\(counts[.edit] ?? 0)") } else { edited.formUnion(paths) }
            }
        }
        if counts[.edit] != nil { counts[.edit] = edited.count }
        let phrases: [(ToolKind, String, String)] = [
            (.read, "Leyó 1 archivo", "Leyó %d archivos"),
            (.search, "Buscó 1 vez", "Buscó %d veces"),
            (.run, "Ejecutó 1 comando", "Ejecutó %d comandos"),
            (.edit, "Editó 1 archivo", "Editó %d archivos"),
            (.browse, "Usó el navegador", "Usó el navegador %d veces"),
            (.delegate, "Delegó en 1 subagente", "Delegó en %d subagentes"),
            (.other, "Usó 1 herramienta", "Usó %d herramientas"),
        ]
        let parts = phrases.compactMap { entry -> String? in
            let (kind, one, many) = entry
            guard let count = counts[kind], count > 0 else { return nil }
            return count == 1 ? one : many.replacingOccurrences(of: "%d", with: String(count))
        }
        return parts.isEmpty ? (calls.first.map { label(for: $0) } ?? "Usó herramientas") : parts.joined(separator: " · ")
    }

    static func command(in arguments: JSONValue?) -> String? {
        guard let arguments else { return nil }
        for key in ["command", "cmd", "commandLine"] {
            if let text = arguments[key]?.stringValue, !text.isEmpty { return text }
            if let parts = arguments[key]?.arrayValue?.compactMap(\.stringValue), !parts.isEmpty {
                return parts.joined(separator: " ")
            }
        }
        return nil
    }

    static func paths(in arguments: JSONValue?) -> [String] {
        guard let arguments else { return [] }
        var found: [String] = []
        for key in ["path", "file_path", "filePath", "target", "from", "to", "new_path"] {
            if let path = arguments[key]?.stringValue, !path.isEmpty, !found.contains(path) { found.append(path) }
        }
        for key in ["paths", "files"] {
            for path in arguments[key]?.arrayValue?.compactMap(\.stringValue) ?? [] where !found.contains(path) {
                found.append(path)
            }
        }
        // apply_patch names its files inside the patch text.
        for key in ["patch", "input"] {
            for line in (arguments[key]?.stringValue ?? "").split(separator: "\n") {
                for marker in ["*** Update File: ", "*** Add File: ", "*** Delete File: ", "+++ b/"] where line.hasPrefix(marker) {
                    let path = String(line.dropFirst(marker.count)).trimmingCharacters(in: .whitespaces)
                    if !path.isEmpty, !found.contains(path) { found.append(path) }
                }
            }
        }
        return found
    }

    /// One call on one line: "Leyó App.swift", "swift test", "Buscó “cursor”".
    static func label(for call: ToolCallItem) -> String {
        let file = paths(in: call.arguments).first.map { $0.split(separator: "/").last.map(String.init) ?? $0 }
        switch call.kind {
        case .read: return file.map { "Leyó \($0)" } ?? call.name
        case .edit: return file.map { "Editó \($0)" } ?? call.name
        case .run: return command(in: call.arguments) ?? call.name
        case .search:
            let query = ["query", "pattern", "q", "text"].lazy.compactMap { call.arguments?[$0]?.stringValue }.first
            return query.map { "Buscó “\($0)”" } ?? call.name
        case .browse:
            let url = call.arguments?["url"]?.stringValue.flatMap { URL(string: $0)?.host }
            return url.map { "Abrió \($0)" } ?? call.name
        case .delegate: return "Delegó una tarea"
        case .other, .followUp:
            switch call.name {
            case "ask_user": return "Te hizo una pregunta"
            case "switch_mode": return "Propuso cambiar de modo"
            default: return call.name
            }
        }
    }
}
