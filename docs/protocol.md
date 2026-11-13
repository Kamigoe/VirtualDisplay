# VirtualDisplay プロトコル v1

送信側 (`vdsend`, Mac) → 受信側 (`vdview`, Windows/Ubuntu) の片方向映像伝送。
LAN 内での利用が前提。整数はすべてビッグエンディアン。

```
vdview                                   vdsend
  | ---- TCP connect (control, 7777) ------> |
  | ---- Hello ----------------------------> |  仮想モニターをクライアントのモードに合わせる
  | <--- Start ----------------------------- |
  | ---- UDP Keepalive (1s毎) -------------> |  UDP 7777: 送信先アドレスの学習・FW 通過用
  | <=== UDP Video ========================= |
  | ---- UDP Nack (欠落時) ----------------> |  再送
  | ---- KeyframeRequest (TCP) ------------> |  再送で間に合わない時
  | ---- Ping / <--- Pong (TCP, 1s毎) ------ |  RTT・時計オフセット推定
```

- 制御 (TCP) と映像 (UDP) は同じポート番号（既定 7777）を使う。
- 視聴者は同時に 1 台。新しい TCP 接続が来ると旧セッションは破棄。
- TCP が切れたらセッション終了。仮想モニターは残す（蓋を閉じた運用で唯一の画面になるため）。

## 時刻

- `server_us`：送信側の単調時計（`mach_absolute_time` を µs 換算）。
- `client_us`：受信側の単調時計。
- 受信側は Ping/Pong から `offset = server_us - (t0 + t1) / 2` を求め、RTT 最小のサンプルを採用する。
  映像ヘッダの `capture_us` と組み合わせて「Mac で描画 → 受信側で表示」までの遅延を推定できる。

## 制御メッセージ (TCP)

フレーミング：`u32 length` (以降のバイト数) + `u8 type` + body。

### 0x01 Hello (C→S)

| 型 | 名前 | 説明 |
|---|---|---|
| u16 | version | `1` |
| u16 | width | 論理幅（pt）。`0` なら送信側の現在のモードを使う |
| u16 | height | 論理高さ（pt） |
| u8 | hidpi | 1 = 2x (Retina) |
| u32 | refresh_mhz | リフレッシュレート（mHz、例 60000） |
| u8 | chroma | 希望クロマ：`0` = 4:2:0, `1` = 4:4:4 |
| u32 | bitrate_kbps | 平均ビットレート。`0` なら送信側の既定値 |
| u8 | bit_depth | （任意・末尾）`8` / `10`。`0` または省略で送信側の既定値。4:4:4 は常に 8 |

Hello は接続中に何度送ってもよい。送信側はパイプラインを組み直し、新しい `session_id` の Start を返す
（受信側は新しいセッションの最初のキーフレームから受け直す）。

### 0x02 Start (S→C)

| 型 | 名前 | 説明 |
|---|---|---|
| u16 | version | `1` |
| u32 | session_id | 映像・Nack・Keepalive に載せる ID |
| u16 | pixel_width | 符号化解像度 |
| u16 | pixel_height | |
| u32 | refresh_mhz | |
| u8 | codec | `1` = HEVC (Annex-B) |
| u8 | chroma | `0` = 4:2:0, `1` = 4:4:4 |
| u8 | bit_depth | （任意・末尾）`8` / `10`。省略時は 8 |

### 0x03 KeyframeRequest (C→S)

body なし。送信側は次のフレームを IDR にする（静止画面なら直前のフレームを即座に再エンコード）。

### 0x04 Ping (C→S) / 0x05 Pong (S→C)

- Ping：`u64 client_us`
- Pong：`u64 client_us`（Ping の値をそのまま）+ `u64 server_us`

### 0x06 Replaced (S→C)

UTF-8 で新しい受信側の接続元（例 `192.168.11.12:51232`）。送信側は受信側を 1 台しか持たないため、
新しい接続が来ると古い受信側にこれを送ってから接続を閉じる。

受信側はこれを受け取ったら**自動で再接続しない**（2 台が互いに奪い合い、どちらも映らなくなるため）。
利用者の操作（vdview では Enter）で再接続する。このメッセージを知らない受信側は未知の type として無視し、
接続断として扱う（従来どおり）。

### 0x7F Error (S→C)

UTF-8 のメッセージ。送信後に接続を閉じる。

## 入力（キーボード・マウス）

受信側の入力を送信側（Mac）へ送る。設計の経緯は `docs/input-plan.md`。

- 両側が同じ**共有シークレット**（32 バイトの乱数）を持つ。通信には流さない。
  Mac は `~/Library/VirtualDisplay/input-secret`、PC は `%APPDATA%\vdview\input-secret`（Windows）/
  `~/.config/vdview/input-secret`（Ubuntu）。どちらも 16 進 64 文字のテキスト。
- Start の後、受信側がシークレットを持っていれば握手（0x10〜0x13）を始める。成功すると、以後の入力は
  0x30 Secure（暗号化した入れ物）の中で送る。握手が成功していない接続からの入力は受け付けない。
- 映像は暗号化しない。
- この節のメッセージを知らない相手は未知の type として無視する（古い送信側に新しい受信側をつなぐと、
  握手に返事がないので映像だけになる）。

### 握手

| type | 向き | body |
|---|---|---|
| 0x10 AuthHello | C→S | `client_nonce`(32) ‖ `client_pub`(32、X25519 の使い捨て公開鍵) |
| 0x11 AuthChallenge | S→C | `server_nonce`(32) ‖ `server_pub`(32) ‖ `server_mac`(32) |
| 0x12 AuthConfirm | C→S | `client_mac`(32) |
| 0x13 AuthResult | S→C | u8 `status`：0 = 成功、1 = 認証失敗、2 = 送信側にシークレットがない、3 = 順序違反 |

鍵の導出（`‖` は連結、文字列は ASCII、HKDF と HMAC は SHA-256）：

```
transcript = "vdinput v1" ‖ client_nonce ‖ client_pub ‖ server_nonce ‖ server_pub
th         = SHA-256(transcript)
dh         = X25519(自分の秘密鍵, 相手の公開鍵)        // 32 バイトすべて 0 なら中止
prk        = HKDF-Extract(salt = th, ikm = psk ‖ dh)
k_auth     = HKDF-Expand(prk, "vdinput auth", 32)
k_c2s      = HKDF-Expand(prk, "vdinput c2s", 32)
k_s2c      = HKDF-Expand(prk, "vdinput s2c", 32)
server_mac = HMAC(k_auth, "server" ‖ th)
client_mac = HMAC(k_auth, "client" ‖ th)
```

- 受信側は `server_mac` を確かめてから `client_mac` を返す（シークレットを知らない相手には入力を送らない）。
  送信側は `client_mac` を確かめてから入力を受け付ける。比較は定数時間。
- 試験データ：psk = `00 01 … 1f`、受信側の秘密鍵 = `20 21 … 3f`、client_nonce = `40 … 5f`、
  送信側の秘密鍵 = `60 … 7f`、server_nonce = `80 … 9f` のとき、
  `client_mac` = `7bc79217…0507cfc`（全値は `client/src/secure.rs` の `test_vector`。
  `vdsend --input-self-test` が同じ値を再現する）。

### 0x30 Secure（C→S、S→C）

- body = ChaCha20-Poly1305 の暗号文 ‖ タグ(16)。鍵は C→S が `k_c2s`、S→C が `k_s2c`。
- nonce（12 バイト）= `00 00 00 00` ‖ u64 カウンタ。向きごとに 0 から 1 ずつ増やす。
  受け手は次の番号でしか復号しないので、再送・入れ替え・削除は復号失敗になる。復号に失敗したら接続を切る。
- 追加認証データ = `30`（1 バイト）。
- 平文 = 内側の type(u8) ‖ 内側の body。

| 内側の type | 向き | body |
|---|---|---|
| 0x20 Key | C→S | u8 `down`(1/0) ‖ u16 `mac_keycode`（macOS の仮想キーコード `kVK_*`）‖ u8 `flags`（bit0 = オートリピート） |
| 0x21 ImeToggle | C→S | なし。送信側が英数 ⇔ かなを切り替える |
| 0x22 Pointer | C→S | u16 `x` ‖ u16 `y`：仮想モニター全体を 0〜65535 に正規化した位置 |
| 0x23 Button | C→S | u8 `button`（0 左、1 右、2 中、3 戻る、4 進む）‖ u8 `down` |
| 0x24 Scroll | C→S | i16 `dx` ‖ i16 `dy` ‖ u8 `unit`（0 = 1/100 行、1 = ピクセル）。正の dy は上 |
| 0x25 ReleaseAll | C→S | なし。押されているキーとボタンをすべて離す |
| 0x40 InputStatus | S→C | u8 `flags`（bit0 = 注入できる＝アクセシビリティの許可あり）。握手の直後と、変わったときに送る |

修飾キーは修飾キー自身のキーコード（Command 55 / 54、Shift 56 / 60、Option 58 / 61、Control 59 / 62）で送り、
送信側が押されている修飾キーからフラグを組み立てる。送信側は接続が切れたとき、または受信側が入れ替わったときも、
押されているものをすべて離す。

## 映像パケット (UDP, S→C)

1 フレーム（HEVC の 1 アクセスユニット、Annex-B）を固定長チャンクに分割して送る。
1 データグラム最大 1400 バイト = ヘッダ 28 バイト + ペイロード最大 1372 バイト。

| オフセット | 型 | 名前 | 説明 |
|---|---|---|---|
| 0 | u8 | magic | `0x56` ('V') |
| 1 | u8 | version | `1` |
| 2 | u8 | flags | bit0 = キーフレーム, bit1 = 再送 |
| 3 | u8 | reserved | `0` |
| 4 | u32 | session_id | |
| 8 | u32 | frame_id | フレームごとに +1（ラップアラウンドあり） |
| 12 | u16 | packet_index | 0 始まり |
| 14 | u16 | packet_count | このフレームのパケット数 |
| 16 | u32 | frame_bytes | フレーム全体のバイト数 |
| 20 | u64 | capture_us | このフレームが Mac 上で描画された時刻（server_us） |
| 28 | … | payload | `frame_bytes` の `packet_index * 1372` バイト目から |

受信側は `packet_count` 個揃ったらデコーダへ渡す。

## 受信側 → 送信側 (UDP)

### Keepalive

| 型 | 名前 |
|---|---|
| u8 | magic `0x4B` ('K') |
| u8 | version `1` |
| u16 | reserved |
| u32 | session_id |

Start 受信直後と以後 1 秒ごとに送る。送信側は最後に受け取った Keepalive の送信元アドレスへ映像を送る
（受信側のファイアウォールを戻りトラフィックとして通過させる目的も兼ねる）。

### Nack

| 型 | 名前 |
|---|---|
| u8 | magic `0x4E` ('N') |
| u8 | version `1` |
| u16 | count |
| u32 | session_id |
| u32 | frame_id |
| u16 × count | 欠落した packet_index |

送信側は直近のフレームを保持しており、該当パケットを `flags.bit1` を立てて再送する。

## 欠落時の振る舞い（受信側）

1. 新しいフレームのパケットが届いた時点、または最後のパケット受信から 3ms 経過した時点で、未完成のフレームに対して Nack を 1 回送る。
2. Nack 後 `max(2 × RTT, 10ms)` 以内に揃わなければ、そのフレームを破棄して KeyframeRequest を送り、キーフレームが届くまで後続フレームも破棄する（参照が壊れているため）。
3. 送信側は KeyframeRequest を 100ms に 1 回までに制限する。
