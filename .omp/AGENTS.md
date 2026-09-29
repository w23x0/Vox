# Vox 项目上下文（给所有代理）

## 这是什么

桌面 + 将要扩展到手机/嵌入式的**实时语音翻译器**。Rust 芯（平台无关）+ 每平台一个外壳 + Tauri/React 界面。
两条流水线：**对外说话**（麦克风 → 译音进虚拟麦 + 字幕）、**听人说话**（抓某个程序的声音 → 中文语音 + 字幕）。
当前发货：Windows（NSIS）、Linux（deb/rpm/AppImage）。**macOS/iOS 已拍"不做"**。
**2026-09-26 起：直接做嵌入式（优先于手机），按 `docs/plans/S4-EMBEDDED-REFACTOR.md` 重构**（共享宿主层 + 算子链），方向见 `docs/architecture/DIRECTIONS.md` §10.9。

## 先读哪几份文档（按顺序）

1. `docs/architecture/DIRECTIONS.md` —— **方向总表 + 冲突裁决（新者胜）+ §10 当前执行计划**。开工前必读。
2. `docs/architecture/DECISIONS.md` —— 已拍板记录（A）/ 待拍板（B）/ 后端契约（C）。
3. `docs/architecture/ARCHITECTURE.md` —— 现状分层（轻内核 + 平台外壳）。
4. `docs/STRUCTURE.md` —— 文档与目录规划（文件该放哪、命名的规矩）。

**口径：代码与文档打架时以代码为准，然后回头把文档改对。**

## 仓库地图

```
crates/vox-core/        芯：设置、协议、状态机、账本(Runtime)、端口 trait(ports.rs)、字幕、用量
crates/vox-net/         WebSocket 传输（tokio + tungstenite）
crates/vox-dsp/         降噪(nnnoiseless/RNNoise)、重采样(rubato sinc)、切块、环形缓冲
crates/vox-osc/         VRChat OSC 发包（跨平台）
crates/vox-overlay-core/ 字幕渲染（画布/布局/合成），平台无关
crates/vox-{audio,input,overlay}-win/   平台外壳（WASAPI / GetAsyncKeyState / Win32 透明窗）
crates/vox-{audio,input,overlay}-linux/ 平台外壳（PipeWire / evdev / GTK+XWayland）
app/src-tauri/          装配层：命令、托盘、持久化、platform/ 分流、事件桥
app/ui/                 React 界面（Tauri 前端）+ 浏览器 Mock
catalog/*.json          模型服务商元数据（前后端共读，可运行时覆盖）
docs/                   文档（见 docs/STRUCTURE.md）
tools/                  一次性验证脚本（linux-verify 等）
```

## 开发命令（改动后必须跑对应的那条）

```bash
cargo test --workspace                 # 全量 Rust（含 vox-core 的账本/DSP 测试）
cargo test -p vox-core -p vox-dsp      # 只跑芯（快，日常用这个）
cargo clippy --workspace --all-targets # 提交前
cd app/ui && npm run verify            # 前端：tsc + vite build + 类名白名单 + a11y + qa:narrow
```

改动**只在一个 crate** 时跑该 crate 的测试即可；跨 crate 接口改动跑 `--workspace`。

## 硬约束（比风格重要）

- **芯里不许出现平台 API**：`vox-core` 不 `use windows`、不碰 Tauri、不碰 tokio（实测零命中）。
  平台能力一律走 `crates/vox-core/src/ports.rs` 的 trait，由外壳注入。
- **单一账本**：所有状态/设置只住在 `vox-core::Runtime`，别处不许存副本。事件只有一个通道
  `vox://event`（前端一个 listener 全收）。
- **热路径零新增分配**：音频回调、DSP 每帧、渲染每帧里不许新增 `Vec`/`clone`/格式化。
  已知热点见 `docs/research/BACKEND_HEALTH_CHECK.md` 与 `docs/architecture/DIRECTIONS.md` §10.3-3。
- **性能事实（实测，别猜）**：采集链 3.03 ms/s、播放链 1.73 ms/s，**≈0.3% 单核**；
  RNNoise 2.5 ms/s 是最大头，sinc 重采样次之。**基准工程已收进仓库**：`tools/bench-dsp/`
  （独立 crate，不随 workspace 跑；`cd tools/bench-dsp && cargo run --release`，建议 `taskset -c 2`）。
- **能力位模型**：provider 侧已有 `ProviderCapabilities` + `supports_*()`（`crates/vox-core/src/catalog.rs`）；
  宿主/设备侧要按同一套做（见 `docs/architecture/DIRECTIONS.md` §10.1-1）。界面按能力位降级，不许灰掉之后什么都不说。

## 工作方式（代理团队）

- 每轮：**architect 出设计稿（`docs/plans/**`）→ 对应 dev 实现 → verifier 独立复核 → Main 汇总**。
- **引用要耐久**：写"施工单"式的引用时，**优先给符号名**（`catalog.rs::ProviderCapabilities`、
  `Plan::build`），行号只作参考并注明"行号是写稿当时的"。行号漂移是本项目已发生过的返工源。
- **一个文件只有一个 owner**：同一文件同时只允许一个代理改；需要跨文件改动时找 Main 排期。
- 提交：**未经用户明确要求不 commit / 不 push**（见 `.omp/RULES.md`）。
- 语言：回复用户用**中文**；代码标识符与注释可用英文。
