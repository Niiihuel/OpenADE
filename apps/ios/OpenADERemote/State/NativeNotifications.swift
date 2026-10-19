import SwiftUI
import UserNotifications
import Observation

struct NotificationDestination: Identifiable, Equatable {
    let id = UUID()
    let deviceID: String
    let taskID: String
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
        Task { @MainActor in NativeNotifications.shared.error = "No se pudo registrar este iPhone para avisos. \(error.localizedDescription)" }
    }

    /// The app shows the same thing live while it is open.
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([])
    }

    /// Tapping a notice opens its task, on the PC that sent it.
    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let content = response.notification.request.content.userInfo["openade"] as? [String: Any]
        if content?["version"] as? Int == 2,
           let deviceID = content?["device_id"] as? String, !deviceID.isEmpty,
           let taskID = content?["session_id"] as? String, !taskID.isEmpty {
            Task { @MainActor in NativeNotifications.shared.destination = NotificationDestination(deviceID: deviceID, taskID: taskID) }
        }
        completionHandler()
    }
}
