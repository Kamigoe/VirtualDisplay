import CryptoKit
import Foundation

/// Authentication and encryption of the input channel (docs/protocol.md, "Input"). Must derive
/// exactly what vdview's `secure.rs` derives; `vdsend --input-self-test` checks the shared vector.
enum InputCrypto {
    static let secretLength = 32
    private static let label = Data("vdinput v1".utf8)

    /// `~/Library/VirtualDisplay/input-secret`: 64 hex digits, made by scripts/make-input-secret.sh.
    static var secretURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/VirtualDisplay/input-secret")
    }

    /// Read on every handshake, so a new secret takes effect without restarting vdsend.
    static func loadSecret() -> Data? {
        guard let text = try? String(contentsOf: secretURL, encoding: .utf8) else { return nil }
        let hex = text.filter { !$0.isWhitespace }
        guard hex.count == secretLength * 2 else { return nil }
        var out = Data(capacity: secretLength)
        var i = hex.startIndex
        while i < hex.endIndex {
            let j = hex.index(i, offsetBy: 2)
            guard let b = UInt8(hex[i..<j], radix: 16) else { return nil }
            out.append(b)
            i = j
        }
        return out.allSatisfy({ $0 == 0 }) ? nil : out
    }

    static func randomBytes(_ n: Int) -> Data {
        SymmetricKey(size: SymmetricKeySize(bitCount: n * 8)).withUnsafeBytes { Data($0) }
    }

    struct Derived {
        let kC2S: Data
        let kS2C: Data
        let serverMac: Data
        let clientMac: Data
    }

    /// The key schedule of docs/protocol.md. `dh` is the X25519 result, already checked to be non-zero.
    static func derive(psk: Data, dh: Data, clientNonce: Data, clientPub: Data,
                       serverNonce: Data, serverPub: Data) -> Derived {
        var h = SHA256()
        for part in [label, clientNonce, clientPub, serverNonce, serverPub] { h.update(data: part) }
        let th = Data(h.finalize())
        // HKDF-Extract(salt = th, ikm = psk ‖ dh), then Expand per label (deriveKey does both).
        let ikm = SymmetricKey(data: psk + dh)
        func expand(_ info: String) -> SymmetricKey {
            HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: th, info: Data(info.utf8), outputByteCount: 32)
        }
        let kAuth = expand("vdinput auth")
        func mac(_ role: String) -> Data {
            Data(HMAC<SHA256>.authenticationCode(for: Data(role.utf8) + th, using: kAuth))
        }
        func raw(_ k: SymmetricKey) -> Data { k.withUnsafeBytes { Data($0) } }
        return Derived(kC2S: raw(expand("vdinput c2s")), kS2C: raw(expand("vdinput s2c")),
                       serverMac: mac("server"), clientMac: mac("client"))
    }

    enum Failure: Error { case malformed, badKey }

    /// The Mac's half of the handshake, between AuthChallenge and AuthConfirm.
    struct Pending {
        let derived: Derived

        /// Answers AuthHello (client_nonce ‖ client_pub): returns the state to keep and the
        /// AuthChallenge body (server_nonce ‖ server_pub ‖ server_mac).
        static func respond(psk: Data, hello: Data,
                            privateKey: Curve25519.KeyAgreement.PrivateKey = .init(),
                            nonce: Data = InputCrypto.randomBytes(32)) throws -> (Pending, Data) {
            guard hello.count == 64 else { throw Failure.malformed }
            let hello = Data(hello) // re-index from 0
            let clientNonce = hello.subdata(in: 0..<32)
            let clientPub = hello.subdata(in: 32..<64)
            let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: clientPub)
            let shared = try privateKey.sharedSecretFromKeyAgreement(with: peer)
            let dh = shared.withUnsafeBytes { Data($0) }
            guard dh.count == 32, !dh.allSatisfy({ $0 == 0 }) else { throw Failure.badKey }
            let serverPub = privateKey.publicKey.rawRepresentation
            let d = derive(psk: psk, dh: dh, clientNonce: clientNonce, clientPub: clientPub,
                           serverNonce: nonce, serverPub: serverPub)
            return (Pending(derived: d), nonce + serverPub + d.serverMac)
        }

        /// Checks AuthConfirm; on success returns the channel.
        func confirm(_ body: Data) -> (opener: Opener, sealer: Sealer)? {
            guard constantTimeEqual(body, derived.clientMac) else { return nil }
            return (Opener(key: derived.kC2S), Sealer(key: derived.kS2C))
        }
    }

    static func constantTimeEqual(_ a: Data, _ b: Data) -> Bool {
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for (x, y) in zip(a, b) { diff |= x ^ y }
        return diff == 0
    }

    private static let aad = Data([Proto.Control.secure.rawValue])

    private static func nonce(_ counter: UInt64) -> ChaChaPoly.Nonce {
        var n = Data(count: 4)
        withUnsafeBytes(of: counter.bigEndian) { n.append(contentsOf: $0) }
        return try! ChaChaPoly.Nonce(data: n) // 12 bytes is always valid
    }

    /// Encrypts outgoing Secure messages; the counter makes each nonce unique.
    struct Sealer {
        let key: SymmetricKey
        private var counter: UInt64 = 0
        init(key: Data) { self.key = SymmetricKey(data: key) }

        mutating func seal(_ inner: UInt8, _ body: Data = Data()) -> Data {
            let box = try! ChaChaPoly.seal(Data([inner]) + body, using: key,
                                           nonce: InputCrypto.nonce(counter), authenticating: InputCrypto.aad)
            counter += 1
            return box.ciphertext + box.tag
        }
    }

    /// Decrypts incoming Secure messages, accepting only the next counter value: replayed,
    /// reordered, dropped or altered messages fail.
    struct Opener {
        let key: SymmetricKey
        private var counter: UInt64 = 0
        init(key: Data) { self.key = SymmetricKey(data: key) }

        mutating func open(_ body: Data) -> (UInt8, Data)? {
            let body = Data(body)
            guard body.count >= 17,
                  let box = try? ChaChaPoly.SealedBox(nonce: InputCrypto.nonce(counter),
                                                      ciphertext: body.prefix(body.count - 16),
                                                      tag: body.suffix(16)),
                  let plain = try? ChaChaPoly.open(box, using: key, authenticating: InputCrypto.aad)
            else { return nil }
            counter += 1
            return (plain[plain.startIndex], Data(plain.dropFirst()))
        }
    }

    // MARK: - self-test

    /// Recomputes the fixed handshake of vdview's `secure::tests::test_vector` (also checked by an
    /// independent implementation) and compares every value. Returns true when all match.
    static func selfTest() -> Bool {
        func seq(_ start: UInt8) -> Data { Data((0..<32).map { start &+ UInt8($0) }) }
        func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
        do {
            let psk = seq(0x00)
            let clientKey = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: seq(0x20))
            let clientPub = clientKey.publicKey.rawRepresentation
            let hello = seq(0x40) + clientPub
            let serverKey = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: seq(0x60))
            let (pending, challenge) = try Pending.respond(psk: psk, hello: hello,
                                                           privateKey: serverKey, nonce: seq(0x80))
            let d = pending.derived
            guard var channel = pending.confirm(d.clientMac) else { return false }
            var c2s = Sealer(key: d.kC2S)
            let key = c2s.seal(0x20, Data([1, 0x00, 0x24, 0]))
            let releaseAll = c2s.seal(0x25)
            let status = channel.sealer.seal(0x40, Data([1]))
            let opened = channel.opener.open(key)
            let got: [(String, String, String)] = [
                ("client_pub", hex(clientPub), "358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254"),
                ("server_pub", hex(challenge.subdata(in: 32..<64)), "675dd574ed7789310b3d2e7681f3790b466c773b1521fecf36577958371ea52f"),
                ("k_c2s", hex(d.kC2S), "ce7ecedd7ef702e0b5a9d6aa0f30066aa6b5fb948e6d13d1289fd686e6d91ac0"),
                ("k_s2c", hex(d.kS2C), "0549cf71f5321d0bb9d957428e7f522ee48f3a01ae3c43c8e32ba9707175b96f"),
                ("server_mac", hex(d.serverMac), "23810b04a67ae6e130814326a9a47173d4c60f0df4409981727cae7677371e6a"),
                ("client_mac", hex(d.clientMac), "7bc792170d1d1fb3e7f2a863a930544daf9d8259b25ffdf0b482f5f3d0507cfc"),
                ("c2s#0 Key(down, 36, 0)", hex(key), "07787f14a151bf627fbf83f4cd31762ff22d8a3ee8"),
                ("c2s#1 ReleaseAll", hex(releaseAll), "edb6558bcd9759ee1dbf2416922320d0a1"),
                ("s2c#0 InputStatus(1)", hex(status), "1249c707259801ce203b2e7ddf31108c1118"),
                ("decrypt c2s#0", opened.map { "\($0.0) \(hex($0.1))" } ?? "failed", "32 01002400"),
                ("replay c2s#0", channel.opener.open(key) == nil ? "rejected" : "accepted", "rejected"),
                ("wrong client_mac", pending.confirm(d.serverMac) == nil ? "rejected" : "accepted", "rejected"),
            ]
            var ok = true
            for (name, value, want) in got {
                let match = value == want
                ok = ok && match
                print("\(match ? "ok  " : "FAIL") \(name): \(value)\(match ? "" : " (expected \(want))")")
            }
            return ok
        } catch {
            print("FAIL: \(error)")
            return false
        }
    }
}
