import CGVirtualDisplayPrivate
import CoreGraphics
import Foundation
import IOKit.pwr_mgt

/// A macOS virtual monitor. The display exists for as long as this object is alive.
///
/// WindowServer only brings the *first* CGVirtualDisplay of a process online (verified on macOS 26.5:
/// later ones never get a mode, even after the first is released), so one display is created per
/// process and mode changes are applied to it in place. That also keeps window placement intact.
final class VirtualDisplay {
    private let display: CGVirtualDisplay
    private let queue = DispatchQueue(label: "vdsend.display")
    var displayID: CGDirectDisplayID { display.displayID }

    /// Upper bound for any mode applied later (backing pixels).
    static let maxPixelsWide = 7680
    static let maxPixelsHigh = 4320
    private static let baseSerial: UInt32 = 0x5644_0001

    /// `serialOffset` lets the caller sidestep a stale display that still holds the serial.
    private init(options o: Options, serialOffset: UInt32) throws {
        let d = CGVirtualDisplayDescriptor()
        d.queue = queue
        d.name = o.name
        d.maxPixelsWide = UInt32(Self.maxPixelsWide)
        d.maxPixelsHigh = UInt32(Self.maxPixelsHigh)
        // Report a plausible physical size (~109 ppi standard, ~218 ppi HiDPI) so macOS picks sane defaults.
        let ppi = o.hiDPI ? 218.0 : 109.0
        d.sizeInMillimeters = CGSize(width: Double(o.pixelWidth) / ppi * 25.4, height: Double(o.pixelHeight) / ppi * 25.4)
        d.vendorID = 0x7664 // "vd"
        d.productID = 0x0001
        // Stable serial so macOS remembers arrangement/main-display choice between runs.
        d.serialNum = Self.baseSerial &+ serialOffset
        // sRGB / Rec.709 primaries, D65 white.
        d.redPrimary = CGPoint(x: 0.640, y: 0.330)
        d.greenPrimary = CGPoint(x: 0.300, y: 0.600)
        d.bluePrimary = CGPoint(x: 0.150, y: 0.060)
        d.whitePoint = CGPoint(x: 0.3127, y: 0.3290)
        d.terminationHandler = { _, _ in log("virtual display was terminated by the system") }

        guard let vd = CGVirtualDisplay(descriptor: d) else {
            throw NSError(domain: "vdsend", code: 1, userInfo: [NSLocalizedDescriptionKey: "CGVirtualDisplay init failed"])
        }
        display = vd
    }

    /// Creates the display and brings it to `o`, recovering from stale ones. While all displays are
    /// asleep, WindowServer defers tearing down virtual displays of exited processes, and a leftover
    /// with the same serial makes creation fail. Declaring user activity wakes the displays, which
    /// lets the teardown complete.
    static func create(options o: Options) async throws -> VirtualDisplay {
        var lastError: Error?
        for attempt in 0..<4 {
            do {
                let d = try VirtualDisplay(options: o, serialOffset: attempt < 3 ? 0 : 1)
                try await d.setMode(o)
                return d
            } catch {
                lastError = error
                log("virtual display creation failed (attempt \(attempt + 1)); waking displays to flush stale ones")
                declareUserActivity()
                try await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
        throw lastError!
    }

    /// Switches the display to the mode in `o` (logical size, HiDPI, refresh rate).
    func setMode(_ o: Options) async throws {
        guard o.pixelWidth <= Self.maxPixelsWide, o.pixelHeight <= Self.maxPixelsHigh else {
            throw NSError(domain: "vdsend", code: 7, userInfo: [NSLocalizedDescriptionKey:
                "\(o.pixelWidth)x\(o.pixelHeight) exceeds the \(Self.maxPixelsWide)x\(Self.maxPixelsHigh) limit"])
        }
        let s = CGVirtualDisplaySettings()
        s.hiDPI = o.hiDPI ? 1 : 0
        s.modes = [CGVirtualDisplayMode(width: UInt32(o.width), height: UInt32(o.height), refreshRate: o.refresh)]
        guard display.apply(s) else {
            throw NSError(domain: "vdsend", code: 2, userInfo: [NSLocalizedDescriptionKey: "CGVirtualDisplay applySettings failed"])
        }
        guard await waitForMode(o) else {
            throw NSError(domain: "vdsend", code: 6, userInfo: [NSLocalizedDescriptionKey:
                "virtual display did not reach \(o.width)x\(o.height) pt / \(o.pixelWidth)x\(o.pixelHeight) px"])
        }
    }

    static func declareUserActivity() {
        var id: IOPMAssertionID = 0
        if IOPMAssertionDeclareUserActivity("vdsend: recreate virtual display" as CFString, kIOPMUserActiveLocal, &id) == kIOReturnSuccess {
            IOPMAssertionRelease(id)
        }
    }

    /// Waits until WindowServer reports the requested mode as current, selecting it explicitly if
    /// macOS picked another one (e.g. 2560x1440 at 1x instead of 1280x720 HiDPI, or a mode it
    /// remembered for this serial).
    private func waitForMode(_ o: Options, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        var lastSelect = Date.distantPast
        while Date() < deadline {
            if let m = CGDisplayCopyDisplayMode(displayID), Self.matches(m, o) {
                log("virtual display \(displayID): \(m.width)x\(m.height) pt, \(m.pixelWidth)x\(m.pixelHeight) px @ \(m.refreshRate) Hz")
                return true
            }
            if Date().timeIntervalSince(lastSelect) > 1 {
                lastSelect = Date()
                selectMode(o)
            }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        let describe = { (m: CGDisplayMode) in "\(m.width)x\(m.height)pt/\(m.pixelWidth)x\(m.pixelHeight)px@\(m.refreshRate)" }
        let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        let modes = (CGDisplayCopyAllDisplayModes(displayID, opts) as? [CGDisplayMode]) ?? []
        log("virtual display \(displayID) stuck at \(CGDisplayCopyDisplayMode(displayID).map(describe) ?? "no mode"); available: \(modes.map(describe).joined(separator: ", "))")
        return false
    }

    private static func matches(_ m: CGDisplayMode, _ o: Options) -> Bool {
        m.pixelWidth == o.pixelWidth && m.pixelHeight == o.pixelHeight && m.width == o.width && m.height == o.height
    }

    private func selectMode(_ o: Options) {
        let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        guard let modes = CGDisplayCopyAllDisplayModes(displayID, opts) as? [CGDisplayMode],
              let mode = modes.first(where: { Self.matches($0, o) }) else { return }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success, let cfg else { return }
        CGConfigureDisplayWithDisplayMode(cfg, displayID, mode, nil)
        CGCompleteDisplayConfiguration(cfg, .forSession)
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
