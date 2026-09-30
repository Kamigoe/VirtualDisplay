import CoreGraphics
import Foundation

/// Owns the virtual display and the capture → encode pipeline, and rebuilds them when the mode changes.
/// All methods run on the main actor.
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

    init(options: Options, stats: Stats, onFrame: @escaping Encoder.OutputHandler) {
        self.options = options
        self.stats = stats
        self.onFrame = onFrame
    }

    /// Brings display + pipeline to `o`. The display is created once and re-moded in place.
    func apply(_ o: Options) async throws {
        let displayChanged = display == nil
            || o.width != options.width || o.height != options.height
            || o.hiDPI != options.hiDPI || o.refresh != options.refresh
        let pipelineChanged = displayChanged || capture == nil
            || o.chroma != options.chroma || o.bitrateMbps != options.bitrateMbps
        guard pipelineChanged else { return }

        // WindowServer stops compositing (and ScreenCaptureKit delivers nothing) while displays sleep.
        VirtualDisplay.declareUserActivity()

        await capture?.stop()
        capture = nil
        encoder = nil

        if let display {
            if displayChanged { try await display.setMode(o) }
        } else {
            let d = try await VirtualDisplay.create(options: o)
            d.configureArrangement(makeMain: o.makeMain, mirrorBuiltin: o.mirrorBuiltin)
            display = d
        }
        if o.pixelRate > 650_000_000 {
            log(String(format: "warning: %.0f Mpx/s exceeds what the M1 encoder sustains (~650 Mpx/s); expect queueing latency", o.pixelRate / 1e6))
        }

        let enc = try Encoder(options: o, stats: stats, onOutput: onFrame)
        enc.setPaused(viewers.isEmpty)
        let stats = self.stats
        let cap = Capture(onFrame: { pb, displayTime in
            stats.frameCaptured()
            enc.submit(pb, displayTime: displayTime)
        }, onStopped: { [weak self] _ in
            Task { @MainActor in await self?.recover() }
        })
        try await cap.start(displayID: display!.displayID, options: o)
        encoder = enc
        capture = cap
        options = o
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
        while capture == nil {
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            VirtualDisplay.declareUserActivity()
            do {
                try await display?.setMode(options)
                try await apply(options)
                log("capture recovered")
                if !viewers.isEmpty { encoder?.requestKeyframe() }
            } catch {
                log("capture recovery failed: \(error.localizedDescription); retrying in \(min(delay * 2, 10))s")
                delay = min(delay * 2, 10)
            }
        }
    }

    func requestKeyframe() {
        encoder?.requestKeyframe()
    }

    /// Encoding runs only while at least one viewer (UDP session or raw TCP) is attached.
    func setViewer(_ id: String, active: Bool) {
        if active {
            viewers.insert(id)
            VirtualDisplay.declareUserActivity() // the display may have slept while nobody was watching
        } else {
            viewers.remove(id)
        }
        encoder?.setPaused(viewers.isEmpty)
    }

    func stop() async {
        await capture?.stop()
        capture = nil
        encoder = nil
        display = nil
    }
}
