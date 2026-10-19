import SwiftUI

@main
struct OpenADERemoteApp: App {
    @UIApplicationDelegateAdaptor(NativeAppDelegate.self) private var appDelegate
    @State private var store = RemoteStore()
    @State private var lock = AppLock()
    @State private var pairingPayload: PairingPayload?
    @AppStorage("openade.remote.appearance") private var appearance = "system"
    @Environment(\.scenePhase) private var scenePhase
    private var notifications: NativeNotifications { .shared }

    var body: some Scene {
        WindowGroup {
            ZStack {
                content
                    .blur(radius: lock.isLocked ? 28 : 0)
                    .allowsHitTesting(!lock.isLocked)
                    .accessibilityHidden(lock.isLocked)
                if lock.isLocked {
                    LockScreen(lock: lock)
                } else if scenePhase != .active {
                    // What the app switcher shows: never the conversation.
                    ZStack {
                        RemoteTheme.canvas.ignoresSafeArea()
                        BrandMark(size: 88)
                    }
                }
            }
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
            .alert("openADE", isPresented: Binding(get: { store.notice != nil }, set: { if !$0 { store.notice = nil } })) {
                Button("Entendido", role: .cancel) { store.notice = nil }
            } message: { Text(store.notice ?? "") }
            .onOpenURL { url in
                do { pairingPayload = try PairingPayload.parse(url.absoluteString) }
                catch { store.notice = error.localizedDescription }
            }
            .sheet(item: $pairingPayload) { PairingReviewView(payload: $0).environment(store).remoteSheet() }
            .task(id: scenePhase) { if scenePhase == .active { await notifications.registerIfAuthorized() } }
            .task(id: "\(store.pushKey)|\(notifications.token ?? "")") { await store.syncPush(token: notifications.token) }
            .onChange(of: notifications.destination, initial: true) {
                guard let destination = notifications.destination else { return }
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
        .background(.ultraThinMaterial)
        .accessibilityAddTraits(.isModal)
    }
}
