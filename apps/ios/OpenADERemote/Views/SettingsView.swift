import SwiftUI

/// Paired PCs, notifications, Face ID and appearance.
struct SettingsView: View {
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @AppStorage("openade.remote.appearance") private var appearance = "system"
    @AppStorage(AppLock.enabledKey) private var lockEnabled = true
    @State private var forgetting: SavedComputer?
    @State private var showingPairing = false
    @State private var changingPush = false
    private var notifications: NativeNotifications { .shared }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(store.library.computers) { computer in
                        Button {
                            store.switchTo(computer.id)
                        } label: {
                            ComputerRow(computer: computer, active: computer.id == store.library.activeID)
                        }
                        .buttonStyle(.plain)
                        .swipeActions { Button("Olvidar", role: .destructive) { forgetting = computer } }
                        .contextMenu {
                            Button("Olvidar esta PC", systemImage: "trash", role: .destructive) { forgetting = computer }
                        }
                    }
                    Button("Emparejar otra PC", systemImage: "qrcode.viewfinder") { showingPairing = true }
                        .frame(minHeight: 44)
                } header: {
                    Text("Computadoras")
                } footer: {
                    Text("Tocá una PC para usarla. Cada una tiene su propio acceso en el llavero de este iPhone.")
                }

                if store.server?.push == true {
                    Section {
                        Toggle("Avisarme en este iPhone", isOn: Binding(
                            get: { store.activeComputer?.notificationsEnabled == true },
                            set: { enabled in Task { await setNotifications(enabled) } }
                        ))
                        .disabled(changingPush || !store.isOnline)
                        if let error = store.pushError ?? notifications.error {
                            Text(error).font(.footnote).foregroundStyle(RemoteTheme.attention)
                        }
                    } header: {
                        Text("Avisos de \(store.computerName)")
                    } footer: {
                        Text("Cuando una tarea necesita tu permiso, te hace una pregunta o termina. Solo llevan el título de la tarea, y la PC no avisa mientras la estás usando.")
                    }
                }

                Section {
                    Toggle("Bloquear con Face ID", isOn: $lockEnabled)
                } footer: {
                    Text("Pide Face ID o el código del iPhone al abrir la app y al volver después de 5 minutos.")
                }

                Section("Apariencia") {
                    Picker("Tema", selection: $appearance) {
                        Text("Del sistema").tag("system")
                        Text("Oscuro").tag("dark")
                        Text("Claro").tag("light")
                    }
                }

                Section {
                    LabeledContent("Versión", value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
                }
            }
            .scrollContentBackground(.hidden)
            .remoteCanvas()
            .navigationTitle("Ajustes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .sheet(isPresented: $showingPairing) { PairingSheet().remoteSheet() }
            .confirmationDialog("¿Olvidar \(forgetting?.name ?? "esta PC")?", isPresented: Binding(
                get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }
            ), titleVisibility: .visible) {
                Button("Olvidar", role: .destructive) {
                    if let forgetting { store.forget(forgetting.id) }
                    forgetting = nil
                }
            } message: {
                Text("Se borran su acceso y lo guardado en este iPhone. La PC sigue igual; para quitarle el acceso a este teléfono, revocalo en openADE → Ajustes → Remoto.")
            }
        }
    }

    private func setNotifications(_ enabled: Bool) async {
        changingPush = true
        defer { changingPush = false }
        if enabled, !(await notifications.requestAccess()) { return }
        await store.setNotifications(enabled, token: notifications.token)
    }
}

private struct ComputerRow: View {
    let computer: SavedComputer
    let active: Bool
    @Environment(RemoteStore.self) private var store

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "desktopcomputer")
                .font(.title3)
                .frame(width: 42, height: 42)
                .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 12))
            VStack(alignment: .leading, spacing: 3) {
                Text(computer.name).font(.body.weight(.medium)).foregroundStyle(RemoteTheme.ink)
                Text(status).font(.caption).foregroundStyle(computer.revoked ? Color.red : RemoteTheme.muted).lineLimit(1)
                Text("Código \(computer.shortFingerprint)").font(.caption2.monospaced()).foregroundStyle(RemoteTheme.muted)
            }
            Spacer(minLength: 6)
            if active { Image(systemName: "checkmark").font(.body.weight(.semibold)).accessibilityLabel("PC en uso") }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var status: String {
        if computer.revoked { return "Acceso revocado" }
        let route = computer.lastRoute.map(RemoteRoute.display)
        guard active else { return route.map { "Última vez por \($0)" } ?? "Guardada" }
        switch store.connection {
        case .online: return route.map { "Conectada · \($0)" } ?? "Conectada"
        case .connecting, .reconnecting: return "Conectando…"
        case .offline: return "Sin conexión"
        case .revoked: return "Acceso revocado"
        case .idle: return "Guardada"
        }
    }
}
