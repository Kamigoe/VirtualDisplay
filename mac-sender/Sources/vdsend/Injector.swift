import CoreGraphics
import Foundation

/// Replays the viewer's keyboard and mouse as macOS events (CGEventPost at the HID level, so they
/// behave like a local keyboard and mouse). Needs the Accessibility permission; without it macOS
/// silently drops the events. Not thread-safe: used from the control queue only.
final class Injector {
    private let displayID: CGDirectDisplayID
    private let source = CGEventSource(stateID: .hidSystemState)

    private var keysDown = Set<UInt16>()
    private var buttonsDown = Set<UInt8>()
    private var flags: CGEventFlags = []
    private var position: CGPoint
    /// The Mac's input mode as last set from here (半角/全角 toggles it).
    private var kana = false
    /// Sub-pixel scroll left over (wheel steps arrive in hundredths of a line).
    private var scrollRest = (dx: 0.0, dy: 0.0)
    private var lastClick: (button: UInt8, time: UInt64, position: CGPoint, count: Int64)?

    /// Scroll distance of one wheel notch (mac-input-cast used the same).
    private static let linePixels = 40.0
    private static let eisu: UInt16 = 102
    private static let kanaKey: UInt16 = 104

    init(displayID: CGDirectDisplayID) {
        self.displayID = displayID
        let b = CGDisplayBounds(displayID)
        position = CGPoint(x: b.midX, y: b.midY)
    }

    static var permitted: Bool { CGPreflightPostEventAccess() }

    // MARK: - keyboard

    private static func modifierFlag(_ code: UInt16) -> CGEventFlags? {
        switch code {
        case 55, 54: return .maskCommand
        case 56, 60: return .maskShift
        case 58, 61: return .maskAlternate
        case 59, 62: return .maskControl
        default: return nil
        }
    }

    func key(_ code: UInt16, down: Bool, autorepeat: Bool) {
        if down { keysDown.insert(code) } else { keysDown.remove(code) }
        if let flag = Self.modifierFlag(code) {
            // Held while either the left or the right key is down.
            let held = keysDown.contains { Self.modifierFlag($0) == flag }
            if held { flags.insert(flag) } else { flags.remove(flag) }
        }
        if down && code == Self.eisu { kana = false }
        if down && code == Self.kanaKey { kana = true }
        guard let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { return }
        e.flags = flags
        if autorepeat { e.setIntegerValueField(.keyboardEventAutorepeat, value: 1) }
        e.post(tap: .cghidEventTap)
    }

    /// 半角/全角: switch to the other input mode with the Mac's own 英数 / かな keys.
    func imeToggle() {
        let code = kana ? Self.eisu : Self.kanaKey
        key(code, down: true, autorepeat: false)
        key(code, down: false, autorepeat: false)
    }

    // MARK: - mouse

    /// `x`, `y`: 0...65535 across the virtual display.
    func pointer(x: UInt16, y: UInt16) {
        let b = CGDisplayBounds(displayID)
        position = CGPoint(x: b.minX + Double(x) / 65535 * (b.width - 1),
                           y: b.minY + Double(y) / 65535 * (b.height - 1))
        let (type, button): (CGEventType, UInt8) =
            if buttonsDown.contains(0) { (.leftMouseDragged, 0) }
            else if buttonsDown.contains(1) { (.rightMouseDragged, 1) }
            else if let other = buttonsDown.min() { (.otherMouseDragged, other) }
            else { (.mouseMoved, 0) }
        postMouse(type, button: button)
    }

    /// 0 left, 1 right, 2 middle, 3 back, 4 forward.
    func button(_ index: UInt8, down: Bool) {
        guard index <= 4 else { return }
        if down { buttonsDown.insert(index) } else { buttonsDown.remove(index) }
        let type: CGEventType = switch (index, down) {
        case (0, true): .leftMouseDown
        case (0, false): .leftMouseUp
        case (1, true): .rightMouseDown
        case (1, false): .rightMouseUp
        case (_, true): .otherMouseDown
        case (_, false): .otherMouseUp
        }
        // Apps tell double clicks by the click count macOS would have computed.
        if down {
            let now = mach_absolute_time()
            let near = lastClick.map {
                $0.button == index && machToMs(now - $0.time) < Self.doubleClickMs
                    && abs($0.position.x - position.x) < 5 && abs($0.position.y - position.y) < 5
            } ?? false
            let count = near ? lastClick!.count + 1 : 1
            lastClick = (index, now, position, count)
        }
        postMouse(type, button: index, clicks: lastClick?.count ?? 1)
    }

    /// The user's double-click speed (System Settings > Mouse), in ms.
    private static var doubleClickMs: Double {
        let s = UserDefaults.standard.persistentDomain(forName: UserDefaults.globalDomain)?["com.apple.mouse.doubleClickThreshold"]
        return ((s as? Double) ?? 0.5) * 1000
    }

    /// `unit` 0: hundredths of a line (wheel); 1: pixels (touchpad). Positive dy scrolls up.
    func scroll(dx: Int16, dy: Int16, unit: UInt8) {
        let k = unit == 0 ? Self.linePixels / 100 : 1
        scrollRest.dx += Double(dx) * k
        scrollRest.dy += Double(dy) * k
        let px = scrollRest.dx.rounded(.towardZero), py = scrollRest.dy.rounded(.towardZero)
        scrollRest.dx -= px
        scrollRest.dy -= py
        guard px != 0 || py != 0,
              let e = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                              wheel1: Int32(py), wheel2: Int32(px), wheel3: 0) else { return }
        e.location = position
        e.flags = flags
        e.post(tap: .cghidEventTap)
    }

    private func postMouse(_ type: CGEventType, button: UInt8, clicks: Int64 = 0) {
        let cgButton: CGMouseButton = button == 0 ? .left : button == 1 ? .right : .center
        guard let e = CGEvent(mouseEventSource: source, mouseType: type,
                              mouseCursorPosition: position, mouseButton: cgButton) else { return }
        e.flags = flags
        if clicks > 0 { e.setIntegerValueField(.mouseEventClickState, value: clicks) }
        if button >= 2 { e.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button)) }
        e.post(tap: .cghidEventTap)
    }

    // MARK: -

    /// Lets go of everything still held (viewer left, lost focus, or turned input off).
    func releaseAll() {
        for b in buttonsDown.sorted() { button(b, down: false) }
        for k in keysDown.sorted(by: { (Self.modifierFlag($0) == nil ? 0 : 1) < (Self.modifierFlag($1) == nil ? 0 : 1) }) {
            key(k, down: false, autorepeat: false)
        }
        flags = []
        scrollRest = (0, 0)
    }
}
