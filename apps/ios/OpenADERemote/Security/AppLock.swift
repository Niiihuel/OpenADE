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

    /// Closed from the first frame: nothing shows before `launch` decides.
    init() {
        isLocked = enabled && available
    }

    /// Nothing to hide before a PC is paired, so an unpaired app opens.
    func launch(paired: Bool) {
        guard !started else { return }
        started = true
        #if DEBUG
        if DebugUIFixtures.mode != nil { isLocked = DebugUIFixtures.mode == "lock"; return }
        #endif
        if !paired { isLocked = false }
        else if isLocked { Task { await unlock() } }
    }

    /// The owner's Face ID or passcode, for turning the lock off. Without a
    /// passcode the lock never engages, so there is nothing to confirm.
    static func authenticate(_ reason: String) async -> Bool {
        let context = LAContext()
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else { return true }
        return (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)) ?? false
    }

    func enteredBackground() { if backgroundedAt == nil { backgroundedAt = .now } }

    func becameActive(paired: Bool) {
        defer { backgroundedAt = nil }
        guard paired, let away = backgroundedAt, enabled, available, Date.now.timeIntervalSince(away) > 5 * 60 else { return }
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
