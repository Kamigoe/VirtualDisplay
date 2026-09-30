import Foundation

struct Options: Equatable {
    /// Logical size in points (what macOS shows as "looks like").
    var width = 1920
    var height = 1080
    var hiDPI = false
    var refresh = 60.0
    var chroma = 420
    var bitrateMbps = 50.0
    var port: UInt16 = 7777
    /// Phase-1 style raw Annex-B TCP stream for ffplay/mpv debugging (0 = off).
    var rawPort: UInt16 = 0
    /// Ignore the mode requested by the viewer and always use the command-line mode.
    var lockMode = false
    var name = "VirtualDisplay"
    var makeMain = false
    var mirrorBuiltin = false
    /// nil = decide automatically from pixel rate.
    var prioritizeSpeed: Bool? = nil
    /// Re-encode the last frame shortly after the screen goes still so text sharpens up.
    var refineIdle = true

    var scale: Int { hiDPI ? 2 : 1 }
    var pixelWidth: Int { width * scale }
    var pixelHeight: Int { height * scale }
    var pixelRate: Double { Double(pixelWidth * pixelHeight) * refresh }

    static let usage = """
    usage: vdsend [options]
      --mode WxH@HZ        initial logical size and refresh rate (default 1920x1080@60);
                           a connecting viewer may request another mode unless --lock-mode
      --hidpi              2x backing store (e.g. --mode 1920x1080@60 --hidpi encodes 3840x2160)
      --chroma 420|444     chroma subsampling (default 420; 444 needs a 4:4:4-capable decoder)
      --bitrate MBPS       average bitrate in Mbit/s (default 50)
      --port N             control (TCP) and video (UDP) port (default 7777)
      --raw-port N         also serve a raw HEVC Annex-B stream over TCP for ffplay/mpv (default off)
      --lock-mode          ignore viewer mode requests
      --name NAME          display name shown in System Settings (default VirtualDisplay)
      --main               make the virtual display the main display while running
      --mirror-builtin     mirror the built-in panel onto the virtual display while running
      --fast / --no-fast   force VideoToolbox speed-over-quality on/off (default: auto)
      --no-refine          do not re-encode the last frame when the screen goes idle
    """

    static func parse(_ args: [String]) -> Options {
        var o = Options()
        var it = args.makeIterator()
        func value(_ flag: String) -> String {
            guard let v = it.next() else { fail("\(flag) needs a value") }
            return v
        }
        while let a = it.next() {
            switch a {
            case "--mode":
                let v = value(a)
                let parts = v.split(separator: "@")
                let wh = parts[0].split(separator: "x")
                guard wh.count == 2, let w = Int(wh[0]), let h = Int(wh[1]), w > 0, h > 0 else { fail("bad --mode \(v)") }
                o.width = w
                o.height = h
                if parts.count > 1 {
                    guard let hz = Double(parts[1]), hz > 0 else { fail("bad refresh in --mode \(v)") }
                    o.refresh = hz
                }
            case "--hidpi": o.hiDPI = true
            case "--chroma":
                guard let c = Int(value(a)), c == 420 || c == 444 else { fail("--chroma must be 420 or 444") }
                o.chroma = c
            case "--bitrate":
                guard let b = Double(value(a)), b > 0 else { fail("bad --bitrate") }
                o.bitrateMbps = b
            case "--port":
                guard let p = UInt16(value(a)) else { fail("bad --port") }
                o.port = p
            case "--raw-port":
                guard let p = UInt16(value(a)) else { fail("bad --raw-port") }
                o.rawPort = p
            case "--lock-mode": o.lockMode = true
            case "--name": o.name = value(a)
            case "--main": o.makeMain = true
            case "--mirror-builtin": o.mirrorBuiltin = true
            case "--fast": o.prioritizeSpeed = true
            case "--no-fast": o.prioritizeSpeed = false
            case "--no-refine": o.refineIdle = false
            case "-h", "--help":
                print(usage)
                exit(0)
            default: fail("unknown option \(a)")
            }
        }
        return o
    }

    private static func fail(_ msg: String) -> Never {
        FileHandle.standardError.write(Data("vdsend: \(msg)\n\(usage)\n".utf8))
        exit(2)
    }
}
