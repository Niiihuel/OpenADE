import XCTest
@testable import OpenADERemote

final class WorkspaceModelsTests: XCTestCase {
    func testModelControlsUseExactHostMetadataAndRemainOptionalForOldHosts() throws {
        let source = #"{"models":[{"id":"account-model","name":"Account model","provider":"codex","reasoning_efforts":["low","high"],"supports_images":false}],"default_model_id":"account-model","selection_enabled":true}"#
        let catalog = try JSONDecoder().decode(RemoteModelCatalog.self, from: Data(source.utf8))
        XCTAssertEqual(catalog.models[0].reasoningEfforts, ["low", "high"])
        XCTAssertEqual(catalog.models[0].supportsImages, false)
        XCTAssertEqual(catalog.selectionEnabled, true)
        let legacy = #"{"models":[{"id":"fixed","name":"Fixed","provider":"local"}],"default_model_id":"fixed"}"#
        let old = try JSONDecoder().decode(RemoteModelCatalog.self, from: Data(legacy.utf8))
        XCTAssertNil(old.selectionEnabled)
        XCTAssertNil(old.models[0].reasoningEfforts)
    }

    func testPendingAndQueuedOptionsSurviveDurableRoundTrip() throws {
        var selected = RemoteTurnOptions(modelID: "account-model", reasoningEffort: "high", approvalPolicy: .auto)
        let action = PendingAction(requestID: "frozen", kind: .sendTurn, resourceID: "session",
                                   prompt: "Task", mode: .plan, turnID: nil, options: selected)
        let queued = QueuedPrompt(id: "queued", sessionID: "session", text: "Later", mode: .build,
                                  createdAt: Date(timeIntervalSince1970: 100), options: selected)
        selected.reasoningEffort = "low"
        selected.approvalPolicy = .fullAccess
        let reloaded = try JSONDecoder().decode(PendingAction.self, from: JSONEncoder().encode(action))
        let reloadedQueue = try JSONDecoder().decode(QueuedPrompt.self, from: JSONEncoder().encode(queued))
        XCTAssertEqual(reloaded.requestID, "frozen")
        XCTAssertEqual(reloaded.options?.modelID, "account-model")
        XCTAssertEqual(reloaded.options?.reasoningEffort, "high")
        XCTAssertEqual(reloaded.options?.approvalPolicy, .auto)
        XCTAssertEqual(reloadedQueue.options, reloaded.options)
        let oldAction = #"{"requestID":"old","kind":"sendTurn","resourceID":"s","prompt":"hi","mode":"build"}"#
        XCTAssertNil(try JSONDecoder().decode(PendingAction.self, from: Data(oldAction.utf8)).options)
        let oldQueue = #"{"id":"old","sessionID":"s","text":"hi","mode":"build","createdAt":0}"#
        XCTAssertNil(try JSONDecoder().decode(QueuedPrompt.self, from: Data(oldQueue.utf8)).options)
    }

    func testApprovalChoicesUseExactWireValuesAndLegacyOptionsStayAskByDefault() throws {
        XCTAssertEqual(RemoteApprovalPolicy.fullAccess.rawValue, "full_access")
        for policy in RemoteApprovalPolicy.allCases {
            XCTAssertEqual(try JSONDecoder().decode(RemoteApprovalPolicy.self, from: JSONEncoder().encode(policy)), policy)
        }
        XCTAssertThrowsError(try JSONDecoder().decode(RemoteApprovalPolicy.self, from: Data(#""everything""#.utf8)))
        let old = try JSONDecoder().decode(RemoteTurnOptions.self, from: Data(#"{"modelID":"kept","reasoningEffort":"high"}"#.utf8))
        XCTAssertNil(old.approvalPolicy)
        XCTAssertEqual(old.approvalPolicy ?? .ask, .ask)
    }

    func testPendingUploadPreviewIsBoundedWithoutChangingRetryPayload() {
        let payload = String(repeating: "A", count: 100_000)
        let mutation = DurableMutation(requestID: "upload", key: "attachment:s", label: "Photo", method: "POST",
                                       path: ["v1", "projects", "p", "attachments"], body: ["data_base64": .string(payload)])
        XCTAssertLessThan(mutation.previewText.count, 500)
        XCTAssertEqual(mutation.body["data_base64"], .string(payload))
    }
    func testPushReceiptIdentifiesManualConnectionForDeepLinkRouting() throws {
        let receipt = try JSONDecoder().decode(PushRegistration.self, from: Data(#"{"registered":true,"environment":"production","device_id":"FBA34540-59D2-4755-A884-73B3069C622F"}"#.utf8))
        XCTAssertTrue(receipt.registered)
        XCTAssertNotNil(UUID(uuidString: receipt.deviceID))
    }
    #if DEBUG
    func testScreenshotFixturesUseProductionDTOs() throws {
        let health = try DebugUIFixtures.decode(HostHealth.self, path: "/v1/health")
        XCTAssertTrue(health.supports("workspace.files.read"))
        let sessions = try DebugUIFixtures.decode(SessionPage.self, path: "/v1/sessions")
        XCTAssertEqual(sessions.sessions.count, 3)
        let events = try DebugUIFixtures.decode(EventPage.self, path: "/v1/sessions/fixture-session/events")
        XCTAssertEqual(events.nextCursor, 6)
        XCTAssertEqual(try DebugUIFixtures.decode(RemoteDirectory.self, path: "/v1/projects/fixture-project/files").entries.count, 5)
        XCTAssertEqual(try DebugUIFixtures.decode(RemoteGitStatus.self, path: "/v1/projects/fixture-project/git/status").files.count, 3)
    }
    #endif
    func testAttachmentRejectsBinaryOversizeAndPathNames() throws {
        XCTAssertThrowsError(try AttachmentUpload(filename: "binary.txt", mediaType: "text/plain", data: Data([0, 65])))
        XCTAssertThrowsError(try AttachmentUpload(filename: "binary.txt", mediaType: "text/plain", data: Data([255])))
        XCTAssertThrowsError(try AttachmentUpload(filename: "../secret", mediaType: "text/plain", data: Data("ok".utf8)))
        XCTAssertThrowsError(try AttachmentUpload(filename: "large.txt", mediaType: "text/plain", data: Data(repeating: 65, count: 256 * 1024 + 1)))
        let upload = try AttachmentUpload(filename: "notes.txt", mediaType: "text/plain", data: Data("abc".utf8))
        XCTAssertEqual(upload.body["sha256"], .string("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"))
        XCTAssertEqual(upload.body["data_base64"], .string("YWJj"))
    }

    func testPendingTurnRoundTripFreezesAttachmentIDs() throws {
        let action = PendingAction(requestID: "stable", kind: .sendTurn, resourceID: "session", prompt: "Review",
                                   mode: .plan, turnID: nil, attachmentIDs: ["attachment-one", "attachment-two"])
        let loaded = try JSONDecoder().decode(PendingAction.self, from: JSONEncoder().encode(action))
        XCTAssertEqual(loaded.attachmentIDs, action.attachmentIDs)
        XCTAssertEqual(loaded.requestID, "stable")
        let legacy = #"{"requestID":"old","kind":"sendTurn","resourceID":"s","prompt":"hi","mode":"build"}"#
        XCTAssertNil(try JSONDecoder().decode(PendingAction.self, from: Data(legacy.utf8)).attachmentIDs)
    }

    func testPayloadRejectedBeforeAdmissionCanBeCorrectedButUnknown413Cannot() {
        XCTAssertTrue(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 413, code: "invalid_request"))
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 413, code: "proxy_unknown"))
        XCTAssertFalse(PendingActionRetentionPolicy.mayDiscardAfterRejection(status: 409, code: "operation_uncertain"))
    }

    func testPairingParserAcceptsOneTimeFragmentAndRejectsSecretsInQueries() throws {
        let source = #"{"version":1,"base_url":"https://pc.example","pairing_id":"1DF29678-8BFD-49DB-9194-8D0B3624D581","secret":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expires_at":2000,"label":"Work PC"}"#
        let encoded = Data(source.utf8).base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let payload = try PairingPayload.parse("openade-remote://pair#\(encoded)", now: Date(timeIntervalSince1970: 1000))
        XCTAssertEqual(payload.baseURL, "https://pc.example")
        XCTAssertEqual(payload.label, "Work PC")
        XCTAssertThrowsError(try PairingPayload.parse("openade-remote://pair?secret=abc#\(encoded)", now: Date(timeIntervalSince1970: 1000)))
        XCTAssertThrowsError(try PairingPayload.parse(source, now: Date(timeIntervalSince1970: 2000)))
        XCTAssertThrowsError(try PairingPayload.parse(source.replacingOccurrences(of: "https://pc.example", with: "http://pc.example"), now: Date(timeIntervalSince1970: 1000)))
    }

    func testV1SessionRemainsUsableWithoutV2Fields() throws {
        let source = #"{"id":"s","project_id":"p","title":"Old session","status":"idle","updated_at_ms":1,"pending_permissions":[]}"#
        let session = try JSONDecoder().decode(RemoteSession.self, from: Data(source.utf8))
        XCTAssertTrue(SessionListFilter.active.includes(session))
        XCTAssertFalse(SessionListFilter.archived.includes(session))
        XCTAssertNil(session.canStartTurn)
    }

    func testDefinitiveFileAndGitConflictsCanBeDiscardedButUncertainEffectsCannot() {
        var mutation = DurableMutation(requestID: "stable", key: "file:p:a", label: "Save a", method: "PUT", path: ["v1", "projects", "p", "file"], body: ["content": .string("original")])
        XCTAssertFalse(mutation.canDiscard)
        mutation.rejectionCode = "operation_uncertain"
        XCTAssertFalse(mutation.canDiscard)
        mutation.rejectionCode = "file_conflict"
        XCTAssertTrue(mutation.canDiscard)
        mutation.rejectionCode = "git_conflict"
        XCTAssertTrue(mutation.canDiscard)
    }

    func testDurableMutationRoundTripPreservesOriginalRevisionAndBody() throws {
        let mutation = DurableMutation(requestID: "same-id", key: "git:p", label: "Commit", method: "POST",
                                       path: ["v1", "projects", "p", "git", "commit"],
                                       body: ["message": .string("Fix\nDetails"), "expected_revision": .string("revision-1")])
        let copy = try JSONDecoder().decode(DurableMutation.self, from: JSONEncoder().encode(mutation))
        XCTAssertEqual(copy.requestID, mutation.requestID)
        XCTAssertEqual(copy.body, mutation.body)
        XCTAssertEqual(copy.path, mutation.path)
        XCTAssertEqual(copy.method, "POST")
    }

    func testMarkdownKeepsCodeLiteralAndHandlesUnclosedStreamingFence() {
        let blocks = MarkdownBlock.parse("# Title\n\nText **bold**\n\n```swift\nlet x = \"# literal\"\n- not a bullet")
        XCTAssertEqual(blocks.map(\.kind), [.heading(1), .paragraph, .code("swift")])
        XCTAssertEqual(blocks.last?.text, "let x = \"# literal\"\n- not a bullet")
        XCTAssertEqual(Set(blocks.map(\.id)).count, blocks.count)
    }

    func testKeychainLibraryKeepsRequestsAndDraftsIsolatedBetweenComputers() throws {
        let keychain = ConnectionKeychain(service: "dev.openade.remote.tests.library.\(UUID().uuidString)")
        defer { try? keychain.deleteLibrary() }
        let first = try ConnectionConfiguration(address: "https://first.example", token: "token-one")
        let second = try ConnectionConfiguration(address: "https://second.example", token: "token-two")
        let pending = PendingAction(requestID: "first-request", kind: .sendTurn, resourceID: "same-session",
                                    prompt: "Only first host", mode: .build, turnID: nil)
        let library = ConnectionLibrary(computers: [
            SavedComputer(id: "one", name: "Work", configuration: first, lastUsedAt: .now,
                          pendingActions: [pending.key: pending], pendingMutations: [:], queuedPrompts: [], drafts: ["same-session": "first draft"]),
            SavedComputer(id: "two", name: "Home", configuration: second, lastUsedAt: .now,
                          pendingActions: [:], pendingMutations: [:], queuedPrompts: [], drafts: ["same-session": "second draft"])
        ], activeID: "two")
        try keychain.saveLibrary(library)
        let loaded = try XCTUnwrap(keychain.loadLibrary())
        XCTAssertEqual(loaded.activeID, "two")
        XCTAssertEqual(loaded.computers[0].pendingActions[pending.key]?.requestID, "first-request")
        XCTAssertTrue(loaded.computers[1].pendingActions.isEmpty)
        XCTAssertEqual(loaded.computers[1].drafts?["same-session"], "second draft")
        try keychain.deleteLibrary()
        XCTAssertNil(try keychain.loadLibrary())
    }
}
