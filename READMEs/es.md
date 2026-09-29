# Vox

Un traductor de voz en tiempo real de escritorio (**Windows** y **Linux**), pensado para conversaciones en vivo. Dos pipelines independientes corren a la vez:

| Pipeline | Entrada | Salida |
| --- | --- | --- |
| **Speak out** | tu micrófono | voz traducida a un micrófono virtual + subtítulos en vivo |
| **Listen in** | el audio de un programa elegido | voz en chino a tus auriculares + subtítulos en vivo |

Construido con **Tauri 2 + React 19 + Rust**. La UI maneja configuración y estado; la captura de audio, reducción de ruido, remuestreo, transporte WebSocket, atajos de teclado y la ventana flotante de subtítulos viven en Rust, repartidos en shells por plataforma (Windows: `vox-*-win`; Linux: `vox-*-linux`). El núcleo (`vox-core` / `vox-net` / `vox-dsp`) es neutral respecto a la plataforma.

English： [`../README.md`](../README.md) · 简体中文： [`zh-CN.md`](zh-CN.md) · 日本語： [`ja.md`](ja.md) · 한국어： [`ko.md`](ko.md) · Español： [`es.md`](es.md) · Français： [`fr.md`](fr.md) · Deutsch： [`de.md`](de.md)

## Alcance

- **Windows**: el loopback por proceso ("Listen in") necesita **Win11 / Server 2022 (build 20348+)**.
- **Linux**: requiere **PipeWire** (predeterminado en las distros habituales). "Listen in" captura los flujos de audio del propio programa; el micrófono virtual es un sink de PipeWire (no hay nada que instalar); en sesiones Wayland la ventana flotante usa XWayland para posicionarse y quedar encima; los atajos globales se leen de `/dev/input` (evdev). No se admiten otras pila de audio distintas de PipeWire.
- Proveedores — **Alibaba Cloud Bailian**, **Google Gemini**, **OpenAI Realtime** — seleccionables por pipeline, cada uno con un modelo fijo de traducción en tiempo real.
- "Listen in" siempre traduce **al chino**; el idioma de origen se detecta automáticamente o se fija a mano.
- Idioma de la UI: 简体中文 / 日本語 / English, independiente de los idiomas de traducción.
- Las API keys de los proveedores se guardan localmente y se cifran por usuario: **DPAPI** en Windows, **Secret Service** (gnome-keyring / KWallet, vía `keyring`) en Linux.
- Los metadatos de proveedores viven en [`catalog/*.json`](../catalog/), no en el código fuente.

## Desarrollar

Requisitos: Windows 11 x64, Node.js `^20.19.0` o `>=22.12.0`, Rust stable (`x86_64-pc-windows-msvc`), VS Build Tools (C++ desktop), WebView2 y una clave de API (o el Mock del frontend, que no necesita ninguna).

```powershell
cd app\ui
npm ci
npm run tauri:dev        # app de escritorio completa
npm run dev              # solo UI, abre http://127.0.0.1:5183/?mock=1
```

## Compilar el instalador

```powershell
cd app\ui
npm run tauri:build      # genera NSIS en target/release/bundle/nsis/
```

No uses `cargo build --release` para distribución; si construyes el binario a mano, pasa la feature custom-protocol:

```powershell
cargo build --release -p vox --features custom-protocol
```

## Probar

```powershell
cargo test --workspace   # Rust, raíz del repo
npm run verify           # en app/ui: type-check, build de producción, chequeos a11y/disabled
```

## Estructura

```text
Vox/
├─ catalog/            # metadatos de proveedores: aliyun.json, gemini.json, gpt.json
├─ crates/
│  ├─ vox-core/        # núcleo neutral de plataforma: ajustes, protocolo, máquina de estados, uso
│  ├─ vox-net/         # transporte WebSocket
│  ├─ vox-dsp/         # denoise RNNoise + remuestreo
│  ├─ vox-audio-win/   # WASAPI captura/reproducción, loopback por proceso, VB-CABLE
│  ├─ vox-input-win/   # atajos globales
│  └─ vox-overlay-win/ # ventana Win32 transparente de subtítulos
├─ app/
│  ├─ src-tauri/       # capa Tauri: comandos, bandeja, persistencia, DPAPI
│  └─ ui/              # UI de ajustes en React + Mock del navegador
├─ docs/               # index: docs/README.md (architecture/ platform/ protocols/ research/ plans/)
└─ README.md
```

## Flujo de datos

**Speak:** `mic → mono → RNNoise → gate → 16 kHz PCM → provider → (subtítulos + voz 24 kHz a VB-CABLE)`

**Listen:** `loopback → mono → 16 kHz PCM → provider → (subtítulos en chino + voz 24 kHz a auriculares)`

Ambos pipelines corren de forma independiente; `vox-core::Runtime` es la única fuente de verdad.

## Contribuir

Vox es deliberadamente conservador: el alcance del núcleo es fijo. Los módulos externos son el punto de extensión abierto — ver **[`../CONTRIBUTING.md`](../CONTRIBUTING.md)** — incluido el planeado módulo Discord fuera de proceso (`docs/protocols/DISCORD_PROTOCOL.md`, segunda fase, sin decidir).

## Documentación

`docs/architecture/ARCHITECTURE.md`, `docs/architecture/DECISIONS.md`, `docs/platform/SCOPE.md`, `docs/platform/LINUX.md`, `docs/protocols/QWEN_PROTOCOL.md`, `docs/protocols/GEMINI_PROTOCOL.md`, `docs/protocols/PROVIDER_CATALOG.md`, `docs/protocols/DISCORD_PROTOCOL.md`.

## Licencia

MIT
