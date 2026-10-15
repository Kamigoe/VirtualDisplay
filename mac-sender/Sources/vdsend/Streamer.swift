import CoreGraphics
import Foundation

/// Owns the virtual display and the capture → encode pipeline. The display lives for the whole
/// process; capture and encoding run only while a viewer is attached, so an idle sender costs
/// nothing and lets the displays sleep. All methods run on the main actor.
@MainActor
final class Streamer {
    private(set) var options: Options
    private let stats: Stats
    private let onFrame: Encoder.OutputHandler

    private var display: VirtualDisplay?
    private var capture: Capture?
    private var encoder: Encoder?
    private var viewers = Set<String>()
    private var recovering = false
    /// Held while someone is watching: keeps the displays awake and the process out of App Nap.
    private var watchActivity: NSObjectProtocol?

    init(options: Options, stats: Stats, onFrame: @escaping Encoder.OutputHandler) {
        self.options = options
        self.stats = stats
        self.onFrame = onFrame
    }

    /// Creates the virtual display in the initial mode (so a lid-closed Mac has a screen right away).
    func start() async throws {
        VirtualDisplay.declareUserActivity()
        let d = try await VirtualDisplay.create(options: options)
        d.configureArrangement(makeMain: options.makeMain, mirrorBuiltin: options.mirrorBuiltin)
        display = d
    }

    /// Brings the display to `o` (re-moded in place) and, while anyone watches, (re)builds the pipeline.
    func apply(_ o: Options) async throws {
        guard let display else {
            throw NSError(domain: "vdsend", code: 8, userInfo: [NSLocalizedDescriptionKey: "virtual display not created"])
        }
        let displayChanged = o.width != options.width || o.height != options.height
            || o.hiDPI != options.hiDPI || o.refresh != options.refresh
        let pipelineChanged = displayChanged || o.chroma != options.chroma || o.bitDepth != options.bitDepth
            || o.bitrateMbps != options.bitrateMbps
        if displayChanged {
            await stopPipeline()
            VirtualDisplay.declareUserActivity()
            try await display.setMode(o)
        }
        options = o
        if viewers.isEmpty {
            await stopPipeline()
        } else if capture == nil || pipelineChanged {
            try await startPipeline()
        }
    }

    func requestKeyframe() {
        encoder?.requestKeyframe()
    }

    /// Call before `apply` when a viewer attaches.
    func addViewer(_ id: String) {
        viewers.insert(id)
        if watchActivity == nil {
            watchActivity = ProcessInfo.processInfo.beginActivity(
                options: [.userInitiated, .latencyCritical, .idleDisplaySleepDisabled],
                reason: "Streaming virtual display")
        }
    }

    func removeViewer(_ id: String) async {
        viewers.remove(id)
        guard viewers.isEmpty else { return }
        await stopPipeline()
        if let a = watchActivity {
            ProcessInfo.processInfo.endActivity(a)
            watchActivity = nil
        }
        log("no viewers: capture stopped, displays may sleep")
    }

    func stop() async {
        await stopPipeline()
        display = nil
    }

    // MARK: - pipeline

    private func startPipeline() async throws {
        await stopPipeline()
        // WindowServer stops compositing (and ScreenCaptureKit delivers nothing) while displays sleep.
        VirtualDisplay.declareUserActivity()
        let o = options
        if o.pixelRate > 650_000_000 {
            log(String(format: "warning: %.0f Mpx/s exceeds what the M1 encoder sustains (~650 Mpx/s); expect queueing latency", o.pixelRate / 1e6))
        }
        let enc = try Encoder(options: o, stats: stats, onOutput: onFrame)
        enc.setPaused(false)
        let stats = self.stats
        let cap = Capture(onFrame: { pb, displayTime in
            stats.frameCaptured()
            enc.submit(pb, displayTime: displayTime)
        }, onStopped: { [weak self] stopped, _ in
            Task { @MainActor in
                // Ignore late errors from a capture we already replaced.
                guard let self, self.capture === stopped else { return }
                await self.recover()
            }
        })
        try await cap.start(displayID: display!.displayID, options: o)
        encoder = enc
        capture = cap
        // The last viewer may have left while we were awaiting (actor reentrancy).
        if viewers.isEmpty { await stopPipeline() }
    }

    private func stopPipeline() async {
        let cap = capture
        capture = nil
        encoder = nil
        await cap?.stop()
    }

    /// ScreenCaptureKit stops when the display goes away or is reconfigured underneath it (display
    /// sleep/wake, Spaces changes). Rebuild the pipeline instead of exiting: with the lid closed this
    /// process is the only way back to the machine.
    private func recover() async {
        guard !recovering else { return }
        recovering = true
        defer { recovering = false }
        capture = nil
        encoder = nil
        var delay: UInt64 = 1
        while capture == nil && !viewers.isEmpty {
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            guard !viewers.isEmpty else { break }
            VirtualDisplay.declareUserActivity()
            do {
                try await display?.setMode(options)
                try await startPipeline()
                log("capture recovered")
            } catch {
                log("capture recovery failed: \(error.localizedDescription); retrying in \(min(delay * 2, 10))s")
                delay = min(delay * 2, 10)
            }
        }
    }
}
