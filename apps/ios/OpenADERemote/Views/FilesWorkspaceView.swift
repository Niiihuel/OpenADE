import SwiftUI

struct FilesWorkspaceView: View {
    let projectID: String
    @Environment(RemoteStore.self) private var store
    @State private var path = ""
    @State private var query = ""
    @State private var listing: RemoteDirectory?
    @State private var error: String?
    @State private var loading = false
    @State private var selectedFile: FileDestination?
    @State private var showingCreate = false
    @State private var newFileName = ""

    private struct FileDestination: Identifiable {
        let path: String
        let create: Bool
        var id: String { path }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                if !path.isEmpty {
                    RemoteIconButton(title: "Subir carpeta", symbol: "chevron.left") {
                        path = path.split(separator: "/").dropLast().joined(separator: "/"); query = ""
                    }
                }
                Text(path.isEmpty ? "Proyecto" : path).font(.subheadline.weight(.medium)).lineLimit(2).textSelection(.enabled)
                Spacer()
                RemoteIconButton(title: "Actualizar archivos", symbol: "arrow.clockwise") { Task { await load() } }
                    .disabled(loading)
                if store.isOnline {
                    RemoteIconButton(title: "Nuevo archivo", symbol: "doc.badge.plus") { newFileName = ""; showingCreate = true }
                }
            }.padding(.horizontal, 16).padding(.vertical, 8)
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass").foregroundStyle(RemoteTheme.muted).accessibilityHidden(true)
                TextField("Buscar archivos", text: $query)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .accessibilityLabel("Buscar archivos")
                if !query.isEmpty {
                    Button("Borrar búsqueda", systemImage: "xmark.circle.fill") { query = "" }
                        .labelStyle(.iconOnly).frame(width: 44, height: 44)
                        .foregroundStyle(RemoteTheme.muted)
                }
            }
            .padding(.leading, 16).padding(.trailing, query.isEmpty ? 16 : 2)
            .frame(minHeight: 48)
            .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 18))
            .padding(.horizontal, 16).padding(.bottom, 8)
            if let error { WorkspaceErrorView(message: error) { Task { await load() } }.padding(.horizontal, 16).padding(.bottom, 8) }
            if loading && listing == nil { ProgressView("Leyendo archivos…").padding() }
            List {
                if listing?.truncated == true {
                    Label("Resultados limitados. Acotá la búsqueda.", systemImage: "line.3.horizontal.decrease.circle")
                        .font(.caption).foregroundStyle(.secondary)
                }
                ForEach(listing?.entries ?? []) { entry in
                    Button {
                        if entry.isDirectory { path = entry.path; query = "" }
                        else { selectedFile = FileDestination(path: entry.path, create: false) }
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: entry.isDirectory ? "folder" : "doc.text")
                                .font(.title3).foregroundStyle(RemoteTheme.muted).frame(width: 28)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(query.isEmpty ? entry.name : entry.path).foregroundStyle(.primary)
                                if let bytes = entry.sizeBytes, !entry.isDirectory {
                                    Text(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                        }.frame(minHeight: 44)
                    }
                    .listRowBackground(RemoteTheme.canvas)
                }
                if listing?.entries.isEmpty == true && error == nil {
                    RemoteEmptyState(symbol: query.isEmpty ? "folder" : "magnifyingglass", title: query.isEmpty ? "Carpeta vacía" : "Sin resultados",
                                     message: query.isEmpty ? "Los archivos de esta carpeta aparecerán acá." : "Probá con otro nombre o una búsqueda más corta.")
                        .listRowBackground(Color.clear).listRowSeparator(.hidden)
                }
            }.listStyle(.plain).scrollContentBackground(.hidden).refreshable { await load() }
        }
        .background(RemoteTheme.canvas).tint(RemoteTheme.ink)
        .task(id: "\(path)\u{0}\(query)") {
            if !query.isEmpty { try? await Task.sleep(for: .milliseconds(300)) }
            if !Task.isCancelled { await load() }
        }
        .sheet(item: $selectedFile, onDismiss: { Task { await load() } }) {
            FileEditorView(projectID: projectID, path: $0.path, isNew: $0.create).remoteSheet()
        }
        .alert("Nuevo archivo", isPresented: $showingCreate) {
            TextField("Nombre del archivo", text: $newFileName).textInputAutocapitalization(.never).autocorrectionDisabled()
            Button("Cancelar", role: .cancel) {}
            Button("Crear") {
                let name = newFileName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty, !name.contains("/"), name != ".", name != ".." else {
                    error = "Ingresá un nombre de archivo dentro de esta carpeta."; return
                }
                selectedFile = FileDestination(path: path.isEmpty ? name : "\(path)/\(name)", create: true)
            }
        }
    }

    private func load() async {
        let expectedPath = path
        let expectedQuery = query
        loading = true
        defer { if path == expectedPath && query == expectedQuery { loading = false } }
        do {
            let response: RemoteDirectory = try await store.get(["v2", "projects", projectID, "files"], query: [
                URLQueryItem(name: "path", value: expectedPath), URLQueryItem(name: "query", value: expectedQuery)
            ])
            guard path == expectedPath, query == expectedQuery, !Task.isCancelled else { return }
            listing = response; error = nil
        } catch {
            guard !(error is CancellationError), !Task.isCancelled, path == expectedPath, query == expectedQuery else { return }
            self.error = RemoteStore.readable(error)
        }
    }
}

struct FileEditorView: View {
    let projectID: String
    let path: String
    var isNew = false
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var file: RemoteFile?
    @State private var draft = ""
    @State private var editing = false
    @State private var loading = false
    @State private var error: String?
    @State private var confirmingClose = false
    @State private var saving = false
    @State private var wrapSource = true
    private var hasChanges: Bool { editing && (isNew && file == nil || draft != file?.content) }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let error { WorkspaceErrorView(message: error).padding(12) }
                if loading && file == nil && !isNew { ProgressView("Leyendo archivo…").padding() }
                if editing {
                    TextEditor(text: $draft).font(.system(.callout, design: .monospaced))
                        .autocorrectionDisabled().textInputAutocapitalization(.never)
                        .scrollContentBackground(.hidden)
                        .padding(.horizontal, 8).disabled(saving)
                        .accessibilityLabel("Contenido de \(path)")
                } else if let file {
                    SourceCodeView(content: file.content, path: path, wrapLines: wrapSource)
                }
                if editing { Text("Los cambios se guardan en tu PC cuando tocás Guardar.").font(.caption).foregroundStyle(.secondary).padding(8) }
            }
            .remoteCanvas()
            .navigationTitle(path.split(separator: "/").last.map(String.init) ?? path)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cerrar") { if hasChanges { confirmingClose = true } else { dismiss() } }
                }
                ToolbarItemGroup(placement: .confirmationAction) {
                    if editing {
                        ShareLink(item: draft) { Label("Compartir borrador", systemImage: "square.and.arrow.up") }
                        Button("Guardar") { Task { await save() } }
                            .disabled(saving || !store.isOnline || !hasChanges)
                    } else if file != nil, store.isOnline {
                        Button("Editar", systemImage: "pencil") { editing = true }
                    }
                    if !editing, file != nil {
                        Button(wrapSource ? "Desactivar ajuste de líneas" : "Ajustar líneas",
                               systemImage: "arrow.turn.down.left") { wrapSource.toggle() }
                            .tint(wrapSource ? RemoteTheme.ink : RemoteTheme.muted)
                    }
                }
            }
            .task {
                if isNew { editing = true }
                else { await load() }
            }
            .interactiveDismissDisabled(hasChanges)
            .confirmationDialog("¿Cerrar sin guardar el borrador?", isPresented: $confirmingClose, titleVisibility: .visible) {
                Button("Descartar borrador local", role: .destructive) { dismiss() }
            } message: { Text("Podés compartir el borrador antes de cerrar.") }
        }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            let response: RemoteFile = try await store.get(["v2", "projects", projectID, "file"], query: [URLQueryItem(name: "path", value: path)])
            file = response
            draft = response.content
            error = nil
        } catch { if !(error is CancellationError) { self.error = RemoteStore.readable(error) } }
    }

    private func save() async {
        guard draft.utf8.count <= 1024 * 1024 else {
            error = "El editor admite archivos de texto de hasta 1 MB. Tu borrador sigue disponible para compartir."; return
        }
        saving = true
        defer { saving = false }
        do {
            let response: RemoteFile = try await store.send("PUT", ["v2", "projects", projectID, "file"], body: [
                "path": .string(path), "content": .string(draft), "expected_revision": file.map { .string($0.revision) } ?? .null
            ])
            file = response; draft = response.content; error = nil; editing = false
        } catch { if !(error is CancellationError) { self.error = RemoteStore.readable(error) } }
    }
}

private struct SourceCodeView: View {
    let content: String
    let path: String
    let wrapLines: Bool
    @ScaledMetric(relativeTo: .footnote) private var numberWidth: CGFloat = 42
    private var lines: [String] { content.components(separatedBy: "\n") }

    var body: some View {
        if content.isEmpty {
            RemoteEmptyState(symbol: "doc", title: "Archivo vacío", message: "Este archivo todavía no tiene contenido.")
                .frame(maxHeight: .infinity)
        } else if wrapLines {
            ScrollView { code }
        } else {
            ScrollView([.horizontal, .vertical]) { code.fixedSize(horizontal: true, vertical: false) }
        }
    }

    private var code: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                HStack(alignment: .top, spacing: 12) {
                    Text("\(index + 1)").font(.system(.caption, design: .monospaced))
                        .foregroundStyle(RemoteTheme.muted)
                        .frame(width: numberWidth, alignment: .trailing).padding(.vertical, 3)
                        .accessibilityHidden(true)
                    DiffCodeText(text: line.isEmpty ? " " : line, path: path)
                        .font(.system(.footnote, design: .monospaced))
                        .fixedSize(horizontal: !wrapLines, vertical: true)
                        .textSelection(.enabled).padding(.vertical, 3).padding(.trailing, 16)
                        .frame(maxWidth: wrapLines ? .infinity : nil, alignment: .leading)
                        .accessibilityLabel("Línea \(index + 1), \(line)")
                }.frame(maxWidth: wrapLines ? .infinity : nil, alignment: .leading)
            }
        }.padding(.vertical, 12)
    }
}
