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

2026-11-16、Mac 側のセッションが記入。

### 前置き：Ubuntu の画面が映らなくなっていた（解決済み）

作業の前に、Ubuntu が読み込み画面から先へ進まなくなっていた（利用者は Windows 側の時刻設定の変更を疑っていた）。
SSH で調べると、Ubuntu 自体は起動していて、画面を映す部分だけが動いていなかった。

- 原因：2026-11-11 の自動更新で入った `linux-firmware-amd-graphics` 20240318.git3b128b60-0ubuntu3.4。
  RX 9070 XT（DCN 4.0.1）用の `amdgpu/dcn_4_0_1_dmcub.bin.zst` が、DCN 3.2.0 用のファイルへのリンクに変わっていた。
  GPU がこれを受け付けず（`LOAD_IP_FW failed`、`Error queueing DMUB command` が 1 起動で 1000 回以上）、
  モニターは信号なし・読み込み画面のまま。カーネルを戻しても同じ（ファームウェアは共有）。11/10 の起動が最後の正常。
- 対処：snapshot.ubuntu.com（20261015T000000Z）から 0ubuntu3.3 を取得し、署名付き索引の SHA256 と照合して入れ直し、
  `apt-mark hold linux-firmware-amd-graphics`。DMUB 0x0A000800 が読み込まれ、エラー 0 件、DP・HDMI とも表示。
  .deb は Ubuntu の `~/fw-rollback/` に残してある。**直った版（`dcn_4_0_1_dmcub` が実ファイル）が出たら `apt-mark unhold` する。**
  画面が映らないときの予備手段：GRUB で `e` → `nomodeset` を足して起動。
- Windows の時刻設定は無関係（むしろ、Ubuntu の起動直後に時計が 9 時間ずれる症状が消えた）。
- この日、Ubuntu は DHCP で 192.168.11.12 になり、192.168.11.7 は別の機器が使っていた。

### A. 準備

1. `d50caa4` を取得。ビルド成功、テスト 8 件成功。新しい apt パッケージは不要だった。
2. シークレットを Mac から SSH の標準入力で投入（`~/.config/vdview/input-secret`、600）。
3. Wayland、GNOME Shell 46.0、Ubuntu 24.04.5 LTS。

### B. 試験（利用者が実施）

ログに `input to the Mac: on (F8 turns it off)` が出て、認証は成功。

| | 項目 | 結果 |
|---|---|---|
| 1 | 文字・記号・Shift・テンキー | ○ |
| 2 | 日本語入力の切り替え・Mac での変換 | ○ |
| 3 | Ctrl+C / V、Ctrl+Tab、キーリピート | ○ |
| 4 | マウス（クリック・ドラッグ・右クリック・ホイール・位置） | ○ |
| 5 | F8 で OFF / ON、ウィンドウ切り替え時の押しっぱなし | ○ |
| 6 | Super 単独、Super+C、Alt+Tab、Alt+F4、Alt+Space、Ctrl+Alt+矢印 | × すべて GNOME の機能が働いた |

### C. 横取りの実装（Wayland）

`client/src/wlinhibit.rs` を追加した：

- keyboard-shortcuts-inhibit（`zwp_keyboard_shortcuts_inhibit_manager_v1`）で、**転送 ON の間だけ** inhibitor を持つ。
  F8 で OFF にしたら破棄する。フォーカスがある間だけ効くのは GNOME 側の仕様なので、別のウィンドウが前面なら GNOME が処理する。
- 止めている間もキーは普通のキー入力として `vdview` に届くので、Mac にはいつもの経路（`forward_key`）で送られる。
  押下と解放が別々の側に届くこともない。転送を止めたときは、今までどおり Mac 側ですべてのキーを離す。
- 止めるか止めないかの全か無かなので、`--map-win none` か `--map-alt none` のときは使わない（ショートカットは GNOME に残す）。
- winit 0.30 はこのプロトコルを公開していないので、`raw-window-handle` で winit の `wl_display` / `wl_surface` を取り、
  `wayland-client`（`wayland-backend` の `client_system`）＋ `wayland-protocols`（`unstable`）で直接要求する。
  クレートはすべて winit が既に使っているもので、Cargo.lock に増えたのは `vdview` の依存の 3 行だけ。
  `Linux` 以外は `#[cfg(target_os = "linux")]` で外してある。終了時は winit が接続を閉じる前（`exiting`）に破棄する。
- ウィンドウの app id（X11 では WM_CLASS）を `vdview` にし、`client/linux/vdview-mac.desktop` に `StartupWMClass=vdview` を足した。
  GNOME がウィンドウとランチャーを結び付け、許可を `vdview-mac.desktop: GRANTED` として記憶する（確認済み）。
- ログ：`desktop shortcuts: go to the Mac (Super+Esc gives them back)` / `desktop shortcuts: back to the desktop`。

再試験（利用者が実施）：初回の許可ダイアログで「許可」→ 6 の Super 単独、Super+C、Alt+Tab、Alt+F4、Alt+Space、
Ctrl+Alt+矢印はすべて Mac に届いた（○）。F8 で OFF にすると GNOME に戻り、別のウィンドウを前面にしても GNOME に戻る（○）。
Super+Esc（GNOME の非常口）で、`vdview` が前面・転送 ON のままショートカットだけ GNOME に戻ることを確認（○）。
戻したあと F8 を 2 回押すと、再び Mac に送られる（ログで `back to the desktop` → F8 OFF → ON → `go to the Mac` を確認）。Ctrl+Alt+F キー（仮想端末の切り替え）は試していない
（GNOME はこれを止めない設計）。X11 のセッションは試していない（横取りしない）。

README（「入力（キーボード・マウス）」と `--map-*`）と `docs/input-plan.md` の状況を更新した。
Windows でのビルドは試していない（Linux 専用のコードは cfg で外してあり、共通部分の変更は cfg 付きのフィールドと呼び出しだけ）。
