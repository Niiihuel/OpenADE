import SwiftUI

struct ConversationEntryView: View {
    let entry: ConversationEntry
    @State private var showingOutput = false

    var body: some View {
        Group {
            if entry.kind == .tool { toolCard }
            else {
                VStack(alignment: .leading, spacing: 8) {
                    Label(entry.title, systemImage: icon)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(entry.isError ? Color.red : RemoteTheme.muted)
                    if entry.kind == .assistant {
                        MarkdownContentView(text: entry.text.isEmpty && entry.isRunning ? "…" : entry.text)
                    } else {
                        Text(entry.text).font(.body).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if entry.textTruncated {
                        Text("Vista recortada. El contenido completo permanece en la PC.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                .padding(entry.kind == .user ? 18 : 0)
                .background(entry.kind == .user ? RemoteTheme.raised : .clear,
                            in: RoundedRectangle(cornerRadius: 22))
            }
        }
        .sheet(isPresented: $showingOutput) {
            ReadOnlyOutputView(title: entry.title, output: entry.text, arguments: entry.arguments, truncated: entry.textTruncated).remoteSheet()
        }
    }

    private var icon: String {
        switch entry.kind {
        case .user: "person.crop.circle"
        case .assistant: "sparkles"
        case .tool: "gearshape.2"
        case .notice: "info.circle"
        }
    }

    private var toolCard: some View {
        DisclosureGroup {
            VStack(alignment: .leading, spacing: 10) {
                if let arguments = entry.arguments, !arguments.isEmpty {
                    Text("Acción").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    codeText(String(arguments.prefix(4_000)))
                }
                if !entry.text.isEmpty {
                    Text("Salida · solo lectura").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    codeText(String(entry.text.suffix(8_000)))
                } else {
                    Text(entry.isRunning ? "Esperando salida…" : "Sin salida de texto.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Button("Abrir salida disponible", systemImage: "doc.text.magnifyingglass") { showingOutput = true }
                    .font(.subheadline).frame(minHeight: 44)
            }
            .padding(.top, 10)
        } label: {
            HStack(spacing: 8) {
                if entry.isRunning { ProgressView().controlSize(.small) }
                else { Image(systemName: entry.isError ? "exclamationmark.circle" : "checkmark.circle") }
                Text(entry.title).font(.subheadline.weight(.semibold)).lineLimit(2)
            }
            .foregroundStyle(entry.isError ? Color.red : Color.primary)
            .frame(minHeight: 44)
            .accessibilityLabel("\(entry.title), \(entry.isRunning ? "en curso" : entry.isError ? "con error" : "finalizada")")
        }
        .padding(.horizontal, 16).padding(.vertical, 6)
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 20))
        .overlay { RoundedRectangle(cornerRadius: 20).strokeBorder(RemoteTheme.border, lineWidth: 0.5) }
        .tint(RemoteTheme.muted)
    }

    private func codeText(_ text: String) -> some View {
        ScrollView(.horizontal) {
            Text(text).font(.system(.caption, design: .monospaced))
                .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

struct ReadOnlyOutputView: View {
    let title: String
    let output: String
    let arguments: String?
    let truncated: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView([.horizontal, .vertical]) {
                VStack(alignment: .leading, spacing: 16) {
                    if truncated {
                        Text("Salida reciente. El contenido completo permanece en la PC.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if let arguments, !arguments.isEmpty {
                        Text("Acción").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        Text(arguments).textSelection(.enabled)
                    }
                    Text("Salida · solo lectura").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Text(output.isEmpty ? "Sin salida de texto." : output).textSelection(.enabled)
                }
                .font(.system(.callout, design: .monospaced))
                .padding()
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .remoteCanvas()
        }
    }
}
