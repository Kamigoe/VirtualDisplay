import Foundation

let options = Options.parse(Array(CommandLine.arguments.dropFirst()))
let stats = Stats()

// Keep the system (and the virtual display) awake and out of App Nap while streaming.
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.userInitiated, .latencyCritical, .idleDisplaySleepDisabled],
    reason: "Streaming virtual display")

let display: VirtualDisplay
do {
    display = try VirtualDisplay(options: options)
} catch {
    log("error: \(error.localizedDescription)")
    exit(1)
}
guard display.waitForMode(options) else {
    log("error: virtual display did not reach \(options.pixelWidth)x\(options.pixelHeight)")
    exit(1)
}
display.configureArrangement(makeMain: options.makeMain, mirrorBuiltin: options.mirrorBuiltin)

var server: StreamServer!
let encoder: Encoder
do {
    encoder = try Encoder(options: options, stats: stats) { data, key in server.send(data, keyframe: key) }
    server = try StreamServer(port: options.port, stats: stats) { encoder.requestKeyframe() }
} catch {
    log("error: \(error.localizedDescription)")
    exit(1)
}

let capture = Capture { pb, displayTime in
    stats.frameCaptured()
    encoder.submit(pb, displayTime: displayTime)
}

Task {
    do {
        try await capture.start(displayID: display.displayID, options: options)
    } catch {
        log("error: \(error.localizedDescription)")
        exit(1)
    }
}

let statsInterval = 2.0
let statsTimer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
statsTimer.schedule(deadline: .now() + statsInterval, repeating: statsInterval)
statsTimer.setEventHandler { log(stats.drain(interval: statsInterval)) }
statsTimer.resume()

// Clean shutdown on Ctrl-C / SIGTERM: stop capture, then let the virtual display go away with the process.
var signalSources: [DispatchSourceSignal] = []
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler {
        log("shutting down")
        Task {
            await capture.stop()
            withExtendedLifetime((display, activity)) {}
            exit(0)
        }
    }
    src.resume()
    signalSources.append(src)
}

RunLoop.main.run()
