import Foundation

/// The last task list and recent conversations of each PC, so the app can be
/// read while the PC is unreachable. Files are protected while the phone is locked.
struct SnapshotCache {
    struct Conversation: Codable {
        let events: [RemoteEvent]
        let pending: [PendingRequest]
        let cursor: Int64
        let hasMore: Bool
    }
    private struct Conversations: Codable {
        var order: [String] = []
        var byTask: [String: Conversation] = [:]
    }

    private let directory: URL? = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
        .appendingPathComponent("openade-remote", isDirectory: true)
    private let conversationLimit = 12

    private func file(_ name: String, server: String) -> URL? {
        let safe = server.filter { $0.isLetter || $0.isNumber || $0 == "-" }
        return directory?.appendingPathComponent("\(name)-\(safe).json")
    }

    private func read<T: Decodable>(_ type: T.Type, _ url: URL?) -> T? {
        guard let url, let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    private func write<T: Encodable>(_ value: T, _ url: URL?) {
        guard let url, let directory, let data = try? JSONEncoder().encode(value) else { return }
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? data.write(to: url, options: [.atomic, .completeFileProtection])
    }

    func snapshot(server: String) -> TaskListSnapshot? { read(TaskListSnapshot.self, file("tasks", server: server)) }
    func save(_ snapshot: TaskListSnapshot, server: String) { write(snapshot, file("tasks", server: server)) }

    func conversation(task: String, server: String) -> Conversation? {
        read(Conversations.self, file("conversations", server: server))?.byTask[task]
    }

    func save(_ conversation: TaskConversation, task: String, server: String) {
        let url = file("conversations", server: server)
        var all = read(Conversations.self, url) ?? Conversations()
        all.byTask[task] = Conversation(events: conversation.events, pending: conversation.pending,
                                        cursor: conversation.cursor, hasMore: conversation.hasMore)
        all.order.removeAll { $0 == task }
        all.order.append(task)
        while all.order.count > conversationLimit { all.byTask[all.order.removeFirst()] = nil }
        write(all, url)
    }

    func remove(server: String) {
        for name in ["tasks", "conversations"] {
            if let url = file(name, server: server) { try? FileManager.default.removeItem(at: url) }
        }
    }
}
