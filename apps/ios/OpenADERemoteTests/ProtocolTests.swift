import XCTest
@testable import OpenADERemote

/// Pairing codes, the event stream parser, certificate pins and routes.
final class ProtocolTests: XCTestCase {
    // MARK: Pairing payload v2

    private func payload(_ overrides: [String: Any] = [:]) -> [String: Any] {
        var value: [String: Any] = [
            "version": 2,
            "routes": ["https://192.168.1.229:7787", "https://[2803:9800:9015:4a1::2]:7787", "https://casa.example.com:7787"],
            "fingerprint": String(repeating: "ab", count: 32),
            "server_id": "6f1c2a9e-3b7d-4f2a-9c1e-5d8b7a6f4e21",
            "pairing_id": "0b1e6d2c-8f4a-4b3e-9a7d-2c5e1f6a8b90",
            "secret": String(repeating: "0f", count: 32),
            "expires_at": Int(Date.now.timeIntervalSince1970) + 600,
            "label": "nixos",
        ]
        overrides.forEach { value[$0.key] = $0.value }
        return value
    }

    private func link(_ object: [String: Any]) throws -> String {
        let encoded = try JSONSerialization.data(withJSONObject: object).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return "openade-remote://pair#\(encoded)"
    }

    private func parseError(_ object: [String: Any]) throws -> PairingError? {
        let code = try link(object)
        do { _ = try PairingPayload.parse(code); return nil }
        catch { return error as? PairingError }
    }

    func testValidPairingLinkCarriesEveryRouteAndTheShortSecurityCode() throws {
        let parsed = try PairingPayload.parse(link(payload()))
        XCTAssertEqual(parsed.version, 2)
        XCTAssertEqual(parsed.routes.count, 3)
        XCTAssertEqual(parsed.label, "nixos")
        XCTAssertEqual(parsed.shortFingerprint, "ABAB-ABAB-ABAB-ABAB")
        XCTAssertEqual(try RemoteRoute.url(parsed.routes[1]).absoluteString, "https://[2803:9800:9015:4a1::2]:7787")
    }

    func testExpiredCodeIsRefusedAsExpired() throws {
        XCTAssertEqual(try parseError(payload(["expires_at": Int(Date.now.timeIntervalSince1970) - 1])), .expired)
    }

    func testOtherVersionsAreRefusedAsUnsupported() throws {
        XCTAssertEqual(try parseError(payload(["version": 1])), .unsupportedVersion)
        XCTAssertEqual(try parseError(payload(["version": 3])), .unsupportedVersion)
    }

    func testBadRoutesPinsAndSecretsAreRefused() throws {
        let bad: [[String: Any]] = [
            ["routes": []],
            ["routes": ["http://192.168.1.229:7787"]],
            ["routes": ["https://192.168.1.229:7787/v2"]],
            ["routes": ["https://user:secret@192.168.1.229:7787"]],
            ["routes": ["https://192.168.1.229:7787?token=x"]],
            ["routes": Array(repeating: "https://pc.example:7787", count: 9)],
            ["fingerprint": String(repeating: "a", count: 63)],
            ["fingerprint": String(repeating: "g", count: 64)],
            ["fingerprint": String(repeating: "AB", count: 32)],
            ["secret": String(repeating: "0F", count: 32)],
            ["server_id": "not-a-uuid"],
            ["secret": "short"],
            ["label": ""],
        ]
        for overrides in bad {
            XCTAssertEqual(try parseError(payload(overrides)), .invalidCode, "\(overrides)")
        }
        XCTAssertThrowsError(try PairingPayload.parse(try link(payload()).replacingOccurrences(of: "pair#", with: "pair?x=1#")))
        XCTAssertThrowsError(try PairingPayload.parse("https://example.com/pair#abc"))
    }

    // MARK: Server-sent events

    private func messages(_ chunks: [String]) -> [SSEMessage] {
        var parser = SSEParser()
        return chunks.flatMap { parser.push(Array($0.utf8)) }
    }

    func testMultiLineDataJoinsWithNewlines() {
        XCTAssertEqual(messages(["event: overview\ndata: {\"a\":\ndata: 1}\n\n"]),
                       [SSEMessage(event: "overview", data: "{\"a\":\n1}")])
    }

    func testCommentsAreKeepAlivesAndNeverDispatch() {
        XCTAssertEqual(messages([":ping\n\n", ": keep-alive\nevent: reset\ndata: {\"task\":\"t\"}\n\n"]),
                       [SSEMessage(event: "reset", data: "{\"task\":\"t\"}")])
    }

    func testChunksSplitAnywhereIncludingCRLFAndUTF8() {
        let stream = "event: events\r\ndata: {\"text\":\"ñandú\"}\r\n\r\nevent: pending\ndata: []\n\n"
        let bytes = Array(stream.utf8)
        var parser = SSEParser()
        var received: [SSEMessage] = []
        for byte in bytes { if let message = parser.push(byte) { received.append(message) } }
        XCTAssertEqual(received, [SSEMessage(event: "events", data: "{\"text\":\"ñandú\"}"), SSEMessage(event: "pending", data: "[]")])
        // The same bytes in uneven chunks: a CR at one chunk's end and its LF at the next.
        let cut = bytes.firstIndex(of: 13)! + 1
        var chunked = SSEParser()
        let split = chunked.push(bytes[..<cut]) + chunked.push(bytes[cut..<(cut + 9)]) + chunked.push(bytes[(cut + 9)...])
        XCTAssertEqual(split, received)
    }

    func testBlankLineWithoutDataResetsTheEventNameAndDefaultsToMessage() {
        XCTAssertEqual(messages(["event: overview\n\ndata:no-space\n\n"]), [SSEMessage(event: "message", data: "no-space")])
        XCTAssertEqual(messages(["data: partial"]), [])
    }

    // MARK: Certificate pin

    func testPinComparesTheSHA256OfTheLeafCertificate() {
        let der = Data("hello".utf8)
        let pin = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        XCTAssertEqual(CertificatePin.fingerprint(of: der), pin)
        XCTAssertTrue(CertificatePin.matches(der: der, pinned: pin))
        XCTAssertTrue(CertificatePin.matches(der: der, pinned: pin.uppercased()))
        XCTAssertFalse(CertificatePin.matches(der: Data("hellO".utf8), pinned: pin))
        XCTAssertFalse(CertificatePin.matches(der: der, pinned: String(pin.dropLast())))
        XCTAssertFalse(CertificatePin.matches(der: der, pinned: ""))
        XCTAssertFalse(CertificatePin.matches(der: der, pinned: String(repeating: "z", count: 64)))
        XCTAssertEqual(CertificatePin.short(pin), "2CF2-4DBA-5FB0-A30E")
    }

    // MARK: Routes and backoff

    func testRoutesAreHTTPSOriginsOnly() throws {
        XCTAssertEqual(try RemoteRoute.url(" https://192.168.1.229:7787/ ").absoluteString, "https://192.168.1.229:7787")
        XCTAssertEqual(RemoteRoute.display("https://casa.example.com:7787"), "casa.example.com:7787")
        for bad in ["http://pc:7787", "https://pc:7787/v2", "https://pc:7787?a=1", "https://pc:7787#x", "https://u:p@pc", "https://", "pc:7787"] {
            XCTAssertThrowsError(try RemoteRoute.url(bad), bad)
        }
    }

    func testBackoffIsJitteredFromHalfASecondToThirtySecondsAndResets() {
        var full = ReconnectionDelay(random: { 1 })
        XCTAssertEqual((0..<8).map { _ in full.next() }, [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000].map { Duration.milliseconds($0) })
        var low = ReconnectionDelay(random: { 0 })
        XCTAssertEqual(low.next(), .milliseconds(250))
        XCTAssertEqual(low.next(), .milliseconds(500))
        full.reset()
        XCTAssertEqual(full.next(), .milliseconds(500))
    }

    func testLearnedRoutesPutThePCsOwnListFirstAndKeepOlderOnes() {
        var computer = SavedComputer(id: "s", name: "nixos", routes: ["https://192.168.1.10:7787", "https://old.example:7787"],
                                     fingerprint: String(repeating: "ab", count: 32), token: "t", deviceID: "d")
        computer.learn(routes: ["https://192.168.1.229:7787", "https://192.168.1.10:7787", "http://insecure:1"])
        XCTAssertEqual(computer.routes, ["https://192.168.1.229:7787", "https://192.168.1.10:7787", "https://old.example:7787"])
        computer.learn(routes: [])
        XCTAssertEqual(computer.routes.count, 3)
    }

    func testKeychainKeepsEachPCsTokenRoutesAndPin() throws {
        let keychain = ConnectionKeychain(service: "dev.openade.remote.tests.\(UUID().uuidString)")
        defer { try? keychain.deleteLibrary() }
        XCTAssertNil(try keychain.loadLibrary())
        let desk = SavedComputer(id: "one", name: "nixos", routes: ["https://192.168.1.229:7787"],
                                 fingerprint: String(repeating: "ab", count: 32), token: "token-one", deviceID: "d1")
        var laptop = SavedComputer(id: "two", name: "Mac", routes: ["https://[::1]:7787"],
                                   fingerprint: String(repeating: "cd", count: 32), token: "token-two", deviceID: "d2")
        laptop.revoked = true
        try keychain.saveLibrary(ConnectionLibrary(computers: [desk, laptop], activeID: "two"))
        let loaded = try XCTUnwrap(keychain.loadLibrary())
        XCTAssertEqual(loaded.activeID, "two")
        XCTAssertEqual(loaded.computers.map(\.token), ["token-one", "token-two"])
        XCTAssertTrue(loaded.computers[1].revoked)
        XCTAssertThrowsError(try keychain.saveLibrary(ConnectionLibrary(computers: [desk, desk], activeID: "one")))
        try keychain.deleteLibrary()
        XCTAssertNil(try keychain.loadLibrary())
    }
}
