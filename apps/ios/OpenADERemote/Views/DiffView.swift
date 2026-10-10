import SwiftUI

struct DiffView: View {
    let projectID: String
    var onComment: ((String) -> Void)? = nil
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var diff: ProjectDiff?
    @State private var document = UnifiedDiffDocument()
    @State private var error: String?
    @State private var loading = false
    @State private var wrapLines = true
    @State private var sourceFile: DiffSourceFile?
    @State private var sourceScope = ""

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if diff != nil {
                    HStack {
                        Text("\(document.files.count) archivos").font(.caption).foregroundStyle(RemoteTheme.muted)
                        Spacer()
                        DiffChangeCount(added: document.addedCount, removed: document.removedCount)
                    }.padding(.horizontal, 20).padding(.vertical, 12)
                }
                if diff?.includesUntracked != true, diff != nil {
                    Label("Solo archivos rastreados", systemImage: "info.circle")
                        .font(.caption).foregroundStyle(RemoteTheme.muted)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 20).padding(.bottom, 10)
                }
                if diff?.truncated == true { DiffTruncationNotice() }
                if loading && diff == nil {
                    ProgressView("Cargando cambios…").frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let error {
                    WorkspaceErrorView(message: error) { Task { await load() } }.padding(16)
                    Spacer()
                } else if document.files.isEmpty {
                    RemoteEmptyState(symbol: "doc.text.magnifyingglass", title: "Sin diferencias disponibles",
                                     message: "No hay cambios en los archivos incluidos en esta vista.")
                        .frame(maxHeight: .infinity)
                } else {
                    UnifiedDiffContent(document: document, wrapLines: wrapLines,
                                       onOpenFile: store.supports("workspace.files.read") ? { sourceFile = DiffSourceFile(path: $0) } : nil,
                                       onComment: onComment)
                }
            }
            .remoteCanvas()
            .navigationTitle("Revisión de cambios").navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cerrar") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Toggle("Ajustar líneas", isOn: $wrapLines)
                        Button("Actualizar", systemImage: "arrow.clockwise") { Task { await load() } }.disabled(loading)
                    } label: { Image(systemName: "line.3.horizontal.decrease").frame(width: 36, height: 36) }
                        .accessibilityLabel("Opciones del diff")
                }
            }
            .task { sourceScope = store.connectionIdentity; await load() }
            .sheet(item: $sourceFile) { FileEditorView(projectID: projectID, path: $0.path).remoteSheet() }
        }
    }

    private func load() async {
        guard !loading, sourceScope == store.connectionIdentity else { return }
        let scope = sourceScope
        loading = true
        defer { if scope == store.connectionIdentity { loading = false } }
        do {
            let response = try await store.loadDiff(projectID: projectID)
            guard scope == store.connectionIdentity, !Task.isCancelled else { return }
            diff = response
            document = UnifiedDiffDocument.parse(response.diff)
            error = nil
        } catch {
            if !(error is CancellationError), !Task.isCancelled, scope == store.connectionIdentity { self.error = error.localizedDescription }
        }
    }
}

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
                                       onOpenFile: store.supports("workspace.files.read") ? { sourceFile = DiffSourceFile(path: $0) } : nil,
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
            .task { sourceScope = store.connectionIdentity; await load() }
            .sheet(item: $sourceFile, onDismiss: { Task { await load() } }) {
                FileEditorView(projectID: projectID, path: $0.path).remoteSheet()
            }
        }
    }

    private func load() async {
        guard !loading, sourceScope == store.connectionIdentity else { return }
        let scope = sourceScope
        loading = true
        defer { if scope == store.connectionIdentity { loading = false } }
        do {
            let response: RemoteFileDiff = try await store.load(["v1", "projects", projectID, "git", "diff"], query: [
                URLQueryItem(name: "path", value: path), URLQueryItem(name: "staged", value: staged ? "true" : "false")
            ])
            guard scope == store.connectionIdentity, !Task.isCancelled else { return }
            diff = response; document = UnifiedDiffDocument.parse(response.diff, fallbackPath: path); error = nil
        } catch {
            if !(error is CancellationError), scope == store.connectionIdentity { self.error = error.localizedDescription }
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
