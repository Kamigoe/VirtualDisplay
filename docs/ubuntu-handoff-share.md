# Ubuntu 側の作業手順：カーソルとクリップボードの共有（Ubuntu 対応）

Windows 側のセッションから、Mac 側のセッション（SSH で Ubuntu の PC に入って作業する）への引き継ぎメモ（2026-11-16）。
作業が終わったら、このファイルの「結果」に記入してコミット・push すること。

## 背景

ロードマップの 3.（設計は `docs/share-plan.md`、通信仕様は `docs/protocol.md` の「カーソルとクリップボード」）は、
Windows と Mac の組み合わせで動作を確認済み（`db8514b`、経過は `docs/mac-handoff-share.md`）：

- **カーソル**：転送 ON の間、Mac はカーソルを映像に入れず、絵が変わるたびに送る。`vdview` はそれを映像の拡大率に
  合わせて縮め（`client/src/cursor.rs`）、winit の `CustomCursor` でウィンドウのカーソルにする（`main.rs` の `update_cursor`）。
  **共通のコードなので Ubuntu でもそのまま動くはず。ただし Ubuntu では一度も試していない。**
- **テキストのクリップボード**：双方向・自動。PC 側の OS 依存部分は `clipboard::Clipboard`（`start(on_copy)` と `set(text)`）。
  Windows は `client/src/clipwin.rs`。**Linux は `clipboard.rs` の空の実装（`start` が `None`）のままなので、Ubuntu では共有されない。**

Ubuntu は Wayland・GNOME Shell 46・24.04.5（`docs/ubuntu-handoff-input.md` の A-3）。

## 手順

### A. 準備（SSH で実施）

1. 最新を取得してビルドする：
   ```sh
   cd ~/VirtualDisplay && git pull && git log --oneline -3
   cd client && cargo build --release 2>&1 | tail -3 && cargo test --release 2>&1 | grep "test result"
   ```
2. 画面のファームウェアの固定（`docs/ubuntu-handoff-input.md` の「前置き」）がそのままか確かめる：
   `apt-mark showhold`。直った版が出ていれば `unhold` してよい（その場合は結果に書く）。

### B. カーソルの試験（利用者が Ubuntu の画面で操作する）

アプリ一覧の「Mac (VirtualDisplay)」で起動してもらい、次を試してもらう（`docs/mac-handoff-share.md` の 5 の 1〜4 と同じ）：

1. PC のマウスに遅れずに付いてくるか。Mac のカーソルが二重に見えないか
2. 形（文字の上で I ビーム、リンクの上で手、ウィンドウの端でリサイズの矢印）とクリック位置
3. 全画面とウィンドウ表示（F11）、HiDPI ⇔ 等倍（F5）での大きさ。GNOME の拡大率が 100% 以外ならその値も記録
4. F8 で OFF → 映像にカーソルが戻る、ON → PC 側の表示に戻る

形が変わらない・カーソルが見えないなどがあれば直してよい（winit の Wayland のカスタムカーソルの扱いを疑う）。

### C. クリップボードを実装する

`clipwin.rs` と同じ振る舞いにする：

- PC でコピーされたテキストを `on_copy`（LF の改行）に渡す。**`vdview` を起動した時点の中身は渡さない**（その後のコピーだけ）。
- `set(text)` で Mac からのテキストを PC のクリップボードに入れる。**それによる変化を `on_copy` に返さない**（Mac に送り返さない）。
- パスワード管理ソフトが「共有しない」印を付けた内容は渡さない。Linux の慣習は MIME 型 `x-kde-passwordManagerHint`
  （KeePassXC などが `secret` を入れる）が提供されていること。
- MIME 型は `text/plain;charset=utf-8` を優先し、`UTF8_STRING`、`text/plain` も受け付ける。書くときも同じ 3 つを提供する。
- Linux 専用のコードは `#[cfg(target_os = "linux")]` で囲み、`clipboard.rs` の空の実装は Linux 以外（今は無いが）に残す。
  Windows のビルドが壊れないこと。通信仕様は変えない。依存は Rust のクレートに限り、なるべく `Cargo.lock` に既にあるもの。

Wayland での注意点と方針：

- GNOME 46 は、フォーカスのないクライアントがクリップボードを読み書きするためのプロトコル（wlr / ext の data-control）を
  持たない。普通のクライアントは `wl_data_device` で、**キーボードのフォーカスがあるときだけ**選択（selection）を受け取り、
  設定にはそのクライアントの最近の入力イベントの serial が要る。
- したがって：
  - **PC → Mac**：別のアプリでコピー → `vdview` をクリックして前面にした時点で `selection` が届くので、そこで送る
    （Mac 側で貼り付ける前に `vdview` に戻るはずなので、使い勝手は変わらない）。届いた選択が自分で設定したものなら送らない。
    起動直後・最初のフォーカスで届く選択は「起動時の中身」なので送らない。
  - **Mac → PC**：Mac でコピーするのは `vdview` が前面のときなので、届いたらすぐ、最新の serial で `set_selection` する。
    前面でないときに届いたら、次にフォーカスを得たときに設定する（README に「Ubuntu では `vdview` を一度前面にしてから」と書く）。
- 実装は `wlinhibit.rs` と同じく、winit の `wl_display` を `wayland-client`（`wayland-backend` の `client_system`）で使い、
  自分のイベントキューで `wl_data_device_manager` を使うのが依存を増やさない方法。serial を知るには、同じ seat から
  自分用の `wl_keyboard`（と必要なら `wl_pointer`）を取る（1 つのクライアントが複数持ってよい）。
  `smithay-clipboard`（winit が既に使っている `smithay-client-toolkit` の上にある）の方が簡単ならそれでもよいが、
  変化の通知がない点（`load` で読みに行く方式）に注意。どちらを選んだかと理由を「結果」に書く。
- 選択の中身はパイプ（`receive` で渡す fd）で受け取る。読み終わるまでイベントループを止めないよう、読み出しは別スレッドで。
  書く側（`wl_data_source` の `send`）も同じ。1 MiB を超える分は読まずに捨ててよい（送らないので）。
- `Clipboard::start` は今、ウィンドウを受け取らない。Linux で `wl_display` / seat が必要なら、`start(&window, on_copy)` のように
  引数を足し、Windows 側（`clipwin.rs`）は受け取って無視する形に揃えてよい。終了時は `wlinhibit` と同じく、winit が接続を
  閉じる前（`exiting`）に破棄する。
- X11 のセッションは対応しなくてよい（`start` が `None`、README に書く）。

### D. クリップボードの試験（利用者が実施）

1. Mac でコピー → Ubuntu のテキストエディタで貼り付け（日本語・複数行）
2. Ubuntu のテキストエディタでコピー → `vdview` を前面にして → Mac で貼り付け
3. 大きいテキスト（数百 KB）を両方向。1 MiB を超えるものは送られず、ログ（`~/.cache/vdview.log`）に出ること
4. 送り返しがないこと（Mac → Ubuntu のあと、Ubuntu から同じテキストが Mac に戻ってこない。ログで確認）
5. `vdview` が前面でないときに Mac でコピー → 前面にしたあと Ubuntu で貼り付けられるか
6. （あれば）KeePassXC などでパスワードをコピーしても Mac に送られないこと

実装と試験が終わったら、README（「カーソルとクリップボード」の Ubuntu の行）、`docs/share-plan.md` の状況、
`docs/roadmap.md` の 3. を更新する。

## 結果

（Mac 側のセッションが記入）
