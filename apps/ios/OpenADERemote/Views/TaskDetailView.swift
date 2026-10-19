import SwiftUI

/// One task: its conversation, what it waits on, and the composer. CLI tasks
/// run in a terminal on the PC, so they show their state instead.
struct TaskDetailView: View {
    let taskID: String
    var onClose: (() -> Void)? = nil
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var draft = ""
    @State private var attachments: [UploadedFile] = []
    @State private var requestID = UUID().uuidString
    @State private var sending = false
    @State private var following = true
    @State private var workspace: WorkspaceTab?
    @State private var renaming = false
    @State private var renameText = ""
    @State private var confirmingArchive = false

    private var bottomID: String { "conversation-bottom" }
    private var task: RemoteTask? { store.task(taskID) }
    private var conversation: TaskConversation? { store.conversations[taskID] }
    private var items: [ConversationItem] { conversation?.projection.items ?? [] }
    private var pending: [PendingRequest] { conversation?.pending ?? [] }
    private var isChat: Bool { task?.chat != false }

    var body: some View {
        Group {
            if isChat { conversationView } else { terminalView }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { bottomBar }
        .connectionPill()
        .remoteCanvas()
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .onAppear {
            store.watch(taskID)
            draft = store.drafts[taskID] ?? ""
        }
        .onDisappear {
            store.drafts[taskID] = draft.isEmpty ? nil : draft
            store.unwatch(taskID)
        }
        .task(id: task?.unread) { if task?.unread == true { await store.markRead(taskID) } }
        .onChange(of: draft) { requestID = UUID().uuidString }
        .sheet(item: $workspace) { tab in
            if let task {
                WorkspacePanelView(projectID: task.project, initialTab: tab, onComment: { text in
                    draft += draft.isEmpty ? text : "\n\n" + text
                    workspace = nil
                }).remoteSheet()
            }
        }
        .alert("Renombrar tarea", isPresented: $renaming) {
            TextField("Título", text: $renameText)
            Button("Cancelar", role: .cancel) {}
            Button("Guardar") { Task { await store.rename(taskID, renameText) } }
        }
        .confirmationDialog("¿Archivar esta tarea?", isPresented: $confirmingArchive, titleVisibility: .visible) {
            Button("Archivar", role: .destructive) {
                Task {
                    await store.archive(taskID)
                    if store.task(taskID) == nil { if let onClose { onClose() } else { dismiss() } }
                }
            }
        } message: { Text("Sale de la lista en la PC y en el teléfono. Podés recuperarla desde openADE.") }
    }

    // MARK: Conversation

    private var conversationView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 18) {
                    if conversation?.hasMore == true {
                        Button {
                            Task { await store.loadOlder(taskID) }
                        } label: {
                            HStack(spacing: 8) {
                                if conversation?.loadingOlder == true { ProgressView().controlSize(.small) }
                                Text("Cargar anterior")
                            }
                            .font(.subheadline.weight(.medium))
                            .frame(maxWidth: .infinity, minHeight: 40)
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(RemoteTheme.muted)
                        .disabled(conversation?.loadingOlder == true || !store.isOnline)
                    }
                    if items.isEmpty { emptyConversation }
                    ForEach(items) { item in
                        ConversationItemView(item: item).equatable().id(item.id)
                    }
                    if task?.isRunning == true {
                        WorkingIndicator(since: liveSince, waiting: task?.isWaiting == true)
                            .padding(.top, 2)
                    }
                    Color.clear.frame(height: 1).id(bottomID)
                        .onAppear { following = true }
                }
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 12)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            // Scrolling back to read stops following the end; reaching it again resumes.
            .simultaneousGesture(DragGesture(minimumDistance: 12).onChanged { value in
                if value.translation.height > 12 { following = false }
            })
            .overlay(alignment: .bottom) {
                if !following && !items.isEmpty {
                    Button {
                        following = true
                        withAnimation(.snappy) { proxy.scrollTo(bottomID, anchor: .bottom) }
                    } label: {
                        Label("Ir al final", systemImage: "arrow.down")
                            .font(.footnote.weight(.semibold))
                            .padding(.horizontal, 14).frame(minHeight: 36)
                            .background(.regularMaterial, in: Capsule())
                            .overlay { Capsule().strokeBorder(RemoteTheme.border, lineWidth: 0.5) }
                            .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, 10)
                    .transition(.opacity.combined(with: .scale(scale: 0.9)))
                }
            }
            .onChange(of: conversation?.projection.revision) {
                if following { proxy.scrollTo(bottomID, anchor: .bottom) }
            }
            .onChange(of: pending.first?.id) {
                if following { withAnimation(.snappy) { proxy.scrollTo(bottomID, anchor: .bottom) } }
            }
        }
    }

    private var liveSince: Date? {
        conversation?.projection.liveTurnStartMS.map { Date(timeIntervalSince1970: Double($0) / 1_000) }
    }

    @ViewBuilder private var emptyConversation: some View {
        if conversation?.loaded == true {
            RemoteEmptyState(symbol: "bubble.left.and.text.bubble.right", title: "Sin mensajes todavía",
                             message: "Escribile al agente abajo para empezar.")
                .padding(.top, 60)
        } else if store.connection == .offline || store.connection == .revoked {
            RemoteEmptyState(symbol: "wifi.slash", title: "Sin conexión",
                             message: "La conversación aparece cuando el teléfono vuelva a llegar a \(store.computerName).")
                .padding(.top, 60)
        } else {
            ProgressView("Cargando la conversación…")
                .frame(maxWidth: .infinity).padding(.top, 80)
        }
    }

    private var terminalView: some View {
        ScrollView {
            RemoteEmptyState(symbol: "terminal",
                             title: "Corre en una terminal de la PC",
                             message: "\(task?.agent ?? "Esta CLI") trabaja en una terminal de openADE. Desde acá ves su estado; para escribirle, usá la PC.")
                .padding(.top, 70)
        }
    }

    // MARK: Bottom bar

    @ViewBuilder private var bottomBar: some View {
        VStack(spacing: 10) {
            if isChat {
                PendingCard(taskID: taskID, requests: pending)
                PromptComposer(text: $draft, attachments: $attachments,
                               placeholder: task?.isRunning == true ? "Escribí el próximo mensaje…" : "Pedile algo a \(task?.agent ?? "openADE")",
                               running: task?.isRunning == true, queued: task?.queued ?? 0,
                               sending: sending, stopping: store.isBusy("stop:\(taskID)"),
                               enabled: store.isOnline && task != nil, onSend: send, onStop: stop)
            } else if let task {
                TerminalStatusCard(task: task)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 6)
        .padding(.bottom, 8)
        .background(RemoteTheme.canvas)
        .animation(.snappy, value: pending.first?.id)
    }

    private func send() {
        let prompt = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty, !sending else { return }
        sending = true
        let files = attachments.map(\.id)
        let id = requestID
        Task {
            defer { sending = false }
            do {
                _ = try await store.send(task: taskID, prompt: prompt, files: files, delivery: .auto, requestID: id)
                draft = ""
                attachments = []
                following = true
                Haptics.success()
            } catch {
                if !(error is CancellationError) { store.notice = RemoteStore.readable(error); Haptics.warning() }
            }
        }
    }

    private func stop() {
        Task { if await store.stop(taskID) { Haptics.warning() } }
    }

    // MARK: Header

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            VStack(spacing: 1) {
                Text(task?.displayTitle ?? "Tarea").font(.headline).lineLimit(1)
                Text(subtitle).font(.caption2).foregroundStyle(RemoteTheme.muted).lineLimit(1)
            }
            .accessibilityElement(children: .combine)
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button("Ver cambios", systemImage: "arrow.triangle.branch") { workspace = .changes }
                Button("Archivos del proyecto", systemImage: "folder") { workspace = .files }
                Divider()
                Button("Renombrar", systemImage: "pencil") { renameText = task?.title ?? ""; renaming = true }
                Button(task?.pinned == true ? "Desfijar" : "Fijar", systemImage: task?.pinned == true ? "pin.slash" : "pin") {
                    Task { await store.setPinned(taskID, task?.pinned != true) }
                }
                Button("Archivar", systemImage: "archivebox", role: .destructive) { confirmingArchive = true }
            } label: {
                Image(systemName: "ellipsis.circle").frame(width: 36, height: 36)
            }
            .accessibilityLabel("Opciones de la tarea")
            .disabled(task == nil)
        }
    }

    private var subtitle: String {
        guard let task else { return store.computerName }
        let project = store.project(task.project)?.name
        return [task.agent, project].compactMap { $0 }.joined(separator: " · ")
    }
}

private struct TerminalStatusCard: View {
    let task: RemoteTask
    var body: some View {
        HStack(spacing: 14) {
            Group {
                if task.isWaiting { Image(systemName: "hand.raised.fill").foregroundStyle(RemoteTheme.attention) }
                else if task.isRunning { ProgressView() }
                else { Image(systemName: "terminal") }
            }
            .font(.title3)
            .frame(width: 30)
            VStack(alignment: .leading, spacing: 3) {
                Text(task.isWaiting ? "Te necesita en la PC" : task.isRunning ? "Trabajando en la terminal" : "En espera")
                    .font(.headline)
                Text("Para escribirle a \(task.agent), usá la terminal en openADE.")
                    .font(.footnote).foregroundStyle(RemoteTheme.muted)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 24, style: .continuous).strokeBorder(RemoteTheme.border, lineWidth: 0.5) }
        .accessibilityElement(children: .combine)
    }
}
