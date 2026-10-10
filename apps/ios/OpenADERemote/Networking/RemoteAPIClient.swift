import Foundation

struct HostHealth: Decodable, Sendable {
    let version: JSONValue
    let model: String
    let provider: String
    let capabilities: [String]?

    func supports(_ capability: String) -> Bool { capabilities?.contains(capability) == true }
}

struct RemoteAPIError: LocalizedError {
    let status: Int
    let code: String
    let message: String
    var errorDescription: String? { message }
}

struct PushRegistration: Decodable, Sendable {
    let registered: Bool
    let deviceID: String
    enum CodingKeys: String, CodingKey { case registered; case deviceID = "device_id" }
}

/// Authenticated requests never follow redirects, including redirects to another host.
private final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

actor RemoteAPIClient {
    private let configuration: ConnectionConfiguration
    private let session: URLSession

    init(configuration: ConnectionConfiguration, transport: URLSession? = nil) {
        self.configuration = configuration
        if let transport { session = transport; return }
        let settings = URLSessionConfiguration.ephemeral
        settings.requestCachePolicy = .reloadIgnoringLocalCacheData
        settings.urlCache = nil
        settings.httpCookieStorage = nil
        settings.urlCredentialStorage = nil
        settings.timeoutIntervalForRequest = 20
        settings.timeoutIntervalForResource = 30
        settings.waitsForConnectivity = false
        session = URLSession(configuration: settings, delegate: NoRedirectDelegate(), delegateQueue: nil)
    }

    func health() async throws -> HostHealth {
        try await get(["v1", "health"])
    }

    func completePairing(_ pending: PendingPairing) async throws -> PairingResult {
        let body = ["pairing_id": pending.payload.pairingID, "secret": pending.payload.secret,
                    "request_id": pending.requestID, "device_name": pending.deviceName]
        return try await perform(path: ["v1", "pairing", "complete"], query: [], body: JSONEncoder().encode(body),
                                 method: "POST", authenticated: false)
    }

    func acknowledgePairing(id: String) async throws {
        let _: JSONValue = try await perform(path: ["v1", "pairing", id, "ack"], query: [], body: nil, method: "POST")
    }

    func setPushToken(_ token: String?) async throws -> PushRegistration {
        let body = try token.map { token in
            try JSONEncoder().encode(["apns_token": token, "environment": "production",
                                      "bundle_id": Bundle.main.bundleIdentifier ?? "dev.openade.remote"])
        }
        return try await perform(path: ["v1", "notifications", "devices"], query: [], body: body,
                                 method: token == nil ? "DELETE" : "POST")
    }

    func projects() async throws -> [RemoteProject] {
        struct Response: Decodable { let projects: [RemoteProject] }
        let response: Response = try await get(["v1", "projects"])
        return response.projects
    }

    func sessions(projectID: String? = nil, search: String = "", pageCount: Int = 1) async throws -> SessionPage {
        var collected: [RemoteSession] = []
        var cursor: String?
        var cursors: Set<String> = []
        var partial = false
        for _ in 0..<max(1, min(pageCount, 50)) {
            var query = [URLQueryItem(name: "limit", value: "100"), URLQueryItem(name: "archived", value: "all")]
            if !search.isEmpty { query.append(URLQueryItem(name: "query", value: search)) }
            if let projectID { query.append(URLQueryItem(name: "project_id", value: projectID)) }
            if let cursor { query.append(URLQueryItem(name: "cursor", value: cursor)) }
            let response: SessionPage = try await get(["v1", "sessions"], query: query)
            partial = partial || response.partial == true
            collected.append(contentsOf: response.sessions)
            guard let next = response.nextCursor, !next.isEmpty else { return SessionPage(sessions: collected, nextCursor: nil, partial: partial) }
            guard cursors.insert(next).inserted else { throw ConnectionError.invalidResponse }
            cursor = next
        }
        return SessionPage(sessions: collected, nextCursor: cursor, partial: partial)
    }

    func session(id: String) async throws -> RemoteSession {
        try await get(["v1", "sessions", id])
    }

    func events(sessionID: String, after cursor: Int64) async throws -> EventPage {
        try await get(["v1", "sessions", sessionID, "events"], query: [
            URLQueryItem(name: "after", value: String(cursor)),
            URLQueryItem(name: "limit", value: "200")
        ])
    }

    func diff(projectID: String) async throws -> ProjectDiff {
        try await get(["v1", "projects", projectID, "diff"])
    }

    func createSession(action: PendingAction) async throws -> RemoteSession {
        try await post(["v1", "sessions"], body: [
            "project_id": action.resourceID,
            "request_id": action.requestID
        ])
    }

    func submitPrompt(action: PendingAction) async throws {
        if action.kind == .steerTurn {
            guard let turnID = action.turnID else { throw ConnectionError.invalidResponse }
            let _: MutationResponse = try await post(["v1", "sessions", action.resourceID, "steer"], body: [
                "request_id": action.requestID,
                "turn_id": turnID,
                "prompt": action.prompt ?? ""
            ])
        } else {
            var body: [String: JSONValue] = [
                "request_id": .string(action.requestID),
                "prompt": .string(action.prompt ?? ""),
                "mode": .string(action.mode?.rawValue ?? "build"),
                "attachment_ids": .array((action.attachmentIDs ?? []).map(JSONValue.string))
            ]
            if let model = action.options?.modelID { body["model_id"] = .string(model) }
            if let effort = action.options?.reasoningEffort { body["reasoning_effort"] = .string(effort) }
            if let policy = action.options?.approvalPolicy { body["approval_policy"] = .string(policy.rawValue) }
            let _: MutationResponse = try await perform(path: ["v1", "sessions", action.resourceID, "turns"],
                                                        query: [], body: JSONEncoder().encode(body), method: "POST")
        }
    }

    func cancelTurn(action: PendingAction) async throws {
        guard let turnID = action.turnID else { throw ConnectionError.invalidResponse }
        let _: MutationResponse = try await post(["v1", "sessions", action.resourceID, "cancel"], body: [
            "request_id": action.requestID,
            "turn_id": turnID
        ])
    }

    func resolvePermission(sessionID: String, requestID: String, allow: Bool) async throws {
        let _: MutationResponse = try await post(["v1", "sessions", sessionID, "permissions", requestID], body: [
            "decision": allow ? "allow" : "deny"
        ])
    }

    private struct MutationResponse: Decodable { let status: String }
    private struct ErrorEnvelope: Decodable {
        struct Detail: Decodable { let code: String; let message: String }
        let error: Detail
    }

    func get<Response: Decodable>(_ path: [String], query: [URLQueryItem] = []) async throws -> Response {
        try await perform(path: path, query: query, body: nil, method: "GET")
    }

    private func post<Response: Decodable>(_ path: [String], body: [String: String]) async throws -> Response {
        try await perform(path: path, query: [], body: JSONEncoder().encode(body), method: "POST")
    }

    func mutate<Response: Decodable>(_ mutation: DurableMutation) async throws -> Response {
        var body = mutation.body
        body["request_id"] = .string(mutation.requestID)
        return try await perform(path: mutation.path, query: [], body: JSONEncoder().encode(body), method: mutation.method)
    }

    private func perform<Response: Decodable>(path: [String], query: [URLQueryItem], body: Data?, method: String,
                                               authenticated: Bool = true) async throws -> Response {
        guard var components = URLComponents(url: configuration.baseURL, resolvingAgainstBaseURL: false) else {
            throw ConnectionError.invalidAddress
        }
        let allowed = CharacterSet.urlPathAllowed.subtracting(CharacterSet(charactersIn: "/%?#"))
        let segments = try path.map { value in
            guard let encoded = value.addingPercentEncoding(withAllowedCharacters: allowed) else {
                throw ConnectionError.invalidResponse
            }
            return encoded
        }
        components.percentEncodedPath = "/" + segments.joined(separator: "/")
        components.queryItems = query.isEmpty ? nil : query
        guard let url = components.url else { throw ConnectionError.invalidAddress }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if authenticated { request.setValue("Bearer \(configuration.token)", forHTTPHeaderField: "Authorization") }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw ConnectionError.invalidResponse }
        if response.statusCode == 401 && authenticated { throw ConnectionError.unauthorized }
        guard (200..<300).contains(response.statusCode) else {
            if let error = try? JSONDecoder().decode(ErrorEnvelope.self, from: data) {
                throw RemoteAPIError(status: response.statusCode, code: error.error.code, message: error.error.message)
            }
            throw ConnectionError.http(response.statusCode)
        }
        do { return try JSONDecoder().decode(Response.self, from: data) }
        catch { throw ConnectionError.invalidResponse }
    }
}
