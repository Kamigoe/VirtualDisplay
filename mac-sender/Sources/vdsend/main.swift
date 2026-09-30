import Foundation

let options = Options.parse(Array(CommandLine.arguments.dropFirst()))
let stats = Stats()

// Keep the system (and the virtual display) awake and out of App Nap while running.
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.userInitiated, .latencyCritical, .idleDisplaySleepDisabled],
    reason: "Streaming virtual display")

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
            onViewerChange: { active in Task { @MainActor in streamer.setViewer("raw", active: active) } },
            onNeedKeyframe: { _ = requestKeyframe() })
    }
} catch {
    log("error: \(error.localizedDescription)")
    exit(1)
}

// The display comes up immediately (so a lid-closed Mac has a screen) even before any viewer connects.
Task { @MainActor in
    do {
        try await streamer.apply(options)
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
