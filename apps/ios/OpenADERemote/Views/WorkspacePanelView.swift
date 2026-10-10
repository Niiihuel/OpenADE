import SwiftUI

enum WorkspaceTab: String, CaseIterable, Identifiable {
    case changes, files, activity
    var id: String { rawValue }
    var title: String {
        switch self { case .changes: "Cambios"; case .files: "Archivos"; case .activity: "Actividad" }
    }
    var icon: String {
        switch self { case .changes: "point.3.connected.trianglepath.dotted"; case .files: "folder"; case .activity: "waveform.path" }
    }
}

struct WorkspacePanelView: View {
    let projectID: String
    let sessionID: String
    var onClose: (() -> Void)?
    var onComment: ((String) -> Void)?
    @Environment(RemoteStore.self) private var store
    @State private var tab: WorkspaceTab = .changes

    init(projectID: String, sessionID: String, onClose: (() -> Void)? = nil,
         onComment: ((String) -> Void)? = nil, initialTab: WorkspaceTab = .changes) {
        self.projectID = projectID; self.sessionID = sessionID; self.onClose = onClose; self.onComment = onComment
        _tab = State(initialValue: initialTab)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Picker("Vista del proyecto", selection: $tab) {
                    ForEach(WorkspaceTab.allCases.filter { $0 != .files || store.supports("workspace.files.read") }) { item in
                        Text(item.title).tag(item)
                    }
                }.pickerStyle(.segmented)
                if let onClose {
                    Button("Cerrar espacio de trabajo", systemImage: "xmark", action: onClose)
                        .labelStyle(.iconOnly).buttonStyle(.plain).frame(width: 44, height: 44)
                }
            }.padding(12)
            Divider()
            switch tab {
            case .changes:
                if store.supports("workspace.git.read") { GitWorkspaceView(projectID: projectID, onComment: onComment) }
                else { DiffView(projectID: projectID, onComment: onComment) }
            case .files: FilesWorkspaceView(projectID: projectID)
            case .activity:
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        Text("Salida de herramientas · solo lectura").font(.caption).foregroundStyle(.secondary)
                        let activity = store.entries(sessionID: sessionID).filter { $0.kind == .tool || $0.kind == .notice }
                        if activity.isEmpty {
                            RemoteEmptyState(symbol: "waveform.path", title: "Sin actividad todavía", message: "Las acciones del agente y su salida aparecerán acá.")
                        }
                        ForEach(activity) { ConversationEntryView(entry: $0) }
                    }.padding()
                }
            }
        }.background(RemoteTheme.canvas).tint(RemoteTheme.ink)
    }
}

struct WorkspaceErrorView: View {
    let message: String
    var retry: (() -> Void)?
    var body: some View {
        RemoteCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange)
                        .accessibilityHidden(true)
                    Text(message).font(.callout).fixedSize(horizontal: false, vertical: true)
                }
                if let retry {
                    Button("Reintentar", systemImage: "arrow.clockwise", action: retry)
                        .font(.subheadline.weight(.medium)).frame(minHeight: 44)
                }
            }
        }
    }
}
