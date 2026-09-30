import Foundation

private let timebase: mach_timebase_info_data_t = {
    var tb = mach_timebase_info_data_t()
    mach_timebase_info(&tb)
    return tb
}()

func machToMs(_ t: UInt64) -> Double {
    Double(t) * Double(timebase.numer) / Double(timebase.denom) / 1e6
}

private let logStart = mach_absolute_time()

func log(_ msg: String) {
    let start = logStart // globals are lazily initialised; read it before taking "now"
    let t = machToMs(mach_absolute_time() - start) / 1000
    FileHandle.standardError.write(Data(String(format: "[%8.3f] %@\n", t, msg).utf8))
}

/// Thread-safe collector for per-interval statistics.
final class Stats {
    private let lock = NSLock()
    private var captured = 0
    private var encoded = 0
    private var dropped = 0
    private var bytes = 0
    private var encodeMs: [Double] = []
    private var pipelineMs: [Double] = []
    private var packets = 0
    private var retransmits = 0
    private var sendFailures = 0
    private var nackMisses = 0

    func frameCaptured() { lock.withLock { captured += 1 } }
    func frameDropped() { lock.withLock { dropped += 1 } }
    func packetSent(retransmit: Bool) { lock.withLock { packets += 1; if retransmit { retransmits += 1 } } }
    func packetSendFailed() { lock.withLock { sendFailures += 1 } }
    func nackMissed() { lock.withLock { nackMisses += 1 } }
    func frameEncoded(bytes b: Int, encodeMs e: Double, pipelineMs p: Double) {
        lock.withLock {
            encoded += 1
            bytes += b
            encodeMs.append(e)
            pipelineMs.append(p)
        }
    }

    /// Returns a one-line summary for the elapsed interval and resets the counters.
    func drain(interval: Double) -> String {
        lock.withLock {
            defer {
                captured = 0; encoded = 0; dropped = 0; bytes = 0
                packets = 0; retransmits = 0; sendFailures = 0; nackMisses = 0
                encodeMs.removeAll(keepingCapacity: true)
                pipelineMs.removeAll(keepingCapacity: true)
            }
            func pct(_ a: [Double], _ p: Double) -> Double {
                guard !a.isEmpty else { return 0 }
                let s = a.sorted()
                return s[min(s.count - 1, Int(Double(s.count) * p))]
            }
            var line = String(format: "capture %5.1f fps | encode %5.1f fps | %6.1f Mbps | enc p50 %5.2f p95 %5.2f ms | composite->encoded p50 %5.2f p95 %5.2f ms",
                              Double(captured) / interval, Double(encoded) / interval, Double(bytes * 8) / interval / 1e6,
                              pct(encodeMs, 0.5), pct(encodeMs, 0.95), pct(pipelineMs, 0.5), pct(pipelineMs, 0.95))
            if packets > 0 || sendFailures > 0 {
                line += String(format: " | udp %d pkt/s, resent %d, send-fail %d, nack-miss %d",
                               Int(Double(packets) / interval), retransmits, sendFailures, nackMisses)
            }
            if dropped > 0 { line += " | raw-dropped \(dropped)" }
            return line
        }
    }
}
