# 入力の統合：設計

`docs/roadmap.md` の 2.「mac-input-cast の機能を vdview / vdsend へ」の設計。2026-11-13 確定。

**状況**：フェーズ 1 は実装・試験済み（2026-11-13、`44f991c`。Windows → Mac で動作を確認。
経過は `docs/mac-handoff-input.md`）。次はフェーズ 2（Windows キーと Alt+Tab、Ubuntu での試験）。
試験で、Win+C が Windows の Copilot に、Win 単独がスタートメニューに取られることを確認した（Mac の Control の
組み合わせが使えない）。フェーズ 2 で解消する。

## 目的

`vdview` の画面を見ながら、PC のキーボードとマウスで Mac を操作できるようにする。
mac-input-cast（Python。Windows → Mac の入力キャスト）を置き換え、同じ使い勝手を保ちつつ、次の制約を解く。

| mac-input-cast の制約 | 統合後 |
|---|---|
| マウスは相対移動のみ（カーソルを画面中央に固定して差分を送る） | `vdview` の画面上の位置をそのまま Mac の座標にする（絶対座標） |
| Mac のメインディスプレイのみ | `vdsend` が仮想モニターの ID と位置を知っているので、そこへ正しく送る |
| Ubuntu 非対応 | winit のキー入力（物理キー）は Windows / Ubuntu 共通 |
| 映像と別のプロセス・別の接続、トークンが平文 | `vdview` / `vdsend` の制御用 TCP に載せ、相互認証と暗号化を行う |

## 決定事項

| | 項目 | 決定 |
|---|---|---|
| D1 | 転送の切り替え | **F8 で ON / OFF、起動時は ON**。ON の間は F1〜F12 を含むすべてのキーを Mac に送る。`vdview` 自身の操作（F1〜F5、F11、Esc、Enter）は OFF のときだけ効く |
| D2 | Windows キー・Alt+Tab | **Windows では低レベルのキーボードフックで、転送 ON かつ `vdview` が前面のときだけ横取りする**（フェーズ 2）。Ctrl+Alt+Del は不可（OS の仕様）。Wayland では不可 |
| D3 | 認証 | **共有シークレットによる相互認証**（下記） |
| D4 | 暗号化 | **入力メッセージを ChaCha20-Poly1305 で暗号化**。PC 側は RustCrypto のクレートを使う（「外部依存を増やさない」方針の例外）。Mac 側は CryptoKit。X25519 の使い捨て鍵交換で前方秘匿性も持たせる |
| D5 | マウス | **絶対座標のみ** |
| D6 | 進め方 | **フェーズに分ける**（下記） |

## フェーズ

1. 通信・認証・暗号化と、キーボード・マウスの基本。Windows で試験。
2. Windows のキーボードフック（D2）、Ubuntu での試験。
3. mac-input-cast を役目終了にする（README で案内）。

## キーボード

- **物理キーで送る**：winit の `KeyCode`（物理位置。W3C の `code` と同じ）を、PC 側で Mac の仮想キーコード（`kVK_*`）に
  変換して送る。文字ではなくキーの位置を送るので、Mac 側の JIS 配列と IME がローカルで打ったときと同じ文字を作る
  （mac-input-cast と同じ考え方）。mac-input-cast の `VK_TO_MAC` を `KeyCode` 基準に書き直して流用する。
  例：`IntlYen` → `kVK_JIS_Yen`(93)、`IntlRo` → `kVK_JIS_Underscore`(94)。
- **日本語入力の切替**（mac-input-cast を踏襲）：

  | PC のキー | Mac |
  |---|---|
  | 無変換（`NonConvert`）、Mac 用キーボードの英数（`Lang2`） | 英数（`kVK_JIS_Eisu` 102） |
  | 変換（`Convert`）、ひらがな/カタカナ（`KanaMode`）、Mac 用キーボードのかな（`Lang1`） | かな（`kVK_JIS_Kana` 104） |
  | 半角/全角（`Backquote` の位置） | 英数 ⇔ かな のトグル（`vdsend` が現在の状態を覚える） |

  PC 側の IME は使わない（winit は既定でウィンドウの IME を無効にしている。試験で確認する）。
- **修飾キー**：Ctrl → Command、Alt → Option、Win → Control、Shift → Shift（mac-input-cast と同じ既定）。
  `--map-ctrl` / `--map-alt` / `--map-win` で `cmd` / `ctrl` / `alt` / `shift` / `none` に変えられる。
  修飾キーも Mac の修飾キーのキーコード（Command 55 など）として送り、`vdsend` がそこから修飾フラグを組み立てる。
- **キーリピート**：PC の OS が出すリピートを、autorepeat の印を付けて送る。
- **押しっぱなし事故の防止**：転送を OFF にしたとき、ウィンドウがフォーカスを失ったとき、接続が切れたときは、
  PC 側が ReleaseAll を送る。`vdsend` も接続が切れたら、押されているキーとボタンをすべて離す。

## マウス

- **絶対座標**：`vdview` は映像を縦横比を保って中央に描いている。その範囲の中の位置を 0〜65535 に正規化して送り、
  `vdsend` が仮想モニターの位置と大きさ（`CGDisplayBounds`、ポイント単位）に掛けて Mac の座標にする。HiDPI でも同じ。
  映像の外（黒帯）にあるときは端に寄せる。
- ボタン（左・右・中・戻る・進む）。ボタンを押したままの移動は、`vdsend` がドラッグイベントにする。
- ホイール：ノッチ単位（`LineDelta`）とピクセル単位（タッチパッドの `PixelDelta`）の両方。
- マウス移動は、`vdview` のイベントループが一巡するごとに最新の位置を 1 回だけ送る。
- カーソル：当面は今までどおり Mac のカーソルが映像に映る。PC 側で即座にカーソルを描く件は `roadmap.md` の 3. で扱う。

## 通信

### 流れ

1. 今までどおり Hello → Start で映像が始まる。
2. `vdview` が共有シークレットを持っていれば、握手（0x10〜0x13）を始める。持っていなければ映像だけ。
3. 握手が成功すると、以後の入力は 0x30 Secure（暗号化した入れ物）で送る。

古い `vdsend` / `vdview` は未知の type を無視するので、互換性は保てる（古い `vdsend` に新しい `vdview` をつなぐと、
握手に返事が来ないので映像だけになる）。

### 共有シークレット

- 32 バイトの乱数。16 進 64 文字のテキストファイルとして置く。
  - Mac：`~/Library/VirtualDisplay/input-secret`（権限 600）。`mac-sender/scripts/make-input-secret.sh` で作り、内容を表示する。
  - PC：Windows は `%APPDATA%\vdview\input-secret`、Ubuntu は `~/.config/vdview/input-secret`。`--input-secret PATH` で変更可。
- 通信には一度も流さない。

### 握手（TLS 1.3 の PSK + (EC)DHE や Noise の NNpsk と同じ形）

| type | 向き | body |
|---|---|---|
| 0x10 AuthHello | C→S | `client_nonce`(32) ‖ `client_pub`(32、X25519 の使い捨て公開鍵) |
| 0x11 AuthChallenge | S→C | `server_nonce`(32) ‖ `server_pub`(32) ‖ `server_mac`(32) |
| 0x12 AuthConfirm | C→S | `client_mac`(32) |
| 0x13 AuthResult | S→C | u8 `status`：0 = 成功、1 = 認証失敗、2 = Mac にシークレットがない、3 = 握手の順序違反 |

鍵の導出（`‖` は連結。文字列は ASCII）：

```
transcript = "vdinput v1" ‖ client_nonce ‖ client_pub ‖ server_nonce ‖ server_pub
th         = SHA-256(transcript)
dh         = X25519(自分の秘密鍵, 相手の公開鍵)          // 32 バイトすべて 0 なら中止
prk        = HKDF-Extract(salt = th, ikm = psk ‖ dh)      // HKDF-SHA256
k_auth     = HKDF-Expand(prk, "vdinput auth", 32)
k_c2s      = HKDF-Expand(prk, "vdinput c2s", 32)
k_s2c      = HKDF-Expand(prk, "vdinput s2c", 32)
server_mac = HMAC-SHA256(k_auth, "server" ‖ th)
client_mac = HMAC-SHA256(k_auth, "client" ‖ th)
```

- `vdview` は `server_mac` を確かめてから `client_mac` を返す（**偽の Mac にはキー入力を送らない**）。
  `vdsend` は `client_mac` を確かめてから入力を受け付ける。MAC の比較は定数時間で行う。
- 失敗したら AuthResult で理由を返し、入力は受け付けない（映像は続ける）。
- 鍵は接続ごとに変わる。シークレットが後で漏れても、X25519 の秘密鍵は使い捨てなので、録られた過去の通信は解読できない。

### 0x30 Secure（暗号化した入れ物）

- body = ChaCha20-Poly1305 の暗号文 ‖ タグ(16)。鍵は向きごとに `k_c2s` / `k_s2c`。
- nonce（12 バイト）= 0 を 4 バイト ‖ u64 のカウンタ（ビッグエンディアン）。向きごとに 0 から 1 ずつ増やす。
  受け手は「次に来るはずの番号」でしか復号しないので、再送・入れ替え・削除は復号失敗になる。
- 追加認証データ（AAD）= `0x30` の 1 バイト。
- 平文 = 内側の type(u8) ‖ 内側の body。復号に失敗したら接続を切る。

### 内側のメッセージ

| type | 向き | body |
|---|---|---|
| 0x20 Key | C→S | u8 `down`(1/0) ‖ u16 `mac_keycode` ‖ u8 `flags`（bit0 = autorepeat） |
| 0x21 ImeToggle | C→S | なし。`vdsend` が英数 ⇔ かなを切り替える（押下時のみ送る） |
| 0x22 Pointer | C→S | u16 `x` ‖ u16 `y`（映像の範囲を 0〜65535 に正規化） |
| 0x23 Button | C→S | u8 `button`（0 左、1 右、2 中、3 戻る、4 進む）‖ u8 `down` |
| 0x24 Scroll | C→S | i16 `dx` ‖ i16 `dy` ‖ u8 `unit`（0 = 1/100 行、1 = ピクセル）。正の dy は上方向 |
| 0x25 ReleaseAll | C→S | なし |
| 0x40 InputStatus | S→C | u8 `flags`（bit0 = 注入できる＝アクセシビリティ許可あり）。握手の成功直後と、状態が変わったときに送る |

## Mac 側（vdsend）

- 注入は `CGEventPost(.cghidEventTap, …)`（Apple 純正）。キーは `CGEvent(keyboardEventSource:virtualKey:keyDown:)` に
  修飾フラグを付けて、マウスは `CGEvent(mouseEventSource:mouseType:mouseCursorPosition:mouseButton:)`。
- 権限：「アクセシビリティ」（イベントの送信）。`CGPreflightPostEventAccess()` で確認し、`--request-permission` のとき
  `CGRequestPostEventAccess()` で許可を求める（画面収録と同じ流れ）。署名が固定なので、再ビルドしても許可は外れないはず。
- 入力を受け付けるのは、今の受信側の接続で、かつ握手が成功したものだけ。

## PC 側（vdview）

- 暗号：`chacha20poly1305`、`hkdf`、`hmac`、`sha2`、`x25519-dalek`（RustCrypto ほか。すべて Rust のみ）、
  乱数は `getrandom`（OS の乱数）。
- F8 の状態はウィンドウタイトルと状態表示に出す（`INPUT ON` / `INPUT OFF`、認証できていなければ理由）。

## 試験の観点

- 握手：Rust と Swift で鍵の導出が一致すること（固定の値で計算した試験データを両側で照合する）
- 認証：シークレットが違う / ない場合に入力が拒否され、映像は映ること
- JIS 配列の全キー（記号、¥、ろ、テンキー）、Shift との組み合わせ、英数 / かな / 半角全角、Mac での IME 変換
- 修飾キーの組み合わせ（Cmd+C / V、Cmd+Tab、ターミナルでの Ctrl+C）、キーリピート
- マウス：クリック、ダブルクリック、ドラッグ、右クリック、ホイール、HiDPI と等倍の両方で位置がずれないこと
- F8 の切り替え、フォーカスを外したときにキーが押しっぱなしにならないこと、接続断・`Replaced` のとき
