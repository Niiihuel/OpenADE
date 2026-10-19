import SwiftUI

/// Shared grayscale surfaces. Color is reserved for meaning (attention, errors and diffs).
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
    /// "The agent needs you": a permission or a question waits.
    static let attention = Color.orange

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

/// A quieter capsule button that sits next to a primary one.
struct RemoteSecondaryButtonStyle: ButtonStyle {
    var role: ButtonRole? = nil
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.medium))
            .frame(maxWidth: .infinity, minHeight: 46)
            .foregroundStyle((role == .destructive ? Color.red : RemoteTheme.ink).opacity(isEnabled ? 1 : 0.4))
            .background(RemoteTheme.raised.opacity(configuration.isPressed ? 0.6 : 1), in: Capsule())
            .contentShape(Capsule())
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
                .font(.system(size: 20, weight: .regular))
                .symbolRenderingMode(.monochrome)
                .frame(width: 40, height: 40)
                .background(selected ? RemoteTheme.raised : .clear, in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(RemoteTheme.ink.opacity(isEnabled ? 1 : 0.35))
        .accessibilityLabel(title)
        .help(title)
    }
}

/// A capsule label: an agent's name, a count.
struct RemoteTag: View {
    let title: String
    var symbol: String? = nil
    var body: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol).imageScale(.small) }
            Text(title).lineLimit(1)
        }
        .font(.caption2.weight(.medium))
        .foregroundStyle(RemoteTheme.muted)
        .padding(.horizontal, 7).padding(.vertical, 3)
        .background(RemoteTheme.raised, in: Capsule())
    }
}

/// A floating pill for every state but online (like t3code): the UI stays
/// on screen with what it has while the phone reconnects.
struct ConnectionPill: View {
    @Environment(RemoteStore.self) private var store
    var onRevoked: (() -> Void)? = nil

    var body: some View {
        ZStack {
            if let content { pill(content) }
        }
        .animation(.snappy, value: store.connection)
    }

    private func pill(_ content: (title: String, symbol: String, spinning: Bool)) -> some View {
        Button {
                if store.connection == .revoked { onRevoked?() } else { store.reconnect() }
            } label: {
                HStack(spacing: 8) {
                    if content.spinning { ProgressView().controlSize(.mini) }
                    else { Image(systemName: content.symbol).font(.caption.weight(.semibold)) }
                    Text(content.title).font(.footnote.weight(.medium)).lineLimit(1)
                }
                .foregroundStyle(store.connection == .revoked ? Color.red : RemoteTheme.ink)
                .padding(.horizontal, 14).frame(minHeight: 34)
                .background(.regularMaterial, in: Capsule())
                .overlay { Capsule().strokeBorder(RemoteTheme.border.opacity(0.8), lineWidth: 0.5) }
                .shadow(color: .black.opacity(0.16), radius: 10, y: 3)
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
            .accessibilityLabel(content.title)
            .accessibilityHint(store.connection == .revoked ? "Abre el emparejamiento" : "Reintenta la conexión")
            .transition(.move(edge: .top).combined(with: .opacity))
    }

    private var content: (title: String, symbol: String, spinning: Bool)? {
        switch store.connection {
        case .online, .idle: return nil
        case .connecting: return ("Conectando con \(store.computerName)…", "", true)
        case .reconnecting: return ("Reconectando…", "", true)
        case .offline:
            return (store.hasSnapshot ? "Sin conexión · mostrando lo guardado" : "Sin conexión con \(store.computerName)", "wifi.slash", false)
        case .revoked: return ("Acceso revocado", "lock.slash", false)
        }
    }
}

enum Haptics {
    static func success() { UINotificationFeedbackGenerator().notificationOccurred(.success) }
    static func warning() { UINotificationFeedbackGenerator().notificationOccurred(.warning) }
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

    /// The pill takes its own room at the top, so it never hides a row.
    func connectionPill(onRevoked: (() -> Void)? = nil) -> some View {
        safeAreaInset(edge: .top, spacing: 0) {
            ConnectionPill(onRevoked: onRevoked)
        }
    }
}
