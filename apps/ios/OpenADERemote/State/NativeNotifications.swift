import SwiftUI
import UserNotifications
import Observation

struct NotificationDestination: Identifiable {
    let id = UUID()
    let deviceID: String
    let sessionID: String
}

@MainActor @Observable
final class NativeNotifications {
    static let shared = NativeNotifications()
    var token: String?
    var destination: NotificationDestination?
    var error: String?

    func requestAccess() async -> Bool {
        do {
            let allowed = try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
            if allowed { UIApplication.shared.registerForRemoteNotifications(); error = nil }
            else { error = "Las notificaciones están desactivadas. Podés habilitarlas en Ajustes del iPhone." }
            return allowed
        } catch { self.error = error.localizedDescription; return false }
    }

    func registerIfAuthorized() async {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        if settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional {
            UIApplication.shared.registerForRemoteNotifications()
        }
    }
}

final class NativeAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in NativeNotifications.shared.token = token; NativeNotifications.shared.error = nil }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        Task { @MainActor in NativeNotifications.shared.error = "No se pudo registrar este iPhone para notificaciones. \(error.localizedDescription)" }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let content = response.notification.request.content.userInfo["openade"] as? [String: Any]
        if content?["version"] as? Int == 1,
           let deviceID = content?["device_id"] as? String, !deviceID.isEmpty,
           let sessionID = content?["session_id"] as? String, !sessionID.isEmpty {
            Task { @MainActor in NativeNotifications.shared.destination = NotificationDestination(deviceID: deviceID, sessionID: sessionID) }
        }
        completionHandler()
    }
}

struct NotificationSettingsSection: View {
    @Environment(RemoteStore.self) private var store
    @State private var changing = false
    private var notifications: NativeNotifications { .shared }
    var body: some View {
        Section("Notificaciones de esta PC") {
            Toggle("Avisar cuando necesite atención", isOn: Binding(get: { store.pushEnabled }, set: { enabled in
                Task {
                    let scope = store.connectionIdentity
                    changing = true
                    let allowed: Bool
                    if enabled { allowed = await notifications.requestAccess() } else { allowed = true }
                    if allowed, scope == store.connectionIdentity { await store.setPushEnabled(enabled, token: notifications.token) }
                    changing = false
                }
            })).disabled(changing)
            if changing { ProgressView() }
            if let error = store.pushError ?? notifications.error { Text(error).font(.caption).foregroundStyle(.orange) }
            Text("Recibí un aviso al terminar un turno o cuando el agente requiera aprobación. El aviso no incluye código ni texto de la conversación.")
                .font(.footnote).foregroundStyle(.secondary)
        }
    }
}
