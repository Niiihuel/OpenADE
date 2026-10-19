import Foundation
import LocalAuthentication
import Observation

/// Face ID (or the passcode) when the app opens and when it comes back after
/// five minutes away. Content stays hidden while locked.
@MainActor @Observable
final class AppLock {
    static let enabledKey = "openade.remote.lock"
    private(set) var isLocked = false
    private(set) var authenticating = false
    var error: String?
    @ObservationIgnored private var backgroundedAt: Date?
    @ObservationIgnored private var started = false

    /// Settings writes it with `@AppStorage`; on unless the person turned it off.
    private var enabled: Bool { UserDefaults.standard.object(forKey: Self.enabledKey) as? Bool ?? true }

    /// A device without a passcode cannot authenticate, so it is never locked out.
    private var available: Bool {
        LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
    }

    func launch() {
        guard !started else { return }
        started = true
        #if DEBUG
        if DebugUIFixtures.mode != nil { isLocked = DebugUIFixtures.mode == "lock"; return }
        #endif
        if enabled && available { isLocked = true; Task { await unlock() } }
    }

    func enteredBackground() { if backgroundedAt == nil { backgroundedAt = .now } }

    func becameActive() {
        defer { backgroundedAt = nil }
        guard let away = backgroundedAt, enabled, available, Date.now.timeIntervalSince(away) > 5 * 60 else { return }
        isLocked = true
        Task { await unlock() }
    }

    func unlock() async {
        guard isLocked, !authenticating else { return }
        authenticating = true
        defer { authenticating = false }
        let context = LAContext()
        do {
            if try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "Desbloqueá openADE para ver tus tareas.") {
                isLocked = false
                error = nil
            }
        } catch {
            if (error as? LAError)?.code != .userCancel && (error as? LAError)?.code != .appCancel {
                self.error = "No se pudo verificar tu identidad. Probá de nuevo."
            }
        }
    }
}
