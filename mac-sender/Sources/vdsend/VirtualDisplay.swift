import CGVirtualDisplayPrivate
import CoreGraphics
import Foundation

/// A macOS virtual monitor. The display exists for as long as this object is alive.
final class VirtualDisplay {
    private let display: CGVirtualDisplay
    private let queue = DispatchQueue(label: "vdsend.display")
    var displayID: CGDirectDisplayID { display.displayID }

    init(options o: Options) throws {
        let d = CGVirtualDisplayDescriptor()
        d.queue = queue
        d.name = o.name
        d.maxPixelsWide = UInt32(o.pixelWidth)
        d.maxPixelsHigh = UInt32(o.pixelHeight)
        // Report a plausible physical size (~109 ppi standard, ~218 ppi HiDPI) so macOS picks sane defaults.
        let ppi = o.hiDPI ? 218.0 : 109.0
        d.sizeInMillimeters = CGSize(width: Double(o.pixelWidth) / ppi * 25.4, height: Double(o.pixelHeight) / ppi * 25.4)
        d.vendorID = 0x7664 // "vd"
        d.productID = 0x0001
        // Stable per-mode serial so macOS remembers arrangement/main-display choice between runs.
        d.serialNum = UInt32(truncatingIfNeeded: o.width &* 31 &+ o.height &* 17 &+ o.scale)
        // sRGB / Rec.709 primaries, D65 white.
        d.redPrimary = CGPoint(x: 0.640, y: 0.330)
        d.greenPrimary = CGPoint(x: 0.300, y: 0.600)
        d.bluePrimary = CGPoint(x: 0.150, y: 0.060)
        d.whitePoint = CGPoint(x: 0.3127, y: 0.3290)
        d.terminationHandler = { _, _ in log("virtual display was terminated by the system") }

        guard let vd = CGVirtualDisplay(descriptor: d) else {
            throw NSError(domain: "vdsend", code: 1, userInfo: [NSLocalizedDescriptionKey: "CGVirtualDisplay init failed"])
        }
        let s = CGVirtualDisplaySettings()
        s.hiDPI = o.hiDPI ? 1 : 0
        s.modes = [CGVirtualDisplayMode(width: UInt32(o.width), height: UInt32(o.height), refreshRate: o.refresh)]
        guard vd.apply(s) else {
            throw NSError(domain: "vdsend", code: 2, userInfo: [NSLocalizedDescriptionKey: "CGVirtualDisplay applySettings failed"])
        }
        display = vd
    }

    /// Waits until WindowServer reports the requested mode as current.
    func waitForMode(_ o: Options, timeout: TimeInterval = 5) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let m = CGDisplayCopyDisplayMode(displayID),
               m.pixelWidth == o.pixelWidth, m.pixelHeight == o.pixelHeight {
                log("virtual display \(displayID): \(m.width)x\(m.height) pt, \(m.pixelWidth)x\(m.pixelHeight) px @ \(m.refreshRate) Hz")
                return true
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        return false
    }

    /// Optional arrangement changes. `.forAppOnly` makes macOS revert them when the process exits.
    func configureArrangement(makeMain: Bool, mirrorBuiltin: Bool) {
        guard makeMain || mirrorBuiltin else { return }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success, let cfg else { return }
        if makeMain {
            // The display at origin (0,0) is the main display.
            CGConfigureDisplayOrigin(cfg, displayID, 0, 0)
        }
        if mirrorBuiltin, let builtin = Self.builtinDisplay() {
            CGConfigureDisplayMirrorOfDisplay(cfg, builtin, displayID)
        }
        let r = CGCompleteDisplayConfiguration(cfg, .forAppOnly)
        log("display arrangement (main=\(makeMain), mirrorBuiltin=\(mirrorBuiltin)): \(r == .success ? "ok" : "failed \(r.rawValue)")")
    }

    private static func builtinDisplay() -> CGDirectDisplayID? {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        return ids.prefix(Int(n)).first { CGDisplayIsBuiltin($0) != 0 }
    }
}
