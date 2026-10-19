import Foundation

/// A permission or a question the task waits on now.
struct PendingRequest: Codable, Identifiable, Hashable, Sendable {
    let requestID: String
    let toolCallID: String
    let toolName: String
    let arguments: JSONValue
    let argumentsTruncated: Bool
    var id: String { requestID }

    enum CodingKeys: String, CodingKey {
        case arguments
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

/// What a permission asks for, in a form a person reads at a glance.
struct PermissionSummary: Equatable {
    enum Style: Equatable { case command, paths, json }
    let style: Style
    let lines: [String]
    var detail: String? = nil

    init(style: Style, lines: [String], detail: String? = nil) {
        self.style = style; self.lines = lines; self.detail = detail
    }

    init(_ request: PendingRequest) {
        let arguments = request.arguments
        let name = request.toolName.split(separator: "__").last.map(String.init) ?? request.toolName
        if ToolKind(toolName: name) == .run, let command = ToolKind.command(in: arguments) {
            self.init(style: .command, lines: [command],
                      detail: (arguments["cwd"] ?? arguments["workdir"])?.stringValue.map { "En \($0)" })
        } else if ToolKind(toolName: name) == .edit {
            let paths = ToolKind.paths(in: arguments)
            if paths.isEmpty { self.init(style: .json, lines: [arguments.displayText]) }
            else { self.init(style: .paths, lines: paths) }
        } else {
            self.init(style: .json, lines: [arguments.displayText])
        }
    }
}
