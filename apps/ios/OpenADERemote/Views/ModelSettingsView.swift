import SwiftUI

/// Compact control placed by the conversation beside its composer.
/// It does not present a modal or change keyboard focus.
struct QuickModelPickerView: View {
    @Binding var options: RemoteTurnOptions
    var disabled = false
    let onAdvanced: () -> Void
    @Environment(RemoteStore.self) private var store

    private var selection: ModelPickerSelection { ModelPickerSelection(options: options, store: store, disabled: disabled) }

    var body: some View {
        VStack(spacing: 16) {
            Button(action: onAdvanced) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 6) {
                        Text(selection.name).font(.title3.weight(.medium))
                        Text(effortTitle(options.reasoningEffort)).font(.title3)
                            .foregroundStyle(RemoteTheme.muted)
                        Image(systemName: "chevron.right").font(.body.weight(.medium))
                    }.fixedSize(horizontal: true, vertical: false)
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(selection.name).font(.headline)
                            Text(effortTitle(options.reasoningEffort)).font(.subheadline).foregroundStyle(RemoteTheme.muted)
                        }.fixedSize(horizontal: false, vertical: true)
                        Image(systemName: "chevron.right").font(.body.weight(.medium))
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Avanzado, \(selection.name), inteligencia \(effortTitle(options.reasoningEffort))")
            if !selection.efforts.isEmpty {
                effortCapsule
            } else if store.modelCatalog == nil {
                ProgressView().frame(maxWidth: .infinity, minHeight: 68)
                    .accessibilityLabel("Cargando modelos de la PC")
            } else {
                Text("Inteligencia automática").font(.subheadline).foregroundStyle(RemoteTheme.muted)
                    .frame(maxWidth: .infinity, minHeight: 54)
                    .background(RemoteTheme.raised.opacity(0.75), in: Capsule())
                    .accessibilityHint("Abrí Avanzado para revisar el modelo")
            }
        }
        .foregroundStyle(RemoteTheme.ink)
        .task { await store.loadModelCatalog() }
    }

    private var effortCapsule: some View {
        GeometryReader { geometry in
            let count = selection.efforts.count + 1
            let cellWidth = max(48, (geometry.size.width - 16) / CGFloat(count))
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 0) {
                    effortDot(nil).frame(width: cellWidth)
                    ForEach(selection.efforts, id: \.self) { effort in
                        effortDot(effort).frame(width: cellWidth)
                    }
                }.padding(.horizontal, 8)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
            .frame(height: 68)
            .background(.ultraThinMaterial, in: Capsule())
            .background(RemoteTheme.raised.opacity(0.6), in: Capsule())
            .overlay { Capsule().strokeBorder(RemoteTheme.border.opacity(0.8), lineWidth: 0.75) }
        }
        .frame(height: 68)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Nivel de inteligencia")
    }

    private func effortDot(_ effort: String?) -> some View {
        Button {
            guard selection.canSelect else { return }
            options.reasoningEffort = effort
        } label: {
            ZStack {
                if options.reasoningEffort == effort {
                    Circle().fill(RemoteTheme.canvas.opacity(0.85)).frame(width: 44, height: 44)
                    Circle().strokeBorder(RemoteTheme.ink, lineWidth: 3.5).frame(width: 44, height: 44)
                } else {
                    Circle().fill(RemoteTheme.muted.opacity(0.75)).frame(width: 10, height: 10)
                }
            }
            .frame(maxWidth: .infinity, minHeight: 68)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!selection.canSelect)
        .accessibilityLabel("Inteligencia \(effortTitle(effort))")
        .accessibilityAddTraits(options.reasoningEffort == effort ? [.isSelected] : [])
    }
}

/// A compact native sheet. Model and effort choices come only from the host's
/// verified catalog; speed remains informative until the host offers a control.
struct AdvancedModelSettingsView: View {
    @Binding var options: RemoteTurnOptions
    var disabled = false
    @Environment(RemoteStore.self) private var store
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var contentHeight: CGFloat = 316

    private var selection: ModelPickerSelection { ModelPickerSelection(options: options, store: store, disabled: disabled) }
    private var detents: Set<PresentationDetent> {
        dynamicTypeSize.isAccessibilitySize ? [.large] : [.height(min(max(contentHeight, 300), 550)), .large]
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 26) {
                HStack(spacing: 8) {
                    Text("Avanzado").font(.body.weight(.medium))
                    Image(systemName: "chevron.right").font(.body.weight(.medium)).accessibilityHidden(true)
                }
                .frame(maxWidth: .infinity, minHeight: 40)
                .accessibilityAddTraits(.isHeader)

                VStack(spacing: 0) {
                    modelRow
                    Divider().overlay(RemoteTheme.border.opacity(0.8))
                    intelligenceRow
                }
                .padding(.horizontal, 18).padding(.vertical, 4)
                .background(RemoteTheme.raised.opacity(0.9), in: RoundedRectangle(cornerRadius: 24))

                settingsRow(title: "Velocidad") {
                    Text("Normal").foregroundStyle(RemoteTheme.muted)
                }
                .padding(.horizontal, 18).padding(.vertical, 4)
                .background(RemoteTheme.raised.opacity(0.9), in: RoundedRectangle(cornerRadius: 24))
                .accessibilityElement(children: .combine)
                .accessibilityHint("La PC usa la velocidad normal del proveedor")
            }
            .padding(.horizontal, 20)
            .padding(.top, 18)
            .padding(.bottom, 28)
            .background {
                GeometryReader { geometry in
                    Color.clear.preference(key: ModelSettingsHeightKey.self, value: geometry.size.height)
                }
            }
        }
        .scrollBounceBehavior(.basedOnSize)
        .foregroundStyle(RemoteTheme.ink)
        .tint(RemoteTheme.ink)
        .background(RemoteTheme.surface.opacity(0.45))
        .presentationBackground(.regularMaterial)
        .presentationCornerRadius(32)
        .presentationDragIndicator(.visible)
        .presentationDetents(detents)
        .onPreferenceChange(ModelSettingsHeightKey.self) { height in
            if height.isFinite, height > 0, abs(height - contentHeight) > 1 { contentHeight = height }
        }
        .task { await store.loadModelCatalog() }
    }

    private var modelRow: some View {
        settingsRow(title: "Modelo") {
            if selection.canSelect, !selection.models.isEmpty {
                Menu {
                    ForEach(selection.models) { model in
                        Button {
                            guard selection.canSelect else { return }
                            options.modelID = model.id
                            if let effort = options.reasoningEffort, !(model.reasoningEfforts ?? []).contains(effort) {
                                options.reasoningEffort = nil
                            }
                        } label: {
                            if model.id == selection.model?.id { Label(model.name, systemImage: "checkmark") }
                            else { Text(model.name) }
                        }
                    }
                } label: { menuValue(selection.name) }
                    .accessibilityLabel("Modelo, \(selection.name)")
            } else {
                Text(selection.name).foregroundStyle(RemoteTheme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var intelligenceRow: some View {
        settingsRow(title: "Inteligencia") {
            if selection.canSelect, !selection.efforts.isEmpty {
                Menu {
                    Button {
                        guard selection.canSelect else { return }
                        options.reasoningEffort = nil
                    } label: {
                        if options.reasoningEffort == nil { Label("Automática", systemImage: "checkmark") }
                        else { Text("Automática") }
                    }
                    ForEach(selection.efforts, id: \.self) { effort in
                        Button {
                            guard selection.canSelect else { return }
                            options.reasoningEffort = effort
                        } label: {
                            if options.reasoningEffort == effort { Label(effortTitle(effort), systemImage: "checkmark") }
                            else { Text(effortTitle(effort)) }
                        }
                    }
                } label: { menuValue(effortTitle(options.reasoningEffort)) }
                    .accessibilityLabel("Inteligencia, \(effortTitle(options.reasoningEffort))")
            } else {
                Text(effortTitle(options.reasoningEffort)).foregroundStyle(RemoteTheme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func settingsRow<Content: View>(title: String, @ViewBuilder value: () -> Content) -> some View {
        Group {
            if dynamicTypeSize.isAccessibilitySize {
                VStack(alignment: .leading, spacing: 8) {
                    Text(title)
                    value()
                }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 12)
            } else {
                HStack(spacing: 16) {
                    Text(title)
                    Spacer(minLength: 0)
                    value().multilineTextAlignment(.trailing)
                }
                .frame(minHeight: 54)
            }
        }
    }

    private func menuValue(_ text: String) -> some View {
        HStack(spacing: 8) {
            Text(text).fixedSize(horizontal: false, vertical: true)
            Image(systemName: "chevron.up.chevron.down").font(.caption)
        }
        .foregroundStyle(RemoteTheme.muted)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
    }
}

/// Compatibility for callers that have not yet adopted the anchored quick view.
struct ModelCatalogView: View {
    @Binding var options: RemoteTurnOptions
    var disabled = false
    var initialAdvanced = false
    @State private var advanced = false

    var body: some View {
        Group {
            if advanced || initialAdvanced {
                AdvancedModelSettingsView(options: $options, disabled: disabled)
            } else {
                QuickModelPickerView(options: $options, disabled: disabled) { advanced = true }
                    .padding(24)
                    .presentationBackground(.regularMaterial)
                    .presentationCornerRadius(32)
                    .presentationDragIndicator(.visible)
                    .presentationDetents([.height(196)])
            }
        }
    }
}

private struct ModelPickerSelection {
    let options: RemoteTurnOptions
    let store: RemoteStore
    let disabled: Bool

    @MainActor var models: [RemoteModel] { store.modelCatalog?.models ?? [] }
    @MainActor var model: RemoteModel? {
        let id = options.modelID ?? store.modelCatalog?.defaultModelID ?? store.health?.model
        return models.first { $0.id == id }
    }
    @MainActor var name: String { model?.name ?? options.modelID ?? store.health?.model ?? "Modelo de la PC" }
    @MainActor var efforts: [String] { model?.reasoningEfforts ?? [] }
    @MainActor var canSelect: Bool {
        store.supports("turn.options") && store.modelCatalog?.selectionEnabled == true && !disabled
    }
}

private struct ModelSettingsHeightKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

func effortTitle(_ effort: String?) -> String {
    switch effort {
    case nil: "Automática"
    case "none": "Sin razonamiento"
    case "minimal": "Mínima"
    case "low": "Baja"
    case "medium": "Media"
    case "high": "Alta"
    case "xhigh": "Muy alta"
    case "max": "Máxima"
    case "ultra": "Ultra"
    default: effort?.capitalized ?? "Automática"
    }
}
