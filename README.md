# VirtualDisplay

Mac に仮想モニターを生やし、その画面を LAN 越しに Windows / Ubuntu へ低遅延で映す自作ツール。
入力の逆送はしない（映像専用）。Mac 側は Apple 純正フレームワークのみで外部依存なし。

```
CGVirtualDisplay (非公開API) → ScreenCaptureKit → VideoToolbox HEVC (HW) → TCP → 受信側でデコード・表示
```

## 構成

| パス | 内容 |
|---|---|
| `mac-sender/` | 送信側 `vdsend`（Swift Package, 依存なし） |
| `client/` | 受信側（Rust, フェーズ2で作成予定） |

## ビルド（Mac）

```sh
cd mac-sender
swift build -c release
# => .build/release/vdsend
```

初回は「画面収録」の許可が必要（起動したターミナルアプリに対して付与される）。

## 使い方

```sh
# FHD 60Hz (4:2:0)
.build/release/vdsend --mode 1920x1080@60

# 4K 60Hz, HiDPI（見た目 1920x1080 / 実解像度 3840x2160）
.build/release/vdsend --mode 1920x1080@60 --hidpi --bitrate 120

# UWQHD 120Hz
.build/release/vdsend --mode 3440x1440@120 --bitrate 150

# 仮想モニターをメインにする（終了時に元に戻る）
.build/release/vdsend --mode 1920x1080@60 --main
```

`vdsend --help` で全オプション。Ctrl-C で終了すると仮想モニターも消える。

## 受信側（フェーズ1：既存の OSS プレイヤーで確認）

TCP ポート 7777 で生の HEVC Annex-B ストリームを配信している。

```sh
# ffplay（FFmpeg 同梱）
ffplay -hide_banner -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 \
       -framedrop -f hevc tcp://<MacのIP>:7777

# mpv（HW デコード: Windows=d3d11va, Ubuntu=vaapi）
mpv --profile=low-latency --untimed --hwdec=auto --demuxer-lavf-format=hevc \
    --no-cache tcp://<MacのIP>:7777
```

## 実測（M1 / macOS 26.5.1, 送信側のみ）

| モード | 出力 | エンコード p50 | 描画→エンコード完了 p50 |
|---|---|---|---|
| FHD 60 4:4:4 | Rext yuv444p | 約7ms | 約9ms |
| 4K 60 (HiDPI) 4:2:0 | Main yuv420p | 約16ms | 約15ms |
| 4K 60 (HiDPI) 4:4:4 | Rext yuv444p | 約13ms | 約19ms |
| UWQHD 120 4:2:0 | Main yuv420p | 約9.5ms | 約10ms |

- M1 のエンコーダ上限は約 650Mpx/s。UWQHD 144Hz（約 713Mpx/s）は処理が追いつかないため、UWQHD は 120Hz まで。
- Radeon（AMD）は HEVC 4:4:4 のハードウェアデコード非対応のため、4:4:4 は受信側 CPU でのデコードになる。

## 制約・注意

- `CGVirtualDisplay` は非公開 API。macOS 更新で動かなくなる可能性がある。
- 画面が静止すると送信は止まる（帯域ほぼゼロ）。静止直後に同じフレームを 2 回再エンコードして画質を上げる（`--no-refine` で無効化）。
- 視聴側は同時に 1 台まで。新しい接続が来ると古い接続は切れる。
