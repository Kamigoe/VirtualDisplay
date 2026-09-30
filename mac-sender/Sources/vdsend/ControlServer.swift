import Foundation
import Network

/// TCP control channel (docs/protocol.md). One viewer at a time; a new connection replaces the old one.
final class ControlServer: @unchecked Sendable { // state is confined to `queue`
    private let listener: NWListener
    private let queue = DispatchQueue(label: "vdsend.control", qos: .userInteractive)
    private let streamer: Streamer
    private let video: VideoSender

    // Queue-confined.
    private var client: NWConnection?
    private var lastKeyframeRequest: UInt64 = 0

    private static let viewerID = "udp"
    private static let keyframeRequestInterval: UInt64 = 100_000 // µs
    private static let maxMessage = 64 * 1024

    init(port: UInt16, streamer: Streamer, video: VideoSender) throws {
        self.streamer = streamer
        self.video = video
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        let params = NWParameters(tls: nil, tcp: tcp)
        params.allowLocalEndpointReuse = true
        listener = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: log("control listening on tcp port \(port)")
            case .failed(let e):
                log("control listener failed: \(e)")
                exit(1)
            default: break
            }
        }
        listener.newConnectionHandler = { [weak self] c in self?.accept(c) }
        listener.start(queue: queue)
    }

    // MARK: - connection lifecycle (queue-confined)

    private func accept(_ c: NWConnection) {
        if let old = client {
            log("replacing viewer \(old.endpoint) with \(c.endpoint)")
            old.cancel()
            video.endSession()
        }
        client = c
        c.stateUpdateHandler = { [weak self, weak c] state in
            guard let self, let c else { return }
            switch state {
            case .ready: log("viewer connected: \(c.endpoint)")
            case .failed(let e):
                log("viewer \(c.endpoint) failed: \(e)")
                self.detach(c)
            case .cancelled: self.detach(c)
            default: break
            }
        }
        c.start(queue: queue)
        readMessage(c)
    }

    private func detach(_ c: NWConnection) {
        guard client === c else { return }
        client = nil
        video.endSession()
        Task { @MainActor in self.streamer.setViewer(Self.viewerID, active: false) }
        log("viewer disconnected: \(c.endpoint)")
    }

    private func readMessage(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 4, maximumLength: 4) { [weak self] header, _, done, err in
            guard let self else { return }
            guard err == nil, !done, let header, header.count == 4 else { c.cancel(); return }
            var r = ByteReader(header)
            let len = Int(r.u32()!)
            guard len >= 1, len <= Self.maxMessage else {
                log("control: bad message length \(len)")
                c.cancel()
                return
            }
            c.receive(minimumIncompleteLength: len, maximumLength: len) { body, _, done, err in
                guard err == nil, let body, body.count == len else { c.cancel(); return }
                self.handle(body, from: c)
                if !done { self.readMessage(c) } else { c.cancel() }
            }
        }
    }

    private func handle(_ msg: Data, from c: NWConnection) {
        guard client === c else { return }
        var r = ByteReader(msg)
        guard let t = r.u8(), let type = Proto.Control(rawValue: t) else {
            log("control: unknown message type")
            return
        }
        switch type {
        case .hello: handleHello(&r, from: c)
        case .keyframeRequest:
            let now = serverMicrosNow()
            guard now &- lastKeyframeRequest >= Self.keyframeRequestInterval else { return }
            lastKeyframeRequest = now
            Task { @MainActor in self.streamer.requestKeyframe() }
        case .ping:
            guard let clientMicros = r.u64() else { return }
            var w = ByteWriter()
            w.u64(clientMicros)
            w.u64(serverMicrosNow())
            send(.pong, w.data, to: c)
        default:
            log("control: unexpected message \(type)")
        }
    }

    private func handleHello(_ r: inout ByteReader, from c: NWConnection) {
        guard let version = r.u16(), let width = r.u16(), let height = r.u16(), let hidpi = r.u8(),
              let refreshMilli = r.u32(), let chroma = r.u8(), let bitrateKbps = r.u32() else {
            fail(c, "malformed Hello")
            return
        }
        guard version == Proto.version else {
            fail(c, "protocol version \(version) not supported (server speaks \(Proto.version))")
            return
        }
        log("hello from \(c.endpoint): \(width)x\(height)\(hidpi != 0 ? " HiDPI" : "") @ \(Double(refreshMilli) / 1000) Hz, chroma \(chroma == 1 ? 444 : 420), \(bitrateKbps) kbps")

        Task { @MainActor in
            var o = self.streamer.options
            if !o.lockMode {
                if width > 0, height > 0 {
                    guard (320...8192).contains(Int(width)), (240...8192).contains(Int(height)),
                          (1_000...240_000).contains(refreshMilli) else {
                        self.queue.async { self.fail(c, "requested mode out of range") }
                        return
                    }
                    o.width = Int(width)
                    o.height = Int(height)
                    o.hiDPI = hidpi != 0
                    o.refresh = Double(refreshMilli) / 1000
                }
                o.chroma = chroma == 1 ? 444 : 420
                if bitrateKbps > 0 { o.bitrateMbps = Double(bitrateKbps) / 1000 }
            }
            do {
                try await self.streamer.apply(o)
            } catch {
                self.queue.async { self.fail(c, "failed to configure: \(error.localizedDescription)") }
                return
            }
            self.streamer.setViewer(Self.viewerID, active: true)
            let applied = self.streamer.options
            self.queue.async {
                guard self.client === c else { return }
                let sid = UInt32.random(in: 1...UInt32.max)
                self.video.beginSession(sid)
                var w = ByteWriter()
                w.u16(Proto.version)
                w.u32(sid)
                w.u16(UInt16(applied.pixelWidth))
                w.u16(UInt16(applied.pixelHeight))
                w.u32(UInt32((applied.refresh * 1000).rounded()))
                w.u8(Proto.codecHEVC)
                w.u8(applied.chroma == 444 ? 1 : 0)
                self.send(.start, w.data, to: c)
                log("session \(String(format: "%08x", sid)) started: \(applied.pixelWidth)x\(applied.pixelHeight) @ \(applied.refresh) Hz")
            }
        }
    }

    private func send(_ type: Proto.Control, _ body: Data, to c: NWConnection) {
        var w = ByteWriter()
        w.u32(UInt32(body.count + 1))
        w.u8(type.rawValue)
        w.bytes(body)
        c.send(content: w.data, completion: .contentProcessed { _ in })
    }

    private func fail(_ c: NWConnection, _ message: String) {
        log("control: \(message)")
        send(.error, Data(message.utf8), to: c)
        c.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { _ in c.cancel() })
    }
}
