import XCTest
@testable import OpenADERemote

final class ConversationProjectionTests: XCTestCase {
    private func event(_ seq: Int64, _ type: String, turn: String = "turn-1", at seconds: Int64 = 0, text: String? = nil,
                       message: String? = nil, tool: String? = nil, name: String? = nil,
                       arguments: JSONValue? = nil, error: Bool? = nil) -> RemoteEvent {
        RemoteEvent(seq: seq, timestampMS: seconds * 1_000, type: type, text: text, turnID: turn, messageID: message,
                    toolCallID: tool, toolName: name, arguments: arguments, isError: error)
    }

    func testDeltasStreamIntoOneMessageAndTheCommittedTextReplacesThem() {
        var projection = ConversationProjection()
        let first = event(1, "assistant_delta", text: "Hol", message: "turn-1:1")
        let second = event(2, "assistant_delta", text: "a", message: "turn-1:1")
        projection.apply([first, second])
        XCTAssertEqual(projection.entries.first?.text, "Hola")
        XCTAssertTrue(projection.entries[0].isRunning)
        // A reconnect may resend what was already applied.
        projection.apply([first, second, event(3, "assistant_message", text: "¡Hola!", message: "turn-1:1")])
        XCTAssertEqual(projection.entries.count, 1)
        XCTAssertEqual(projection.entries[0].text, "¡Hola!")
        XCTAssertFalse(projection.entries[0].isRunning)
        XCTAssertEqual(projection.cursor, 3)
    }

    func testConsecutiveToolCallsFoldIntoOneGroupWithASummaryByKind() {
        var projection = ConversationProjection()
        projection.apply([
            event(1, "user_message", text: "Arreglá el login"),
            event(2, "assistant_message", text: "Miro el código.", message: "turn-1:1"),
            event(3, "tool_started", tool: "a", name: "read_file", arguments: .object(["path": .string("src/a.rs")])),
            event(4, "tool_finished", text: "fn a()", tool: "a"),
            event(5, "tool_started", tool: "b", name: "read_file", arguments: .object(["path": .string("src/b.rs")])),
            event(6, "tool_finished", text: "fn b()", tool: "b"),
            event(7, "tool_started", tool: "c", name: "apply_patch",
                  arguments: .object(["patch": .string("*** Update File: src/a.rs\n*** Update File: src/b.rs")])),
            event(8, "tool_finished", text: "ok", tool: "c"),
            event(9, "tool_started", tool: "d", name: "run_command", arguments: .object(["command": .string("cargo test")])),
        ])
        XCTAssertEqual(projection.items.count, 3)
        guard case .tools(let group) = projection.items[2] else { return XCTFail("expected a tool group") }
        XCTAssertEqual(group.calls.count, 4)
        XCTAssertTrue(group.isRunning)
        XCTAssertEqual(group.summary, "Leyó 2 archivos · Ejecutó 1 comando · Editó 2 archivos")
        XCTAssertEqual(group.calls.map(\.label), ["Leyó a.rs", "Leyó b.rs", "Editó a.rs", "cargo test"])
    }

    func testFailedToolMarksTheGroup() {
        var projection = ConversationProjection()
        projection.apply([
            event(1, "tool_started", tool: "a", name: "run_command", arguments: .object(["command": .string("make")])),
            event(2, "tool_finished", text: "error: no rule", tool: "a", error: true),
        ])
        guard case .tools(let group) = projection.items.first else { return XCTFail("expected a tool group") }
        XCTAssertEqual(group.failures, 1)
        XCTAssertFalse(group.isRunning)
    }

    func testCompletedTurnFoldsItsWorkBehindTrabajoAndKeepsTheLastAnswer() {
        var projection = ConversationProjection()
        projection.apply([
            event(1, "user_message", at: 0, text: "Revisá la reconexión"),
            event(2, "assistant_message", at: 5, text: "Voy a mirar.", message: "turn-1:1"),
            event(3, "tool_started", at: 6, tool: "a", name: "read_file", arguments: .object(["path": .string("Store.swift")])),
            event(4, "tool_finished", at: 8, text: "…", tool: "a"),
            event(5, "assistant_message", at: 120, text: "Listo.", message: "turn-1:2"),
            event(6, "turn_completed", at: 125, text: "Listo."),
        ])
        XCTAssertEqual(projection.items.map(\.id), ["user-1", "work-turn-1", "assistant-turn-1:2"])
        guard case .work(let work) = projection.items[1] else { return XCTFail("expected the folded work") }
        XCTAssertEqual(work.title, "Trabajó 2 min")
        XCTAssertEqual(work.summary, "Leyó 1 archivo")
        XCTAssertEqual(work.items.map(\.id), ["assistant-turn-1:1", "tools-tool-turn-1-a"])
        XCTAssertNil(projection.liveTurnStartMS)
    }

    func testRunningTurnStaysFlatAndKnowsWhenItStarted() {
        var projection = ConversationProjection()
        projection.apply([event(1, "user_message", at: 50, text: "Hola"), event(2, "assistant_delta", at: 51, text: "Ho", message: "turn-1:1")])
        XCTAssertEqual(projection.items.count, 2)
        XCTAssertEqual(projection.liveTurnStartMS, 50_000)
    }

    func testFailedAndCancelledTurnsSayWhyWithoutFolding() {
        var projection = ConversationProjection()
        projection.apply([
            event(1, "user_message", text: "A"),
            event(2, "turn_failed", text: "Se alcanzó el límite de uso."),
            event(3, "user_message", turn: "turn-2", text: "B"),
            event(4, "turn_cancelled", turn: "turn-2"),
        ])
        let notices = projection.items.compactMap { item -> ConversationEntry? in
            if case .notice(let entry) = item { return entry }
            return nil
        }
        XCTAssertEqual(notices.map(\.text), ["Se alcanzó el límite de uso.", "Turno detenido."])
        XCTAssertEqual(notices.map(\.isError), [true, false])
    }

    func testWorkTitlesReadNaturally() {
        XCTAssertEqual(TurnWork.title(durationMS: 42_000), "Trabajó 42 s")
        XCTAssertEqual(TurnWork.title(durationMS: 300), "Trabajó 1 s")
        XCTAssertEqual(TurnWork.title(durationMS: 125_000), "Trabajó 2 min")
        XCTAssertEqual(TurnWork.title(durationMS: 3_600_000), "Trabajó 1 h")
        XCTAssertEqual(TurnWork.title(durationMS: 3_900_000), "Trabajó 1 h 5 min")
        XCTAssertEqual(TurnWork.title(durationMS: nil), "Trabajó")
    }

    func testUnknownEventsAdvanceTheCursorWithoutContent() {
        var projection = ConversationProjection()
        projection.apply([event(8, "future_event"), event(2, "user_message", text: "Hola")])
        XCTAssertEqual(projection.cursor, 8)
        XCTAssertEqual(projection.entries.map(\.text), ["Hola"])
    }

    func testLongHistoryAndHugeStreamsStayBounded() {
        var projection = ConversationProjection()
        projection.apply((1...320).map { event(Int64($0), "user_message", turn: "t\($0)", text: "Mensaje \($0)") })
        XCTAssertEqual(projection.entries.count, 300)
        XCTAssertEqual(projection.entries.first?.text, "Mensaje 21")
        var stream = ConversationProjection()
        stream.apply([event(1, "assistant_delta", text: String(repeating: "x", count: 200_000), message: "m")])
        XCTAssertLessThanOrEqual(stream.entries[0].text.utf8.count, 128 * 1_024)
        XCTAssertTrue(stream.entries[0].textTruncated)
        stream.apply([event(2, "assistant_message", text: "Final", message: "m")])
        XCTAssertEqual(stream.entries[0].text, "Final")
        XCTAssertFalse(stream.entries[0].textTruncated)
    }

    func testConversationFollowsTheJournalCursorPrependsOlderPagesAndCompactsDeltas() {
        let recent = TaskHistory(events: [
            event(10, "user_message", turn: "t2", text: "Segundo"),
            event(11, "assistant_delta", turn: "t2", text: "Res", message: "t2:1"),
        ], pending: [], cursor: 14, hasMore: true)
        var conversation = TaskConversation()
        conversation.replace(with: recent)
        XCTAssertTrue(conversation.loaded)
        XCTAssertEqual(conversation.cursor, 14)
        conversation.append([event(15, "assistant_delta", turn: "t2", text: "puesta", message: "t2:1")])
        conversation.append([event(16, "assistant_message", turn: "t2", text: "Respuesta", message: "t2:1")])
        XCTAssertEqual(conversation.cursor, 16)
        XCTAssertFalse(conversation.events.contains { $0.type == "assistant_delta" })
        XCTAssertEqual(conversation.projection.entries.last?.text, "Respuesta")

        conversation.prepend(TaskHistory(events: [
            event(1, "user_message", turn: "t1", text: "Primero"),
            event(2, "assistant_message", turn: "t1", text: "Hecho", message: "t1:1"),
            event(10, "user_message", turn: "t2", text: "Segundo"),
        ], pending: [], cursor: 16, hasMore: false))
        XCTAssertFalse(conversation.hasMore)
        XCTAssertEqual(conversation.projection.entries.map(\.text), ["Primero", "Hecho", "Segundo", "Respuesta"])
        XCTAssertEqual(conversation.events.map(\.seq), [1, 2, 10, 16])
    }
}
