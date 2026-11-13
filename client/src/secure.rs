//! Authentication and encryption of the input channel (docs/protocol.md, "Input").
//!
//! Both ends hold the same 32-byte secret. Each connection runs an X25519 exchange mixed with the
//! secret (like TLS 1.3 PSK + ECDHE): the Mac proves it knows the secret before the viewer sends
//! any input, the viewer proves it before the Mac accepts any, and the per-connection keys protect
//! input messages with ChaCha20-Poly1305. A leaked secret does not expose recorded sessions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::protocol::MSG_SECURE;

pub const SECRET_LEN: usize = 32;
const LABEL: &[u8] = b"vdinput v1";

/// Where the secret lives unless `--input-secret` says otherwise.
pub fn default_secret_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        PathBuf::from(x)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(dir.join("vdview").join("input-secret"))
}

/// Parses the secret file: 64 hex digits (whitespace ignored).
pub fn parse_secret(text: &str) -> Result<[u8; SECRET_LEN]> {
    let hex: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if hex.len() != SECRET_LEN * 2 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("the input secret must be {} hex digits", SECRET_LEN * 2);
    }
    let mut out = [0u8; SECRET_LEN];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    if out.iter().all(|&b| b == 0) {
        bail!("the input secret is all zeros");
    }
    Ok(out)
}

pub fn load_secret(path: &Path) -> Result<[u8; SECRET_LEN]> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_secret(&text).with_context(|| format!("in {}", path.display()))
}

/// Writes the secret file (creating its folder); on Unix only the owner may read it.
pub fn save_secret(path: &Path, secret: &[u8; SECRET_LEN]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, hex + "\n").with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random number generator");
    b
}

/// Everything both ends derive from one handshake.
pub struct Derived {
    pub k_c2s: [u8; 32],
    pub k_s2c: [u8; 32],
    pub server_mac: [u8; 32],
    pub client_mac: [u8; 32],
}

/// The key schedule of docs/protocol.md. `dh` is the X25519 result, already checked to be non-zero.
pub fn derive(
    psk: &[u8; 32],
    dh: &[u8; 32],
    client_nonce: &[u8; 32],
    client_pub: &[u8; 32],
    server_nonce: &[u8; 32],
    server_pub: &[u8; 32],
) -> Derived {
    let th: [u8; 32] = Sha256::new()
        .chain_update(LABEL)
        .chain_update(client_nonce)
        .chain_update(client_pub)
        .chain_update(server_nonce)
        .chain_update(server_pub)
        .finalize()
        .into();
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(psk);
    ikm[32..].copy_from_slice(dh);
    let (_, hk) = Hkdf::<Sha256>::extract(Some(&th), &ikm);
    let expand = |info: &[u8]| {
        let mut k = [0u8; 32];
        hk.expand(info, &mut k)
            .expect("32 bytes is a valid HKDF length");
        k
    };
    let k_auth = expand(b"vdinput auth");
    let mac = |role: &[u8]| -> [u8; 32] {
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&k_auth).expect("any key length");
        m.update(role);
        m.update(&th);
        m.finalize().into_bytes().into()
    };
    Derived {
        k_c2s: expand(b"vdinput c2s"),
        k_s2c: expand(b"vdinput s2c"),
        server_mac: mac(b"server"),
        client_mac: mac(b"client"),
    }
}

/// The viewer's half of the handshake, from AuthHello until AuthChallenge arrives.
pub struct ClientHandshake {
    psk: [u8; 32],
    secret: StaticSecret,
    nonce: [u8; 32],
    public: [u8; 32],
}

impl ClientHandshake {
    pub fn new(psk: [u8; 32]) -> Self {
        Self::with(psk, random_bytes(), random_bytes())
    }

    fn with(psk: [u8; 32], private: [u8; 32], nonce: [u8; 32]) -> Self {
        let secret = StaticSecret::from(private);
        let public = PublicKey::from(&secret).to_bytes();
        ClientHandshake {
            psk,
            secret,
            nonce,
            public,
        }
    }

    /// Body of AuthHello: client_nonce ‖ client_pub.
    pub fn hello(&self) -> Vec<u8> {
        [self.nonce, self.public].concat()
    }

    /// Checks AuthChallenge (server_nonce ‖ server_pub ‖ server_mac). On success returns the
    /// AuthConfirm body and the channel; on failure the Mac does not know the secret.
    pub fn challenge(self, body: &[u8]) -> Result<([u8; 32], Channel)> {
        let body: &[u8; 96] = body
            .try_into()
            .map_err(|_| anyhow!("AuthChallenge has {} bytes, expected 96", body.len()))?;
        let server_nonce: [u8; 32] = body[..32].try_into().unwrap();
        let server_pub: [u8; 32] = body[32..64].try_into().unwrap();
        let shared = self.secret.diffie_hellman(&PublicKey::from(server_pub));
        if !shared.was_contributory() {
            bail!("the Mac sent an invalid key");
        }
        let d = derive(
            &self.psk,
            shared.as_bytes(),
            &self.nonce,
            &self.public,
            &server_nonce,
            &server_pub,
        );
        if !ct_eq(&d.server_mac, &body[64..]) {
            bail!("the Mac could not prove it has the same input secret");
        }
        Ok((
            d.client_mac,
            Channel {
                tx: Sealer::new(&d.k_c2s),
                rx: Opener::new(&d.k_s2c),
            },
        ))
    }
}

/// Constant-time comparison of two MACs.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Channel {
    pub tx: Sealer,
    pub rx: Opener,
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    n
}

/// Encrypts outgoing input messages; the counter makes each nonce unique.
pub struct Sealer {
    aead: ChaCha20Poly1305,
    counter: u64,
}

impl Sealer {
    pub fn new(key: &[u8; 32]) -> Self {
        Sealer {
            aead: ChaCha20Poly1305::new_from_slice(key).expect("32-byte key"),
            counter: 0,
        }
    }

    /// Body of a Secure message carrying `inner` ‖ `body`.
    pub fn seal(&mut self, inner: u8, body: &[u8]) -> Vec<u8> {
        let mut plain = Vec::with_capacity(1 + body.len());
        plain.push(inner);
        plain.extend_from_slice(body);
        let n = nonce(self.counter);
        self.counter += 1;
        self.aead
            .encrypt(
                (&n).into(),
                Payload {
                    msg: &plain,
                    aad: &[MSG_SECURE],
                },
            )
            .expect("ChaCha20-Poly1305 encryption cannot fail for small messages")
    }
}

/// Decrypts incoming Secure messages, accepting only the next counter value.
pub struct Opener {
    aead: ChaCha20Poly1305,
    counter: u64,
}

impl Opener {
    pub fn new(key: &[u8; 32]) -> Self {
        Opener {
            aead: ChaCha20Poly1305::new_from_slice(key).expect("32-byte key"),
            counter: 0,
        }
    }

    /// Returns (inner type, inner body).
    pub fn open(&mut self, body: &[u8]) -> Result<(u8, Vec<u8>)> {
        let n = nonce(self.counter);
        let mut plain = self
            .aead
            .decrypt(
                (&n).into(),
                Payload {
                    msg: body,
                    aad: &[MSG_SECURE],
                },
            )
            .map_err(|_| anyhow!("an encrypted input message failed to authenticate"))?;
        self.counter += 1;
        if plain.is_empty() {
            bail!("empty encrypted input message");
        }
        let inner = plain.remove(0);
        Ok((inner, plain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn seq(start: u8) -> [u8; 32] {
        std::array::from_fn(|i| start.wrapping_add(i as u8))
    }

    /// Plays the Mac's side with fixed keys, for the test vector.
    fn server(
        psk: &[u8; 32],
        hello: &[u8],
        private: [u8; 32],
        nonce: [u8; 32],
    ) -> (Vec<u8>, Derived) {
        let secret = StaticSecret::from(private);
        let public = PublicKey::from(&secret).to_bytes();
        let client_nonce: [u8; 32] = hello[..32].try_into().unwrap();
        let client_pub: [u8; 32] = hello[32..].try_into().unwrap();
        let dh = secret.diffie_hellman(&PublicKey::from(client_pub));
        let d = derive(
            psk,
            dh.as_bytes(),
            &client_nonce,
            &client_pub,
            &nonce,
            &public,
        );
        ([&nonce[..], &public, &d.server_mac].concat(), d)
    }

    /// The fixed handshake that `vdsend --input-self-test` must reproduce byte for byte.
    #[test]
    fn test_vector() {
        let psk = seq(0x00);
        let client = ClientHandshake::with(psk, seq(0x20), seq(0x40));
        let hello = client.hello();
        let (challenge, d) = server(&psk, &hello, seq(0x60), seq(0x80));
        let (confirm, mut ch) = client.challenge(&challenge).unwrap();
        assert_eq!(confirm, d.client_mac);
        let sealed = ch.tx.seal(0x20, &[1, 0x00, 0x24, 0]);
        let sealed2 = ch.tx.seal(0x25, &[]);
        let mut s2c = Sealer::new(&d.k_s2c);
        let status = s2c.seal(0x40, &[1]);
        assert_eq!(ch.rx.open(&status).unwrap(), (0x40, vec![1]));

        let got = [
            ("client_pub", hex(&hello[32..])),
            ("server_pub", hex(&challenge[32..64])),
            ("k_c2s", hex(&d.k_c2s)),
            ("k_s2c", hex(&d.k_s2c)),
            ("server_mac", hex(&d.server_mac)),
            ("client_mac", hex(&d.client_mac)),
            ("c2s#0 Key(down, 36, 0)", hex(&sealed)),
            ("c2s#1 ReleaseAll", hex(&sealed2)),
            ("s2c#0 InputStatus(1)", hex(&status)),
        ];
        for (name, value) in &got {
            println!("{name}: {value}");
        }
        // Also reproduced by an independent pure-Python implementation (RFC 7748 / 8439 / 5869).
        let expected = [
            "358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254",
            "675dd574ed7789310b3d2e7681f3790b466c773b1521fecf36577958371ea52f",
            "ce7ecedd7ef702e0b5a9d6aa0f30066aa6b5fb948e6d13d1289fd686e6d91ac0",
            "0549cf71f5321d0bb9d957428e7f522ee48f3a01ae3c43c8e32ba9707175b96f",
            "23810b04a67ae6e130814326a9a47173d4c60f0df4409981727cae7677371e6a",
            "7bc792170d1d1fb3e7f2a863a930544daf9d8259b25ffdf0b482f5f3d0507cfc",
            "07787f14a151bf627fbf83f4cd31762ff22d8a3ee8",
            "edb6558bcd9759ee1dbf2416922320d0a1",
            "1249c707259801ce203b2e7ddf31108c1118",
        ];
        for ((name, value), want) in got.iter().zip(expected) {
            assert_eq!(value, want, "{name}");
        }
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let client = ClientHandshake::with(seq(0), seq(0x20), seq(0x40));
        let (challenge, _) = server(&seq(1), &client.hello(), seq(0x60), seq(0x80));
        assert!(client.challenge(&challenge).is_err());
    }

    #[test]
    fn replay_and_reorder_fail() {
        let key = seq(7);
        let mut tx = Sealer::new(&key);
        let mut rx = Opener::new(&key);
        let a = tx.seal(0x22, &[0, 1, 0, 2]);
        let b = tx.seal(0x22, &[0, 3, 0, 4]);
        assert!(rx.open(&b).is_err());
        assert_eq!(rx.open(&a).unwrap().0, 0x22);
        assert!(rx.open(&a).is_err());
        let mut tampered = b.clone();
        tampered[0] ^= 1;
        assert!(rx.open(&tampered).is_err());
        assert!(rx.open(&b).is_ok());
    }

    #[test]
    fn secret_file_format() {
        let s = "00112233445566778899aabbccddeeff\n00112233445566778899AABBCCDDEEFF\n";
        assert_eq!(parse_secret(s).unwrap()[17], 0x11);
        assert!(parse_secret("0011").is_err());
        assert!(parse_secret(&"0".repeat(64)).is_err());
    }
}
