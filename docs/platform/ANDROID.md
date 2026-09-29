# Android（手机档）—— 能做什么 / 做不到什么 / 怎么落地

> **来源：一轮桌面调研（2026-09-21 ～ 09-22）＋ 未实机验证。**
> 平台结论全部来自官方文档直抓（逐条带 URL）；**本仓库目前没有 Android 外壳**
> （`crates/vox-audio-android/`、`app/android/` 均未建，见 `docs/STRUCTURE.md` §2），
> 所以凡属"真机上会怎样"而没拿到一手实测的判断一律标 **[未核实]**（口径见 §6）。
> 口径与 `docs/architecture/DECISIONS.md` 一致：**代码与本文件打架时以代码为准**，然后回头把这里改对。
> 排期见 `docs/architecture/DIRECTIONS.md` §10.2 的 **S2 手机壳（Android）**；清单实例与能力位见
> `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.3.3（Android 那份清单）与 §2.5.1（位表 Android 列）。
>
> **环境阻塞（2026-09-22 实测）**：开工前需要 **Android NDK + `cargo-ndk` + `aarch64-linux-android` / `x86_64-linux-android` 两个 rust target**。
> 本机有 SDK（`~/Android/Sdk`）、`adb`、JDK 17，但**无 NDK、无 android rust target、无 `cargo-ndk`**；装 NDK 是 GB 级下载 + 系统变更，**需用户明确同意**。
> 未装前 S2 只能做不依赖 SDK 的部分（**目前为零**）。详见 `docs/architecture/DIRECTIONS.md` §10.7 的「环境阻塞」。

---

## 0. 一句话

手机版的价值是**面对面翻译**：**自己的麦克风**进 → 云端翻译 → **耳机/外放**出声 + **应用内**字幕。
"抓别人的通话当输入"在 Android 上**结构上做不到**（§2.1），"把译音灌进别的 App 的麦克风"同样做不到（§2.2）。

**做不到 ≠ 不做**：不删功能，而是**位报 `false` + 界面说"这台设备做不到"**。
位与 reason 由外壳报、界面渲染，规矩见 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.0（位的六步推导链）与 §2.6 R9（文案纪律）。

---

## 1. 能做什么

| 能力 | 手机档上的形态 | 依据 |
| --- | --- | --- |
| 对外说话（翻译） | `in: mic` → `ops`（mono → denoise → gate → resample）→ `out: playback{role: speaker}`（耳机/外放）+ `out: captions`（应用内）。**与 Windows 的 Speak 清单逐项相同，只换音频后端与外壳** | `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.3.3 |
| 听人说话 | 面对面：声音从**本机麦克风**进来（对方就在旁边）。本机抓别的 App 的声音**进不了清单**（§2.1） | 同上 §2.3.2 的 Android 行 |
| 常驻 | 不是"开机自动听"：要用户点了开始、保持 `microphone` 型前台服务、期间不锁屏不静置；**按"随时被杀"设计** | [FGS 后台启动限制](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start) |
| 控制面 | 手机没有 CLI；走 `docs/plans/S1-AGENT-FACE.md` 的本机 loopback HTTP 通道 | `docs/architecture/DIRECTIONS.md` §10.2（S1 行） |
| 界面 | 不用重写：现有 `app/ui` + `app/src-tauri` 经 Tauri v2 Android 直接编进 APK/AAB | [Tauri 开发](https://v2.tauri.app/develop/) |

---

## 2. 硬件 / 系统做不到什么

> 每一条都写成"**位报 `false(reason)` + 界面说这台设备做不到**"，不是"这条腿不存在"。

### 2.1 抓通话音频：做不到（`program_tap` 位 `false(unsupported)`）

`AudioPlaybackCaptureConfiguration` **只认** `USAGE_MEDIA` / `USAGE_GAME` / `USAGE_UNKNOWN`，**且**被采集方必须
`ALLOW_CAPTURE_BY_ALL`。通话走 `USAGE_VOICE_COMMUNICATION`，**通话期间音频"总是"被通话本身接收**；
只有**预装应用**且持有 `CAPTURE_AUDIO_OUTPUT` 才能抓上下行。

- <https://developer.android.com/media/platform/av-capture>
- <https://developer.android.com/reference/android/media/AudioPlaybackCaptureConfiguration>
- <https://developer.android.com/media/platform/sharing-audio-input>

附带限制：媒体类即便允许被录，**每次会话都要用户点同意、token 一次性**，锁屏/新会话/进程被杀即 `onStop`；
Android 14 起同一个 `MediaProjection` 只能 `createVirtualDisplay()` 一次，重复即 `SecurityException`。
（[Media projection](https://developer.android.com/media/grow/media-projection)）

### 2.2 没有虚拟麦（`virtual_mic` 位 `false(unsupported)`）

Android **没有官方的虚拟麦克风接口**；"给别的 App 造一个麦克风"要 root 或系统签名。 **[未核实]**
→ 这一格**不进 Android 的清单**；`PlaybackRole::VirtualMic` 仍是 Windows / Linux 的取值。
产品形态：**戴耳机听译音 + 屏幕字幕**。
（`docs/architecture/DIRECTIONS.md` §2.5.4；位表见 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.1）

### 2.3 跨 App 悬浮窗：要特殊权限，且穿透触摸被丢弃（`captions` 位：应用内 `true`，跨 App 另算）

- 跨 App 悬浮窗需 `SYSTEM_ALERT_WINDOW`：**特殊权限，没有运行时弹窗**，只能 `startActivity` 跳"特殊应用访问"设置页，
  回来自查 `onResume`（[特殊权限](https://developer.android.com/training/permissions/requesting-special)、
  [Play 许可做法](https://support.google.com/googleplay/android-developer/answer/16558241)）。
- **Android 12 起穿透触摸会被丢弃**：`FLAG_NOT_TOUCHABLE` 且不透明度高于阈值的窗口会遮挡下方触摸，系统直接丢弃
  （日志 `Untrusted touch due to occlusion by ...`）——桌面那套"永久穿透常驻"语义在手机上**不存在**
  （[Android 12 行为变更](https://developer.android.com/about/versions/12/behavior-changes-all)）。
- 合规替代：**应用内字幕**（复用 `crates/vox-overlay-core` 的 render/layout/canvas/geom）、分区/PiP、通知。
  读别的 App 的**文字**只有 `AccessibilityService`，且被 Play 列为受限用途并明确"不能用于远程通话录音"。

### 2.4 "开机自动听"不成立（`background_service` 位 `false(permission)`）

- 麦克风型前台服务**必须在有可见 Activity 时创建**（Android 14+ 建服务时校验 while-in-use 权限）；
  后台状态下 `checkSelfPermission` 仍可能返回 `GRANTED`，造成**误判**。
- Android 12+ 后台不能起前台服务（仅少数豁免：用户关掉电池优化、持有 `SYSTEM_ALERT_WINDOW` 等）。
- Doze 会掐网络（停网络、忽略 wake lock、推迟 alarm/job）；实时消息官方要求走 FCM 高优先级，
  而 Play 又限制滥用电池优化豁免 → **WebSocket 必须把断线重连当常态**。

出处：<https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start>、
<https://developer.android.com/training/monitoring-device-state/doze-standby>、
<https://support.google.com/googleplay/android-developer/answer/16559646>

### 2.5 位表（与 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.1 的 Android 列逐格一致）

| 位 | Android 报什么 | 界面怎么说 |
| --- | --- | --- |
| `mic` | `false(permission)`；授权 + 前台服务起来后 `true` **`[未核实]`** | "需要麦克风权限，且要开着应用（点开始）" |
| `program_tap` | `false(unsupported)` **`[未核实]`** | "这台设备抓不到别的 App 的声音（通话类系统不给抓）" |
| `virtual_mic` | `false(unsupported)` **`[未核实]`** | "这台设备不能给别的 App 当麦克风，请戴耳机听译音" |
| `captions` | **应用内** `true`；跨 App 悬浮窗要 `SYSTEM_ALERT_WINDOW`，排在实现靠后 **`[未核实]`** | 跨 App 未开 → "字幕只在 Vox 窗口里显示" |
| `global_hotkey` | `false(unsupported)` **`[未核实]`** | "这台设备没有全局热键"（区块撤下 + 一句说明；不许静默） |
| `tray` | `false(unsupported)` **`[未核实]`** | "手机没有系统托盘"（同上） |
| `background_service` | `false(permission)` **`[未核实]`** | "手机不会让它自己跑，要你点开始" |
| `vr_captions` | `false(unsupported)` | 不渲染（**未落地位**，R8） |
| `net_in` / `net_out` | `false(unsupported)`（未实现） | 不渲染（同上，R8） |
| `file_config` | `false(unsupported)` **`[未核实]`** | "配置只走界面（这台设备没有配置文件控制面）" |

---

## 3. 怎么落地

### 3.1 外壳与界面：Tauri v2 Android（省掉"原生 UI + 自搭 JNI 桥"两层）

官方支持 `tauri android init / dev / build`，现有 `app/ui` + `app/src-tauri` 与 Rust 芯可直接编入 APK/AAB。

- <https://v2.tauri.app/develop/>、<https://v2.tauri.app/distribute/google-play/>、<https://v2.tauri.app/start/prerequisites/>
- 需要 Java 能力时用 `ndk-context` 拿 `Context` / `JavaVM`，或做 Tauri 移动插件（Kotlin `Plugin` + `@Command` + Rust `PluginHandle`）：
  <https://v2.tauri.app/develop/plugins/>、<https://v2.tauri.app/develop/plugins/develop-mobile/>
- `cargo-ndk` 只在"非 Tauri 的最小壳"里需要（`cargo ndk -t arm64-v8a -o ./jniLibs build --release`）；
  UniFFI **不解决打包**，本项目收益低：<https://mozilla.github.io/uniffi-rs/latest/>

平台差异收进 `app/src-tauri/src/platform/android.rs`（`Cargo.toml` 加 `target_os = "android"` 依赖），
`platform/mod.rs` 那张对照表加一列 —— 与现有 `win.rs` / `linux/` 同一套做法。

### 3.2 音频：Oboe，显式 48 kHz 单声道 f32

- **采集与播放都走 Oboe**（API 16+ 自动在 OpenSL ES / AAudio 之间选；Rust 有 `oboe` crate）：
  <https://raw.githubusercontent.com/google/oboe/main/docs/FullGuide.md>、<https://lib.rs/crates/oboe>
- **显式请求 48000 Hz 单声道 f32**：AAudio 文档明确"显式指定的采样率 / 格式 / 每帧样本数不会被改"
  （<https://developer.android.com/ndk/guides/audio/aaudio/aaudio>）。这样
  `crates/vox-dsp/src/denoise.rs` 的 `NATIVE_SAMPLE_RATE = 48_000` / 480 帧**原生长度直接成立**：
  20 ms = 960 样本 = 正好 2 帧（`block_ms = 20` 见 `crates/vox-core/src/pipeline/mod.rs` 的 `INPUT_BLOCK_MS`）。
- **别追 LowLatency，该用 `PowerSaving` 大缓冲**——真延迟在云端。20 ms 块长已属保守；48k 下 HAL burst 典型 96/128/160/192/240/256/512 帧
  （[音频延迟](https://developer.android.com/ndk/guides/audio/audio-latency)）。
- 播放用 `USAGE_MEDIA` + `CONTENT_TYPE_SPEECH`。
- **线程规矩照抄 `crates/vox-audio-linux`**：实时回调只往无锁队列搬数据，普通线程再调 `on_chunk`
  （Linux 侧注释已写明：`on_chunk` 会加锁 + 分配，故意不设 RT 优先级）。块长由 `vox_dsp::chunk::Blocker` 切，与 HAL burst 解耦。
- 协商参考 `PROPERTY_OUTPUT_SAMPLE_RATE` / `PROPERTY_OUTPUT_FRAMES_PER_BUFFER`（API 34 起有 Oboe `getHardwareSampleRate()`）；
  **注册 `AudioRecordingCallback` 处理路由/采样率变化**。

### 3.3 生命周期：`microphone` 型前台服务

三件套：`microphone` 型前台服务 + `FOREGROUND_SERVICE_MICROPHONE` + `RECORD_AUDIO`；Android 13+ 再加 `POST_NOTIFICATIONS`
（未授权时前台服务通知**不进抽屉**，用户会在任务管理器里看到并直接停掉服务）。

- <https://developer.android.com/develop/background-work/services/fgs/service-types>
- <https://developer.android.com/develop/ui/views/notifications/notification-permission>

规矩：**只能在有可见 Activity 时 `startForegroundService`**（后台建即 `SecurityException`）；
**按"随时被杀"设计**：状态落 `filesDir`，重连即恢复。

### 3.4 权限

| 权限 | 用途 | 性质 |
| --- | --- | --- |
| `RECORD_AUDIO` | 麦克风采集 | 运行时权限 |
| `FOREGROUND_SERVICE_MICROPHONE` + `microphone` 服务类型 | 前台服务 | manifest 声明 + 启动校验（§2.4） |
| `POST_NOTIFICATIONS` | 前台服务通知可见 | 运行时权限（Android 13+） |
| `SYSTEM_ALERT_WINDOW` | 跨 App 悬浮窗 | **特殊权限**：无弹窗，只能跳设置页（§2.3） |
| `CAPTURE_AUDIO_OUTPUT` | 抓系统音频 | **普通 App 申请不到**（预装/系统签名），所以通话抓不到（§2.1） |

### 3.5 密钥：Keystore（不是 DPAPI 的等价物）

Keystore 的语义与 Windows DPAPI **不同**：密钥**不可导出**，须用 `KeyGenParameterSpec` 指定用途（AES/GCM），
可选 `setUserAuthenticationRequired`；落盘时用"别名 + AES-GCM"包一层。
（<https://developer.android.com/privacy-and-security/keystore>）

仓库现状（`app/src-tauri/src/sys/secrets.rs` 的 `DpapiSecretStore`、`platform/linux/secrets.rs` 的 keyring 实现）里，
`keyring` crate **没有 Android 后端** → 新增 `vox-secrets-android`（实现 `crates/vox-core/src/ports.rs` 的 `SecretStore`），
`sys/secrets.rs` 按 `target` 分流。结构照 `DpapiSecretStore`：按 provider 分文件、原子写（tmp + rename）、固定别名。

### 3.6 配置与状态

- 无界面配置 = `filesDir` 下的 `settings.json`（[应用专属存储](https://developer.android.com/training/data-storage/app-specific)）
  + **Intent extra** 传入（`adb shell am start --es ...`，走 Tauri 的 `onNewIntent`）。
- **不要开 localhost 端口做控制面**：官方安全清单明确这类接口**对同机其它应用可达**，敏感 IPC 应走 Service/Binder
  （<https://developer.android.com/privacy-and-security/security-tips>）。S1 那个本机 HTTP 是给 Agent 用的，不是给同机 App 用的。

### 3.7 打包、ABI 与 CI

- ABI **保 `arm64-v8a`**（Play 64 位必需）；`--split-per-abi` 或 AAB 控体积（<https://developer.android.com/ndk/guides/abis>）。
- **16 KB 页**：NDK 库须重编并对齐，r27 及以下还要 RELRO 参数（<https://developer.android.com/guide/practices/page-sizes>）。
- CI：`.github/workflows/release.yml` 增 android job（`tauri android build --aab --target aarch64`，签名走 secret；
  APK 另出一份供直发；补 `NDK_HOME` / `ANDROID_HOME` / `JAVA_HOME`）。

### 3.8 分发与审核

- **直发 APK**：无审核、不需要 FGS 申报；但用户要手动允许未知来源、无分 ABI 优化。
- **Play**：出 `--aab`；FGS 需在 App content 申报类型 + 描述 + 用户影响 + **演示视频**
  （<https://support.google.com/googleplay/android-developer/answer/13392821>）。
  政策要求 FGS 由**用户发起 / 可感知 / 用户可停 / 不可被系统推迟 / 只跑必要时间**；
  `TYPE_MICROPHONE` 的官方用例是"后台音频访问（如语音助手、不保存）"
  （<https://support.google.com/googleplay/android-developer/answer/16558241>）。
  **后台麦克风不是政策禁止项**，但"麦克风 + 悬浮窗 + MediaProjection"三项叠加会抬高审核压力
  → **首发只申请麦克风**。
- 目标 API 时间线：2026-08-31 起新提交须 target Android 16（36）（<https://developer.android.com/google/play/requirements/target-sdk>）；
  2027-02-01 起 target 15+ 的更新必须支持 16 KB 页（<https://developer.android.com/guide/practices/page-sizes>）。

### 3.9 这一档要新写的代码（仓库动作，尚未执行）

| # | 动作 | 说明 |
| --- | --- | --- |
| 1 | 新增 `crates/vox-audio-android/` | 照 `crates/vox-audio-linux/src/{lib,capture,playback,registry}.rs` 的形状实现 `vox_core::ports` 的 `CaptureSource` / `PlaybackSink` / `DeviceRegistry`；实时回调只搬数据；错误翻成中文 `PortError`，**永不 panic** |
| 2 | `app/src-tauri/src/platform/android.rs` + `platform/mod.rs` 加列 | 把时钟 / 密钥库 / 音频三件套 / 悬浮字幕 / 虚拟麦 的 Android 事实写进那张对照表，虚拟麦恒 `not_applicable` |
| 3 | Keystore 版 `SecretStore`（§3.5） | `sys/secrets.rs` 按 target 分流 |
| 4 | `crates/vox-core/src/ports.rs` 的 `CaptureTarget` 小改 | `CaptureTarget` 里"抓别的程序"这一格只有 `ProcessLoopback{executable, include_tree}`；Android **无法按可执行文件定位**（没有公开的"正在放音的进程"API）→ 加平台中立变体（如 `SystemPlayback{usages, exclude_self}`）；`DeviceRegistry::audio_apps()` 在 Android 返回空表，界面按空表隐藏选择器（同 `virtual_cable_installed` 恒 false 的套路） |
| 5 | 第一个 Tauri 移动插件（`plugin new android-service --android`） | Kotlin 侧起 `microphone` 型前台服务并封装 `RECORD_AUDIO` / `POST_NOTIFICATIONS` / `SYSTEM_ALERT_WINDOW` 三个请求；Rust 侧用 `PluginHandle` 调，别让"前台服务 + 权限"污染芯 |
| 6 | 字幕先复用 `crates/vox-overlay-core` 画在**应用内** | 在 `app/src-tauri/src/overlay.rs` 加 Android 分支；"跨 App 悬浮字幕"标 Phase 2 |

---

## 4. 坑与限制（为什么 + 出处）

| 坑 | 为什么 | 出处 |
| --- | --- | --- |
| 后台建麦克风前台服务直接抛异常 | Android 14+ 建服务时校验 while-in-use 权限；后台时该权限不在手，而 `checkSelfPermission` 仍返回 `GRANTED` → 误判 | [FGS 后台启动限制](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start) |
| Android 12+ 后台不能起前台服务 | 少数豁免（用户关电池优化、持有 `SYSTEM_ALERT_WINDOW`）→ "开机自动听"不成立，必须**用户手势**触发 | 同上 |
| Doze 掐网络 | 停网络、忽略 wake lock、推迟 alarm/job；官方要求实时消息走 FCM 高优先级，Play 又不许滥用豁免 → 断线重连是常态 | [Doze](https://developer.android.com/training/monitoring-device-state/doze-standby)、[Play 政策](https://support.google.com/googleplay/android-developer/answer/16559646) |
| 通话音频抓不到 | capture 配置要求 usage ∈ {MEDIA, GAME, UNKNOWN} 且 `ALLOW_CAPTURE_BY_ALL`；通话场景"总是由通话接收" | [av-capture](https://developer.android.com/media/platform/av-capture)、[AudioPlaybackCaptureConfiguration](https://developer.android.com/reference/android/media/AudioPlaybackCaptureConfiguration)、[共享音频输入](https://developer.android.com/media/platform/sharing-audio-input) |
| 两个普通应用不能同时录音 | `VOICE_COMMUNICATION` / `CAMCORDER` 属 privacy-sensitive，优先级更高：用户一边微信语音一边开我们会收到**静音** → 须监听 `isClientSilenced` 并提示 | [共享音频输入](https://developer.android.com/media/platform/sharing-audio-input) |
| 全局麦克风开关可整体静音 | Android 12 起用户可按快捷开关关掉整机麦克风，我们收到静音（并有状态栏隐私指示器） | [解释权限访问](https://developer.android.com/training/permissions/explaining-access) |
| 穿透悬浮窗触摸被拦 | Android 12 起 `FLAG_NOT_TOUCHABLE` 且不透明度高于阈值的窗口遮挡下方触摸 → 系统直接丢弃 | [Android 12 行为变更](https://developer.android.com/about/versions/12/behavior-changes-all) |
| 特殊权限无运行时弹窗 | 只能跳"特殊应用访问"页，回来自查 `onResume` | [特殊权限](https://developer.android.com/training/permissions/requesting-special) |
| MediaProjection 授权不可持久 | 每次会话都要用户点同意、token 一次用完；锁屏/另一会话开始/进程被杀都会 `onStop`；Android 14+ 同一 projection 只能 `createVirtualDisplay()` 一次 | [Media projection](https://developer.android.com/media/grow/media-projection) |
| Oboe/AAudio 流随时断开 | 插拔耳机、路由变化、设备不再是主设备都会断开 → **必须注册 error callback 重建流**；回调内禁止分配 / 加锁 / sleep / 对自身流 read-write | [Oboe FullGuide](https://raw.githubusercontent.com/google/oboe/main/docs/FullGuide.md)、[AAudio](https://developer.android.com/ndk/guides/audio/aaudio/aaudio) |
| 16 KB 页 + 目标 API 两条硬门槛 | 2027-02-01 起 target 15+ 的更新必须支持 16 KB 页；2026-08-31 起新提交须 target 36 | [页大小](https://developer.android.com/guide/practices/page-sizes)、[目标 API](https://developer.android.com/google/play/requirements/target-sdk) |
| 别开 localhost 端口做控制面 | 这层接口**对同机其它应用可达**；敏感 IPC 应走 Service/Binder | [安全清单](https://developer.android.com/privacy-and-security/security-tips) |
| 通知权限影响前台服务可见性 | Android 13+ 未授 `POST_NOTIFICATIONS` 时前台服务通知不进抽屉 | [通知权限](https://developer.android.com/develop/ui/views/notifications/notification-permission) |
| `AccessibilityService` 不是读字幕的后门 | Play 把它列为受限用途，并明确"不能用于远程通话录音" | [Play 政策](https://support.google.com/googleplay/android-developer/answer/16558241) |

---

## 5. 待验证项（**全部未实机验证**）

以下都是**设计输入，不是实测**；S2 真机时必须回来改 §2.5 那张位表。 **[未核实]**

1. **RNNoise 在具体 ARM64 机型的 CPU / 耗电**，以及 `rubato` sinc 48k→16k 的成本：只拿到算法侧一手材料
   （[xiph/rnnoise README](https://raw.githubusercontent.com/xiph/rnnoise/master/README) 指向 MMSP 2018 论文 + 交互 demo），
   **无机型实测数字**。建议按 `docs/architecture/DIRECTIONS.md` §3.3 路线五（开机自测 + 算子成本表）做自测，
   并把"便宜重采样档"列为 Android 必须项（该路线记着"重采样只有贵的 sinc 那一档"）。
2. **Tauri v2 现有依赖在 Android 的支持矩阵**：`tauri-plugin-autostart` / `updater` / `single-instance` / `tray-icon`
   在手机上大概率不可用或无语义，需 `cfg` 门控；落地第一天就实测。
3. **`panic = "abort"` + `cdylib` 在 Tauri Android 构建下的实际表现**（是否要在移动端关掉 `panic=abort`，以免 panic 跨 FFI 炸进程）。
4. **悬浮窗 + 麦克风前台服务同时申报时的 Play 通过率**：政策文本只给条件不给概率，无公开一手数据。
5. **Android 上读别的 App 字幕/文本除 `AccessibilityService` 外是否有其它合规路径**：只查到政策侧禁止性描述，**没有许可性路径**。
6. **§2.5 位表里所有标 `[未核实]` 的格子**（`mic` 授权后的真实值、`program_tap` / `virtual_mic` / `global_hotkey` / `tray` / `file_config` 的 `unsupported` 判定）。
7. 调研方法注记：本机 `web_search` 被搜索引擎挡，全程直抓 URL；Tauri 移动插件文档的
   `/develop/plugins/develop-mobile-plugin/` **404**，实际路径是 `/develop/plugins/develop-mobile/`。

---

## 6. 标注与引用口径

- **[未核实]**：没有一手出处、或属于"真机上会怎样"但**未实机验证**的条目。
- 本文件所有外部事实都带 URL；仓库内事实带 `path`（符号名优先，行号只在必要处）。
- **不改事实内容**：若发现本文件某条已被新证据推翻，**先加状态头/标注**，别直接改写结论（`docs/STRUCTURE.md` §3）。
- 位与 reason 的词汇表是 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.1 / §2.5.2（`UnavailableReason`）；
  界面文案只进 `app/ui/src/i18n/*`，**不进芯**（同文件 §2.6 R3）。
