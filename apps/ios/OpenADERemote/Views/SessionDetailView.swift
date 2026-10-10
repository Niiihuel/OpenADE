import SwiftUI

@MainActor
struct SessionDetailView: View {
    let sessionID: String
    var initialPanel: String? = nil
    @Environment(RemoteStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase
    @State private var prompt = ""
    @State private var mode: TurnMode = .build
    @State private var showingWorkspace = false
    @State private var composerPanel: ComposerPanel?
    @State private var showingAdvancedModels = false
    @State private var showingQueue = false
    @State private var showingPermissions = false
    @State private var showingAttachments = false
    @State private var options = RemoteTurnOptions()
    @State private var dictation = ComposerDictation()
    @State private var dictationBase = ""
    @State private var dictationTask: Task<Void, Never>?
    @State private var didPresentInitialPanel = false
    @State private var draftScope = ""
    @State private var confirmingStop = false
    @State private var turnToCancel: String?
    @State private var followingLatest = true
    @State private var projectChangeCount: Int?
    @FocusState private var composerFocused: Bool

    private var snapshot: RemoteSession? { store.session(id: sessionID) }
    private var entries: [ConversationEntry] { store.entries(sessionID: sessionID) }
    private var pending: PendingAction? { store.pendingTurn(sessionID: sessionID) }
    private var hasAttachments: Bool { !(store.attachments[sessionID] ?? []).isEmpty }
    private var selectedModel: RemoteModel? {
        let id = options.modelID ?? store.modelCatalog?.defaultModelID ?? store.health?.model
        return store.modelCatalog?.models.first { $0.id == id }
    }
    private var hasUnsupportedImages: Bool {
        selectedModel?.supportsImages == false && (store.attachments[sessionID] ?? []).contains(where: \.isImage)
    }
    private var sending: Bool {
        store.actionIsBusy("sendTurn:\(sessionID)") || store.actionIsBusy("steerTurn:\(sessionID)")
    }
    private var canSend: Bool {
        store.phase == .connected && !sending &&
        (snapshot?.isBusy == true ? snapshot?.canControlActive != false : snapshot?.canStartTurn != false) &&
        (pending != nil || (snapshot != nil && (!prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || hasAttachments))) &&
        (pending != nil || (!hasUnsupportedImages && store.pendingMutations["attachment:\(sessionID)"] == nil && !(snapshot?.isBusy == true && hasAttachments)))
    }
    private var settingsDisabled: Bool { pending != nil || sending || snapshot?.isBusy == true }
    private var approvalSymbol: String {
        switch options.approvalPolicy ?? .ask {
        case .ask: "hand.raised"
        case .auto: "checkmark.shield"
        case .fullAccess: "exclamationmark.shield"
        }
    }
    private var queuedCount: Int { store.queuedPrompts.filter { $0.sessionID == sessionID }.count }
    private var computerName: String { store.computers.first { $0.id == store.library.activeID }?.name ?? "Tu PC" }
    private var unavailableTurnMessage: String? {
        guard let snapshot, snapshot.canStartTurn == false, !snapshot.isBusy else { return nil }
        if snapshot.origin == "desktop" {
            return "Esta sesión se inicia desde el escritorio. Podés revisar su historial y crear una sesión remota desde la lista."
        }
        if snapshot.status == "interrupted" {
            return "Esta sesión quedó interrumpida. Revisá su estado en la PC antes de continuar. Podés crear una sesión remota nueva desde la lista."
        }
        return "Esta sesión no permite iniciar otro turno desde el móvil. Podés revisar su historial y crear una sesión remota nueva desde la lista."
    }

    var body: some View {
        GeometryReader { geometry in
            HStack(spacing: 0) {
                chatColumn.frame(maxWidth: .infinity)
                if showingWorkspace, geometry.size.width >= 850, let snapshot {
                    Divider()
                    WorkspacePanelView(projectID: snapshot.projectID, sessionID: sessionID,
                                       onClose: { showingWorkspace = false }, onComment: reviewCommentAction)
                        .frame(width: 370)
                }
            }
            .sheet(isPresented: Binding(get: { showingWorkspace && geometry.size.width < 850 }, set: { showingWorkspace = $0 })) {
                if let snapshot {
                    WorkspacePanelView(projectID: snapshot.projectID, sessionID: sessionID,
                                       onClose: { showingWorkspace = false }, onComment: reviewCommentAction).remoteSheet()
                }
            }
        }
        .remoteCanvas()
        .navigationTitle(snapshot.map { $0.title.isEmpty ? "Sesión" : $0.title } ?? "Sesión")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Menu {
                    Button("Mensajes en cola (\(queuedCount))", systemImage: "tray") { showingQueue = true }
                    Button("Acciones pendientes", systemImage: "hand.raised") { showingPermissions = true }
                } label: {
                    Image(systemName: "ellipsis").frame(width: 32, height: 36)
                }.accessibilityLabel("Opciones de la conversación")
                Button("Espacio de trabajo", systemImage: "sidebar.right") { showingWorkspace.toggle() }
                    .disabled(snapshot == nil)
                if snapshot?.isBusy == true && snapshot?.canControlActive != false {
                    Button("Detener turno", systemImage: "stop.circle", role: .destructive) {
                        turnToCancel = snapshot?.activeTurnID
                        confirmingStop = turnToCancel != nil
                    }
                    .disabled(store.actionIsBusy("cancelTurn:\(sessionID)") || store.phase != .connected || snapshot?.activeTurnID == nil)
                }
            }
        }
        .task(id: scenePhase) {
            if scenePhase == .active { await store.pollSession(id: sessionID) }
            else if scenePhase == .background || dictation.isRecording { stopDictation() }
        }
        .task(id: snapshot?.updatedAtMS) {
            guard store.supports("workspace.git.read"), let snapshot, !snapshot.isBusy,
                  !entries.isEmpty else { projectChangeCount = nil; return }
            let scope = store.connectionIdentity
            do {
                let status: RemoteGitStatus = try await store.load(["v1", "projects", snapshot.projectID, "git", "status"])
                guard scope == store.connectionIdentity, !Task.isCancelled else { return }
                projectChangeCount = status.files.count
            } catch { projectChangeCount = nil }
        }
        .task(id: prompt) {
            do { try await Task.sleep(for: .milliseconds(600)) } catch { return }
            if pending == nil, draftScope == store.connectionIdentity { store.saveDraft(sessionID: sessionID, text: prompt) }
        }
        .onAppear {
            draftScope = store.connectionIdentity
            if let pending { prompt = pending.prompt ?? ""; mode = pending.mode ?? .build; options = pending.options ?? .init() }
            else { prompt = store.drafts[sessionID] ?? "" }
        }
        .onDisappear {
            composerPanel = nil
            stopDictation()
            if pending == nil, draftScope == store.connectionIdentity { store.saveDraft(sessionID: sessionID, text: prompt) }
        }
        .onChange(of: dictation.transcript) {
            guard draftScope == store.connectionIdentity, pending == nil, !sending else { stopDictation(); return }
            prompt = dictationBase + (dictationBase.isEmpty || dictation.transcript.isEmpty ? "" : " ") + dictation.transcript
        }
        .onChange(of: store.connectionIdentity) { stopDictation(); composerPanel = nil }
        .onChange(of: scenePhase) { if scenePhase != .active { composerPanel = nil } }
        .onChange(of: showingAttachments) { if showingAttachments { stopDictation(); composerPanel = nil } }
        .onChange(of: pending?.requestID) { if pending != nil { stopDictation() } }
        .sheet(isPresented: $showingAdvancedModels) {
            AdvancedModelSettingsView(options: $options, disabled: settingsDisabled)
        }
        .sheet(isPresented: $showingPermissions) { SessionPermissionsView(sessionID: sessionID, mode: mode) }
        .task { await presentFixturePanel() }
        .alert("Dictado", isPresented: Binding(get: { dictation.error != nil }, set: { if !$0 { dictation.error = nil } })) {
            Button("Entendido") { dictation.error = nil }
        } message: { Text(dictation.error ?? "") }
        .sheet(isPresented: $showingQueue) { MessageQueueView(sessionID: sessionID).remoteSheet() }
        .confirmationDialog("¿Detener el turno actual?", isPresented: $confirmingStop, titleVisibility: .visible) {
            Button("Detener turno", role: .destructive) {
                if let turnToCancel { Task { await store.cancelTurn(sessionID: sessionID, turnID: turnToCancel) } }
            }
        } message: { Text("Los cambios que el agente ya hizo permanecerán en el proyecto.") }
    }

    private var chatColumn: some View {
        VStack(spacing: 0) {
            if snapshot?.origin == "desktop" {
                Label(snapshot?.canStartTurn == false ? "Sesión del escritorio · historial disponible" : "Sesión del escritorio",
                      systemImage: "desktopcomputer").font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(10)
            }
            if let error = store.sessionErrors[sessionID] {
                Label(error, systemImage: "wifi.exclamationmark")
                    .font(.caption)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
                    .background(Color.orange.opacity(0.12))
                    .accessibilityElement(children: .combine)
            }
            if let snapshot, !snapshot.pendingPermissions.isEmpty {
                Button { showingPermissions = true } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "hand.raised").font(.title3)
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Necesita tu aprobación").font(.subheadline.weight(.medium))
                            Text("\(snapshot.pendingPermissions.count) acciones para revisar").font(.caption).foregroundStyle(RemoteTheme.muted)
                        }
                        Spacer()
                        Image(systemName: "chevron.right").font(.caption.weight(.semibold))
                    }
                    .padding(16)
                    .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 20))
                }
                .buttonStyle(.plain).padding(.horizontal, 16).padding(.vertical, 8)
            }
            conversation
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .overlayPreferenceValue(ComposerAnchorKey.self) { anchors in
            GeometryReader { geometry in
                if let panel = composerPanel, let anchor = anchors[panel] {
                    floatingComposerPanel(panel, anchor: geometry[anchor], size: geometry.size)
                }
            }
        }
    }

    @ViewBuilder private func floatingComposerPanel(_ panel: ComposerPanel, anchor: CGRect, size: CGSize) -> some View {
        let preferredWidth: CGFloat = panel == .permissions ? 354 : 370
        let width: CGFloat = min(preferredWidth, max(0, size.width - 24))
        let leading: CGFloat = panel == .permissions
            ? max(12, min(anchor.midX - 58, size.width - width - 12))
            : (size.width - width) / 2
        let availableHeight: CGFloat = max(44, anchor.minY - 14)
        Color.black.opacity(0.001)
            .contentShape(Rectangle())
            .onTapGesture { closeComposerPanel() }
            .accessibilityHidden(true)
        ComposerFloatingPanel(width: width, maxHeight: availableHeight,
                              arrowX: panel == .permissions ? anchor.midX - leading : nil) {
            composerPanelContents(panel)
        }
        .frame(width: width, height: availableHeight, alignment: .bottom)
        .offset(x: leading)
        .accessibilityAddTraits(.isModal)
        .accessibilityAction(.escape, closeComposerPanel)
    }

    @ViewBuilder private func composerPanelContents(_ panel: ComposerPanel) -> some View {
        if panel == .permissions {
            ComposerPermissionsPicker(options: $options, disabled: settingsDisabled,
                                      mode: mode, pendingCount: snapshot?.pendingPermissions.count ?? 0,
                                      onSelected: closeComposerPanel) {
                closeComposerPanel(); showingPermissions = true
            }
        } else {
            QuickModelPickerView(options: $options, disabled: settingsDisabled) {
                closeComposerPanel(); showingAdvancedModels = true
            }.padding(20)
        }
    }

    private func presentFixturePanel() async {
        #if DEBUG
        guard !didPresentInitialPanel else { return }
        didPresentInitialPanel = true
        if initialPanel == "models" || initialPanel == "permissions" {
            composerFocused = true
            try? await Task<Never, Never>.sleep(for: .milliseconds(450))
        }
        switch initialPanel {
        case "models": composerPanel = .intelligence
        case "advanced": showingAdvancedModels = true
        case "permissions": composerPanel = .permissions
        case "attachments": showingAttachments = true
        default: break
        }
        #endif
    }

    private var conversation: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 20) {
                    if store.projections[sessionID]?.didTrimHistory == true {
                        Label("Mostrando actividad reciente. El historial completo permanece en tu PC.", systemImage: "clock.arrow.circlepath")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if entries.isEmpty {
                        if snapshot == nil {
                            ProgressView("Cargando sesión…").frame(maxWidth: .infinity).padding(.top, 50)
                        } else {
                            Color.clear.frame(height: 1)
                        }
                    }
                    ForEach(entries) { entry in ConversationEntryView(entry: entry).id(entry.id) }
                    if let count = projectChangeCount, count > 0, snapshot?.isBusy != true {
                        Button { closeComposerPanel(); showingWorkspace = true } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "doc.text.magnifyingglass").font(.title3)
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("Cambios del proyecto").font(.subheadline.weight(.medium))
                                    Text("\(count) \(count == 1 ? "archivo modificado" : "archivos modificados")")
                                        .font(.caption).foregroundStyle(RemoteTheme.muted)
                                }
                                Spacer()
                                Image(systemName: "chevron.right").font(.caption.weight(.medium))
                            }.padding(16).background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 20))
                        }.buttonStyle(.plain)
                    }
                    if snapshot?.isBusy == true {
                        HStack(spacing: 8) {
                            ProgressView()
                            Text(snapshot?.statusTitle ?? "Trabajando").font(.footnote).foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .combine)
                    }
                    Color.clear.frame(height: 1).id("conversation-bottom")
                }
                .padding()
            }
            .scrollDismissesKeyboard(.interactively)
            .simultaneousGesture(DragGesture(minimumDistance: 8).onChanged { _ in followingLatest = false })
            .overlay(alignment: .bottomTrailing) {
                if !followingLatest, !entries.isEmpty {
                    Button("Ir al final", systemImage: "arrow.down") {
                        followingLatest = true
                        withAnimation { proxy.scrollTo("conversation-bottom", anchor: .bottom) }
                    }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.plain)
                    .foregroundStyle(RemoteTheme.onAccent)
                    .padding(.horizontal, 14).frame(minHeight: 44)
                    .background(RemoteTheme.accent, in: Capsule())
                    .padding()
                }
            }
            .onChange(of: entries.last?.id) {
                if followingLatest { proxy.scrollTo("conversation-bottom", anchor: .bottom) }
            }
            .onChange(of: entries.last?.text.count) {
                if followingLatest { proxy.scrollTo("conversation-bottom", anchor: .bottom) }
            }
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 10) {
            if pending != nil {
                Label("Hay un envío sin confirmar. Reintentá para recuperar su resultado.", systemImage: "arrow.clockwise")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 8)
            }
            HStack(spacing: 8) {
                Label(computerName, systemImage: "desktopcomputer")
                    .lineLimit(1)
                if let snapshot, let project = store.projects.first(where: { $0.id == snapshot.projectID }) {
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .medium))
                    Text(project.name).lineLimit(1)
                }
                Spacer(minLength: 0)
                if queuedCount > 0 {
                    Button { showingQueue = true } label: { Label("\(queuedCount)", systemImage: "tray") }
                        .accessibilityLabel("\(queuedCount) mensajes en cola")
                }
            }
            .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 10)

            VStack(alignment: .leading, spacing: 8) {
                AttachmentSelectionView(sessionID: sessionID, disabled: pending != nil || sending)
                if hasUnsupportedImages {
                    Text("El modelo elegido no admite imágenes. Elegí otro modelo o quitá las fotos.")
                        .font(.caption).foregroundStyle(.orange).padding(.horizontal, 6)
                }
                if mode == .plan {
                    HStack(spacing: 6) {
                        Image(systemName: "list.bullet.clipboard")
                        Text("Planificar").font(.caption.weight(.medium))
                        if !settingsDisabled {
                            Button { mode = .build } label: { Image(systemName: "xmark").font(.caption2).frame(width: 30, height: 32) }
                                .buttonStyle(.plain).accessibilityLabel("Salir del modo de planificación")
                        }
                    }
                    .padding(.leading, 12).padding(.trailing, settingsDisabled ? 12 : 0)
                    .frame(minHeight: 32)
                    .background(RemoteTheme.raised, in: Capsule())
                }
                TextField(snapshot?.isBusy == true ? "Dale una indicación al agente…" : "Preguntale a openADE", text: $prompt, axis: .vertical)
                    .font(.body)
                    .lineLimit(2...6)
                    .padding(.horizontal, 6).padding(.top, 4)
                    .focused($composerFocused)
                    .disabled(pending != nil || sending || dictation.isRecording || dictation.isStarting)
                    .accessibilityLabel("Mensaje para el agente")
                if dictation.isRecording {
                    Label("Escuchando… Tocá el micrófono para terminar.", systemImage: "waveform")
                        .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 6)
                        .accessibilityAddTraits(.updatesFrequently)
                }
                HStack(spacing: 2) {
                    if let snapshot {
                        AttachmentComposerView(sessionID: sessionID, projectID: snapshot.projectID,
                                               disabled: pending != nil || sending || snapshot.isBusy || snapshot.canStartTurn == false || dictation.isRecording,
                                               modeDisabled: settingsDisabled, allowsImages: selectedModel?.supportsImages != false, prompt: $prompt, mode: $mode,
                                               showingOptions: $showingAttachments)
                    }
                    ComposerToolButton(title: "Permisos de las acciones", symbol: approvalSymbol,
                                       selected: composerPanel == .permissions || snapshot?.pendingPermissions.isEmpty == false) {
                        toggleComposerPanel(.permissions)
                    }
                    .anchorPreference(key: ComposerAnchorKey.self, value: .bounds) { [.permissions: $0] }
                    Spacer(minLength: 0)
                    if store.supports("models.list") {
                        ComposerToolButton(title: "Modelo e inteligencia", symbol: "gauge.with.dots.needle.33percent") {
                            toggleComposerPanel(.intelligence)
                        }
                        .anchorPreference(key: ComposerAnchorKey.self, value: .bounds) { [.intelligence: $0] }
                    }
                    ComposerToolButton(title: dictation.isRecording ? "Terminar dictado" : dictation.isStarting ? "Cancelar dictado" : "Dictar mensaje",
                                       symbol: dictation.isRecording ? "waveform" : "mic", selected: dictation.isRecording) {
                        if dictation.isRecording || dictation.isStarting { stopDictation() }
                        else {
                            closeComposerPanel()
                            composerFocused = false
                            dictationBase = prompt
                            dictationTask = Task { await dictation.start() }
                        }
                    }
                    .disabled(pending != nil || sending)
                    .overlay(alignment: .topTrailing) {
                        if dictation.isStarting { ProgressView().controlSize(.mini).allowsHitTesting(false) }
                    }
                    Button(action: send) {
                        Group {
                            if sending { ProgressView().tint(RemoteTheme.onAccent) }
                            else { Image(systemName: pending == nil ? "arrow.up" : "arrow.clockwise").font(.system(size: 20, weight: .medium)) }
                        }
                        .frame(width: 44, height: 44)
                        .foregroundStyle(canSend ? RemoteTheme.onAccent : RemoteTheme.muted)
                        .background(canSend ? RemoteTheme.accent : RemoteTheme.raised, in: Circle())
                    }
                    .buttonStyle(.plain)
                    .disabled(!canSend)
                    .accessibilityLabel(pending != nil ? "Reintentar envío sin duplicar" : snapshot?.isBusy == true ? "Enviar indicación al turno actual" : "Enviar mensaje")
                }
            }
            .padding(10)
            .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
            .overlay { RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(RemoteTheme.border, lineWidth: 0.75) }

            if snapshot?.isBusy == true, !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, pending == nil, !hasAttachments {
                Button("Guardar mensaje en cola", systemImage: "tray.and.arrow.down") {
                    stopDictation()
                    if store.enqueue(sessionID: sessionID, text: prompt, mode: mode, options: options) { prompt = ""; store.saveDraft(sessionID: sessionID, text: "") }
                }
                .font(.caption).padding(.horizontal, 10).disabled(sending)
            }
            if let unavailableTurnMessage {
                Text(unavailableTurnMessage)
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 10)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 8)
        .background(RemoteTheme.canvas)
    }

    private func stopDictation() {
        dictationTask?.cancel()
        dictationTask = nil
        dictation.stop()
    }

    private func closeComposerPanel() { composerPanel = nil }

    private var reviewCommentAction: ((String) -> Void)? {
        guard pending == nil, !sending, draftScope == store.connectionIdentity else { return nil }
        let scope = store.connectionIdentity
        return { text in
            guard scope == store.connectionIdentity, self.pending == nil, !self.sending else { return }
            stopDictation()
            prompt += prompt.isEmpty ? text : "\n\n" + text
            store.saveDraft(sessionID: sessionID, text: prompt)
            showingWorkspace = false
            composerFocused = true
        }
    }

    private func toggleComposerPanel(_ panel: ComposerPanel) {
        stopDictation()
        composerPanel = composerPanel == panel ? nil : panel
    }

    private func send() {
        guard canSend else { return }
        closeComposerPanel()
        stopDictation()
        composerFocused = false
        followingLatest = true
        let scope = store.connectionIdentity
        let text = prompt
        let turnMode = mode
        let turnOptions = options
        Task {
            guard scope == store.connectionIdentity else { return }
            if await store.sendPrompt(sessionID: sessionID, text: text, mode: turnMode, options: turnOptions),
               scope == store.connectionIdentity {
                prompt = ""; store.saveDraft(sessionID: sessionID, text: "")
            }
        }
    }
}
