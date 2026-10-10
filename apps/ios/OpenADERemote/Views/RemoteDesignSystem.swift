import SwiftUI

/// Shared grayscale surfaces. Color is reserved for meaning (errors and diffs).
enum RemoteTheme {
    static let canvas = adaptive(light: 0.98, dark: 0.015)
    static let surface = adaptive(light: 1, dark: 0.085)
    static let raised = adaptive(light: 0.94, dark: 0.145)
    static let border = adaptive(light: 0.87, dark: 0.21)
    // Explicit adaptive colors avoid hierarchical/vibrant foregrounds becoming
    // transparent inside material overlays or native glass toolbar buttons.
    static let muted = adaptive(light: 0.40, dark: 0.68)
    static let ink = adaptive(light: 0.07, dark: 0.96)
    static let accent = ink
    static let onAccent = adaptive(light: 1, dark: 0.07)

    private static func adaptive(light: CGFloat, dark: CGFloat) -> Color {
        Color(uiColor: UIColor { traits in
            UIColor(white: traits.userInterfaceStyle == .dark ? dark : light, alpha: 1)
        })
    }
}

struct RemoteCard<Content: View>: View {
    var padding: CGFloat = 16
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) { content() }
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
            .overlay { RoundedRectangle(cornerRadius: 24, style: .continuous).strokeBorder(RemoteTheme.border.opacity(0.65), lineWidth: 0.5) }
    }
}

struct RemoteIconButton: View {
    let title: String
    let symbol: String
    let action: () -> Void
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 19, weight: .regular))
                .symbolRenderingMode(.monochrome)
                .frame(width: 44, height: 44)
                .background(RemoteTheme.surface, in: Circle())
                .overlay { Circle().strokeBorder(RemoteTheme.border.opacity(0.65), lineWidth: 0.5) }
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(RemoteTheme.ink.opacity(isEnabled ? 1 : 0.35))
        .accessibilityLabel(title)
        .help(title)
    }
}

struct RemoteEmptyState: View {
    let symbol: String
    let title: String
    let message: String

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 32, weight: .light))
                .symbolRenderingMode(.monochrome)
                .padding(.bottom, 8)
                .accessibilityHidden(true)
            Text(title).font(.title3.weight(.medium))
            Text(message).font(.subheadline).foregroundStyle(RemoteTheme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .multilineTextAlignment(.center)
        .padding(28)
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }
}

struct RemoteChip: View {
    let title: String
    var symbol: String? = nil
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 7) {
                if let symbol { Image(systemName: symbol).symbolRenderingMode(.monochrome) }
                Text(title).lineLimit(1)
            }
            .font(.subheadline.weight(selected ? .medium : .regular))
            .padding(.horizontal, 16)
            .frame(minHeight: 44)
            .background(selected ? RemoteTheme.raised : Color.clear, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .foregroundStyle(selected ? RemoteTheme.ink : RemoteTheme.muted)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

struct RemotePrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .frame(maxWidth: .infinity, minHeight: 50)
            .foregroundStyle(RemoteTheme.onAccent)
            .background(RemoteTheme.accent.opacity(isEnabled ? (configuration.isPressed ? 0.7 : 1) : 0.25), in: Capsule())
            .contentShape(Capsule())
    }
}

struct RemoteSearchField: View {
    @Binding var text: String
    let prompt: String
    var autoFocus = false
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass").foregroundStyle(RemoteTheme.muted)
            TextField(prompt, text: $text)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .submitLabel(.search).focused($focused)
                .onSubmit { focused = false }
                .accessibilityLabel(prompt)
            if !text.isEmpty {
                Button { text = "" } label: { Image(systemName: "xmark.circle.fill").frame(width: 44, height: 44) }
                    .buttonStyle(.plain).foregroundStyle(RemoteTheme.muted).accessibilityLabel("Borrar búsqueda")
            }
        }.padding(.leading, 16).padding(.trailing, text.isEmpty ? 16 : 2)
            .frame(minHeight: 50)
            .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 18))
            .overlay { RoundedRectangle(cornerRadius: 18).strokeBorder(RemoteTheme.border, lineWidth: 0.5) }
            .task { if autoFocus { focused = true } }
    }
}

extension View {
    func remoteCanvas() -> some View {
        self.background(RemoteTheme.canvas.ignoresSafeArea())
            .toolbarBackground(RemoteTheme.canvas, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .tint(RemoteTheme.ink)
    }

    func remoteSheet() -> some View {
        self.presentationBackground(RemoteTheme.surface)
            .presentationCornerRadius(32)
            .presentationDragIndicator(.visible)
            .tint(RemoteTheme.ink)
    }
}
