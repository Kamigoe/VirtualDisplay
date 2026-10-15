// Encodes synthetic UI-like frames with vdsend's VideoToolbox settings into an Annex-B file, for
// decoder benchmarks (`vdview --decode-test`).
// Build: swiftc -O tools/encfile.swift -o /tmp/encfile
// Usage: /tmp/encfile WxH FRAMES 8|10 420|444 OUT.hevc [speed|quality]
import CoreMedia
import Foundation
import VideoToolbox

let a = CommandLine.arguments
guard a.count >= 6, let x = a[1].firstIndex(of: "x"), let w = Int(a[1][..<x]), let h = Int(a[1][a[1].index(after: x)...]),
      let frames = Int(a[2]), let bits = Int(a[3]), let chroma = Int(a[4]) else {
    print("usage: encfile WxH FRAMES 8|10 420|444 OUT.hevc [speed|quality]")
    exit(2)
}
let speed = a.count > 6 ? a[6] == "speed" : true

// Draw in RGB, then convert with VTPixelTransferSession exactly like vdsend's 4:4:4 path.
let srcFormat: OSType = bits == 10 ? kCVPixelFormatType_ARGB2101010LEPacked : kCVPixelFormatType_32BGRA
let dstFormat: OSType = switch (bits, chroma) {
case (8, 420): kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
case (10, 420): kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
case (8, _): kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange
default: kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange
}

func makeBuffer(_ f: OSType) -> CVPixelBuffer {
    var pb: CVPixelBuffer?
    CVPixelBufferCreate(nil, w, h, f, [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pb)
    return pb!
}

/// Gradient background with small saturated "buttons", "badges" and text-like stripes that move.
func draw(_ pb: CVPixelBuffer, frame: Int) {
    CVPixelBufferLockBaseAddress(pb, [])
    defer { CVPixelBufferUnlockBaseAddress(pb, []) }
    let base = CVPixelBufferGetBaseAddress(pb)!
    let bpr = CVPixelBufferGetBytesPerRow(pb)
    for y in 0..<h {
        let row = (base + y * bpr).assumingMemoryBound(to: UInt32.self)
        for x in 0..<w {
            var r = Double(x) / Double(w), g = Double(y) / Double(h), b = 0.5 + 0.3 * sin(Double(x + frame * 4) / 300)
            let cx = (x + frame * 3) % 160, cy = y % 90
            if cx < 14, cy < 14 { (r, g, b) = cy < 5 ? (1, 0.27, 0.23) : cy < 10 ? (1, 0.74, 0.18) : (0.16, 0.8, 0.25) }
            if cx > 40, cx < 140, cy > 30, cy < 42, (cx / 3 + cy) % 4 != 0 { (r, g, b) = (0.9, 0.9, 0.95) }
            let pr = UInt32(max(0, min(1, r)) * 1023), pg = UInt32(max(0, min(1, g)) * 1023), pbv = UInt32(max(0, min(1, b)) * 1023)
            row[x] = bits == 10 ? (3 << 30) | (pr << 20) | (pg << 10) | pbv
                : (255 << 24) | ((pr >> 2) << 16) | ((pg >> 2) << 8) | (pbv >> 2)
        }
    }
}

var t: VTPixelTransferSession?
VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &t)
VTSessionSetProperty(t!, key: kVTPixelTransferPropertyKey_DestinationYCbCrMatrix, value: kCVImageBufferYCbCrMatrix_ITU_R_709_2)

var s: VTCompressionSession?
let spec: [CFString: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true]
VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_HEVC, encoderSpecification: spec as CFDictionary,
                           imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
var props: [(CFString, CFTypeRef)] = [
    (kVTCompressionPropertyKey_RealTime, kCFBooleanTrue),
    (kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse),
    (kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, speed ? kCFBooleanTrue : kCFBooleanFalse),
    (kVTCompressionPropertyKey_ExpectedFrameRate, 60 as CFNumber),
    (kVTCompressionPropertyKey_AverageBitRate, 50_000_000 as CFNumber),
    (kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2),
]
if chroma == 420 {
    props.append((kVTCompressionPropertyKey_ProfileLevel, bits == 10 ? kVTProfileLevel_HEVC_Main10_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel))
}
for (k, v) in props where VTSessionSetProperty(s!, key: k, value: v) != noErr { print("rejected \(k)") }

let src = makeBuffer(srcFormat), dst = makeBuffer(dstFormat)
var out = Data()
let lock = NSLock()
for i in 0..<frames {
    draw(src, frame: i)
    precondition(VTPixelTransferSessionTransferImage(t!, from: src, to: dst) == noErr)
    VTCompressionSessionEncodeFrame(s!, imageBuffer: dst, presentationTimeStamp: CMTime(value: Int64(i), timescale: 60), duration: .invalid,
                                    frameProperties: nil, infoFlagsOut: nil) { st, _, sb in
        guard st == noErr, let sb else { return }
        let notSync = ((CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]])?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false
        var au = Data()
        if !notSync, let fd = CMSampleBufferGetFormatDescription(sb) {
            var n = 0
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fd, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil, parameterSetCountOut: &n, nalUnitHeaderLengthOut: nil)
            for k in 0..<n {
                var p: UnsafePointer<UInt8>?; var sz = 0
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fd, parameterSetIndex: k, parameterSetPointerOut: &p, parameterSetSizeOut: &sz, parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
                au += [0, 0, 0, 1]; au.append(p!, count: sz)
            }
        }
        let bb = CMSampleBufferGetDataBuffer(sb)!
        let total = CMBlockBufferGetDataLength(bb)
        var raw = [UInt8](repeating: 0, count: total)
        CMBlockBufferCopyDataBytes(bb, atOffset: 0, dataLength: total, destination: &raw)
        var off = 0
        while off + 4 <= total {
            let len = Int(raw[off]) << 24 | Int(raw[off + 1]) << 16 | Int(raw[off + 2]) << 8 | Int(raw[off + 3])
            off += 4; au += [0, 0, 0, 1]; au += raw[off..<off + len]; off += len
        }
        lock.lock(); out += au; lock.unlock()
    }
    VTCompressionSessionCompleteFrames(s!, untilPresentationTimeStamp: .invalid)
}
try! out.write(to: URL(fileURLWithPath: a[5]))
print("wrote \(frames) frames, \(out.count / 1024) KiB to \(a[5])")
