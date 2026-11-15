import AppKit
import Foundation

/// The cursor and the text clipboard, shared with an authenticated viewer (docs/protocol.md,
/// "カーソルとクリップボード"). Runs on the main thread (AppKit); messages go out through `send`,
/// which seals them on the control queue.
///
/// - Cursor: while the viewer draws the cursor itself (CursorMode 1), the capture leaves it out
///   of the video and the current cursor picture is sent whenever it changes (polled at 60 Hz).
/// - Clipboard: once the viewer asks (ClipboardMode 1), text copied on the Mac is sent (polled
///   every 0.25 s). Text from the viewer is put on the pasteboard and not sent back. Text marked
///   concealed or transient (password managers) is never sent.
@MainActor
final class DesktopShare {
    typealias Send = @Sendable (_ inner: UInt8, _ body: Data) -> Void

    private let streamer: Streamer
    private let send: Send

    private var cursorTimer: Timer?
    private var lastShape: Data?

    private var clipboardTimer: Timer?
    /// changeCount of the pasteboard already handled (ours or sent).
    private var seenChange = 0
    private var incoming = ClipAssembler()

    static let maxText = 1 << 20
    static let chunk = 60_000
    static let maxCursorSide = 120.0

    init(streamer: Streamer, send: @escaping Send) {
        self.streamer = streamer
        self.send = send
    }

    /// The viewer went away (or a new one is not authenticated yet): back to the defaults.
    func reset() {
        setCursorLocal(false)
        setClipboard(false)
        incoming = ClipAssembler()
    }

    // MARK: - cursor

    func setCursorLocal(_ on: Bool) {
        streamer.setShowsCursor(!on)
        cursorTimer?.invalidate()
        cursorTimer = nil
        lastShape = nil
        guard on else { return }
        pollCursor()
        cursorTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollCursor() }
        }
    }

    private func pollCursor() {
        let o = streamer.options
        let scale = CGFloat(o.pixelWidth) / CGFloat(max(o.width, 1))
        guard let body = Self.cursorShape(scale: scale), body != lastShape else { return }
        lastShape = body
        send(Proto.Input.cursorShape.rawValue, body)
    }

    /// CursorShape body for the current system cursor, rendered at `scale` pixels per point
    /// (the virtual display's backing scale), at most `maxCursorSide` pixels a side.
    static func cursorShape(scale: CGFloat) -> Data? {
        guard let cursor = NSCursor.currentSystem else { return nil }
        let image = cursor.image
        let size = image.size
        guard size.width > 0, size.height > 0 else { return nil }
        let s = min(scale, maxCursorSide / size.width, maxCursorSide / size.height)
        let w = max(1, Int((size.width * s).rounded()))
        let h = max(1, Int((size.height * s).rounded()))
        var pixels = [UInt8](repeating: 0, count: w * h * 4)
        let drawn: Bool = pixels.withUnsafeMutableBytes { buf in
            guard let space = CGColorSpace(name: CGColorSpace.sRGB),
                  let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8,
                                      bytesPerRow: w * 4, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
            image.draw(in: NSRect(x: 0, y: 0, width: w, height: h), from: .zero, operation: .copy, fraction: 1)
            NSGraphicsContext.restoreGraphicsState()
            return true
        }
        guard drawn else { return nil }
        // The wire carries straight (not premultiplied) alpha. Row 0 of the buffer is the top.
        for i in stride(from: 0, to: pixels.count, by: 4) {
            let a = Int(pixels[i + 3])
            guard a > 0, a < 255 else { continue }
            for c in 0..<3 {
                pixels[i + c] = UInt8(min(255, (Int(pixels[i + c]) * 255 + a / 2) / a))
            }
        }
        // hotSpot is in the image's flipped coordinates (origin top left), in points.
        let hot = cursor.hotSpot
        var wr = ByteWriter()
        wr.u16(UInt16(w))
        wr.u16(UInt16(h))
        wr.u16(UInt16(min(max(0, Int((hot.x * s).rounded())), w - 1)))
        wr.u16(UInt16(min(max(0, Int((hot.y * s).rounded())), h - 1)))
        wr.bytes(Data(pixels))
        return wr.data
    }

    // MARK: - clipboard

    func setClipboard(_ on: Bool) {
        clipboardTimer?.invalidate()
        clipboardTimer = nil
        guard on else { return }
        // What is on the pasteboard already is not sent; only later copies are.
        seenChange = NSPasteboard.general.changeCount
        clipboardTimer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollClipboard() }
        }
    }

    private static let privateTypes: Set<String> = [
        "org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType",
    ]

    private func pollClipboard() {
        let pb = NSPasteboard.general
        guard pb.changeCount != seenChange else { return }
        seenChange = pb.changeCount
        if let types = pb.types, types.contains(where: { Self.privateTypes.contains($0.rawValue) }) { return }
        guard let raw = pb.string(forType: .string), !raw.isEmpty else { return }
        let text = raw.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        let bytes = Data(text.utf8)
        guard bytes.count <= Self.maxText else {
            log("clipboard: \(bytes.count / 1024) KiB of text is over the 1 MiB limit; not sent to the viewer")
            return
        }
        var offset = 0
        while offset < bytes.count {
            let end = min(offset + Self.chunk, bytes.count)
            var wr = ByteWriter()
            wr.u32(UInt32(bytes.count))
            wr.u32(UInt32(offset))
            wr.bytes(bytes.subdata(in: offset..<end))
            send(Proto.Input.clipText.rawValue, wr.data)
            offset = end
        }
    }

    /// A ClipText piece from the viewer.
    func received(_ body: Data) {
        guard let text = incoming.push(body) else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(text, forType: .string)
        seenChange = pb.changeCount // ours: not sent back
    }
}

/// Puts ClipText pieces back together (u32 total ‖ u32 offset ‖ UTF-8 bytes, in order).
struct ClipAssembler {
    private var total = 0
    private var buffer = Data()

    mutating func push(_ body: Data) -> String? {
        let body = Data(body) // indices from 0
        var r = ByteReader(body)
        guard let t = r.u32(), let o = r.u32() else { return nil }
        let total = Int(t), offset = Int(o)
        let piece = body.subdata(in: 8..<body.count)
        if offset == 0 {
            buffer = Data()
            self.total = total
        }
        guard total > 0, total <= DesktopShare.maxText, total == self.total,
              offset == buffer.count, offset + piece.count <= total else {
            buffer = Data()
            self.total = 0
            return nil
        }
        buffer.append(piece)
        guard buffer.count == total else { return nil }
        defer { buffer = Data(); self.total = 0 }
        return String(data: buffer, encoding: .utf8)
    }
}
