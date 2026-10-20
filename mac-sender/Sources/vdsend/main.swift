import Foundation

import CoreGraphics

// Permission helpers for the installed app bundle (run via `open vdsend.app --args ...` so macOS
// attributes the request to the app, not the terminal).
if CommandLine.arguments.contains("--check-permission") {
    let ok = CGPreflightScreenCaptureAccess()
    print("screen recording permission: \(ok ? "granted" : "not granted")")
    exit(ok ? 0 : 1)
}
if CommandLine.arguments.contains("--request-permission") {
    // Adds vdsend to System Settings > Privacy & Security > Screen & System Audio Recording.
    let ok = CGRequestScreenCaptureAccess()
    print("screen recording permission: \(ok ? "granted" : "requested (enable vdsend in System Settings)")")
    exit(ok ? 0 : 1)
}

let options = Options.parse(Array(CommandLine.arguments.dropFirst()))
let stats = Stats()

// Keep the machine reachable for viewers. Display sleep is only blocked while someone watches (Streamer).
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.idleSystemSleepDisabled],
    reason: "Waiting for VirtualDisplay viewers")

var video: VideoSender!
var rawServer: RawStreamServer?
// Top-level code runs on the main thread.
let streamer = MainActor.assumeIsolated {
    Streamer(options: options, stats: stats) { data, key, captureTime in
        video.send(data, keyframe: key, captureMach: captureTime)
        rawServer?.send(data, keyframe: key)
    }
}
let requestKeyframe = { Task { @MainActor in streamer.requestKeyframe() } }

var control: ControlServer!
do {
    video = try VideoSender(port: options.port, stats: stats) { _ = requestKeyframe() }
    if options.rawPort != 0 {
        rawServer = try RawStreamServer(
            port: options.rawPort, stats: stats,
            onViewerChange: { active in
                Task { @MainActor in
                    if active {
                        streamer.addViewer("raw")
                        do { try await streamer.apply(streamer.options) } catch { log("error: \(error.localizedDescription)") }
                    } else {
                        await streamer.removeViewer("raw")
                    }
                }
            },
            onNeedKeyframe: { _ = requestKeyframe() })
    }
} catch {
    log("error: \(error.localizedDescription)")
    exit(1)
}

// The display comes up immediately (so a lid-closed Mac has a screen) even before any viewer connects.
Task { @MainActor in
    do {
        try await streamer.start()
        control = try ControlServer(port: options.port, streamer: streamer, video: video)
    } catch {
        log("error: \(error.localizedDescription)")
        exit(1)
    }
}

let statsInterval = 2.0
let statsTimer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
statsTimer.schedule(deadline: .now() + statsInterval, repeating: statsInterval)
statsTimer.setEventHandler {
    let line = stats.drain(interval: statsInterval)
    if !line.hasPrefix("capture   0.0 fps | encode   0.0 fps") { log(line) }
}
statsTimer.resume()

// Clean shutdown on Ctrl-C / SIGTERM: stop capture, then let the virtual display go away with the process.
var signalSources: [DispatchSourceSignal] = []
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler {
        log("shutting down")
        Task { @MainActor in
            await streamer.stop()
            withExtendedLifetime(activity) {}
            exit(0)
        }
    }
    src.resume()
    signalSources.append(src)
}

RunLoop.main.run()
