import SwiftUI

/// The PC's tasks as its sidebar lists them, with the open task beside them
/// on iPad and pushed on iPhone.
struct HomeView: View {
    @Environment(RemoteStore.self) private var store
    @State private var selection: String?
    @State private var compactColumn: NavigationSplitViewColumn = .sidebar
    @State private var query = ""
    @State private var showingNewTask = false
    @State private var showingSettings = false
    @State private var showingPairing = false
    @State private var renaming: RemoteTask?
    @State private var renameText = ""

    private var sections: [TaskSection] {
        TaskListLayout.sections(tasks: store.snapshot.tasks, projects: store.snapshot.projects, query: query)
    }

    var body: some View {
        NavigationSplitView(preferredCompactColumn: $compactColumn) {
            taskList
                .navigationSplitViewColumnWidth(min: 300, ideal: 360, max: 440)
        } detail: {
            if let selection {
                TaskDetailView(taskID: selection, onClose: { self.selection = nil }).id(selection)
            } else {
                VStack(spacing: 8) {
                    RemoteEmptyState(symbol: "bubble.left.and.bubble.right", title: "Elegí una tarea",
                                     message: "O empezá una nueva en \(store.computerName).")
                    Button("Nueva tarea", systemImage: "plus") { showingNewTask = true }
                        .buttonStyle(RemotePrimaryButtonStyle()).frame(maxWidth: 240)
                        .disabled(!store.isOnline)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .remoteCanvas()
            }
        }
        .onChange(of: store.openTaskRequest, initial: true) {
            guard let id = store.openTaskRequest else { return }
            store.openTaskRequest = nil
            selection = id
            compactColumn = .detail
        }
        .sheet(isPresented: $showingNewTask) { NewTaskView().remoteSheet() }
        .sheet(isPresented: $showingSettings) { SettingsView().remoteSheet() }
        .sheet(isPresented: $showingPairing) { PairingSheet().remoteSheet() }
        .alert("Renombrar tarea", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Título", text: $renameText)
            Button("Cancelar", role: .cancel) { renaming = nil }
            Button("Guardar") {
                if let task = renaming { Task { await store.rename(task.id, renameText) } }
                renaming = nil
            }
        }
    }

    private var taskList: some View {
        List(selection: $selection) {
            if store.connection == .revoked { revokedBanner }
            ForEach(sections) { section in
                Section {
                    ForEach(section.tasks) { task in
                        NavigationLink(value: task.id) { TaskRow(task: task, projectName: section.id == "pinned" ? store.project(task.project)?.name : nil) }
                            .listRowBackground(selection == task.id ? RemoteTheme.raised : Color.clear)
                            .listRowSeparatorTint(RemoteTheme.border.opacity(0.5))
                            .listRowInsets(EdgeInsets(top: 10, leading: 16, bottom: 10, trailing: 14))
                            .swipeActions(edge: .leading) {
                                Button(task.pinned ? "Desfijar" : "Fijar", systemImage: task.pinned ? "pin.slash" : "pin") {
                                    Task { await store.setPinned(task.id, !task.pinned) }
                                }
                                .tint(.indigo)
                            }
                            .swipeActions(edge: .trailing) {
                                Button("Archivar", systemImage: "archivebox", role: .destructive) {
                                    Task { await store.archive(task.id) }
                                }
                                .tint(.gray)
                                Button("Renombrar", systemImage: "pencil") { renameText = task.title; renaming = task }
                                    .tint(.blue)
                            }
                            .contextMenu {
                                Button("Renombrar", systemImage: "pencil") { renameText = task.title; renaming = task }
                                Button(task.pinned ? "Desfijar" : "Fijar", systemImage: task.pinned ? "pin.slash" : "pin") {
                                    Task { await store.setPinned(task.id, !task.pinned) }
                                }
                                Button("Archivar", systemImage: "archivebox", role: .destructive) { Task { await store.archive(task.id) } }
                            }
                    }
                } header: {
                    HStack(spacing: 6) {
                        if section.id == "pinned" { Image(systemName: "pin.fill").imageScale(.small) }
                        Text(section.title)
                    }
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(RemoteTheme.muted)
                    .textCase(nil)
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .searchable(text: $query, prompt: "Buscar tareas")
        .refreshable { await store.refresh() }
        .overlay { if sections.isEmpty && store.connection != .revoked { emptyState } }
        .safeAreaInset(edge: .bottom, spacing: 0) { newTaskDock }
        .connectionPill(onRevoked: { showingPairing = true })
        .remoteCanvas()
        .navigationTitle(store.computerName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarTitleMenu {
            ForEach(store.library.computers) { computer in
                Button {
                    store.switchTo(computer.id)
                } label: {
                    if computer.id == store.library.activeID { Label(computer.name, systemImage: "checkmark") }
                    else { Text(computer.name) }
                }
            }
            Button("Emparejar otra PC", systemImage: "qrcode.viewfinder") { showingPairing = true }
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Ajustes", systemImage: "gearshape") { showingSettings = true }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Nueva tarea", systemImage: "square.and.pencil") { showingNewTask = true }
                    .disabled(!store.isOnline)
            }
        }
    }

    private var revokedBanner: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Acceso revocado", systemImage: "lock.slash").font(.headline).foregroundStyle(.red)
            Text("\(store.computerName) ya no reconoce a este iPhone. Emparejá de nuevo desde openADE en tu PC: Ajustes → Remoto.")
                .font(.subheadline).foregroundStyle(RemoteTheme.muted)
            Button("Emparejar de nuevo", systemImage: "qrcode.viewfinder") { showingPairing = true }
                .buttonStyle(RemotePrimaryButtonStyle())
        }
        .padding(16)
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 22))
        .listRowBackground(Color.clear)
        .listRowSeparator(.hidden)
        .padding(.vertical, 6)
    }

    @ViewBuilder private var emptyState: some View {
        if !query.isEmpty {
            RemoteEmptyState(symbol: "magnifyingglass", title: "Sin resultados", message: "Ninguna tarea coincide con “\(query)”.")
        } else if !store.hasSnapshot && (store.connection == .connecting || store.connection == .reconnecting) {
            ProgressView("Conectando con \(store.computerName)…")
        } else if !store.hasSnapshot {
            VStack(spacing: 4) {
                RemoteEmptyState(symbol: "wifi.slash", title: "No se pudo conectar",
                                 message: store.connectionDetail ?? "Revisá que openADE esté abierto en \(store.computerName) y que el iPhone esté en la misma red.")
                Button("Reintentar", systemImage: "arrow.clockwise") { store.reconnect() }
                    .buttonStyle(RemoteSecondaryButtonStyle()).frame(maxWidth: 220)
            }
        } else {
            VStack(spacing: 4) {
                RemoteEmptyState(symbol: "sparkles", title: "Todavía no hay tareas",
                                 message: "Empezá una desde acá y seguila desde donde estés.")
                Button("Nueva tarea", systemImage: "plus") { showingNewTask = true }
                    .buttonStyle(RemotePrimaryButtonStyle()).frame(maxWidth: 220)
                    .disabled(!store.isOnline)
            }
        }
    }

    private var newTaskDock: some View {
        Button { showingNewTask = true } label: {
            HStack(spacing: 12) {
                Image(systemName: "plus").font(.system(size: 20, weight: .regular))
                Text("Pedile algo a \(store.computerName)").font(.body).foregroundStyle(RemoteTheme.muted).lineLimit(1)
                Spacer(minLength: 0)
                Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold))
                    .frame(width: 34, height: 34).background(RemoteTheme.raised, in: Circle())
            }
            .padding(.leading, 18).padding(.trailing, 7).padding(.vertical, 7)
            .background(RemoteTheme.surface, in: Capsule())
            .overlay { Capsule().strokeBorder(RemoteTheme.border, lineWidth: 0.5) }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(!store.isOnline)
        .opacity(store.isOnline ? 1 : 0.55)
        .accessibilityLabel("Nueva tarea")
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(RemoteTheme.canvas)
    }
}

struct TaskRow: View {
    let task: RemoteTask
    var projectName: String? = nil
    /// The app speaks Spanish whatever the iPhone's language: "hace 4 min".
    private static let relative = Date.RelativeFormatStyle(presentation: .numeric, unitsStyle: .abbreviated,
                                                           locale: Locale(identifier: "es"))

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            indicator
                .frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(task.displayTitle)
                        .font(.body.weight(task.unread ? .semibold : .regular))
                        .foregroundStyle(RemoteTheme.ink)
                        .lineLimit(2)
                    Spacer(minLength: 6)
                    if task.updatedAtMS > 0 {
                        Text(task.updatedAt.formatted(Self.relative))
                            .font(.caption).foregroundStyle(RemoteTheme.muted).lineLimit(1)
                    }
                }
                HStack(spacing: 6) {
                    RemoteTag(title: task.agent, symbol: task.chat ? nil : "terminal")
                    if let projectName {
                        HStack(spacing: 3) { Image(systemName: "folder"); Text(projectName) }
                            .foregroundStyle(RemoteTheme.muted)
                    }
                    if task.isWaiting { Text("Te necesita").fontWeight(.medium).foregroundStyle(RemoteTheme.attention) }
                    else if task.isRunning { Text("Trabajando").foregroundStyle(RemoteTheme.muted) }
                    if task.queued > 0 {
                        HStack(spacing: 3) { Image(systemName: "tray.full"); Text("\(task.queued) en cola") }
                            .foregroundStyle(RemoteTheme.muted)
                    }
                    Spacer(minLength: 0)
                    if task.unread {
                        Circle().fill(Color.accentColor).frame(width: 9, height: 9)
                            .accessibilityHidden(true)
                    }
                }
                .font(.caption)
                .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityValue(task.unread ? "Sin leer" : "")
    }

    @ViewBuilder private var indicator: some View {
        if task.isWaiting {
            Image(systemName: "hand.raised.fill").foregroundStyle(RemoteTheme.attention)
        } else if task.isRunning {
            ProgressView().controlSize(.small)
        } else {
            Image(systemName: task.chat ? "bubble.left" : "terminal").foregroundStyle(RemoteTheme.muted)
        }
    }
}
