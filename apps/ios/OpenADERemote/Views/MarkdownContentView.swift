import SwiftUI

struct MarkdownContentView: View {
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(MarkdownBlock.parse(text)) { block in
                switch block.kind {
                case .code(let language): CodeBlockView(text: block.text, language: language)
                case .heading(let level):
                    inline(block.text).font(level < 3 ? .title3.weight(.semibold) : .headline).accessibilityAddTraits(.isHeader)
                case .quote:
                    HStack(alignment: .top, spacing: 12) { RoundedRectangle(cornerRadius: 2).fill(RemoteTheme.border).frame(width: 3); inline(block.text).foregroundStyle(RemoteTheme.muted) }
                        .fixedSize(horizontal: false, vertical: true)
                case .bullet:
                    HStack(alignment: .firstTextBaseline, spacing: 8) { Text("•"); inline(block.text) }
                case .divider: Divider()
                case .paragraph: inline(block.text)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
            .tint(RemoteTheme.ink)
    }

    private func inline(_ source: String) -> Text {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return Text((try? AttributedString(markdown: source, options: options)) ?? AttributedString(source))
    }
}

struct CodeBlockView: View {
    let text: String
    var language = ""
    @State private var copied = false
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "Código" : language).font(.caption.weight(.medium)).foregroundStyle(.secondary)
                Spacer()
                Button(copied ? "Copiado" : "Copiar", systemImage: copied ? "checkmark" : "doc.on.doc") {
                    UIPasteboard.general.string = text
                    copied = true
                }.font(.caption).frame(minWidth: 44, minHeight: 44).accessibilityLabel("Copiar bloque de código")
            }.padding(.horizontal, 14).padding(.vertical, 2)
            Divider()
            ScrollView(.horizontal) {
                Text(text).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(12)
            }
        }
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(RemoteTheme.border.opacity(0.65), lineWidth: 0.5) }
        .tint(RemoteTheme.ink)
    }
}
