# Vox

Ein Desktop-Übersetzer für Echtzeit-Sprache in Live-Gesprächen (**Windows** und **Linux**). Zwei unabhängige Pipelines laufen gleichzeitig:

| Pipeline | Eingang | Ausgang |
| --- | --- | --- |
| **Speak out** | dein Mikrofon | übersetzte Sprache in ein virtuelles Mikrofon + Live-Untertitel |
| **Listen in** | Audio eines gewählten Programms | chinesische Sprache zu deinen Kopfhörern + Live-Untertitel |

Basiert auf **Tauri 2 + React 19 + Rust**. Die UI steuert Konfiguration und Status; Audioaufnahme, Rauschunterdrückung, Resampling, WebSocket-Transport, Hotkeys und das Overlay-Untertitelfenster liegen in Rust, aufgeteilt in plattformspezifische Shells (Windows: `vox-*-win`; Linux: `vox-*-linux`). Der Kern (`vox-core` / `vox-net` / `vox-dsp`) ist plattformunabhängig.

English： [`../README.md`](../README.md) · 简体中文： [`zh-CN.md`](zh-CN.md) · 日本語： [`ja.md`](ja.md) · 한국어： [`ko.md`](ko.md) · Español： [`es.md`](es.md) · Français： [`fr.md`](fr.md) · Deutsch： [`de.md`](de.md)

## Umfang

- **Windows**: Prozess-Loopback („Listen in") benötigt **Win11 / Server 2022 (Build 20348+)**.
- **Linux**: **PipeWire** erforderlich (Standard bei gängigen Distributionen). „Listen in" erfasst die Audiostreams des Zielprogramms direkt; das virtuelle Mikrofon ist ein PipeWire-Sink (nichts zu installieren); in Wayland-Sitzungen läuft das Overlay über XWayland, um sich zu positionieren und oben zu bleiben; globale Hotkeys werden aus `/dev/input` (evdev) gelesen. Andere Audio-Stacks als PipeWire werden nicht unterstützt.
- Anbieter — **Alibaba Cloud Bailian**, **Google Gemini**, **OpenAI Realtime** — pro Pipeline wählbar, jeweils ein festes Echtzeit-Übersetzungsmodell.
- „Listen in" übersetzt immer **ins Chinesische**; die Quellsprache wird automatisch erkannt oder manuell festgelegt.
- UI-Sprache: 简体中文 / 日本語 / English, unabhängig von den Übersetzungssprachen.
- API-Schlüssel der Anbieter werden lokal gespeichert und pro Benutzer verschlüsselt: **DPAPI** unter Windows, **Secret Service** (gnome-keyring / KWallet, via `keyring`) unter Linux.
- Anbieter-Metadaten liegen in [`catalog/*.json`](../catalog/), nicht im Quellcode.

## Entwickeln

Voraussetzungen: Windows 11 x64, Node.js `^20.19.0` oder `>=22.12.0`, Rust stable (`x86_64-pc-windows-msvc`), VS Build Tools (C++ Desktop), WebView2 sowie ein API-Schlüssel (oder das Frontend-Mock, das keines benötigt).

```powershell
cd app\ui
npm ci
npm run tauri:dev        # komplette Desktop-App
npm run dev              # nur UI; öffne http://127.0.0.1:5183/?mock=1
```

## Installer erstellen

```powershell
cd app\ui
npm run tauri:build      # erzeugt NSIS unter target/release/bundle/nsis/
```

Für Releases nicht `cargo build --release` verwenden; beim manuellen Bauen des Binaries das Feature für das Custom-Protocol mitgeben:

```powershell
cargo build --release -p vox --features custom-protocol
```

## Testen

```powershell
cargo test --workspace   # Rust, Repository-Wurzel
npm run verify           # in app/ui: Typ-Check, Prod-Build, a11y/disabled-Prüfung
```

## Struktur

```text
Vox/
├─ catalog/            # Anbieter-Metadaten: aliyun.json, gemini.json, gpt.json
├─ crates/
│  ├─ vox-core/        # plattformneutraler Kern: Einstellungen, Protokoll, Zustandsmaschine, Verbrauch
│  ├─ vox-net/         # WebSocket-Transport
│  ├─ vox-dsp/         # RNNoise-Entrauschung + Resampling
│  ├─ vox-audio-win/   # WASAPI-Aufnahme/Wiedergabe, Prozess-Loopback, VB-CABLE
│  ├─ vox-input-win/   # globale Hotkeys
│  └─ vox-overlay-win/ # transparentes Win32-Untertitel-Fenster
├─ app/
│  ├─ src-tauri/       # Tauri-Ebene: Befehle, Tray, Persistenz, DPAPI
│  └─ ui/              # React-Einstellungs-UI + Browser-Mock
├─ docs/               # index: docs/README.md (architecture/ platform/ protocols/ research/ plans/)
└─ README.md
```

## Datenfluss

**Speak:** `mic → mono → RNNoise → gate → 16 kHz PCM → provider → (Untertitel + 24 kHz Sprache zu VB-CABLE)`

**Listen:** `loopback → mono → 16 kHz PCM → provider → (chinesische Untertitel + 24 kHz Sprache zu Kopfhörern)`

Beide Pipelines laufen unabhängig; `vox-core::Runtime` ist die einzige Wahrheitsquelle.

## Beitragen

Vox ist bewusst konservativ: der Kern-Umfang ist fest. Externe Module sind der offene Erweiterungspunkt — siehe **[`../CONTRIBUTING.md`](../CONTRIBUTING.md)** — einschließlich des geplanten externen Discord-Moduls (`docs/protocols/DISCORD_PROTOCOL.md`, Phase 2 · unentschieden).

## Doku

`docs/architecture/ARCHITECTURE.md`, `docs/architecture/DECISIONS.md`, `docs/platform/SCOPE.md`, `docs/platform/LINUX.md`, `docs/protocols/QWEN_PROTOCOL.md`, `docs/protocols/GEMINI_PROTOCOL.md`, `docs/protocols/PROVIDER_CATALOG.md`, `docs/protocols/DISCORD_PROTOCOL.md`.

## Lizenz

MIT
