import XCTest
@testable import OpenADERemote

final class RemoteModelsTests: XCTestCase {
    func testSessionDecodesHostContractAndStructuredArguments() throws {
        let source = #"{"id":"s1","project_id":"p1","title":"Fix login","status":"waiting_permission","updated_at_ms":1700000000000,"active_turn_id":"t1","pending_permissions":[{"request_id":"r1","tool_call_id":"c1","tool_name":"Bash","arguments":{"command":"swift test","timeout":30}}]}"#
        let session = try JSONDecoder().decode(RemoteSession.self, from: Data(source.utf8))
        XCTAssertEqual(session.projectID, "p1")
        XCTAssertEqual(session.activeTurnID, "t1")
        XCTAssertTrue(session.isBusy)
        XCTAssertEqual(session.pendingPermissions.first?.id, "r1")
        XCTAssertTrue(session.pendingPermissions[0].arguments.displayText.contains("swift test"))
        XCTAssertEqual(session.updatedAt.timeIntervalSince1970, 1_700_000_000)
    }

    func testPendingTurnRoundTripPreservesIdAndExactPrompt() throws {
        let action = PendingAction(requestID: "stable-id", kind: .sendTurn, resourceID: "s1",
                                   prompt: "Check auth\nDon't deploy.", mode: .plan, turnID: nil)
        let persisted = try JSONEncoder().encode([action.key: action])
        let restored = try JSONDecoder().decode([String: PendingAction].self, from: persisted)
        XCTAssertEqual(restored[action.key]?.requestID, "stable-id")
        XCTAssertEqual(restored[action.key]?.prompt, action.prompt)
        XCTAssertEqual(restored[action.key]?.mode, .plan)
    }

    func testPendingSteeringKeepsTheOriginalTurnToAvoidSteeringAnotherTurn() throws {
        let action = PendingAction(requestID: "stable-steer", kind: .steerTurn, resourceID: "s1",
                                   prompt: "Use the existing component", mode: .build, turnID: "original-turn")
        let restored = try JSONDecoder().decode(PendingAction.self, from: JSONEncoder().encode(action))
        XCTAssertEqual(restored.requestID, "stable-steer")
        XCTAssertEqual(restored.turnID, "original-turn")
        XCTAssertEqual(restored.kind, .steerTurn)
    }

    func testTruncatedPermissionAndDiffScopeArePreserved() throws {
        let permission = #"{"request_id":"r1","tool_call_id":"c1","tool_name":"Bash","arguments":{"notice":"too large"},"arguments_truncated":true}"#
        let decoded = try JSONDecoder().decode(RemotePermission.self, from: Data(permission.utf8))
        XCTAssertEqual(decoded.argumentsTruncated, true)
        let diff = try JSONDecoder().decode(ProjectDiff.self, from: Data(#"{"diff":"","truncated":false,"includes_untracked":false}"#.utf8))
        XCTAssertEqual(diff.includesUntracked, false)
    }

    func testUncertainConflictsAndServerFailuresNeverDiscardTheRequestId() {
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 409, code: "operation_uncertain"))
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 409, code: "conflict"))
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 500, code: "internal"))
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 403, code: "forbidden"))
        XCTAssertTrue(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 400, code: "invalid_request"))
    }

    func testDiffHeadersAreNotMistakenForChangedLines() {
        let lines = DiffLine.parse("--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n context")
        XCTAssertEqual(lines.map(\.kind), [.header, .header, .header, .removed, .added, .context])
    }
}
