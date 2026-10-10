import Foundation

struct ReconnectionDelay {
    private(set) var attempt = 0

    mutating func next() -> Duration {
        let seconds = min(pow(2, Double(min(attempt, 5))), 30)
        attempt = min(attempt + 1, 6)
        return .milliseconds(Int64(seconds * 1_000))
    }

    mutating func reset() { attempt = 0 }
}
