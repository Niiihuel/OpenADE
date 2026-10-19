import Foundation

struct RemoteFileEntry: Decodable, Identifiable, Hashable, Sendable {
    let path: String
    let name: String
    let kind: String
    let sizeBytes: Int64?
    var id: String { path }
    var isDirectory: Bool { kind == "directory" }
    enum CodingKeys: String, CodingKey { case path, name, kind; case sizeBytes = "size_bytes" }
}

struct RemoteDirectory: Decodable, Sendable {
    let path: String
    let entries: [RemoteFileEntry]
    let truncated: Bool
}

struct RemoteFile: Decodable, Sendable {
    let path: String
    let content: String
    let revision: String
    let language: String?
    let truncated: Bool
}

struct RemoteGitFile: Decodable, Identifiable, Hashable, Sendable {
    let path: String
    let indexStatus: String
    let worktreeStatus: String
    let staged: Bool
    let untracked: Bool
    var id: String { path }
    enum CodingKeys: String, CodingKey {
        case path, staged, untracked
        case indexStatus = "index_status"
        case worktreeStatus = "worktree_status"
    }
}

struct RemoteGitStatus: Decodable, Sendable {
    let revision: String
    let branch: String?
    let files: [RemoteGitFile]
    let clean: Bool
    let commit: String?
}

struct RemoteFileDiff: Decodable, Sendable {
    let path: String
    let diff: String
    let truncated: Bool
}

/// A paired PC as this phone keeps it in the Keychain.
struct SavedComputer: Codable, Identifiable, Equatable, Sendable {
    /// The PC's `server_id`.
    let id: String
    var name: String
    /// Every address it answered on, likeliest first, learned from the PC.
    var routes: [String]
    let fingerprint: String
    let token: String
    let deviceID: String
    var lastRoute: String? = nil
    var lastConnectedAt: Date? = nil
    var notificationsEnabled = false
    /// The APNs token this PC holds for this phone, if any.
    var registeredPushToken: String? = nil
    /// The PC answered 401: its owner revoked this phone.
    var revoked = false

    var shortFingerprint: String { CertificatePin.short(fingerprint) }

    /// The PC's own list first, then older addresses it no longer names.
    mutating func learn(routes fresh: [String]) {
        let valid = fresh.filter { (try? RemoteRoute.url($0)) != nil }
        guard !valid.isEmpty else { return }
        routes = Array((valid + routes.filter { !valid.contains($0) }).prefix(8))
    }
}

struct ConnectionLibrary: Codable, Equatable, Sendable {
    var computers: [SavedComputer] = []
    var activeID: String?

    func validated() throws -> ConnectionLibrary {
        guard computers.count <= 20, Set(computers.map(\.id)).count == computers.count,
              computers.allSatisfy({ CertificatePin.isValid($0.fingerprint) && !$0.token.isEmpty
                  && !$0.routes.isEmpty && $0.routes.allSatisfy { (try? RemoteRoute.url($0)) != nil } })
        else { throw ConnectionError.invalidResponse }
        return self
    }
}
