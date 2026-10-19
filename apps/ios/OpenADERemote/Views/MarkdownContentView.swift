import SwiftUI
import UniformTypeIdentifiers

struct MarkdownContentView: View {
    let text: String
    @State private var pendingLink: URL?
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
            // An agent's link text can say anything: only web pages open, after the full address is shown.
            .environment(\.openURL, OpenURLAction { url in
                guard ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return .discarded }
                pendingLink = url
                return .handled
            })
            .alert("¿Abrir este enlace?", isPresented: Binding(get: { pendingLink != nil }, set: { if !$0 { pendingLink = nil } }),
                   presenting: pendingLink) { url in
                Button("Abrir") { UIApplication.shared.open(url) }
                Button("Cancelar", role: .cancel) {}
            } message: { url in
                Text(url.absoluteString)
            }
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
                    // Only this device, and only for two minutes: code can carry secrets.
                    UIPasteboard.general.setItems([[UTType.utf8PlainText.identifier: text]],
                                                  options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(120)])
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
