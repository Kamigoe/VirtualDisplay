import CoreMedia
import Foundation
import VideoToolbox

/// Hardware HEVC encoder producing Annex-B access units (parameter sets prepended to keyframes).
final class Encoder {
    /// `captureTime` is the mach time the source frame was composited (re-encodes use the re-encode time).
    typealias OutputHandler = (_ annexB: Data, _ keyframe: Bool, _ captureTime: UInt64) -> Void

    private let options: Options
    private let stats: Stats
    private let onOutput: OutputHandler
    private let queue = DispatchQueue(label: "vdsend.encoder", qos: .userInteractive)
    private var session: VTCompressionSession!
    private var transfer: VTPixelTransferSession?
    private var pool: CVPixelBufferPool?

    // All of the following are only touched on `queue`.
    private var lastInput: CVPixelBuffer?
    /// Latest captured frame while paused (not yet converted/encoded).
    private var pendingRaw: CVPixelBuffer?
    /// Paused while nobody is watching: frames are tracked but not encoded (saves power on a fanless Mac).
    private var paused = true
    private var keyframeRequested = true
    private var refineWork: DispatchWorkItem?

    private static let refinePasses = 2
    private static let refineDelay: TimeInterval = 0.15

    init(options o: Options, stats: Stats, onOutput: @escaping OutputHandler) throws {
        options = o
        self.stats = stats
        self.onOutput = onOutput

        let spec: [CFString: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true]
        var s: VTCompressionSession?
        let st = VTCompressionSessionCreate(
            allocator: nil, width: Int32(o.pixelWidth), height: Int32(o.pixelHeight), codecType: kCMVideoCodecType_HEVC,
            encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard st == noErr, let s else {
            throw NSError(domain: "vdsend", code: 4, userInfo: [NSLocalizedDescriptionKey: "VTCompressionSessionCreate failed (\(st))"])
        }
        session = s

        let speed = o.prioritizeSpeed ?? (o.pixelRate > 400_000_000)
        let bytesPerSecond = o.bitrateMbps * 1_000_000 / 8
        var props: [(CFString, CFTypeRef)] = [
            (kVTCompressionPropertyKey_RealTime, kCFBooleanTrue),
            (kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse),
            (kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, speed ? kCFBooleanTrue : kCFBooleanFalse),
            (kVTCompressionPropertyKey_ExpectedFrameRate, o.refresh as CFNumber),
            // Keyframes only on demand (client connect / loss recovery).
            (kVTCompressionPropertyKey_MaxKeyFrameInterval, Int32.max as CFNumber),
            (kVTCompressionPropertyKey_AverageBitRate, Int(o.bitrateMbps * 1_000_000) as CFNumber),
            // Allow bursts up to 2x the average within a second (LAN has headroom; keeps scene changes sharp).
            (kVTCompressionPropertyKey_DataRateLimits, [bytesPerSecond * 2, 1.0] as CFArray),
            (kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2),
            (kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2),
            (kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2),
        ]
        // 4:4:4 input selects the Main 4:4:4 (RExt) profile automatically.
        if o.chroma == 420 {
            props.append((kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel))
        }
        for (k, v) in props {
            let r = VTSessionSetProperty(s, key: k, value: v)
            if r != noErr { log("warning: encoder rejected \(k) (\(r))") }
        }
        VTCompressionSessionPrepareToEncodeFrames(s)

        if o.chroma == 444 {
            var t: VTPixelTransferSession?
            VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &t)
            guard let t else { throw NSError(domain: "vdsend", code: 5, userInfo: [NSLocalizedDescriptionKey: "VTPixelTransferSessionCreate failed"]) }
            VTSessionSetProperty(t, key: kVTPixelTransferPropertyKey_DestinationYCbCrMatrix, value: kCVImageBufferYCbCrMatrix_ITU_R_709_2)
            VTSessionSetProperty(t, key: kVTPixelTransferPropertyKey_DestinationColorPrimaries, value: kCVImageBufferColorPrimaries_ITU_R_709_2)
            VTSessionSetProperty(t, key: kVTPixelTransferPropertyKey_DestinationTransferFunction, value: kCVImageBufferTransferFunction_ITU_R_709_2)
            transfer = t
            let attrs: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange,
                kCVPixelBufferWidthKey: o.pixelWidth,
                kCVPixelBufferHeightKey: o.pixelHeight,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        }
        log("encoder: HEVC \(o.chroma == 444 ? "4:4:4" : "4:2:0") \(o.pixelWidth)x\(o.pixelHeight) @ \(o.refresh) Hz, \(o.bitrateMbps) Mbps avg, speed-priority \(speed)")
    }

    /// Called from the capture queue with a freshly composited frame.
    func submit(_ pb: CVPixelBuffer, displayTime: UInt64) {
        queue.async {
            self.refineWork?.cancel()
            if self.paused {
                self.pendingRaw = pb
                self.lastInput = nil
                return
            }
            guard let input = self.convert(pb) else { return }
            self.lastInput = input
            self.encode(input, displayTime: displayTime)
            self.scheduleRefine(remaining: Self.refinePasses)
        }
    }

    /// Next frame will be an IDR. If the screen is idle, the last frame is re-encoded immediately.
    func requestKeyframe() {
        queue.async {
            self.keyframeRequested = true
            if !self.paused { self.encodeLatestNow() }
        }
    }

    /// Resuming always starts with an IDR of the latest frame, so a static screen still reaches the viewer.
    func setPaused(_ p: Bool) {
        queue.async {
            guard p != self.paused else { return }
            self.paused = p
            log("encoder \(p ? "paused (no viewers)" : "resumed")")
            if p {
                self.refineWork?.cancel()
            } else {
                self.keyframeRequested = true
                self.encodeLatestNow()
            }
        }
    }

    private func encodeLatestNow() {
        if self.lastInput == nil, let raw = self.pendingRaw {
            self.lastInput = self.convert(raw)
            self.pendingRaw = nil
        }
        if let last = self.lastInput {
            self.encode(last, displayTime: mach_absolute_time())
        }
    }

    // MARK: - queue-confined helpers

    private func convert(_ pb: CVPixelBuffer) -> CVPixelBuffer? {
        guard let transfer, let pool else { return pb }
        var out: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &out)
        guard let out, VTPixelTransferSessionTransferImage(transfer, from: pb, to: out) == noErr else {
            log("warning: 4:4:4 conversion failed")
            return nil
        }
        return out
    }

    private func scheduleRefine(remaining: Int) {
        guard options.refineIdle, remaining > 0 else { return }
        let work = DispatchWorkItem { [weak self] in
            guard let self, let last = self.lastInput else { return }
            self.encode(last, displayTime: mach_absolute_time())
            self.scheduleRefine(remaining: remaining - 1)
        }
        refineWork = work
        queue.asyncAfter(deadline: .now() + Self.refineDelay, execute: work)
    }

    private func encode(_ pb: CVPixelBuffer, displayTime: UInt64) {
        let t0 = mach_absolute_time()
        let pts = CMClockMakeHostTimeFromSystemUnits(t0)
        var frameProps: CFDictionary?
        if keyframeRequested {
            frameProps = [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary
            keyframeRequested = false
        }
        let st = VTCompressionSessionEncodeFrame(session, imageBuffer: pb, presentationTimeStamp: pts, duration: .invalid,
                                                 frameProperties: frameProps, infoFlagsOut: nil) { [weak self] status, _, sb in
            guard let self else { return }
            let t1 = mach_absolute_time()
            guard status == noErr, let sb, CMSampleBufferDataIsReady(sb) else {
                if status != noErr { log("encode error \(status)") }
                return
            }
            let key = Self.isKeyframe(sb)
            let data = Self.annexB(sb, includeParameterSets: key)
            self.stats.frameEncoded(bytes: data.count, encodeMs: machToMs(t1 - t0),
                                    pipelineMs: machToMs(t1 &- min(displayTime, t1)))
            self.onOutput(data, key, displayTime)
        }
        if st != noErr { log("VTCompressionSessionEncodeFrame failed \(st)") }
    }

    // MARK: - bitstream helpers

    private static func isKeyframe(_ sb: CMSampleBuffer) -> Bool {
        guard let arr = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]],
              let first = arr.first else { return true }
        return !((first[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
    }

    private static let startCode: [UInt8] = [0, 0, 0, 1]

    /// Converts VideoToolbox's length-prefixed (hvcC) sample into an Annex-B byte stream.
    private static func annexB(_ sb: CMSampleBuffer, includeParameterSets: Bool) -> Data {
        var out = Data()
        if includeParameterSets, let fd = CMSampleBufferGetFormatDescription(sb) {
            var count = 0
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fd, parameterSetIndex: 0, parameterSetPointerOut: nil,
                                                              parameterSetSizeOut: nil, parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
            for i in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fd, parameterSetIndex: i, parameterSetPointerOut: &ptr,
                                                                     parameterSetSizeOut: &size, parameterSetCountOut: nil,
                                                                     nalUnitHeaderLengthOut: nil) == noErr, let ptr {
                    out.append(contentsOf: startCode)
                    out.append(ptr, count: size)
                }
            }
        }
        guard let bb = CMSampleBufferGetDataBuffer(sb) else { return out }
        let total = CMBlockBufferGetDataLength(bb)
        var raw = [UInt8](repeating: 0, count: total)
        CMBlockBufferCopyDataBytes(bb, atOffset: 0, dataLength: total, destination: &raw)
        out.reserveCapacity(out.count + total + 16)
        var off = 0
        while off + 4 <= total {
            let len = Int(raw[off]) << 24 | Int(raw[off + 1]) << 16 | Int(raw[off + 2]) << 8 | Int(raw[off + 3])
            off += 4
            guard len > 0, off + len <= total else { break }
            out.append(contentsOf: startCode)
            out.append(contentsOf: raw[off..<(off + len)])
            off += len
        }
        return out
    }
}
