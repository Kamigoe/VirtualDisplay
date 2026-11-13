# Mac 側の作業手順：入力の統合（フェーズ 1）

Windows 側のセッションから Mac 側のセッションへの引き継ぎメモ（2026-11-13）。
作業が終わったら、このファイルの「結果」に記入してコミット・push するか、結果を利用者に伝えること。

## 背景

`vdview`（PC）のキーボードとマウスで Mac を操作できるようにした（mac-input-cast の後継）。設計は
`docs/input-plan.md`、通信仕様は `docs/protocol.md` の「入力」。

- 制御用 TCP の上で、共有シークレット（32 バイト）による相互認証 + X25519 の鍵交換を行い、
  入力は ChaCha20-Poly1305 で暗号化して送る。Mac 側は CryptoKit（Apple 純正）。
- Mac 側の変更：`InputCrypto.swift`（新規。握手と暗号）、`Injector.swift`（新規。CGEvent でキー・マウスを再生）、
  `ControlServer.swift`（握手と入力メッセージの処理）、`Protocol.swift`、`Streamer.swift`（仮想モニターの ID）、
  `main.swift`（`--input-self-test`、アクセシビリティの許可の確認・要求）、`scripts/make-input-secret.sh`（新規）。
- **Mac 側のコードは Windows ではビルドできていない。** 鍵の導出と暗号は、Rust の実装と、別に書いた
  Python の実装で同じ試験データになることを確認済み。Swift 側は手順 3 の自己テストで照合する。

## 手順

1. 最新を取得する：
   ```sh
   cd <VirtualDisplay のリポジトリ>
   git pull
   git log --oneline -3   # 「Forward keyboard and mouse to the Mac」が先頭にあること
   ```
2. ビルドが通り、警告が増えていないことを確認する：
   ```sh
   cd mac-sender && swift build -c release 2>&1 | grep -E "error:|warning:|Compiling|Build complete"
   ```
   **コンパイルエラーが出たら直してよい**（Windows 側では Swift をビルドできなかったため）。
   直した内容は「結果」に書き、コミットに含めること。プロトコルや鍵の導出（`docs/protocol.md`）は変えないこと。
3. 鍵の導出が `vdview` と一致することを確かめる：
   ```sh
   .build/release/vdsend --input-self-test
   ```
   全行が `ok` で、最後に `input self-test passed` と出ること。`FAIL` があれば、その行を「結果」に貼って止める
   （インストールしない）。
4. **今の起動引数を確認する**（`install.sh` は引数で LaunchAgent を書き直す。引数なしだと
   `--mode 3440x1440@60 --main`）：
   ```sh
   /usr/libexec/PlistBuddy -c "Print :ProgramArguments" ~/Library/LaunchAgents/com.kamigoe.vdsend.plist
   ```
5. 同じ引数でインストールする（動いているエージェントは自動で再起動。Windows の表示は数秒途切れる）：
   ```sh
   cd .. && mac-sender/scripts/install.sh <手順 4 の引数>
   ```
6. アクセシビリティの許可（キー・マウスのイベントを送るのに必要）：
   ```sh
   open -a ~/Library/VirtualDisplay/vdsend.app --args --request-permission
   sleep 3; tail -3 ~/Library/Logs/vdsend-permission.log
   ```
   システム設定 > プライバシーとセキュリティ > **アクセシビリティ** に vdsend が出るので、**利用者に**オンにしてもらう
   （画面での操作が必要。Mac の画面は Windows の vdview か VNC で見られる）。その後：
   ```sh
   ~/Library/VirtualDisplay/vdsend.app/Contents/MacOS/vdsend --check-permission
   ```
   `accessibility permission ...: granted` と `screen recording permission: granted` の両方が出ること。
   （ターミナルから直接実行した場合、表示されるのはターミナルの許可になることがある。その場合は
   `open -a ~/Library/VirtualDisplay/vdsend.app --args --check-permission; sleep 2; tail -2 ~/Library/Logs/vdsend-permission.log`）
7. 共有シークレットを作る：
   ```sh
   mac-sender/scripts/make-input-secret.sh
   ```
   表示された 16 進 64 文字は**利用者に直接伝える**（このファイル・コミット・ログには書かない）。
   利用者は Windows で `vdview --set-input-secret` を実行して貼り付ける。
8. 起動を確認する：
   ```sh
   tail -5 ~/Library/Logs/vdsend.log   # 「control listening on tcp port 7777」があること
   ```
9. Windows 側のセッションに「Mac 側の更新が終わった」と伝えてもらう（利用者経由）。Windows 側で接続して
   キーボード・マウスを試したあと、Mac のログで次を確認する：
   ```sh
   grep -E "input:|viewer connected|viewer disconnected" ~/Library/Logs/vdsend.log | tail -20
   ```
   期待する結果：`input: <PC の接続元> authenticated` が出て、`accessibility permission missing` が付いていないこと。
   キー入力の内容はログに出ない（出していない）。

## 結果

（Mac 側のセッションが記入する）
