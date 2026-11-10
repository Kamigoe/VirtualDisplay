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

### 2. コード変更（Claude）— 2026-11-10 実施

1. ✅ `build.rs`：Windows ではビルド時に `FFMPEG_DIR\bin\*.dll` を `target\<profile>\` へコピー
2. ✅ `receiver.rs`：Windows で `timeBeginPeriod(1)` を呼ぶ（`windows-sys` は増やさず、`extern "system"` で `winmm` を直接宣言）
3. ✅ `client/windows/`：`vdview-mac.ps1`、`mac-wifi-restart.ps1`、`mac-unlock.ps1`、`mac-vnc.ps1`、ショートカット作成用の `install.ps1`
   - Windows PowerShell 5.1 で構文確認済み。`mac-unlock.ps1` のポート確認と MAC アドレス検索は実機で動作確認済み
   - 非表示で動かすショートカットは `conhost.exe --headless` 経由（`-WindowStyle Hidden` は既定のターミナルが
     Windows Terminal だとウィンドウが出る）
   - TigerVNC は Apple 認証に非対応のため、Mac 側で VNC パスワードを有効にする必要がある（要確認）
4. ✅ README に Windows のビルド手順と運用手順を追加

### 3. 動作確認

- SSH 経由で起動したプロセスはセッション 0（デスクトップ外）で動くため、ウィンドウが表示されない。
  表示を伴う試験は、`schtasks /create ... /it` で対話型タスクを作り、`schtasks /run` でユーザーのデスクトップ上に起動する。
- 確認の順番：
  1. `vdview --decode-test`：4:2:0 10bit が `d3d11va (p010le)`、4:4:4 が software になること
  2. `--snapshot`：色の PSNR が Ubuntu と同等（4:4:4 で約 55dB）であること
  3. 実機接続：遅延の内訳、F1〜F4、再接続
- Windows Defender ファイアウォール：初回起動時に「プライベート ネットワーク」で許可する。

### 実施結果（2026-11-10、この PC の Windows 11）

- ツール：winget で VS 2022 Build Tools（VCTools）、Rustup（rustc 1.99.1 MSVC）、`BtbN.FFmpeg.GPL.Shared.7.1`
  （7.1.5）を導入。`C:\ffmpeg` は winget の展開先へのジャンクション。`cargo build --release` は警告なしで成功し、
  DLL は `target\release` へコピーされた。
- `--decode-test`（libx265 で作った 3440x1440 の 120 フレーム、B フレームなし）：

  | 入力 | auto | none |
  |---|---|---|
  | 4:2:0 10bit | d3d11va (p010le) 5.5 ms | software 4.4 ms |
  | 4:2:0 8bit | d3d11va (nv12) 4.5 ms | software 4.0 ms |
  | 4:4:4 8bit | software 4.7 ms | software 4.6 ms |

  `--hwaccel vulkan` も 4:2:0 10bit で動作（5.1 ms）。D3D11VA の GPU → CPU 転送込みでも VA-API（約 7ms）より速い。
- `--snapshot`：vdview の出力は FFmpeg 自身の YUV→RGB 変換と 75 dB で一致（D3D11VA とソフトウェアで同一）。
  元画像との PSNR は libx265 ロスレスの testsrc2 で 4:4:4 = 51.2 dB、4:2:0 10bit = 44.0 dB（8bit・リミテッドレンジの
  量子化が支配的なので、Mac 経由の README の値とは直接比べられない）。
- 実機接続（`--windowed --server-mode`、UWQHD 60Hz 4:4:4、Wi-Fi）：接続・表示・統計とも正常、欠落 0。

  | 描画 API | present | 遅延 p50 | show p95 |
  |---|---|---|---|
  | Vulkan（wgpu の既定） | Immediate | 約 36 ms | 約 1.0 ms |
  | DX12（`WGPU_BACKEND=dx12`） | Mailbox | 約 36 ms | 約 0.5 ms |

  遅延の大半は net（Mac での描画 → PC 受信、約 35ms）。ティアリングがなく show も短いため、
  `render.rs` で Windows の既定を DX12 にした（`WGPU_BACKEND` で上書き可）。
- `vdview-mac.ps1` をショートカットと同じ `conhost --headless` 経由で起動し、引数の受け渡しとログ追記を確認。
- 未確認：全画面での表示、F1〜F4、再接続、`install.ps1`（ショートカット作成）、`mac-wifi-restart.ps1`
  （Windows 用の鍵が未登録）、TigerVNC。

## 未確定事項

- ~~DX12 と Vulkan のどちらが低遅延か~~ → DX12（Mailbox）を既定にした。全画面での比較は未実施
- D3D11VA の GPU → CPU 転送時間（VA-API では UWQHD 4:2:0 10bit で約 7ms）。遅ければ、D3D11 テクスチャを wgpu と共有するゼロコピー化を検討
