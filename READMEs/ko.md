# Vox

라이브 대화를 위한 데스크톱 실시간 음성 번역기(**Windows** / **Linux**). 두 개의 독립적인 파이프라인이 동시에 실행됩니다:

| 파이프라인 | 입력 | 출력 |
| --- | --- | --- |
| **Speak out** | 내 마이크 | 가상 마이크로 번역 음성 + 라이브 자막 |
| **Listen in** | 선택한 프로그램의 오디오 | 헤드폰으로 중국어 음성 + 라이브 자막 |

**Tauri 2 + React 19 + Rust**로 제작되었습니다. UI는 설정과 상태를 담당하고, 오디오 캡처·노이즈 제거·리샘플링·WebSocket 전송·단축키·자막 오버레이 창은 Rust로 구현되어 있으며 플랫폼별 셸(Windows: `vox-*-win`, Linux: `vox-*-linux`)로 나뉩니다. 코어(`vox-core` / `vox-net` / `vox-dsp`)는 플랫폼 비의존입니다.

English： [`../README.md`](../README.md) · 简体中文： [`zh-CN.md`](zh-CN.md) · 日本語： [`ja.md`](ja.md) · 한국어： [`ko.md`](ko.md) · Español： [`es.md`](es.md) · Français： [`fr.md`](fr.md) · Deutsch： [`de.md`](de.md)

## 범위

- **Windows**: 프로세스 루프백("Listen in")은 **Win11 / Server 2022(build 20348+)**가 필요.
- **Linux**: **PipeWire**가 필요합니다(주요 배포판 기본). "Listen in"은 대상 프로그램의 오디오 스트림을 직접 캡처합니다. 가상 마이크는 PipeWire sink 자체(설치 불필요). Wayland 세션에서는 위치 지정과 항상 위 표시를 위해 오버레이가 XWayland로 동작합니다. 전역 단축키는 `/dev/input`(evdev)을 직접 읽습니다. PipeWire 이외의 오디오 스택은 지원하지 않습니다.
- 프로바이더 — **Alibaba Cloud Bailian**, **Google Gemini**, **OpenAI Realtime** — 파이프라인별로 선택 가능하며, 각각 고정된 실시간 번역 모델 1개를 사용.
- "Listen in"은 항상 **중국어로** 번역. 원본 언어는 자동 감지 또는 수동 지정.
- UI 언어는 简体中文 / 日本語 / English이며, 번역 언어와는 독립적.
- 프로바이더 API 키는 로컬에 저장되고 사용자별로 암호화됩니다: Windows는 **DPAPI**, Linux는 **Secret Service**(gnome-keyring / KWallet, `keyring` 사용).
- 프로바이더 메타데이터는 [`catalog/*.json`](../catalog/)에 있으며 소스 코드에는 포함하지 않음.

## 개발 (Develop)

사전 요구: Windows 11 x64, Node.js `^20.19.0` 또는 `>=22.12.0`, Rust stable(`x86_64-pc-windows-msvc`), VS Build Tools(C++ 데스크톱), WebView2, 그리고 API 키(또는 프론트엔드 Mock, 이 경우 불필요).

```powershell
cd app\ui
npm ci
npm run tauri:dev        # 데스크톱 앱 전체
npm run dev              # UI만, http://127.0.0.1:5183/?mock=1 열기
```

## 인스톨러 빌드

```powershell
cd app\ui
npm run tauri:build      # NSIS를 target/release/bundle/nsis/에 생성
```

배포에 `cargo build --release`는 사용하지 마세요. 바이너리를 수동으로 빌드할 때는 custom protocol 기능을 전달하세요:

```powershell
cargo build --release -p vox --features custom-protocol
```

## 테스트

```powershell
cargo test --workspace   # Rust, 저장소 루트
npm run verify           # app/ui 내: 타입 체크, 프로덕션 빌드, a11y/disabled 검사
```

## 디렉터리 구조

```text
Vox/
├─ catalog/            # 프로바이더 메타데이터: aliyun.json, gemini.json, gpt.json
├─ crates/
│  ├─ vox-core/        # 플랫폼 중립 코어: 설정, 프로토콜, 상태 기계, 사용량
│  ├─ vox-net/         # WebSocket 전송
│  ├─ vox-dsp/         # RNNoise 노이즈 제거 + 리샘플링
│  ├─ vox-audio-win/   # WASAPI 캡처/재생, 프로세스 루프백, VB-CABLE
│  ├─ vox-input-win/   # 전역 단축키
│  └─ vox-overlay-win/ # Win32 투명 자막 창
├─ app/
│  ├─ src-tauri/       # Tauri 계층: 명령, 트레이, 영속화, DPAPI
│  └─ ui/              # React 설정 UI + 브라우저 Mock
├─ docs/               # index: docs/README.md (architecture/ platform/ protocols/ research/ plans/)
└─ README.md
```

## 데이터 흐름

**Speak:** `mic → mono → RNNoise → gate → 16 kHz PCM → provider → (자막 + 24 kHz 음성을 VB-CABLE로)`

**Listen:** `loopback → mono → 16 kHz PCM → provider → (중국어 자막 + 24 kHz 음성을 헤드폰으로)`

두 파이프라인은 독립적으로 실행되며, `vox-core::Runtime`이 유일한 진실의 원천입니다.

## 기여 (Contributing)

Vox는 의도적으로 보수적으로 유지되며, 핵심 범위는 고정되어 있습니다. 외부 모듈이 열린 확장 지점입니다 — **[`../CONTRIBUTING.md`](../CONTRIBUTING.md)**를 참고하세요 — 계획된 Discord 아웃오브프로세스 모듈(`docs/protocols/DISCORD_PROTOCOL.md`, 2단계·미정)도 여기에 포함됩니다.

## 문서

`docs/architecture/ARCHITECTURE.md`, `docs/architecture/DECISIONS.md`, `docs/platform/SCOPE.md`, `docs/platform/LINUX.md`, `docs/protocols/QWEN_PROTOCOL.md`, `docs/protocols/GEMINI_PROTOCOL.md`, `docs/protocols/PROVIDER_CATALOG.md`, `docs/protocols/DISCORD_PROTOCOL.md`.

## 라이선스

MIT
