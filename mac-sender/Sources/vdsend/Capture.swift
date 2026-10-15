import CoreMedia
import Foundation
import ScreenCaptureKit

/// Captures one display with ScreenCaptureKit. Only frames whose content changed are delivered.
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    typealias FrameHandler = (_ pixelBuffer: CVPixelBuffer, _ displayTime: UInt64) -> Void

    private let queue = DispatchQueue(label: "vdsend.capture", qos: .userInteractive)
    private var stream: SCStream?
    private let onFrame: FrameHandler
    private let onStopped: (Capture, Error) -> Void

    init(onFrame: @escaping FrameHandler, onStopped: @escaping (Capture, Error) -> Void) {
        self.onFrame = onFrame
        self.onStopped = onStopped
    }

    func start(displayID: CGDirectDisplayID, options o: Options) async throws {
        var display: SCDisplay?
        // A freshly created virtual display can take a moment to show up in shareable content.
        for _ in 0..<50 {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            display = content.displays.first { $0.displayID == displayID }
            if display != nil { break }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        guard let display else {
            throw NSError(domain: "vdsend", code: 3, userInfo: [NSLocalizedDescriptionKey: "display \(displayID) not visible to ScreenCaptureKit"])
        }

        let cfg = SCStreamConfiguration()
        cfg.width = o.pixelWidth
        cfg.height = o.pixelHeight
        cfg.scalesToFit = false
        cfg.showsCursor = true
        cfg.capturesAudio = false
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(o.refresh.rounded()))
        cfg.queueDepth = 5
        cfg.colorSpaceName = CGColorSpace.sRGB
        cfg.pixelFormat = Self.pixelFormat(o)
        if o.chroma == 420 {
            // WindowServer converts to 4:2:0 on the GPU; hand straight to the encoder.
            cfg.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        }

        let s = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: cfg, delegate: self)
        try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try await s.startCapture()
        stream = s
        log("capture started: \(o.pixelWidth)x\(o.pixelHeight) @ \(o.refresh) Hz, \(fourCC(cfg.pixelFormat))")
    }

    /// 4:2:0 comes straight from WindowServer as YCbCr ('420v' / 10-bit 'x420'); 4:4:4 is captured
    /// as RGB ('BGRA' / 10-bit 'l10r') and converted by the encoder.
    static func pixelFormat(_ o: Options) -> OSType {
        switch (o.chroma, o.bitDepth) {
        case (420, 10): kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
        case (420, _): kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        case (_, 10): kCVPixelFormatType_ARGB2101010LEPacked
        default: kCVPixelFormatType_32BGRA
        }
    }

    func stop() async {
        try? await stream?.stopCapture()
        stream = nil
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let rawStatus = info[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pb = CMSampleBufferGetImageBuffer(sb)
        else { return }
        let displayTime = (info[.displayTime] as? UInt64) ?? mach_absolute_time()
        onFrame(pb, displayTime)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        log("capture stopped with error: \(error.localizedDescription)")
        onStopped(self, error)
    }
}
