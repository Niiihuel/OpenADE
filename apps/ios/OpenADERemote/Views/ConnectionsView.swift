import SwiftUI

struct ConnectionsView: View {
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var showingAdd = false
    @State private var forgetting: SavedComputer?
    @State private var renaming: SavedComputer?
    @State private var name = ""
    @AppStorage("openade.remote.appearance") private var appearance = "system"

    var body: some View {
        NavigationStack {
            List {
                Section("Tus computadoras") {
                    ForEach(store.computers) { computer in
                        Button {
                            if computer.id == store.library.activeID { dismiss() }
                            else { Task { if await store.switchComputer(id: computer.id) { dismiss() } } }
                        } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "desktopcomputer")
                                    .font(.title3).foregroundStyle(RemoteTheme.ink)
                                    .frame(width: 44, height: 44)
                                    .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 14))
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(computer.name).foregroundStyle(.primary)
                                    Text(computer.configuration.displayAddress).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if computer.id == store.library.activeID {
                                    Image(systemName: "checkmark").foregroundStyle(RemoteTheme.ink)
                                        .accessibilityLabel("Computadora actual")
                                }
                            }.padding(.vertical, 4)
                        }
                        .disabled(store.isConnecting)
                        .contextMenu {
                            Button("Cambiar nombre", systemImage: "pencil") { name = computer.name; renaming = computer }
                            Button("Olvidar computadora", systemImage: "trash", role: .destructive) { forgetting = computer }
                        }
                        .swipeActions { Button("Olvidar", role: .destructive) { forgetting = computer } }
                    }
                    Button("Agregar computadora", systemImage: "plus") { showingAdd = true }
                        .frame(minHeight: 44)
                }
                .listRowBackground(RemoteTheme.surface)
                if let host = store.health {
                    Section("Conexión actual") {
                        ConnectionStatusView()
                        LabeledContent("Proveedor", value: host.provider)
                        LabeledContent("Modelo", value: host.model)
                    }
                    .listRowBackground(RemoteTheme.surface)
                }
                if store.supports("notifications.push") { NotificationSettingsSection().listRowBackground(RemoteTheme.surface) }
                Section("Apariencia") {
                    Picker("Tema", selection: $appearance) {
                        Text("Del sistema").tag("system")
                        Text("Oscuro").tag("dark")
                        Text("Claro").tag("light")
                    }
                }
                .listRowBackground(RemoteTheme.surface)
                Section {
                    Text("Las credenciales y los envíos pendientes se guardan por computadora en el llavero de este iPhone.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                .listRowBackground(Color.clear)
            }
            .scrollContentBackground(.hidden)
            .remoteCanvas()
            .navigationTitle("Computadoras")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .sheet(isPresented: $showingAdd) { ConnectView(isAdding: true).remoteSheet() }
            .confirmationDialog("¿Olvidar esta computadora?", isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } })) {
                Button("Olvidar", role: .destructive) { if let forgetting { store.forgetComputer(id: forgetting.id) }; forgetting = nil }
            } message: {
                Text("Se eliminarán sus credenciales y mensajes guardados en este iPhone. Los trabajos de la PC siguen ejecutándose; revocá el acceso desde la PC si es necesario.")
            }
            .alert("Nombre de la computadora", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
                TextField("Nombre", text: $name)
                Button("Cancelar", role: .cancel) { renaming = nil }
                Button("Guardar") { if let renaming { store.renameComputer(id: renaming.id, name: name) }; renaming = nil }
            }
        }
    }
}

struct PendingRequestsView: View {
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var discarding: DurableMutation?
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text("Cada reintento consulta la misma operación con su identificador original. No creamos otra acción si su resultado es incierto.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                .listRowBackground(Color.clear)
                ForEach(store.pendingActions.values.sorted { $0.key < $1.key }, id: \.requestID) { action in
                    Section(action.kind == .createSession ? "Crear sesión" : action.kind == .cancelTurn ? "Detener turno" : "Mensaje pendiente") {
                        if let prompt = action.prompt { Text(String(prompt.prefix(8_000))).font(.callout).textSelection(.enabled) }
                        if let count = action.attachmentIDs?.count, count > 0 { Text("\(count) adjuntos incluidos").font(.caption) }
                        Button("Reintentar operación original", systemImage: "arrow.clockwise") { Task { await store.retryPendingAction(action) } }
                            .frame(minHeight: 44)
                            .disabled(store.phase != .connected || store.actionIsBusy(action.key))
                        if action.kind != .createSession {
                            Button("Abrir sesión", systemImage: "bubble.left") { store.navigationSessionID = action.resourceID; dismiss() }
                                .frame(minHeight: 44)
                        }
                    }
                    .listRowBackground(RemoteTheme.surface)
                }
                ForEach(store.pendingMutations.values.sorted { $0.label < $1.label }) { mutation in
                    Section(mutation.label) {
                        Text(mutation.path.dropFirst().joined(separator: "/")).font(.caption.monospaced()).textSelection(.enabled)
                        if let rejection = mutation.rejectionCode { Text(rejection).font(.caption).foregroundStyle(.orange) }
                        DisclosureGroup("Datos del envío original") {
                            Text(mutation.previewText).font(.caption.monospaced()).textSelection(.enabled)
                        }
                        Button("Reintentar envío original", systemImage: "arrow.clockwise") { Task { await store.retryMutation(mutation) } }
                            .frame(minHeight: 44)
                            .disabled(store.phase != .connected || store.actionIsBusy(mutation.key))
                        if mutation.canDiscard { Button("Descartar intento rechazado", role: .destructive) { discarding = mutation } }
                    }
                    .listRowBackground(RemoteTheme.surface)
                }
                if store.pendingMutations.isEmpty && store.pendingActions.isEmpty {
                    RemoteEmptyState(symbol: "checkmark.circle", title: "Todo confirmado", message: "No hay envíos pendientes de confirmar con tu PC.")
                        .listRowBackground(Color.clear)
                }
            }
            .scrollContentBackground(.hidden)
            .remoteCanvas()
            .navigationTitle("Envíos pendientes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .confirmationDialog("¿Descartar el intento rechazado?", isPresented: Binding(get: { discarding != nil }, set: { if !$0 { discarding = nil } })) {
                Button("Descartar intento", role: .destructive) { if let discarding { store.discardRejectedMutation(discarding) }; discarding = nil }
            } message: {
                Text("La PC confirmó que no aplicó este cambio porque el archivo o Git cambió. Volvé a cargar su estado antes de preparar otra acción.")
            }
        }
    }
}
