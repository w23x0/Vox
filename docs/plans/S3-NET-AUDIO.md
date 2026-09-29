# S3 设计稿：网络音频进出（`net_in` / `net_out`）

> 状态：**设计稿·第 1 版**（2026-09-22，第二十一轮）＋ **第二十三轮回填**（帧层落地后的三处偏离已并回本文，见 §0.3）
> 对应施工阶段 **S3 嵌入式（小主板）**（`docs/architecture/DIRECTIONS.md` §10.2）
> 本稿**自己不含实现代码**；实现按 §3 的施工单在 `crates/` / `app/` 落地，进度见下。
> 引用口径照 `.omp/AGENTS.md`「引用要耐久」：**优先符号名，行号是写稿当时的**。
> 上游必读：`docs/plans/S0-COMPOSITION-MANIFEST.md`（清单 + 能力位）、`docs/plans/S1-AGENT-FACE.md`（控制面）、
> `docs/platform/EMBEDDED.md`（无屏档）、`docs/architecture/DIRECTIONS.md` §3.3（五条技术路线）与 §10.2（S3 行）。

---

## 实现进度（截至 2026-09-22，第二十三轮）

| 块 | 状态 | 证据（本轮亲自核过，出处见 §1.6） |
| --- | --- | --- |
| **帧层**：`crates/vox-net/src/media/{mod,frame,pipe,server,client}.rs`、`crates/vox-net/examples/media_probe.rs`、`crates/vox-net/tests/media_pipe.rs` | **已落地** | 用例 **23 条** = 集成 13 条（`tests/media_pipe.rs`：`#[test]` 5 + `#[tokio::test]` 8）+ 单测 10 条（`media/frame.rs` 5、`media/pipe.rs` 5） |
| `CaptureTarget::Net { pipe }` 与全部调用方迁移 | **已落地** | `crates/vox-core/src/ports.rs` 的变体；四个穷尽 `match` 点已迁：`vox-audio-linux` 的 `capture.rs::resolve_plan` 与 `examples/smoke.rs`、`vox-audio-win` 的 `capture/mod.rs`（两处） |
| `vox-net` 的依赖边与模块门面 | **已落地** | `crates/vox-net/Cargo.toml`：描述已改、`tokio` 显式 `features = ["net"]`、`parking_lot.workspace = true`；`crates/vox-net/src/lib.rs` 已加 `pub mod media;` |
| **S3 后半**：`Settings.net` / `NetSettings`、清单派生（`listen.rs` / `speak.rs`）、`Plan.net_out` / `Deps.net_out`、`runtime.rs` 两格与守卫、无屏外壳装配（`vox-headless/src/media.rs`、`media.json`）、`SecretStore` 的键值式 API、UI 的 `disabled` reason | **未做** | `grep -rn "NetSettings\|settings\.net\|net_out:\|net_in:" crates` → **零命中**；`crates/vox-headless/src/platform/linux.rs::host_facts()` 今天仍只报 `background_service: not_wired` 一位 |

帧层落地时对本稿有**三处偏离**（`KeepAlivePayload`、入站侧也发保活、`flush_done` 的时机）。
**实现已按偏离后的口径落地**，本文已回填：§0.3 逐条列，正文的 §2.1.3 / §2.4.3 / §2.6.2 已改成落地后的样子。

---

## 0. 本稿的边界

### 0.1 做什么 / 不做什么

**做**（S3 里唯一还没落地的那一件）：

1. **媒体面协议与帧格式**：网络音频怎么封、怎么重切块、抖动缓冲与欠载/过载怎么办（§2.1、§2.6）。
2. **两条腿的网络化**：无屏档「听人说话」的输入换成 `net_in`、「对外说话」的输出换成 `net_out`（§2.2、§2.3）。
3. **能力位从"恒假"变成"有定义者"**：`Capability::NetIn` / `NetOut` 进 `host_ceiling(HostKind::LinuxHeadless)`，
   定义者与凭据写死（§2.2.3）。
4. **心跳来源**：确认 `DIRECTIONS.md` §3.3 路线四"上游管子叫醒"，并给出落法（§2.4）。
5. **安全**：监听地址、凭据、Origin，以及与 `Settings.control` 的边界（§2.5）。

**不做**（写下来防止被顺手做掉）：

| 不做 | 为什么 |
| --- | --- |
| **不加第三条腿**（纯中继 `net_in → net_out`、`session: null` 的产品入口） | 中继腿没有云端会话、没有阀门、没有激活方式，塞进现有两条腿会把"按一下热键才说话"这套桌面语义带到一个没有键盘的盒子上（`DIRECTIONS.md` §3.2 第 6 条）。S0 §3.4 已明确"加第三条腿是清单落地**之后**的事"。本稿只保证：`Composition::endpoint()` 能派生出合法作业单（§4.3 有用例），**产品入口留到那一轮**。 |
| 桌面档 / Android 档的媒体面 | 上限不加这两位（§2.2.3 的理由），`app/src-tauri` 一行不改。 |
| `host_feed` / `host_sink`（同进程插件宿主） | 插件宿主排在 `DIRECTIONS.md` §10.2 的"本次不承诺"。本稿只把**共用点**写清（§2.2.2），实现归那一轮。 |
| `wss://` **服务端**（TLS 监听） | 要证书分发，是独立话题；见 §5 的已知代价与两条绕法。出站侧 `wss://` 沿用现有 rustls。 |
| WebRTC | 见 §2.1 的选型结论与代价。 |
| Opus / 任何编解码 | 帧头留了 `kind` 位（§2.1.3），v1 只认 PCM16LE。 |
| 控制面暴露媒体面配置 | `crates/vox-mcp/src/endpoints.rs` 的 `editable` 表不动：媒体面是**配置面 + 外壳**的事（§2.5.4）。 |

### 0.2 与既有决策的关系（到期 / 推翻 / 确认）

| # | 议题 | 旧说法（出处） | 本稿 | 依据 |
| --- | --- | --- | --- | --- |
| 1 | `CaptureTarget` 要不要加网络变体 | "不动 `ports.rs` 的 `CaptureTarget` 加 `net_in` 变体：清单先占名，**实现归 S3**"（`docs/plans/S0-COMPOSITION-MANIFEST.md` §3.4） | **到期**：S3 就是那一轮，所以本稿加 `CaptureTarget::Net { pipe }` | 同上（这句自己把到期时间写成了 S3） |
| 2 | 声音管子选型 | "跨设备（**WebRTC 或现有 WebSocket**）"（`DIRECTIONS.md` §3.3 路线三）；§6.1 第 3 条列为待拍 | **拍板：裸 WebSocket + 自定义二进制帧**；WebRTC 降为第二档，代价逐条写在 §2.1.2 | §8"新者胜" |
| 3 | 心跳来源 | "路线四：心跳来源 —— **确认**'上游管子叫醒'这条路"（`DIRECTIONS.md` §6.1 第 2 条） | **确认**，并给出落法：管子自造节拍（paced drain），**芯一行不改**（§2.4） | 同上 |
| 4 | 无屏档的"听人说话" | "这档的'听'应该是 `net_in`，S3 目标、还没实现"（`crates/vox-headless/src/headless.rs` 的 `legs_to_check` 注释） | **兑现**：`net_in` 成为这一档 Listen 腿的输入 | 本稿 §2.2、§2.3 |
| 5 | `net_in`/`net_out` 恒假 | "没有定义者 ⇒ 实现落地前恒假"（`crates/vox-core/src/capability.rs` 头注释 + S0 §2.5.4 末表） | **保持规则、换掉结论**：本稿给定义者；落地前仍恒假（S0 规则 R8 不破） | S0 §2.5.4 R6 |
| 6 | `UnavailableReason` 的取值 | 七种（`Unsupported` / `NotInstalled` / `Permission` / `NotBuilt` / `NotWired` / `PendingReboot` / `Busy`） | **加第八种 `Disabled`**（用户/配置把它关了）——取值与谁报它见 §2.2.3 的定义者表与 §2.6.3 的 `host_facts` | 本稿；R9 要求文案分得清"没开"与"做不到" |
| 7 | 媒体面凭据放哪 | 本稿第 1 版写"`SecretStore` 的 `net_audio.token` / `net_audio.peer_token`"（§2.5.1、§2.5.3、§2.6.3），但**这套键值式 API 今天不存在**（§1.6） | **补 API，不搬秘密**：给 `SecretStore` 加三个键值式方法并迁四个实现（§3.1）；**不**把 token 塞进 `settings.json`——那会破掉本稿自己那条"配置文件永不承载秘密" | 本稿；`media.json` 是随进程起落的**发布面**（§2.6.3 第 4 步：停服即擦），当不了凭据的家 |

### 0.3 帧层落地后的三处偏离（**实现已按此落地**，本文已回填）

| # | 本稿原写法 | 落地成了什么 | 为什么 |
| --- | --- | --- | --- |
| 1 | §2.6.2 的 `MediaError` 列了九种（`Short` / `Magic` / `Version` / `Kind` / `Flags` / `Rate` / `Channels` / `Oversize` / `TextFrame`） | 多一种 **`KeepAlivePayload(usize)`** | §2.1.3 已经写了"`kind=keepalive` 的载荷必须为空"这条 MUST，却没给它一个错误取值。**拒收比默默忽略安全**（与 `flags` 那条同口径），所以补一个取值：`bad_frames` +1 并关连接。见 `crates/vox-net/src/media/frame.rs::MediaError::KeepAlivePayload` |
| 2 | §2.4.3 只写了"**出站**空闲超过 `keepalive_ms` 发保活" | **入站侧（监听侧）也发**：对端连上后，空闲 `keepalive_ms` 同样发一个 20 字节保活帧（`server.rs` 的读循环里与 `read.next()` 并列的那个 `sleep` 分支） | 出站侧的判死规则是"`peer_timeout_ms` 内**没收到任何帧**就重连"（§2.4.3），而入站侧 v1 本来只收不发（音频是"收"的方向）——**只有一侧发保活 ⇒ 另一侧必然把自己人误杀**。保活帧不是音频，不违反"入站不回音频" |
| 3 | §2.4.3 的 `flush()` 只写"把不足一块的余头发出去" | `flush_done` 的置位挪到**收尾保活真的发出去之后**（`client.rs::next_frame`）：顺序固定为 **整块 → flush 的余头 → 保活**，且"这一次 flush 办完"的标志是那个保活发出去了 | `flush()` 的语义是"把已经推进来的东西全送出去"，而"余头 + 一个保活"才是完整的收尾。先置 `flush_done` 会让余头之后那个保活被下一次 flush 的判定吃掉（或干脆不发），对端也就少一次"我还活着"的证据 |

---

## 1. 现状证据

> 每一条都用 `grep` / `read` 亲自核过；行号是**写稿当时**（2026-09-22）的。

### 1.1 清单里这两位已经占好名（S0 已落地）

| 事实 | 出处 |
| --- | --- |
| `Input::NetIn { pipe: String, block_ms: u32 }`，注释写着"**[S3 目标，现状未实现]**" | `crates/vox-core/src/composition.rs:139` |
| `Output::NetOut { pipe: String, source: EdgeSource }`，注释同上 | `crates/vox-core/src/composition.rs:225` |
| `Input::NetIn` 要的位 = `Capability::NetIn`；`Output::NetOut` 要的位 = `Capability::NetOut`（`required_capability()` 的穷尽 `match`） | `crates/vox-core/src/composition.rs:158-162`、`:256-259` |
| `Composition::endpoint(pipe_in, pipe_out)` 产出的就是 `in:[net_in] / out:[net_out] / ops:[] / session:null / host:linux_headless` | `crates/vox-core/src/composition.rs:529-548` |
| `CompositionError::MissingCapability { bit, entry }` 的 `entry` 由 `requirement()` 反查"哪一格要它"（`in[0]: net_in` 这种） | `crates/vox-core/src/composition.rs` 的 `fn requirement` |

### 1.2 作业单里这两位是**硬拒**

| 事实 | 出处 |
| --- | --- |
| `Plan::from` 见到 `Input::NetIn \| Input::HostFeed` 直接 `Err("清单里的输入还没实现（net_in / host_feed 归 S3）。")` | `crates/vox-core/src/pipeline/mod.rs:140-144` |
| `Plan::from` 只把 `in.first()` 映射成 `CaptureTarget`（麦克风 / 进程环回两种），**没有第三种** | `crates/vox-core/src/pipeline/mod.rs:127-139` |
| `Plan` 只有 `target: CaptureTarget` 与 `playback_device: Option<Option<String>>` 两格描述"声音从哪来/往哪去"，**没有网络出口这一格** | `crates/vox-core/src/pipeline/mod.rs:90-110` |
| `Deps` 五个工厂：`transport` / `capture` / `playback` / `denoise` / `resample`，**没有网络出口工厂** | `crates/vox-core/src/pipeline/mod.rs:78-85` |
| 端点今天**跑不起来**有钉子用例：`assert!(Plan::from(&endpoint).is_err(), "网络进/网络出还没实现，不许被装成今天的作业单")` | `crates/vox-core/src/composition.rs:1131-1134` |

### 1.3 芯的音频端口形状（媒体面要实现的正是这两个 trait）

| 事实 | 出处 |
| --- | --- |
| `enum CaptureTarget { Microphone(Option<String>), ProcessLoopback { executable, include_tree } }` | `crates/vox-core/src/ports.rs:61-69` |
| `trait CaptureSource { fn start(&mut self, target, block_ms: u32, on_chunk: Box<dyn FnMut(AudioChunk)+Send>) -> PortResult<CaptureFormat>; fn stop(&mut self); }` | `crates/vox-core/src/ports.rs:72-83` |
| `struct CaptureFormat { sample_rate: u32, channels: u16 }` —— **协商率就是这里回去的**，算子链的 `RateRef::Capture` 指的是它 | `crates/vox-core/src/ports.rs:85-89` |
| `trait PlaybackSink { open(device: Option<&str>, source_rate: u32) -> PortResult<u32>; push(&[f32]); stats() -> PlaybackStats; flush(); close(); }` —— 芯推 **24 kHz 单声道 f32** | `crates/vox-core/src/ports.rs:93-105` |
| `struct AudioChunk { samples: Vec<f32>, sample_rate: u32, channels: u16 }`（一次一块，`Vec` 是所有权契约的一部分） | `crates/vox-core/src/ports.rs:36-42` |
| `Plan::from` 之后 `boot()` 的开流次序：先按 `plan.playback_device` 开 `playback` sink（非直通用 `OUTPUT_SAMPLE_RATE`、直通用采集率），再 `capture.start(&plan.target, INPUT_BLOCK_MS, on_chunk)`，然后用**采集率**建阀门与重采样器 | `crates/vox-core/src/pipeline/mod.rs:768-789`、`:812-835` |
| 推给 sink 的地方只有两处：链上直推（`sink.push(block)`）与云端回来的译音（`pcm16_to_float` 之后 `sink.push(&samples)`，**顺带推一份给 `monitor_sink`**） | `crates/vox-core/src/pipeline/mod.rs:1110-1113`、`:1264-1270` |
| 常数的现状：`INPUT_BLOCK_MS = 20`、`INPUT_QUEUE_SIZE = 8`（= 160 ms）、`POLL_MS = 5` | `crates/vox-core/src/pipeline/mod.rs:47`、`:49`、`:51` |
| 云端两个协议率：`INPUT_SAMPLE_RATE = 16_000`、`OUTPUT_SAMPLE_RATE = 24_000`；**云端音频走文本帧里的 base64 PCM16LE**（不是二进制帧） | `crates/vox-core/src/cloud/protocol.rs:26-28`、`:196` |
| 现成的 PCM 转换：`float_to_pcm16` / `pcm16_to_float`（都是 `pub`）——媒体面**直接复用**，不再写第二份（同一份夹取语义：负半轴 ×32768、正半轴 ×32767） | `crates/vox-core/src/cloud/protocol.rs:463`、`:478` |

### 1.4 无屏档的现状（S3 的宿主）

| 事实 | 出处 |
| --- | --- |
| `HostKind::LinuxHeadless` 的上限**只有两位**：`Mic` + `BackgroundService` | `crates/vox-core/src/capability.rs:414-418` |
| 所以 `net_in` / `net_out` 在这一档报 `false(unsupported)`；有钉子用例把"外壳想关也关不掉、只能报 unsupported"钉死 | `crates/vox-core/src/capability.rs:708-722` |
| 无屏外壳的 `host_facts()` 只报 `background_service: not_wired` 一位 | `crates/vox-headless/src/platform/linux.rs:62-66` |
| 无屏外壳有一条用例断言 `NetIn` / `NetOut` / `FileConfig` **不许为真**（"还没实现，不许广告"） | `crates/vox-headless/src/platform/linux.rs:147-152` |
| 装配第 2 步造 `Deps`：`transport` 走 `vox_net::WsTransport::new(handle)`（复用本进程的 tokio runtime，2 个工作线程） | `crates/vox-headless/src/headless.rs:232-241`、`:47-49` |
| `--print-capabilities` / `--print-composition` / `--dry-run` 走 `Probe::Nothing`：**不碰 PipeWire、不建目录、不写文件、不监听端口** | `crates/vox-headless/src/headless.rs:62-68`；`crates/vox-headless/src/cli.rs:30-38` |
| CLI 现有开关面：`--config` / `--print-capabilities` / `--print-composition` / `--dry-run` / `--start <speak\|listen\|all>` / `--run-for <秒>` | `crates/vox-headless/src/cli.rs:73-89` |
| `legs_to_check` 只在 `settings.listen.target.is_some()` 时才验 Listen 腿 | `crates/vox-headless/src/headless.rs:418-421` |
| Listen 腿的清单**要求** `config.loopback_target`，没有就 `Err("还没选择监听程序。")` | `crates/vox-core/src/pipeline/listen.rs:35-38` |
| `Runtime::start` 也拦一道：Listen 且 `settings.listen.target.is_none()` → `Notice::error("请先选择监听程序")` 并**不启动** | `crates/vox-core/src/runtime.rs:764-768` |

### 1.5 控制面已有的可复用形状（媒体面**照抄做法、不共用凭据**）

| 事实 | 出处 |
| --- | --- |
| 本机 HTTP 传输：只绑 `127.0.0.1`、单路径 `/mcp`、POST-only、`Origin` 一律 403、`Authorization: Bearer` 常数时间比较、握手文件 `0600` | `crates/vox-mcp/src/transport/http.rs` 头注释 + `PATH` / `authorized` / `create_owner_only` |
| token 生成：32 字节随机 → base64url 无填充 43 字符 | `crates/vox-mcp/src/transport/http.rs` 的 `random_token` / `TOKEN_BYTES` |
| 握手文件形状与生命周期：`{"port","token","pid","protocolVersion"}`，先写 `.tmp` 再 rename，停服时**只删还是自己那一份**的 | `crates/vox-mcp/src/transport/http.rs` 的 `write_handshake` / `remove_handshake_if_ours` |
| 设置里的控制面段：`Settings.control: ControlSettings { enabled, port, allow_*, transcript_notify_ms }`，**缺省全关（fail-closed）**，`port = 0` 表示系统分配，`normalize()` 把特权端口拉回 0 | `crates/vox-core/src/settings.rs:76`、`:105-133`、`:742-756` |
| S1 的承诺：**音频永不进控制面协议**（`audio` content type 是一次性 base64，不是流式） | `docs/plans/S1-AGENT-FACE.md` §2.4.3 |
| `vox-net` 今天的全部内容：`lib.rs`（`Transport` 的 WS 实现说明）+ `ws.rs`（tokio↔同步的桥接、读循环、错误映射）。**没有服务端、没有二进制帧、没有帧格式** | `crates/vox-net/src/lib.rs`、`crates/vox-net/src/ws.rs` |
| `vox-net` 的依赖里已经有 `tokio` / `tokio-tungstenite` / `rustls` / `futures-util` / `tokio-util`；dev-deps 里已带 `rt-multi-thread`/`net` | `crates/vox-net/Cargo.toml` |
| 无屏外壳**已经依赖 `vox-net`**（path 依赖），加媒体面不需要给无屏档新增任何包 | `crates/vox-headless/Cargo.toml` 的 `vox-net = { path = "../vox-net" }` |
| 仓库已有"真机探针 example"的先例（无屏/无头环境下可复跑的观察出口） | `crates/vox-overlay-linux/examples/frame_loop_probe.rs`（`docs/architecture/DIRECTIONS.md` §10 第十轮回填引它） |
| 能力位的消费端已经认识这两位：`app/ui/src/capabilities.ts` 的 `HOST_CAPABILITIES` 里有 `net_in`/`net_out`，i18n key 是 `capabilities.bit.netIn` / `netOut`；而 `(位, reason) → 句子` 那张表是**穷尽 `Record`**（`REASON_KEY: Record<UnavailableReason, string>`） | `app/ui/src/capabilities.ts::HOST_CAPABILITIES` / `CAPABILITY_KEY` / `REASON_KEY` / `UNAVAILABLE_REASONS`；`app/ui/scripts/check-capabilities.mjs::REASONS` / `REASON_MARK` |

### 1.6 第二十三轮新核的事实（回填本文用）

| 事实 | 出处（符号名；行号是**本轮**的） |
| --- | --- |
| `SecretStore` 只有**服务商键**那三个方法 + 三个带默认实现的 `*_for(provider)`，**没有**键值式的 `load_secret` / `store_secret` / `clear_secret` | `crates/vox-core/src/ports.rs::SecretStore`（`load_api_key` / `store_api_key` / `clear_api_key` + `load_api_key_for` / `store_api_key_for` / `clear_api_key_for`） |
| 全仓库 `impl SecretStore` 共**四处** | `crates/vox-headless/src/secrets.rs::SecretFile`、`app/src-tauri/src/sys/secrets.rs::DpapiSecretStore`、`app/src-tauri/src/platform/linux/secrets.rs::SecretServiceStore`、`crates/vox-core/src/pipeline/mod.rs` 测试里的 `MemoryStore` |
| 四个实现的存储**本来就是 key→value**，所以加键值式方法是"把 provider 换成字符串"而不是新后端：`SecretFile` 是 `BTreeMap<String,String>` 的 0600 JSON，键就是 `provider.as_id()`；`DpapiSecretStore` 是"每个服务商一个文件"（`path_for(provider)`：`Aliyun` 走**基准文件名**，其余是 `{stem}-{id}.{ext}`）；`SecretServiceStore` 的条目名是 `format!("api-key.{}", provider.as_id())` | `crates/vox-headless/src/secrets.rs::read_file` / `write_file` / `load` / `store` / `clear`；`app/src-tauri/src/sys/secrets.rs::DpapiSecretStore::path_for` 与私有 `store_secret(path, key)` / `load_secret(path)` / `clear_secret(path)`（**注意这三个名字与要加的同名 trait 方法撞名**）；`app/src-tauri/src/platform/linux/secrets.rs::user_for` |
| token 生成是 `vox-mcp` 的**私有** `fn random_token() -> io::Result<String>`（`TOKEN_BYTES = 32` → `URL_SAFE_NO_PAD`），每次 `serve()` 调一次；`vox-net` 调不到（也不该依赖控制面 crate） | `crates/vox-mcp/src/transport/http.rs::random_token` / `TOKEN_BYTES` / `serve`（`let token = random_token()?;`）；它的 `base64.workspace` + `getrandom = "0.3"` 是**自己 crate 的**依赖，workspace 根只有 `base64 = "0.22"`，**没有** `getrandom` |
| 控制面 token **不持久化**：`control.json`（`0600`）是唯一落点，停服即擦；`vox-mcp` 全 crate 不碰 `SecretStore` | `crates/vox-mcp/src/transport/http.rs::write_handshake` / `remove_handshake_if_ours`；`crates/vox-headless/src/mcp.rs::STATE_FILE` 的注释 |
| 能力位报告的 JSON 形状：`host` 是**每位一格**的表（`Capability::HOST` 全在），上限之外的位照 `unsupported` 报 | `crates/vox-headless/src/status.rs::capabilities_json`；`crates/vox-headless/src/platform/linux.rs::host_facts` 的注释与用例 `the_report_matches_the_headless_tier` |
| i18n 文件名是 **`zh.ts` / `en.ts`**（另有 `ja.ts`，是**冻结包**：`Omit<DictShape, "capabilities">`，新 key 不加、缺的键回落 zh） | `app/ui/src/i18n/{zh,en,ja}.ts`；`ja.ts` 头注释的"保守ルール（2026-09-22 改定）" |
| 检查脚本的 reason 全集与特征文案是两张表，注释里写的还是"**七种**" | `app/ui/scripts/check-capabilities.mjs::REASONS` / `REASON_MARK`（第 [7] 条遍历按 `REASONS` 走，zh 与 en 都要命中） |
| 本机**没有 `sox`**（`python3` / `jq` 在 `/usr/bin/`） | `which sox python3 jq` → 只打出 `/usr/bin/python3`、`/usr/bin/jq`，退出码 **1** |
| `vox-net` 的帧层依赖已进 manifest：`parking_lot.workspace = true`（入站排空线程的 `Mutex + Condvar`）；`base64` / `getrandom` **还没有** | `crates/vox-net/Cargo.toml` 的 `[dependencies]` |
| `Deps` 今天只有两个构造点 | `app/src-tauri/src/lib.rs` 与 `crates/vox-headless/src/headless.rs`（都在 `vox_core::pipeline::Deps { … }` 字面量里） |

---

## 2. 目标形状

### 2.1 协议选型：裸 WebSocket + 自定义二进制帧（WebRTC 降为第二档）

#### 2.1.1 结论

**媒体面 = 裸 WebSocket（`ws://` / `wss://`）+ 一份 20 字节定长头的二进制帧，载荷 PCM16LE 单声道。**
入站与出站**共用同一份帧格式与同一套管子实现**；对端可以是另一个盒子、服务器、或浏览器页面。

理由（每条都对着现状）：

1. **零新增依赖**。`vox-net` 已经有 `tokio-tungstenite` + `rustls`（§1.5），服务端只要 `accept_async`；
   WebRTC 要引入一整栈（`webrtc`/`str0m` 之类）**外加 STUN/TURN 服务端**，与本项目"能不引新包就不引"的口径相反。
2. **延迟预算里媒体面不是瓶颈**。端到端延迟由云端模型主导（`DIRECTIONS.md` §4 记着"听人说话"在 GPT 上没有回合结束事件，
   也就是说延迟/回合是产品已知痛点）；媒体面 40 ms 级抖动缓冲相对可忽略。**把复杂度花在 WS 上、把确定性留给算子链**更划算。
3. **PCM16LE 与云端协议同源**：`INPUT_SAMPLE_RATE`/`OUTPUT_SAMPLE_RATE` 与 `float_to_pcm16`/`pcm16_to_float`（§1.3）
   已经是 PCM16LE 世界，媒体面用它就不必在两条管子之间再加一层编解码。
4. **回退路径不封死**：`pipe` 是**逻辑名**不是 URL（§2.3.2），帧头有 `kind` 位（§2.1.3），
   将来换 WebRTC 或换 Opus **不动清单、不动芯**。

#### 2.1.2 不选 WebRTC 的代价（逐条写清，不是"以后再说"）

| 代价 | 后果 | 兜底 / 触发条件 |
| --- | --- | --- |
| **没有 NAT 穿透**（无 ICE/STUN/TURN） | 至少一端必须可达：回环、局域网、或用户自己的隧道/端口映射。两端都在 NAT 后面时**连不上** | 第一站（同机 / 同局域网）不受影响；跨公网由用户用隧道（WireGuard/Tailscale 类）解决，或以后加中继 |
| **没有拥塞控制与丢包局部化**（TCP 之上） | 丢一个 TCP 段会**队头阻塞**整条音频流：一次 100–300 ms 的卡顿，且重传把延迟放大 | 抖动缓冲 + 欠载补静音（§2.4.3）能吸收一部分；**这是本选型最主要的代价** |
| **带宽大**：PCM16 16 kHz 单声道 = 32 KB/s ≈ **256 kbit/s**（24 kHz 出站 ≈ 384 kbit/s）；Opus 通常 16–32 kbit/s | 移动网络/按流量计费的场景差 8–10 倍 | 出站可先降到 16 kHz；Opus 留 `kind` 位。**Opus 的具体档位与体积本轮未实测** `[未核实]` |
| **没有内置加密**（要 `wss://`） | v1 服务端只做明文 `ws://`（§5.3） | 回环/局域网内先接受；跨网必须走隧道或等 `wss` 服务端 |
| **没有浏览器侧的 AEC/AGC** | 浏览器 `getUserMedia` 的流**默认已被浏览器做过 AEC/AGC/NS**，所以这一条在浏览器侧不算输；非浏览器对端（另一个盒子）要自己保证 | 盒子上是"网络来的音频"，本来就不该再降噪（与 Listen 腿"数字源不降噪"同口径） |
| 需要自己写抖动缓冲、保活、重连 | 工程量落在管子（§2.6），不在芯 | 芯侧**零改动**（§2.4.1） |

#### 2.1.3 帧格式（v1，逐字节定死）

```
 偏移   0      2    3    4      6      10     14     18     20
        ├──────┼────┼────┼──────┼──────┼──────┼──────┼──────┤
 字段   magic  ver  kind flags  seq    ts_ms  rate   ch     载荷…
 类型   "VB"   u8   u8   u16LE  u32LE  u32LE  u32LE  u16LE  （帧长 − 20）
 宽度   2      1    1    2      4      4      4      2      ← 合计 20
```

（宽度那一行是**自检**用的：`2 + 1 + 1 + 2 + 4 + 4 + 4 + 2 = 20`。实现里的偏移常量与这张表一一对应：
`crates/vox-net/src/media/frame.rs` 的 `OFF_VERSION` / `OFF_KIND` / `OFF_FLAGS` / `OFF_SEQ` / `OFF_TS_MS` / `OFF_RATE` / `OFF_CHANNELS`。）

- `magic` = `b"VB"`；`ver` = `1`。两者不符 → 协议错误，**关连接**。
- `kind`：`0` = `pcm16le`，`1` = `keepalive`（**载荷必须为空**；带了载荷是协议错误
  `MediaError::KeepAlivePayload`，见 §0.3 第 1 条）。其它值 → 协议错误。
  **留位含义**：将来加 `2 = opus` 或别的编码，靠这个字节扩，不动头长与清单。
- `flags`：u16，v1 **必须为 0**（非 0 即协议错误）。留给"将来要标记"的场合，今天拒收比默默忽略安全。
- `seq`：u32 环绕，**每条连接从随机值起**。用途只有一个：检测丢帧/乱序（TCP 下不该发生，
  发生就是管子 bug 或中间有代理）→ 计数 `seq_gaps`。
- `ts_ms`：发送侧单调毫秒（连接建立时的零点）。**只进统计与日志，不参与播放调度**——跨机时钟不可比，
  调度一律用接收侧的到达时刻与本机时钟（§2.4.2）。
- `rate`：载荷采样率（Hz）。**入站必须等于 `Settings.net.rate_hz`**（§2.3.4），不符即协议错误（**不静默重采样**：
  重采样是算子链的事，管子不许偷偷记状态）。
- `ch`：v1 **必须 = 1**（单声道）。要立体声请在对端下混（`Op::Mono` 的语义在芯里）。
- `payload` 长度 = **帧长 − 20**，上限 `MAX_PAYLOAD = 8 KiB`（4096 样本 PCM16 = 256 ms @16 kHz）。
  超过上限或帧长 < 20 → 协议错误。
- **只收二进制帧**；文本帧 → 协议错误（没有 JSON 通道：音频管子不认协议方言）。
- 常规帧：`block_ms`（20 ms）一块 → @16 kHz 是 320 样本 = 640 字节载荷，整帧 **660 字节**；
  @24 kHz 是 480 样本 = 960 字节，整帧 980 字节。

#### 2.1.4 与 `vox-net` 的关系

**扩展 `vox-net`，新增 `src/media/` 子模块**（不是新建 crate）：

| 备选 | 代价 | 结论 |
| --- | --- | --- |
| **扩展 `vox-net`**（`src/media/{mod,frame,pipe,server,client}.rs`） | crate 描述要改（从"云端 `Transport` 的 WS 实现"扩成"网络传输：云端 WS + 媒体面音频管子"）；`ws.rs` 与 `Transport` 契约**一个字不动** | **选它**：零新增包（`vox-net` 已带 tokio/tungstenite/rustls，无屏档已依赖它）；实现 `vox_core::ports` 的 trait 与 `ws.rs` 实现 `cloud::Transport` 是**同一种角色**（芯的端口实现方） |
| 新建 `crates/vox-net-audio/` | 要么把 tokio↔同步那套桥接复制一份，要么给 `vox-net` 开一段新 API 供它复用——两条都比加一个子模块贵 | 否 |
| 把音频塞进现有 `Transport` | `Transport` 是**云端协议**的契约（文本帧、`ConnectRequest` 带 `Authorization`），改成"还能发音频"会污染 S1 §2.4.3 划的那条线 | 否 |

**不共用凭据、不共用端口、不共用协议**：控制面是 `/mcp` + `control.json`，媒体面是 `/audio` + `media.json`（§2.5）。

### 2.2 谁在跑

#### 2.2.1 三种形态，一条管子

| 形态 | `in` / `out` 取值 | 传输 | S3 做不做 |
| --- | --- | --- | --- |
| **无屏档（本稿的主角）** | `net_in` / `net_out` | WebSocket（回环或局域网） | **做** |
| **寄生宿主·同进程**（OBS 类插件、编辑器插件） | `host_feed` / `host_sink` | **进程内队列**（宿主回调 ↔ `mpsc`） | 不做（插件宿主那一轮），但**共用点写死**（下条） |
| **寄生宿主·跨机器**（浏览器页面 + 本机/远端盒子） | `net_in` / `net_out` | WebSocket | **做**（与无屏档同一条管子，只是地址不同） |

**共用点（这就是"怎么共用"的答案）**：

1. **同一份帧格式**（§2.1.3）：宿主形态也用它（进程内传的是同一份字节，只是不过 socket）——
   这样"宿主喂进来的声音"与"网络上来的声音"在管子里的路径**逐字相同**。
2. **同一个 `Pipe`**（抖动缓冲 + 重切块 + paced drain + 补静音/丢最旧，§2.4.3）：宿主形态的传输层
   把帧推进同一个环形缓冲，**没有第二套调度逻辑**。
3. **同一对 trait**：`host_feed` 是"另一份 `CaptureSource` 实现"、`host_sink` 是"另一份 `PlaybackSink` 实现"。
   清单里这两格的 `required_capability()` 是 `None`（`crates/vox-core/src/composition.rs:161`、`:258`）——
   **宿主自己给的声音永远可用**，所以宿主形态不需要新档位、不需要端口、不需要凭据。S0 早就把这条路留好了。
4. **不需要新的 `HostKind`**：宿主形态落在现有四档里最近的那一档 + `host_feed`/`host_sink` 两格即可
   （`HostKind` 的四档是并列的宿主**档位**，`hosted` 只是 `Life::Hosted` 那一格，不是档位）。

#### 2.2.2 无屏档怎么装

```
装配期（Assembly）
  Settings.net ──► MediaListener::bind(addr)   ← 这一步真的绑上 ⇒ net_in 的定义者
                 └► MediaOut::spawn(peer)      ← 这一步真的起来 ⇒ net_out 的定义者
                        │
                        ▼
  Daemon::start ──► Deps { capture: 分派壳(设备 / 网络), net_out: Some(工厂), … }
                        │
                        ▼
  Runtime::start(Listen) ──► Composition::of ──► Plan::from ──► boot()
                                                            ├─ capture.start(CaptureTarget::Net{pipe})  → 收帧、重切块、on_chunk
                                                            └─ (net_out) → PlaybackSink::push(f32)      → 攒块、发帧
```

- 监听**由外壳持有**（进程一份），管子只从它取一条已鉴权的连接（`MediaListener::capture()` 造出腿级的 `CaptureSource`）。
- **只读模式（`--print-capabilities` / `--print-composition` / `--dry-run`）不 bind、不 connect**（§1.4 的 `Probe::Nothing` 口径）：
  报告模式下这两位如实报 `false(not_wired)`——"这次启动没把它打开"就是事实。要看真值走常驻模式 + 状态出口（§4.4）。

#### 2.2.3 与 `HostKind::LinuxHeadless` 位表的关系

**只把这两位加进无屏这一档的上限**：

```rust
// crates/vox-core/src/capability.rs::host_ceiling
HostKind::LinuxHeadless => CapabilitySet::of(&[
    Capability::Mic,               // 挂了 USB 声卡且枚举得到时这一位才为真（事实由外壳报）
    Capability::BackgroundService,
    Capability::NetIn,             // S3：从网络收声音（定义者见下）
    Capability::NetOut,            // S3：往网络发声音
]),
```

**桌面两档与 Android 档不加**，理由（这是个决定，不是遗漏）：

- 上限的含义是"这一档**结构上**能不能"（S0 §2.5.1），而 `host_ceiling` 是 S1 `describe_endpoint` 回答
  "**别的**档位能不能做某件事"的唯一依据。桌面今天**没有端点入口**（没有 `--endpoint`、没有监听装配），
  把这两位加进去等于让 Agent 读到"桌面能当端点"——那是**广告做不到的事**，正是 S0 §2.5.4 R6 与 §2.6 R8 要封的东西。
- 将来桌面要当端点时，**上限与定义者同轮一起进**（本稿不动它）。

**定义者（R6，逐位）**：

| 位 | 定义者（"真的把这条路打开的那段代码"） | 凭据（可观察） | 关着的 reason |
| --- | --- | --- | --- |
| `net_in` | `MediaListener::bind(&options)` 返回 `Ok` 且句柄存活（外壳装配期） | `--print-composition` / `describe_endpoint` 里该位为真；`media.json` 出现且 `listen` 是**实际绑定**地址 | 总闸关 → `disabled`；配置没给 `listen` 或绑失败 → `not_wired`；端口被占 → `busy` |
| `net_out` | `MediaOut::spawn(peer)` 返回 `Ok`（出站池线程真的起来了） | 同上；统计里 `connected` 反映此刻连接 | 总闸关 → `disabled`；配置没给 `peer` → `not_wired` |

**出站这一位的定义者比入站弱，是有意的**：入站必须"监听真的绑上"（不绑就没有这条路）；
出站如果要求"此刻连着对端"，那"对端在重启"就会变成"这台设备没这个能力"——位会随对端抖动而翻，
清单也跟着抖（`out` 那格一进一出），那是把**链路状态**错当**能力**。所以：

- 位 = "出站池起来了"（装配期事实）；
- **连接状态**进统计（`connected` / `reconnects` / `last_error`），进日志与状态出口，**不进位**。

这条不对称必须写进 `capability.rs` 的位表注释里，否则下一个人会把它当 bug 修掉。

### 2.3 清单怎么表达

#### 2.3.1 逐字取值（四位/两位都照 S0 已占名的形状，**不加字段**）

| 清单格 | v1 取值 | 说明 |
| --- | --- | --- |
| `in[].kind` | `"net_in"` | 已占名（`Input::NetIn`） |
| `in[].pipe` | `"default"` | **逻辑管道名**，不是地址。v1 只有 `"default"` 一条（`pub const DEFAULT_PIPE: &str = "default"`） |
| `in[].block_ms` | `20` | = `INPUT_BLOCK_MS`，与麦克风/环回两格同一个值 |
| `out[].kind` | `"net_out"` | 已占名（`Output::NetOut`） |
| `out[].pipe` | `"default"` | 同 `in` |
| `out[].source` | `"chain"` 或 `"session"` | `"chain"` = 直通原声（中继）；`"session"` = 云端译音 |

**为什么 `pipe` 是名字不是地址**：地址是**环境**（哪个网卡、哪个端口、对端在哪），名字是**装配**。
清单要能随手分享、能被 Agent 生成、能被界面显示（`Composition` 的文档注释就是这么写的），
所以地址只住在 `Settings.net`（配置面）与 `media.json`（发布面）。未知的 `pipe` 名 → 装配期**报错**，
不静默当成 `default`。

#### 2.3.2 无屏档的四份清单（逐字）

**① 中继端点（`Composition::endpoint("default","default")` 的原样，`session: null`）**

```json
{
  "schema_version": 1,
  "host": "linux_headless",
  "in":  [{ "kind": "net_in",  "pipe": "default", "block_ms": 20 }],
  "ops": [],
  "out": [{ "kind": "net_out", "pipe": "default", "source": "chain" }],
  "life": "daemon",
  "ui": "none",
  "control": ["mcp", "cli", "config_file"],
  "session": null
}
```

**② 翻译端点（同一条腿、译音回给对端）**——只有两格与①不同：

```json
  "out": [{ "kind": "net_out", "pipe": "default", "source": "session" }],
  "session": { "provider": "aliyun", "hot_update": false,
               "uplink_rate": "session", "downlink_rate": "playback", "params": { "…": "…" } }
```

**③ 无屏档「听人说话」（S3 的主交付：输入来自网络，中文语音走 USB 声卡）**

```json
{
  "schema_version": 1,
  "host": "linux_headless",
  "in":  [{ "kind": "net_in", "pipe": "default", "block_ms": 20 }],
  "ops": ["mono", { "kind": "gate", "config": { "mode": "level", "threshold": 0.0,
                                                "tail_ms": 600, "preroll_ms": 200 } },
          { "kind": "resample", "from": "capture", "to": "session" }],
  "out": [{ "kind": "playback", "role": "speaker", "device": null, "source": "session" }],
  "life": "daemon",
  "ui": "none",
  "control": ["mcp", "cli", "config_file"],
  "session": { "provider": "aliyun", "hot_update": false,
               "uplink_rate": "session", "downlink_rate": "playback", "params": { "…": "…" } }
}
```

- `ops` 与 Listen 腿**逐项相同**（不装 `denoise`：网络来的是数字源，与 `listen.rs` 模块头同口径；
  `gate` 是 `GateConfig::level(0.0)` 无条件放行，省 token 靠服务端 VAD）。
- 声卡那一格只在 `mic` 位为真且用户选了输出设备时才在；没声卡就只剩 `captions`（无屏档 `captions` 位为假 →
  那一格也进不来）→ 这条腿退化成"只有日志/资源面的字幕"，**腿仍在**（R9：说清"这台设备做不到"，不删腿）。

**④ 无屏档「对外说话」（本机麦克风 → 译音发给对端）**

```json
  "in":  [{ "kind": "mic", "device": null, "block_ms": 20 }],
  "out": [{ "kind": "net_out", "pipe": "default", "source": "session" }]
```

（`ops` 与桌面 Speak 逐项相同：`mono` / `denoise?` / `gate` / `resample`。）

#### 2.3.3 清单 → `Settings` 的映射（谁决定"这一格进不进"）

| 清单格 | 由谁决定 | 现有同类先例 |
| --- | --- | --- |
| `in: net_in` 进不进 | `effective(facts).contains(Capability::NetIn)` **且** `config.net_in.is_some()` | `if bits.contains(Capability::ProgramTap) { push(Input::ProcessLoopback{..}) }`（`listen.rs:44-51`） |
| `out: net_out` 进不进 | `effective(facts).contains(Capability::NetOut)` **且** `config.net_out.is_some()` | `let role = if bits.contains(Capability::VirtualMic) { … }`（`speak.rs:61-65`） |
| `out: net_out` 的 `source` | 有 `session` → `"session"`，没有 → `"chain"`（直通） | `let source = if passthrough { Chain } else { Session }`（`speak.rs:72-76`） |

**没有第二条派生路径**：清单仍只由 `Composition::of(config, facts)` 派生、`Plan` 仍只由 `Plan::from(&composition)` 派生（S0 §2.7）。

#### 2.3.4 配置面（新增 `Settings.net`）

```rust
/// 媒体面（网络音频进出）。**缺省全关**，与 `ControlSettings` 同款 fail-closed。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetSettings {
    /// 媒体面总开关。缺省 `false`：装完不开，必须用户主动打开。
    pub enabled: bool,
    /// 入站监听地址 `IP:PORT`。`None` = 不监听。端口 `0` = 系统分配（实际地址发布到 `media.json`）。
    pub listen: Option<String>,
    /// 出站对端，只认 `ws://` / `wss://`。`None` = 不连。
    pub peer: Option<String>,
    /// 声明采样率（Hz）。白名单 `{8000, 16000, 24000, 48000}`，缺省 `16000`。入站首帧必须相符。
    pub rate_hz: u32,
    /// 抖动缓冲目标深度（ms）。缺省 `40`（= 2 块）。
    pub jitter_ms: u32,
    /// 连续欠载最多补多久静音（ms）。缺省 `200`。
    pub pad_ms: u32,
    /// 环形缓冲上限（ms）。缺省 `160`（= `INPUT_QUEUE_SIZE × INPUT_BLOCK_MS`）。
    pub queue_ms: u32,
    /// 出站空闲多久发一个 `keepalive` 帧（ms）。缺省 `1000`。
    pub keepalive_ms: u32,
    /// 多久没收到任何帧就判对端掉线（ms）。缺省 `3000`。
    pub peer_timeout_ms: u32,
    /// 允许的浏览器 `Origin`（空 = **拒绝一切带 Origin 的握手**）。
    pub allowed_origins: Vec<String>,
}
```

`Settings::normalize()` 的规则（照 `ControlSettings` 的既有做法）：

| 字段 | 规则 |
| --- | --- |
| `listen` | 只接受能解析成 `SocketAddr` 的 `IP:PORT`（**不猜主机名**）；解析失败 → `None`；端口 `< 1024` → `None`（特权端口）；端口 `0` 保留（系统分配）；非回环地址**允许但记一条 warning**（§5.3） |
| `peer` | scheme 白名单 `ws` / `wss`；其它（含 `http`）→ `None` |
| `rate_hz` | 不在白名单 → `16000` |
| `jitter_ms` | 夹到 `0..=500` |
| `pad_ms` | 夹到 `0..=1000` |
| `queue_ms` | 夹到 `40..=1000` |
| `keepalive_ms` | 夹到 `250..=30_000` |
| `peer_timeout_ms` | 夹到 `1000..=60_000`，且**必须 > `keepalive_ms`**（否则对端还在发心跳就被判死） |
| `allowed_origins` | 逐条 trim；空串丢掉；最多 16 条（防手抖写一屏） |

`SessionConfig` 增两格（由 `Runtime::derive_session_config` 从 `Settings.net` 填，**只有该腿有份的那一格才填**）：

```rust
pub struct SessionConfig {
    // …既有字段不动…
    /// 这条腿的输入来自媒体面（`None` = 不是）。只有 Listen 腿会填。
    pub net_in: Option<String>,   // pipe 名；v1 恒 Some("default") 或 None
    /// 这条腿的输出走媒体面（`None` = 不是）。只有 Speak 腿会填。
    pub net_out: Option<String>,
}
```

| 腿 | `net_in` | `net_out` | 条件 |
| --- | --- | --- | --- |
| `Pipeline::Speak` | `None` | `Some("default")` | `settings.net.enabled && settings.net.peer.is_some()` |
| `Pipeline::Listen` | `Some("default")` | `None` | `settings.net.enabled && settings.net.listen.is_some()` |

配套的两处守卫要一起改（否则腿起不来）：

- `Runtime::start`（`runtime.rs:764-768`）：`listen.target.is_none()` 的拦截改成
  **`settings.listen.target.is_none() && !net_listen_enabled(settings)`** —— 媒体面配好了就不算"还没选监听程序"。
- `listen.rs::composition`（`listen.rs:35-38`）：`loopback_target` 的 `ok_or` 改成
  "**有 `net_in` 时不需要它**"（`net_in` 优先；两个都没有才报错）。

### 2.4 心跳：上游管子叫醒（路线四的落法）

#### 2.4.1 芯一行不改

`CaptureSource::start` 的回调契约不变（`on_chunk` 一块一块地喂），`PlaybackSink::push` 不变，
`INPUT_BLOCK_MS` / `POLL_MS` / `INPUT_QUEUE_SIZE` 三个常数都不动。**节拍从哪来是管子的实现细节**——
这正是 `DIRECTIONS.md` §3.3 路线四那句话的落地形态：声卡、定时器、网络包三种来源都变成管子内部的事。

| 来源 | 谁造节拍 | 现状/目标 |
| --- | --- | --- |
| 声卡（麦克风 / 环回） | 设备驱动的回调（PipeWire / WASAPI），20 ms 一块 | 现状，不动 |
| **网络（`net_in`）** | **管子自己的单调时钟 + paced drain**（下面） | **S3** |
| 定时器 | 还没有这种管子（`DIRECTIONS.md` §3.3 里提过） | 不做 |

#### 2.4.2 入站节拍：paced drain

```
tokio 读循环（1 条任务）                 抖动环形缓冲              排空线程（1 条）
 accept/recv ─► 校验帧头 ─► 写环形缓冲 ─► [ PCM16 bytes ] ─► 每 block_ms 醒一次
                                   │                          ├─ 取 block_ms 的样本 → pcm16_to_float → on_chunk
                                   │                          ├─ 空 → 补静音（≤ pad_ms）
                                   └─ 满 → 丢最旧（= INPUT_QUEUE_SIZE 的既有策略）
```

- **预填**：连上后先攒够 `jitter_ms`（缺省 40 ms）才开始排空——吸收网络到达抖动。
- **排空**：绝对时刻表（`Instant` 累加 `block_ms`），**不按到达时刻**。落后一拍就补一拍、最多补 1 拍（防雪崩）；
  这块"排空线程"用 `parking_lot::Mutex + Condvar` 与读循环同步（workspace 已有 `parking_lot`，芯里也是它）。
- **欠载**：环空 → 补一个静音块（用一条复用缓冲，`resize` + `fill(0.0)`，不新增分配），计数 `padded_ms`。
  连续欠载累计超过 `pad_ms` → **停止回调**（进入"没有输入"状态，不再补静音）：
  理由有两条——① 无限补静音对按音频时长计费的服务端（Gemini 一类）就是白烧钱；
  ② "没有输入"是事实，该被看见，不该被静音伪装。
- **过载**：环满 → 丢最旧（与 `INPUT_QUEUE_SIZE` 同策略），计数 `dropped_ms`。
- **对端还没来 / 已经走了**：**不补静音**（那会把"对端不在"伪装成"对端在静音"），
  只是没有回调；`connected` 为假、`last_frame_age_ms` 增长，进统计与日志。
- **对端可以重连**：一条连接断了就收掉它，等下一条通过鉴权的连接（手机随时被杀是产品前提，
  `DIRECTIONS.md` §2.5.2）。重连后**重新预填**（不沿用上一个对端的时间线）。
- **`AudioChunk` 的所有权**：每块一次 `Vec<f32>`（回调拿走它）——与麦克风源**同款**，不是新增的分配模式。

#### 2.4.3 出站节拍与保活

- `PlaybackSink::push(&[f32])` 是**推驱动**的：攒够 `block_ms` 一块再发一帧（避免"一小段一帧"把 WS 打碎）。
  `flush()` 是**非阻塞**的（只记一个"要 flush"的意图，发送在管子线程里做）；一次 flush 的收尾是
  "余头 + 一个保活"。**发帧顺序固定**：整块 → flush 的余头 → 保活，而"这次 flush 办完"的标志
  （`flush_done`）只在**收尾保活真的发出去之后**才置位——见 §0.3 第 3 条。
- **保活**：空闲超过 `keepalive_ms` 发一个 `kind=keepalive` 的 20 字节帧。它让对端能区分
  "对端还在但没人说话"与"对端掉了"。
  **入站侧（监听侧）也发**，用的是同一个 `keepalive_ms`：出站侧的判死规则是"`peer_timeout_ms` 内
  **没收到任何帧**"，而入站侧 v1 只收不发（音频是"收"的方向）——只让一侧发保活，另一侧就会把自己人误杀。
  见 §0.3 第 2 条。
- **判死**：`peer_timeout_ms` 内没收到任何帧 → 认为对端掉了 → 退避重连（`reconnects` 计数）。
- WS 层的 Ping/Pong 仍由 tungstenite 在 `read()` 里自动处理（`vox-net/src/ws.rs` 的既有结论），
  **不暴露给上层**；应用层保活用上面那个 `keepalive` 帧（它与 WS Ping 不是一回事：一个证明"管子还在"，
  一个证明"对端程序还在"）。

### 2.5 安全

#### 2.5.1 与 `Settings.control` 的关系：两条管子，两套凭据，两个开关

| 项 | 控制面（S1） | 媒体面（S3） |
| --- | --- | --- |
| 路径 | `/mcp`（`crates/vox-mcp/src/transport/http.rs::PATH`） | `/audio`（新，`crates/vox-net/src/media` 的 `PATH`） |
| 协议 | JSON-RPC（MCP 2026-07-28） | 二进制音频帧（§2.1.3），**不认 JSON** |
| 绑定 | **只绑 `127.0.0.1`**（硬编码） | `Settings.net.listen`（**可以**是局域网地址——手机要连过来） |
| 凭据 | `control.json` 里的 token（`0600`；**每进程新生成**，`serve()` 里 `random_token()` 一次，停服即擦，不落别处） | **`SecretStore` 的两个键**：`net_audio.token`（本机，重启不变）、`net_audio.peer_token`（出站连的那个对端）；发布面 `media.json`（`0600`）只发布**本机**那串 |
| 总开关 | `Settings.control.enabled`（缺省 `false`） | `Settings.net.enabled`（缺省 `false`） |
| 授权位 | `allow_microphone` / `allow_system_audio` / `allow_audible_output` / `allow_config_write` | **没有 `allow_*`**：媒体面不是 Agent 驱动的，它是设备自己的配置（谁能连由 token + Origin 决定） |
| 谁能改 | Agent（受 `allow_*` 管） | **只有配置面**（`settings.json`）+ 外壳；`voxctl`/MCP **不暴露**（§2.5.4） |

**两边互不可用**：控制面的 token 拿去连 `/audio` 必须 401，反之亦然（各读各的文件、各比各的串）。

#### 2.5.2 监听地址

- 缺省 `listen = None` → **不监听**（与 `ControlSettings.port = 0` 的"缺省不占端口"同口径）。
- 回环（`127.0.0.1` / `::1`）是**推荐形态**：同机对端（寄生宿主 / 探针 / 同机页面）。
- 非回环（`0.0.0.0` / 具体网卡地址）**允许**——盒子上没有别的办法让手机连过来——但：
  ① 端口必须显式给（`< 1024` 一律拒绝）；② 装配期发一条 `Notice::warning`（无屏档进 journal：
  "媒体面在明文 `ws://` 上监听非回环地址"）；③ 对端仍必须过 token。
- 端口 `0` = 系统分配，**实际地址写进 `media.json`**（同机对端读它；跨机对端由用户显式配地址）。

#### 2.5.3 鉴权

| 项 | 定值 |
| --- | --- |
| 凭据 | **对称共享秘密**：32 字节随机 → base64url 无填充 **43 字符**（形状照 `transport/http.rs::random_token`，但那一个是 `vox-mcp` 的私有 fn，见 §1.6） |
| 存放 | `SecretStore` 的**两个键**：`net_audio.token`（**本机**凭据，首次生成后持久化 → 重启不变）、`net_audio.peer_token`（**对端**凭据，出站连它时用）。**配置文件永不承载秘密**（配置要能随手分享）。这套键值式 API **今天不存在**（`SecretStore` 只有 `*_api_key`），要按 §3.1 补 |
| 发布 | 启动时把**本机 token** 与**实际监听地址**写进 `media.json`（`0600`，先写 `.tmp` 再 rename，停服时**只删还是自己那一份**的——三条都照 `control.json` 的既有做法）。同机对端读它 |
| 跨机配对 | 对端把同一串存进自己的 `SecretStore`（键 `net_audio.peer_token`）；S3 只做**最小可用**：一条 CLI/文档说明怎么抄，不做配对码/二维码 |
| 通道①（非浏览器） | `Authorization: Bearer <token>`，**常数时间比较** |
| 通道②（浏览器） | `Sec-WebSocket-Protocol: vox.media.v1.<token>` —— 浏览器的 `WebSocket` 构造函数**只能设子协议、不能设请求头**，所以这条通道必须有；服务端在 101 响应里回同一个子协议名 |
| **不接受** | URL query（`?token=`）——会进访问日志与代理日志 |
| 失败 | 在 **upgrade 之前**回 `401`（token 缺/错）。**不带任何 body**，也不回帧 |

**为什么本机 token 要持久化、且必须住 `SecretStore`**（这是 §0.2 第 7 条那个决定的展开）：

- `media.json` 是**发布面**：它随进程起落（§2.6.3 第 4 步"停服即擦"），当不了凭据的家。
- token 若每次重启都换，**跨机对端抄过的那串就废了**——那是"能用一次"的配对，不是最小可用。
- 不塞 `settings.json`：那会破掉本稿自己那条"配置文件永不承载秘密"（配置要能随手分享、要能被 Agent 读）。
- 所以：**本机 token 存 `net_audio.token`**，`media.json` 只把它发布给同机对端（同机对端当然也能直接读
  `SecretStore`，但读一个 0600 文件更省事，也不必知道无屏档的密钥文件在哪）。

**存不下怎么办**（keyring 不可用 / 磁盘只读 / `store_secret` 报错）：装配期发一条 `Notice::warning` 并
**照常把媒体面起起来**（用本次进程内的临时 token）。理由：**"能听人说话"比"token 跨重启稳定"重要**，
代价只是对端要重抄一次——而"存不了密钥"这件事必须被看见（与无屏档 `SecretFile` 的既有口径一致）。

#### 2.5.4 Origin（浏览器是唯一会带 `Origin` 的对端）

- 带 `Origin` 的握手：必须**逐字命中** `Settings.net.allowed_origins`，否则 `403`（在 upgrade 之前）。
- `allowed_origins` 缺省**空** = 拒绝一切带 `Origin` 的握手（fail-closed）。
  理由：**任意网页都能向 `localhost` 开 WebSocket**，所以"同机"不等于"可信"。
- 不带 `Origin` 的握手（我们的盒子、探针、CLI）：走 token 那条路，不做 Origin 检查。
- 控制面今天的做法是"带 `Origin` 一律 403"（它只服务本机 CLI）；媒体面**必须有**白名单，因为它的正当消费者里就有浏览器。

#### 2.5.5 控制面不暴露媒体面配置

`crates/vox-mcp/src/endpoints.rs` 的 `editable` 表**不动**：`compose_endpoint` 改不了 `Settings.net`。
理由：媒体面是**这台设备的网络暴露面**，改它等于改"谁能把音频灌进这台设备"——
这不该由一个 Agent 会话顺手改掉（S1 的四个 `allow_*` 位没有一位覆盖"开放监听端口"这种后果）。
它进 `describe_endpoint`（能力位报告里看得见），**不进** `compose_endpoint` 的可写格。

### 2.6 类型与接口签名（照着编码的程度）

#### 2.6.1 芯（`crates/vox-core`）

```rust
// ports.rs —— 输入侧多一种"从哪来"
pub enum CaptureTarget {
    Microphone(Option<String>),
    ProcessLoopback { executable: String, include_tree: bool },
    /// 网络对端（媒体面）。`pipe` 是清单里那个逻辑名（v1 只有 `"default"`）。
    Net { pipe: String },
}

// pipeline/mod.rs —— 作业单多一格"往网络去"、Deps 多一个工厂
pub(crate) struct Plan {
    // …既有字段不动…
    /// 网络出口的 pipe 名（`None` = 这条腿不出网）。
    pub net_out: Option<String>,
}

/// 网络出口工厂：pipe 名 → 一个"看起来像播放汇"的东西（f32 进、网络出）。
pub type NetOutFactory = Box<dyn Fn(&str) -> Box<dyn PlaybackSink> + Send + Sync>;

pub struct Deps {
    pub transport: TransportFactory,
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    /// `None` = 这个外壳不接媒体面（桌面/Android 今天就是这样）。
    pub net_out: Option<NetOutFactory>,
    pub denoise: DenoiseFactory,
    pub resample: ResampleFactory,
}
```

`Plan::from` 的两处映射：

```rust
// in.first()
Some(Input::NetIn { pipe, .. }) => CaptureTarget::Net { pipe: pipe.clone() },
// HostFeed 仍然拒（插件宿主那一轮）
Some(Input::HostFeed { .. }) => return Err(PortError::new("清单里的输入还没实现（host_feed 归插件宿主那一轮）。")),

// out
net_out: composition.out.iter().find_map(|o| match o {
    Output::NetOut { pipe, .. } => Some(pipe.clone()),
    _ => None,
}),
```

`boot()` 的两处新分支（**都在既有分支旁边，形状照抄**）：

```rust
// ① 网络出口：与 playback 并列的第二个 sink（推的时候两份都推）
if let Some(pipe) = self.plan.net_out.clone() {
    let factory = self.deps.net_out.as_ref().ok_or_else(|| {
        PortError::new("这台设备没接媒体面（外壳没注入 net_out 工厂）。")
    })?;
    let mut sink = factory(&pipe);
    sink.open(None, OUTPUT_SAMPLE_RATE)?;   // 直通时用 format.sample_rate
    self.net_sink = Some(sink);
}
// ② 推的时候（两处推送点各加一行，形状与 monitor_sink 逐字相同）
if let Some(sink) = self.net_sink.as_mut() {
    sink.push(&samples);
}
```

`net_sink` 是 **`Worker` 的字段**（与 `sink` / `monitor_sink` 并列，`pipeline/mod.rs:638-639`），**不是** `Plan` 的字段：
`Plan` 只说"往网络去，pipe 叫什么"，句柄归 Worker。

**为什么用第二个 sink 而不是替换主 sink**：清单的 `out` 是**数组**，它已经能表达
"本地出声 + 同时发给网络"；`Plan` 只有一个 sink 是现状的窄点，`monitor_sink` 就是同一个窄点的现成先例
（`pipeline/mod.rs:1268-1270`）。**不加新 trait**：`net_out` 就是"一个恰好通往 socket 的播放汇"。

#### 2.6.2 媒体面（`crates/vox-net/src/media/`）

```rust
// frame.rs —— 纯数据，不碰网络、不碰线程（照 cloud/protocol.rs 的分工）
pub const MAGIC: [u8; 2] = *b"VB";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 20;
pub const MAX_PAYLOAD: usize = 8 * 1024;

pub enum FrameKind { Pcm16Le = 0, KeepAlive = 1 }

pub struct FrameHeader {
    pub kind: FrameKind,
    pub seq: u32,
    pub ts_ms: u32,
    pub rate: u32,
    pub channels: u16,
}

pub enum MediaError {
    Short, Magic, Version, Kind(u8), Flags(u16), Rate { got: u32, want: u32 },
    Channels(u16), Oversize(usize), KeepAlivePayload(usize), TextFrame,   // ← 落地时补的取值（原列表 9 种 → 10 种），见 §0.3
}

pub fn encode(header: &FrameHeader, payload: &[u8], out: &mut Vec<u8>);   // out 复用，热路径不新增分配
pub fn decode(bytes: &[u8], want_rate: u32) -> Result<(FrameHeader, &[u8]), MediaError>;

// pipe.rs —— 抖动缓冲 + 重切块 + paced drain + 统计
pub struct PipeConfig {
    pub block_ms: u32,        // 20
    pub jitter_ms: u32,       // 40
    pub pad_ms: u32,          // 200
    pub queue_ms: u32,        // 160
    pub keepalive_ms: u32,    // 1000
    pub peer_timeout_ms: u32, // 3000
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PipeStats {
    pub frames_in: u64, pub frames_out: u64,
    pub bytes_in: u64, pub bytes_out: u64,
    pub dropped_ms: u64,      // 过载丢最旧
    pub padded_ms: u64,       // 欠载补静音
    pub idle_ms: u64,         // 连续欠载超过 pad_ms、已停止回调的时长
    pub seq_gaps: u64, pub bad_frames: u64, pub rate_mismatch: u64,
    pub connected: bool, pub reconnects: u64, pub last_frame_age_ms: u64,
}

// server.rs —— 入站（外壳装配期 bind；定义者在这里）
pub const PATH: &str = "/audio";

pub struct MediaOptions {
    pub listen: SocketAddr,
    pub rate_hz: u32,
    pub token: String,
    pub allowed_origins: Vec<String>,
    pub pipe: PipeConfig,
}

pub struct MediaListener { /* Arc<共享态> */ }

impl MediaListener {
    /// 真的绑上才算（`net_in` 的定义者）。失败 → `PortError`。
    pub fn bind(options: MediaOptions) -> PortResult<Self>;
    pub fn local_addr(&self) -> SocketAddr;
    /// 腿级的采集源（一次会话一个）。同一时刻只服务一条腿。
    pub fn capture(&self) -> Box<dyn CaptureSource>;
    pub fn stats(&self) -> PipeStats;
    pub fn shutdown(&self);
}

// client.rs —— 出站（外壳装配期起池；定义者在这里）
pub struct MediaOut { /* Arc<共享态> */ }

impl MediaOut {
    /// 起出站池（含退避重连线程）。连不上**不算失败**（那是链路状态，不是能力）。
    pub fn spawn(peer: String, token: String, pipe: PipeConfig) -> PortResult<Self>;
    pub fn sink(&self) -> Box<dyn PlaybackSink>;
    pub fn stats(&self) -> PipeStats;
    pub fn shutdown(&self);
}

// mod.rs —— 门面 + 凭据生成（S3 后半：今天只落了门面，没有 `random_token`）
pub const DEFAULT_PIPE: &str = "default";

/// 32 字节随机 → base64url 无填充 43 字符（§2.5.3）。
///
/// 与 `vox-mcp` 的 `random_token` 同形状、两份实现：媒体面不依赖控制面 crate（§2.5.1
/// "两边互不可用"），也不值得为 6 行 helper 提一个公共 crate。两边的契约是长度与字符集
/// （43 个 base64url 字符），各自有用例钉住。
pub fn random_token() -> PortResult<String>;
```

两个 trait 实现的行为契约（写进文档注释，供 verifier 对表）：

| trait 方法 | `MediaListener::capture()` | `MediaOut::sink()` |
| --- | --- | --- |
| `CaptureSource::start(target, block_ms, on_chunk)` | 只认 `CaptureTarget::Net { pipe }`（别的 → `PortError`，**不静默换源**）；立刻返回 `CaptureFormat { sample_rate: rate_hz, channels: 1 }`；**在第一条帧到达前不发任何 `on_chunk`** | — |
| `CaptureSource::stop()` | 保证回调不再触发（排空线程先停，再断连接） | — |
| `PlaybackSink::open(device, source_rate)` | — | `device` 必须为 `None`（非 `None` → `PortError`：网络出口不是设备）；返回线上的 `rate`（= `source_rate`） |
| `PlaybackSink::push(&[f32])` | — | 攒够 `block_ms` 发一帧；队列满**丢最旧**，绝不阻塞（trait 契约原文） |
| `PlaybackSink::stats()` | — | 把 `PipeStats` 映射到 `PlaybackStats`（`queued_samples` / `dropped_samples` / `sample_rate` / `channels`；`rendered_samples` 记"真的写进 socket 的样本数"） |
| `PlaybackSink::flush()` / `close()` | — | `flush` **非阻塞**：只记一个意图，管子线程按"整块 → 余头 → 保活"收尾，**保活发出去才算这次 flush 办完**（§0.3 第 3 条）；`close` 关连接、停池（幂等） |

#### 2.6.3 无屏外壳（`crates/vox-headless/`）

```rust
// 新文件 src/media.rs —— 媒体面的装配（形状照 mcp.rs：起、注入、写/擦凭据文件、关机）
pub const MEDIA_FILE: &str = "media.json";      // 与 control.json 同目录

/// 凭据发布：`{"listen":"127.0.0.1:47100","token":"<43>","pid":1234,"rate_hz":16000}`
///
/// token 三步（顺序定死，§2.5.3）：① `secrets.load_secret("net_audio.token")`；
/// ② `None` 就 `vox_net::media::random_token()` 生成 + `store_secret`（存不下 → 一条 `Notice::warning`
/// 并继续用临时 token）；③ 交给 `MediaOptions` / `MediaOut::spawn`。
/// 出站那一侧另读 `secrets.load_secret("net_audio.peer_token")`——没有它 = 出站不启（`not_wired`）。
pub fn start(settings: &Settings, secrets: &dyn SecretStore, config_dir: &Path)
    -> Result<Option<MediaHandle>, Error>;

pub struct MediaHandle { /* listener: Option<MediaListener>, out: Option<MediaOut> */ }

impl MediaHandle {
    /// `net_in` / `net_out` 的凭据来源（定义者）。
    pub fn listener(&self) -> Option<&MediaListener>;
    pub fn out(&self) -> Option<&MediaOut>;
    /// 采集合成：设备那份 + 网络那份（按 `CaptureTarget` 分派）
    pub fn capture_factory(&self, device: CaptureFactory) -> CaptureFactory;
    /// 出站工厂（`Deps.net_out`）
    pub fn net_out_factory(&self) -> Option<NetOutFactory>;
    pub fn stats(&self) -> Option<PipeStats>;
    pub fn shutdown(&self);   // 先断连接再擦 media.json
}
```

装配次序（`headless.rs`）：

1. `Assembly` 里（**只读模式不做这一步**）：`Settings.net.enabled` 为真时 `media::start(...)`
   → `MediaListener::bind` / `MediaOut::spawn` → 写 `media.json`。
2. `Daemon::start`：`Deps { capture: media.capture_factory(platform.capture), net_out: media.net_out_factory(), .. }`。
3. `set_host_facts(platform::host_facts(&media))` —— **事实注入排在媒体面之后**（事实没齐就开门 = 广告做不到的事，
   与 S0 §2.5.0 第 3 步、`headless.rs` 的"控制面排在事实注入之后"同一条纪律）。
4. `Daemon::shutdown`：先停流水线、再 `media.shutdown()`（擦 `media.json`）、最后 `persist.flush()`。

`platform::host_facts()` 的新签名与内容：

```rust
pub fn host_facts(media: Option<&MediaHandle>) -> HostFacts {
    let mut off = BTreeMap::new();
    off.insert(Capability::BackgroundService, UnavailableReason::NotWired);  // 既有
    // 媒体面：总闸关 → disabled；配了但没起来 → not_wired；端口被占 → busy
    match media {
        Some(m) if m.listener().is_some() => {}                              // net_in 为真
        _ => { off.insert(Capability::NetIn, media_reason()); }
    }
    match media {
        Some(m) if m.out().is_some() => {}                                    // net_out 为真
        _ => { off.insert(Capability::NetOut, media_reason()); }
    }
    HostFacts { host: host_kind(), off, virtual_mic_device: None }
}
```

---

## 3. 改动清单

> 一个文件一个 owner；跨 owner 的改动找 Main 排期。**标 ★ 的必须同轮落地**（否则中间态是外壳 bug）。

### 3.1 core-dev（`crates/vox-core/`）

> 除标 **已落地** 的行外，本表**全部属 S3 后半（未做）**。

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| `src/capability.rs` | 修改 ★ | ① `host_ceiling(HostKind::LinuxHeadless)` 加 `NetIn` / `NetOut`（§2.2.3）；② 头注释与 `Capability::NetIn/NetOut` 的文档注释改成"定义者 = 媒体面监听/出站池真的起来（S3）"，并把**出站这一位有意比入站弱**的理由写进去；③ `UnavailableReason` 加 `Disabled` |
| `src/ports.rs` | 修改（**已落地**） | `CaptureTarget` 加 `Net { pipe: String }`（含文档注释：媒体面，v1 只有 `"default"`；**设备采集实现遇到它必须报错**，不许当默认设备——四处穷尽 `match` 已按此迁，见 §1.6） |
| `src/ports.rs` | 修改 ★（**未做**） | `SecretStore` 加三个**键值式**方法：`load_secret(&self, key: &str) -> PortResult<Option<String>>` / `store_secret(&self, key: &str, value: &str) -> PortResult<()>` / `clear_secret(&self, key: &str) -> PortResult<()>`。**四个实现同轮迁**（`SecretFile` / `DpapiSecretStore` / `SecretServiceStore` / 测试里的 `MemoryStore`，见 §1.6）。两条硬要求：① **键名兼容**——服务商那套今天的键名（`SecretFile` 的 JSON 键、DPAPI 的"每服务商一个文件"、Secret Service 的 `api-key.<id>` 条目名）**保持现状**，否则老用户盘上已存的密钥会读不出来；② DPAPI 那份里的私有 `store_secret(path, key)` / `load_secret(path)` / `clear_secret(path)` 与新的 trait 方法**撞名**，同轮改名（如 `write_encrypted` / `read_encrypted` / `remove_encrypted`） |
| `src/pipeline/mod.rs` | 修改 ★（**未做**，仅测试） | 测试用的 `MemoryStore` 补三个键值式方法（键值用现成的 map 即可） |
| `src/settings.rs` | 新增 + 修改 | 新增 `NetSettings`（§2.3.4）+ `Settings.net` 字段 + `Default`（**全关、fail-closed**）+ `normalize()` 的九条夹紧规则 + 单测（缺段读出来全关、夹紧边界、`peer_timeout_ms > keepalive_ms`） |
| `src/pipeline/mod.rs` | 修改 | ① `Deps` 加 `net_out: Option<NetOutFactory>`（+ `NetOutFactory` 类型别名）；② `Plan` 加 `net_out: Option<String>`；③ `Plan::from` 把 `Input::NetIn` → `CaptureTarget::Net`、`Output::NetOut` → `Plan.net_out`，**删掉**那句"`net_in` / `host_feed` 归 S3"的 `PortError`（只留 `HostFeed`）；④ `boot()` 造第二个 sink（`net_sink`）并在两处推送点各加一行；⑤ `shutdown` 里 `close_sink(&mut self.net_sink)` |
| `src/pipeline/listen.rs` | 修改 ★ | 输入选择：`net_in` 位开着且 `config.net_in.is_some()` → `Input::NetIn`；**`loopback_target` 的 `ok_or` 只在没有 `net_in` 时才要求** |
| `src/pipeline/speak.rs` | 修改 ★ | 输出选择：`net_out` 位开着且 `config.net_out.is_some()` → `Output::NetOut { pipe, source }`（与既有的 `playback` 并列，不替换） |
| `src/runtime.rs` | 修改 ★ | ① `SessionConfig` 加 `net_in` / `net_out` 两格；② `derive_session_config` 按 §2.3.4 的表填；③ `start()` 的 Listen 守卫加 `&& !net_listen_enabled(settings)`；④ `Snapshot` 里带一份媒体面统计（`Option<PipeStats>` 的只读投影，供状态出口用）——**若判定为不必要的公开面，可只进日志**（留给实现时定，见 §5 未决） |
| `src/composition.rs` | 修改 | ① 端点那条钉子用例从 `assert!(Plan::from(&endpoint).is_err())` 改成"**能派生出直通作业单**"（`target == CaptureTarget::Net{..}`、`passthrough`、`net_out == Some("default")`）；② `Input::NetIn` / `Output::NetOut` 的注释去掉"[S3 目标，现状未实现]" |

### 3.2 shell-dev（`crates/vox-net/` + `crates/vox-audio-{linux,win}/` + `crates/vox-headless/` + `app/ui/` + `app/src-tauri/`）

> 「动作」列里标 **已落地** 的行是帧层这一轮真的动过的文件（对照 `git status --short crates/vox-net crates/vox-core/src/ports.rs`：`M crates/vox-net/Cargo.toml`、`M crates/vox-net/src/lib.rs`、`?? crates/vox-net/src/media/`、`?? crates/vox-net/tests/`、`?? crates/vox-net/examples/`）；**其余（含标"未做"的）都是 S3 后半**。

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| `crates/vox-net/src/lib.rs` | 修改（**已落地**） | 加 `pub mod media;` + 一句模块文档（两条管子不共用协议/路径/凭据）。**本稿第 1 版漏了这个文件**——帧层实际动过它 |
| `crates/vox-net/src/media/mod.rs` | **新增**（**已落地**） | 子模块门面 + 文档注释（协议形状、与 `ws.rs` 的边界：`Transport` 契约一个字不动）+ `DEFAULT_PIPE` + 收紧的 `ws_config()` |
| `crates/vox-net/src/media/mod.rs` | 修改（**未做**） | 加 `pub fn random_token() -> PortResult<String>`（§2.6.2）：32 字节随机 → base64url 无填充 43 字符。`random_token` 今天是 `crates/vox-mcp/src/transport/http.rs` 的**私有 fn**（§1.6），`vox-net` 调不到、也不该依赖控制面 crate |
| `crates/vox-net/src/media/frame.rs` | **新增**（**已落地**） | 帧编解码（§2.1.3、§2.6.2）+ 5 条单测（线上布局逐字节、往返、每种 `MediaError`、保活帧 20 字节无载荷、与芯的 PCM16 助手等价） |
| `crates/vox-net/src/media/pipe.rs` | **新增**（**已落地**） | 抖动环形缓冲 + paced drain + 欠载/过载 + `PipeStats`（纯逻辑，时钟注入 → 确定性单测）+ 5 条单测 |
| `crates/vox-net/src/media/server.rs` | **新增**（**已落地**） | `MediaListener`：绑定、鉴权（两条凭据通道）、Origin 白名单、单路径 `/audio`、只服务一条腿、`CaptureSource` 实现；**入站侧也按 `keepalive_ms` 发保活**（§0.3 第 2 条） |
| `crates/vox-net/src/media/client.rs` | **新增**（**已落地**） | `MediaOut`：出站池 + 退避重连 + 攒块 + 保活 + `PlaybackSink` 实现；`flush` 的收尾次序与 `flush_done` 的置位时机见 §0.3 第 3 条 |
| `crates/vox-net/examples/media_probe.rs` | **新增**（**已落地**） | 真机探针（照 `frame_loop_probe` 的先例）：`--listen` / `--url` / `--token` / `--seconds` / `--out` / `--help`；同进程既听又说 = 自测 |
| `crates/vox-net/tests/media_pipe.rs` | **新增**（**已落地**） | 13 条集成用例（§4.2 的 12 条 + 一条"两个端口只服务自己那一格"的契约用例） |
| `crates/vox-net/Cargo.toml` | 修改（**已落地**） | 描述改成"网络传输：云端 WS + 媒体面音频管子"；tokio features 显式加 `net`（此前靠 `tokio-tungstenite` 的 feature 合并带进来，**别继续靠它**）；加 `parking_lot.workspace = true`（入站排空线程的 `Mutex + Condvar`） |
| `crates/vox-net/Cargo.toml` | 修改（**未做**） | `random_token` 要的两个依赖：`base64.workspace = true` + `getrandom = "0.3"`（**workspace 根没有 `getrandom`**，照 `crates/vox-mcp/Cargo.toml` 的写法在 crate 里声明，见 §1.6） |
| `crates/vox-audio-linux/src/capture.rs` | 修改（**已落地**） | `resolve_plan` 补 `CaptureTarget::Net { .. }` 分支：**报错**（网络音频由 `vox-net` 的监听侧提供），不许静默换源 |
| `crates/vox-audio-linux/examples/smoke.rs` | 修改（**已落地**） | 打印用的 `match` 补 `Net` 分支（例子里只描述、不采集） |
| `crates/vox-audio-win/src/capture/mod.rs` | 修改（**已落地**） | 两处 `match target` 补 `Net` 分支：都**报错**（同上） |
| `crates/vox-headless/src/media.rs` | **新增**（**未做**） | 装配媒体面 + `media.json` 发布/擦除 + token 三步（§2.6.3）+ `capture_factory` 分派壳 + `net_out_factory` |
| `crates/vox-headless/src/secrets.rs` | 修改（**未做**） | `SecretFile` 迁到键值式 `SecretStore`（§3.1）：JSON 键**保持现状**，新增 `net_audio.token` / `net_audio.peer_token` 两个键 |
| `crates/vox-headless/src/headless.rs` | 修改 ★（**未做**） | `Assembly` 起媒体面（只读模式不做）；`Daemon::start` 注入 `Deps` 两处（`Deps { … }` 字面量就在这里）；`set_host_facts` 排在媒体面之后；`shutdown` 先停流水线再 `media.shutdown()`；`legs_to_check` 把"配了媒体面"也算作"要验 Listen 腿" |
| `crates/vox-headless/src/platform/linux.rs` | 修改 ★（**未做**） | `host_facts(media)` 新签名与两位的报法（§2.6.3）；**改掉**用例 `the_report_matches_the_headless_tier` 里"`NetIn` / `NetOut` / `FileConfig` 不许为真"那段：前两位改成"按句柄报"（有句柄 → 真；无句柄 → `not_wired`；总闸关 → `disabled`），**`FileConfig` 那一位保持不动**（它归另一轮） |
| `crates/vox-headless/src/config.rs` | 修改 | 加 `MEDIA_FILE` 常量（与 `CONTROL_FILE` 同目录同形状） |
| `crates/vox-headless/src/status.rs` | 修改 | 把媒体面统计进状态出口（一条 `tracing` 结构化事件 + `--print-composition` 的 `capabilities` 已经带位，不用改形状） |
| `crates/vox-headless/src/cli.rs` | 修改 | `--help` 增一段"媒体面（`settings.json` 的 `net` 段）"；**不加新开关**（配置面负责） |
| `crates/vox-headless/settings.example.json` | 修改 | 加 `net` 段（全关 + 注释性取值） |
| `crates/vox-headless/README.md` | 修改 | §5"还没做（不许广告）"里 `net_in`/`net_out` 两行按实情改写 |
| `app/src-tauri/src/lib.rs` | 修改（**未做**） | `Deps` 补 `net_out: None`（桌面档不接媒体面；清单也到不了那一格）。`Deps` 今天只有两个构造点：`app/src-tauri/src/lib.rs` 与 `crates/vox-headless/src/headless.rs`（§1.6） |
| `app/src-tauri/src/sys/secrets.rs` | 修改（**未做**） | `DpapiSecretStore` 迁到键值式 `SecretStore`：**文件名规则保持现状**（`DpapiSecretStore::path_for`：`Aliyun` 走基准文件名，其余 `{stem}-{id}.{ext}`），键值式方法按同一套规则落盘；三个**按路径**的私有 `store_secret` / `load_secret` / `clear_secret` 同轮改名（§3.1） |
| `app/src-tauri/src/platform/linux/secrets.rs` | 修改（**未做**） | `SecretServiceStore` 迁到键值式 `SecretStore`：条目名由 `user_for(provider)` 泛化（`api-key.<id>` / `net_audio.token` / `net_audio.peer_token`） |
| `app/ui/src/capabilities.ts` | 修改 ★（**未做**） | `REASON_KEY`（`Record<UnavailableReason, string>`，**穷尽**）加 `disabled: "capabilities.reason.disabled"`。**不改它就 `tsc` 红**（缺键）；`check-capabilities.mjs` 第 [7] 条遍历读的 `UNAVAILABLE_REASONS` 就是由这张表的键导出的 |
| `app/ui/src/types.snapshot.ts` | 修改 ★（**未做**） | `UnavailableReason` 联合类型加 `"disabled"`（与上一行**必须同轮**） |
| `app/ui/src/i18n/zh.ts` + `app/ui/src/i18n/en.ts` | 修改 ★（**未做**） | `capabilities.reason.disabled` 两条文案（"媒体面没打开：去 `settings.json` 的 `net` 段打开" / "Media plane is off…"）。**文件名就是 `zh.ts` / `en.ts`**——本稿第 1 版写的 `zh-CN.ts` **不存在**；**`ja.ts` 不加**（冻结包，`Omit<DictShape, "capabilities">`，缺的键回落 zh，见 §1.6） |
| `app/ui/scripts/check-capabilities.mjs` | 修改 ★（**未做**） | `REASONS` 加 `"disabled"` + `REASON_MARK` 加一条特征文案（第 [7] 条遍历自动覆盖到；zh 与 en 都要命中），并把脚本里"七种 reason"的注释改成八种 |

### 3.3 agent-face-dev（`crates/vox-mcp/`）

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| — | **无改动** | `describe_endpoint` 的 `capabilities` 直接读 `CapabilityReport`，两位进上限后**自动出现**；`editable` 表**故意不加** `Settings.net`（§2.5.5）。若实现时发现 `crates/vox-mcp/tests/endpoints.rs` 里"无屏档这两位不许为真"一类断言存在，**改成按事实断言**（那是本轮唯一可能的改动） |

### 3.4 docs-scribe（文档回填，不在本稿权限内）

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| `docs/platform/EMBEDDED.md` | 修改 | §2 表 `net_in` / `net_out` 两行改成"已落地（定义者 = 监听/出站池）"；§1 的"端点形态（S3 目标，现状未实现）"按实情改 |
| `docs/architecture/DIRECTIONS.md` | 修改 | §6.1 第 2、3 条（心跳来源 / 管子选型）标为**已答**并回链本稿；§10.2 的 S3 行补"媒体面"这一件 |
| `docs/plans/S0-COMPOSITION-MANIFEST.md` | 修改 | §2.5.1 位表"无屏 ARM64"列的两位、§3.4"不动 `CaptureTarget`"那条按实情回填 |

### 3.5 删除

> 三条今天**都还在**（本轮核过）——它们全属 S3 后半。另外：`CaptureTarget::Net` 落地时**不是删**，
> 而是给四个穷尽 `match` 各加一个**报错**分支（§1.6 已列文件）。

| 位置 | 删什么 | 为什么 |
| --- | --- | --- |
| `crates/vox-core/src/pipeline/mod.rs` 的 `Plan::from`（`Input::NetIn \| Input::HostFeed` 那条 `PortError`） | "`net_in` / `host_feed` 归 S3"整句 → 只留 `host_feed` | 这一轮到期 |
| `crates/vox-headless/src/platform/linux.rs` 的用例 `the_report_matches_the_headless_tier` | "`NetIn` / `NetOut` 不许为真"那两位 | 它钉的是"还没实现"，本轮实现落地 → 换成"按句柄报"的两条（`FileConfig` 那一位留着） |
| `crates/vox-core/src/composition.rs` 的用例 `an_endpoint_is_the_minimal_instance` | "端点今天跑不起来"那条断言（`Plan::from(&endpoint).is_err()`，"网络进/网络出还没实现，不许被装成今天的作业单"） | 同上（换成"能派生出直通作业单"） |

---

## 4. 验收标准

### 4.1 命令（离线、可复跑）

```bash
cargo test --workspace            # 跨 crate：芯 + vox-net + 无屏外壳 + app + vox-mcp
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p vox-net --test media_pipe       # 帧 + 管子：13 条，真 socket，全在回环 —— 今天可跑
cargo run -p vox-net --example media_probe -- --help   # 探针自报用法（`--help` / `-h`）—— 今天可跑
```

> **可跑性标注（第二十三轮核）**：后两条今天就能跑（帧层已落地，§1.6）。前三条是全量门，
> **本轮只改文档，一条都没重跑**（改 `crates/` 的是帧层那几位，全量门由 Main 在收尾时统一跑；
> 第二十三轮的记录：`cargo test --workspace` = 601 passed / 0 failed / 5 ignored，clippy `-D warnings` 0，fmt 干净）。

### 4.2 `crates/vox-net/tests/media_pipe.rs`：每条断言都对应一个可观察契约

| # | 用例 | 钉什么 |
| --- | --- | --- |
| 1 | `a_round_trip_is_byte_identical` | 监听侧 ↔ 出站侧在 `127.0.0.1:0` 上跑：灌 2 s 已知 PCM16@16k，收到的样本**逐字节相等**、顺序正确、`frames_in == frames_out` |
| 2 | `a_burst_is_reblocked_to_the_manifest_block_ms` | 一次发 100 ms（5 块）的突发 → 排空侧**每块 320 样本**（@16k）地回调，块数与总样本数守恒 |
| 3 | `the_jitter_buffer_prefills_before_the_first_callback` | 预填未满 `jitter_ms` 前**一次回调都没有**；满了之后按 `block_ms` 节奏回调（用注入时钟，确定性） |
| 4 | `an_underrun_pads_silence_up_to_pad_ms_then_stops` | 欠载补静音；累计超过 `pad_ms` → **停止回调** + `idle_ms` 增长（"没有输入"不许被静音伪装） |
| 5 | `an_overrun_drops_the_oldest_block` | 环满丢最旧，`dropped_ms` 增长，深度不变（照 `INPUT_QUEUE_SIZE` 的既有策略） |
| 6 | `no_peer_means_no_callback_at_all` | 只有监听、没有对端：`on_chunk` **零调用**、`padded_ms == 0`（"对端还没来" ≠ "抖动"） |
| 7 | `a_wrong_token_is_rejected_before_upgrade` | 错 token → `401`（**不是**"连上再关"）；`Origin` 不在白名单 → `403`；`allowed_origins` 为空时带 `Origin` 一律 403 |
| 8 | `the_browser_subprotocol_channel_works` | `Sec-WebSocket-Protocol: vox.media.v1.<token>` 能连上，且 101 回同一个子协议名 |
| 9 | `a_rate_mismatch_kills_the_connection` | 帧头 `rate` ≠ 声明率 → 断连 + `rate_mismatch` 计数（**不许**静默重采样） |
| 10 | `a_malformed_frame_is_counted_and_the_connection_dies` | 坏 magic / 版本 / `kind` / `flags` / `channels≠1` / 超长 / 文本帧：逐条 `bad_frames` + 断连 |
| 11 | `the_sink_coalesces_and_sends_a_keepalive` | `push` 攒成 `block_ms` 一帧；空闲 `keepalive_ms` 发一个 20 字节 `keepalive` 帧；`flush` 发余头 |
| 12 | `the_outbound_pool_reconnects_with_backoff` | 对端先不在 → `connected=false` 但**不算失败**；对端起来后自动连上，`reconnects` 增长 |
| 13 | `the_media_ports_refuse_what_they_do_not_serve` | 落地时补的一条（§2.6.2 的行为契约表）：采集源只认 `CaptureTarget::Net`（`Microphone` 与 `pipe != "default"` 都**报错**，不静默换源）；一个监听同时只服务一条腿 |

> 13 条 = 设计稿的 12 条 + 落地时补的第 13 条（契约表那一行本来就是 MUST，只是第 1 版没给它编号）。
> 另有 10 条**单测**在 `media/frame.rs`（5）与 `media/pipe.rs`（5）里——总计 23 条（见顶部的实现进度）。

### 4.3 芯侧：清单 ↔ 作业单

```bash
cargo test -p vox-core          # 清单/能力位/两条腿的既有用例全绿（"不改行为"的第一层证据）
```

**可观察行为**（`crates/vox-core` 的用例，不是人眼）：

- 无屏档 + 媒体面开着：Listen 腿的清单里 `in[0].kind == "net_in"`、`in[0].pipe == "default"`、`in[0].block_ms == 20`；
  `ops` 与现状 Listen **逐项相同**（不装 `denoise`）；`out[0]` 仍是 `playback{role: speaker}`。
- 无屏档 + 媒体面关着：Listen 腿 `in == []` → `validate()` 报 `MissingInput`（**与今天的形状一致，只是理由变了**）。
- `Composition::endpoint("default","default")` → `Plan::from` **成功**：`target == CaptureTarget::Net{pipe:"default"}`、
  `passthrough == true`、`net_out == Some("default")`；用假端口跑通"网络进 → 网络出"的直通（**这就是中继腿的逻辑，
  只是没有产品入口**）。
- `host_ceiling(LinuxHeadless)` 含两位、桌面两档与 Android **不含**；`HostFacts { off: {net_in: disabled} }`
  在上限之内（不是 `excess_off_bits`）。
- 桌面档的 `host_facts()` 报这两位 `false(unsupported)`（上限之外，外壳塞进 `off` 会被 `excess_off_bits` 抓住）。

### 4.4 无屏档真跑（本机可复现的端到端）

> **今天跑到哪一步（第二十三轮核）**：整段是 **S3 后半**的验收——② 要无屏外壳的媒体面装配
> （`crates/vox-headless/src/media.rs` 还没写），所以第②步之后**没有 `media.json` 可读**。
> 今天能跑的替代是直接对着探针自己的 `--listen` 跑（同进程既听又说，见 `media_probe.rs` 头注释）：
> 那验的是**帧层**（§4.2），不是外壳装配。
> ① 今天打出来的**不是** `not_wired` 而是 `{"enabled":false,"reason":"unsupported"}`——这两位还没进
> `host_ceiling(HostKind::LinuxHeadless)`（§2.2.3，S3 后半才进）。"上限之外 ⇒ `unsupported`"是既有语义
> （§1.6 的 `host_facts` 注释），不是缺陷；S3 后半落地后才变成"总闸关 ⇒ `disabled` / 没配 ⇒ `not_wired`"。
> ④ 需要 `sox`，**本机没装**（§1.6 的 `which` 证据），所以下面给了一条等价的 `python3` 读法（标准库，不引依赖）。

```bash
# ── 准备：一份开了媒体面的配置 ──
mkdir -p /tmp/vb && cat > /tmp/vb/settings.json <<'JSON'
{ "version": 2,
  "net": { "enabled": true, "listen": "127.0.0.1:0", "peer": "ws://127.0.0.1:47100/audio",
           "rate_hz": 16000 } }
JSON

# ── ① 只读模式：不许 bind、不许 connect ⇒ 两位如实报假（诚实，不是缺陷） ──
cargo run -p vox-headless -- --config /tmp/vb/settings.json --print-capabilities \
  | jq -c '.host.net_in, .host.net_out'
#   期望（S3 后半落地后）：{"enabled":false,"reason":"not_wired"} ×2
#   今天实际：{"enabled":false,"reason":"unsupported"} ×2（两位还没进无屏档的上限，见本节顶部的标注）

# ── ② 常驻模式：媒体面真的起来 ──
cargo run -p vox-headless -- --config /tmp/vb/settings.json --start listen --run-for 30 &
sleep 2
cat /tmp/vb/media.json
#   期望：{"listen":"127.0.0.1:<实际端口>","token":"<43 字符>","pid":<pid>,"rate_hz":16000}
ls -l /tmp/vb/media.json      # 期望：-rw-------（0600）

# ── ③ 真探针：连上去、发 2 s 已知音频、看回程与统计 ──
PORT=$(python3 -c "import json;print(json.load(open('/tmp/vb/media.json'))['listen'].split(':')[1])")
TOKEN=$(python3 -c "import json;print(json.load(open('/tmp/vb/media.json'))['token'])")
cargo run -p vox-net --example media_probe -- \
  --url "ws://127.0.0.1:$PORT/audio" --token "$TOKEN" --seconds 2 --out /tmp/probe.wav
#   期望：打印 frames_in / frames_out / dropped_ms / padded_ms / reconnects，且 dropped_ms == 0

# ── ④ 回程音频是真的（不是静音、不是空文件） ──
#    本机没有 sox（`which sox` 退出码 1，§1.6），用标准库读 WAV：
python3 - <<'PY'
import struct, wave
w = wave.open("/tmp/probe.wav")
n, rate, ch = w.getnframes(), w.getframerate(), w.getnchannels()
peak = max(abs(s) for s in struct.unpack(f"<{n * ch}h", w.readframes(n)))
print(f"Length={n / rate:.3f}s rate={rate} ch={ch} peak={peak / 32768:.3f}")
PY
#   期望：Length ≈ 2.0（秒）、peak > 0.3（我们发的是 0.5 幅度的正弦）
#   （装了 sox 的话 `sox /tmp/probe.wav -n stat` 等价，但那不是本机的前提）

# ── ⑤ 错 token 连不上（安全面） ──
cargo run -p vox-net --example media_probe -- --url "ws://127.0.0.1:$PORT/audio" \
  --token "wrong" --seconds 1
#   期望：退出码非 0 + 一句"401"，并且**没有**任何音频流过

# ── ⑥ 关机不留幽灵 ──
wait
test ! -f /tmp/vb/media.json && echo "OK：凭据文件已擦"
```

**日志侧（journald / stderr）**：`--start listen` 起来时能看见一条媒体面起停日志
（监听地址、rate、pipe 名）；非回环监听时必须有一条 warning；对端连接/断开各一条（**不含音频内容、不含 token**）。

### 4.5 明确**不**作为验收的东西

- 不断言"端到端延迟 < X ms"：媒体面的延迟预算没有实测基线（§5 第 4 条），拿一个编出来的数当门槛是自欺。
- 不断言"Opus/WebRTC 将来能接"：本稿只留位，不写实现承诺。
- 不跑真 ARM64 板：那是 S3 的**板上实测**项（`docs/platform/EMBEDDED.md` §3.10 第 9 条），本机 x86_64 上的
  回环验证**不能**代替它，但它是本轮的验收出口。

---

## 5. 风险与未决

### 5.1 已知风险（判断，不是"未核实"）

| 风险 | 说明 | 兜底 |
| --- | --- | --- |
| **TCP 队头阻塞**（选 WS 的主要代价，§2.1.2） | 丢包会变成一次可听见的卡顿 | 抖动缓冲 + 欠载补静音；真到了"公网必须流畅"的场景再上 WebRTC（清单与芯都不用改） |
| **位随链路抖动** | 出站"连着没连着"很容易被写成位 | 本稿把定义者定成"出站池起来了"，连接状态只进统计（§2.2.3）；这条要写进注释，否则会被"顺手修好" |
| **明文监听非回环** | v1 服务端只有 `ws://` | 一条 warning + 建议走隧道/反向代理；`wss` 服务端（要证书）留到 S4 |
| **两条腿抢一个监听** | 一条腿一条连接；第二条腿要 `net_in` 时会失败 | v1 的形态是"Listen 用 `net_in`、Speak 用 `net_out`"，不会撞；撞上时**报错**（`PortError`），不静默抢连接 |
| **`Disabled` 这个新 reason 要过 UI 那一关** | 多一个枚举值 = 一张**穷尽 `Record`**（`capabilities.ts::REASON_KEY`，不改 `tsc` 直接红）+ 联合类型（`types.snapshot.ts::UnavailableReason`）+ 两份 i18n（`zh.ts` / `en.ts`；`ja.ts` 是冻结包）+ 检查脚本的两张表 | 已在 §3.2 列进改动清单（**四个文件 + 一个脚本**） |
| **给 `SecretStore` 加键值式方法要碰 Windows 那份 DPAPI 实现** | 本机是 Linux，`app/src-tauri` 的 Windows 目标**编不了也测不了** → 那一路只能靠 Windows 机器/CI 验 | 三条硬约束把风险压住：① 键名保持现状（老密钥照读）；② 键值式方法与既有私有函数一一对应（换的只是"文件名/键名从哪来"）；③ 私有函数与 trait 方法**撞名** → 同轮改名（§3.1） |
| **token 存不下**（keyring 不可用 / 磁盘只读） | 跨机对端抄的那串会在本机重启后失效 | 装配期一条 `Notice::warning` + 用临时 token **照常起**（§2.5.3 末段）："能听人说话"优先于"token 跨重启稳定" |
| **`net_sink` 与 `monitor_sink` 并存** | 三份输出同时推（设备 + 回听 + 网络） | 推送点只有两处（`mod.rs:1110` / `:1264`），照 `monitor_sink` 的现成形状加一行；`close_sink` 一起收 |

### 5.2 不确定之处（末尾清单；**≥3 条**，全部标 `[未核实]`，不编）

1. **`[未核实]` 媒体面对端到底谁先说话（谁发起连接）**。本稿按"盒子监听 + 对端连过来"设计入站、
   按"盒子出站连对端"设计出站，**两端都在 NAT 后面时这两条都不通**（§2.1.2）。手机 ↔ 盒子的真实拓扑
   （谁在谁的局域网里、谁有公网地址）**没有实测**，所以"要不要一条反向连接/中继"这件事本稿**没有定**。
   触发条件：S2 手机壳落地后按真机拓扑再定。
2. **`[未核实]` 抖动缓冲目标深度该取多少**。缺省 `jitter_ms = 40` 是**照 2 块推的**，不是实测值：
   没有在真网络（Wi-Fi、移动网）上量过到达抖动分布，也没有量过"云端延迟 + 40 ms"在主观上是否可接受。
   本稿把它做成配置项正是因为它得能被实测调。触发条件：有真对端之后按统计（`padded_ms` / `dropped_ms`）调。
3. **`[未核实]` PCM16 的带宽在移动网络上的真实代价**。256 kbit/s（16 kHz）与 384 kbit/s（24 kHz）
   是算出来的，不是量出来的：没有量过手机流量、没有量过弱网下的实际可用性，也没有验证过
   "出站降到 16 kHz 后译音质量还能不能接受"。
4. **`[未核实]` 端到端延迟基线**。媒体面自己的开销（预填 + 块 + socket）没有实测，
   也不知道它在整条链路（采集 → 云端 → 播放）里占多少。所以 §4.5 明确不设延迟门槛。
5. **`[未核实]` `tokio-tungstenite` 0.30 服务端的两个细节**：① `accept_async` 之后
   服务端是否**自动回 Ping**（`ws.rs` 只核实过**客户端**会自动回）；② 能否在握手阶段读到
   `Sec-WebSocket-Protocol` 与 `Origin` 的原始头（做子协议凭据与 Origin 白名单要用）。
   两者都能在实现时用一条用例立刻证伪/证实，本稿不猜。
6. **`[未核实]` 浏览器侧的实际可行性**：`AudioWorklet` 里打包 PCM16 并 `postMessage` 到主线程发 WS，
   这条路径的**延迟与抖动没有实测**；浏览器页面切后台被限速对"一直开着"的影响也没量过。
   （`DIRECTIONS.md` §2.5.2 只记了"标签页切后台会被限速/暂停"，没有量化。）
7. **`[未核实]` 统计要不要进 `Snapshot`**（`runtime.rs` 那一格）。进：Agent 面与界面都能看见管子状态，
   但要给芯加一个"媒体面"的公开面（芯不该认识 socket）；不进：无屏档只在日志/状态出口里看得到。
   本稿倾向**不进 `Snapshot`**（芯保持不认识网络），但这一条留给实现时按"状态出口够不够用"定。
8. **`[未核实]` 多管道（一份配置两条 `net_in`）**。本稿 v1 只认 `"default"`（§2.3.1），
   理由是"留一个没人维护的字段不如不留"（与 `DIRECTIONS.md` §10.0 砍 MCU 留位同款判断）；
   但"一台盒子同时听两路网络音频"是不是真实需求，**没有用户输入**。

### 5.3 与既有文档的耦合（本稿不越权改，交对应 owner）

| 文件 | 耦合点 | owner |
| --- | --- | --- |
| `docs/platform/EMBEDDED.md` | §2 表两位的"实现前恒假"、§1 的"端点形态（现状未实现）" | docs-scribe（本轮 `EmbeddedTableSplit` 正在动 §3.3，**别撞车**） |
| `docs/plans/S0-COMPOSITION-MANIFEST.md` | §2.5.1 位表、§2.5.4 定义者表、§3.4"不动 `CaptureTarget`" | 本稿给出回填内容，改由 S0 作者那一轮做 |
| `docs/plans/S1-AGENT-FACE.md` | §2.4.3"音频走媒体面"（已一致）；`editable` 表不动（§2.5.5） | agent-face-dev |
| `docs/architecture/DIRECTIONS.md` | §6.1 第 2/3 条、§10.2 S3 行 | Main |

---

## 6. 给下一轮的输入

1. **帧层已落地，接着做 S3 后半**：开工前先看顶部的「实现进度」——§3 里标 **已落地** 的行**不要重做**，
   三处偏离（§0.3）就是落地后的口径。剩下的是 §3 里标 **未做** 的行，验收仍是 §4 的两档（离线 + 真跑）。
2. **本稿唯一新增的公共接口是 `SecretStore` 的三个键值式方法**（§3.1）：它是"媒体面凭据要持久、又不能进配置"
   这条约束的唯一出口。四个实现**必须同轮迁**，且**键名兼容是硬要求**（老用户盘上的密钥要照读）。
3. **必须同轮落地的三处耦合**（否则中间态是外壳 bug）：`capability.rs` 的上限表 ↔
   `vox-headless/src/platform/linux.rs` 的 `host_facts`；`listen.rs` 的输入选择 ↔ `runtime.rs` 的启动守卫；
   `Deps.net_out` ↔ 两个外壳的装配（`app/src-tauri/src/lib.rs` 补 `None`）。
4. **留给下一轮拍的两个岔路**（都不阻塞本稿）：`wss` 服务端（要证书分发）、
   纯中继腿的产品入口（要加第三条腿，S0 §3.4 已把它排在清单落地之后）。
5. **S2（手机壳）与本稿的关系**：手机端既可以是"对端"（页面/App 连盒子），
   也可以是"宿主"（本机 loopback 连自己的芯）——本稿的管子两种都能服务，手机那一轮**不需要第二套音频协议**。
