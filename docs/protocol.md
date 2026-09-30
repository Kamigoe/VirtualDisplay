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

### 0x03 KeyframeRequest (C→S)

body なし。送信側は次のフレームを IDR にする（静止画面なら直前のフレームを即座に再エンコード）。

### 0x04 Ping (C→S) / 0x05 Pong (S→C)

- Ping：`u64 client_us`
- Pong：`u64 client_us`（Ping の値をそのまま）+ `u64 server_us`

### 0x7F Error (S→C)

UTF-8 のメッセージ。送信後に接続を閉じる。

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
