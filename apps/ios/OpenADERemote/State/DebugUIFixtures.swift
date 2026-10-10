#if DEBUG
import Foundation
import SwiftUI

/// Explicit simulator capture fixture, compiled out of every Release build.
/// URLProtocol handles all requests locally; no host, credential or network is used.
enum DebugUIFixtures {
    static var mode: String? {
        ProcessInfo.processInfo.arguments.first { $0.hasPrefix("--ui-fixture=") }
            .map { String($0.dropFirst("--ui-fixture=".count)) }
            .flatMap { ["chat", "files", "changes", "projects", "empty", "new-task", "composer", "attachments", "permissions", "models", "advanced", "connections", "large", "git-diff", "git-commit", "git-comment", "permissions-large", "models-large", "source"].contains($0) ? $0 : nil }
    }
    static let sessionID = "fixture-session"
    static func transport() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [FixtureURLProtocol.self]
        return URLSession(configuration: config)
    }
    static func decode<T: Decodable>(_ type: T.Type, path: String) throws -> T {
        try JSONDecoder().decode(type, from: data(path: path))
    }
    static func data(path: String, query: [URLQueryItem] = []) throws -> Data {
        let now = Int64(Date.now.timeIntervalSince1970 * 1000)
        let session: [String: Any] = ["id": sessionID, "project_id": "fixture-project", "title": "Revisar reconexión del chat",
            "status": "completed", "updated_at_ms": now, "pending_permissions": [], "archived": false, "favorite": true,
            "origin": "remote", "can_start_turn": true, "can_control_active": false]
        let result: [String: Any]
        switch path {
        case "/v1/health":
            result = ["version": "1", "model": "gpt-5.6-luna", "provider": "fixture",
                      "capabilities": ["sessions.metadata", "models.list", "workspace.files.read", "workspace.files.write",
                                       "workspace.git.read", "workspace.git.stage", "workspace.git.commit", "attachments.text", "attachments.images", "turn.options", "turn.approval_policy"]]
        case "/v1/projects":
            result = ["projects": [["id": "fixture-project", "name": "openADE", "path": "/fixture/openADE"]]]
        case "/v1/sessions":
            var second = session
            second["id"] = "fixture-session-2"; second["title"] = "Mejorar la vista de archivos"; second["favorite"] = false
            second["updated_at_ms"] = now - 3_600_000
            var third = session
            third["id"] = "fixture-session-3"; third["title"] = "Revisar cambios de autenticación"; third["favorite"] = false
            third["updated_at_ms"] = now - 7_200_000; third["origin"] = "desktop"; third["can_start_turn"] = false
            result = ["sessions": mode == "empty" ? [] : [session, second, third], "next_cursor": NSNull()]
        case "/v1/sessions/\(sessionID)": result = session
        case "/v1/sessions/\(sessionID)/events":
            let after = Int(query.first { $0.name == "after" }?.value ?? "0") ?? 0
            let events: [[String: Any]] = [
                ["seq": 1, "timestamp_ms": now - 10000, "type": "user_message", "turn_id": "turn-1", "text": "Revisá cómo se recupera el chat cuando el iPhone pierde conexión."],
                ["seq": 2, "timestamp_ms": now - 9000, "type": "assistant_message", "turn_id": "turn-1", "message_id": "message-1", "text": "Revisé la reconexión y el registro de eventos.\n\n## Cambios realizados\n- El cursor se conserva entre reintentos.\n- Los mensajes confirmados no se duplican.\n- El borrador permanece disponible sin conexión.\n\n```swift\nawait store.refreshSession(id: sessionID)\n```"],
                ["seq": 3, "timestamp_ms": now - 8000, "type": "tool_started", "turn_id": "turn-1", "tool_call_id": "tool-1", "tool_name": "run_command", "arguments": ["command": "swift test"]],
                ["seq": 4, "timestamp_ms": now - 7000, "type": "tool_finished", "turn_id": "turn-1", "tool_call_id": "tool-1", "tool_name": "run_command", "text": "Test Suite 'RemoteStateTests' passed.\nExecuted 8 tests, with 0 failures.", "is_error": false],
                ["seq": 5, "timestamp_ms": now - 6000, "type": "assistant_message", "turn_id": "turn-1", "message_id": "message-2", "text": "Las pruebas pasaron. Podés revisar los archivos y el diff en **Espacio de trabajo** antes de crear un commit."],
                ["seq": 6, "timestamp_ms": now - 5000, "type": "turn_completed", "turn_id": "turn-1"]
            ]
            let showConversation = mode == nil || mode == "chat" || mode == "files" || mode == "changes" || mode == "large"
            result = ["events": showConversation ? events.filter { ($0["seq"] as? Int ?? 0) > after } : [], "next_cursor": showConversation ? 6 : 0, "has_more": false]
        case "/v1/projects/fixture-project/files":
            result = ["path": "", "truncated": false, "entries": [
                ["path": "Sources", "name": "Sources", "kind": "directory"],
                ["path": "Tests", "name": "Tests", "kind": "directory"],
                ["path": "README.md", "name": "README.md", "kind": "file", "size_bytes": 2840],
                ["path": "Package.swift", "name": "Package.swift", "kind": "file", "size_bytes": 1250],
                ["path": "CHANGELOG.md", "name": "CHANGELOG.md", "kind": "file", "size_bytes": 764]
            ]]
        case "/v1/projects/fixture-project/file":
            result = ["path": query.first { $0.name == "path" }?.value ?? "Sources/RemoteStore.swift",
                      "revision": "fixture-file-revision", "language": "swift", "truncated": false, "content": """
            import Foundation

            @MainActor
            final class RemoteStore {
                private var cursor = 0
                private var connectionState = "connected"
                private var drafts: [String: String] = [:]

                // Conservá el historial después de reconectar.
                func reconnect() async throws {
                    let savedCursor = cursor
                    guard !Task.isCancelled else { return }
                    try await connect(after: savedCursor)
                    connectionState = "connected"
                }

                func saveDraft(_ text: String, sessionID: String) {
                    drafts[sessionID] = text
                    persistLibrary()
                }
            }
            """]
        case "/v1/projects/fixture-project/git/status":
            result = ["revision": "fixture-revision", "branch": "feature/mobile-workspace", "clean": false, "commit": NSNull(), "files": [
                ["path": "Sources/RemoteStore.swift", "index_status": "M", "worktree_status": " ", "staged": true, "untracked": false],
                ["path": "Sources/SessionDetailView.swift", "index_status": " ", "worktree_status": "M", "staged": false, "untracked": false],
                ["path": "Tests/ReconnectionTests.swift", "index_status": "?", "worktree_status": "?", "staged": false, "untracked": true]
            ]]
        case "/v1/projects/fixture-project/git/diff":
            result = ["path": query.first { $0.name == "path" }?.value ?? "Sources/RemoteStore.swift", "truncated": false, "diff": """
            diff --git a/Sources/RemoteStore.swift b/Sources/RemoteStore.swift
            index 1abc234..5def678 100644
            --- a/Sources/RemoteStore.swift
            +++ b/Sources/RemoteStore.swift
            @@ -18,5 +18,8 @@ final class RemoteStore {
                 func reconnect() async {
            -        cursor = 0
            +        let savedCursor = cursor
            +        guard !Task.isCancelled else { return }
                     do {
            -            try await connect()
            +            try await connect(after: savedCursor)
            +            connectionState = .connected
            +            notice = nil
                     } catch {
            @@ -36,3 +39,4 @@ final class RemoteStore {
                 func saveDraft(_ text: String) {
            +        drafts[sessionID] = text
                     persistLibrary()
                 }
            """]
        case "/v1/models":
            result = ["models": [
                ["id": "gpt-5.6-luna", "name": "GPT-5.6 Luna", "provider": "fixture", "reasoning_efforts": ["low", "medium", "high", "xhigh", "max"], "supports_images": true],
                ["id": "gpt-5.6-sol", "name": "GPT-5.6 Sol", "provider": "fixture", "reasoning_efforts": ["low", "medium", "high", "xhigh", "max", "ultra"], "supports_images": true]
            ], "default_model_id": "gpt-5.6-luna", "selection_enabled": true]
        default: throw URLError(.unsupportedURL)
        }
        return try JSONSerialization.data(withJSONObject: result)
    }
}

private final class FixtureURLProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            guard request.httpMethod == "GET", let url = request.url else { throw URLError(.unsupportedURL) }
            let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
            let data = try DebugUIFixtures.data(path: url.path, query: query)
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

struct DebugFixtureScreen: View {
    let mode: String
    @Environment(RemoteStore.self) private var store
    var body: some View {
        if !store.isPaired { ProgressView().task { store.restore() } }
        else if mode == "projects" || mode == "empty" { WorkspaceView() }
        else if mode == "connections" { ConnectionsView() }
        else if mode == "new-task" { NewTaskProjectView { _ in } }
        else if mode == "source" { FileEditorView(projectID: "fixture-project", path: "Sources/RemoteStore.swift") }
        else if mode == "git-diff" || mode == "git-commit" || mode == "git-comment" {
            GitWorkspaceView(projectID: "fixture-project", onComment: { _ in }, initialPanel: mode == "git-commit" ? "commit" : "diff")
        }
        else if mode == "permissions-large" || mode == "models-large" {
            NavigationStack {
                SessionDetailView(sessionID: DebugUIFixtures.sessionID,
                                  initialPanel: mode == "permissions-large" ? "permissions" : "models")
            }.environment(\.dynamicTypeSize, .accessibility3)
        }
        else if mode == "large" {
            NavigationStack { SessionDetailView(sessionID: DebugUIFixtures.sessionID) }
                .environment(\.dynamicTypeSize, .accessibility3)
        }
        else {
            NavigationStack {
                if ["chat", "composer", "attachments", "permissions", "models", "advanced"].contains(mode) {
                    SessionDetailView(sessionID: DebugUIFixtures.sessionID,
                                      initialPanel: ["attachments", "permissions", "models", "advanced"].contains(mode) ? mode : nil)
                }
                else {
                    WorkspacePanelView(projectID: "fixture-project", sessionID: DebugUIFixtures.sessionID,
                                       initialTab: mode == "files" ? .files : .changes)
                        .navigationTitle("openADE · \(mode == "files" ? "Archivos" : "Cambios")")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }
        }
    }
}
#endif
