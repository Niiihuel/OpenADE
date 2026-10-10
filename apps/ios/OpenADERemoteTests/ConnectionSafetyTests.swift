import XCTest
@testable import OpenADERemote

final class ConnectionSafetyTests: XCTestCase {
    func testDeviceKeychainStoresCredentialsAndScopesPendingActionsToTheirConnection() throws {
        let keychain = ConnectionKeychain(service: "dev.openade.remote.tests.\(UUID().uuidString)")
        defer {
            try? keychain.delete()
            try? keychain.savePendingActions([:])
        }
        try keychain.delete()
        XCTAssertNil(try keychain.load())
        let connection = try ConnectionConfiguration(address: "https://first.example", token: "first-token")
        try keychain.save(connection)
        XCTAssertEqual(try keychain.load(), connection)

        let pending = PendingAction(requestID: "retry-id", kind: .steerTurn, resourceID: "s1",
                                    prompt: "Keep this instruction", mode: .build, turnID: "t1")
        try keychain.savePendingActions([pending.key: pending], for: connection)
        XCTAssertEqual(try keychain.loadPendingActions(for: connection)[pending.key]?.requestID, "retry-id")
        let replacement = try ConnectionConfiguration(address: "https://first.example", token: "replaced-token")
        XCTAssertTrue(try keychain.loadPendingActions(for: replacement).isEmpty)
        let otherHost = try ConnectionConfiguration(address: "https://second.example", token: "first-token")
        XCTAssertTrue(try keychain.loadPendingActions(for: otherHost).isEmpty)

        try keychain.save(replacement)
        XCTAssertEqual(try keychain.load(), replacement)
        try keychain.delete()
        try keychain.savePendingActions([:])
        XCTAssertNil(try keychain.load())
        XCTAssertTrue(try keychain.loadPendingActions(for: connection).isEmpty)
    }

    func testRejectsInsecureAndCredentialBearingEndpoints() {
        let rejected = [
            "http://host.example", "https://user:secret@host.example", "https://host.example/v1",
            "https://host.example?token=secret", "https://host.example#token", "file:///tmp/host",
            "https://"
        ]
        for address in rejected {
            XCTAssertThrowsError(try ConnectionConfiguration(address: address, token: "secret"), address)
        }
        XCTAssertThrowsError(try ConnectionConfiguration(address: "https://host.example", token: " "))
        XCTAssertThrowsError(try ConnectionConfiguration(address: "https://host.example", token: "token\ninjected"))
        XCTAssertThrowsError(try ConnectionConfiguration(address: "https://host.example", token: "token\tinjected"))
        XCTAssertThrowsError(try ConnectionConfiguration(address: "https://host.example", token: "token\0injected"))
    }

    func testNormalizesSecureEndpointAndTrimsPastedToken() throws {
        let config = try ConnectionConfiguration(address: " https://host.example:8443/ ", token: " token ")
        XCTAssertEqual(config.displayAddress, "https://host.example:8443")
        XCTAssertEqual(config.token, "token")
    }

    func testReconnectBackoffIsBoundedAndResetsAfterRecovery() {
        var backoff = ReconnectionDelay()
        XCTAssertEqual((0..<8).map { _ in backoff.next() }, [
            .seconds(1), .seconds(2), .seconds(4), .seconds(8),
            .seconds(16), .seconds(30), .seconds(30), .seconds(30)
        ])
        backoff.reset()
        XCTAssertEqual(backoff.next(), .seconds(1))
    }
}
