// Measures VideoToolbox HEVC encode throughput/latency with vdsend's realtime settings.
// Build: swiftc -O tools/encbench.swift -o /tmp/encbench
// Usage: /tmp/encbench WxH@FPS [8|10] [420|444] [speed|quality] [seconds]
import CoreMedia
import Foundation
import VideoToolbox

let a = CommandLine.arguments
guard a.count >= 2, let at = a[1].firstIndex(of: "@"), let x = a[1].firstIndex(of: "x"),
      let w = Int(a[1][..<x]), let h = Int(a[1][a[1].index(after: x)..<at]), let fps = Int(a[1][a[1].index(after: at)...]) else {
    print("usage: encbench WxH@FPS [8|10] [420|444] [speed|quality] [seconds]")
    exit(2)
}
let bits = a.count > 2 ? Int(a[2])! : 8
let chroma = a.count > 3 ? Int(a[3])! : 420
let speed = a.count > 4 ? a[4] == "speed" : true
let seconds = a.count > 5 ? Double(a[5])! : 3

let fmt: OSType = switch (bits, chroma) {
case (8, 420): kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
case (10, 420): kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
case (8, 444): kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange
default: kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange
}

var tb = mach_timebase_info_data_t()
mach_timebase_info(&tb)
func ms(_ t: UInt64) -> Double { Double(t) * Double(tb.numer) / Double(tb.denom) / 1e6 }

var s: VTCompressionSession?
let spec: [CFString: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true]
guard VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_HEVC,
                                 encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                 outputCallback: nil, refcon: nil, compressionSessionOut: &s) == noErr, let s else {
    print("session create failed"); exit(1)
}
var props: [(CFString, CFTypeRef)] = [
    (kVTCompressionPropertyKey_RealTime, kCFBooleanTrue),
    (kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse),
    (kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, speed ? kCFBooleanTrue : kCFBooleanFalse),
    (kVTCompressionPropertyKey_ExpectedFrameRate, fps as CFNumber),
    (kVTCompressionPropertyKey_AverageBitRate, 50_000_000 as CFNumber),
]
if chroma == 420 {
    props.append((kVTCompressionPropertyKey_ProfileLevel, bits == 10 ? kVTProfileLevel_HEVC_Main10_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel))
}
for (k, v) in props where VTSessionSetProperty(s, key: k, value: v) != noErr { print("rejected \(k)") }
VTCompressionSessionPrepareToEncodeFrames(s)

// A few frames of moving, gradient-heavy content.
let bufs: [CVPixelBuffer] = (0..<8).map { k in
    var pb: CVPixelBuffer?
    CVPixelBufferCreate(nil, w, h, fmt, [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pb)
    CVPixelBufferLockBaseAddress(pb!, [])
    for p in 0..<CVPixelBufferGetPlaneCount(pb!) {
        let base = CVPixelBufferGetBaseAddressOfPlane(pb!, p)!
        let bpr = CVPixelBufferGetBytesPerRowOfPlane(pb!, p), ph = CVPixelBufferGetHeightOfPlane(pb!, p)
        for y in 0..<ph {
            if bits == 10 {
                let row = (base + y * bpr).assumingMemoryBound(to: UInt16.self)
                for i in 0..<(bpr / 2) { row[i] = UInt16(((i + y + k * 9) * 3) % 900 + 64) << 6 }
            } else {
                let row = (base + y * bpr).assumingMemoryBound(to: UInt8.self)
                for i in 0..<bpr { row[i] = UInt8(((i + y + k * 9) / 2) % 220 + 16) }
            }
        }
    }
    CVPixelBufferUnlockBaseAddress(pb!, [])
    return pb!
}

let lock = NSLock()
var lat: [Double] = []
var bytes = 0
var desc = "?"
let n = Int(Double(fps) * seconds)
let start = Date()
for i in 0..<n {
    let d = start.addingTimeInterval(Double(i) / Double(fps)).timeIntervalSinceNow
    if d > 0 { Thread.sleep(forTimeInterval: d) }
    let t0 = mach_absolute_time()
    VTCompressionSessionEncodeFrame(s, imageBuffer: bufs[i % bufs.count], presentationTimeStamp: CMTime(value: Int64(i), timescale: Int32(fps)),
                                    duration: .invalid, frameProperties: nil, infoFlagsOut: nil) { st, _, sb in
        let t1 = mach_absolute_time()
        guard st == noErr, let sb else { return }
        lock.lock(); defer { lock.unlock() }
        if desc == "?", let f = CMSampleBufferGetFormatDescription(sb),
           let atoms = CMFormatDescriptionGetExtension(f, extensionKey: kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms) as? [String: Any],
           let c = atoms["hvcC"] as? Data {
            desc = "profile_idc \(c[1] & 0x1F), \(["4:0:0", "4:2:0", "4:2:2", "4:4:4"][Int(c[16] & 3)]), \((c[17] & 7) + 8)-bit"
        }
        bytes += CMSampleBufferGetTotalSampleSize(sb)
        lat.append(ms(t1 - t0))
    }
}
VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
let wall = -start.timeIntervalSinceNow
let l = lat.sorted()
print(String(format: "%@ %d-bit %d %@: %@ | %5.1f fps | enc p50 %5.2f p95 %5.2f ms | %4.0f Mbps",
             a[1], bits, chroma, speed ? "speed" : "quality", desc, Double(l.count) / wall,
             l[l.count / 2], l[Int(Double(l.count) * 0.95)], Double(bytes * 8) / wall / 1e6))
