import Foundation

/// A permission or a question the task waits on now.
struct PendingRequest: Codable, Identifiable, Hashable, Sendable {
    let requestID: String
    let toolCallID: String
    let toolName: String
    let arguments: JSONValue
    /// The PC accepts only "deny" for arguments too large to show here.
    let argumentsTruncated: Bool
    /// The command prefix "always allow" would allow from now on, shown
    /// verbatim; nil where only "allow in this task" exists.
    var always: String? = nil
    var id: String { requestID }

    enum CodingKeys: String, CodingKey {
        case arguments, always
        case requestID = "request_id"
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case argumentsTruncated = "arguments_truncated"
    }

    var isQuestion: Bool { toolName == "ask_user" }
    var questions: [PendingQuestion] { PendingQuestion.parse(arguments) }
}

struct PendingQuestion: Identifiable, Hashable, Sendable {
    struct Option: Hashable, Sendable {
        let label: String
        let description: String?
    }
    let header: String
    let question: String
    let options: [Option]
    let multiSelect: Bool
    var id: String { header }

    static func parse(_ arguments: JSONValue) -> [PendingQuestion] {
        (arguments["questions"]?.arrayValue ?? []).compactMap { item in
            guard let header = item["header"]?.stringValue, !header.isEmpty else { return nil }
            let options = (item["options"]?.arrayValue ?? []).compactMap { option -> Option? in
                guard let label = option["label"]?.stringValue, !label.isEmpty else { return nil }
                return Option(label: label, description: option["description"]?.stringValue)
            }
            return PendingQuestion(header: header, question: item["question"]?.stringValue ?? "",
                                   options: options, multiSelect: item["multi_select"]?.boolValue ?? false)
        }
    }
}

/// The person's answer to an `ask_user` request, in the shape the desktop
/// records: a question left untouched is reported as skipped, so the model
/// never reads silence as a choice.
enum QuestionAnswer {
    static func json(questions: [PendingQuestion], selected: [String: [String]], other: [String: String]) -> String {
        let answers: [JSONValue] = questions.map { question in
            let labels = question.options.map(\.label).filter { (selected[question.header] ?? []).contains($0) }
            let text = (other[question.header] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if labels.isEmpty && text.isEmpty {
                return .object(["header": .string(question.header), "skipped": .bool(true),
                                "note": .string("The user skipped this question.")])
            }
            var answer: [String: JSONValue] = ["header": .string(question.header), "selected": .array(labels.map(JSONValue.string))]
            if !text.isEmpty { answer["other_answer"] = .string(text) }
            return .object(answer)
        }
        return JSONValue.object(["answered_by": .string("user"), "answers": .array(answers)]).compactText
    }
}

/// What a permission asks for, in a form a person reads at a glance. Nothing
/// it would do is left out: every argument the summary does not read is listed
/// in `extras`, and the card always offers the full arguments.
struct PermissionSummary: Equatable {
    enum Style: Equatable { case command, paths, json }
    let style: Style
    let lines: [String]
    var detail: String? = nil
    /// "key: value" for every other argument, such as `elevated` or `env`.
    var extras: [String] = []

    init(style: Style, lines: [String], detail: String? = nil, extras: [String] = []) {
        self.style = style; self.lines = lines; self.detail = detail; self.extras = extras
    }

    init(_ request: PendingRequest) {
        let arguments = request.arguments
        let name = request.toolName.split(separator: "__").last.map(String.init) ?? request.toolName
        func extras(besides keys: Set<String>) -> [String] {
            guard case .object(let object) = arguments else { return [] }
            return object.keys.filter { !keys.contains($0) }.sorted().map { key in
                "\(key): \(object[key]?.stringValue ?? object[key]?.compactText ?? "")"
            }
        }
        if ToolKind(toolName: name) == .run, let command = ToolKind.command(in: arguments) {
            self.init(style: .command, lines: [command],
                      detail: (arguments["cwd"] ?? arguments["workdir"])?.stringValue.map { "En \($0)" },
                      extras: extras(besides: ToolKind.commandKeys))
        } else if ToolKind(toolName: name) == .edit, !ToolKind.paths(in: arguments).isEmpty {
            // The content of a patch or a file is in the full arguments.
            self.init(style: .paths, lines: ToolKind.paths(in: arguments),
                      extras: extras(besides: ["path", "file_path", "filePath", "paths", "files", "patch", "input", "content"]))
        } else {
            self.init(style: .json, lines: [arguments.displayText])
        }
    }
}
