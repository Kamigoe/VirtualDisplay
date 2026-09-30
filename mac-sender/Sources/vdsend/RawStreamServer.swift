import Foundation
import Network

/// Debug transport: serves the raw Annex-B stream over TCP to a single viewer (a new connection
/// replaces the old one), plain enough for `ffplay`/`mpv` to consume directly.
final class RawStreamServer {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "vdsend.net", qos: .userInteractive)
    private let stats: Stats
    private let onNeedKeyframe: () -> Void
    private let onViewerChange: (Bool) -> Void

    // Only touched on `queue`.
    private var client: NWConnection?
    private var inflight = 0
    private var waitingForKeyframe = true

    /// Frames allowed to sit in the socket send path before we start dropping.
    private static let maxInflight = 3

    init(port: UInt16, stats: Stats, onViewerChange: @escaping (Bool) -> Void, onNeedKeyframe: @escaping () -> Void) throws {
        self.stats = stats
        self.onNeedKeyframe = onNeedKeyframe
        self.onViewerChange = onViewerChange
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        let params = NWParameters(tls: nil, tcp: tcp)
        params.serviceClass = .interactiveVideo
        params.allowLocalEndpointReuse = true
        listener = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: log("raw stream listening on tcp port \(port)")
            case .failed(let e):
                log("listener failed: \(e)")
                exit(1)
            default: break
            }
        }
        listener.newConnectionHandler = { [weak self] c in self?.accept(c) }
        listener.start(queue: queue)
    }

    private func accept(_ c: NWConnection) {
        if let old = client {
            log("replacing viewer \(old.endpoint) with \(c.endpoint)")
            old.cancel()
        }
        client = c
        inflight = 0
        waitingForKeyframe = true
        c.stateUpdateHandler = { [weak self, weak c] state in
            guard let self, let c else { return }
            switch state {
            case .ready:
                log("raw viewer connected: \(c.endpoint)")
                self.onViewerChange(true)
                self.onNeedKeyframe()
            case .failed(let e):
                log("viewer \(c.endpoint) failed: \(e)")
                self.drop(c)
            case .cancelled:
                self.drop(c)
            default: break
            }
        }
        c.start(queue: queue)
        receiveUntilClosed(c)
    }

    /// The viewer never sends anything; reading only serves to notice disconnects promptly.
    private func receiveUntilClosed(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] _, _, done, err in
            if done || err != nil {
                if let self, self.client === c { log("viewer disconnected: \(c.endpoint)") }
                c.cancel()
            } else {
                self?.receiveUntilClosed(c)
            }
        }
    }

    private func drop(_ c: NWConnection) {
        guard client === c else { return }
        client = nil
        onViewerChange(false)
    }

    /// Thread-safe. Drops frames (and asks for a keyframe) if the viewer falls behind.
    func send(_ data: Data, keyframe: Bool) {
        queue.async {
            guard let c = self.client, c.state == .ready else { return }
            if keyframe { self.waitingForKeyframe = false }
            if self.waitingForKeyframe || (self.inflight >= Self.maxInflight && !keyframe) {
                self.stats.frameDropped()
                if !self.waitingForKeyframe {
                    self.waitingForKeyframe = true
                    self.onNeedKeyframe()
                }
                return
            }
            self.inflight += 1
            c.send(content: data, completion: .contentProcessed { [weak self] err in
                guard let self else { return }
                if self.client === c { self.inflight -= 1 }
                if let err {
                    log("send error: \(err)")
                    c.cancel()
                }
            })
        }
    }
}
