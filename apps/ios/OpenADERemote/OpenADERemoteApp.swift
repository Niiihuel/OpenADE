import SwiftUI

@main
struct OpenADERemoteApp: App {
    @UIApplicationDelegateAdaptor(NativeAppDelegate.self) private var appDelegate
    @State private var store = RemoteStore()
    @State private var pairingPayload: PairingPayload?
    @AppStorage("openade.remote.appearance") private var appearance = "system"
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ZStack {
                Group {
                    #if DEBUG
                    if let mode = DebugUIFixtures.mode { DebugFixtureScreen(mode: mode) }
                    else { primaryContent }
                    #else
                    primaryContent
                    #endif
                }
                .environment(store)
                .tint(RemoteTheme.ink)
                .task { store.restore() }

                if scenePhase != .active {
                    RemoteTheme.canvas.ignoresSafeArea()
                    Label("openADE", systemImage: "laptopcomputer.and.iphone")
                        .font(.title.bold())
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
            }
            .preferredColorScheme(appearance == "dark" ? .dark : appearance == "light" ? .light : nil)
            .onOpenURL { url in
                do { pairingPayload = try PairingPayload.parse(url.absoluteString) }
                catch { store.notice = error.localizedDescription }
            }
            .sheet(item: $pairingPayload) { PairingReviewView(payload: $0).environment(store) }
            .task(id: scenePhase) {
                if scenePhase == .active { await NativeNotifications.shared.registerIfAuthorized() }
            }
            .task(id: "\(store.connectionIdentity):\(store.statusTitle):\(store.pushEnabled):\(store.health?.supports("notifications.push") == true):\(NativeNotifications.shared.token ?? "")") {
                await store.syncPushRegistration(token: NativeNotifications.shared.token)
            }
            .task(id: NativeNotifications.shared.destination?.id) {
                store.restore()
                if let destination = NativeNotifications.shared.destination {
                    await store.openNotification(destination)
                    if NativeNotifications.shared.destination?.id == destination.id { NativeNotifications.shared.destination = nil }
                }
            }
        }
    }

    @ViewBuilder private var primaryContent: some View {
        if store.isPaired { WorkspaceView().id(store.connectionIdentity) }
        else { ConnectView() }
    }
}
