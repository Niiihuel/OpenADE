import SwiftUI

enum ComposerPanel: Hashable { case permissions, intelligence }

struct ComposerAnchorKey: PreferenceKey {
    static var defaultValue: [ComposerPanel: Anchor<CGRect>] { [:] }
    static func reduce(value: inout [ComposerPanel: Anchor<CGRect>], nextValue: () -> [ComposerPanel: Anchor<CGRect>]) {
        value.merge(nextValue(), uniquingKeysWith: { _, new in new })
    }
}

private struct ComposerPanelHeightKey: PreferenceKey {
    static var defaultValue: CGFloat { 0 }
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

/// Kept in the composer's view tree so opening a control never resigns the
/// draft's first responder. Content scrolls when the keyboard or large text
/// leaves less room, instead of covering the keyboard or clipping choices.
struct ComposerFloatingPanel<Content: View>: View {
    let width: CGFloat
    let maxHeight: CGFloat
    var arrowX: CGFloat? = nil
    @ViewBuilder var content: () -> Content
    @State private var contentHeight: CGFloat = 1

    var body: some View {
        ScrollView {
            content()
                .frame(maxWidth: .infinity)
                .background {
                    GeometryReader { geometry in
                        Color.clear.preference(key: ComposerPanelHeightKey.self, value: geometry.size.height)
                    }
                }
        }
        .scrollBounceBehavior(.basedOnSize)
        .scrollDismissesKeyboard(.never)
        .scrollIndicators(.hidden)
        .frame(width: width, height: min(contentHeight, maxHeight))
        .onPreferenceChange(ComposerPanelHeightKey.self) { contentHeight = max(1, $0) }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 28).strokeBorder(RemoteTheme.border.opacity(0.8), lineWidth: 0.5) }
        .overlay(alignment: .bottomLeading) {
            if let arrowX {
                ComposerPopoverArrow().fill(.regularMaterial)
                    .frame(width: 22, height: 11)
                    .offset(x: max(20, min(arrowX - 11, width - 42)), y: 10)
                    .accessibilityHidden(true)
            }
        }
        .shadow(color: .black.opacity(0.2), radius: 18, y: 6)
    }
}

private struct ComposerPopoverArrow: Shape {
    func path(in rect: CGRect) -> Path {
        Path { path in
            path.move(to: CGPoint(x: rect.minX, y: rect.minY))
            path.addLine(to: CGPoint(x: rect.midX - 3, y: rect.maxY - 2))
            path.addQuadCurve(to: CGPoint(x: rect.midX + 3, y: rect.maxY - 2),
                              control: CGPoint(x: rect.midX, y: rect.maxY + 1))
            path.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
            path.closeSubpath()
        }
    }
}

struct ComposerPermissionsPicker: View {
    @Binding var options: RemoteTurnOptions
    var disabled = false
    let mode: TurnMode
    let pendingCount: Int
    let onSelected: () -> Void
    let onReview: () -> Void
    @Environment(RemoteStore.self) private var store
    @State private var showingExplanation = false
    private var selected: RemoteApprovalPolicy { options.approvalPolicy ?? .ask }
    private var canChange: Bool { store.supports("turn.approval_policy") && !disabled }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("¿Cómo deben aprobarse las acciones?")
                .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                .fixedSize(horizontal: false, vertical: true)
            Button { showingExplanation = true } label: {
                Text("Obtener más información").underline().font(.subheadline)
                    .foregroundStyle(RemoteTheme.muted).frame(minHeight: 32, alignment: .leading)
            }.buttonStyle(.plain)
            VStack(spacing: 4) {
                policyRow(.ask, title: "Pedir aprobación", symbol: "hand.raised",
                          detail: "Pregunta antes de editar archivos o ejecutar comandos.")
                policyRow(.auto, title: "Aprueba por mí", symbol: "checkmark.shield",
                          detail: "Permite ediciones del proyecto; pregunta para comandos y borrados.")
                policyRow(.fullAccess, title: "Acceso completo", symbol: "exclamationmark.shield",
                          detail: "Ejecuta sin pedir aprobación dentro de lo habilitado por esta PC.")
            }
            if mode == .plan {
                Text("En planificación, las herramientas permanecen en modo de solo lectura.")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.top, 4)
            }
            if !store.supports("turn.approval_policy") {
                Text("Esta PC usa Pedir aprobación. Actualizá su servicio para elegir otra opción.")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.top, 4)
            } else if disabled {
                Text("El turno actual conserva sus permisos. Elegí los del próximo mensaje cuando termine y se confirmen los envíos pendientes.")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.top, 4)
            }
            if pendingCount > 0 {
                Divider().padding(.vertical, 4)
                Button(action: onReview) {
                    Label("Revisar acciones pendientes (\(pendingCount))", systemImage: "hand.raised")
                        .font(.subheadline).frame(minHeight: 44)
                }.buttonStyle(.plain)
            }
        }
        .padding(18)
        .foregroundStyle(RemoteTheme.ink)
        .sheet(isPresented: $showingExplanation) { PermissionPolicyExplanationView() }
    }

    private func policyRow(_ policy: RemoteApprovalPolicy, title: String, symbol: String, detail: String) -> some View {
        Button {
            guard canChange else { return }
            options.approvalPolicy = policy
            onSelected()
        } label: {
            HStack(alignment: .center, spacing: 12) {
                Image(systemName: symbol).font(.system(size: 21, weight: .regular))
                    .symbolRenderingMode(.monochrome).frame(width: 26)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.body)
                    Text(detail).font(.caption).foregroundStyle(RemoteTheme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                Image(systemName: "checkmark").font(.body.weight(.medium))
                    .opacity(selected == policy ? 1 : 0).frame(width: 20)
            }.padding(.vertical, 10).frame(minHeight: 44).contentShape(Rectangle())
        }
        .buttonStyle(.plain).disabled(!canChange)
        .opacity(canChange || selected == policy ? 1 : 0.5)
        .accessibilityAddTraits(selected == policy ? [.isSelected] : [])
    }
}

private struct PermissionPolicyExplanationView: View {
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    explanation("Pedir aprobación", "Podés leer y explorar el proyecto. Las ediciones y los comandos necesitan tu aprobación antes de ejecutarse.")
                    explanation("Aprueba por mí", "Autoriza ediciones de texto dentro del proyecto. Los comandos, borrados y movimientos siguen pidiendo aprobación. Las herramientas no habilitadas continúan bloqueadas.")
                    explanation("Acceso completo", "Las herramientas habilitadas en la PC pueden actuar sin confirmaciones durante el próximo turno. Esto incluye cambios y comandos: elegilo solo si querés delegar esas acciones.")
                    Text("La selección se aplica al próximo mensaje. No modifica un turno en curso, los límites configurados en la PC ni las restricciones de planificación.")
                        .font(.footnote).foregroundStyle(RemoteTheme.muted)
                }.padding(22)
            }
            .navigationTitle("Permisos").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
        }.presentationDetents([.medium, .large]).remoteSheet()
    }
    private func explanation(_ title: String, _ text: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            Text(text).font(.subheadline).foregroundStyle(RemoteTheme.muted)
        }
    }
}

struct ComposerActionRow: View {
    let title: String
    let symbol: String
    var subtitle: String? = nil
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 16) {
                Image(systemName: symbol)
                    .font(.system(size: 22, weight: .regular))
                    .symbolRenderingMode(.monochrome)
                    .frame(width: 50, height: 50)
                    .background(RemoteTheme.raised, in: Circle())
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.body)
                    if let subtitle {
                        Text(subtitle).font(.caption).foregroundStyle(RemoteTheme.muted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 8)
                if selected { Image(systemName: "checkmark").font(.body.weight(.medium)) }
            }
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(RemoteTheme.ink)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

struct ComposerToolButton: View {
    let title: String
    let symbol: String
    var selected = false
    let action: () -> Void
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 21, weight: .regular))
                .symbolRenderingMode(.monochrome)
                .frame(width: 44, height: 44)
                .background(selected ? RemoteTheme.raised : .clear, in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(RemoteTheme.ink.opacity(isEnabled ? 1 : 0.35))
        .accessibilityLabel(title)
        .help(title)
    }
}

struct SessionPermissionsView: View {
    let sessionID: String
    let mode: TurnMode
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss

    private var permissions: [RemotePermission] { store.session(id: sessionID)?.pendingPermissions ?? [] }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Revisá las acciones pendientes").font(.title2.weight(.medium))
                        Text("La PC solicita confirmación para las acciones protegidas. Cada aprobación corresponde únicamente a la acción que revisás.")
                            .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                    }
                    if permissions.isEmpty {
                        Label("No hay acciones esperando tu aprobación.", systemImage: "checkmark.circle")
                            .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                        RemoteCard {
                            Label(mode == .plan ? "Modo de planificación" : "Modo de construcción",
                                  systemImage: mode == .plan ? "list.bullet.clipboard" : "hammer")
                                .font(.headline)
                            Text(mode == .plan ? "El agente analiza el proyecto sin editar archivos ni ejecutar comandos." : "El agente puede proponer cambios y ejecutar las acciones permitidas por esta PC.")
                                .font(.subheadline).foregroundStyle(RemoteTheme.muted).padding(.top, 5)
                        }
                    } else {
                        ForEach(permissions) { permission in
                            PermissionReviewCard(sessionID: sessionID, permission: permission)
                        }
                    }
                }
                .padding(20)
            }
            .navigationTitle("Acciones pendientes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .background(RemoteTheme.canvas)
        }
        .presentationDetents([.medium, .large])
        .remoteSheet()
    }
}

private struct PermissionReviewCard: View {
    let sessionID: String
    let permission: RemotePermission
    @Environment(RemoteStore.self) private var store
    @State private var expanded = true

    var body: some View {
        RemoteCard {
            VStack(alignment: .leading, spacing: 16) {
                Label("Necesita tu aprobación", systemImage: "hand.raised")
                    .font(.headline)
                Text(permission.toolName).font(.subheadline.weight(.semibold)).textSelection(.enabled)
                if permission.argumentsTruncated == true {
                    Label("La acción supera el tamaño de esta vista. Revisala en la PC o denegala.", systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(.orange)
                }
                DisclosureGroup("Ver la acción completa", isExpanded: $expanded) {
                    ScrollView(.horizontal) {
                        Text(permission.arguments.displayText)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.top, 8)
                }
                VStack(spacing: 8) {
                    Button("Aprobar esta acción") { resolve(true) }
                        .buttonStyle(RemotePrimaryButtonStyle())
                        .disabled(permission.argumentsTruncated == true)
                    Button("Denegar", role: .destructive) { resolve(false) }
                        .frame(maxWidth: .infinity, minHeight: 44)
                    if store.actionIsBusy("permission:\(permission.id)") { ProgressView() }
                }
                .disabled(store.actionIsBusy("permission:\(permission.id)") || store.phase != .connected || store.session(id: sessionID)?.canControlActive == false)
            }
        }
    }

    private func resolve(_ allow: Bool) {
        Task { await store.resolvePermission(sessionID: sessionID, permission: permission, allow: allow) }
    }
}
