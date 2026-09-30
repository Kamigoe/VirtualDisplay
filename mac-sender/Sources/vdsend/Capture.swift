import CoreMedia
import Foundation
import ScreenCaptureKit

/// Captures one display with ScreenCaptureKit. Only frames whose content changed are delivered.
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    typealias FrameHandler = (_ pixelBuffer: CVPixelBuffer, _ displayTime: UInt64) -> Void

    private let queue = DispatchQueue(label: "vdsend.capture", qos: .userInteractive)
    private var stream: SCStream?
    private let onFrame: FrameHandler
    private let onStopped: (Error) -> Void

    init(onFrame: @escaping FrameHandler, onStopped: @escaping (Error) -> Void) {
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
        if o.chroma == 420 {
            // WindowServer converts to 4:2:0 on the GPU; hand straight to the encoder.
            cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            cfg.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        } else {
            // Full-resolution colour; converted to 4:4:4 YCbCr before encoding.
            cfg.pixelFormat = kCVPixelFormatType_32BGRA
        }

        let s = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: cfg, delegate: self)
        try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try await s.startCapture()
        stream = s
        log("capture started: \(o.pixelWidth)x\(o.pixelHeight) @ \(o.refresh) Hz, \(o.chroma == 420 ? "420v" : "BGRA")")
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
        onStopped(error)
    }
}
