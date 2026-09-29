> 状态：已完成（2026-09-21）｜ 条目已全部落地（WASAPI 样板抽到 `crates/vox-audio-win/src/capture/shared.rs`、`ProcGuard` 消失、`build.rs` 合成 `MiniCatalog`、`vox-audio-win/src/resample.rs` 改成 `Resample` trait 等，逐条清单见 `docs/architecture/DIRECTIONS.md` §7；仅 §4.2 一条按本文自评“收益小，可不动”）｜ 保留作留痕，别照它施工。

# Vox 后端 Rust 代码体检报告

**扫描范围**：`crates/vox-core`、`vox-net`、`vox-dsp`、`vox-audio-win`、`vox-input-win`、`vox-overlay-win` 全部 `.rs`，加 `app/src-tauri/src/` 装配层。只读不编译。

**严格遵守四条边界（刻意设计，不报）**：
1. 跨平台设计是刻意的——内核用 trait 抽象，不直接调平台 API；
2. -win crate 不做假实现，没 cfg 桩是故意的；
3. 降噪用 RNNoise 而非 DeepFilterNet（已拍板 DECISIONS.md B1）；
4. 当前只做 Windows 单端，跨平台已明确延后（docs/platform/SCOPE.md）。

---

## 一、冗余与重复

### 1.1 `capture/mic.rs` 与 `capture/endpoint.rs` 的 WASAPI 初始化样板几乎逐行重复
- **位置**：`crates/vox-audio-win/src/capture/mic.rs:40-97` 与 `crates/vox-audio-win/src/capture/endpoint.rs:22-71`
- **问题**：两条打开设备的路子——GetMixFormat → 闭包里 parse_format + initialize_min_period（失败回退 initialize_default_period）→ CoTaskMemFree → create_stream_event → SetEventHandle → GetService → Start——结构完全一致，唯一差别是方向标志（CAPTURE vs RENDER）和 `AUDCLNT_STREAMFLAGS_LOOPBACK`、以及日志文案。
- **证据**：两个文件第 48-84 行 / 30-58 行的 `let (client, info) = unsafe { ... }` 块是同一个骨架，只换了 `flow` 和文案字符串。
- **严重度：中**
- **建议**：抽一个 `open_capture_client(flow, stream_flags, err_prefix)` 把"拿到 IAudioClient 之后"的全流程收成一处，mic/endpoint 各自只剩方向和标志两个参数。

### 1.2 句柄守卫（`ProcGuard`/`DeviceSetGuard`）复制 6 处
- **位置**：`policy.rs:239-248`（ProcGuard）、`cable.rs:542-551`、`cable.rs:1046-1055`、`cable.rs:1140-1149`（ProcGuard 共 4 份）、`cable.rs:194-203`、`cable.rs:269-278`（DeviceSetGuard 共 2 份）
- **问题**：同一个"包 Win32 句柄、Drop 关一次"的模式以函数内嵌 struct 复制了 6 次。而 `com.rs:104-131` 已经有等价的 `OwnedHandle`（通用 HANDLE 守卫）。
- **证据**：`cable.rs:542` 的 `struct ProcGuard(HANDLE); impl Drop for ProcGuard { fn drop(&mut self) { unsafe { CloseHandle(self.0) } } }` 与 `policy.rs:239` 字面重复；`OwnedHandle` 在 `com.rs` 已存在。
- **严重度：中**
- **建议**：4 份 `ProcGuard` 直接复用 `com::OwnedHandle`；2 份 `DeviceSetGuard` 合成一个模块级 `OwnedDeviceSet`。

### 1.3 `build.rs` 里 `MiniCatalog` 与 `GptCatalog` 字段完全重复
- **位置**：`crates/vox-core/build.rs:84-92`（MiniCatalog）与 `build.rs:109-117`（GptCatalog）
- **问题**：两个结构体字段完全一致（schema_version / verified_at / provider / model(MiniModel) / api(MiniApi) / capabilities），仅名字不同，各 deserialize 一份 Gemini、一份 GPT。
- **证据**：两者字段逐字相同，只是类型别名分别命名。
- **严重度：中**
- **建议**：合成一个 `MiniCatalog`，Gemini 和 GPT 共用；用同一个 deserialize 调用读两份 JSON。

### 1.4 `catalog_updater.rs::apply_update` 重拼了一遍已有函数能算出的 URL
- **位置**：`app/src-tauri/src/catalog_updater.rs:114-117`
- **问题**：`apply_update` 里已经拿了 `file = catalog_file(provider)`，却又手写 `format!("{RAW_BASE}{file}")` 重拼 URL，而上面 41-43 行明明有现成的 `remote_url()` 函数。
- **证据**：`:115 let url = catalog_file(provider).map(|file| format!("{RAW_BASE}{file}")).unwrap();` 与 `:41-43 fn remote_url(...) { catalog_file(provider).map(|file| format!("{RAW_BASE}{file}")) }` 是同一逻辑。
- **严重度：低**
- **建议**：`let url = remote_url(provider).ok_or(...)?;`，删掉手拼那行。

---

## 二、结构与耦合

### 2.1 `vox-audio-win` 的公共 API 面远大于实际所需
- **位置**：`crates/vox-audio-win/src/lib.rs:34-50`
- **问题**：这个 crate 的唯一**生产**消费者是 `app/src-tauri`（`examples/smoke.rs` 这个自检 example 也是公共面消费者，但它只用到 `os_build_number`/`process_loopback_available`，不碰下面点名的项）。lib.rs 重导出了大量内部实现细节：`scoring` 的四个选设备函数、`proc` 的 PID 选择策略（choose_target_pid / climb_to_root / name_matches / SessionHint）、`osver` 的纯判断版（process_loopback_supported / MIN_PROCESS_LOOPBACK_BUILD）。这些项要么在生产代码零引用，要么只是 crate **内部**实现细节却对外暴露。
- **证据**：`lib.rs:43-44` 重导出 `proc` 函数、`:48-50` 重导出 `scoring` 四函数、`:36-37` 重导出 `process_loopback_supported`/`MIN_PROCESS_LOOPBACK_BUILD`；grep 确认 app/ 下无引用，smoke.rs 也不用。
- **重要区分（避免误读）**：`proc` 的 `choose_target_pid`(capture/mod:303)、`climb_to_root`(proc:128)、`name_matches`(proc:159)、`SessionHint`(sessions.rs) **在 crate 内部都在用**，它们是**过度暴露**、不是死代码——要做的只是降可见性，不是删除。真正生产零调用、可整体退役的是 `scoring` 四函数（3.5）。`process_loopback_supported`/`MIN_PROCESS_LOOPBACK_BUILD` 同理是内部用、对外不必暴露。
- **严重度：中**（API 卫生问题，不是运行时缺陷）
- **建议**：把对外生产不需要的项降级 `pub(crate)` 并从 lib.rs 重导出移除；`scoring` 尤其可疑（注释自陈"分数从旧版 devices.py 搬来的资产"，是旧选设备方案的残留），除非计划重新启用，否则整模块可挪出公共面。保留 smoke.rs 实际用到的 `os_build_number`/`process_loopback_available` 的重导出。

### 2.2（观察，非缺陷）装配层缓存完整 `GateStatus` 是内核 Snapshot 粒度偏粗所致
- **位置**：`app/src-tauri/src/state.rs`（AppState 缓存 GateStatus）对 `crates/vox-core/src/runtime.rs` 的 `PipelineSnapshot`
- **观察**：内核 `PipelineSnapshot` 只给前端 `gate_rms` / `gate_open` 两个标量，但前端要完整 `GateStatus`，于是装配层在 state.rs 里额外缓存一份——内核 Snapshot 设计偏粗，逼装配层打补丁。内核注释有取舍说明，**这是可接受的轻微耦合，不列为需要处理的发现**，仅作记录。
- **建议**：若以后 Snapshot 扩展，把完整 GateStatus 直接放进 Snapshot，装配层就不必自行缓存。当前无需改动。

---

## 三、死代码

### 3.1 `subtitle.rs::finish_segment()` 是空函数却被调用
- **位置**：`crates/vox-core/src/subtitle.rs:135`（定义）被 `runtime.rs:937`（调用）使用
- **问题**：`pub fn finish_segment(&mut self) {}` 空函数体，注释说"不清屏，让每个字按 TTL 淡掉"——这是个显式占位的生命周期钩子，但既然什么都不做，这个调用点本身就是死代码异味。
- **证据**：`:135 pub fn finish_segment(&mut self) {}`；`:937 slot.finish_segment();`
- **严重度：低**（codex 审查指出：空生命周期钩子可辩护为显式意图，故从中下调为低）
- **建议**：删掉函数和 runtime.rs:937 的调用；如果将来要用，留个 `// TODO` 比留个空函数更诚实。优先级低，可暂留。

### 3.2 `catalog.rs::default_model(provider)` 与 `supports_audio_output_for(provider, lang)` 全仓零调用
- **位置**：`crates/vox-core/src/catalog.rs:37`（default_model）与 `:50`（supports_audio_output_for）
- **问题**：两个带 provider 参数的查询函数全仓没有任何调用点。而且 `supports_audio_output_for` 和单参版 `supports_audio_output()`（catalog.rs:115）并存，是"单参 + 带 provider 两套、其中一套没人用"的不一致。
- **证据**：grep 全仓 `default_model(` 只命中定义和测试无关项；`supports_audio_output_for` 仅出现在定义处。
- **严重度：中**
- **建议**：删掉这两个死函数，统一用单参版 + `provider_info(provider).capabilities.voice_selection` 组合判断。

### 3.3 `rates.rs::RateChoice.needs_resample` 字段从未被读取
- **位置**：`crates/vox-audio-win/src/rates.rs:16`
- **问题**：字段在 rates.rs 内部填充（50/65/77/109 行）并在测试里断言（129 行），但没有任何外部代码读它；`playback.rs:324-333` 只读 `.rate`。
- **证据**：grep `needs_resample` 全部命中在 rates.rs 内部（定义 + 赋值 + 一处测试），playback.rs 从不读它。
- **严重度：中**
- **建议**：删字段；playback 用 `LinearResampler` 自己判 `is_passthrough`，不需要这个信号。

### 3.4 `capture/mod.rs::with_endpoint_fallback()` 是死 API
- **位置**：`crates/vox-audio-win/src/capture/mod.rs:63`
- **问题**：宽松模式构造器全仓无调用点（连测试都不用，测试走 `WinCapture::new()` 严格模式）。内部 fallback 路径由 `Plan::Process { endpoint_fallback }` 触发，但没有任何地方传 `true`。
- **证据**：grep `with_endpoint_fallback` 仅命中定义和一句文档注释。
- **严重度：低-中**
- **建议**：删掉这个 pub fn（若不打算暴露宽松模式）。

### 3.5 `vox-audio-win` 的 `scoring` 四函数对外无调用
- **位置**：`scoring.rs:13/67/101/136`，重导出 `lib.rs:48-50`
- **问题**：`is_virtual_audio_device`（内部还被 pick_* 调）、`pick_microphone`、`pick_virtual_output`、`pick_listen_input` 在 app/ 下零引用，仅模块内测试调用。
- **证据**：grep 四个函数名，命中全部在 scoring.rs 自身（定义 + 测试）和 lib.rs 重导出。
- **严重度：中**
- **建议**：降级 `pub(crate)` 并移除重导出；或确认是否旧选设备方案残留、整模块退役。（与 2.1 同源）

### 3.6 `cloud/protocol.rs` 几个方法仅服务测试
- **位置**：`crates/vox-core/src/cloud/protocol.rs:86`（text_only）、`:103`（effective_voice）、`subtitle.rs:202`（text()）
- **问题**：这几个 pub 方法只在各自模块的 `#[cfg(test)]` 里被调用，生产代码不碰。`text_only` 生产用结构体字面量、`effective_voice` 生产没调用、`Track::text()` 也是只测试用。
- **证据**：grep `text_only` 生产调用仅在 protocol.rs 定义和测试；`effective_voice` 同样；`fn text(` 仅 subtitle.rs 定义处。
- **严重度：低**（pub 了但仅测试用，不是纯死代码，是"测试便利方法被过度暴露"）
- **建议**：移到 `#[cfg(test)]` 模块或改成 `pub(crate)`；属于可选清理。

---

## 四、过度工程

### 4.1 两套重采样器并存：`vox-audio-win/resample.rs`（线性）与 `vox-dsp/resample.rs`（rubato sinc）
- **位置**：`crates/vox-audio-win/src/resample.rs:1-95` 对 `crates/vox-dsp/src/resample.rs:1-145`
- **问题**：职责重叠——都是"任意采样率互转的流式重采样器"，但算法不同（线性插值 vs 128 点 sinc）。`vox-audio-win/resample.rs` 自己注释明说"这是临时件，等 vox-dsp 稳定后由 app 层接进来，这里就能删掉"。目前两边并存，是"临时方案"和"目标方案"同在。
- **证据**：`vox-audio-win/src/resample.rs:1-9` 模块头注释自陈是过渡件。
- **依赖前提（codex 审查补充）**：**vox-audio-win 当前不依赖 vox-dsp**。要让 `WinPlayback` 改用内核已有的 `Resample` trait（`vox-core/src/ports.rs:147-154`，装配层已有 adapter 见 `app/src-tauri/src/dsp.rs:22-33`），不能简单地"让 WinPlayback depend on core trait"——需要先给 `WinPlayback` 加一个重采样器工厂参数，或在装配层包一层注入。trait 和 adapter 路径都成立，但这是一次有依赖注入成本的改造，不是纯替换。
- **严重度：中**
- **建议**：先确认 vox-dsp 的 sinc 重采样器已稳定可用，再按上面注入方案让 `WinPlayback` 改用内核 `Resample` trait，最后删掉 vox-audio-win 这份线性重采样器。删之前这是 crate 里最大的并行实现。

### 4.2 `map_connect_error` 用字符串匹配判 DNS 错误（脆弱）
- **位置**：`crates/vox-net/src/ws.rs` 的 `map_connect_error()`（本仓库的行号引用会随改动漂移，按符号名找）
- **问题**：用 `format!("{err}").contains("dns"/"resolve"/"getaddrinfo")` 字符串匹配来区分 DNS 失败与一般 TCP 失败——依赖错误消息文案，locale 或库版本一变就可能漏判。
- **证据**：函数里 `if format!("{err}").contains("dns") || ...contains("resolve") || ...contains("getaddrinfo")`
- **严重度：低**（只是错误分类的文案友好度，不影响正确性）
- **建议**：分类问题的判断成立，但**修复路径需注意**（codex 审查指出）：`std::io::ErrorKind` **没有** DNS 这个 kind，Windows DNS 失败是以原始 OS 错误码（如 11001/12007）出现的，不是独立 ErrorKind。所以不能简单"改成匹配 ErrorKind"。可考虑匹配底层 raw OS error code，或接受现状。收益小，当前可不动。

### 4.3 `events.rs` 的 `Event::LatencyChanged { .. } => {}` 空 arm
- **位置**：`app/src-tauri/src/events.rs:125`
- **问题**：事件桥里 LatencyChanged 这个 arm 是空的——仅靠顶部统一的 `handle.emit(EVENT_CHANNEL, event)` 透传给前端（前端 store.tsx 处理）。空 arm 加注释是可接受的（透传已发生），但读起来像漏匹配。
- **证据**：`:125 Event::LatencyChanged { .. } => {}` 与其它 `=> {}` arm 并列，顶部 emit 已覆盖。
- **严重度：低**（不是 bug，是可读性）
- **建议**：可不动；或在注释里点明"靠顶部透传"，与其它空 arm 保持一致的说明风格。

---

## 体检结论

整体健康度**中上**。这套代码写得很扎实：注释质量在整个 workspace 里都属于顶尖水平——几乎每个 unsafe 块、每个硬编码常数、每个看似奇怪的判断都有"踩过坑"的注释解释为什么这么写。错误处理统一走 PortError + HRESULT 翻译，库代码无裸 unwrap/expect/panic。分层清晰：内核零平台依赖、外壳 trait 注入、装配层只编排不判断，没有跨层违规。vox-core 的延迟测量体系虽然庞大，但前端有完整 Latency.tsx 真实消费，是产品功能不是屎山；vox-overlay-win 的字符身份追踪（LCS）+ 行过渡动画（ease-out cubic）是字幕平滑滚动的真实需求。**没有发现严重的架构腐化、安全隐患或数据正确性缺陷。**

发现的问题集中在两类机械问题：**一是重复**（WASAPI 打开样板、句柄守卫、build.rs 类型），**二是死代码/过度暴露**（内核若干零调用函数、vox-audio-win 的 scoring/proc/osver 对外重导出过大、两套重采样器并存）。这些都不影响运行，但让代码比实际需要的更胖。

**最该先处理的 3 个问题：**

1. **收敛 `vox-audio-win` 的公共 API（2.1 + 3.5 + 3.3 + 3.4）**——把 scoring 四函数、proc 内部函数、osver 纯判断版从 `pub` 降到 `pub(crate)` 并移除 lib.rs 重导出（注意 proc 函数是**降可见性**不删，内部在用；scoring 才是可整体退役）；顺手删 `RateChoice.needs_resample` 死字段和 `with_endpoint_fallback` 死 API。这一组改动纯减法、零风险，能立刻让 crate 的公共面瘦身到与实际所需一致。保留 smoke.rs 用到的 `os_build_number`/`process_loopback_available` 重导出。

2. **清理内核零调用死代码（3.1 + 3.2）**——删 `subtitle.rs::finish_segment()` 空函数及其调用点、删 `catalog.rs::default_model()` 和 `supports_audio_output_for()`，统一用单参版。内核是零重依赖的核心，保持它的整洁收益最高。（3.1 现为低严重度，可暂留。）

3. **评估退役 `vox-audio-win/resample.rs`（4.1）**——这是注释自己承诺过要删的"临时件"。确认 vox-dsp 的 sinc 重采样器稳定后，按"先加工厂注入/装配层包装，再让 WinPlayback 用内核 Resample trait"的路径改造（vox-audio-win 当前不依赖 vox-dsp，不能简单替换），最后删掉这份线性重采样器。这一项收益最大但需注入改造成本，放最后。

---

## 附：codex 独立审查记录（v2 修订依据）

本报告经 codex CLI（`--dangerously-bypass-approvals-and-sandbox`）独立核对：它跑了完整 `cargo test`（vox-core、vox-audio-win、vox-overlay-win、装配层共约 170 个测试全绿），并逐条核对仓库源码。审查后采纳了以下修订：

- **2.1**：纠正"唯一消费者"措辞——`examples/smoke.rs` 也是公共面消费者（但它只用 `os_build_number`/`process_loopback_available`，不碰点名的项）。并明确 `proc` 内部函数是**过度暴露而非死代码**（capture/mod:303、proc:128/159、sessions.rs 在用）。
- **2.2**：从正式 finding 降级为"观察"，因内核注释已说明取舍、属可接受耦合。
- **3.1**：严重度由"中"降为"低"——空生命周期钩子可辩护为显式意图。
- **4.1**：补充依赖前提——vox-audio-win 当前不依赖 vox-dsp，退役线性重采样器需先建立工厂注入，不能简单替换。
- **4.2**：收回"改匹配 `io::ErrorKind`"的修复建议——`std::io::ErrorKind` 无 DNS kind，Windows DNS 失败是 raw OS error code，该修复路径不可行。

codex 未系统性补充漏报项（只做了证伪，未做补全）。
