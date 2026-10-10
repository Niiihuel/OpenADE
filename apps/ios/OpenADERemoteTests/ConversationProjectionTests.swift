import XCTest
@testable import OpenADERemote

final class ConversationProjectionTests: XCTestCase {
    func testReconnectDoesNotDuplicateDeltasAndFinalReplacesStream() {
        var projection = ConversationProjection()
        let first = event(1, "assistant_delta", text: "Hel", message: "m1")
        let second = event(2, "assistant_delta", text: "lo", message: "m1")
        projection.apply([first, second])
        projection.apply([first, second, event(3, "assistant_message", text: "Hello!", message: "m1")])
        XCTAssertEqual(projection.entries.count, 1)
        XCTAssertEqual(projection.entries.first?.text, "Hello!")
        XCTAssertEqual(projection.cursor, 3)
        XCTAssertFalse(projection.entries[0].isRunning)
    }

    func testDistinctMessagesWithinOneTurnRemainDistinct() {
        var projection = ConversationProjection()
        projection.apply([
            event(1, "assistant_message", text: "I will inspect this.", message: "m1"),
            event(2, "tool_started", tool: "tool-1"),
            event(3, "tool_output", text: "intermediate", tool: "tool-1"),
            event(4, "tool_finished", text: "complete result", tool: "tool-1"),
            event(5, "assistant_delta", text: "Done", message: "m2"),
            event(6, "assistant_message", text: "Done.", message: "m2"),
            event(7, "turn_completed", text: "Done.")
        ])
        XCTAssertEqual(projection.entries.map(\.text), ["I will inspect this.", "complete result", "Done."])
        XCTAssertEqual(projection.entries.filter { $0.kind == .assistant }.count, 2)
        XCTAssertTrue(projection.entries.allSatisfy { !$0.isRunning })
    }

    func testUnknownEventsAdvanceReplayCursorWithoutAddingContent() {
        var projection = ConversationProjection()
        projection.apply([event(8, "future_event"), event(2, "user_message", text: "Hello")])
        projection.apply([event(8, "future_event")])
        XCTAssertEqual(projection.cursor, 8)
        XCTAssertEqual(projection.entries.count, 1)
        XCTAssertEqual(projection.entries[0].text, "Hello")
    }

    func testCursorAdvancesAcrossHostFilteredEventsAndOldPagesNeverRegress() {
        var projection = ConversationProjection()
        projection.apply(EventPage(events: [event(1, "user_message", text: "Hello")], nextCursor: 5, hasMore: false))
        projection.apply(EventPage(events: [], nextCursor: 10, hasMore: false))
        projection.apply(EventPage(events: [event(1, "user_message", text: "Hello")], nextCursor: 5, hasMore: false))
        XCTAssertEqual(projection.cursor, 10)
        XCTAssertEqual(projection.entries.count, 1)
    }

    func testLongHistoryRemainsBoundedWithoutReplayingEvictedEvents() {
        var projection = ConversationProjection()
        let events = (1...320).map { event(Int64($0), "user_message", text: "Message \($0)") }
        projection.apply(events)
        XCTAssertEqual(projection.entries.count, 300)
        XCTAssertEqual(projection.cursor, 320)
        XCTAssertTrue(projection.didTrimHistory)
        XCTAssertEqual(projection.entries.first?.text, "Message 21")
        projection.apply(events)
        XCTAssertEqual(projection.entries.count, 300)
    }

    func testSingleLargeStreamIsCappedAndAuthoritativeFinalStillReplacesIt() {
        var projection = ConversationProjection()
        projection.apply([event(1, "assistant_delta", text: String(repeating: "x", count: 200_000), message: "m1")])
        XCTAssertLessThanOrEqual(projection.entries[0].text.utf8.count, 128 * 1_024)
        XCTAssertTrue(projection.entries[0].textTruncated)
        projection.apply([event(2, "assistant_message", text: "Final answer", message: "m1")])
        XCTAssertEqual(projection.entries.count, 1)
        XCTAssertEqual(projection.entries[0].text, "Final answer")
    }

    private func event(_ seq: Int64, _ type: String, text: String? = nil,
                       message: String? = nil, tool: String? = nil) -> RemoteEvent {
        RemoteEvent(seq: seq, timestampMS: 0, type: type, text: text, turnID: "turn-1",
                    messageID: message, requestID: nil, toolCallID: tool, toolName: tool == nil ? nil : "Read",
                    arguments: nil, decision: nil, isError: false)
    }
}
