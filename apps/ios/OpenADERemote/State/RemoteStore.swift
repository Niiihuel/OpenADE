import Foundation
import Observation

@MainActor
@Observable
final class RemoteStore {
    enum ConnectionPhase { case offline, connecting, connected, reconnecting, authorizationRequired }

    private(set) var configuration: ConnectionConfiguration?
    private(set) var phase: ConnectionPhase = .offline
    private(set) var projects: [RemoteProject] = []
    private(set) var sessions: [RemoteSession] = []
    private(set) var hasMoreSessions = false
    private(set) var hasUnreadableSessions = false
    private(set) var sessionPageCount = 1
    private(set) var sessionSearch = ""
    private(set) var health: HostHealth?
    private(set) var lastSync: Date?
    private(set) var isConnecting = false
    private(set) var busyActions: Set<String> = []
    private(set) var projections: [String: ConversationProjection] = [:]
    private(set) var sessionErrors: [String: String] = [:]
    private(set) var pendingActions: [String: PendingAction] = [:]
    private(set) var pendingMutations: [String: DurableMutation] = [:]
    private(set) var queuedPrompts: [QueuedPrompt] = []
    private(set) var drafts: [String: String] = [:]
    private(set) var attachments: [String: [RemoteAttachment]] = [:]
    private(set) var library = ConnectionLibrary()
    private(set) var modelCatalog: RemoteModelCatalog?
    private(set) var pushError: String?
    var navigationSessionID: String?
    var notice: String?
    var connectionMessage: String?

    @ObservationIgnored private var api: RemoteAPIClient?
    @ObservationIgnored private let keychain = ConnectionKeychain()
    @ObservationIgnored private var restored = false
    @ObservationIgnored private var projectionRecency: [String] = []
    @ObservationIgnored private let restoreDisabledKey = "openade.remote.restore-disabled"
    private var connectionGeneration = UUID()

    var isPaired: Bool { configuration != nil }
    var computers: [SavedComputer] { library.computers.sorted { $0.lastUsedAt > $1.lastUsedAt } }
    var activeComputerName: String {
        library.computers.first { $0.id == library.activeID }?.name ?? "Mi PC"
    }
    var pushEnabled: Bool { library.computers.first { $0.id == library.activeID }?.notificationsEnabled == true }
    func supports(_ capability: String) -> Bool { health?.supports(capability) == true }
    var connectionIdentity: String { connectionGeneration.uuidString }
    var statusTitle: String {
        switch phase {
        case .offline: "Sin conexión"
        case .connecting: "Conectando"
        case .connected: "PC conectada"
        case .reconnecting: "Reconectando"
        case .authorizationRequired: "Volvé a conectar"
        }
    }

    func restore() {
        guard !restored else { return }
        restored = true
        #if DEBUG
        if DebugUIFixtures.mode != nil {
            do {
                let config = try ConnectionConfiguration(address: "https://fixture.invalid", token: "local-ui-fixture")
                configuration = config
                api = RemoteAPIClient(configuration: config, transport: DebugUIFixtures.transport())
                health = try DebugUIFixtures.decode(HostHealth.self, path: "/v1/health")
                projects = [RemoteProject(id: "fixture-project", name: "openADE", path: "/fixture/openADE")]
                sessions = try DebugUIFixtures.decode(SessionPage.self, path: "/v1/sessions").sessions
                let computer = SavedComputer(id: "fixture", name: "Mac de desarrollo · fixture", configuration: config,
                                             lastUsedAt: .now, pendingActions: [:], pendingMutations: [:], queuedPrompts: [])
                library = ConnectionLibrary(computers: [computer], activeID: computer.id)
                phase = .connected
            } catch { notice = "No se pudo cargar la captura de prueba." }
            return
        }
        #endif
        guard !UserDefaults.standard.bool(forKey: restoreDisabledKey) else { return }
        do {
            if let stored = try keychain.loadLibrary() { library = stored }
            else if let saved = try keychain.load() {
                let computer = SavedComputer(id: UUID().uuidString, name: saved.baseURL.host ?? "Mi PC",
                                             configuration: saved, lastUsedAt: .now,
                                             pendingActions: try keychain.loadPendingActions(for: saved),
                                             pendingMutations: [:], queuedPrompts: [])
                library = ConnectionLibrary(computers: [computer], activeID: computer.id)
                try keychain.saveLibrary(library)
                // Only delete legacy records after the atomic V2 vault has committed.
                try keychain.delete()
                try keychain.savePendingActions([:])
            }
            if let saved = library.computers.first(where: { $0.id == library.activeID }) {
                activate(saved)
                phase = .connecting
            }
        } catch {
            notice = readableError(error)
        }
    }

    func connect(address: String, token: String, name: String = "") async -> Bool {
        guard !isConnecting else { return false }
        let generation = connectionGeneration
        let previousPhase = phase
        isConnecting = true
        phase = .connecting
        notice = nil
        defer { isConnecting = false }
        do {
            let candidate = try ConnectionConfiguration(address: address, token: token)
            let client = RemoteAPIClient(configuration: candidate)
            let host = try await client.health()
            guard host.version == .string("1") || host.version == .number(1) else {
                throw ConnectionError.unsupportedVersion
            }
            let available = try await client.projects()
            guard generation == connectionGeneration else { return false }
            var next = library
            let existing = next.computers.first { $0.configuration.baseURL == candidate.baseURL }
            let sameCredential = existing?.configuration == candidate
            let computer = SavedComputer(id: existing?.id ?? UUID().uuidString,
                                         name: name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                            ? existing?.name ?? candidate.baseURL.host ?? "Mi PC" : name,
                                         configuration: candidate, lastUsedAt: .now,
                                         pendingActions: sameCredential ? existing?.pendingActions ?? [:] : [:],
                                         pendingMutations: sameCredential ? existing?.pendingMutations ?? [:] : [:],
                                         queuedPrompts: sameCredential ? existing?.queuedPrompts ?? [] : [],
                                         drafts: sameCredential ? existing?.drafts : nil,
                                         deviceID: sameCredential ? existing?.deviceID : nil,
                                         notificationsEnabled: sameCredential ? existing?.notificationsEnabled : nil,
                                         attachments: sameCredential ? existing?.attachments : nil)
            next.computers.removeAll { $0.id == computer.id }
            next.computers.append(computer)
            next.activeID = computer.id
            try keychain.saveLibrary(next)
            library = next
            UserDefaults.standard.set(false, forKey: restoreDisabledKey)
            activate(computer)
            api = client
            health = host
            projects = available
            connected()
            return true
        } catch {
            guard generation == connectionGeneration else { return false }
            phase = configuration == nil ? .offline : previousPhase
            notice = readableError(error)
            return false
        }
    }

    func disconnect() {
        guard let activeID = library.activeID else { return }
        forgetComputer(id: activeID)
    }

    func switchComputer(id: String) async -> Bool {
        guard let computer = library.computers.first(where: { $0.id == id }) else { return false }
        return await connect(address: computer.configuration.displayAddress, token: computer.configuration.token, name: computer.name)
    }

    func pair(_ payload: PairingPayload, deviceName: String) async -> Bool {
        guard !isConnecting else { return false }
        let generation = connectionGeneration
        do {
            if let pending = library.pendingPairing, pending.payload != payload { throw PairingError.pending }
            let pending = library.pendingPairing ?? PendingPairing(payload: payload, requestID: UUID().uuidString, deviceName: deviceName)
            var next = library
            next.pendingPairing = pending
            try keychain.saveLibrary(next)
            library = next
            isConnecting = true
            let endpoint = try ConnectionConfiguration(address: payload.baseURL, token: "pairing")
            let client = RemoteAPIClient(configuration: endpoint)
            let result = try await client.completePairing(pending)
            guard generation == connectionGeneration else { isConnecting = false; return false }
            let verified = try ConnectionConfiguration(address: result.baseURL, token: result.token)
            guard result.version == 1, verified.baseURL == endpoint.baseURL, UUID(uuidString: result.deviceID) != nil else {
                throw ConnectionError.invalidResponse
            }
            isConnecting = false
            guard await connect(address: verified.displayAddress, token: verified.token, name: payload.label) else { return false }
            guard let index = library.computers.firstIndex(where: { $0.id == library.activeID }),
                  library.computers[index].configuration == verified else { return false }
            var committed = library
            committed.computers[index].deviceID = result.deviceID
            committed.pendingPairing = nil
            try keychain.saveLibrary(committed)
            library = committed
            // The credential and device identity are durable before the server purges its retry token.
            try? await RemoteAPIClient(configuration: verified).acknowledgePairing(id: payload.pairingID)
            return true
        } catch {
            isConnecting = false
            if generation == connectionGeneration || configuration != nil { notice = error.localizedDescription }
            return false
        }
    }

    func discardPendingPairing() {
        guard !isConnecting else { return }
        var next = library
        next.pendingPairing = nil
        do { try keychain.saveLibrary(next); library = next } catch { notice = readableError(error) }
    }

    func renameComputer(id: String, name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let index = library.computers.firstIndex(where: { $0.id == id }) else { return }
        var next = library
        next.computers[index].name = trimmed
        do { try keychain.saveLibrary(next); library = next } catch { notice = readableError(error) }
    }

    func forgetComputer(id: String) {
        let active = library.activeID == id
        var next = library
        next.computers.removeAll { $0.id == id }
        if active { next.activeID = nil }
        if active {
            UserDefaults.standard.set(true, forKey: restoreDisabledKey)
            clearConnection()
        }
        library = next
        do {
            try keychain.saveLibrary(next)
            library = next
            try keychain.delete()
            try keychain.savePendingActions([:])
            UserDefaults.standard.set(false, forKey: restoreDisabledKey)
        } catch { notice = readableError(error) }
    }

    private func activate(_ computer: SavedComputer) {
        clearConnection()
        configuration = computer.configuration
        api = RemoteAPIClient(configuration: computer.configuration)
        pendingActions = computer.pendingActions
        pendingMutations = computer.pendingMutations
        queuedPrompts = computer.queuedPrompts
        drafts = computer.drafts ?? [:]
        attachments = computer.attachments ?? [:]
    }

    private func clearConnection() {
        connectionGeneration = UUID()
        api = nil
        configuration = nil
        projects = []
        sessions = []
        hasMoreSessions = false; hasUnreadableSessions = false; sessionPageCount = 1; sessionSearch = ""
        projections = [:]
        projectionRecency = []
        pendingActions = [:]
        pendingMutations = [:]
        queuedPrompts = []
        drafts = [:]
        attachments = [:]
        modelCatalog = nil
        pushError = nil
        navigationSessionID = nil
        busyActions = []
        sessionErrors = [:]
        health = nil
        lastSync = nil
        notice = nil
        connectionMessage = nil
        phase = .offline
    }

    func pollOverview() async {
        guard let client = api else { return }
        let generation = connectionGeneration
        var retry = ReconnectionDelay()
        while !Task.isCancelled, isPaired, generation == connectionGeneration {
            do {
                if health == nil {
                    let info = try await client.health()
                    guard generation == connectionGeneration else { return }
                    guard info.version == .string("1") || info.version == .number(1) else {
                        throw ConnectionError.unsupportedVersion
                    }
                    health = info
                }
                let search = sessionSearch
                let pages = sessionPageCount
                async let projectList = client.projects()
                async let sessionList = client.sessions(search: search, pageCount: pages)
                let (availableProjects, availableSessions) = try await (projectList, sessionList)
                try Task.checkCancellation()
                guard generation == connectionGeneration else { return }
                guard search == sessionSearch, pages == sessionPageCount else { continue }
                projects = availableProjects
                applySessionPage(availableSessions)
                connected()
                retry.reset()
                try await Task.sleep(for: .seconds(5))
            } catch {
                if isCancellation(error) { return }
                guard generation == connectionGeneration else { return }
                markDisconnected(error)
                if phase == .authorizationRequired { return }
                do { try await Task.sleep(for: retry.next()) } catch { return }
            }
        }
    }

    func refreshOverview() async {
        guard let client = api else { return }
        let generation = connectionGeneration
        do {
            let search = sessionSearch
            let pages = sessionPageCount
            async let projectList = client.projects()
            async let sessionList = client.sessions(search: search, pageCount: pages)
            let (availableProjects, availableSessions) = try await (projectList, sessionList)
            try Task.checkCancellation()
            guard generation == connectionGeneration, search == sessionSearch, pages == sessionPageCount else { return }
            projects = availableProjects
            applySessionPage(availableSessions)
            connected()
        } catch {
            guard generation == connectionGeneration else { return }
            if !isCancellation(error) { markDisconnected(error) }
        }
    }

    private func applySessionPage(_ page: SessionPage) {
        var unique: [String: RemoteSession] = [:]
        for session in page.sessions { unique[session.id] = session }
        sessions = unique.values.sorted { $0.updatedAtMS > $1.updatedAtMS }
        hasMoreSessions = page.nextCursor != nil
        hasUnreadableSessions = page.partial == true
    }

    func searchSessions(_ query: String) async {
        guard query.utf8.count <= 256 else { notice = "La búsqueda puede tener hasta 256 bytes."; return }
        guard sessionSearch != query else { return }
        sessionSearch = query; sessionPageCount = 1
        await refreshOverview()
    }

    func loadMoreSessions() async {
        guard hasMoreSessions, sessionPageCount < 50 else { return }
        sessionPageCount += 1
        await refreshOverview()
    }

    func pollSession(id: String) async {
        let generation = connectionGeneration
        var retry = ReconnectionDelay()
        while !Task.isCancelled, isPaired, generation == connectionGeneration {
            do {
                try await refreshSession(id: id)
                retry.reset()
                try await Task.sleep(for: .seconds(1))
            } catch {
                if isCancellation(error) { return }
                guard generation == connectionGeneration else { return }
                sessionErrors[id] = readableError(error)
                markDisconnected(error)
                if phase == .authorizationRequired { return }
                do { try await Task.sleep(for: retry.next()) } catch { return }
            }
        }
    }

    func refreshSession(id: String) async throws {
        guard let client = api else { throw ConnectionError.disconnected }
        let generation = connectionGeneration
        let snapshot = try await client.session(id: id)
        try Task.checkCancellation()
        guard generation == connectionGeneration else { throw CancellationError() }
        upsert(snapshot)
        // Drain a bounded batch before yielding so long histories do not block UI work.
        for _ in 0..<10 {
            let cursor = projections[id]?.cursor ?? 0
            let page = try await client.events(sessionID: id, after: cursor)
            try Task.checkCancellation()
            guard generation == connectionGeneration else { throw CancellationError() }
            guard page.nextCursor >= cursor else { throw ConnectionError.invalidResponse }
            var projection = projections[id] ?? ConversationProjection()
            projection.apply(page)
            projections[id] = projection
            touchProjection(id)
            if !page.hasMore { break }
            guard projection.cursor > cursor else { throw ConnectionError.invalidResponse }
        }
        sessionErrors[id] = nil
        connected()
    }

    func session(id: String) -> RemoteSession? { sessions.first { $0.id == id } }
    func entries(sessionID: String) -> [ConversationEntry] { projections[sessionID]?.entries ?? [] }
    func pendingTurn(sessionID: String) -> PendingAction? {
        pendingActions["sendTurn:\(sessionID)"] ?? pendingActions["steerTurn:\(sessionID)"]
    }
    func actionIsBusy(_ key: String) -> Bool { busyActions.contains(key) }

    func createSession(projectID: String) async -> String? {
        guard let client = api else { return nil }
        let generation = connectionGeneration
        let key = "createSession:\(projectID)"
        guard !busyActions.contains(key) else { return nil }
        busyActions.insert(key)
        defer { if generation == connectionGeneration { busyActions.remove(key) } }
        do {
            let action = try prepareAction(kind: .createSession, resourceID: projectID)
            let created = try await client.createSession(action: action)
            guard generation == connectionGeneration else { return nil }
            try finishAction(action)
            upsert(created)
            return created.id
        } catch {
            guard generation == connectionGeneration else { return nil }
            if invalidateIfUnauthorized(error) { return nil }
            clearDefinitivelyRejectedAction(key: key, error: error)
            notice = readableError(error)
            return nil
        }
    }

    func sendPrompt(sessionID: String, text: String, mode: TurnMode, options: RemoteTurnOptions = .init()) async -> Bool {
        guard let client = api else { return false }
        let generation = connectionGeneration
        let snapshot = session(id: sessionID)
        let previous = pendingTurn(sessionID: sessionID)
        let kind: PendingAction.Kind = previous?.kind ?? (snapshot?.isBusy == true ? .steerTurn : .sendTurn)
        let key = "\(kind.rawValue):\(sessionID)"
        guard !busyActions.contains(key) else { return false }
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard previous != nil || prompt.utf8.count <= 32 * 1024 else {
            notice = "El mensaje puede tener hasta 32 KB. Para texto más largo, adjuntá un archivo."; return false
        }
        let selectedAttachments = attachments[sessionID] ?? []
        guard !prompt.isEmpty || !selectedAttachments.isEmpty || pendingActions[key] != nil else { return false }
        guard previous != nil || (pendingMutations["attachment:\(sessionID)"] == nil &&
                                  (kind != .steerTurn || selectedAttachments.isEmpty)) else {
            notice = "Confirmá los adjuntos pendientes y esperá a que termine el turno para enviarlos."; return false
        }
        busyActions.insert(key)
        defer { if generation == connectionGeneration { busyActions.remove(key) } }
        do {
            let action = try prepareAction(kind: kind, resourceID: sessionID, prompt: prompt, mode: mode,
                                           turnID: snapshot?.activeTurnID, attachmentIDs: selectedAttachments.map(\.id),
                                           options: kind == .sendTurn ? options : nil)
            try await client.submitPrompt(action: action)
            guard generation == connectionGeneration else { return false }
            try finishAction(action, clearingAttachments: true)
            // A refresh failure must not turn an acknowledged mutation into a retry.
            await refreshAfterMutation(id: sessionID)
            guard generation == connectionGeneration else { return false }
            return true
        } catch {
            guard generation == connectionGeneration else { return false }
            if invalidateIfUnauthorized(error) { return false }
            clearDefinitivelyRejectedAction(key: key, error: error)
            if let rejection = error as? RemoteAPIError, rejection.status == 409, pendingActions[key] != nil {
                notice = "La PC necesita resolver un conflicto. Conservamos el envío original para evitar duplicarlo. \(readableError(error))"
            } else {
                notice = pendingActions[key] != nil
                    ? "No se pudo confirmar el envío. Reintentá el mismo mensaje para consultar su resultado sin duplicarlo. \(readableError(error))"
                    : readableError(error)
            }
            return false
        }
    }

    func cancelTurn(sessionID: String, turnID: String) async {
        guard let client = api else { return }
        let generation = connectionGeneration
        let key = "cancelTurn:\(sessionID)"
        guard !busyActions.contains(key) else { return }
        busyActions.insert(key)
        defer { if generation == connectionGeneration { busyActions.remove(key) } }
        do {
            let action = try prepareAction(kind: .cancelTurn, resourceID: sessionID, turnID: turnID)
            try await client.cancelTurn(action: action)
            guard generation == connectionGeneration else { return }
            try finishAction(action)
            await refreshAfterMutation(id: sessionID)
        } catch {
            guard generation == connectionGeneration else { return }
            if invalidateIfUnauthorized(error) { return }
            clearDefinitivelyRejectedAction(key: key, error: error)
            notice = readableError(error)
        }
    }

    func resolvePermission(sessionID: String, permission: RemotePermission, allow: Bool) async {
        guard let client = api else { return }
        let generation = connectionGeneration
        let key = "permission:\(permission.id)"
        guard !busyActions.contains(key) else { return }
        busyActions.insert(key)
        defer { if generation == connectionGeneration { busyActions.remove(key) } }
        do {
            try await client.resolvePermission(sessionID: sessionID, requestID: permission.id, allow: allow)
            guard generation == connectionGeneration else { return }
            await refreshAfterMutation(id: sessionID)
        } catch {
            guard generation == connectionGeneration else { return }
            if invalidateIfUnauthorized(error) { return }
            notice = readableError(error)
            await refreshAfterMutation(id: sessionID)
        }
    }

    func loadDiff(projectID: String) async throws -> ProjectDiff {
        guard let api else { throw ConnectionError.disconnected }
        let generation = connectionGeneration
        do {
            let diff = try await api.diff(projectID: projectID)
            guard generation == connectionGeneration else { throw CancellationError() }
            return diff
        } catch {
            if generation == connectionGeneration { _ = invalidateIfUnauthorized(error) }
            throw error
        }
    }

    func load<Response: Decodable>(_ path: [String], query: [URLQueryItem] = []) async throws -> Response {
        guard let api else { throw ConnectionError.disconnected }
        let generation = connectionGeneration
        do {
            let value: Response = try await api.get(path, query: query)
            try Task.checkCancellation()
            guard generation == connectionGeneration else { throw CancellationError() }
            return value
        } catch {
            if generation == connectionGeneration { _ = invalidateIfUnauthorized(error) }
            throw error
        }
    }

    func mutate<Response: Decodable>(key: String, label: String, method: String, path: [String],
                                     body: [String: JSONValue]) async throws -> Response {
        guard let api else { throw ConnectionError.disconnected }
        let generation = connectionGeneration
        guard !busyActions.contains(key) else {
            throw RemoteAPIError(status: 409, code: "local_busy", message: "La acción todavía está en curso.")
        }
        busyActions.insert(key)
        defer { if generation == connectionGeneration { busyActions.remove(key) } }
        var action = pendingMutations[key] ?? DurableMutation(requestID: UUID().uuidString, key: key,
                                                             label: label, method: method, path: path, body: body)
        do {
            if pendingMutations[key] == nil {
                var next = pendingMutations
                next[key] = action
                try persistActiveState(mutations: next)
                pendingMutations = next
            }
            let response: Response = try await api.mutate(action)
            guard generation == connectionGeneration else { throw CancellationError() }
            var next = pendingMutations
            next[key] = nil
            try persistActiveState(mutations: next)
            pendingMutations = next
            return response
        } catch {
            guard generation == connectionGeneration else { throw CancellationError() }
            if invalidateIfUnauthorized(error) { throw error }
            if let rejection = error as? RemoteAPIError {
                var next = pendingMutations
                if PendingActionRetentionPolicy.mayDiscardAfterRejection(status: rejection.status, code: rejection.code) {
                    next[key] = nil
                } else {
                    action.rejectionCode = rejection.code
                    next[key] = action
                }
                if (try? persistActiveState(mutations: next)) != nil { pendingMutations = next }
            }
            throw error
        }
    }

    func updateSession(id: String, title: String? = nil, archived: Bool? = nil, favorite: Bool? = nil) async {
        guard pendingMutations["session:\(id)"] == nil else {
            notice = "Primero confirmá la actualización de esta sesión en Envíos pendientes."; return
        }
        var body: [String: JSONValue] = [:]
        if let title { body["title"] = .string(title) }
        if let archived { body["archived"] = .bool(archived) }
        if let favorite { body["favorite"] = .bool(favorite) }
        do {
            let updated: RemoteSession = try await mutate(key: "session:\(id)", label: "Actualizar sesión",
                                                          method: "PATCH", path: ["v1", "sessions", id], body: body)
            upsert(updated)
        } catch { if !isCancellation(error) { notice = readableError(error) } }
    }

    func retryMutation(_ action: DurableMutation) async {
        do {
            if action.key.hasPrefix("attachment:") {
                try await performAttachmentUpload(action, sessionID: String(action.key.dropFirst("attachment:".count)))
                return
            }
            let _: JSONValue = try await mutate(key: action.key, label: action.label, method: action.method,
                                                path: action.path, body: action.body)
            await refreshOverview()
        } catch { if !isCancellation(error) { notice = readableError(error) } }
    }

    func retryPendingAction(_ action: PendingAction) async {
        guard pendingActions[action.key]?.requestID == action.requestID else { return }
        switch action.kind {
        case .createSession:
            if let created = await createSession(projectID: action.resourceID) { navigationSessionID = created }
        case .sendTurn, .steerTurn:
            _ = await sendPrompt(sessionID: action.resourceID, text: action.prompt ?? "", mode: action.mode ?? .build)
        case .cancelTurn:
            if let turnID = action.turnID { await cancelTurn(sessionID: action.resourceID, turnID: turnID) }
        }
    }

    func discardRejectedMutation(_ action: DurableMutation) {
        guard pendingMutations[action.key]?.requestID == action.requestID, action.canDiscard else { return }
        var next = pendingMutations
        next[action.key] = nil
        do {
            try persistActiveState(mutations: next)
            pendingMutations = next
        } catch { notice = readableError(error) }
    }

    func loadModelCatalog() async {
        guard supports("models.list") else { return }
        do { modelCatalog = try await load(["v1", "models"]) }
        catch { if !isCancellation(error) { notice = readableError(error) } }
    }

    func setPushEnabled(_ enabled: Bool, token: String?) async {
        guard let index = library.computers.firstIndex(where: { $0.id == library.activeID }) else { return }
        var next = library
        next.computers[index].notificationsEnabled = enabled
        do { try keychain.saveLibrary(next); library = next; await syncPushRegistration(token: token) }
        catch { pushError = readableError(error) }
    }

    func syncPushRegistration(token: String?) async {
        guard supports("notifications.push"), let api, phase == .connected else { return }
        guard !pushEnabled || token != nil else { return }
        let generation = connectionGeneration
        do {
            let registration = try await api.setPushToken(pushEnabled ? token : nil)
            guard generation == connectionGeneration else { return }
            guard UUID(uuidString: registration.deviceID) != nil,
                  let index = library.computers.firstIndex(where: { $0.id == library.activeID }) else {
                throw ConnectionError.invalidResponse
            }
            if let existing = library.computers[index].deviceID, existing != registration.deviceID {
                throw ConnectionError.invalidResponse
            }
            if library.computers[index].deviceID == nil {
                var next = library
                next.computers[index].deviceID = registration.deviceID
                try keychain.saveLibrary(next)
                library = next
            }
            pushError = nil
        } catch {
            guard generation == connectionGeneration else { return }
            if !invalidateIfUnauthorized(error) { pushError = readableError(error) }
        }
    }

    func openNotification(_ destination: NotificationDestination) async {
        guard let computer = library.computers.first(where: { $0.deviceID == destination.deviceID }) else {
            notice = "Este aviso corresponde a una computadora que ya no está emparejada."; return
        }
        if library.activeID != computer.id {
            guard await switchComputer(id: computer.id) else { return }
        }
        do {
            try await refreshSession(id: destination.sessionID)
            navigationSessionID = destination.sessionID
        } catch { if !isCancellation(error) { notice = readableError(error) } }
    }

    func enqueue(sessionID: String, text: String, mode: TurnMode, options: RemoteTurnOptions = .init()) -> Bool {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        guard queuedPrompts.count < 30 else { notice = "La cola puede guardar hasta 30 mensajes."; return false }
        var next = queuedPrompts
        next.append(QueuedPrompt(id: UUID().uuidString, sessionID: sessionID, text: text, mode: mode, createdAt: .now, options: options))
        do { try persistActiveState(queue: next); queuedPrompts = next; return true }
        catch { notice = readableError(error); return false }
    }

    func removeQueuedPrompt(id: String) {
        let next = queuedPrompts.filter { $0.id != id }
        do { try persistActiveState(queue: next); queuedPrompts = next }
        catch { notice = readableError(error) }
    }

    func sendQueuedPrompt(_ queued: QueuedPrompt) async {
        let generation = connectionGeneration
        guard (attachments[queued.sessionID] ?? []).isEmpty, pendingMutations["attachment:\(queued.sessionID)"] == nil else {
            notice = "El borrador actual tiene adjuntos. Envialos o quitalos antes de enviar otro mensaje de la cola."; return
        }
        if let pending = pendingTurn(sessionID: queued.sessionID), pending.prompt != queued.text {
            notice = "Primero resolvé el envío pendiente de esta sesión."
            return
        }
        if await sendPrompt(sessionID: queued.sessionID, text: queued.text, mode: queued.mode, options: queued.options ?? .init()), generation == connectionGeneration {
            removeQueuedPrompt(id: queued.id)
        }
    }

    func addAttachment(_ upload: AttachmentUpload, sessionID: String, projectID: String) async throws {
        let key = "attachment:\(sessionID)"
        guard pendingTurn(sessionID: sessionID) == nil, pendingMutations[key] == nil else { throw AttachmentError.pendingUpload }
        guard session(id: sessionID)?.projectID == projectID,
              session(id: sessionID)?.isBusy != true,
              session(id: sessionID)?.canStartTurn != false,
              supports(upload.mediaType == "text/plain" ? "attachments.text" : "attachments.images") else {
            throw ConnectionError.invalidResponse
        }
        let selected = attachments[sessionID] ?? []
        let textSize = selected.filter { !$0.isImage }.reduce(Int64(0)) { $0 + $1.sizeBytes }
        guard selected.count < 8, upload.mediaType != "text/plain" || textSize + Int64(upload.data.count) + 4_096 <= 1024 * 1024 else {
            throw AttachmentError.limit
        }
        let action = DurableMutation(requestID: UUID().uuidString, key: key, label: "Adjuntar \(upload.filename)",
                                     method: "POST", path: ["v1", "projects", projectID, "attachments"], body: upload.body)
        var next = pendingMutations
        next[key] = action
        try persistActiveState(mutations: next)
        pendingMutations = next
        try await performAttachmentUpload(action, sessionID: sessionID)
    }

    private func performAttachmentUpload(_ action: DurableMutation, sessionID: String) async throws {
        guard let api else { throw ConnectionError.disconnected }
        guard !busyActions.contains(action.key) else { throw AttachmentError.pendingUpload }
        let generation = connectionGeneration
        busyActions.insert(action.key)
        defer { if generation == connectionGeneration { busyActions.remove(action.key) } }
        do {
            let uploaded: RemoteAttachment = try await api.mutate(action)
            guard generation == connectionGeneration else { throw CancellationError() }
            guard case .string(let expectedDigest)? = action.body["sha256"], expectedDigest == uploaded.sha256,
                  case .string(let expectedName)? = action.body["filename"], expectedName == uploaded.filename else {
                throw ConnectionError.invalidResponse
            }
            var nextAttachments = attachments
            if nextAttachments[sessionID]?.contains(where: { $0.id == uploaded.id }) != true {
                nextAttachments[sessionID, default: []].append(uploaded)
            }
            var nextMutations = pendingMutations
            nextMutations[action.key] = nil
            // Commit the attachment reference and receipt together before dropping the upload ID.
            try persistActiveState(mutations: nextMutations, attachments: nextAttachments)
            attachments = nextAttachments; pendingMutations = nextMutations
        } catch {
            guard generation == connectionGeneration else { throw CancellationError() }
            if invalidateIfUnauthorized(error) { throw error }
            if let rejection = error as? RemoteAPIError,
               PendingActionRetentionPolicy.mayDiscardAfterRejection(status: rejection.status, code: rejection.code) {
                var next = pendingMutations; next[action.key] = nil
                if (try? persistActiveState(mutations: next)) != nil { pendingMutations = next }
            }
            throw error
        }
    }

    func removeAttachment(id: String, sessionID: String) {
        guard pendingTurn(sessionID: sessionID) == nil else { return }
        var next = attachments
        next[sessionID]?.removeAll { $0.id == id }
        if next[sessionID]?.isEmpty == true { next[sessionID] = nil }
        do { try persistActiveState(attachments: next); attachments = next }
        catch { notice = readableError(error) }
    }

    func saveDraft(sessionID: String, text: String) {
        guard (drafts[sessionID] ?? "") != text else { return }
        var next = drafts
        next[sessionID] = text.isEmpty ? nil : text
        // Drafts are bounded and scoped to this computer, like the transcript cache.
        guard text.utf8.count <= 256 * 1024, next.count <= 100 else { return }
        do { try persistActiveState(drafts: next); drafts = next }
        catch { notice = readableError(error) }
    }

    private func persistActiveState(actions: [String: PendingAction]? = nil,
                                    mutations: [String: DurableMutation]? = nil,
                                    queue: [QueuedPrompt]? = nil, drafts: [String: String]? = nil,
                                    attachments: [String: [RemoteAttachment]]? = nil) throws {
        guard let index = library.computers.firstIndex(where: { $0.id == library.activeID }),
              library.computers[index].configuration == configuration else { throw ConnectionError.disconnected }
        var next = library
        if let actions { next.computers[index].pendingActions = actions }
        if let mutations { next.computers[index].pendingMutations = mutations }
        if let queue { next.computers[index].queuedPrompts = queue }
        if let drafts { next.computers[index].drafts = drafts }
        if let attachments { next.computers[index].attachments = attachments }
        #if DEBUG
        if DebugUIFixtures.mode != nil { library = next; return }
        #endif
        try keychain.saveLibrary(next)
        library = next
    }

    private func prepareAction(kind: PendingAction.Kind, resourceID: String, prompt: String? = nil,
                               mode: TurnMode? = nil, turnID: String? = nil, attachmentIDs: [String]? = nil,
                               options: RemoteTurnOptions? = nil) throws -> PendingAction {
        let key = "\(kind.rawValue):\(resourceID)"
        if let existing = pendingActions[key] { return existing }
        let action = PendingAction(requestID: UUID().uuidString, kind: kind, resourceID: resourceID,
                                   prompt: prompt, mode: mode, turnID: turnID, attachmentIDs: attachmentIDs, options: options)
        var saved = pendingActions
        saved[key] = action
        try persistActiveState(actions: saved)
        pendingActions = saved
        return action
    }

    private func finishAction(_ action: PendingAction, clearingAttachments: Bool = false) throws {
        var saved = pendingActions
        saved[action.key] = nil
        var nextAttachments = attachments
        if clearingAttachments {
            let sent = Set(action.attachmentIDs ?? [])
            nextAttachments[action.resourceID]?.removeAll { sent.contains($0.id) }
            if nextAttachments[action.resourceID]?.isEmpty == true { nextAttachments[action.resourceID] = nil }
        }
        try persistActiveState(actions: saved, attachments: nextAttachments)
        pendingActions = saved
        attachments = nextAttachments
    }

    private func clearDefinitivelyRejectedAction(key: String, error: Error) {
        guard let error = error as? RemoteAPIError,
              PendingActionRetentionPolicy.mayDiscardAfterRejection(status: error.status, code: error.code),
              let action = pendingActions[key] else { return }
        try? finishAction(action)
    }

    private func upsert(_ session: RemoteSession) {
        if let index = sessions.firstIndex(where: { $0.id == session.id }) { sessions[index] = session }
        else { sessions.insert(session, at: 0) }
    }

    private func touchProjection(_ id: String) {
        projectionRecency.removeAll { $0 == id }
        projectionRecency.append(id)
        while projectionRecency.count > 6 {
            let expired = projectionRecency.removeFirst()
            projections[expired] = nil
            sessionErrors[expired] = nil
        }
    }

    private func refreshAfterMutation(id: String) async {
        let generation = connectionGeneration
        do { try await refreshSession(id: id) }
        catch {
            if generation == connectionGeneration, !isCancellation(error) { markDisconnected(error) }
        }
    }

    private func connected() {
        phase = .connected
        lastSync = .now
        connectionMessage = nil
    }

    private func markDisconnected(_ error: Error) {
        if invalidateIfUnauthorized(error) { return }
        phase = .reconnecting
        if let connectionError = error as? ConnectionError {
            switch connectionError {
            case .unauthorized, .unsupportedVersion: phase = .authorizationRequired
            default: break
            }
        }
        connectionMessage = readableError(error)
    }

    @discardableResult
    private func invalidateIfUnauthorized(_ error: Error) -> Bool {
        guard let connectionError = error as? ConnectionError else { return false }
        guard case .unauthorized = connectionError else { return false }
        if let activeID = library.activeID { forgetComputer(id: activeID) }
        else { clearConnection() }
        phase = .authorizationRequired
        connectionMessage = nil
        notice = connectionError.localizedDescription
        if UserDefaults.standard.bool(forKey: restoreDisabledKey) {
            notice = "El acceso fue revocado. No se pudo limpiar el llavero; volvé a conectar para reemplazar las credenciales."
        }
        return true
    }

    private func isCancellation(_ error: Error) -> Bool {
        Task.isCancelled || error is CancellationError || (error as? URLError)?.code == .cancelled
    }

    private func readableError(_ error: Error) -> String {
        if let network = error as? URLError {
            switch network.code {
            case .notConnectedToInternet: return "El iPhone está sin conexión a Internet."
            case .timedOut: return "La PC tardó demasiado en responder. Comprobá que esté encendida y conectada."
            case .cannotConnectToHost, .cannotFindHost, .networkConnectionLost:
                return "No se pudo contactar a tu PC. Revisá la dirección y la conexión privada."
            case .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateHasUnknownRoot,
                    .serverCertificateNotYetValid, .secureConnectionFailed:
                return "No se pudo verificar la conexión HTTPS de la PC. Revisá su certificado."
            default: return "No se pudo completar la conexión. Volvé a intentarlo."
            }
        }
        return error.localizedDescription
    }
}
