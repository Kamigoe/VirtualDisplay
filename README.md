# VirtualDisplay

Mac に仮想モニターを生やし、その画面を LAN 越しに Windows / Ubuntu へ低遅延で映す自作ツール。
映像専用（入力の逆送はしない）。

```
[Mac: vdsend]  CGVirtualDisplay → ScreenCaptureKit → VideoToolbox HEVC ─┐
                                                                      │ UDP (独自プロトコル v1)
[PC:  vdview]  画面に表示 ← wgpu (YUV→RGB) ← FFmpeg デコード ←──────────┘
```

| パス | 内容 |
|---|---|
| `mac-sender/` | 送信側 `vdsend`（Swift。Apple 純正フレームワークのみ） |
| `client/` | 受信側 `vdview`（Rust + FFmpeg の小さな C シム + wgpu） |
| `docs/protocol.md` | 通信プロトコル仕様 |

## クイックスタート

```sh
# Mac
cd mac-sender && swift build -c release
.build/release/vdsend

# PC（ビルド方法は下記）
vdview <MacのIPアドレス>
```

`vdview` は PC のモニターの解像度・スケール・リフレッシュレートを Mac に伝え、Mac 側の仮想モニターが
それに合わせて切り替わる（実モニターを挿した時の EDID と同じ考え方）。

---

## 送信側 `vdsend`（Mac）

```sh
cd mac-sender
swift build -c release
.build/release/vdsend [options]
```

| オプション | 説明 |
|---|---|
| `--mode WxH@HZ` | 起動時のモード（既定 1920x1080@60）。接続した受信側の要求で変わる |
| `--hidpi` | 2x（Retina）。`--mode 1920x1080@60 --hidpi` で実解像度 3840x2160 |
| `--chroma 420\|444` | 既定の色差サンプリング（受信側の要求が優先） |
| `--bitrate MBPS` | 既定の平均ビットレート（既定 50） |
| `--port N` | 制御 (TCP) と映像 (UDP) のポート（既定 7777） |
| `--lock-mode` | 受信側からのモード変更要求を無視する |
| `--main` | 実行中は仮想モニターをメインディスプレイにする |
| `--raw-port N` | デバッグ用：生の HEVC を TCP で配信（ffplay / mpv で見られる） |

- 初回は「画面収録」の許可が必要（起動したターミナルアプリに付与される）。
- 視聴者がいない間はエンコードを止める（ファンレスの Air 向けの省電力）。
- 画面が静止すると送信も止まり、静止直後に同じフレームを再エンコードして文字を鮮明にする。

### 蓋を閉じて使う

```sh
sudo pmset -a disablesleep 1   # 蓋を閉じてもスリープしない（戻すときは 0）
```

蓋を閉じると内蔵パネルが消え、仮想モニターが唯一の画面になる。
Mac の操作には Bluetooth のキーボード / マウスを使う。

## 受信側 `vdview`（Windows / Ubuntu）

### ビルド

必要なもの：Rust（rustup）、C コンパイラ、FFmpeg の開発用ファイル（libavcodec / libavutil）。

**Windows**

1. [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)（C++ ワークロード）と Rust をインストール
2. FFmpeg の shared ビルドを取得（例：BtbN の `ffmpeg-master-latest-win64-gpl-shared.zip`）し、任意の場所に展開
3. ビルド：
   ```powershell
   $env:FFMPEG_DIR = "C:\ffmpeg"   # include\ と lib\ がある場所
   cd client
   cargo build --release
   ```
4. 実行時に `C:\ffmpeg\bin` の DLL（`avcodec-*.dll`, `avutil-*.dll` など）が必要。
   PATH に追加するか、`target\release\vdview.exe` と同じフォルダへコピーする。

**Ubuntu**

```sh
sudo apt install build-essential pkg-config libavcodec-dev libavutil-dev libva-dev mesa-va-drivers
cd client && cargo build --release
# 4K のキーフレームを取りこぼさないよう UDP 受信バッファの上限を上げる
sudo sysctl -w net.core.rmem_max=8388608
```

RX 9070 XT（RDNA4）の VA-API デコードには Mesa 25 以降が必要。

### 使い方

```sh
vdview <MacのIP>[:port] [options]
```

| オプション | 説明 |
|---|---|
| `--mode WxH@HZ` | 要求する論理解像度（既定：このPCのモニターに合わせる） |
| `--hidpi` / `--no-hidpi` | Mac 側を 2x にするか（既定：モニターのスケールが 175% 以上なら 2x） |
| `--server-mode` | Mac 側の現在のモードをそのまま使う |
| `--chroma 420\|444` | 既定 420。444 は AMD GPU ではCPUデコードになる |
| `--bitrate MBPS` | 平均ビットレート |
| `--hwaccel NAME` | `auto`（既定）/ `none` / `d3d11va` / `vaapi` / `vulkan` |
| `--monitor N` | 表示するモニター（起動時に一覧を表示） |
| `--windowed` | ウィンドウで起動（F11 で全画面切替、Esc で全画面解除） |
| `--vsync` | FIFO 表示（ティアリングなし・遅延増） |

ウィンドウタイトルと標準出力に 1 秒ごとの統計が出る：
fps / 表示 fps / ビットレート / 遅延（Mac で描画 → PC で表示）/ デコード時間 / RTT / 再送・欠落。

モニターのリフレッシュレートが Mac のエンコーダの上限（約 650 Mpx/s）を超える場合は自動で下げる
（例：4K 144Hz → 60Hz、UWQHD 144Hz → 120Hz）。

### 自己テスト

送信側やモニターなしで、デコードと色変換を検証できる：

```sh
vdview --snapshot 画像.hevc 出力.ppm [--hwaccel none]
```

---

## 実測（M1 / macOS 26.5.1）

**エンコード（送信側）**

| モード | 出力 | エンコード p50 |
|---|---|---|
| FHD 60 4:4:4 | Rext yuv444p | 約7ms |
| 4K 60 (HiDPI) 4:2:0 | Main yuv420p | 約16ms |
| 4K 60 (HiDPI) 4:4:4 | Rext yuv444p | 約13ms |
| UWQHD 120 4:2:0 | Main yuv420p | 約9.5ms |

UWQHD 144Hz（約 713 Mpx/s）はエンコーダが追いつかないため 120Hz まで。

**画質（元画像との PSNR、Apple の変換 + VideoToolbox HEVC を通した testsrc2）**

| 経路 | PSNR |
|---|---|
| 4:4:4 | 55.5 dB |
| 4:2:0（色差は最近傍で復元） | 49.0 dB |

## 既知の制約

- `CGVirtualDisplay` は非公開 API。macOS 更新で動かなくなる可能性がある。
- WindowServer は 1 プロセスにつき最初の仮想モニターしかオンラインにしないため、モード変更は同じモニターへの設定変更で行う。
- ディスプレイがスリープしている間、終了したプロセスの仮想モニターは片付けられず、画面の合成も止まる。`vdsend` は起動時と視聴者接続時にディスプレイを起こして対処している。
- 再起動直後（FileVault のロック解除前）は動かない。ログインまでは macOS 標準の画面共有（VNC）を使う。
- 受信側 1 台まで。新しい接続が来ると古い接続は切れる。
