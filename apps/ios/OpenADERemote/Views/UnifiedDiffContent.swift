import SwiftUI

struct UnifiedDiffContent: View {
    let document: UnifiedDiffDocument
    var wrapLines = true
    var onOpenFile: ((String) -> Void)? = nil
    var onComment: ((String) -> Void)? = nil

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 18) {
                ForEach(document.files) { file in
                    UnifiedDiffFileSection(file: file, wrapLines: wrapLines, onOpenFile: onOpenFile, onComment: onComment)
                }
            }.padding(.vertical, 8)
        }
        .background(RemoteTheme.canvas)
    }
}

private struct UnifiedDiffFileSection: View {
    let file: UnifiedDiffFile
    let wrapLines: Bool
    let onOpenFile: ((String) -> Void)?
    let onComment: ((String) -> Void)?
    @State private var collapsed = false
    @State private var collapsedHunks: Set<Int> = []
    @State private var commentingLine: DiffLine?
    @State private var comment = ""

    var body: some View {
        VStack(spacing: 0) {
            fileHeader
            if !collapsed {
                if file.isBinary {
                    Label("Archivo binario · sin diferencias de texto", systemImage: "doc")
                        .font(.subheadline).foregroundStyle(RemoteTheme.muted).padding(20)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else if file.hunks.isEmpty {
                    Text(file.metadata.filter { !$0.hasPrefix("diff ") && !$0.hasPrefix("index ") }.joined(separator: "\n"))
                        .font(.caption.monospaced()).foregroundStyle(RemoteTheme.muted)
                        .textSelection(.enabled).padding(16).frame(maxWidth: .infinity, alignment: .leading)
                }
                ForEach(file.hunks) { hunk in
                    Button {
                        if !collapsedHunks.insert(hunk.id).inserted { collapsedHunks.remove(hunk.id) }
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: collapsedHunks.contains(hunk.id) ? "chevron.right" : "chevron.down").font(.caption2)
                            Text(hunk.header).font(.caption.monospaced()).lineLimit(1)
                            Spacer(minLength: 0)
                        }
                        .foregroundStyle(RemoteTheme.muted)
                        .padding(.horizontal, 14).frame(minHeight: 44)
                        .background(RemoteTheme.surface.opacity(0.65))
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(collapsedHunks.contains(hunk.id) ? "Expandir" : "Contraer") bloque \(hunk.header)")
                    if !collapsedHunks.contains(hunk.id) {
                        if wrapLines { hunkContent(hunk) }
                        else {
                            ScrollView(.horizontal) {
                                hunkContent(hunk).fixedSize(horizontal: true, vertical: false)
                            }
                        }
                    }
                }
            }
        }
        .overlay(alignment: .bottom) { Rectangle().fill(RemoteTheme.border.opacity(0.5)).frame(height: 0.5) }
        .onAppear {
            #if DEBUG
            if DebugUIFixtures.mode == "git-comment" {
                commentingLine = file.hunks.flatMap(\.lines).first { $0.kind == .added }
                comment = "Agregá una prueba que conserve el cursor al reconectar."
            }
            #endif
        }
    }

    private var fileHeader: some View {
        HStack(spacing: 10) {
            Button { withAnimation(.easeInOut(duration: 0.15)) { collapsed.toggle() } } label: {
                HStack(spacing: 10) {
                    Image(systemName: collapsed ? "chevron.right" : "chevron.down")
                        .font(.caption.weight(.medium)).frame(width: 16)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(file.filename).font(.subheadline.weight(.semibold)).lineLimit(2)
                        if !file.directory.isEmpty {
                            Text(file.directory).font(.caption).foregroundStyle(RemoteTheme.muted)
                                .lineLimit(1).truncationMode(.middle)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }.contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(collapsed ? "Expandir" : "Contraer") \(file.path)")
            DiffChangeCount(added: file.addedCount, removed: file.removedCount)
            if let onOpenFile, let sourcePath = file.newPath, !file.isDeleted {
                Button { onOpenFile(sourcePath) } label: {
                    Image(systemName: "arrow.up.right.square").font(.body)
                        .frame(width: 40, height: 44)
                }
                .buttonStyle(.plain).accessibilityLabel("Abrir archivo actual \(file.path)")
            }
        }
        .padding(.leading, 12).padding(.trailing, 6).padding(.vertical, 10)
        .background(RemoteTheme.surface)
    }

    private func hunkContent(_ hunk: UnifiedDiffHunk) -> some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(hunk.lines) { line in
                UnifiedDiffLineView(line: line, path: file.path, wrapLines: wrapLines,
                                    onComment: onComment == nil || line.locationTitle == nil ? nil : {
                    commentingLine = line; comment = ""
                })
                if commentingLine?.id == line.id {
                    commentEditor(line)
                }
            }
        }
    }

    private func commentEditor(_ line: DiffLine) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Label("Comentario", systemImage: "bubble.left").font(.subheadline.weight(.medium))
                Spacer(minLength: 8)
                Text(line.locationTitle ?? "").font(.caption.monospaced()).foregroundStyle(RemoteTheme.muted)
                Button { commentingLine = nil; comment = "" } label: {
                    Image(systemName: "xmark").font(.caption).frame(width: 32, height: 32)
                }.buttonStyle(.plain).accessibilityLabel("Descartar comentario")
            }
            TextField("¿Qué querés revisar de esta línea?", text: $comment, axis: .vertical)
                .lineLimit(2...6).font(.body)
                .accessibilityLabel("Comentario sobre \(line.locationTitle ?? "la línea")")
            HStack {
                Spacer()
                Button("Agregar al chat", systemImage: "arrow.up.left") {
                    if let context = line.commentContext(path: file.path, comment: comment), let onComment {
                        onComment(context); commentingLine = nil; comment = ""
                    }
                }
                .font(.subheadline.weight(.medium))
                .disabled(comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || comment.utf8.count > 8_000)
            }
        }
        .padding(14)
        .frame(maxWidth: wrapLines ? .infinity : 330, alignment: .leading)
        .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 18))
        .padding(12)
    }
}

private struct UnifiedDiffLineView: View {
    let line: DiffLine
    let path: String
    let wrapLines: Bool
    let onComment: (() -> Void)?
    @ScaledMetric(relativeTo: .footnote) private var gutterWidth: CGFloat = 29

    private var background: Color {
        switch line.kind {
        case .added: .green.opacity(0.10)
        case .removed: .red.opacity(0.09)
        default: .clear
        }
    }
    private var marker: String { line.kind == .added ? "+" : line.kind == .removed ? "−" : " " }

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            Group {
                if let onComment {
                    Button(action: onComment) { gutter }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Comentar \(path), \(line.locationTitle ?? "")")
                } else { gutter }
            }
            .background(background)
            Text(marker).font(.footnote.monospaced())
                .foregroundStyle(line.kind == .added ? Color.green : line.kind == .removed ? Color.red : RemoteTheme.muted)
                .frame(width: 18).padding(.vertical, 3).accessibilityHidden(true)
            DiffCodeText(text: line.content.isEmpty ? " " : line.content, path: path)
                .font(.system(.footnote, design: .monospaced))
                .fixedSize(horizontal: !wrapLines, vertical: true)
                .textSelection(.enabled)
                .padding(.vertical, 3).padding(.trailing, 12)
                .frame(maxWidth: wrapLines ? .infinity : nil, alignment: .leading)
                .accessibilityLabel("\(line.locationTitle ?? ""), \(line.kind == .added ? "agregado" : line.kind == .removed ? "eliminado" : ""), \(line.content)")
        }
        .frame(maxWidth: wrapLines ? .infinity : nil, alignment: .leading)
        .background(background)
        .contextMenu {
            if let onComment { Button("Comentar línea", systemImage: "bubble.left", action: onComment) }
        }
    }

    private var gutter: some View {
        HStack(alignment: .top, spacing: 0) {
            Text(line.oldLineNumber.map(String.init) ?? " ").frame(width: gutterWidth, alignment: .trailing)
            Text(line.newLineNumber.map(String.init) ?? " ").frame(width: gutterWidth, alignment: .trailing)
        }
        .font(.system(.caption, design: .monospaced)).foregroundStyle(RemoteTheme.muted)
        .padding(.trailing, 6).padding(.vertical, 4)
        .contentShape(Rectangle())
    }
}

struct DiffChangeCount: View {
    let added: Int
    let removed: Int
    var body: some View {
        HStack(spacing: 6) {
            Text("+\(added)").foregroundStyle(.green)
            Text("−\(removed)").foregroundStyle(.red)
        }
        .font(.caption.monospaced().weight(.medium))
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(added) líneas agregadas, \(removed) eliminadas")
    }
}

/// Lightweight per-line coloring for common source files; the patch and its
/// line numbers stay authoritative, and text remains selectable.
struct DiffCodeText: View {
    let text: String
    let path: String
    private static let expression = try? NSRegularExpression(
        pattern: #"(//[^\n]*|#[^\n]*|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\b(?:import|from|public|private|internal|struct|class|enum|func|let|var|const|function|return|if|else|guard|case|switch|for|while|try|catch|throw|throws|async|await|static|final|true|false|nil|null|self|export|default|type|interface|fn|pub|use|impl|match|def|in)\b)"#
    )
    private var colored: AttributedString {
        var value = AttributedString(text)
        guard text.utf8.count <= 16_384 else { return value }
        let suffix = path.split(separator: ".").last?.lowercased() ?? ""
        guard ["swift", "rs", "ts", "tsx", "js", "jsx", "py", "java", "kt", "c", "h", "cpp", "go"].contains(suffix),
              let expression = Self.expression else { return value }
        for match in expression.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
            guard let range = Range(match.range, in: text),
                  let lower = AttributedString.Index(range.lowerBound, within: value),
                  let upper = AttributedString.Index(range.upperBound, within: value) else { continue }
            let token = text[range]
            value[lower..<upper].foregroundColor = token.hasPrefix("//") || token.hasPrefix("#") ? RemoteTheme.muted
                : token.hasPrefix("\"") || token.hasPrefix("'") ? Color.orange.opacity(0.85) : Color.blue
        }
        return value
    }
    var body: some View { Text(colored) }
}
