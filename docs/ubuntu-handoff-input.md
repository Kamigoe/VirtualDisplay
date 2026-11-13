# Ubuntu 側の作業手順：入力の統合（Ubuntu 対応）

Windows 側のセッションから、Mac 側のセッション（SSH で Ubuntu の PC に入って作業する）への引き継ぎメモ（2026-11-13）。
作業が終わったら、このファイルの「結果」に記入してコミット・push すること。

## 背景

`vdview` のキーボード・マウスを Mac に送る機能（`docs/input-plan.md`）は、Windows で動作を確認済み：

- フェーズ 1（`44f991c`）：共有シークレットによる相互認証、入力の暗号化、キー（位置で送る。JIS・IME 切替）、
  絶対位置のマウス、F8 で転送の ON / OFF。
- フェーズ 2 の Windows 分（`6cd1d08`）：Win キー・Alt+Tab・Alt+F4 などは OS が先に処理してしまうため、
  `client/src/winhook.rs` の低レベルキーボードフックで、転送 ON かつ `vdview` が前面のときだけ横取りして Mac へ送る。
- フェーズ 3：mac-input-cast は役目を終えた（README で案内）。

キーボード・マウスの処理（`client/src/input.rs`、`control.rs`、`secure.rs`、`main.rs`）は winit の物理キーを使う
共通コードなので、**Ubuntu でもそのまま動くはず**。ただし Ubuntu では一度も試していない。
`winhook.rs` は Windows 専用で、Ubuntu には同じ仕組みがまだない（GNOME が Super キーや Alt+Tab を先に処理するはず）。

## 手順

### A. 準備（SSH で実施）

1. 最新を取得してビルドする（Ubuntu のリポジトリは `~/VirtualDisplay`）：
   ```sh
   cd ~/VirtualDisplay && git pull && git log --oneline -3
   cd client && cargo build --release 2>&1 | tail -3 && cargo test --release 2>&1 | grep "test result"
   ```
   新しいクレート（暗号）は Rust のみなので、追加の apt パッケージは要らないはず。
2. 共有シークレットを Ubuntu に入れる。**Mac のファイルから SSH の標準入力で流し込む**
   （画面・ログ・シェルの履歴に値を出さない）。Mac の端末で：
   ```sh
   ssh <Ubuntu> '~/VirtualDisplay/client/target/release/vdview --set-input-secret' < ~/Library/VirtualDisplay/input-secret
   ```
   `saved /home/<user>/.config/vdview/input-secret` と出ること。
3. Ubuntu のセッションの種類を記録する（Wayland か X11 か。キーの横取りの方法が変わる）：
   ```sh
   loginctl show-session $(loginctl list-sessions --no-legend | awk '$3=="'"$USER"'" {print $1; exit}') -p Type
   gnome-shell --version; lsb_release -ds
   ```

### B. 試験（利用者が Ubuntu の画面で操作する）

利用者にアプリ一覧の「vdview」（`vdview-mac`）で起動してもらう。ログは `~/.cache/vdview.log`。
起動後、ログに `input to the Mac: on (F8 turns it off)` が出ていれば認証は成功。

利用者に次を試してもらい、結果（○ / × と、× のときに何が起きたか）を「結果」に書く：

1. 文字：英字・数字・記号（`- ^ ¥ @ [ ; : ] , . / _`）、Shift との組み合わせ、テンキー
2. 日本語入力：無変換 → 英数、変換・ひらがな → かな、半角/全角 → 切り替え、Mac での変換
   （Ubuntu 側の IME（Mozc など）が半角/全角を先に取らないか）
3. 修飾キー：Ctrl+C / V（Mac の Cmd）、Ctrl+Tab（Cmd+Tab）、キーリピート
4. マウス：クリック、ダブルクリック、ドラッグ、右クリック、ホイールの向きと量、位置のずれ
   （Ubuntu の画面の拡大率が 100% 以外なら、その値も記録）
5. F8 で OFF / ON、ウィンドウを切り替えたときにキーが押しっぱなしにならないか
6. **OS が先に取るキー**（Windows で横取りが必要だったもの）：Super 単独、Super+C（Mac の Control+C）、
   Alt+Tab、Alt+F4、Alt+Space、Ctrl+Alt+矢印など。それぞれ Ubuntu 側で何が起きたか

### C. 6 で OS に取られたキーがあれば、横取りを実装する

Windows の `winhook.rs` と同じ振る舞いにする：

- 横取りするのは **転送 ON かつ `vdview` が前面（フォーカスあり）** のときだけ。OFF にしたら・フォーカスを失ったら、すぐに OS に返す。
- 押下を横取りしたキーは離すときも横取りし、その逆も同じ（片方だけ届いてキーが押しっぱなしにならないように）。
- `--map-win none` のように `none` にしたキーは横取りしない。
- 通信プロトコルと鍵の導出（`docs/protocol.md`）は変えない。依存は Rust のクレートに限る（大きな C ライブラリを足さない）。

方法の候補（A-3 のセッションの種類で選ぶ）：

- **Wayland（GNOME）**：`zwp_keyboard_shortcuts_inhibit_manager_v1`（keyboard-shortcuts-inhibit プロトコル）で、
  `vdview` のサーフェスがフォーカスを持つ間だけ、コンポジターのショートカットを止められる。GNOME は初回に
  「ショートカットを止めることを許可するか」を確認する。winit 0.30 はこれを公開していないので、
  `raw-window-handle` で winit の `wl_display` / `wl_surface` を取り、`wayland-client` + `wayland-protocols`
  （`wayland-backend` の `client_system` で既存の接続を使う）で直接要求する。転送 OFF にしたら inhibitor を破棄する。
- **X11**：`XGrabKeyboard` で、転送 ON かつフォーカスがある間だけキーボードを掴む（OFF・フォーカス喪失で `XUngrabKeyboard`）。
- どちらの場合も、Windows と同じく「横取りできないもの」（Ctrl+Alt+F キーの仮想端末切替など）は README に書く。

Windows で見つかった落とし穴（参考）：winit が Raw Input を登録すると Windows のキーボードフックが呼ばれなくなった。
Ubuntu で横取りが効かないときは、まず「横取りの仕組みが本当に働いているか」をログで確かめること。

実装したら、B の 6 をもう一度試してもらい、README（「入力（キーボード・マウス）」の「まだできないこと」）と
`docs/input-plan.md` の状況を更新する。Windows でのビルドが壊れないよう、Linux 専用のコードは `#[cfg(target_os = "linux")]` で囲む。

## 結果

（Mac 側のセッションが記入する）
