import Darwin
import Foundation

/// UDP side of the protocol: packetises encoded frames, learns the viewer's address from Keepalives,
/// and retransmits packets on Nack. Everything runs on one serial queue.
final class VideoSender {
    private struct SentFrame {
        let id: UInt32
        let flags: UInt8
        let captureMicros: UInt64
        let data: Data
        var packetCount: Int { max(1, (data.count + Proto.payloadSize - 1) / Proto.payloadSize) }
    }

    private let fd: Int32
    private let queue = DispatchQueue(label: "vdsend.video", qos: .userInteractive)
    private var readSource: DispatchSourceRead?
    private let stats: Stats
    /// Called (on an arbitrary queue) when a viewer becomes reachable and needs an IDR to start decoding.
    private let onViewerReady: () -> Void

    // Queue-confined state.
    private var sessionID: UInt32 = 0
    private var active = false
    private var destination: sockaddr_storage?
    private var destinationLength: socklen_t = 0
    private var nextFrameID: UInt32 = 0
    private var history: [SentFrame] = []
    private var sendBuffer = [UInt8](repeating: 0, count: Proto.maxDatagram)

    /// Frames kept for retransmission (~130ms at 120 Hz).
    private static let historyDepth = 16

    init(port: UInt16, stats: Stats, onViewerReady: @escaping () -> Void) throws {
        self.stats = stats
        self.onViewerReady = onViewerReady
        fd = socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP)
        guard fd >= 0 else { throw posixError("socket") }

        var off: Int32 = 0, on: Int32 = 1
        setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &off, socklen_t(MemoryLayout<Int32>.size)) // accept IPv4 too
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, socklen_t(MemoryLayout<Int32>.size))
        var sndbuf: Int32 = 4 << 20
        setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, socklen_t(MemoryLayout<Int32>.size))
        // Wi-Fi WMM "video" access category.
        var service: Int32 = NET_SERVICE_TYPE_VI
        setsockopt(fd, SOL_SOCKET, SO_NET_SERVICE_TYPE, &service, socklen_t(MemoryLayout<Int32>.size))
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)

        var addr = sockaddr_in6()
        addr.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
        addr.sin6_family = sa_family_t(AF_INET6)
        addr.sin6_port = port.bigEndian
        addr.sin6_addr = in6addr_any
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in6>.size)) }
        }
        guard rc == 0 else {
            let e = posixError("bind udp \(port)")
            close(fd)
            throw e
        }

        let src = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
        src.setEventHandler { [weak self] in self?.drainIncoming() }
        src.resume()
        readSource = src
        log("udp video on port \(port)")
    }

    func beginSession(_ id: UInt32) {
        queue.async {
            self.sessionID = id
            self.active = true
            self.destination = nil
            self.history.removeAll()
        }
    }

    func endSession() {
        queue.async {
            self.active = false
            self.destination = nil
            self.history.removeAll()
        }
    }

    /// Thread-safe. `captureMach` is the WindowServer composite time of the frame.
    func send(_ data: Data, keyframe: Bool, captureMach: UInt64) {
        queue.async {
            guard self.active, self.destination != nil else { return }
            let frame = SentFrame(id: self.nextFrameID, flags: keyframe ? Proto.flagKeyframe : 0,
                                  captureMicros: machToMicros(captureMach), data: data)
            self.nextFrameID &+= 1
            self.history.append(frame)
            if self.history.count > Self.historyDepth { self.history.removeFirst(self.history.count - Self.historyDepth) }
            for i in 0..<frame.packetCount { self.transmit(frame, index: i, retransmit: false) }
        }
    }

    // MARK: - queue-confined

    private func transmit(_ f: SentFrame, index: Int, retransmit: Bool) {
        guard var dest = destination else { return }
        let offset = index * Proto.payloadSize
        let len = min(Proto.payloadSize, f.data.count - offset)
        guard len >= 0 else { return }
        let total = Proto.videoHeaderSize + len
        let sent: Int = sendBuffer.withUnsafeMutableBytes { buf in
            var w = UnsafeWriter(buf)
            w.u8(Proto.videoMagic)
            w.u8(UInt8(Proto.version))
            w.u8(f.flags | (retransmit ? Proto.flagRetransmit : 0))
            w.u8(0)
            w.u32(sessionID)
            w.u32(f.id)
            w.u16(UInt16(index))
            w.u16(UInt16(f.packetCount))
            w.u32(UInt32(f.data.count))
            w.u64(f.captureMicros)
            f.data.withUnsafeBytes { src in
                buf.baseAddress!.advanced(by: Proto.videoHeaderSize).copyMemory(from: src.baseAddress!.advanced(by: offset), byteCount: len)
            }
            return withUnsafePointer(to: &dest) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, buf.baseAddress, total, 0, $0, destinationLength) }
            }
        }
        if sent < 0 {
            stats.packetSendFailed()
        } else {
            stats.packetSent(retransmit: retransmit)
        }
    }

    private func drainIncoming() {
        var buf = [UInt8](repeating: 0, count: 2048)
        while true {
            var from = sockaddr_storage()
            var fromLen = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let n = buf.withUnsafeMutableBytes { b in
                withUnsafeMutablePointer(to: &from) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, b.baseAddress, b.count, 0, $0, &fromLen) }
                }
            }
            guard n > 0 else { return }
            handleDatagram(buf[0..<n], from: from, fromLen: fromLen)
        }
    }

    private func handleDatagram(_ d: ArraySlice<UInt8>, from: sockaddr_storage, fromLen: socklen_t) {
        var r = ByteReader(d)
        guard active, let magic = r.u8(), let version = r.u8(), UInt16(version) == Proto.version else { return }
        switch magic {
        case Proto.keepaliveMagic:
            guard r.u16() != nil, let sid = r.u32(), sid == sessionID else { return }
            let wasUnknown = destination == nil
            destination = from
            destinationLength = fromLen
            if wasUnknown {
                log("viewer reachable over udp: \(Self.describe(from))")
                onViewerReady()
            }
        case Proto.nackMagic:
            guard let count = r.u16(), let sid = r.u32(), sid == sessionID, let fid = r.u32(),
                  let frame = history.last(where: { $0.id == fid }) else {
                stats.nackMissed()
                return
            }
            for _ in 0..<count {
                guard let idx = r.u16(), Int(idx) < frame.packetCount else { break }
                transmit(frame, index: Int(idx), retransmit: true)
            }
        default:
            break
        }
    }

    private static func describe(_ s: sockaddr_storage) -> String {
        var s = s
        let len = socklen_t(s.ss_len)
        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        var serv = [CChar](repeating: 0, count: Int(NI_MAXSERV))
        _ = withUnsafePointer(to: &s) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getnameinfo($0, len, &host, socklen_t(host.count), &serv, socklen_t(serv.count), NI_NUMERICHOST | NI_NUMERICSERV)
            }
        }
        return "\(String(cString: host)):\(String(cString: serv))"
    }
}

/// Minimal big-endian writer over a raw buffer (avoids allocations on the hot path).
private struct UnsafeWriter {
    let buf: UnsafeMutableRawBufferPointer
    var pos = 0
    init(_ buf: UnsafeMutableRawBufferPointer) { self.buf = buf }
    mutating func u8(_ v: UInt8) { buf[pos] = v; pos += 1 }
    mutating func u16(_ v: UInt16) { buf.storeBytes(of: v.bigEndian, toByteOffset: pos, as: UInt16.self); pos += 2 }
    mutating func u32(_ v: UInt32) { buf.storeBytes(of: v.bigEndian, toByteOffset: pos, as: UInt32.self); pos += 4 }
    mutating func u64(_ v: UInt64) { buf.storeBytes(of: v.bigEndian, toByteOffset: pos, as: UInt64.self); pos += 8 }
}

func posixError(_ what: String) -> NSError {
    NSError(domain: NSPOSIXErrorDomain, code: Int(errno), userInfo: [NSLocalizedDescriptionKey: "\(what): \(String(cString: strerror(errno)))"])
}
