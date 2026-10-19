import Foundation

/// Jittered exponential backoff from 0.5 s to 30 s. Each delay is between half
/// and all of its step, so phones that lost the PC together do not return together.
struct ReconnectionDelay {
    private(set) var attempt = 0
    private let random: () -> Double

    init(random: @escaping () -> Double = { Double.random(in: 0...1) }) { self.random = random }

    mutating func next() -> Duration {
        let step = min(0.5 * pow(2, Double(attempt)), 30)
        attempt = min(attempt + 1, 7)
        let seconds = step * (0.5 + 0.5 * min(max(random(), 0), 1))
        return .milliseconds(Int64((seconds * 1_000).rounded()))
    }

    mutating func reset() { attempt = 0 }
}
