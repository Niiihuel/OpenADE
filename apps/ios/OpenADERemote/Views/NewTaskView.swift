import SwiftUI

/// Starts a task on the PC: where, with which agent, building or planning.
struct NewTaskView: View {
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @AppStorage("openade.remote.last-project") private var lastProject = ""
    @AppStorage("openade.remote.last-agent") private var lastAgent = "native"
    @State private var project = ""
    @State private var agent = ""
    @State private var plan = false
    @State private var prompt = ""
    @State private var attachments: [UploadedFile] = []
    @State private var requestID = UUID().uuidString
    @State private var creating = false
    @State private var error: String?

    private var projects: [RemoteProject] { store.snapshot.projects }
    private var agents: [RemoteAgent] { store.snapshot.agents }
    private var ready: Bool { store.isOnline && !project.isEmpty && !agent.isEmpty }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(spacing: 0) {
                        choiceMenu(title: "Proyecto", symbol: "folder", value: store.project(project)?.name ?? "Elegí un proyecto",
                                   detail: store.project(project)?.path) {
                            ForEach(projects) { item in
                                Button { project = item.id } label: {
                                    Text(item.name)
                                    Text(item.path)
                                    if item.id == project { Image(systemName: "checkmark") }
                                }
                            }
                        }
                        Divider().padding(.leading, 56)
                        choiceMenu(title: "Agente", symbol: "cpu", value: agents.first { $0.backend == agent }?.name ?? "Elegí un agente",
                                   detail: nil) {
                            ForEach(agents) { item in
                                Button { agent = item.backend } label: {
                                    if item.backend == agent { Label(item.name, systemImage: "checkmark") } else { Text(item.name) }
                                }
                            }
                        }
                    }
                    .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
                    .overlay { RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(RemoteTheme.border.opacity(0.65), lineWidth: 0.5) }

                    VStack(alignment: .leading, spacing: 8) {
                        Picker("Modo", selection: $plan) {
                            Text("Construir").tag(false)
                            Text("Planear").tag(true)
                        }
                        .pickerStyle(.segmented)
                        Text(plan ? "Analiza y propone un plan, sin cambiar archivos."
                                  : "Edita archivos y ejecuta comandos, y te pide permiso cuando hace falta.")
                            .font(.footnote).foregroundStyle(RemoteTheme.muted)
                    }

                    PromptComposer(text: $prompt, attachments: $attachments, placeholder: "Describí la tarea…",
                                   sending: creating, enabled: ready, lineLimit: 4...12, focusOnAppear: true, onSend: create)

                    if let error { WorkspaceErrorView(message: error) }
                    if projects.isEmpty && store.hasSnapshot {
                        Text("La PC todavía no tiene proyectos abiertos. Abrí uno en openADE para empezar.")
                            .font(.footnote).foregroundStyle(RemoteTheme.muted)
                    }
                }
                .padding(20)
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle("Nueva tarea")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancelar") { dismiss() }.disabled(creating) } }
            .remoteCanvas()
            .interactiveDismissDisabled(creating)
            // The list may arrive after the sheet opens: choose as soon as there is something.
            .onChange(of: projects.map(\.id) + agents.map(\.id), initial: true) {
                if project.isEmpty { project = projects.contains { $0.id == lastProject } ? lastProject : projects.first?.id ?? "" }
                if agent.isEmpty { agent = agents.contains { $0.backend == lastAgent } ? lastAgent : agents.first?.backend ?? "" }
            }
            // The same request id is a retry; anything changed is a new request.
            .onChange(of: "\(prompt)|\(project)|\(agent)|\(plan)|\(attachments.map(\.id))") { requestID = UUID().uuidString }
        }
    }

    private func choiceMenu<Items: View>(title: String, symbol: String, value: String, detail: String?,
                                         @ViewBuilder items: () -> Items) -> some View {
        Menu {
            items()
        } label: {
            HStack(spacing: 14) {
                Image(systemName: symbol).font(.title3).foregroundStyle(RemoteTheme.muted).frame(width: 26)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.caption).foregroundStyle(RemoteTheme.muted)
                    Text(value).font(.body.weight(.medium)).foregroundStyle(RemoteTheme.ink).lineLimit(1)
                    if let detail { Text(detail).font(.caption2).foregroundStyle(RemoteTheme.muted).lineLimit(1).truncationMode(.middle) }
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.up.chevron.down").font(.caption.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
            }
            .padding(.horizontal, 16).padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .accessibilityLabel("\(title): \(value)")
    }

    private func create() {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, ready, !creating else { return }
        creating = true
        error = nil
        let files = attachments.map(\.id)
        let id = requestID
        Task {
            defer { creating = false }
            do {
                _ = try await store.createTask(project: project, backend: agent, prompt: text, files: files, plan: plan, requestID: id)
                lastProject = project
                lastAgent = agent
                Haptics.success()
                dismiss()
            } catch {
                self.error = RemoteStore.readable(error)
                Haptics.warning()
            }
        }
    }
}
