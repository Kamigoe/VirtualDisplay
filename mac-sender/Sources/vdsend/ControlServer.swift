import CoreGraphics
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
    /// server_us of the last control message from the viewer (it pings every 500 ms).
    private var lastHeard: UInt64 = 0
    private var watchdog: DispatchSourceTimer?

    /// Input from the current viewer: accepted only after it proved it knows the input secret.
    private enum Auth {
        case idle
        case challenged(InputCrypto.Pending)
        case ready(opener: InputCrypto.Opener, sealer: InputCrypto.Sealer)
    }
    private var auth = Auth.idle
    private let injector: Injector
    /// Last accessibility state reported to the viewer.
    private var reportedPermission: Bool?

    /// A viewer that vanished without closing (Wi-Fi drop) is dropped after this long.
    private static let viewerTimeout: UInt64 = 6_000_000 // µs

    private static let viewerID = "udp"
    private static let keyframeRequestInterval: UInt64 = 100_000 // µs
    private static let maxMessage = 64 * 1024

    init(port: UInt16, streamer: Streamer, video: VideoSender, displayID: CGDirectDisplayID) throws {
        self.streamer = streamer
        self.video = video
        injector = Injector(displayID: displayID)
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

        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 2, repeating: 2)
        t.setEventHandler { [weak self] in self?.checkViewer() }
        t.resume()
        watchdog = t
    }

    // MARK: - connection lifecycle (queue-confined)

    private func accept(_ c: NWConnection) {
        if let old = client {
            log("replacing viewer \(old.endpoint) with \(c.endpoint)")
            // Tell it why, so it does not reconnect at once and take the sender back (two viewers
            // would otherwise keep replacing each other). Viewers that predate this ignore it.
            send(.replaced, Data("\(c.endpoint)".utf8), to: old)
            old.send(content: nil, contentContext: .finalMessage, isComplete: true,
                     completion: .contentProcessed { _ in old.cancel() })
            // A viewer that vanished never acknowledges; do not keep it around waiting.
            queue.asyncAfter(deadline: .now() + 1) { old.cancel() }
            video.endSession()
        }
        resetInput()
        client = c
        lastHeard = serverMicrosNow()
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
        resetInput()
        video.endSession()
        Task { @MainActor in await self.streamer.removeViewer(Self.viewerID) }
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

    private func checkViewer() {
        sendInputStatus()
        guard let c = client, serverMicrosNow() &- lastHeard > Self.viewerTimeout else { return }
        log("viewer \(c.endpoint) silent for \(Self.viewerTimeout / 1_000_000)s; dropping it")
        c.cancel()
    }

    private func handle(_ msg: Data, from c: NWConnection) {
        guard client === c else { return }
        lastHeard = serverMicrosNow()
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
        case .authHello: handleAuthHello(Data(msg.dropFirst()), from: c)
        case .authConfirm: handleAuthConfirm(Data(msg.dropFirst()), from: c)
        case .secure: handleSecure(Data(msg.dropFirst()), from: c)
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
        // Optional trailing fields (protocol v1 extensions); absent from older viewers.
        let bitDepth: Int? = r.u8().flatMap { $0 == 8 || $0 == 10 ? Int($0) : nil } // 0 = sender's default
        log("hello from \(c.endpoint): \(width)x\(height)\(hidpi != 0 ? " HiDPI" : "") @ \(Double(refreshMilli) / 1000) Hz, chroma \(chroma == 1 ? 444 : 420), \(bitDepth.map { "\($0)-bit" } ?? "default depth"), \(bitrateKbps) kbps")

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
                if let bitDepth { o.bitDepth = bitDepth }
                // The M1 encoder emits 8-bit RExt for 4:4:4 even from 10-bit input.
                if o.chroma == 444 { o.bitDepth = 8 }
                if bitrateKbps > 0 { o.bitrateMbps = Double(bitrateKbps) / 1000 }
            }
            self.streamer.addViewer(Self.viewerID)
            do {
                try await self.streamer.apply(o)
            } catch {
                await self.streamer.removeViewer(Self.viewerID)
                self.queue.async { self.fail(c, "failed to configure: \(error.localizedDescription)") }
                return
            }
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
                w.u8(UInt8(applied.bitDepth))
                self.send(.start, w.data, to: c)
                log("session \(String(format: "%08x", sid)) started: \(applied.pixelWidth)x\(applied.pixelHeight) @ \(applied.refresh) Hz, \(applied.bitDepth)-bit")
            }
        }
    }

    // MARK: - input (queue-confined)

    /// Forgets the viewer's authentication and lets go of anything it still held.
    private func resetInput() {
        if case .ready = auth { injector.releaseAll() }
        auth = .idle
        reportedPermission = nil
    }

    private func authResult(_ status: Proto.AuthStatus, to c: NWConnection) {
        send(.authResult, Data([status.rawValue]), to: c)
    }

    private func handleAuthHello(_ body: Data, from c: NWConnection) {
        guard case .idle = auth else {
            log("input: handshake restarted out of order; input disabled for \(c.endpoint)")
            resetInput()
            authResult(.outOfOrder, to: c)
            return
        }
        guard let secret = InputCrypto.loadSecret() else {
            log("input: no input secret at \(InputCrypto.secretURL.path) (run scripts/make-input-secret.sh)")
            authResult(.noSecret, to: c)
            return
        }
        do {
            let (pending, challenge) = try InputCrypto.Pending.respond(psk: secret, hello: body)
            auth = .challenged(pending)
            send(.authChallenge, challenge, to: c)
        } catch {
            log("input: bad AuthHello from \(c.endpoint)")
            authResult(.failed, to: c)
        }
    }

    private func handleAuthConfirm(_ body: Data, from c: NWConnection) {
        guard case .challenged(let pending) = auth else {
            resetInput()
            authResult(.outOfOrder, to: c)
            return
        }
        guard let channel = pending.confirm(body) else {
            auth = .idle
            log("input: \(c.endpoint) does not have the same input secret; input refused")
            authResult(.failed, to: c)
            return
        }
        auth = .ready(opener: channel.opener, sealer: channel.sealer)
        authResult(.ok, to: c)
        log("input: \(c.endpoint) authenticated\(Injector.permitted ? "" : "; accessibility permission missing, events will be dropped")")
        sendInputStatus()
    }

    /// Tells the viewer whether events can be injected, when that is new or changed.
    private func sendInputStatus() {
        guard let c = client, case .ready(let opener, var sealer) = auth else { return }
        let permitted = Injector.permitted
        guard permitted != reportedPermission else { return }
        reportedPermission = permitted
        let sealed = sealer.seal(Proto.Input.status.rawValue, Data([permitted ? 1 : 0]))
        auth = .ready(opener: opener, sealer: sealer)
        send(.secure, sealed, to: c)
    }

    private func handleSecure(_ body: Data, from c: NWConnection) {
        guard case .ready(var opener, let sealer) = auth else { return } // not authenticated: ignore
        guard let opened = opener.open(body) else {
            log("input: message from \(c.endpoint) failed to authenticate; closing")
            resetInput()
            c.cancel()
            return
        }
        auth = .ready(opener: opener, sealer: sealer)
        var r = ByteReader(opened.1)
        guard let kind = Proto.Input(rawValue: opened.0) else { return }
        switch kind {
        case .key:
            guard let down = r.u8(), let code = r.u16(), let flags = r.u8() else { return }
            injector.key(code, down: down != 0, autorepeat: flags & 1 != 0)
        case .imeToggle:
            injector.imeToggle()
        case .pointer:
            guard let x = r.u16(), let y = r.u16() else { return }
            injector.pointer(x: x, y: y)
        case .button:
            guard let b = r.u8(), let down = r.u8() else { return }
            injector.button(b, down: down != 0)
        case .scroll:
            guard let dx = r.u16(), let dy = r.u16(), let unit = r.u8() else { return }
            injector.scroll(dx: Int16(bitPattern: dx), dy: Int16(bitPattern: dy), unit: unit)
        case .releaseAll:
            injector.releaseAll()
        case .status:
            break // Mac -> viewer only
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
