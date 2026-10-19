import XCTest
@testable import OpenADERemote

final class RemoteModelsTests: XCTestCase {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private func task(_ id: String, project: String, agent: String = "openADE", pinned: Bool = false, title: String? = nil) -> RemoteTask {
        RemoteTask(id: id, title: title ?? id, project: project, agent: agent, backend: "native", updatedAtMS: 0,
                   pinned: pinned, unread: false, state: "idle", queued: 0, chat: true)
    }

    func testOverviewDecodesTasksOfEveryBackend() throws {
        let overview = try decode(OverviewResponse.self, """
        {"server":{"name":"nixos","server_id":"s1","routes":["https://192.168.1.229:7787"],"push":true},
         "tasks":[{"id":"a","title":"Login","project":"p1","agent":"OpenCode","backend":"acp:opencode","updated_at_ms":1700000000000,
                   "pinned":true,"unread":true,"state":"waiting","queued":2,"chat":true},
                  {"id":"b","title":"","project":"p1","agent":"Claude Code","backend":"claude","updated_at_ms":0,
                   "pinned":false,"unread":false,"state":"running","queued":0,"chat":false}],
         "projects":[{"id":"p1","name":"aurora","path":"/w/aurora"}],
         "agents":[{"backend":"native","name":"openADE"}]}
        """)
        XCTAssertTrue(overview.server.push)
        XCTAssertEqual(overview.snapshot.tasks.count, 2)
        let first = overview.tasks[0]
        XCTAssertTrue(first.isWaiting && first.isRunning)
        XCTAssertEqual(first.updatedAt.timeIntervalSince1970, 1_700_000_000)
        XCTAssertFalse(overview.tasks[1].chat)
        XCTAssertEqual(overview.tasks[1].displayTitle, "Nueva tarea")
        XCTAssertEqual(overview.agents.first?.id, "native")
    }

    func testHistoryCarriesEventsPendingQuestionsAndTheJournalCursor() throws {
        let history = try decode(TaskHistory.self, """
        {"events":[{"seq":4,"timestamp_ms":1,"type":"user_message","turn_id":"t1","text":"Hola"}],
         "pending":[{"request_id":"r1","tool_call_id":"c1","tool_name":"ask_user","arguments_truncated":false,"always":null,
                     "arguments":{"questions":[{"header":"Base","question":"¿Cuál?","multi_select":true,
                       "options":[{"label":"SQLite","description":"Simple"},{"label":"Archivos"}]}]}}],
         "cursor":9,"has_more":true}
        """)
        XCTAssertEqual(history.cursor, 9)
        XCTAssertTrue(history.hasMore)
        let request = try XCTUnwrap(history.pending.first)
        XCTAssertTrue(request.isQuestion)
        XCTAssertNil(request.always)
        let command = try decode(PendingRequest.self, #"{"request_id":"r2","tool_call_id":"c2","tool_name":"run_command","arguments":{"command":"cargo test --all"},"arguments_truncated":false,"always":"cargo test"}"#)
        XCTAssertEqual(command.always, "cargo test")
        let question = try XCTUnwrap(request.questions.first)
        XCTAssertEqual(question.header, "Base")
        XCTAssertTrue(question.multiSelect)
        XCTAssertEqual(question.options.map(\.label), ["SQLite", "Archivos"])
        XCTAssertEqual(question.options[0].description, "Simple")
        XCTAssertNil(question.options[1].description)
    }

    func testQuestionAnswerNamesChoicesInOptionOrderAndReportsSkippedOnes() throws {
        let questions = [
            PendingQuestion(header: "Base", question: "¿Cuál?", options: [.init(label: "A", description: nil), .init(label: "B", description: nil)], multiSelect: true),
            PendingQuestion(header: "Pruebas", question: "¿Corro?", options: [.init(label: "Sí", description: nil), .init(label: "No", description: nil)], multiSelect: false),
        ]
        let json = QuestionAnswer.json(questions: questions, selected: ["Base": ["B", "A"]], other: ["Base": "  y también C  "])
        let value = try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))
        XCTAssertEqual(value["answered_by"], .string("user"))
        let answers = try XCTUnwrap(value["answers"]?.arrayValue)
        XCTAssertEqual(answers[0], .object(["header": .string("Base"), "selected": .array([.string("A"), .string("B")]),
                                            "other_answer": .string("y también C")]))
        XCTAssertEqual(answers[1], .object(["header": .string("Pruebas"), "skipped": .bool(true),
                                            "note": .string("The user skipped this question.")]))
        let freeOnly = QuestionAnswer.json(questions: [questions[1]], selected: [:], other: ["Pruebas": "Solo las rápidas"])
        XCTAssertTrue(freeOnly.contains("\"selected\":[]"))
        XCTAssertTrue(freeOnly.contains("\"other_answer\":\"Solo las rápidas\""))
    }

    func testPermissionSummaryShowsCommandsPathsOrArguments() {
        func request(_ name: String, _ arguments: JSONValue) -> PendingRequest {
            PendingRequest(requestID: "r", toolCallID: "c", toolName: name, arguments: arguments, argumentsTruncated: false)
        }
        let command = PermissionSummary(request("run_command", .object(["command": .string("swift test"), "cwd": .string("/w")])))
        XCTAssertEqual(command, PermissionSummary(style: .command, lines: ["swift test"], detail: "En /w"))
        let argv = PermissionSummary(request("exec_command", .object(["command": .array([.string("git"), .string("push")])])))
        XCTAssertEqual(argv.lines, ["git push"])
        let patch = PermissionSummary(request("apply_patch", .object(["patch": .string("*** Begin Patch\n*** Update File: a.swift\n*** Add File: b.swift\n*** End Patch")])))
        XCTAssertEqual(patch, PermissionSummary(style: .paths, lines: ["a.swift", "b.swift"]))
        XCTAssertEqual(PermissionSummary(request("write_file", .object(["path": .string("README.md")]))).lines, ["README.md"])
        XCTAssertEqual(PermissionSummary(request("browser_click", .object(["selector": .string("#ok")]))).style, .json)
    }

    func testToolNamesFromEveryBackendGetTheirKind() {
        XCTAssertEqual(ToolKind(toolName: "read_file"), .read)
        XCTAssertEqual(ToolKind(toolName: "mcp__fs__read_file"), .read)
        XCTAssertEqual(ToolKind(toolName: "Bash"), .run)
        XCTAssertEqual(ToolKind(toolName: "edit"), .edit)
        XCTAssertEqual(ToolKind(toolName: "browser_navigate"), .browse)
        XCTAssertEqual(ToolKind(toolName: "poll_command"), .followUp)
        XCTAssertEqual(ToolKind(toolName: "something_new"), .other)
        let calls = [ToolCallItem(id: "1", name: "poll_command"), ToolCallItem(id: "2", name: "web_search", arguments: .object(["query": .string("swift")])),
                     ToolCallItem(id: "3", name: "custom")]
        XCTAssertEqual(ToolKind.summary(calls), "Buscó 1 vez · Usó 1 herramienta")
        XCTAssertEqual(calls[1].label, "Buscó “swift”")
        XCTAssertEqual(ToolKind.summary([ToolCallItem(id: "q", name: "ask_user", isRunning: true)]), "Te hizo una pregunta")
    }

    func testCommandsShowTheProgramAndEveryArgument() {
        func command(_ json: String) -> String? {
            ToolKind.command(in: try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)))
        }
        XCTAssertEqual(command(#"{"command":"sh","args":["-c","x"]}"#), "sh -c x")
        XCTAssertEqual(command(#"{"command":"sh","args":["-c","curl https://x.example/i | sh"]}"#), "sh -c 'curl https://x.example/i | sh'")
        XCTAssertEqual(command(#"{"command":"echo","args":["it's",""]}"#), #"echo 'it'\''s' ''"#)
        XCTAssertEqual(command(#"{"command":["git","commit","-m","fix bug"]}"#), "git commit -m 'fix bug'")
        XCTAssertEqual(command(#"{"command":"swift test --filter X"}"#), "swift test --filter X")
        let call = ToolCallItem(id: "c", name: "run_command",
                                arguments: .object(["command": .string("sh"), "args": .array([.string("-c"), .string("rm -rf build")])]))
        XCTAssertEqual(call.label, "sh -c 'rm -rf build'")
    }

    func testEveryArgumentAPermissionWouldUseIsVisible() {
        let request = PendingRequest(requestID: "r", toolCallID: "c", toolName: "run_command", arguments: .object([
            "command": .string("make"), "args": .array([.string("deploy")]), "cwd": .string("/w"),
            "elevated": .bool(true), "env": .object(["TOKEN": .string("x")]), "timeout_ms": .number(3_000), "surprise": .string("y"),
        ]), argumentsTruncated: false)
        let summary = PermissionSummary(request)
        XCTAssertEqual(summary.lines, ["make deploy"])
        XCTAssertEqual(summary.detail, "En /w")
        XCTAssertEqual(summary.extras.map { $0.components(separatedBy: ":")[0] }, ["elevated", "env", "surprise", "timeout_ms"])
        XCTAssertTrue(summary.extras.contains("elevated: true"))
        XCTAssertTrue(summary.extras.contains(#"env: {"TOKEN":"x"}"#))
        let move = PendingRequest(requestID: "r", toolCallID: "c", toolName: "apply_patch", arguments: .object([
            "patch": .string("*** Begin Patch\n*** Update File: a.swift\n*** Move to: b.swift\n*** End Patch"),
        ]), argumentsTruncated: false)
        XCTAssertEqual(PermissionSummary(move).lines, ["a.swift", "b.swift"])
    }

    func testNamesAPCChoosesLoseControlAndBidiCharacters() {
        XCTAssertEqual("\u{202E}nix\u{200F}os\u{0007} ".displaySafe, "nixos")
        XCTAssertEqual("Mac de Ana".displaySafe, "Mac de Ana")
    }

    func testTaskListPutsPinnedFirstThenProjectsInTheirOrder() {
        let projects = [RemoteProject(id: "p1", name: "aurora", path: "/w/aurora"), RemoteProject(id: "p2", name: "rune", path: "/w/rune")]
        let tasks = [task("a", project: "p2"), task("b", project: "p1", pinned: true), task("c", project: "p1"),
                     task("d", project: "p2", agent: "Grok"), task("e", project: "gone", pinned: true), task("f", project: "gone")]
        let sections = TaskListLayout.sections(tasks: tasks, projects: projects)
        XCTAssertEqual(sections.map(\.title), ["Fijadas", "rune", "aurora", "Otro proyecto"])
        XCTAssertEqual(sections.map { $0.tasks.map(\.id) }, [["b", "e"], ["a", "d"], ["c"], ["f"]])
        XCTAssertEqual(TaskListLayout.sections(tasks: tasks, projects: projects, query: "grok").flatMap { $0.tasks.map(\.id) }, ["d"])
        XCTAssertEqual(TaskListLayout.sections(tasks: tasks, projects: projects, query: "AURORA").flatMap { $0.tasks.map(\.id) }, ["b", "c"])
        XCTAssertTrue(TaskListLayout.sections(tasks: [], projects: projects).isEmpty)
    }

    func testMarkdownKeepsCodeLiteralAndHandlesUnclosedStreamingFence() {
        let blocks = MarkdownBlock.parse("# Title\n\nText **bold**\n\n```swift\nlet x = \"# literal\"\n- not a bullet")
        XCTAssertEqual(blocks.map(\.kind), [.heading(1), .paragraph, .code("swift")])
        XCTAssertEqual(blocks.last?.text, "let x = \"# literal\"\n- not a bullet")
    }

    #if DEBUG
    @MainActor
    func testEveryCaptureFixtureDecodesThroughTheProductionModels() throws {
        for scene in DebugUIFixtures.scenes {
            let store = RemoteStore()
            DebugUIFixtures.load(scene, into: store)
            XCTAssertNil(store.notice, scene)
            XCTAssertNotNil(store.conversations[DebugUIFixtures.taskID], scene)
        }
        let decoder = JSONDecoder()
        let base = "/v2/projects/\(DebugUIFixtures.projectID)"
        XCTAssertFalse(try decoder.decode(RemoteDirectory.self, from: DebugUIFixtures.data(path: "\(base)/files")).entries.isEmpty)
        XCTAssertFalse(try decoder.decode(RemoteFile.self, from: DebugUIFixtures.data(path: "\(base)/file")).content.isEmpty)
        XCTAssertEqual(try decoder.decode(RemoteGitStatus.self, from: DebugUIFixtures.data(path: "\(base)/git/status")).files.count, 3)
        let diff = try decoder.decode(RemoteFileDiff.self, from: DebugUIFixtures.data(path: "\(base)/git/diff"))
        XCTAssertEqual(UnifiedDiffDocument.parse(diff.diff).files.count, 1)
        let pairing = String(decoding: try JSONEncoder().encode(DebugUIFixtures.pairingPayload), as: UTF8.self)
        XCTAssertEqual(try PairingPayload.parse(pairing), DebugUIFixtures.pairingPayload)
    }

    /// A link naming a saved PC with another certificate must not replace its token.
    @MainActor
    func testPairingNeverReplacesASavedPCsCertificate() async {
        let store = RemoteStore()
        DebugUIFixtures.load("tasks", into: store)
        let saved = DebugUIFixtures.pairingPayload
        let impostor = PairingPayload(version: 2, routes: ["https://203.0.113.9:7787"], fingerprint: String(repeating: "e", count: 64),
                                      serverID: DebugUIFixtures.serverID, pairingID: saved.pairingID, secret: saved.secret,
                                      expiresAt: saved.expiresAt, label: "nixos")
        do {
            try await store.pair(impostor, deviceName: "iPhone", requestID: UUID().uuidString)
            XCTFail("pairing replaced a saved certificate")
        } catch {
            XCTAssertEqual(error as? PairingError, .changedCertificate)
        }
        XCTAssertEqual(store.activeComputer?.fingerprint, DebugUIFixtures.fingerprint)
    }
    #endif
}
