import SwiftUI

enum WorkspaceTab: String, CaseIterable, Identifiable {
    case changes, files
    var id: String { rawValue }
    var title: String { self == .changes ? "Cambios" : "Archivos" }
}

/// A task's project on the PC: its Git changes and its files.
struct WorkspacePanelView: View {
    let projectID: String
    var onComment: ((String) -> Void)?
    @Environment(\.dismiss) private var dismiss
    @State private var tab: WorkspaceTab

    init(projectID: String, initialTab: WorkspaceTab = .changes, onComment: ((String) -> Void)? = nil) {
        self.projectID = projectID
        self.onComment = onComment
        _tab = State(initialValue: initialTab)
    }

    var body: some View {
        NavigationStack {
            Group {
                switch tab {
                case .changes: GitWorkspaceView(projectID: projectID, onComment: onComment)
                case .files: FilesWorkspaceView(projectID: projectID)
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Picker("Vista del proyecto", selection: $tab) {
                        ForEach(WorkspaceTab.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .frame(maxWidth: 240)
                }
                ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } }
            }
            .remoteCanvas()
        }
    }
}

struct WorkspaceErrorView: View {
    let message: String
    var retry: (() -> Void)?
    var body: some View {
        RemoteCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "exclamationmark.triangle").foregroundStyle(RemoteTheme.attention)
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
