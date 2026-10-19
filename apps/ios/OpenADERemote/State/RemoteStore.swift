import Foundation
import Network
import Observation

/// The phone's view of its PCs: which one is active, how it is reached, the
/// task list and the conversations open on screen. One stream runs while the
/// app is in the foreground; cached data stays readable without it.
@MainActor @Observable
final class RemoteStore {
    enum Connection: Equatable { case idle, connecting, online, reconnecting, offline, revoked }

    private(set) var library = ConnectionLibrary()
    private(set) var connection: Connection = .idle
    private(set) var connectionDetail: String?
    private(set) var server: ServerInfo?
    private(set) var snapshot = TaskListSnapshot()
    private(set) var hasSnapshot = false
    private(set) var conversations: [String: TaskConversation] = [:]
    private(set) var busy: Set<String> = []
    private(set) var pushError: String?
    private(set) var pairing = false
    /// Changes whenever the active PC does, so views scope their loads to it.
    private(set) var connectionID = UUID()
    var notice: String?
    /// A task to open: a new one, or one a notification named.
    var openTaskRequest: String?
    /// Unsent text per task while the app runs.
    @ObservationIgnored var drafts: [String: String] = [:]

    @ObservationIgnored private var client: RemoteAPIClient?
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var watchedTask: String?
    @ObservationIgnored private var recentTasks: [String] = []
    @ObservationIgnored private var foreground = false
    @ObservationIgnored private var restored = false
    @ObservationIgnored private var fixture = false
    @ObservationIgnored private let keychain = ConnectionKeychain()
    @ObservationIgnored private let cache = SnapshotCache()
    @ObservationIgnored private var monitor: NWPathMonitor?
    @ObservationIgnored private var lastPath: String?

    private struct StreamReset: Error {}

    var activeComputer: SavedComputer? { library.computers.first { $0.id == library.activeID } }
    var isPaired: Bool { activeComputer != nil }
    var computerName: String { activeComputer?.name ?? "tu PC" }
    var isOnline: Bool { connection == .online }
    func task(_ id: String) -> RemoteTask? { snapshot.tasks.first { $0.id == id } }
    func project(_ id: String) -> RemoteProject? { snapshot.projects.first { $0.id == id } }
    func isBusy(_ key: String) -> Bool { busy.contains(key) }
    /// What push registration depends on; the app re-syncs when it changes.
    var pushKey: String {
        "\(library.activeID ?? "")|\(isOnline)|\(server?.push == true)|\(activeComputer?.notificationsEnabled == true)"
    }

    // MARK: Lifecycle

    func restore() {
        guard !restored else { return }
        restored = true
        #if DEBUG
        if let mode = DebugUIFixtures.mode { DebugUIFixtures.load(mode, into: self); return }
        #endif
        keychain.deleteLegacyRecords()
        do { library = try keychain.loadLibrary() ?? ConnectionLibrary() }
        catch { notice = Self.readable(error) }
        activate()
    }

    func setForeground(_ active: Bool) {
        guard !fixture, restored, active != foreground else { return }
        foreground = active
        if active {
            startMonitor()
            restartLoop(probe: true)
        } else {
            stopLoop()
            stopMonitor()
            if let watchedTask { persist(watchedTask) }
        }
    }

    /// The task on screen: the stream follows it. `nil` for the list.
    func watch(_ taskID: String?) {
        guard watchedTask != taskID else { return }
        if let previous = watchedTask { persist(previous) }
        watchedTask = taskID
        if let taskID {
            recentTasks.removeAll { $0 == taskID }
            recentTasks.append(taskID)
            while recentTasks.count > 6 { conversations[recentTasks.removeFirst()] = nil }
            if conversations[taskID] == nil, let server = activeComputer?.id,
               let cached = cache.conversation(task: taskID, server: server) {
                conversations[taskID] = TaskConversation(events: cached.events, pending: cached.pending,
                                                         cursor: cached.cursor, hasMore: cached.hasMore)
            }
        }
        if !fixture { restartLoop(probe: false) }
    }

    func unwatch(_ taskID: String) { if watchedTask == taskID { watch(nil) } }

    func reconnect() { restartLoop(probe: true) }

    func refresh() async {
        guard isOnline, let client else { reconnect(); return }
        do {
            let overview = try await client.overview()
            apply(overview.snapshot)
        } catch { report(error) }
    }

    private func activate() {
        stopLoop()
        client = nil
        server = nil
        conversations = [:]
        recentTasks = []
        busy = []
        pushError = nil
        connectionDetail = nil
        connectionID = UUID()
        guard let computer = activeComputer else {
            snapshot = TaskListSnapshot(); hasSnapshot = false; connection = .idle
            return
        }
        if let cached = cache.snapshot(server: computer.id) { snapshot = cached; hasSnapshot = true }
        else { snapshot = TaskListSnapshot(); hasSnapshot = false }
        connection = computer.revoked ? .revoked : .connecting
        restartLoop(probe: true)
    }

    private func stopLoop() {
        loop?.cancel()
        loop = nil
        generation += 1
    }

    private func restartLoop(probe: Bool) {
        stopLoop()
        guard foreground, !fixture, let computer = activeComputer, !computer.revoked else { return }
        let gen = generation
        let needsProbe = probe || client == nil
        loop = Task { [weak self] in await self?.run(gen, probe: needsProbe) }
    }

    // MARK: The connection loop

    private func run(_ gen: Int, probe: Bool) async {
        var backoff = ReconnectionDelay()
        var needsProbe = probe
        while !Task.isCancelled, gen == generation {
            var healthySince: Date?
            do {
                if needsProbe || client == nil {
                    try await connectRoute(gen)
                    needsProbe = false
                }
                guard let client, gen == generation else { return }
                let watched = watchedTask.flatMap { task($0)?.chat == false ? nil : $0 }
                if let watched, conversations[watched]?.loaded != true {
                    do { try await loadHistory(watched, client: client, gen: gen) }
                    catch let error as RemoteAPIError {
                        // A task just created may have no journal yet: follow it from the start.
                        guard error.status == 404 else { throw error }
                        var empty = TaskConversation()
                        empty.loaded = true
                        conversations[watched] = empty
                    }
                }
                let after = watched.flatMap { conversations[$0]?.cursor } ?? 0
                let bytes = try await client.stream(task: watched, after: after)
                guard gen == generation else { return }
                healthySince = .now
                markOnline()
                for try await message in EventStream.messages(from: bytes) {
                    guard gen == generation else { return }
                    try handle(message, watched: watched)
                }
                throw ConnectionError.streamEnded
            } catch {
                guard gen == generation, !Task.isCancelled else { return }
                if (error as? ConnectionError) == .unauthorized { markRevoked(); return }
                if error is StreamReset { continue }
                if let healthySince, Date.now.timeIntervalSince(healthySince) > 30 { backoff.reset() }
                needsProbe = true
                connection = error is PairingError ? .offline : .reconnecting
                connectionDetail = Self.readable(error)
                do { try await Task.sleep(for: backoff.next()) } catch { return }
            }
        }
    }

    /// Finds the route that answers as this PC, then reads what it says now.
    private func connectRoute(_ gen: Int) async throws {
        guard let computer = activeComputer else { throw ConnectionError.notConnected }
        let found = try await Self.probe(routes: computer.routes, fingerprint: computer.fingerprint, serverID: computer.id)
        guard gen == generation else { throw CancellationError() }
        let authorized = found.authorized(computer.token)
        let overview = try await authorized.overview()
        guard gen == generation else { throw CancellationError() }
        guard overview.server.serverID == computer.id else { throw PairingError.wrongComputer }
        client = authorized
        server = overview.server
        apply(overview.snapshot)
        update(computer.id) { saved in
            saved.learn(routes: overview.server.routes)
            saved.lastRoute = found.baseURL.absoluteString
            if !overview.server.name.isEmpty { saved.name = overview.server.name }
        }
    }

    /// Asks every route at once who answers; the likeliest one that is this
    /// PC wins once every likelier route has failed.
    nonisolated static func probe(routes: [String], fingerprint: String, serverID: String) async throws -> RemoteAPIClient {
        let urls = routes.compactMap { try? RemoteRoute.url($0) }
        guard !urls.isEmpty else { throw PairingError.unreachable }
        let rejectedBefore = PinnedSessions.session(for: fingerprint).1.rejections
        var outcomes = [Bool?](repeating: nil, count: urls.count)
        var otherComputer = false
        let winner: Int? = await withTaskGroup(of: (Int, ServerIdentity?).self) { group in
            for (index, url) in urls.enumerated() {
                group.addTask {
                    (index, try? await RemoteAPIClient(baseURL: url, fingerprint: fingerprint, token: nil).identity(timeout: 3))
                }
            }
            for await (index, identity) in group {
                let matches = identity.map { $0.serverID == serverID && $0.fingerprint.lowercased() == fingerprint.lowercased() } ?? false
                if identity != nil && !matches { otherComputer = true }
                outcomes[index] = matches
                if let first = outcomes.firstIndex(where: { $0 != false }), outcomes[first] == true {
                    group.cancelAll()
                    return first
                }
            }
            return nil
        }
        if let winner { return RemoteAPIClient(baseURL: urls[winner], fingerprint: fingerprint, token: nil) }
        if otherComputer { throw PairingError.wrongComputer }
        if PinnedSessions.session(for: fingerprint).1.rejections > rejectedBefore { throw PairingError.certificateMismatch }
        throw PairingError.unreachable
    }

    private func handle(_ message: SSEMessage, watched: String?) throws {
        let data = Data(message.data.utf8)
        let decoder = JSONDecoder()
        switch message.event {
        case "overview":
            if let snapshot = try? decoder.decode(TaskListSnapshot.self, from: data) { apply(snapshot) }
        case "events":
            if let payload = try? decoder.decode(StreamEventsPayload.self, from: data), payload.task == watched {
                conversations[payload.task, default: TaskConversation()].append(payload.events)
            }
        case "pending":
            if let payload = try? decoder.decode(StreamPendingPayload.self, from: data), payload.task == watched {
                conversations[payload.task, default: TaskConversation()].pending = payload.pending
            }
        case "reset":
            // The journal was rewritten: reload the window, then follow it again.
            if let payload = try? decoder.decode(StreamTaskPayload.self, from: data), payload.task == watched {
                conversations[payload.task]?.loaded = false
                throw StreamReset()
            }
        default:
            break
        }
    }

    private func apply(_ snapshot: TaskListSnapshot) {
        guard !hasSnapshot || snapshot != self.snapshot else { return }
        self.snapshot = snapshot
        hasSnapshot = true
        if let server = activeComputer?.id { cache.save(snapshot, server: server) }
    }

    private func loadHistory(_ task: String, client: RemoteAPIClient, gen: Int) async throws {
        let history = try await client.history(task: task)
        guard gen == generation else { throw CancellationError() }
        var conversation = TaskConversation()
        conversation.replace(with: history)
        conversations[task] = conversation
        persist(task)
    }

    func loadOlder(_ task: String) async {
        guard let client, let conversation = conversations[task], conversation.hasMore, !conversation.loadingOlder,
              let first = conversation.events.first?.seq else { return }
        conversations[task]?.loadingOlder = true
        defer { conversations[task]?.loadingOlder = false }
        do {
            let older = try await client.history(task: task, before: first)
            conversations[task]?.prepend(older)
        } catch { report(error) }
    }

    private func persist(_ task: String) {
        guard !fixture, let server = activeComputer?.id, let conversation = conversations[task], !conversation.events.isEmpty else { return }
        cache.save(conversation, task: task, server: server)
    }

    private func markOnline() {
        connection = .online
        connectionDetail = nil
        // Every task switch reopens the stream; the Keychain hears about it at most every few minutes.
        if let computer = activeComputer, computer.lastConnectedAt.map({ Date.now.timeIntervalSince($0) > 300 }) ?? true {
            update(computer.id) { $0.lastConnectedAt = .now }
        }
    }

    private func markRevoked() {
        guard let id = library.activeID else { return }
        stopLoop()
        client = nil
        update(id) { $0.revoked = true }
        connection = .revoked
        connectionDetail = ConnectionError.unauthorized.errorDescription
    }

    // MARK: Network changes

    private func startMonitor() {
        guard monitor == nil, !fixture else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            let signature = "\(path.status == .satisfied)|" + path.availableInterfaces.map(\.name).joined(separator: ",")
            Task { @MainActor in self?.pathChanged(signature) }
        }
        monitor.start(queue: .global(qos: .utility))
        self.monitor = monitor
    }

    private func stopMonitor() {
        monitor?.cancel()
        monitor = nil
        lastPath = nil
    }

    private func pathChanged(_ signature: String) {
        defer { self.lastPath = signature }
        // The first answer is the path the app started on, not a change.
        guard let lastPath, lastPath != signature, foreground else { return }
        restartLoop(probe: true)
    }

    // MARK: Actions

    private func perform<T>(_ key: String?, _ work: (RemoteAPIClient) async throws -> T) async throws -> T {
        guard let client, connection != .revoked else { throw ConnectionError.notConnected }
        if let key {
            guard !busy.contains(key) else { throw CancellationError() }
            busy.insert(key)
        }
        defer { if let key { busy.remove(key) } }
        do { return try await work(client) }
        catch {
            if (error as? ConnectionError) == .unauthorized { markRevoked() }
            throw error
        }
    }

    private func report(_ error: Error) {
        if error is CancellationError || (error as? URLError)?.code == .cancelled { return }
        notice = Self.readable(error)
    }

    func createTask(project: String, backend: String, prompt: String, files: [String], plan: Bool, requestID: String) async throws -> String {
        let id = try await perform("create") {
            try await $0.createTask(project: project, backend: backend, prompt: prompt, files: files, plan: plan, requestID: requestID)
        }
        openTaskRequest = id
        return id
    }

    /// "started" or "queued": a busy task keeps it on the PC until its turn ends.
    func send(task: String, prompt: String, files: [String], delivery: MessageDelivery, requestID: String) async throws -> String {
        try await perform("send:\(task)") {
            try await $0.send(task: task, prompt: prompt, files: files, delivery: delivery, requestID: requestID)
        }
    }

    func stop(_ task: String) async -> Bool {
        do { try await perform("stop:\(task)") { try await $0.stop(task: task) }; return true }
        catch { report(error); return false }
    }

    func setPinned(_ task: String, _ pinned: Bool) async {
        mutateTask(task) { $0.with(pinned: pinned) }
        do { try await perform(nil) { try await $0.patch(task: task, pinned: pinned) } }
        catch { mutateTask(task) { $0.with(pinned: !pinned) }; report(error) }
    }

    func rename(_ task: String, _ title: String) async {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, let previous = self.task(task)?.title else { return }
        mutateTask(task) { $0.with(title: title) }
        do { try await perform(nil) { try await $0.patch(task: task, title: title) } }
        catch { mutateTask(task) { $0.with(title: previous) }; report(error) }
    }

    func archive(_ task: String) async {
        let previous = snapshot.tasks
        snapshot.tasks.removeAll { $0.id == task }
        do { try await perform(nil) { try await $0.patch(task: task, archived: true) } }
        catch { snapshot.tasks = previous; report(error) }
    }

    func markRead(_ task: String) async {
        guard self.task(task)?.unread == true else { return }
        mutateTask(task) { $0.with(unread: false) }
        do { try await perform("read:\(task)") { try await $0.markRead(task: task) } } catch {}
    }

    func resolve(_ request: PendingRequest, task: String, decision: PermissionDecision) async -> Bool {
        do {
            try await perform("pending:\(request.id)") { try await $0.resolve(task: task, request: request.id, decision: decision) }
            conversations[task]?.pending.removeAll { $0.id == request.id }
            return true
        } catch {
            await answeredElsewhere(error, task: task)
            return false
        }
    }

    func answer(_ request: PendingRequest, task: String, answer: String) async -> Bool {
        do {
            try await perform("pending:\(request.id)") { try await $0.answer(task: task, request: request.id, answer: answer) }
            conversations[task]?.pending.removeAll { $0.id == request.id }
            return true
        } catch {
            await answeredElsewhere(error, task: task)
            return false
        }
    }

    /// 409: someone answered at the PC first. Say so and read what it waits on now.
    private func answeredElsewhere(_ error: Error, task: String) async {
        report(error)
        guard (error as? RemoteAPIError)?.status == 409, let client else { return }
        if let history = try? await client.history(task: task) { conversations[task]?.pending = history.pending }
    }

    func upload(_ data: Data, filename: String, mimeType: String) async throws -> UploadedFile {
        try await perform(nil) { try await $0.upload(data, filename: filename, mimeType: mimeType) }
    }

    func get<Response: Decodable>(_ path: [String], query: [URLQueryItem] = []) async throws -> Response {
        try await perform(nil) { try await $0.get(path, query: query) }
    }

    func send<Response: Decodable>(_ method: String, _ path: [String], body: [String: JSONValue]) async throws -> Response {
        try await perform(nil) { try await $0.send(method, path, body: body) }
    }

    private func mutateTask(_ id: String, _ change: (RemoteTask) -> RemoteTask) {
        guard let index = snapshot.tasks.firstIndex(where: { $0.id == id }) else { return }
        snapshot.tasks[index] = change(snapshot.tasks[index])
    }

    // MARK: Computers

    func pair(_ payload: PairingPayload, deviceName: String, requestID: String) async throws {
        pairing = true
        defer { pairing = false }
        let found = try await Self.probe(routes: payload.routes, fingerprint: payload.fingerprint, serverID: payload.serverID)
        let result = try await found.pair(payload, requestID: requestID, deviceName: deviceName)
        guard result.serverID == payload.serverID, result.fingerprint.lowercased() == payload.fingerprint.lowercased(),
              !result.token.isEmpty else { throw PairingError.wrongComputer }
        var computer = SavedComputer(id: payload.serverID, name: result.name.isEmpty ? payload.label : result.name,
                                     routes: payload.routes, fingerprint: payload.fingerprint.lowercased(),
                                     token: result.token, deviceID: result.deviceID, lastRoute: found.baseURL.absoluteString)
        computer.learn(routes: result.routes)
        var next = library
        next.computers.removeAll { $0.id == computer.id }
        next.computers.append(computer)
        next.activeID = computer.id
        try save(next)
        // The token is durable before the PC forgets this code's retry answer.
        try? await found.authorized(result.token).acknowledgePairing(id: payload.pairingID)
        activate()
    }

    func switchTo(_ id: String) {
        guard library.activeID != id, library.computers.contains(where: { $0.id == id }) else { return }
        var next = library
        next.activeID = id
        do { try save(next); activate() } catch { notice = Self.readable(error) }
    }

    func forget(_ id: String) {
        var next = library
        next.computers.removeAll { $0.id == id }
        let wasActive = next.activeID == id
        if wasActive { next.activeID = next.computers.first?.id }
        do {
            try save(next)
            cache.remove(server: id)
            if wasActive { activate() }
        } catch { notice = Self.readable(error) }
    }

    func setNotifications(_ enabled: Bool, token: String?) async {
        guard let id = library.activeID else { return }
        update(id) { $0.notificationsEnabled = enabled }
        await syncPush(token: token)
    }

    func syncPush(token: String?) async {
        guard let computer = activeComputer, server?.push == true, isOnline else { return }
        let wanted = computer.notificationsEnabled ? token : nil
        guard wanted != computer.registeredPushToken, !(computer.notificationsEnabled && token == nil) else { return }
        do {
            try await perform(nil) { client in
                if let wanted { try await client.registerPush(token: wanted) } else { try await client.removePush() }
            }
            update(computer.id) { $0.registeredPushToken = wanted }
            pushError = nil
        } catch {
            pushError = Self.readable(error)
        }
    }

    func open(_ destination: NotificationDestination) {
        guard let computer = library.computers.first(where: { $0.deviceID == destination.deviceID }) else {
            notice = "Este aviso es de una PC que ya no está emparejada."
            return
        }
        if library.activeID != computer.id { switchTo(computer.id) }
        openTaskRequest = destination.taskID
    }

    private func save(_ next: ConnectionLibrary) throws {
        if !fixture { try keychain.saveLibrary(next) }
        library = next
    }

    private func update(_ id: String, _ change: (inout SavedComputer) -> Void) {
        guard let index = library.computers.firstIndex(where: { $0.id == id }) else { return }
        var next = library
        change(&next.computers[index])
        guard next != library else { return }
        do { try save(next) } catch { notice = Self.readable(error) }
    }

    nonisolated static func readable(_ error: Error) -> String {
        if let error = error as? RemoteAPIError { return error.message }
        if error is URLError { return ConnectionError.readable(error) }
        return error.localizedDescription
    }

    #if DEBUG
    func loadFixture(library: ConnectionLibrary, snapshot: TaskListSnapshot, server: ServerInfo,
                     conversations: [String: TaskConversation], connection: Connection, client: RemoteAPIClient) {
        fixture = true
        self.library = library
        self.snapshot = snapshot
        hasSnapshot = !snapshot.tasks.isEmpty || connection == .online
        self.server = server
        self.conversations = conversations
        self.connection = connection
        connectionDetail = connection == .offline ? "No se pudo contactar a tu PC." : nil
        self.client = client
    }
    #endif
}
