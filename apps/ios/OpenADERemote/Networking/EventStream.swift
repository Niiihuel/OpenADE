import Foundation

struct SSEMessage: Equatable, Sendable {
    var event: String
    var data: String
}

/// Server-sent events, fed byte by byte or in chunks of any size. A line ends
/// with LF, CR or CRLF; a blank line dispatches; `:` lines are keep-alives.
struct SSEParser {
    private var line: [UInt8] = []
    private var event = ""
    private var data: [String] = []
    private var afterCR = false
    /// A line longer than this is dropped rather than growing without bound.
    private let lineLimit = 16 * 1_024 * 1_024

    mutating func push<Bytes: Sequence>(_ bytes: Bytes) -> [SSEMessage] where Bytes.Element == UInt8 {
        bytes.compactMap { push($0) }
    }

    mutating func push(_ byte: UInt8) -> SSEMessage? {
        if afterCR {
            afterCR = false
            if byte == 10 { return nil }
        }
        guard byte == 10 || byte == 13 else {
            if line.count < lineLimit { line.append(byte) }
            return nil
        }
        afterCR = byte == 13
        let text = String(decoding: line, as: UTF8.self)
        line.removeAll(keepingCapacity: true)
        return process(text)
    }

    private mutating func process(_ text: String) -> SSEMessage? {
        if text.isEmpty {
            defer { event = ""; data = [] }
            guard !data.isEmpty else { return nil }
            return SSEMessage(event: event.isEmpty ? "message" : event, data: data.joined(separator: "\n"))
        }
        if text.hasPrefix(":") { return nil }
        let field: Substring
        var value: Substring
        if let colon = text.firstIndex(of: ":") {
            field = text[..<colon]
            value = text[text.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = text[...]
            value = ""
        }
        switch field {
        case "event": event = String(value)
        case "data": data.append(String(value))
        default: break
        }
        return nil
    }
}

enum EventStream {
    /// Parses the stream off the main thread; cancelling the consumer closes it.
    static func messages(from bytes: URLSession.AsyncBytes) -> AsyncThrowingStream<SSEMessage, Error> {
        AsyncThrowingStream { continuation in
            let reader = Task.detached {
                var parser = SSEParser()
                do {
                    for try await byte in bytes {
                        if let message = parser.push(byte) { continuation.yield(message) }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in reader.cancel() }
        }
    }
}
