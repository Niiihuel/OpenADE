import SwiftUI

struct GitFileDiffView: View {
    let projectID: String
    let path: String
    let staged: Bool
    var onComment: ((String) -> Void)? = nil
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var diff: RemoteFileDiff?
    @State private var document = UnifiedDiffDocument()
    @State private var error: String?
    @State private var loading = false
    @State private var wrapLines = true
    @State private var sourceFile: DiffSourceFile?
    @State private var sourceScope = ""

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if diff?.truncated == true { DiffTruncationNotice() }
                if let error {
                    WorkspaceErrorView(message: error) { Task { await load() } }.padding(16)
                    Spacer()
                } else if diff == nil {
                    ProgressView("Cargando cambios…").frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if document.files.isEmpty {
                    RemoteEmptyState(symbol: "doc.text", title: "Sin diferencias de texto",
                                     message: "Este archivo no tiene diferencias de texto disponibles.")
                        .frame(maxHeight: .infinity)
                } else {
                    UnifiedDiffContent(document: document, wrapLines: wrapLines,
                                       onOpenFile: { sourceFile = DiffSourceFile(path: $0) },
                                       onComment: onComment)
                }
            }
            .remoteCanvas()
            .navigationTitle(staged ? "Cambios preparados" : "Cambios del archivo")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cerrar") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { wrapLines.toggle() } label: {
                        Image(systemName: wrapLines ? "arrow.turn.down.left" : "arrow.left.and.right")
                            .frame(width: 36, height: 36)
                    }
                    .accessibilityLabel(wrapLines ? "Mostrar líneas sin ajuste" : "Ajustar líneas a la pantalla")
                    .accessibilityValue(wrapLines ? "Ajuste activado" : "Ajuste desactivado")
                }
            }
            .task { sourceScope = store.connectionID.uuidString; await load() }
            .sheet(item: $sourceFile, onDismiss: { Task { await load() } }) {
                FileEditorView(projectID: projectID, path: $0.path).remoteSheet()
            }
        }
    }

    private func load() async {
        guard !loading, sourceScope == store.connectionID.uuidString else { return }
        let scope = sourceScope
        loading = true
        defer { if scope == store.connectionID.uuidString { loading = false } }
        do {
            let response: RemoteFileDiff = try await store.get(["v2", "projects", projectID, "git", "diff"], query: [
                URLQueryItem(name: "path", value: path), URLQueryItem(name: "staged", value: staged ? "true" : "false")
            ])
            guard scope == store.connectionID.uuidString, !Task.isCancelled else { return }
            diff = response; document = UnifiedDiffDocument.parse(response.diff, fallbackPath: path); error = nil
        } catch {
            if !(error is CancellationError), scope == store.connectionID.uuidString { self.error = RemoteStore.readable(error) }
        }
    }
}

private struct DiffSourceFile: Identifiable {
    let path: String
    var id: String { path }
}

private struct DiffTruncationNotice: View {
    var body: some View {
        Label("Vista parcial · diff recortado por su tamaño", systemImage: "exclamationmark.circle")
            .font(.caption).foregroundStyle(RemoteTheme.muted)
            .padding(.horizontal, 20).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RemoteTheme.surface)
    }
}
