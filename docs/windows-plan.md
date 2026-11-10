# Windows 版 vdview：検討と作業計画

新しいセッションで作業を始めるための引き継ぎメモ。README と `docs/protocol.md` を先に読むこと。

## 前提

- 受信 PC は Ubuntu と同じ機体（Ryzen 9 9950X3D / Radeon RX 9070 XT / 2.5GbE）。Windows はデュアルブート想定。
- 送信側（Mac の `vdsend`）は変更不要。プロトコルも共通。
- 方針：外部依存は増やさない（FFmpeg の公式系ビルド、Rust の既存クレートのみ）。

## 現状コードの Windows 適合性

| 部分 | 状況 | 必要な作業 |
|---|---|---|
| winit / wgpu | Windows 対応済み（DX12 / Vulkan） | 動作確認のみ。DX12 で Mailbox が選ばれるか確認 |
| C シム `csrc/vd_decoder.c` | 自動選択の候補に `d3d11va` → `vulkan` を記述済み | MSVC でのビルド確認 |
| `build.rs` | `FFMPEG_DIR`（include/ と lib/）に対応済み | ビルド後に FFmpeg の DLL を `target/release` へコピーする処理を追加 |
| UDP 受信 | WSAECONNRESET を無視する処理と、受信バッファの拡張は実装済み | Windows の既定のタイマー分解能（約 15.6ms）で 2ms の待ち時間が粗くなるため、`timeBeginPeriod(1)` を検討 |
| デコード | RX 9070 XT は D3D11VA で HEVC Main / Main10 に対応。4:4:4 は非対応のため CPU デコード（Ubuntu と同じ扱い） | 4:4:4 を既定にしたときに CPU デコードへ回ることを確認 |
| 起動スクリプト | `client/linux/*` は sh。`~/.cache/vdview.log` に出力 | PowerShell 版を `client/windows/` に用意（ログは `%LOCALAPPDATA%\vdview\vdview.log`） |
| Mac 操作ヘルパー | `mac-wifi-restart` / `mac-unlock`（ssh）、`mac-vnc`（Remmina） | Windows 標準の OpenSSH クライアントで同等の .ps1 を作る。VNC は TigerVNC Viewer（OSS）を想定 |

## 作業計画

### 1. Windows 側の準備（ユーザー作業）

1. Visual Studio Build Tools（「C++ によるデスクトップ開発」）をインストール
2. Rust（rustup、MSVC ツールチェーン）と Git をインストール
3. FFmpeg の shared ビルド（例：BtbN `ffmpeg-n7.1-latest-win64-gpl-shared-7.1.zip`）を `C:\ffmpeg` に展開
4. 遠隔作業用：「オプション機能」から OpenSSH サーバーを有効化し、Mac の公開鍵を登録する
   - 管理者ユーザーの場合、鍵は `C:\ProgramData\ssh\administrators_authorized_keys` に置く（`~/.ssh/authorized_keys` は無視される）
   - 権限：`icacls administrators_authorized_keys /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"`

### 2. コード変更（Claude）

1. `build.rs`：Windows ではビルド後に `FFMPEG_DIR\bin\*.dll` を出力先へコピー
2. `receiver.rs`：Windows で `timeBeginPeriod(1)` を呼ぶ（`windows-sys` は増やさず、`extern "system"` で `winmm` を直接宣言）
3. `client/windows/`：`vdview-mac.ps1`、`mac-wifi-restart.ps1`、`mac-unlock.ps1`、`mac-vnc.ps1`、ショートカット作成用の `install.ps1`
4. README に Windows のビルド手順と運用手順を追加

### 3. 動作確認

- SSH 経由で起動したプロセスはセッション 0（デスクトップ外）で動くため、ウィンドウが表示されない。
  表示を伴う試験は、`schtasks /create ... /it` で対話型タスクを作り、`schtasks /run` でユーザーのデスクトップ上に起動する。
- 確認の順番：
  1. `vdview --decode-test`：4:2:0 10bit が `d3d11va (p010le)`、4:4:4 が software になること
  2. `--snapshot`：色の PSNR が Ubuntu と同等（4:4:4 で約 55dB）であること
  3. 実機接続：遅延の内訳、F1〜F4、再接続
- Windows Defender ファイアウォール：初回起動時に「プライベート ネットワーク」で許可する。

## 未確定事項

- DX12 と Vulkan のどちらが低遅延か（Mailbox の可否、フレーム待ち時間）
- D3D11VA の GPU → CPU 転送時間（VA-API では UWQHD 4:2:0 10bit で約 7ms）。遅ければ、D3D11 テクスチャを wgpu と共有するゼロコピー化を検討
