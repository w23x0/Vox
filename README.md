# Vox

A desktop real-time speech translator for live conversations — **Windows** and **Linux**. Two independent pipelines run at once:

| Pipeline | Input | Output |
| --- | --- | --- |
| **Speak out** | your microphone | translated speech into a virtual mic + live captions |
| **Listen in** | a chosen program's audio | Chinese speech to your headphones + live captions |

Built with **Tauri 2 + React 19 + Rust**. The UI handles config and status; audio capture, noise reduction, resampling, WebSocket transport, hotkeys and the overlay caption window live in Rust, behind per-platform shells (`vox-*-win` on Windows, `vox-*-linux` on Linux) — the core (`vox-core`, `vox-net`, `vox-dsp`) is platform-neutral.

简体中文：[`READMEs/zh-CN.md`](READMEs/zh-CN.md) · 日本語：[`READMEs/ja.md`](READMEs/ja.md) · 한국어：[`READMEs/ko.md`](READMEs/ko.md) · Español：[`READMEs/es.md`](READMEs/es.md) · Français：[`READMEs/fr.md`](READMEs/fr.md) · Deutsch：[`READMEs/de.md`](READMEs/de.md)

## Scope

- **Windows**: process loopback ("Listen in") needs **Win11 / Server 2022 (build 20348+)**.
- **Linux**: **PipeWire** required (default on mainstream distros). "Listen in" captures the chosen program's own streams via PipeWire; the virtual microphone is a PipeWire sink (nothing to install). The overlay runs through XWayland on Wayland sessions so it can position itself and stay on top; global hotkeys are read from `/dev/input` (evdev). Non-PipeWire audio stacks are not supported.
- Providers — **Alibaba Cloud Bailian**, **Google Gemini**, **OpenAI Realtime** — selectable per pipeline, each with one fixed realtime translation model.
- "Listen in" always translates **into Chinese**; the source language is auto-detected or set manually.
- UI language: 简体中文 / 日本語 / English, independent of the translation languages.
- Provider API keys are stored locally and encrypted per user: **Windows DPAPI** on Windows, **Secret Service** (gnome-keyring / KWallet, via `keyring`) on Linux.
- Provider metadata lives in [`catalog/*.json`](catalog/), not in source code.

## Develop

Prereqs, both platforms: Node.js `^20.19.0` or `>=22.12.0`, Rust stable, and an API key (or the frontend Mock, which needs none).

- **Windows**: Windows 11 x64, VS Build Tools (C++ desktop), WebView2, target `x86_64-pc-windows-msvc`. OpenVR bindings are vendored under `vendor/openvr_sys`; LLVM/Clang is not required for the regular build.
- **Linux**: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libpipewire-0.3-dev`, `libasound2-dev` (ALSA backend for the headless build, `vox-audio-alsa`), `clang` + `libclang-dev` (bindgen for `libspa-sys`), `patchelf` (AppImage), plus `build-essential`/`pkg-config`. Full list and the reason for each: [`docs/platform/LINUX.md`](docs/platform/LINUX.md) §7.

```powershell
cd app\ui
npm ci
npm run tauri:dev        # full desktop app
npm run dev              # UI only, open http://127.0.0.1:5183/?mock=1
```

Same commands on Linux (`cd app/ui && npm ci && npm run tauri:dev`). The UI mock has two extra switches: `?cold=1` (empty state) and `?platform=linux` (Linux shape of the virtual-microphone panel).

## Build the installer

```powershell
cd app\ui
npm run tauri:build      # produces NSIS at target/release/bundle/nsis/
```

On Linux the same command produces `deb` / `rpm` / `AppImage` under `target/release/bundle/`
(configured in `app/src-tauri/tauri.linux.conf.json`). Only the AppImage has in-app updates;
deb/rpm users download the new package manually.

Released Linux packages are built on Ubuntu 24.04, so they need **glibc ≥ 2.39**
(Ubuntu 24.04+, Fedora 40+, Debian 13+) and **PipeWire 1.0+** at runtime.

Don't use `cargo build --release` for shipping; if you build the binary by hand, pass the custom protocol feature:

```powershell
cargo build --release -p vox --features custom-protocol

# Optional SteamVR/OpenVR overlay build
cargo build --release -p vox --features "custom-protocol,steamvr-overlay"
```

## Test

```powershell
cargo test --workspace   # Rust, repo root
npm run verify           # in app/ui: type-check, prod build, a11y/disabled checks
```

Linux-only extras (need real hardware, so they are `#[ignore]`d or examples):

```bash
cargo run -p vox-audio-linux --example smoke -- devices   # devices + programs currently playing
cargo run -p vox-audio-linux --example smoke -- app pw-cat 5   # capture a program's audio, report RMS
cargo run -p vox-audio-linux --example virtual_mic 15     # create the virtual microphone for 15s
cargo run -p vox-overlay-linux --example live -- 20       # real caption overlay, scripted subtitles
cargo test -p vox-input-linux -- --ignored                # real key events through a uinput keyboard (needs root)
cargo test -p vox --lib -- --ignored secret_service_round_trip   # Secret Service round trip
```

## Layout

```text
Vox/
├─ catalog/            # provider metadata: aliyun.json, gemini.json, gpt.json
├─ crates/
│  ├─ vox-core/        # platform-neutral core: settings, protocol, state machine, usage
│  ├─ vox-net/         # WebSocket transport
│  ├─ vox-dsp/         # RNNoise denoise, resampling, playback ring, chunker
│  ├─ vox-osc/         # VRChat OSC (plain UDP, both platforms)
│  ├─ vox-overlay-core/# platform-neutral overlay rendering: canvas, layout, frame composition
│  ├─ vox-audio-win/   # WASAPI capture/playback, process loopback, VB-CABLE
│  ├─ vox-input-win/   # global hotkeys (GetAsyncKeyState polling)
│  ├─ vox-overlay-win/ # Win32 transparent caption window
│  ├─ vox-audio-linux/ # PipeWire capture/playback, per-program capture, virtual microphone
│  ├─ vox-audio-alsa/  # ALSA capture/playback/devices, default backend for the headless build (S4-C, under construction)
│  ├─ vox-input-linux/ # global hotkeys (evdev)
│  └─ vox-overlay-linux/# GTK transparent caption window (XWayland), swash glyph raster
├─ app/
│  ├─ src-tauri/       # Tauri layer: commands, tray, persistence, per-platform `platform/` split
│  └─ ui/              # React settings UI + browser Mock
├─ docs/               # index: docs/README.md (architecture/ platform/ protocols/ research/ plans/)
└─ README.md
```

## Data flow

**Speak:** `mic → mono → RNNoise → gate → 16 kHz PCM → provider → (captions + 24 kHz speech to the virtual mic)`

**Listen:** `program audio → mono → 16 kHz PCM → provider → (Chinese captions + 24 kHz speech to headphones)`

The virtual mic is VB-CABLE on Windows and a PipeWire sink on Linux; "program audio" is WASAPI process
loopback on Windows and PipeWire per-node capture on Linux.

Both pipelines run independently; `vox-core::Runtime` is the single source of truth.

## Contributing

Vox is deliberately conservative: core scope is fixed. External modules are the open extension point — see **[`CONTRIBUTING.md`](CONTRIBUTING.md)** — including the planned Discord out-of-process module (`docs/protocols/DISCORD_PROTOCOL.md`, second phase, undecided).

## Docs

Index: **[`docs/README.md`](docs/README.md)** — every document with a one-line summary, a status marker, and the reading order.

`docs/architecture/DIRECTIONS.md` (all directions + conflict resolution), `docs/architecture/ARCHITECTURE.md`, `docs/architecture/DECISIONS.md`, `docs/platform/SCOPE.md`, `docs/platform/LINUX.md`,
`docs/protocols/QWEN_PROTOCOL.md`, `docs/protocols/GEMINI_PROTOCOL.md`, `docs/protocols/PROVIDER_CATALOG.md`, `docs/protocols/DISCORD_PROTOCOL.md`.

## License

MIT
