import Foundation

struct DiffLine: Identifiable, Equatable {
    enum Kind { case added, removed, header, context }
    let id: Int
    let text: String
    let kind: Kind
    var oldLineNumber: Int? = nil
    var newLineNumber: Int? = nil

    var content: String {
        if kind == .added || kind == .removed || (kind == .context && text.hasPrefix(" ")) { return String(text.dropFirst()) }
        return text
    }
    var locationTitle: String? {
        if kind == .removed, let oldLineNumber { return "Anterior · L\(oldLineNumber)" }
        if let newLineNumber { return "Nueva · L\(newLineNumber)" }
        return nil
    }
    func commentContext(path: String, comment: String) -> String? {
        guard let locationTitle, !comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        let version = kind == .removed ? "versión anterior, línea eliminada" : "versión nueva"
        return "Comentario sobre \(path) · \(locationTitle) (\(version)):\n> \(String(content.prefix(1_500)))\n\n\(comment)"
    }

    static func parse(_ patch: String) -> [DiffLine] {
        var old: Int?
        var new: Int?
        var inHunk = false
        var source = patch.components(separatedBy: "\n")
        if source.last == "" { source.removeLast() }
        return source.enumerated().map { index, raw in
            let text = raw.hasSuffix("\r") ? String(raw.dropLast()) : raw
            if text.hasPrefix("diff ") {
                inHunk = false; old = nil; new = nil
                return DiffLine(id: index, text: text, kind: .header)
            }
            if text.hasPrefix("@@") {
                let range = DiffHunkRange.parse(text)
                old = range?.oldStart; new = range?.newStart; inHunk = range != nil
                return DiffLine(id: index, text: text, kind: .header)
            }
            guard inHunk else { return DiffLine(id: index, text: text, kind: .header) }
            if text.hasPrefix("+") {
                let line = DiffLine(id: index, text: text, kind: .added, newLineNumber: new)
                new = increment(new); return line
            }
            if text.hasPrefix("-") {
                let line = DiffLine(id: index, text: text, kind: .removed, oldLineNumber: old)
                old = increment(old); return line
            }
            if text.hasPrefix(" ") {
                let line = DiffLine(id: index, text: text, kind: .context, oldLineNumber: old, newLineNumber: new)
                old = increment(old); new = increment(new); return line
            }
            // Metadata such as "\ No newline at end of file" consumes no line.
            return DiffLine(id: index, text: text, kind: .header)
        }
    }

    private static func increment(_ value: Int?) -> Int? {
        guard let value else { return nil }
        let next = value.addingReportingOverflow(1)
        return next.overflow ? nil : next.partialValue
    }
}

struct DiffHunkRange: Equatable {
    let oldStart: Int
    let newStart: Int
    static func parse(_ header: String) -> Self? {
        guard let expression = try? NSRegularExpression(pattern: #"^@@ -([0-9]+)(?:,[0-9]+)? \+([0-9]+)(?:,[0-9]+)? @@"#),
              let match = expression.firstMatch(in: header, range: NSRange(header.startIndex..., in: header)),
              let oldRange = Range(match.range(at: 1), in: header),
              let newRange = Range(match.range(at: 2), in: header),
              let old = Int(header[oldRange]), let new = Int(header[newRange]) else { return nil }
        return Self(oldStart: old, newStart: new)
    }
}

struct UnifiedDiffHunk: Identifiable, Equatable {
    let id: Int
    let header: String
    var lines: [DiffLine] = []
}

struct UnifiedDiffFile: Identifiable, Equatable {
    let id: Int
    var path: String
    var oldPath: String?
    var newPath: String?
    var metadata: [String] = []
    var hunks: [UnifiedDiffHunk] = []
    var isBinary = false
    var isDeleted = false
    var addedCount: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .added }.count } }
    var removedCount: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .removed }.count } }
    var filename: String { path.split(separator: "/").last.map(String.init) ?? path }
    var directory: String { path.split(separator: "/").dropLast().joined(separator: "/") }
}

struct UnifiedDiffDocument: Equatable {
    var files: [UnifiedDiffFile] = []
    var addedCount: Int { files.reduce(0) { $0 + $1.addedCount } }
    var removedCount: Int { files.reduce(0) { $0 + $1.removedCount } }

    static func parse(_ patch: String, fallbackPath: String? = nil) -> Self {
        guard !patch.isEmpty else { return Self() }
        var files: [UnifiedDiffFile] = []
        var current: UnifiedDiffFile?
        for line in DiffLine.parse(patch) {
            if line.text.hasPrefix("diff --git ") {
                if let current { files.append(current) }
                let paths = headerPaths(String(line.text.dropFirst(11)))
                current = UnifiedDiffFile(id: line.id, path: paths?.new ?? fallbackPath ?? "Archivo \(files.count + 1)",
                                          oldPath: paths?.old, newPath: paths?.new ?? fallbackPath)
            }
            if current == nil { current = UnifiedDiffFile(id: line.id, path: fallbackPath ?? "Cambios", newPath: fallbackPath) }
            if line.text.hasPrefix("@@") {
                current?.hunks.append(UnifiedDiffHunk(id: line.id, header: line.text))
            } else if current?.hunks.isEmpty == false {
                let index = current!.hunks.count - 1
                current?.hunks[index].lines.append(line)
            } else {
                current?.metadata.append(line.text)
                if line.text.hasPrefix("--- ") {
                    current?.oldPath = decodePath(String(line.text.dropFirst(4)))
                    if let old = current?.oldPath { current?.path = old }
                } else if line.text.hasPrefix("+++ ") {
                    let path = decodePath(String(line.text.dropFirst(4)))
                    current?.newPath = path
                    current?.isDeleted = path == nil
                    if let path { current?.path = path }
                } else if line.text.hasPrefix("rename to ") {
                    let path = decodePath(String(line.text.dropFirst(10)), stripPrefix: false)
                    current?.newPath = path
                    if let path { current?.path = path }
                } else if line.text.hasPrefix("Binary files ") || line.text == "GIT binary patch" {
                    current?.isBinary = true
                }
            }
        }
        if let current { files.append(current) }
        return Self(files: files)
    }

    private static func headerPaths(_ value: String) -> (old: String, new: String)? {
        var tokens: [String] = []
        var token = ""
        var quoted = false
        var escaped = false
        for character in value {
            if character == " ", !quoted {
                if !token.isEmpty { tokens.append(token); token = "" }
                continue
            }
            token.append(character)
            if escaped { escaped = false }
            else if character == "\\", quoted { escaped = true }
            else if character == "\"" { quoted.toggle() }
        }
        if !token.isEmpty { tokens.append(token) }
        if tokens.count == 2, let old = decodePath(tokens[0]), let new = decodePath(tokens[1]) {
            return (old, new)
        }
        // Git leaves ordinary spaces unquoted. Equal before/after paths make
        // the boundary unambiguous; renamed files also carry "rename to".
        guard value.hasPrefix("a/"), !value.hasPrefix("\"") else { return nil }
        var search = value.startIndex..<value.endIndex
        while let separator = value.range(of: " b/", range: search) {
            let old = String(value[value.index(value.startIndex, offsetBy: 2)..<separator.lowerBound])
            let new = String(value[separator.upperBound...])
            if old == new { return (old, new) }
            search = separator.upperBound..<value.endIndex
        }
        return nil
    }

    /// Git's quoted paths use C escapes and UTF-8 octal bytes, not JSON escapes.
    private static func decodePath(_ value: String, stripPrefix: Bool = true) -> String? {
        let path: String
        if value.hasPrefix("\"") {
            let bytes = Array(value.utf8)
            var decoded: [UInt8] = []
            var index = 1
            while index < bytes.count {
                let byte = bytes[index]
                if byte == 34 { break }
                if byte == 92, index + 1 < bytes.count {
                    index += 1
                    let escaped = bytes[index]
                    if (48...55).contains(escaped) {
                        var octal = Int(escaped - 48)
                        var count = 1
                        while count < 3, index + 1 < bytes.count, (48...55).contains(bytes[index + 1]) {
                            index += 1; count += 1; octal = octal * 8 + Int(bytes[index] - 48)
                        }
                        guard octal <= 255 else { return nil }
                        decoded.append(UInt8(octal))
                    } else {
                        let escapes: [UInt8: UInt8] = [97: 7, 98: 8, 116: 9, 110: 10, 118: 11, 102: 12, 114: 13, 34: 34, 92: 92]
                        decoded.append(escapes[escaped] ?? escaped)
                    }
                } else { decoded.append(byte) }
                index += 1
            }
            guard let decodedPath = String(bytes: decoded, encoding: .utf8) else { return nil }
            path = decodedPath
        } else { path = String(value.split(separator: "\t", maxSplits: 1, omittingEmptySubsequences: false)[0]) }
        guard path != "/dev/null", !path.isEmpty else { return nil }
        return stripPrefix && (path.hasPrefix("a/") || path.hasPrefix("b/")) ? String(path.dropFirst(2)) : path
    }
}
