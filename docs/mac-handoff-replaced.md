# Mac 側の作業手順：受信側の奪い合い対策（0x06 Replaced）

Windows 側のセッションから Mac 側のセッションへの引き継ぎメモ（2026-11-13）。
作業が終わったら、このファイルの「結果」に記入してコミットするか、結果を利用者に伝えること。

## 背景

`vdsend` は受信側を 1 台しか持たず、新しい接続が来ると古い接続を切る。`vdview` は切られると 2 秒ごとに
自動で再接続するので、`vdview` が 2 つ動いていると互いに Mac を奪い合い、どちらも映らなくなっていた
（2026-11-12 の測定中に実際に起きた）。

対策（このコミットで実装済み。Mac 側はまだビルド・インストールしていない）：

- `vdsend`（`ControlServer.swift` の `accept`）：古い受信側を切る前に、新しい制御メッセージ
  `0x06 Replaced`（本文は新しい受信側の接続元、UTF-8）を送り、送信完了後に閉じる。応答がなくても 1 秒後に閉じる。
- `vdview`：`Replaced` を受け取ったら自動再接続せず、「TAKEN OVER BY ANOTHER VIEWER」と表示して待つ。Enter で再接続。
- 仕様は `docs/protocol.md` の「0x06 Replaced」。古い受信側は未知のメッセージとして無視する（互換性あり）。

## 手順

1. 最新を取得する：
   ```sh
   cd <VirtualDisplay のリポジトリ>
   git pull
   git log --oneline -3   # 「Stop two viewers from taking the sender from each other」が先頭にあること
   ```
2. ビルドが通り、警告が増えていないことを確認する：
   ```sh
   cd mac-sender && swift build -c release 2>&1 | tail -20
   ```
3. **今の起動引数を確認する**（`install.sh` は引数で LaunchAgent を書き直す。引数なしだと
   `--mode 3440x1440@60 --main` になる）：
   ```sh
   /usr/libexec/PlistBuddy -c "Print :ProgramArguments" ~/Library/LaunchAgents/com.kamigoe.vdsend.plist
   ```
   先頭の `.../vdsend` を除いた残りが、今の引数。
4. 同じ引数でインストールする（動いているエージェントは自動で再起動される。その間、Windows の表示は数秒途切れる）：
   ```sh
   mac-sender/scripts/install.sh <手順 3 の引数>
   ```
   署名は固定の証明書なので、画面収録の許可は外れないはず。外れた場合は README の「初回のみ」の手順で許可し直す。
5. 起動を確認する：
   ```sh
   tail -5 ~/Library/Logs/vdsend.log   # 「control listening on tcp port 7777」があること
   ```
6. Windows 側のセッションに「Mac 側の更新が終わった」と伝えてもらう（利用者経由）。
   Windows 側で `vdview` を 2 つ起動して試験する。その後、Mac のログで次を確認する：
   ```sh
   grep -E "replacing viewer|viewer connected|viewer disconnected|hello from" ~/Library/Logs/vdsend.log | tail -20
   ```
   期待する結果：`replacing viewer A with B` が **1 回だけ**出て、その後 A からの `hello from` が続かないこと
   （修正前は 2 秒おきに A と B が交互に現れていた）。

## 結果

- 2026-11-13 02:22（JST）Mac 側で更新完了（`28b6651`）。
  - `swift build -c release`：警告・エラーなし。
  - 起動引数は従来どおり `--mode 3440x1440@60 --main` で `install.sh` を実行。エージェントは自動で再起動（`restarted com.kamigoe.vdsend (update)`）。
  - 署名の designated requirement は不変（`certificate leaf = H"0764f89b…"`）。画面収録の許可は `granted` のまま。
  - `vdsend.log` に `control listening on tcp port 7777` を確認。更新時点で受信側の接続はなし。
  - 補足：Mac のリポジトリに 11/11 の未コミットの Windows 対応（`client/windows/` ほか、Windows 側のコミットで置き換え済み）が
    残っていたため、`git stash`（`Mac-side WIP Windows support (2026-11-11)…`）に退避してから pull した。
- 2026-11-13 Windows 側で手順 6 の試験を実施（どちらも `--windowed --server-mode`）：
  - 0 秒 A 起動 → 接続。6 秒 B 起動 → B が接続し、A は `the sender switched to another viewer (192.168.11.12:49593)`
    と出して停止。その後 10 秒間、A は再接続しなかった（修正前は 2 秒おきに奪い合っていた）。
  - 16 秒 A で Enter → A が再接続して映像を取り戻し、今度は B が同じ表示で停止。
  - A・B とも、接続中は映像が流れた（統計行 13 行 / 10 行）。
- 同じ時間帯の Mac 側のログ（手順 6）：`replacing viewer` はちょうど 2 回（B が A を、10 秒後に再接続した A が B を）。
  その間に再接続の `viewer connected` はなく、最後は試験終了時の `viewer disconnected` のみ。**対策は有効。**
  置き換えられた側の接続は `detach` を通らないため、`viewer disconnected` が出ないのは想定どおり。
