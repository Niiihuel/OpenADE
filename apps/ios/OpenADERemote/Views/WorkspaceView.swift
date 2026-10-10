import SwiftUI

struct WorkspaceView: View {
    @Environment(RemoteStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase
    @State private var selectedSession: String?
    @State private var compactColumn: NavigationSplitViewColumn = .sidebar
    @State private var selectedProject = ""
    @State private var query = ""
    @State private var searching = false
    @State private var filter: SessionListFilter = .active
    @State private var showingConnections = false
    @State private var showingPending = false
    @State private var showingNewTask = false
    @State private var renaming: RemoteSession?
    @State private var renameText = ""

    private var visibleSessions: [RemoteSession] {
        store.sessions.filter { session in
            filter.includes(session) && (selectedProject.isEmpty || session.projectID == selectedProject)
                && (query.isEmpty || session.title.localizedStandardContains(query) || session.id.localizedStandardContains(query))
        }.sorted {
            if ($0.favorite == true) != ($1.favorite == true) { return $0.favorite == true }
            return $0.updatedAtMS > $1.updatedAtMS
        }
    }

    var body: some View {
        NavigationSplitView(preferredCompactColumn: $compactColumn) {
            sidebar.navigationTitle("openADE").navigationBarTitleDisplayMode(.inline)
                .navigationSplitViewColumnWidth(min: 300, ideal: 360, max: 440).remoteCanvas()
        } detail: {
            if let selectedSession {
                SessionDetailView(sessionID: selectedSession).id(selectedSession)
            } else {
                VStack {
                    RemoteEmptyState(symbol: "bubble.left.and.bubble.right", title: "Tu próxima idea empieza acá",
                                     message: "Elegí una tarea o empezá una conversación con tu PC.")
                    Button("Nueva tarea", systemImage: "plus") { showingNewTask = true }
                        .buttonStyle(RemotePrimaryButtonStyle()).frame(maxWidth: 240)
                        .disabled(store.projects.isEmpty || store.phase != .connected)
                }.frame(maxWidth: .infinity, maxHeight: .infinity).remoteCanvas()
            }
        }
        .task(id: scenePhase) { if scenePhase == .active { await store.pollOverview() } }
        .task(id: store.health?.supports("models.list")) { await store.loadModelCatalog() }
        .task(id: query) {
            do { try await Task.sleep(for: .milliseconds(300)) } catch { return }
            await store.searchSessions(query)
        }
        .onChange(of: store.navigationSessionID, initial: true) {
            if let target = store.navigationSessionID { openSession(target) }
        }
        .sheet(isPresented: $showingConnections) { ConnectionsView().remoteSheet() }
        .sheet(isPresented: $showingPending) { PendingRequestsView().remoteSheet() }
        .sheet(isPresented: $showingNewTask) {
            NewTaskProjectView(initialProjectID: selectedProject) { id in
                showingNewTask = false; openSession(id)
            }.remoteSheet()
        }
        .alert("Renombrar tarea", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Título", text: $renameText)
            Button("Cancelar", role: .cancel) { renaming = nil }
            Button("Guardar") {
                if let session = renaming {
                    let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !title.isEmpty { Task { await store.updateSession(id: session.id, title: title) } }
                }
                renaming = nil
            }
        }
        .alert("openADE", isPresented: Binding(get: { store.notice != nil }, set: { if !$0 { store.notice = nil } })) {
            Button("Entendido", role: .cancel) { store.notice = nil }
        } message: { Text(store.notice ?? "") }
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            if searching {
                RemoteSearchField(text: $query, prompt: "Buscar tareas en esta PC", autoFocus: true)
                    .padding(.horizontal, 16).padding(.top, 8)
            }
            computerStrip
            HStack(spacing: 0) {
                ScrollView(.horizontal) {
                    HStack(spacing: 2) {
                        ForEach(SessionListFilter.allCases.filter {
                            store.supports("sessions.metadata") || $0 == .active || $0 == .attention
                        }) { item in
                            RemoteChip(title: item == .active ? "Todas" : item.title, selected: filter == item) { filter = item }
                        }
                    }
                }.scrollIndicators(.hidden)
                projectFilter
            }.padding(.horizontal, 12).padding(.bottom, 6)
            if store.hasUnreadableSessions {
                Label("Hay tareas del escritorio que necesitan revisión en la PC.", systemImage: "exclamationmark.circle")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 20).padding(.vertical, 8)
            }
            if visibleSessions.isEmpty {
                GeometryReader { geometry in
                    ScrollView {
                        VStack {
                            Spacer(minLength: 24)
                            RemoteEmptyState(symbol: query.isEmpty ? "bubble.left.and.bubble.right" : "magnifyingglass",
                                             title: store.hasMoreSessions ? "Sin coincidencias en esta página" : query.isEmpty ? "Aún no hay tareas aquí" : "No encontramos esa tarea",
                                             message: store.hasMoreSessions ? "Podés cargar más tareas o cambiar los filtros." : query.isEmpty ? "Iniciá una tarea para verla acá." : "Probá otro nombre o cambiá los filtros.")
                            if !query.isEmpty || !selectedProject.isEmpty || filter != .active {
                                Button("Limpiar filtros") { query = ""; selectedProject = ""; filter = .active }
                                    .font(.subheadline).padding(12)
                            }
                            if store.hasMoreSessions { paginationControls.padding(.horizontal, 20) }
                            Spacer(minLength: 24)
                        }.frame(maxWidth: .infinity, minHeight: geometry.size.height)
                    }.refreshable { await store.refreshOverview() }
                }
            } else { sessionList }
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Computadoras", systemImage: "line.3.horizontal") { showingConnections = true }
                    .labelStyle(.iconOnly)
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                if !store.pendingMutations.isEmpty || !store.pendingActions.isEmpty {
                    Button("Envíos pendientes", systemImage: "tray") { showingPending = true }.labelStyle(.iconOnly)
                }
                Button(searching ? "Cerrar búsqueda" : "Buscar tareas", systemImage: searching ? "xmark" : "magnifyingglass") {
                    searching.toggle()
                    if !searching { query = "" }
                }.labelStyle(.iconOnly)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { composeDock }
    }

    private var computerStrip: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(store.computers) { computer in
                    RemoteChip(title: computer.name, symbol: "laptopcomputer", selected: computer.id == store.library.activeID) {
                        guard computer.id != store.library.activeID else { showingConnections = true; return }
                        Task { _ = await store.switchComputer(id: computer.id) }
                    }.disabled(store.isConnecting)
                }
                RemoteIconButton(title: "Administrar computadoras", symbol: "plus") { showingConnections = true }
            }.padding(.horizontal, 16)
        }.scrollIndicators(.hidden).padding(.vertical, 12)
    }

    private var projectFilter: some View {
        Menu {
            Picker("Proyecto", selection: $selectedProject) {
                Text("Todos los proyectos").tag("")
                ForEach(store.projects) { Text($0.name).tag($0.id) }
            }
        } label: {
            Image(systemName: selectedProject.isEmpty ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
                .font(.body).frame(width: 44, height: 44).contentShape(Rectangle())
        }.accessibilityLabel("Filtrar por proyecto")
    }

    private var sessionList: some View {
        List(selection: $selectedSession) {
            ForEach(store.projects.filter { selectedProject.isEmpty || $0.id == selectedProject }) { project in
                let projectSessions = visibleSessions.filter { $0.projectID == project.id }
                if !projectSessions.isEmpty {
                    Section {
                        ForEach(projectSessions) { session in
                            NavigationLink(value: session.id) { SessionRow(session: session) }
                                .contextMenu { sessionActions(session) }
                                .listRowBackground(Color.clear).listRowSeparator(.hidden)
                                .listRowInsets(EdgeInsets(top: 6, leading: 20, bottom: 6, trailing: 18))
                                .swipeActions(edge: .trailing) {
                                    if store.supports("sessions.metadata"), session.origin != "desktop" {
                                        Button(session.archived == true ? "Restaurar" : "Archivar", systemImage: "archivebox") {
                                            Task { await store.updateSession(id: session.id, archived: session.archived != true) }
                                        }.tint(.gray).disabled(metadataBlocked(session))
                                    }
                                }
                        }
                    } header: {
                        Text(project.name).font(.caption.weight(.medium)).textCase(nil).foregroundStyle(RemoteTheme.muted)
                    }
                }
            }
            if store.hasMoreSessions {
                Section { paginationControls }.listRowBackground(Color.clear)
            }
        }.listStyle(.plain).scrollContentBackground(.hidden)
            .refreshable { await store.refreshOverview() }
    }

    @ViewBuilder private var paginationControls: some View {
        if store.sessionPageCount < 50 {
            Button("Cargar más tareas") { Task { await store.loadMoreSessions() } }.frame(minHeight: 44)
        } else {
            Text("Acotá la búsqueda para consultar otras tareas.").font(.caption).foregroundStyle(RemoteTheme.muted)
        }
    }

    private var composeDock: some View {
        VStack(spacing: 12) {
            if store.phase != .connected { ConnectionStatusView().padding(.horizontal, 8) }
            Button { showingNewTask = true } label: {
                HStack(spacing: 14) {
                    Image(systemName: "plus").font(.system(size: 23, weight: .regular))
                    Text("Pedile algo a openADE").font(.body).foregroundStyle(RemoteTheme.muted)
                    Spacer(minLength: 0)
                    Image(systemName: "arrow.up").font(.system(size: 18, weight: .medium))
                        .frame(width: 38, height: 38).background(RemoteTheme.raised, in: Circle())
                }.padding(.leading, 18).padding(.trailing, 8).padding(.vertical, 8)
                    .background(RemoteTheme.surface, in: Capsule())
                    .overlay { Capsule().strokeBorder(RemoteTheme.border, lineWidth: 0.5) }.contentShape(Capsule())
            }.buttonStyle(.plain).disabled(store.projects.isEmpty || store.phase != .connected)
                .opacity(store.phase == .connected ? 1 : 0.55)
                .accessibilityLabel("Nueva tarea").accessibilityHint("Elegí el proyecto y escribí tu mensaje")
        }.padding(.horizontal, 20).padding(.top, 12).padding(.bottom, 12).background(RemoteTheme.canvas)
    }

    private func openSession(_ id: String) { selectedSession = id; compactColumn = .detail }
    @ViewBuilder private func sessionActions(_ session: RemoteSession) -> some View {
        if store.supports("sessions.metadata"), session.origin != "desktop" {
            Button("Renombrar", systemImage: "pencil") { renameText = session.title; renaming = session }
                .disabled(metadataBlocked(session))
            Button(session.favorite == true ? "Quitar de favoritas" : "Marcar favorita", systemImage: session.favorite == true ? "star.slash" : "star") {
                Task { await store.updateSession(id: session.id, favorite: session.favorite != true) }
            }.disabled(metadataBlocked(session))
            Button(session.archived == true ? "Restaurar tarea" : "Archivar tarea", systemImage: "archivebox") {
                Task { await store.updateSession(id: session.id, archived: session.archived != true) }
            }.disabled(metadataBlocked(session))
            if store.pendingMutations["session:\(session.id)"] != nil {
                Button("Revisar actualización pendiente", systemImage: "tray") { showingPending = true }
            }
        }
    }
    private func metadataBlocked(_ session: RemoteSession) -> Bool {
        store.pendingMutations["session:\(session.id)"] != nil || store.actionIsBusy("session:\(session.id)")
    }
}

struct SessionRow: View {
    let session: RemoteSession
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: session.pendingPermissions.isEmpty ? (session.isBusy ? "circle.dotted" : "bubble.left") : "hand.raised")
                .font(.system(size: 18, weight: .regular)).foregroundStyle(RemoteTheme.muted)
                .frame(width: 26, height: 28).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 7) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(session.title.isEmpty ? "Nueva tarea" : session.title).font(.body.weight(.medium)).lineLimit(2)
                    if session.favorite == true { Image(systemName: "star.fill").font(.caption2).foregroundStyle(RemoteTheme.muted) }
                }
                HStack {
                    Text(session.statusTitle).lineLimit(1)
                    Spacer(minLength: 6)
                    Text(session.updatedAt, style: .relative)
                }.font(.caption).foregroundStyle(RemoteTheme.muted)
            }
        }.padding(.vertical, 8).accessibilityElement(children: .combine)
    }
}

struct ConnectionStatusView: View {
    @Environment(RemoteStore.self) private var store
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: store.phase == .connected ? "checkmark.circle" : "wifi.exclamationmark")
            Text(store.statusTitle).font(.caption)
            Spacer()
            if store.phase == .connecting || store.phase == .reconnecting { ProgressView().controlSize(.small) }
        }.foregroundStyle(RemoteTheme.muted).accessibilityElement(children: .combine)
    }
}

struct NewTaskProjectView: View {
    var initialProjectID = ""
    let onCreated: (String) -> Void
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var selected = ""
    @State private var creating = false
    @State private var query = ""
    @State private var creationError: String?
    private var choices: [RemoteProject] {
        store.projects.filter { query.isEmpty || $0.name.localizedStandardContains(query) || $0.path.localizedStandardContains(query) }
    }
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("¿En qué vamos a trabajar?").font(.title2.weight(.medium))
                        Label(store.activeComputerName, systemImage: "laptopcomputer").font(.subheadline).foregroundStyle(RemoteTheme.muted)
                    }.padding(.top, 12)
                    if store.projects.count > 5 {
                        RemoteSearchField(text: $query, prompt: "Buscar proyecto")
                    }
                    RemoteCard(padding: 0) {
                        VStack(spacing: 0) {
                            ForEach(choices) { project in
                                Button { selected = project.id } label: {
                                    HStack(spacing: 14) {
                                        Image(systemName: "folder").font(.title3).frame(width: 40, height: 44)
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(project.name).font(.body.weight(.medium))
                                            Text(project.path).font(.caption).foregroundStyle(RemoteTheme.muted).lineLimit(2).truncationMode(.middle)
                                        }
                                        Spacer(minLength: 8)
                                        Image(systemName: selected == project.id ? "checkmark.circle.fill" : "circle")
                                            .foregroundStyle(selected == project.id ? RemoteTheme.ink : RemoteTheme.muted)
                                    }.padding(16).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                                }.buttonStyle(.plain).disabled(creating)
                                    .accessibilityAddTraits(selected == project.id ? [.isSelected] : [])
                                if project.id != choices.last?.id { Divider().padding(.leading, 70) }
                            }
                        }
                    }
                    if choices.isEmpty {
                        RemoteEmptyState(symbol: "folder", title: "Sin proyectos disponibles", message: query.isEmpty ? "Agregá un proyecto al servicio remoto de esta PC para empezar." : "Probá con otro nombre.")
                    }
                    if let creationError { Label(creationError, systemImage: "exclamationmark.circle").font(.callout).foregroundStyle(.red) }
                }.padding(20)
            }
            .navigationTitle("Nueva tarea").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancelar") { dismiss() }.disabled(creating) } }
            .safeAreaInset(edge: .bottom) {
                Button {
                    guard !creating else { return }
                    creating = true
                    let scope = store.connectionIdentity
                    let projectID = selected
                    Task {
                        guard scope == store.connectionIdentity else { creating = false; return }
                        let id = await store.createSession(projectID: projectID)
                        creating = false
                        guard scope == store.connectionIdentity else { return }
                        if let id { onCreated(id) }
                        else { creationError = store.notice ?? "No se pudo crear la tarea. Reintentá para recuperar el mismo envío." }
                    }
                } label: {
                    HStack(spacing: 10) {
                        if creating { ProgressView().tint(RemoteTheme.onAccent) }
                        Text(creating ? "Preparando…" : "Continuar")
                    }
                }.buttonStyle(RemotePrimaryButtonStyle())
                    .disabled(selected.isEmpty || creating || store.phase != .connected)
                    .padding(20).background(RemoteTheme.canvas)
            }.remoteCanvas()
            .onAppear {
                if store.projects.contains(where: { $0.id == initialProjectID }) { selected = initialProjectID }
                else if store.projects.count == 1 { selected = store.projects[0].id }
            }.interactiveDismissDisabled(creating)
        }
    }
}
