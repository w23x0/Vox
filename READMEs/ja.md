# Vox

ライブ会話のためのデスクトップ向けリアルタイム音声翻訳アプリ（**Windows** / **Linux**）。2 つの独立したパイプラインが同時に動作します:

| パイプライン | 入力 | 出力 |
| --- | --- | --- |
| **Speak out** | 自分のマイク | バーチャルマイクへの翻訳音声 + ライブ字幕 |
| **Listen in** | 選択したプログラムの音声 | ヘッドホンへの中国語音声 + ライブ字幕 |

**Tauri 2 + React 19 + Rust** で構築されています。UI は設定と状態を担当し、音声キャプチャ・ノイズ除去・リサンプリング・WebSocket 転送・ホットキー・字幕オーバーレイウィンドウは Rust 側に実装され、プラットフォームごとのシェル（Windows: `vox-*-win`、Linux: `vox-*-linux`）に分かれています。コア（`vox-core` / `vox-net` / `vox-dsp`）はプラットフォーム非依存です。

English： [`../README.md`](../README.md) · 简体中文： [`zh-CN.md`](zh-CN.md) · 한국어： [`ko.md`](ko.md) · Español： [`es.md`](es.md) · Français： [`fr.md`](fr.md) · Deutsch： [`de.md`](de.md)

## スコープ

- **Windows**: プロセスループバック（"Listen in"）は **Win11 / Server 2022（build 20348+）** が必要。
- **Linux**: **PipeWire** が必要（主要ディストリビューションでは標準）。"Listen in" は対象プログラム自身の音声ストリームを直接取り込みます。仮想マイクは PipeWire の sink そのもの（インストール不要）。Wayland セッションでは、位置指定と最前面表示のためにオーバーレイは XWayland 経由で動作します。グローバルホットキーは `/dev/input`（evdev）を直接読みます。PipeWire 以外の音声基盤は非対応。
- プロバイダ — **Alibaba Cloud Bailian**、**Google Gemini**、**OpenAI Realtime** — パイプラインごとに選択可能で、それぞれ固定のリアルタイム翻訳モデルを 1 つ使用。
- "Listen in" は常に **中国語へ** 翻訳。翻訳元の言語は自動検出または手動指定。
- UI 言語は 简体中文 / 日本語 / English で、翻訳の言語とは独立。
- プロバイダの API キーはローカルに保存され、ユーザーごとに暗号化されます: Windows は **DPAPI**、Linux は **Secret Service**（gnome-keyring / KWallet、`keyring` 経由）。
- プロバイダのメタデータは [`catalog/*.json`](../catalog/) 内に置かれ、ソースコードには含めない。

## 開発 (Develop)

前提条件: Windows 11 x64、Node.js `^20.19.0` または `>=22.12.0`、Rust stable（`x86_64-pc-windows-msvc`）、VS Build Tools（C++ デスクトップ）、WebView2、および API キー（またはフロントの Mock。これは不要）。

```powershell
cd app\ui
npm ci
npm run tauri:dev        # デスクトップアプリ全体
npm run dev              # UI のみ、http://127.0.0.1:5183/?mock=1 を開く
```

## インストーラのビルド

```powershell
cd app\ui
npm run tauri:build      # NSIS を target/release/bundle/nsis/ に生成
```

配布に `cargo build --release` は使わないでください。バイナリを手動でビルドする場合は、custom protocol フィーチャーを渡してください:

```powershell
cargo build --release -p vox --features custom-protocol
```

## テスト

```powershell
cargo test --workspace   # Rust、リポジトリルート
npm run verify           # app/ui 内: 型チェック、本番ビルド、a11y/disabled チェック
```

## ディレクトリ構成

```text
Vox/
├─ catalog/            # プロバイダのメタデータ: aliyun.json、gemini.json、gpt.json
├─ crates/
│  ├─ vox-core/        # プラットフォーム非依存のコア: 設定、プロトコル、状態機械、利用統計
│  ├─ vox-net/         # WebSocket 転送
│  ├─ vox-dsp/         # RNNoise ノイズ除去 + リサンプリング
│  ├─ vox-audio-win/   # WASAPI キャプチャ/再生、プロセスループバック、VB-CABLE
│  ├─ vox-input-win/   # グローバルホットキー
│  └─ vox-overlay-win/ # Win32 透過字幕ウィンドウ
├─ app/
│  ├─ src-tauri/       # Tauri レイヤー: コマンド、トレイ、永続化、DPAPI
│  └─ ui/              # React 設定 UI + ブラウザ Mock
├─ docs/               # 索引：docs/README.md（architecture/ platform/ protocols/ research/ plans/）
└─ README.md
```

## データフロー

**Speak:** `mic → mono → RNNoise → gate → 16 kHz PCM → provider → (字幕 + 24 kHz 音声を VB-CABLE へ)`

**Listen:** `loopback → mono → 16 kHz PCM → provider → (中国語字幕 + 24 kHz 音声をヘッドホンへ)`

両パイプラインは独立して動作し、`vox-core::Runtime` が唯一の真実の源です。

## 貢献 (Contributing)

Vox は意図的に保守的で、コアスコープは固定されています。外部モジュールが拡張のための公開ポイントです — **[`../CONTRIBUTING.md`](../CONTRIBUTING.md)** を参照してください — また、計画中の Discord アウトプロセスモジュール（`docs/protocols/DISCORD_PROTOCOL.md`、第 2 期・未定）も含まれます。

## ドキュメント

`docs/architecture/ARCHITECTURE.md`、`docs/architecture/DECISIONS.md`、`docs/platform/SCOPE.md`、`docs/platform/LINUX.md`、`docs/protocols/QWEN_PROTOCOL.md`、`docs/protocols/GEMINI_PROTOCOL.md`、`docs/protocols/PROVIDER_CATALOG.md`、`docs/protocols/DISCORD_PROTOCOL.md`。

## ライセンス

MIT

English: [`../README.md`](../README.md) · 简体中文: [`zh-CN.md`](zh-CN.md) · 한국어: [`ko.md`](ko.md) · Español: [`es.md`](es.md) · Français: [`fr.md`](fr.md) · Deutsch: [`de.md`](de.md)