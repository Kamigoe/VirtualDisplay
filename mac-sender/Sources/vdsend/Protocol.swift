import Foundation

/// Wire constants and helpers for docs/protocol.md (v1). All integers are big-endian.
enum Proto {
    static let version: UInt16 = 1

    enum Control: UInt8 {
        case hello = 0x01
        case start = 0x02
        case keyframeRequest = 0x03
        case ping = 0x04
        case pong = 0x05
        case replaced = 0x06
        // Input: handshake in the clear, then everything inside `secure`.
        case authHello = 0x10
        case authChallenge = 0x11
        case authConfirm = 0x12
        case authResult = 0x13
        case secure = 0x30
        case error = 0x7F
    }

    /// Messages inside `secure`.
    enum Input: UInt8 {
        case key = 0x20
        case imeToggle = 0x21
        case pointer = 0x22
        case button = 0x23
        case scroll = 0x24
        case releaseAll = 0x25
        case status = 0x40
    }

    enum AuthStatus: UInt8 {
        case ok = 0
        case failed = 1
        case noSecret = 2
        case outOfOrder = 3
    }

    static let videoMagic: UInt8 = 0x56 // 'V'
    static let keepaliveMagic: UInt8 = 0x4B // 'K'
    static let nackMagic: UInt8 = 0x4E // 'N'

    static let videoHeaderSize = 28
    static let maxDatagram = 1400
    static let payloadSize = maxDatagram - videoHeaderSize // 1372

    static let flagKeyframe: UInt8 = 1 << 0
    static let flagRetransmit: UInt8 = 1 << 1

    static let codecHEVC: UInt8 = 1
}

struct ByteWriter {
    private(set) var data = Data()
    mutating func u8(_ v: UInt8) { data.append(v) }
    mutating func u16(_ v: UInt16) { withUnsafeBytes(of: v.bigEndian) { data.append(contentsOf: $0) } }
    mutating func u32(_ v: UInt32) { withUnsafeBytes(of: v.bigEndian) { data.append(contentsOf: $0) } }
    mutating func u64(_ v: UInt64) { withUnsafeBytes(of: v.bigEndian) { data.append(contentsOf: $0) } }
    mutating func bytes(_ d: Data) { data.append(d) }
}

struct ByteReader {
    private let bytes: [UInt8]
    private var pos = 0
    init<D: Collection>(_ d: D) where D.Element == UInt8 { bytes = Array(d) }
    var remaining: Int { bytes.count - pos }

    mutating func u8() -> UInt8? {
        guard remaining >= 1 else { return nil }
        defer { pos += 1 }
        return bytes[pos]
    }
    mutating func u16() -> UInt16? { uint(2).map { UInt16($0) } }
    mutating func u32() -> UInt32? { uint(4).map { UInt32($0) } }
    mutating func u64() -> UInt64? { uint(8) }

    private mutating func uint(_ n: Int) -> UInt64? {
        guard remaining >= n else { return nil }
        var v: UInt64 = 0
        for i in 0..<n { v = v << 8 | UInt64(bytes[pos + i]) }
        pos += n
        return v
    }
}

/// `server_us` clock: mach_absolute_time in microseconds.
func machToMicros(_ t: UInt64) -> UInt64 { UInt64(machToMs(t) * 1000) }
func serverMicrosNow() -> UInt64 { machToMicros(mach_absolute_time()) }
