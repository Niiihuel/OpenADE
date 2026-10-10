import Foundation

struct MarkdownBlock: Identifiable, Equatable {
    enum Kind: Equatable { case paragraph, heading(Int), code(String), quote, bullet, divider }
    let id: Int
    let kind: Kind
    let text: String

    static func parse(_ source: String) -> [MarkdownBlock] {
        var result: [MarkdownBlock] = []
        var paragraph: [String] = []
        var code: [String] = []
        var fence: String?
        var language = ""
        func append(_ kind: Kind, _ text: String) { result.append(MarkdownBlock(id: result.count, kind: kind, text: text)) }
        func flush() {
            if !paragraph.isEmpty { append(.paragraph, paragraph.joined(separator: "\n")); paragraph = [] }
        }
        for line in source.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if let delimiter = fence {
                if trimmed.hasPrefix(delimiter) {
                    append(.code(language), code.joined(separator: "\n")); code = []; fence = nil
                } else { code.append(line) }
                continue
            }
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                flush(); fence = String(trimmed.prefix(3)); language = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
            } else if trimmed.isEmpty { flush() }
            else if ["---", "***", "___"].contains(trimmed) { flush(); append(.divider, "") }
            else if trimmed.hasPrefix("> ") { flush(); append(.quote, String(trimmed.dropFirst(2))) }
            else if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") { flush(); append(.bullet, String(trimmed.dropFirst(2))) }
            else {
                let hashes = trimmed.prefix { $0 == "#" }.count
                if (1...6).contains(hashes), trimmed.dropFirst(hashes).hasPrefix(" ") {
                    flush(); append(.heading(hashes), String(trimmed.dropFirst(hashes + 1)))
                } else { paragraph.append(line) }
            }
        }
        if fence != nil { append(.code(language), code.joined(separator: "\n")) }
        flush()
        return result
    }
}
