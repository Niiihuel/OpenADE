import SwiftUI

@main
struct OpenADERemoteApp: App {
    @UIApplicationDelegateAdaptor(NativeAppDelegate.self) private var appDelegate
    @State private var store = RemoteStore()
    @State private var lock = AppLock()
    @State private var cover = CoverWindow()
    @AppStorage("openade.remote.appearance") private var appearance = "system"
    @Environment(\.scenePhase) private var scenePhase
    private var notifications: NativeNotifications { .shared }

    private enum Cover: Equatable { case none, lock, privacy }
    private var coverState: Cover { lock.isLocked ? .lock : scenePhase == .active ? .none : .privacy }

    var body: some Scene {
        WindowGroup {
            content
                .allowsHitTesting(!lock.isLocked)
                .accessibilityHidden(lock.isLocked)
                .environment(store)
                .tint(RemoteTheme.ink)
                .preferredColorScheme(appearance == "dark" ? .dark : appearance == "light" ? .light : nil)
                .task {
                    store.restore()
                    lock.launch(paired: store.isPaired)
                    store.setForeground(scenePhase == .active)
                }
                .onChange(of: scenePhase) { _, phase in
                    switch phase {
                    case .active:
                        lock.becameActive(paired: store.isPaired)
                        store.setForeground(true)
                    case .background:
                        lock.enteredBackground()
                        store.setForeground(false)
                    default:
                        break
                    }
                }
                .onChange(of: coverState, initial: true) { _, state in
                    let style: UIUserInterfaceStyle = appearance == "dark" ? .dark : appearance == "light" ? .light : .unspecified
                    switch state {
                    case .none: cover.hide()
                    case .lock:
                        // The keyboard must not keep typing into the hidden composer.
                        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
                        cover.show(AnyView(LockScreen(lock: lock)), style: style)
                    // What the app switcher shows: never a task.
                    case .privacy: cover.show(AnyView(ZStack { RemoteTheme.canvas.ignoresSafeArea(); BrandMark(size: 88) }), style: style)
                    }
                }
                .alert("openADE", isPresented: Binding(get: { store.notice != nil }, set: { if !$0 { store.notice = nil } })) {
                    Button("Entendido", role: .cancel) { store.notice = nil }
                } message: { Text(store.notice ?? "") }
                .task(id: scenePhase) { if scenePhase == .active { await notifications.registerIfAuthorized() } }
                .task(id: "\(store.pushKey)|\(notifications.token ?? "")") { await store.syncPush(token: notifications.token) }
                // A tapped notice waits for the unlock before it switches PC or opens a task.
                .onChange(of: lock.isLocked ? nil : notifications.destination, initial: true) { _, destination in
                    guard let destination else { return }
                    store.restore()
                    store.open(destination)
                    notifications.destination = nil
                }
        }
    }

    @ViewBuilder private var content: some View {
        #if DEBUG
        if let mode = DebugUIFixtures.mode { DebugFixtureScreen(mode: mode) } else { primary }
        #else
        primary
        #endif
    }

    @ViewBuilder private var primary: some View {
        if store.isPaired { HomeView().id(store.library.activeID) }
        else { WelcomeView() }
    }
}

/// The lock and the app-switcher cover live in their own window above every
/// sheet and alert, so nothing of a task shows or answers behind them.
@MainActor
private final class CoverWindow {
    private var window: UIWindow?

    func show(_ root: AnyView, style: UIUserInterfaceStyle) {
        guard let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first else { return }
        let window = self.window ?? UIWindow(windowScene: scene)
        window.windowLevel = .alert + 1
        window.overrideUserInterfaceStyle = style
        let controller = UIHostingController(rootView: root)
        controller.view.accessibilityViewIsModal = true
        window.rootViewController = controller
        window.isHidden = false
        self.window = window
    }

    func hide() {
        window?.isHidden = true
        window = nil
    }
}

private struct LockScreen: View {
    let lock: AppLock

    var body: some View {
        VStack(spacing: 16) {
            BrandMark(size: 72)
            Text("openADE está bloqueado").font(.title3.weight(.semibold))
            Text("Usá Face ID o el código del iPhone para ver tus tareas.")
                .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                .multilineTextAlignment(.center)
            Button {
                Task { await lock.unlock() }
            } label: {
                Label("Desbloquear", systemImage: "faceid")
            }
            .buttonStyle(RemotePrimaryButtonStyle())
            .frame(maxWidth: 240)
            .disabled(lock.authenticating)
            .padding(.top, 6)
            if let error = lock.error {
                Text(error).font(.footnote).foregroundStyle(.red).multilineTextAlignment(.center)
            }
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(RemoteTheme.canvas.ignoresSafeArea())
        .tint(RemoteTheme.ink)
    }
}
