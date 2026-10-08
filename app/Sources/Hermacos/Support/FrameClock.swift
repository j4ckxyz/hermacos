import AppKit
import QuartzCore

/// Calls back once per display refresh while running. Drives the streaming text reveal so it
/// advances in step with what the screen can actually show.
@MainActor
final class FrameClock: NSObject {
    private var link: CADisplayLink?
    private var fallback: Timer?
    private var lastTimestamp: CFTimeInterval = 0
    private let onFrame: (Double) -> Void

    init(onFrame: @escaping (Double) -> Void) {
        self.onFrame = onFrame
    }

    var isRunning: Bool { link != nil || fallback != nil }

    func start() {
        guard !isRunning else { return }
        lastTimestamp = 0
        if let screen = NSApp.keyWindow?.screen ?? NSScreen.main {
            let link = screen.displayLink(target: self, selector: #selector(step(_:)))
            link.add(to: .main, forMode: .common)
            self.link = link
        } else {
            // No screen (headless session): still make progress.
            fallback = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.onFrame(1.0 / 60) }
            }
        }
    }

    func stop() {
        link?.invalidate()
        link = nil
        fallback?.invalidate()
        fallback = nil
    }

    @objc private func step(_ link: CADisplayLink) {
        let now = link.timestamp
        let delta = lastTimestamp == 0 ? link.duration : now - lastTimestamp
        lastTimestamp = now
        onFrame(delta)
    }
}
