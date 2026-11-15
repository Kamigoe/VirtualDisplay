# Mac 側の作業手順：カーソルとクリップボードの共有

Windows 側のセッションから Mac 側のセッションへの引き継ぎメモ（2026-11-16）。
作業が終わったら、このファイルの「結果」に記入してコミット・push すること。

## 背景

ロードマップの 3. を実装した（設計は `docs/share-plan.md`、通信仕様は `docs/protocol.md` の「カーソルとクリップボード」）。

- **カーソル**：転送 ON の間、Mac はカーソルを映像に入れず、カーソルの絵が変わるたびに `vdview` に送る。
  `vdview` はそれを PC 自身のマウスカーソルとして表示する（映像の遅れがなくなる）。F8 で OFF にすると映像に戻る。
- **クリップボード**：テキストを双方向・自動で共有する（1 MiB まで、暗号化した入力の経路で送る）。
- Mac 側の変更：`DesktopShare.swift`（新規。カーソルの監視と絵の作成、`NSPasteboard` の監視と書き込み）、
  `ControlServer.swift`（新しいメッセージの処理、`sendSecure`）、`Capture.swift` / `Streamer.swift`
  （`showsCursor` を取り込みを止めずに切り替える）、`Protocol.swift`、`Package.swift`（AppKit）。
- **Mac 側のコードは Windows ではビルドできていない。** PC 側はビルド・単体テスト・Windows の実際のクリップボードでの試験まで済み。

## 手順

1. 最新を取得する：
   ```sh
   cd <VirtualDisplay のリポジトリ>
   git pull
   git log --oneline -3   # 「Share the cursor and the text clipboard with the viewer」が先頭にあること
   ```
2. ビルドが通り、警告が増えていないことを確認する：
   ```sh
   cd mac-sender && swift build -c release 2>&1 | grep -E "error:|warning:|Build complete"
   ```
   **コンパイルエラーが出たら直してよい**。直した内容は「結果」に書き、コミットに含めること。
   通信仕様（`docs/protocol.md`）は変えないこと。
3. 入力の自己テストが今までどおり通ることを確認する：
   ```sh
   .build/release/vdsend --input-self-test   # 最後に input self-test passed
   ```
4. 今の起動引数を確認し、同じ引数でインストールする（Windows の表示は数秒途切れる）：
   ```sh
   /usr/libexec/PlistBuddy -c "Print :ProgramArguments" ~/Library/LaunchAgents/com.kamigoe.vdsend.plist
   cd .. && mac-sender/scripts/install.sh <上の引数>
   tail -3 ~/Library/Logs/vdsend.log   # control listening on tcp port 7777
   ```
5. 利用者に Windows の `vdview`（新しい版。Windows 側で `cargo build --release` 済み）で接続してもらい、次を試してもらう。
   結果（○ / × と、× のときに何が起きたか）を「結果」に書く：
   1. カーソル：PC のマウスの動きに遅れずに付いてくるか。Mac のカーソルが二重に見えないか（映像に残っていないか）
   2. カーソルの形：文字の上で I ビーム、リンクの上で手、ウィンドウの端でリサイズの矢印などに変わるか。
      クリック位置（ホットスポット）がずれていないか
   3. カーソルの大きさ：全画面とウィンドウ表示（F11）、HiDPI ⇔ 等倍（F5）で、Mac 上の見た目と同じくらいか
   4. F8 で OFF：Mac のカーソルが映像に戻るか。ON に戻すと PC 側の表示に戻るか
   5. クリップボード：Mac でコピー → Windows で貼り付け、Windows でコピー → Mac で貼り付け
      （日本語、複数行。Windows のメモ帳で改行が崩れないか）
   6. 大きいテキスト（数百 KB）が両方向で届くか。1 MiB を超えるものは送られず、送った側のログに出るか
6. Mac のログを確認する：
   ```sh
   grep -E "capture: cursor|clipboard:|input:|error" ~/Library/Logs/vdsend.log | tail -20
   ```
   期待する結果：転送 ON で `capture: cursor left out (the viewer draws it)`、F8 で OFF にすると
   `capture: cursor drawn into the video`。`cannot change the cursor setting` が出ないこと。

## うまくいかないときの手がかり

- **カーソルの形が届かない・矢印のまま変わらない**：`vdsend` は LaunchAgent で動く `NSApplication` のない
  プロセスなので、`NSCursor.currentSystem` が `nil` や古い絵を返す可能性がある（Windows 側では確かめようがなかった）。
  `DesktopShare.cursorShape` に一時的なログを入れて確かめる。必要なら `NSApplication.shared` を
  `.prohibited` の活性化方針で用意する、などを試してよい（Dock に出さないこと）。
- **カーソルが映像から消えない**：`SCStream.updateConfiguration` が効いていない。ログの `capture: cursor ...` を確認。
- **クリップボードが届かない**：PC 側は認証のあと ClipboardMode を送る。Mac 側は `NSPasteboard.general.changeCount` を
  0.25 秒ごとに見ている。どちらの向きが失敗するかを「結果」に書く。

## 結果

（Mac 側のセッションが記入）
