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

- 2026-11-13 13:55（JST）Mac 側で手順 1〜8 を実施（`44f991c`）。
  - 手順 2：`swift build -c release` はエラー・警告なし。**コードの修正は不要だった。**
  - 手順 3：`--input-self-test` は全 12 行 `ok`、`input self-test passed`。
  - 手順 4〜5：起動引数は従来どおり `--mode 3440x1440@60 --main`。`install.sh` でエージェントが自動再起動し、
    `control listening on tcp port 7777` を確認。
  - 手順 6：アクセシビリティの許可を要求済み（`accessibility permission: requested`）。画面収録は `granted` のまま。
    **利用者がシステム設定でオンにするのを待っている。**
  - 手順 7：`~/Library/VirtualDisplay/input-secret` を作成（権限 600）。値は利用者に直接伝えた（ここには書かない）。
  - インストール前に確認したこと：認証前の入力メッセージ（`secure`）は無視され、復号に失敗したら接続を切る。
    クライアントの MAC は定数時間で比較している。
- 手順 9（Windows からのキーボード・マウスの試験と、Mac のログ確認）は未実施。
- 2026-11-13 Windows 側で手順 9 の試験を実施：
  - シークレットを `vdview --set-input-secret` で保存。接続すると認証が成功し、
    `input to the Mac: on (F8 turns it off)`（＝ Mac 側がアクセシビリティの許可ありと報告）。
  - 利用者がキーボード・マウスを操作して試験し、「よい感じに動いている」。キーの押しっぱなし
    （F8 での切り替え、別ウィンドウへの切り替え）も、簡単に試した範囲では問題なし。
  - 新しい `vdview` を更新前の `vdsend` につないだ場合は、映像は映り、入力は
    `no reply (vdsend too old?)` で OFF のままだった（互換性あり）。
  - Win キーは Windows に先に取られる：Win+C は Windows の Copilot が開き、Win 単独ではスタートメニューが出る。
    Win キーを Mac の Control にしている既定では、Mac の Control の組み合わせが使えない。フェーズ 2
    （`docs/input-plan.md` の D2、低レベルのキーボードフック）で対応する想定どおりの制約。
  - Mac 側のログ（`input: ... authenticated`）は未確認。
- 2026-11-14 Mac 側で手順 9 のログを確認：Windows（`192.168.11.12` と IPv6 リンクローカル）からの接続はすべて
  `input: <接続元> authenticated` で、`accessibility permission missing` は付いていない（＝アクセシビリティの許可あり）。
  なお、エージェントの vdsend が動いている間は `open -a vdsend.app --args --check-permission` が
  LaunchServices のエラー -600 で起動できない。許可の確認は上のログで代用できる。
