import Foundation

/// Talks to one route of one PC over its pinned session. The token is sent
/// only on that session, so it never reaches a server whose pin did not match.
final class RemoteAPIClient: @unchecked Sendable {
    let baseURL: URL
    let fingerprint: String
    private let token: String?
    private let session: URLSession

    init(baseURL: URL, fingerprint: String, token: String?) {
        self.baseURL = baseURL
        self.fingerprint = fingerprint
        self.token = token
        session = PinnedSessions.session(for: fingerprint).0
    }

    #if DEBUG
    /// Captures only: a local URLProtocol answers, never a PC, and no token is sent.
    init(fixtureURL: URL, transport: URLSession) {
        baseURL = fixtureURL
        fingerprint = ""
        token = nil
        session = transport
    }
    #endif

    func authorized(_ token: String) -> RemoteAPIClient {
        RemoteAPIClient(baseURL: baseURL, fingerprint: fingerprint, token: token)
    }

    // MARK: Pairing and identity

    func identity(timeout: TimeInterval = 3) async throws -> ServerIdentity {
        try await perform(makeRequest([".well-known", "openade"], timeout: timeout, authenticated: false))
    }

    func pair(_ payload: PairingPayload, requestID: String, deviceName: String) async throws -> PairingResult {
        let body: [String: JSONValue] = ["pairing_id": .string(payload.pairingID), "secret": .string(payload.secret),
                                         "request_id": .string(requestID), "device_name": .string(deviceName)]
        return try await perform(makeRequest(["v2", "pair"], method: "POST", body: body, authenticated: false))
    }

    func acknowledgePairing(id: String) async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "pair", id, "ack"], method: "POST", body: [:]))
    }

    // MARK: Tasks

    func overview() async throws -> OverviewResponse {
        try await perform(makeRequest(["v2", "overview"]))
    }

    func history(task: String, before: Int64? = nil) async throws -> TaskHistory {
        try await perform(makeRequest(["v2", "tasks", task, "history"], query: before.map { [URLQueryItem(name: "before", value: String($0))] } ?? []))
    }

    /// The live stream: the task list, and the watched task's new events.
    func stream(task: String?, after: Int64) async throws -> URLSession.AsyncBytes {
        var query = [URLQueryItem(name: "after", value: String(max(after, 0)))]
        if let task { query.insert(URLQueryItem(name: "task", value: task), at: 0) }
        // Keep-alives arrive every 15 s: silence for longer means the route is gone.
        var request = try makeRequest(["v2", "stream"], query: query, timeout: 45)
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw ConnectionError.invalidResponse }
        if http.statusCode == 401 { throw ConnectionError.unauthorized }
        guard (200..<300).contains(http.statusCode) else { throw ConnectionError.http(http.statusCode) }
        return bytes
    }

    func createTask(project: String, backend: String, prompt: String, files: [String], plan: Bool, requestID: String) async throws -> String {
        struct Created: Decodable { let task: String }
        let created: Created = try await perform(makeRequest(["v2", "tasks"], method: "POST", body: [
            "request_id": .string(requestID), "project": .string(project), "backend": .string(backend),
            "prompt": .string(prompt), "files": .array(files.map(JSONValue.string)), "plan": .bool(plan)
        ]))
        return created.task
    }

    func send(task: String, prompt: String, files: [String], delivery: MessageDelivery, requestID: String) async throws -> String {
        struct Sent: Decodable { let delivered: String }
        let sent: Sent = try await perform(makeRequest(["v2", "tasks", task, "messages"], method: "POST", body: [
            "request_id": .string(requestID), "prompt": .string(prompt),
            "files": .array(files.map(JSONValue.string)), "delivery": .string(delivery.rawValue)
        ]))
        return sent.delivered
    }

    func stop(task: String) async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "tasks", task, "stop"], method: "POST",
                                                     body: ["request_id": .string(UUID().uuidString)]))
    }

    func patch(task: String, title: String? = nil, pinned: Bool? = nil, archived: Bool? = nil) async throws {
        var body: [String: JSONValue] = ["request_id": .string(UUID().uuidString)]
        if let title { body["title"] = .string(title) }
        if let pinned { body["pinned"] = .bool(pinned) }
        if let archived { body["archived"] = .bool(archived) }
        let _: JSONValue = try await perform(makeRequest(["v2", "tasks", task], method: "PATCH", body: body))
    }

    func markRead(task: String) async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "tasks", task, "read"], method: "POST", body: [:]))
    }

    func resolve(task: String, request requestID: String, decision: PermissionDecision, always: String? = nil) async throws {
        var body: [String: JSONValue] = ["decision": .string(decision.rawValue)]
        if decision == .allowAlways, let always { body["always"] = .string(always) }
        let _: JSONValue = try await perform(makeRequest(["v2", "tasks", task, "permissions", requestID], method: "POST", body: body))
    }

    func answer(task: String, request requestID: String, answer: String) async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "tasks", task, "answers", requestID], method: "POST",
                                                     body: ["answer": .string(answer)]))
    }

    func upload(_ data: Data, filename: String, mimeType: String) async throws -> UploadedFile {
        var request = try makeRequest(["v2", "uploads"], method: "POST", timeout: 180)
        request.setValue(mimeType, forHTTPHeaderField: "Content-Type")
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        request.setValue(filename.addingPercentEncoding(withAllowedCharacters: allowed) ?? "archivo", forHTTPHeaderField: "X-Filename")
        let (body, response) = try await session.upload(for: request, from: data)
        return try decode(body, response)
    }

    func registerPush(token: String) async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "notifications", "devices"], method: "POST", body: [
            // The app's entitlement asks for the production APNs environment in every build.
            "apns_token": .string(token), "environment": .string("production"),
            "bundle_id": .string(Bundle.main.bundleIdentifier ?? "dev.openade.remote")
        ]))
    }

    func removePush() async throws {
        let _: JSONValue = try await perform(makeRequest(["v2", "notifications", "devices"], method: "DELETE", body: [:]))
    }

    // MARK: Projects (files and Git keep their v1 shapes)

    func get<Response: Decodable>(_ path: [String], query: [URLQueryItem] = []) async throws -> Response {
        try await perform(makeRequest(path, query: query))
    }

    func send<Response: Decodable>(_ method: String, _ path: [String], body: [String: JSONValue]) async throws -> Response {
        var body = body
        body["request_id"] = .string(UUID().uuidString)
        return try await perform(makeRequest(path, method: method, body: body))
    }

    // MARK: Plumbing

    private struct ErrorEnvelope: Decodable {
        struct Detail: Decodable { let code: String; let message: String }
        let error: Detail
    }

    private func makeRequest(_ path: [String], query: [URLQueryItem] = [], method: String = "GET",
                         body: [String: JSONValue]? = nil, timeout: TimeInterval = 20,
                         authenticated: Bool = true) throws -> URLRequest {
        guard var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false) else { throw ConnectionError.invalidRoute }
        let allowed = CharacterSet.urlPathAllowed.subtracting(CharacterSet(charactersIn: "/%?#;"))
        let segments = try path.map { value in
            guard let encoded = value.addingPercentEncoding(withAllowedCharacters: allowed) else { throw ConnectionError.invalidRoute }
            return encoded
        }
        components.percentEncodedPath = "/" + segments.joined(separator: "/")
        components.queryItems = query.isEmpty ? nil : query
        guard let url = components.url else { throw ConnectionError.invalidRoute }
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if authenticated, let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return request
    }

    private func perform<Response: Decodable>(_ request: URLRequest) async throws -> Response {
        let (data, response) = try await session.data(for: request)
        return try decode(data, response, authenticated: request.value(forHTTPHeaderField: "Authorization") != nil)
    }

    private func decode<Response: Decodable>(_ data: Data, _ response: URLResponse, authenticated: Bool = true) throws -> Response {
        guard let response = response as? HTTPURLResponse else { throw ConnectionError.invalidResponse }
        // Pairing answers 401 for a bad code; only a token's 401 means revoked.
        if response.statusCode == 401 && authenticated { throw ConnectionError.unauthorized }
        guard (200..<300).contains(response.statusCode) else {
            if let envelope = try? JSONDecoder().decode(ErrorEnvelope.self, from: data) {
                throw RemoteAPIError(status: response.statusCode, code: envelope.error.code, message: envelope.error.message)
            }
            throw ConnectionError.http(response.statusCode)
        }
        do { return try JSONDecoder().decode(Response.self, from: data) }
        catch { throw ConnectionError.invalidResponse }
    }
}
