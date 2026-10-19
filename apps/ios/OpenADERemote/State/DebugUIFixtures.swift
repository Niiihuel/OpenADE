#if DEBUG
import Foundation
import SwiftUI

/// Explicit simulator capture fixtures, compiled out of every Release build.
/// Data goes through the production decoders; files and Git are answered by
/// a local URLProtocol. No host, credential or network is used.
enum DebugUIFixtures {
    static let scenes = ["tasks", "tasks-empty", "tasks-offline", "chat", "chat-queue", "tools", "permission", "question",
                         "cli", "new-task", "settings", "pairing", "lock", "files", "changes", "git-diff", "git-commit",
                         "git-comment", "source", "large"]
    static var mode: String? {
        ProcessInfo.processInfo.arguments.first { $0.hasPrefix("--ui-fixture=") }
            .map { String($0.dropFirst("--ui-fixture=".count)) }
            .flatMap { scenes.contains($0) ? $0 : nil }
    }

    static let taskID = "fixture-task"
    static let terminalTaskID = "fixture-cli"
    static let projectID = "p-openade"
    static let serverID = "6f1c2a9e-3b7d-4f2a-9c1e-5d8b7a6f4e21"
    static let fingerprint = "a3f19c0be27d4c6188e5b0d2f7a19c3e6b4d8f2a1c7e9b0d3f5a2c8e4b6d1f07"

    static var pairingPayload: PairingPayload {
        PairingPayload(version: 2, routes: ["https://192.168.1.229:7787", "https://[2803:9800:9015:4a1::2]:7787", "https://casa.example.com:7787"],
                       fingerprint: fingerprint, serverID: serverID, pairingID: "0b1e6d2c-8f4a-4b3e-9a7d-2c5e1f6a8b90",
                       secret: String(repeating: "7c", count: 32), expiresAt: Int64(Date.now.timeIntervalSince1970) + 540, label: "nixos")
    }

    @MainActor
    static func load(_ mode: String, into store: RemoteStore) {
        let now = Int64(Date.now.timeIntervalSince1970 * 1_000)
        let routes = ["https://192.168.1.229:7787", "https://casa.example.com:7787"]
        let desk = SavedComputer(id: serverID, name: "nixos", routes: routes, fingerprint: fingerprint, token: "fixture-token",
                                 deviceID: "d-fixture", lastRoute: routes[0], lastConnectedAt: .now, notificationsEnabled: true)
        let laptop = SavedComputer(id: "2a7e4c1b-9d3f-4e6a-8b2c-1f5d7e9a3c60", name: "MacBook del estudio",
                                   routes: ["https://10.0.0.12:7787"], fingerprint: String(fingerprint.reversed()),
                                   token: "fixture-token-2", deviceID: "d-fixture-2", lastRoute: "https://10.0.0.12:7787")
        let library = ConnectionLibrary(computers: [desk, laptop], activeID: desk.id)
        let server = ServerInfo(name: "nixos", serverID: serverID, routes: routes, push: true)

        let mainState: String
        switch mode {
        case "permission", "question": mainState = "waiting"
        case "tools": mainState = "idle"
        default: mainState = "running"
        }
        func task(_ id: String, _ title: String, _ project: String, _ agent: String, _ backend: String, minutes: Int64,
                  pinned: Bool = false, unread: Bool = false, state: String = "idle", queued: Int = 0) -> [String: Any] {
            ["id": id, "title": title, "project": project, "agent": agent, "backend": backend,
             "updated_at_ms": now - minutes * 60_000, "pinned": pinned, "unread": unread, "state": state,
             "queued": queued, "chat": backend == "native" || backend.hasPrefix("acp:")]
        }
        let tasks: [[String: Any]] = mode == "tasks-empty" ? [] : [
            task(taskID, "Arreglar la reconexión del chat", projectID, "openADE", "native", minutes: 1, pinned: true,
                 state: mainState, queued: mode == "chat-queue" ? 1 : 0),
            task("t-passkeys", "Migrar el login a passkeys", "p-aurora", "OpenCode", "acp:opencode", minutes: 4, unread: true, state: "waiting"),
            task(terminalTaskID, "Actualizar dependencias de Rust", projectID, "Claude Code", "claude", minutes: 9, state: "running"),
            task("t-settings", "Diseñar la pantalla de ajustes", "p-aurora", "Grok", "acp:grok", minutes: 180, unread: true),
            task("t-perf", "Revisar el informe de rendimiento", "p-rune", "openADE", "native", minutes: 26, state: "running", queued: 2),
            task("t-docs", "Documentar la API v2", projectID, "openADE", "native", minutes: 1_500),
        ]
        let overview: [String: Any] = [
            "tasks": tasks,
            "projects": [["id": projectID, "name": "openADE", "path": "/home/nihuel/projects/personal/openADE"],
                         ["id": "p-aurora", "name": "aurora", "path": "/home/nihuel/projects/aurora"],
                         ["id": "p-rune", "name": "rune", "path": "/home/nihuel/projects/rune"]],
            "agents": [["backend": "native", "name": "openADE"], ["backend": "acp:opencode", "name": "OpenCode"],
                       ["backend": "acp:grok", "name": "Grok"]],
        ]
        do {
            let snapshot = try decode(TaskListSnapshot.self, overview)
            var conversation = TaskConversation()
            conversation.replace(with: try decode(TaskHistory.self, history(mode: mode, now: now)))
            if mode == "chat-queue" { store.drafts[taskID] = "Después corré también las pruebas de UI en el simulador." }
            let client = RemoteAPIClient(baseURL: URL(string: "https://fixture.invalid")!, fingerprint: fingerprint,
                                         token: "fixture-token", transport: transport())
            store.loadFixture(library: library, snapshot: snapshot, server: server, conversations: [taskID: conversation],
                              connection: mode == "tasks-offline" ? .offline : .online, client: client)
        } catch {
            store.notice = "No se pudo cargar la captura de prueba: \(error)"
        }
    }

    private static func decode<T: Decodable>(_ type: T.Type, _ value: Any) throws -> T {
        try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: value))
    }

    static func history(mode: String, now: Int64) -> [String: Any] {
        let start = now - 9 * 60_000
        var seq = 0
        var events: [[String: Any]] = []
        func add(_ type: String, _ turn: String, at offset: Int64, _ fields: [String: Any] = [:]) {
            seq += 1
            var event: [String: Any] = ["seq": seq, "timestamp_ms": start + offset * 1_000, "type": type, "turn_id": turn]
            fields.forEach { event[$0.key] = $0.value }
            events.append(event)
        }
        func tool(_ turn: String, _ id: String, _ name: String, _ arguments: [String: Any], output: String?, at offset: Int64, error: Bool = false) {
            add("tool_started", turn, at: offset, ["tool_call_id": id, "tool_name": name, "arguments": arguments])
            if let output { add("tool_finished", turn, at: offset + 3, ["tool_call_id": id, "text": output, "is_error": error]) }
        }
        add("user_message", "turn-1", at: 0, ["text": "Revisá por qué el chat se corta cuando el iPhone cambia de red."])
        add("assistant_message", "turn-1", at: 4, ["message_id": "turn-1:1", "text": "Voy a mirar cómo se abre el stream y qué pasa al reconectar."])
        tool("turn-1", "c1", "read_file", ["path": "apps/ios/OpenADERemote/State/RemoteStore.swift"], output: "@MainActor @Observable\nfinal class RemoteStore {\n    …\n}", at: 6)
        tool("turn-1", "c2", "read_file", ["path": "apps/ios/OpenADERemote/Networking/EventStream.swift"], output: "struct SSEParser { … }", at: 10)
        tool("turn-1", "c3", "search_text", ["query": "after=", "path": "apps/ios"], output: "RemoteAPIClient.swift:58", at: 14)
        tool("turn-1", "c4", "read_file", ["path": "crates/openide-remote/src/stream.rs"], output: "pub(crate) fn open(…)", at: 18)
        tool("turn-1", "c5", "apply_patch", ["patch": "*** Begin Patch\n*** Update File: apps/ios/OpenADERemote/State/RemoteStore.swift\n@@\n-        cursor = 0\n+        let after = conversations[task]?.cursor ?? 0\n*** Update File: apps/ios/OpenADERemote/Networking/RemoteAPIClient.swift\n*** End Patch"], output: "Done", at: 40)
        tool("turn-1", "c6", "run_command", ["command": "swift test --filter ReconnectionTests"],
             output: "Test Suite 'ReconnectionTests' passed.\nExecuted 8 tests, with 0 failures (0 unexpected) in 0.412 seconds", at: 70)
        add("assistant_message", "turn-1", at: 131, ["message_id": "turn-1:2", "text": """
        Listo: el chat ya no pierde mensajes al cambiar de red.

        ## Qué cambió
        - El stream se vuelve a abrir desde el **último evento** que tiene el teléfono, no desde cero.
        - Si el journal se reescribe, la app recarga la ventana y sigue.
        - Las rutas se prueban en paralelo y gana la primera que responde.

        ```swift
        let bytes = try await client.stream(task: id, after: cursor)
        ```
        """])
        add("turn_completed", "turn-1", at: 134)
        if mode == "tools" { return ["events": events, "pending": [], "cursor": seq, "has_more": true] }

        add("user_message", "turn-2", at: 300, ["text": "Ahora agregá una prueba para cuando el PC reescribe el journal."])
        add("assistant_message", "turn-2", at: 306, ["message_id": "turn-2:1", "text": "Agrego la prueba en `ConversationProjectionTests` y la corro."])
        tool("turn-2", "d1", "read_file", ["path": "apps/ios/OpenADERemoteTests/ConversationProjectionTests.swift"], output: "final class ConversationProjectionTests …", at: 310)
        tool("turn-2", "d2", "edit_file", ["path": "apps/ios/OpenADERemoteTests/ConversationProjectionTests.swift"], output: "Edited", at: 318)
        var pending: [[String: Any]] = []
        switch mode {
        case "permission":
            let arguments: [String: Any] = ["command": "xcodebuild test -scheme OpenADERemote -destination 'platform=iOS Simulator,name=iPhone 16'",
                                            "cwd": "/home/nihuel/projects/personal/openADE/apps/ios"]
            add("tool_started", "turn-2", at: 330, ["tool_call_id": "d3", "tool_name": "run_command", "arguments": arguments])
            add("permission_requested", "turn-2", at: 330, ["request_id": "r-perm", "tool_call_id": "d3", "tool_name": "run_command", "arguments": arguments])
            pending = [["request_id": "r-perm", "tool_call_id": "d3", "tool_name": "run_command", "arguments": arguments, "arguments_truncated": false]]
        case "question":
            let arguments: [String: Any] = ["questions": [
                ["header": "Alcance", "question": "¿Hasta dónde querés que llegue la prueba?", "multi_select": false, "options": [
                    ["label": "Solo el reset", "description": "Comprueba que la ventana se recarga y no duplica mensajes."],
                    ["label": "Reset y reconexión", "description": "También corta el stream a mitad de un turno."],
                    ["label": "Todo, con UI"]]],
                ["header": "Pruebas", "question": "¿Qué corro al terminar?", "multi_select": true, "options": [
                    ["label": "Unitarias"], ["label": "De UI"]]]]]
            add("tool_started", "turn-2", at: 330, ["tool_call_id": "q1", "tool_name": "ask_user", "arguments": arguments])
            pending = [["request_id": "r-question", "tool_call_id": "q1", "tool_name": "ask_user", "arguments": arguments, "arguments_truncated": false]]
        default:
            tool("turn-2", "d3", "run_command", ["command": "xcodebuild test -scheme OpenADERemote -only-testing:OpenADERemoteTests"], output: nil, at: 330)
        }
        return ["events": events, "pending": pending, "cursor": seq, "has_more": true]
    }

    static func transport() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [FixtureURLProtocol.self]
        return URLSession(configuration: config)
    }

    static func data(path: String, query: [URLQueryItem] = []) throws -> Data {
        let base = "/v2/projects/\(projectID)"
        let result: [String: Any]
        switch path {
        case "\(base)/files":
            result = ["path": "", "truncated": false, "entries": [
                ["path": "apps", "name": "apps", "kind": "directory"],
                ["path": "crates", "name": "crates", "kind": "directory"],
                ["path": "docs", "name": "docs", "kind": "directory"],
                ["path": "README.md", "name": "README.md", "kind": "file", "size_bytes": 2_840],
                ["path": "Cargo.toml", "name": "Cargo.toml", "kind": "file", "size_bytes": 1_250],
                ["path": "CHANGELOG.md", "name": "CHANGELOG.md", "kind": "file", "size_bytes": 764]
            ]]
        case "\(base)/file":
            result = ["path": query.first { $0.name == "path" }?.value ?? "apps/ios/OpenADERemote/Networking/EventStream.swift",
                      "revision": "fixture-file-revision", "language": "swift", "truncated": false, "content": """
            import Foundation

            /// Server-sent events, fed byte by byte or in chunks of any size.
            struct SSEParser {
                private var line: [UInt8] = []
                private var event = ""
                private var data: [String] = []

                mutating func push(_ byte: UInt8) -> SSEMessage? {
                    guard byte == 10 || byte == 13 else {
                        line.append(byte)
                        return nil
                    }
                    let text = String(decoding: line, as: UTF8.self)
                    line.removeAll(keepingCapacity: true)
                    return process(text)
                }
            }
            """]
        case "\(base)/git/status":
            result = ["revision": "fixture-revision", "branch": "wt/remote-ios", "clean": false, "commit": NSNull(), "files": [
                ["path": "apps/ios/OpenADERemote/State/RemoteStore.swift", "index_status": "M", "worktree_status": " ", "staged": true, "untracked": false],
                ["path": "apps/ios/OpenADERemote/Networking/RemoteAPIClient.swift", "index_status": " ", "worktree_status": "M", "staged": false, "untracked": false],
                ["path": "apps/ios/OpenADERemoteTests/ProtocolTests.swift", "index_status": "?", "worktree_status": "?", "staged": false, "untracked": true]
            ]]
        case "\(base)/git/diff":
            result = ["path": query.first { $0.name == "path" }?.value ?? "apps/ios/OpenADERemote/State/RemoteStore.swift", "truncated": false, "diff": """
            diff --git a/apps/ios/OpenADERemote/State/RemoteStore.swift b/apps/ios/OpenADERemote/State/RemoteStore.swift
            index 1abc234..5def678 100644
            --- a/apps/ios/OpenADERemote/State/RemoteStore.swift
            +++ b/apps/ios/OpenADERemote/State/RemoteStore.swift
            @@ -118,7 +118,9 @@ final class RemoteStore {
                     let watched = watchedTask
            -        let after: Int64 = 0
            +        // Follow the journal from the last event this phone has.
            +        let after = watched.flatMap { conversations[$0]?.cursor } ?? 0
                     let bytes = try await client.stream(task: watched, after: after)
            -        connection = .online
            +        healthySince = .now
            +        markOnline()
                     for try await message in EventStream.messages(from: bytes) {
            @@ -204,3 +206,4 @@ final class RemoteStore {
                 private func persist(_ task: String) {
            +        guard let server = activeComputer?.id else { return }
                     cache.save(conversation, task: task, server: server)
                 }
            """]
        default: throw URLError(.unsupportedURL)
        }
        return try JSONSerialization.data(withJSONObject: result)
    }
}

private final class FixtureURLProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let url = request.url else { client?.urlProtocol(self, didFailWithError: URLError(.badURL)); return }
        do {
            let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
            // Mutations succeed without effect: captures never change anything.
            let data = request.httpMethod == "GET" ? try DebugUIFixtures.data(path: url.path, query: query) : Data("{}".utf8)
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

    var body: some View {
        switch mode {
        case "tasks", "tasks-empty", "tasks-offline", "lock": HomeView()
        case "new-task": HomeView().sheet(isPresented: .constant(true)) { NewTaskView().remoteSheet() }
        case "settings": HomeView().sheet(isPresented: .constant(true)) { SettingsView().remoteSheet() }
        case "pairing":
            WelcomeView().sheet(isPresented: .constant(true)) { PairingReviewView(payload: DebugUIFixtures.pairingPayload).remoteSheet() }
        case "files", "changes":
            WorkspacePanelView(projectID: DebugUIFixtures.projectID, initialTab: mode == "files" ? .files : .changes)
        case "git-diff", "git-commit", "git-comment":
            GitWorkspaceView(projectID: DebugUIFixtures.projectID, onComment: { _ in }, initialPanel: mode == "git-commit" ? "commit" : "diff")
        case "source":
            FileEditorView(projectID: DebugUIFixtures.projectID, path: "apps/ios/OpenADERemote/Networking/EventStream.swift")
        case "large":
            NavigationStack { TaskDetailView(taskID: DebugUIFixtures.taskID) }
                .environment(\.dynamicTypeSize, .accessibility3)
        default:
            NavigationStack {
                TaskDetailView(taskID: mode == "cli" ? DebugUIFixtures.terminalTaskID : DebugUIFixtures.taskID)
            }
        }
    }
}
#endif
