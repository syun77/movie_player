# movie_player

Rustで作成した、macOSネイティブ動作のローカル動画プレイヤーです。  
YouTubeライクな基本操作（再生/一時停止、10秒戻る/進む、シークバー、ショートカット）を実装しています。

## 実装済み機能

- メニュー `ファイル > 開く` から動画ファイルを選択
- 再生 / 一時停止（トグル）
- 10秒戻る / 10秒進む
- シークバーで任意位置へ移動
- 再生時間 / 総時間の表示
- キーボードショートカット
	- `Space`: 再生 / 一時停止
	- `←`: 10秒戻る
	- `→`: 10秒進む

対応拡張子の例: `mp4`, `mkv`, `webm`, `mov`, `avi`, `m4v`

## 技術スタック

- Rust
- GTK4 (`gtk4-rs`)
- GStreamer (`gstreamer-rs` + `gtk4paintablesink`)
- OSネイティブファイルダイアログ (`rfd`)

## macOS セットアップ

GTK4のシステムライブラリが必要です。

```bash
brew install gtk4 pkg-config
```

動画再生は GStreamer (`playbin` + `gtk4paintablesink`) を利用します。動画が読み込めない・再生できない場合は GStreamer を導入してください:

```bash
# GStreamer 本体
brew install gstreamer
# Homebrew では gstreamer formula に主要プラグインが同梱されています
# (必要に応じて再インストール)
brew reinstall gstreamer
```

再生できない場合は、まず GStreamer がプラグインを認識しているか確認してください。

```bash
gst-inspect-1.0 playbin
gst-inspect-1.0 qtdemux
```

`No such element` が出る場合は、プラグインパスが通っていない可能性があります。

```bash
export GST_PLUGIN_PATH="/opt/homebrew/lib/gstreamer-1.0:$GST_PLUGIN_PATH"
export GST_PLUGIN_SYSTEM_PATH="/opt/homebrew/lib/gstreamer-1.0:$GST_PLUGIN_SYSTEM_PATH"
```

Apple Silicon環境で `pkg-config` が見つけられない場合は、以下をシェル設定に追加してください。

```bash
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:/opt/homebrew/share/pkgconfig:$PKG_CONFIG_PATH"
```

Intel Macの場合は `/usr/local` 配下になることがあります。

## 起動方法

```bash
cargo run
```

起動後、メニューの `ファイル > 開く` から動画を選択して再生します。