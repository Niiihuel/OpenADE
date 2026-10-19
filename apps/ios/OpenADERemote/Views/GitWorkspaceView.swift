import SwiftUI

struct GitWorkspaceView: View {
    let projectID: String
    var onComment: ((String) -> Void)? = nil
    var initialPanel: String? = nil
    @Environment(RemoteStore.self) private var store
    @State private var status: RemoteGitStatus?
    @State private var error: String?
    @State private var loading = false
    @State private var selection: GitListSelection = .changes
    @State private var diffTarget: DiffTarget?
    @State private var commitTarget: CommitTarget?
    @State private var commitMessage = ""
    @State private var mutating = false
    @State private var sourceScope = ""
    @State private var didPresentInitialPanel = false

    private struct DiffTarget: Identifiable {
        let path: String
        let staged: Bool
        var id: String { "\(path):\(staged)" }
    }
    private struct CommitTarget: Identifiable {
        let id = UUID()
        let status: RemoteGitStatus
    }
    private enum GitListSelection { case changes, staged }
    private var staged: [RemoteGitFile] { status?.files.filter(\.staged) ?? [] }
    private var unstaged: [RemoteGitFile] {
        status?.files.filter { $0.untracked || !$0.worktreeStatus.trimmingCharacters(in: .whitespaces).isEmpty } ?? []
    }
    private var visibleFiles: [RemoteGitFile] { selection == .changes ? unstaged : staged }
    private var canMutate: Bool {
        sourceScope == store.connectionID.uuidString && store.isOnline && !loading && !mutating && status != nil
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Label(status?.branch ?? "Git", systemImage: "arrow.triangle.branch")
                    .font(.subheadline.weight(.medium)).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 4)
                if loading || mutating { ProgressView().controlSize(.small) }
                Menu {
                    Button("Actualizar", systemImage: "arrow.clockwise") { Task { await load() } }.disabled(loading)
                    if !visibleFiles.isEmpty {
                        Button(selection == .changes ? "Preparar todos" : "Quitar todos de preparación",
                               systemImage: selection == .changes ? "plus" : "minus") {
                            let files = visibleFiles.map(\.path)
                            let action = selection == .changes ? "stage" : "unstage"
                            Task { await index(action, paths: files) }
                        }.disabled(!canMutate)
                    }
                } label: {
                    Image(systemName: "ellipsis").frame(width: 44, height: 44).contentShape(Circle())
                }.accessibilityLabel("Acciones de Git")
            }.padding(.horizontal, 20).padding(.vertical, 4)

            HStack(spacing: 6) {
                filterChip("Cambios", count: unstaged.count, value: .changes)
                filterChip("Preparados", count: staged.count, value: .staged)
                Spacer(minLength: 0)
            }.padding(.horizontal, 16).padding(.bottom, 10)

            if let error { WorkspaceErrorView(message: error) { Task { await load() } }.padding(.horizontal, 16) }
            if status == nil, loading {
                ProgressView("Leyendo cambios…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if visibleFiles.isEmpty, error == nil {
                ScrollView {
                    RemoteEmptyState(symbol: status?.clean == true ? "checkmark.circle" : "doc.on.doc",
                                     title: status?.clean == true ? "Proyecto sin cambios" : selection == .changes ? "Todo preparado" : "Nada preparado todavía",
                                     message: status?.clean == true ? "Tu proyecto está al día." : selection == .changes ? "Podés revisar lo preparado antes de crear el commit." : "Prepará los archivos que querés incluir en el commit.")
                        .padding(.top, 30)
                }.refreshable { await load() }
            } else {
                List {
                    ForEach(visibleFiles) { file in
                        fileRow(file, isStaged: selection == .staged)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets(top: 3, leading: 20, bottom: 3, trailing: 10))
                            .listRowSeparatorTint(RemoteTheme.border.opacity(0.55))
                    }
                }.listStyle(.plain).scrollContentBackground(.hidden).refreshable { await load() }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !staged.isEmpty {
                Button {
                    if let status { commitTarget = CommitTarget(status: status) }
                } label: {
                    HStack {
                        Text("Crear commit")
                        Spacer()
                        Text("\(staged.count)").font(.subheadline.monospaced()).opacity(0.65)
                        Image(systemName: "arrow.right")
                    }.padding(.horizontal, 18)
                }
                .buttonStyle(RemotePrimaryButtonStyle())
                .disabled(!canMutate)
                .padding(.horizontal, 20).padding(.vertical, 12).background(RemoteTheme.canvas)
            }
        }
        .background(RemoteTheme.canvas).tint(RemoteTheme.ink)
        .task {
            sourceScope = store.connectionID.uuidString
            await load()
            #if DEBUG
            guard !didPresentInitialPanel else { return }
            didPresentInitialPanel = true
            if initialPanel == "commit", let status { commitTarget = CommitTarget(status: status) }
            if initialPanel == "diff", let file = unstaged.first ?? staged.first {
                diffTarget = DiffTarget(path: file.path, staged: unstaged.isEmpty)
            }
            #endif
        }
        .sheet(item: $diffTarget, onDismiss: { Task { await load() } }) {
            GitFileDiffView(projectID: projectID, path: $0.path, staged: $0.staged, onComment: onComment).remoteSheet()
        }
        .sheet(item: $commitTarget, onDismiss: { Task { await load() } }) { target in
            GitCommitView(projectID: projectID, status: target.status, message: $commitMessage) { message in
                await commit(message: message, expectedRevision: target.status.revision)
            }.remoteSheet()
        }
    }

    private func filterChip(_ title: String, count: Int, value: GitListSelection) -> some View {
        Button { selection = value } label: {
            HStack(spacing: 8) {
                Text(title)
                Text("\(count)").font(.caption.monospaced()).foregroundStyle(RemoteTheme.muted)
            }
            .font(.subheadline.weight(selection == value ? .medium : .regular))
            .padding(.horizontal, 14).frame(minHeight: 42)
            .background(selection == value ? RemoteTheme.raised : .clear, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain).accessibilityAddTraits(selection == value ? [.isSelected] : [])
    }

    private func fileRow(_ file: RemoteGitFile, isStaged: Bool) -> some View {
        HStack(spacing: 12) {
            Button { diffTarget = DiffTarget(path: file.path, staged: isStaged) } label: {
                HStack(spacing: 12) {
                    Image(systemName: "doc.text").font(.body).foregroundStyle(RemoteTheme.muted).frame(width: 22)
                    GitFileName(path: file.path)
                    Spacer(minLength: 0)
                    Text(changeBadge(file, isStaged: isStaged))
                        .font(.caption.monospaced().weight(.medium)).foregroundStyle(RemoteTheme.muted)
                        .frame(width: 22, height: 26)
                        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 6))
                }.frame(minHeight: 56).contentShape(Rectangle())
            }.buttonStyle(.plain)
            Button {
                Task { await index(isStaged ? "unstage" : "stage", paths: [file.path]) }
            } label: {
                Image(systemName: isStaged ? "minus" : "plus")
                    .font(.body).foregroundStyle(RemoteTheme.muted).frame(width: 44, height: 44)
            }
            .buttonStyle(.plain).disabled(!canMutate)
            .accessibilityLabel("\(isStaged ? "Quitar de preparación" : "Preparar") \(file.path)")
        }
        .contextMenu {
            Button("Ver cambios", systemImage: "doc.text.magnifyingglass") { diffTarget = DiffTarget(path: file.path, staged: isStaged) }
            Button(isStaged ? "Quitar de preparación" : "Preparar archivo", systemImage: isStaged ? "minus" : "plus") {
                Task { await index(isStaged ? "unstage" : "stage", paths: [file.path]) }
            }.disabled(!canMutate)
        }
    }

    private func changeBadge(_ file: RemoteGitFile, isStaged: Bool) -> String {
        if file.untracked { return "A" }
        let value = (isStaged ? file.indexStatus : file.worktreeStatus).trimmingCharacters(in: .whitespaces)
        return value.isEmpty ? "M" : value
    }

    private func load() async {
        guard !loading, sourceScope == store.connectionID.uuidString else { return }
        let scope = sourceScope
        loading = true
        defer { if scope == store.connectionID.uuidString { loading = false } }
        do {
            let response: RemoteGitStatus = try await store.get(["v2", "projects", projectID, "git", "status"])
            guard scope == store.connectionID.uuidString, !Task.isCancelled else { return }
            status = response; error = nil
        } catch {
            if !(error is CancellationError), scope == store.connectionID.uuidString { self.error = RemoteStore.readable(error) }
        }
    }

    private func index(_ action: String, paths: [String]) async {
        guard canMutate, let status else { return }
        _ = await mutate(action, body: ["expected_revision": .string(status.revision), "paths": .array(paths.map { .string($0) })])
    }

    private func commit(message: String, expectedRevision: String) async -> Bool {
        guard canMutate else { return false }
        let success = await mutate("commit", body: ["expected_revision": .string(expectedRevision), "message": .string(message)])
        if success { commitMessage = "" }
        return success
    }

    private func mutate(_ action: String, body: [String: JSONValue]) async -> Bool {
        let scope = sourceScope
        guard scope == store.connectionID.uuidString else { return false }
        mutating = true
        defer { mutating = false }
        do {
            let updated: RemoteGitStatus = try await store.send("POST", ["v2", "projects", projectID, "git", action], body: body)
            guard scope == store.connectionID.uuidString else { return false }
            status = updated; error = nil; return true
        } catch {
            if !(error is CancellationError), scope == store.connectionID.uuidString { self.error = RemoteStore.readable(error) }
            return false
        }
    }
}

struct GitFileName: View {
    let path: String
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(path.split(separator: "/").last.map(String.init) ?? path).font(.subheadline.weight(.medium)).lineLimit(2)
            let directory = path.split(separator: "/").dropLast().joined(separator: "/")
            if !directory.isEmpty { Text(directory).font(.caption).foregroundStyle(RemoteTheme.muted).lineLimit(1).truncationMode(.middle) }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct GitCommitView: View {
    let projectID: String
    let status: RemoteGitStatus
    @Binding var message: String
    let onCommit: (String) async -> Bool
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var saving = false
    @State private var failed = false
    private var staged: [RemoteGitFile] { status.files.filter(\.staged) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    Label(status.branch ?? "Rama actual", systemImage: "arrow.triangle.branch")
                        .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                    TextField("Describí este cambio", text: $message, axis: .vertical)
                        .lineLimit(3...7).font(.title3).padding(18)
                        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 20))
                        .disabled(saving)
                        .accessibilityLabel("Mensaje del commit")
                    VStack(alignment: .leading, spacing: 16) {
                        Text(staged.count == 1 ? "1 archivo preparado" : "\(staged.count) archivos preparados")
                            .font(.subheadline.weight(.medium))
                        ForEach(staged) { file in
                            HStack(spacing: 12) {
                                Image(systemName: "doc.text").foregroundStyle(RemoteTheme.muted)
                                GitFileName(path: file.path)
                            }
                        }
                    }
                    if failed {
                        Text("No se pudo crear el commit. Tu mensaje sigue disponible.")
                            .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                    }
                }.padding(20)
            }
            .safeAreaInset(edge: .bottom) {
                Button {
                    guard !saving else { return }
                    saving = true; failed = false
                    let text = message
                    Task {
                        let success = await onCommit(text)
                        saving = false
                        if success { dismiss() } else { failed = true }
                    }
                } label: {
                    HStack(spacing: 10) {
                        if saving { ProgressView().tint(RemoteTheme.onAccent) }
                        Text(saving ? "Creando commit…" : "Crear commit")
                    }
                }
                .buttonStyle(RemotePrimaryButtonStyle())
                .disabled(saving || !store.isOnline || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .padding(20).background(RemoteTheme.canvas)
            }
            .navigationTitle("Crear commit").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cerrar") { dismiss() }.disabled(saving) } }
            .interactiveDismissDisabled(saving)
            .remoteCanvas()
        }
    }
}
