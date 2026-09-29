# S0 设计稿：组合清单 × 统一能力位模型

> 状态：**设计稿·修订第 7 版**（2026-09-22）｜ 对应施工阶段 **S0 地基**（`docs/architecture/DIRECTIONS.md:509`）
> **第五轮只改 §2.5.4 末尾那段的措辞与修订记录**（见下"修订记录（2026-09-22，第五轮）"）：把"三条各有一个单测"改成
> 第九轮的实情（判据是**纯函数** + 钉子单测），补一条"**探针必须进程级**"的教训与真机 example 指路。**设计一字未改**。
> **第六轮只改状态注与引用耐久**（见下"修订记录（2026-09-22，第六轮 · 文档修订）"）：四处「`--print-composition` 仍未落地」按实况改成**状态注**
> （**第十五轮已两档落地**：无屏档 `vox-headless --print-composition` 真跑 exit=0、stdout 合法 JSON；桌面档 `app/src-tauri/src/composition.rs`），
> 并把 §1.1 / §1.5 / §2.2 / §2.3.1 / §3.1 / §4.2 里一批过期行号改成**符号优先**（`catalog.rs::supports` / `catalog.rs::supports_audio_output` / `pipeline/speak.rs::composition` / `Plan::build`）。**设计一字未改，命令体一字未改。**
> **第七轮只补一处状态注**（见下"修订记录（2026-09-22，第七轮 · 文档修订）"）：§3.2 表后注的末句（本轮改前在第 1183 行）从裸的「`--print-composition` 仍未落地」补成**第六轮时点仍未落地 + 第十五轮已两档落地**。**设计一字未改，命令体一字未改。**
> **实现进度（2026-09-22，第三轮时点）**：**芯侧已落地** —— `crates/vox-core/src/{composition.rs,capability.rs}`、
> `Plan::build` 经清单中转（行为不变）、`Runtime::set_host_facts` + `Snapshot.capabilities`；
> **外壳侧也已落地** —— `platform::host_kind()` / `host_facts()`、`assemble()` 里注入、4 秒轮询复核、
> **Linux 虚拟麦已接线**（本机实测 `virtual_mic: enabled=true`）。测试：`cargo test --workspace` = 436 passed / 0 failed（数字由 Main 提供）。
> **第六轮状态回填（2026-09-22）**：**UI 能力位已落地** —— `app/ui/src/capabilities.ts`（`hostBit` / `capabilityNote`，位在界面侧的唯一读数点）+ `components/Capability.tsx`，各 section（`CableManager` / `Subtitle` / `Settings` / `Vrchat` / `PipelineCard`）按位降级，`npm run verify` 已含 `check:capabilities`（`app/ui/package.json`）；`Snapshot` 的 `capabilities` 字段在 `app/src-tauri/src/dto.rs` 落地。控制面（端点投影 / 账本）见 `docs/plans/S1-AGENT-FACE.md`，本稿不重复。**第六轮那个时点仍未落地**：`--print-composition`（§4.3-A / §4.4 的命令当时跑不了）——**第十五轮已两档落地**：无屏档 `vox-headless --print-composition`（真跑 exit=0、stdout 合法 JSON）、桌面档 `app/src-tauri/src/composition.rs`（排在 Tauri 之前、只 `build()`）；**命令体一个字没改**，只在 §4.3-A 与本稿三处历史记录旁加了状态注。第六轮收口时 Main 报 `cargo test --workspace` = 456 passed / 0 failed；**第四轮接线落地后 shell-dev（`CapDefiners`）报 459 passed / 0 failed / 5 ignored、`cargo clippy --workspace --all-targets` 0 warning**（都是**时点数字**，本稿不据此下结论、也不用它当验收）。
> 本稿**不含实现代码**；除 §3.2 末尾那一条已拍的 Linux 虚拟麦接线外，`crates/` 与 `app/` 的现行行为一个字不改。
> 口径：代码与文档打架以代码为准（`docs/architecture/DIRECTIONS.md:1-6`）；文档之间冲突**新者胜**（§8）。
> 前置已读：`docs/architecture/DIRECTIONS.md`（§8 裁决 / §10 计划）、`.omp/AGENTS.md`、`docs/STRUCTURE.md`。
> 调研输入：`agent://AndroidPath`、`agent://EmbeddedFirst`、`agent://RuntimeCompose`、`agent://HostShells`、`agent://AgentFace`、`docs/platform/LINUX.md`。

---

## 修订记录（2026-09-22，第二轮）

> 触发：`docs/architecture/DIRECTIONS.md` §10.5（三条原则）+ §10.6（三问答复）+ §10.5 末尾 **Main 拍掉的四项**；派工见 §10.7。
> 本轮**只动本文件**；五节结构（现状证据 / 目标形状 / 改动清单 / 验收标准 / 风险与未决）不变。

| # | 对应拍板 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | §10.5-1：**功能不删，只按能力位开门/关门** | §0.1（新增说明）、§2.3（重写 + 新增 §2.3.2 / §2.3.3）、§2.5.1（`program_tap` / `virtual_mic` 措辞）、§4.4（新增断言）、§3.4 | 两条流水线**都留在架构里**：四档宿主各有一份 Speak / Listen 的清单实例（§2.3.1 / §2.3.2 / §2.3.3）。位为假只是**关门**（条目不进清单 + 位报 `false` + 界面说清"这台设备做不到"）。删掉读起来像"这条腿不做了"的措辞（原文 `program_tap` 无屏列写的"无此需求"） |
| 2 | §10.5-2：**能力位必须是事实** | 新增 §2.5.0；§2.5.1 位表重写；§2.5.2 / §2.5.3 / §2.5.4 / §2.6 收口 | 补上从**宿主报表（`HostFacts`）**到**界面降级**的完整推导链（§2.5.0：六步、每步标 owner、每步给可观察证据）；位表从"✅/⚠️/❌"改成**逐平台真实值**（Windows / Linux 桌面 / Android / 无屏 ARM64），拿不到实据的格子标 `[未核实]`；新增 `UnavailableReason::NotWired` |
| 3 | Main 拍的第 ① 项：Linux 虚拟麦**接线** | §3.2 结尾那段（原"(a)/(b) 二选一"）、§3.2 主表新增一行 + 接线两段、§4.4 重写、§5.1 | 从"二选一且必须选"改成**已拍：接线**（owner shell-dev，逐文件、含验收怎么跑）；写清**中间态** = 位报 `off(not_wired)` + **撤下**"去目标程序里选虚拟麦"的引导文案（现状那种引导指向一个不存在的设备） |
| 4 | §10.5-3：**桌面不是被冻结的旧版，是一档宿主** | §2.3（标题 + `host` 值）、§2.4、§2.2 `host` 行、§0.2 新增两行 | `host` 从常量 `native` 改成**宿主档位**（`windows` / `linux_desktop` / `android` / `linux_headless`）：能力位表按档位索引，Windows/Linux 与 Android/无屏在 §2 里**并列**表述。**正文里不再有"旧版 / 冻结"这类描述**（`grep -n "旧版\|冻结"` 只命中 §0.2 第 4 行与本表的**否定式**："不是'旧版'"）。顺带把 `missing_on` 的返回类型从 `Vec<Capability>` 改成 `Vec<CompositionError>`，好把"拿错档位的清单"（`HostMismatch`）一起报出来——S1 的"非空即 `endpoint_unavailable`"逻辑不变 |
| 5 | Main 拍的第 ②③④ 项 | §0.2 第 2 行、§2.5.1 provider 表、§2.5.2、§3.1（`build.rs` / `catalog/*.json` / `composition.rs` 三行）、§3.3、§5.1、§5.2 | ① `session` 拍为**第 8 格**（原写"需要 Main 或用户点头"→ 改"已拍"）；② provider 4 个待补位**只进代码枚举、不进 `catalog/*.json`**（原文要求三份 JSON 各加 4 个字段 → 改成 `build.rs` 与 `catalog/*.json` **都不动**）；③ `schemars` **锁 0.8.22**（可选依赖 + feature `json-schema`，与 `docs/plans/S1-AGENT-FACE.md` §2.6 同一套做法） |
| 6 | 顺带对齐 §10.5「功能处置表」（不在五点之列，写在这里免得后面返工） | §3.2 的 i18n 行、§5.3 第 3 行、§2.5.1 的 `usage_reporting` 行、§5.1 | 三语 UI 收敛为 **zh + en**（`ja` 不再扩）；`usage_reporting` 的界面消费者（用量页）已按该表砍掉 → 这位在 S0 **只占名、暂时没有消费点** |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §0.2 第 1、3 行（`out` 拆 `kind` + `role`；`chunk` 落 `in[].block_ms`） | 未动 | 与本轮拍板无关，仍以代码为准 |
| §1 现状证据全部 | **只核未改** | 本轮重核过的少数几处已写进对应小节（§1.4 / §2.3.2 / §3.2 / §4.4）；§1 的表仍是上一版的行号（"写稿当时"），没有重排 |
| §2.1 里 `Input` / `Op` / `Output` / `PlaybackRole` / `EdgeSource` / `Life` / `Ui` / `Control` / `SessionSpec` | 未动 | 字段一字未改 → **S1 稿"逐字照抄"的那份仍然对**（S1 稿也已按四档 `host` 与 `tier` 自行对齐，见 §5.4） |
| §2.2 字段表除 `host` 行外 | 未动 | `in[]` / `ops[]` / `out[]` / `life` / `ui` / `control[]` / `session` 行的结论不变 |
| §2.5.3 的"条目 → 需要的位"映射表 | 未动 | 本轮只补了"谁来用这张表"（§2.5.3 新增的两种装配上下文） |
| §2.6 的 R1–R8 | 未动（新增 R9 一条） | 原有 8 条规则文本不变；R9 只是把 §10.5-1 的文案纪律落下来 |
| §2.7 的"进清单 / 不进清单"表 | 未动 | 那张表是 S1 反方向翻译的权威（S1 §2.1.4 引用它），本轮只改流程图一行 |
| §3.1 除 `build.rs` / `catalog/*.json` / `composition.rs` 三行外的行 | 未动 | 清单中转、`Plan` 不动、`ports.rs` 删 trait 方法等结论不变 |
| §4.1 / §4.2 / §4.3 | 未动（§4.1、§4.3 各加一条断言） | 命令与测试清单 1–10 仍是新类型落地时的正交测试；§4.4 改的是**接线**那条真机检查 |
| §5.1 的第 3、6、7、8 条 | 未动（编号未变） | 与本轮拍板无关；第 4 条只补了一句"已逐格标 `[未核实]`"，结论不变 |

---

## 修订记录（2026-09-22，第三轮）

> 触发：独立复核 `agent://AuditS0Impl` 的缺陷清单（**D1 / D2 / D4 / D10**）+ Main 本轮的裁决（**D1 签名统一、D2 两个来源的口径**，2026-09-22）。
> 本轮**只动本文件**，且**只做文档回填**：不改实现的任何一行，也不改本稿的字段与取值定义（`crates/`、`app/` 一个字不动）。
> 本轮五节结构不变。

| # | 对应缺陷 / 裁决 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | **D1 签名统一（Main 裁决）**：`Composition::of(&SessionConfig, &HostFacts)` 是**唯一**签名（实现已如此：`crates/vox-core/src/composition.rs::of`） | §2.1（`impl Composition` 新增 `of` 的声明）、§3.1（`composition.rs` 行）、§4.2-1 / §4.2-12（测试的调用点） | 把 `of` 的第二个实参全稿统一成 **`&HostFacts`**（旧稿把它误写成"一个报告类型的引用"；改完 `grep -n "&CapabilityReport"` 只剩 `missing_on` 那两行，**两行都逐字标了"另一个函数"**）。**`missing_on(&CapabilityReport)` 一个字不改**——它是**另一个函数**（问"拿一份算好的位，这条清单装得上吗"），已在 §2.1 与 §3.1 两处**逐字标注"另一个函数"**。**S1 侧的上游结论**（一文件一 owner，由 S1DocFix 落地）：投影从 `Runtime::host_facts()`（`crates/vox-core/src/runtime.rs::host_facts`）取**同一份**事实，`vox-mcp` 依赖 `vox-core` 是允许的，**不另立第二份事实** |
| 2 | **D2 两个来源（口径，2026-09-22）**："能不能用"的唯一真相是能力位 `virtual_mic`；`DeviceRegistry::virtual_cable_installed()` **保留**，降级为"仅 Windows 安装管理用" | §3.1 的 `ports.rs` 行、§2.5.4 的 `virtual_mic`(Windows) 行、§3.2 的 `dto.rs` / `devices.rs` 两行 | **推翻**原稿的"把 `DeviceRegistry::virtual_cable_installed()` 从 trait 上删掉——平台事实不该挂在设备枚举上"。理由：VB-CABLE 的下载/安装 UI 还要它，删了安装态就没有来源。**不删**，改成"这个字段只描述**安装器状态**"；任何"能不能把译音灌进虚拟麦"的判断**只许读能力位 `virtual_mic`**（trait 文档注释由 core-dev 在 `ports.rs` 补同一句话） |
| 3 | **D10 文档漂移**（复核逐条）：`Composition` 的 derive 里那个 `Eq` 是笔误；§1 的行号普遍漂 1–7 行 | §2.1（`Composition` 的 derive）、§0.2 第 3 条 / §1.3 / §1.4 / §2.1 / §2.3.1 / §2.5.1 / §2.5.4 / §3.1 / §3.2 / §5.1-8 / §5.1-12 | ① `Composition` 的 `#[derive(.., Eq)]` **去掉 `Eq`**：`ops` 里的 `Op::Gate` 带 `f32` 阈值，`Eq` 落不下来（实现里就是这么写的，注释写着"设计稿里那行 derive 是笔误"）。② 复核点到名的**四类**行号按**符号优先**重核：`INPUT_BLOCK_MS`（`:42`→`:47`）、`SessionParams`（`:72-89`→`:79`）、`SpeakSettings::output_device` 缺省（`:135`→`:137`）、`win.rs` 的 VB-CABLE 探测（`:93-109`→`virtual_device_status()` 现 `:167` 起；`tray_host_available()` 现 `:194`）；顺带把同一批里的 `DEFAULT_FONT_FAMILY`（`:255-258`→`:257-260`，§1.6 与 §3.1 各一处）也重核了。**行号一律只作参考**——`.omp/AGENTS.md` 要求"施工单式引用优先给符号名"，本轮把这几处改成符号在前、行号退成括注 |
| 4 | **第三轮时点的实现状态回填**（不是裁决，是"文档别再说反话"）：外壳侧已落地，**Linux 虚拟麦已接线**（本机实测 `virtual_mic: enabled=true`） | 顶部状态行、§1.4（补记）、§2.5.1 的 `virtual_mic` 行、§2.5.4 的 `virtual_mic`(Linux) 行、§2.5.3（补 `Runtime::host_facts()`）、§3.2 中间态那段（补记）、§4.4（补记）、§5.4 的 S1 行 | 第二轮稿写的是"外壳侧进行中""今天 `false(not_wired)`"——第三轮已过时，留着会让"位表 = 今天会报的有效值"这句自相矛盾。**只有回填状态，不改设计**：接线前的中间态（`off(not_wired)`）与两态验收（§4.4 态 A / 态 B）作为**口径与回归依据**原样保留 |
| 5 | **D4 `in: []` 进 `validate()`**（core-dev 已落地，本轮**只回填文档**） | §2.1 的 `CompositionError` 枚举、§4.2 第 9 条 | 枚举新增变体 **`MissingInput`**（无字段）：`in: []` 由**第一道闸 `validate()`** 挡下，不再拖到 `Plan::from` 才炸——外部提交的清单因此拿到一个准确的错误码（否则 S1 会把它说成 `unsupported_field` 之类）。§4.2 第 9 条补上对应测试名 `a_manifest_without_an_input_is_rejected` |
| 6 | **S1 侧的两条同步**（跨 owner，S0 只做留档与引用方式） | §2.1（`CompositionError` 的注释）、§3.3 的 agent-face-dev 行、§5.1-10、§5.4 的 S1 行 | ① **`CompositionError` 缺 `Serialize`**：S1 要把 `data.errors` 送上线路（`{"kind":"host_mismatch", …}`，S1 §2.1.2 已定形），而实现里只有 `Debug/Clone/PartialEq/Eq` → S0 在 §2.1 记成 **core-dev 待补**（带判别键的 serde，**线上名字以 S1 为准**，S0 不另定义）。② **S1 稿的行号第三轮已漂**（S1 前 250 行被改过）→ S0 引 S1 的地方一律改成**按节引**（§1.4 / §2.1.2 / §2.1.3 / §2.1.3-③ / §2.1.4 / §2.6） |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §2.1 里 `HostKind` / `Input` / `Op` / `Output` / `PlaybackRole` / `EdgeSource` / `Life` / `Ui` / `Control` / `SessionSpec` 的 derive 与字段；`CompositionError` 的 derive 与**其余变体** | 未动（`CompositionError` 只**加了一个变体** `MissingInput` 与一条 `Serialize` 待补说明，见第 5、6 行） | D10 点名的只有 `Composition` 那一行。其余逐条与实现对齐（`Op` 与 `SessionSpec` 本来就**没有** `Eq`，因为都带 `f32`），字段一字未改 |
| §2.5.3 的"条目 → 需要的位"映射表、两种装配上下文表 | 未动（Runtime 查询块只补了一行 `host_facts()`，见第 4 行） | 那张表本来就写的 `Composition::of(&config, &facts)`，与 D1 后的唯一签名一致 |
| §2.7 的流程图与"进清单 / 不进清单"表 | 未动 | 同上：`Composition::of(&config, &facts)` 本来就对，不必改 |
| §2.5.0 六步链、§2.5.2 数据位置 | 未动 | 与本轮三条无关；§2.5.0 第 2 步里"Linux 没接线 → `off(not_wired)`"作为**举例**保留（接线后这一项自然不再出现） |
| §2.5.4 定义者表的**正文结论** | 只改了两行 | `virtual_mic`(Windows) 行加了一句附注（安装器状态 ≠ 能不能用，D2）、`virtual_mic`(Linux) 行按接线落地回填状态；其余各行与定义者规则本身未动 |
| §3.2 末尾"Linux 虚拟麦：**已拍 = 接线**"整节（含 (i)/(ii) 两个候选） | 未动（只加了一条时点补记） | 与本轮三条无关。**注意**：接线已落地（本机实测 `virtual_mic: enabled=true`），但 **(i)/(ii) 的实测结论仍未回填**——见 §5.1-9 |
| §4.2 的测试清单 2–11、§4.3、§5.1 除第 8/12 条外、§5.2、§5.3 | 未动 | 与本轮拍板无关；§4.2 只有第 1、12 条的调用点按 D1 改了实参 |
| §4.4 的两态检查体 | 未动（只加了一条时点补记） | 态 A / 态 B 的命令一个字未改；补记只说"当前该验态 B"，两道态仍是"位 = 事实"的验收形状 |

---

## 修订记录（2026-09-22，第四轮）

> 触发：第五轮独立复核 `agent://AuditRound5` 的三条星号（**★1 = F1** 定义者表与实现冲突 / **★2 = F2** 缺省事实窗口的失败模式 / **★3 = 行号自伤漂移），以及同一份复核的 **F5 / F6** 时点项。
> 本轮**只动本文件**，且仍**只做文档**：`crates/` 与 `app/` 一个字没动。第六轮的 UI 能力位与本稿的**状态回填**只改"读起来像现状"的那几处（顶部状态行 + 本节），不改任何设计。
> 本轮写实的三个定义者（owner shell-dev）**在同一轮内已接线**：`platform::{record_captions, record_background_service, captions_status, background_service_status, vr_captions_status, OverlayFailure}` / `vr_overlay::{status, hmd_ready}` + `RUNNING`/`CONNECTED`（核法见 §2.5.4 末的四条 `grep`）。本稿只写形状与判据，不写实现。
> 五节结构不变。

| # | 对应星号 / 缺陷 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | **★1 / F1（重要）**：`mic` / `captions` / `background_service` / `vr_captions` 四位在**产品路径上恒报开、没有定义者**，与 §2.5.4 第一句话冲突 | §2.5.4（四行重写 + 表后新增"第四轮（F1）补的口径"三条）、§2.5.1（这四位的"依据"格 + 读表须知新增第 4 条）、§5.1-6、§5.1-13（新增）、§5.3 第 2 行 | 逐位收口，两类处理**分开写**：① **`captions` / `background_service` / `vr_captions` 接上定义者**——报 `true` 的凭据写死成一段真的打开了它的代码（`record_captions` × `platform::overlay_running()` / `events::sync_autostart` 的返回值 / `vr_overlay::status()`），并给出各自的 `off(reason)` 分档（含 `busy` 与平台差异）；② **`mic` 写明是"有意不接定义者、按档位上限报开"**，代价（界面不提前降级、被占要等用户按"对外说话"才由 `PortError` → `Notice` 兜底）与"新增位不许照抄这一条"一起写进去。核法（**第四轮复核时点**）：`grep -rn "Capability::" app/src-tauri/src` 全 app 只 5 处写 `off`（`program_tap` / `virtual_mic` / `vr_captions` 的 `cfg` 那一半 / `global_hotkey` / `tray`）——F1 说"这 4 位没有定义者"因此**在文档里被承认并分头处理**，不再是"宣称有、代码里没有"。三位接线的核法见 §2.5.4 末 |
| 2 | **★3 / 行号自伤**：§3.1 的 `ports.rs` 行把 `virtual_cable_installed` 记在 `:170`，而**D2 自己要求补的那段注释**把它推到了 `:178` | §3.1 的 `ports.rs` 行 | 按本稿自己定的**符号优先**规矩办：先给符号 `crates/vox-core/src/ports.rs::DeviceRegistry::virtual_cable_installed`，行号退成括注并回填 `:178`，并把"这 8 行是 D2 自己的注释推的"记成**行号自伤记录**（`git diff crates/vox-core/src/ports.rs` 可复现）；同批的 `DeviceRegistry` trait 头仍在 `:164`（未动） |
| 3 | **★2 / F2（措辞）**：报告称"缺省（未注入）事实的覆盖窗口里，Speak 腿会被 `Plan::build` 以 `PortError` 拒掉（`[MissingInput]`）" | §2.5.0（两条不变量之后新增"一条容易读错的推论"）、§5.1-6 | 先说清楚：**本稿原本没有这句话**（复核核的是第五轮的**报告**，不是本稿的字面），但 §2.5.0 的六步链与 §2.5.3 的 ① 很容易被读成那个意思 → **补一段把两条腿的失败模式分开写死**：Speak 腿在缺省事实下**降级成 `Speaker`**（`speak.rs::composition` 的 `role`）、`Plan::from` 照样成功、译音**静默播到系统默认输出**；只有 **Listen 腿**（`program_tap` 关着 → `in` 为空）才是 `MissingInput`。装配第 10–13 步那条热键窄窗口里起来的也是"播到默认输出"的会话，不是"起不来" |
| 4 | 顺带（不在三条之列）：§2.5.0 第 2 步给的例子 `麦克风被占 → off[mic] = busy` **今天没人写**（`mic` 本来就是按上限报开的那一位） | §2.5.0 第 2 步 | 例子换成今天真的会写的那几位（`captions` / `global_hotkey` / `virtual_mic`），并注明 `mic` **不在这里写**、指向 §2.5.4 的偏离条——免得下游照着一个不存在的写入点去核 |
| 5 | 第六轮**状态回填**（不是裁决，是"文档别再说反话"） | 顶部状态行、§2.6 的 R4 / R5 两格、§3.2（表后新增"第六轮状态"注）、§4.3-B（新增一节状态注）、本节的"没改的节"表 | UI 能力位（`app/ui/src/capabilities.ts` + `components/Capability.tsx` + `check:capabilities` 进 `verify` 链）与 `Snapshot.capabilities`（`dto.rs`）已落地；§2.6 的 R4 / R5 两格从"新增 / 现状缺口"改成"已落地 / 已修"；§4.3-B 补上脚本已落地与它实际用的 URL 开关（`?host=` / `?off=<位>:<reason>` / `?virtual_mic=<reason>` 简写）；`--print-composition` **第六轮时点仍未落地**（**第十五轮已两档落地**：无屏档 `vox-headless --print-composition` 真跑 exit=0、stdout 合法 JSON；桌面档 `app/src-tauri/src/composition.rs`，排在 Tauri 之前、只 `build()`——本行保留第六轮当时的记录）；§3.2 里 `VirtualDeviceStatus` 的删除与 `virtual_cable_status` → `virtual_mic_detail` 的改名**实现里没做**（两行留在表里，状态注写明"别照本表去找"）；测试数从"第三轮 436"补到"第六轮收口 456（时点数字）"。**只回填状态，不改设计**。控制面（端点投影 / 账本）属 S1 稿，本稿只指路 |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §2.5.1 其余 7 个宿主位与全部 provider 位 | 未动 | F1 点名的只有 4 位；`program_tap` / `virtual_mic` / `global_hotkey` / `tray` 的定义者第三轮已经写实 |
| §2.5.0 六步链的**步骤与 owner**、§2.5.2 数据位置、§2.5.3 查询 API | 未动（第 2 步的**举例**见第 4 行；§2.5.3 的 ① 只在 §2.5.0 新增那段里被点名） | 与本轮三条无关；六步链仍成立——新增的三位定义者正好用上第 3 步"事实变了再注入"（`devices.rs` 的 4 秒复核）。**留个提醒**：§2.5.3 的 ① 那格写"位为假的条目**不进清单**"，最容易读成"这条腿装不上"——真实的失败模式见 §2.5.0 新增的"一条容易读错的推论"（Speak 降级成 `Speaker`、只有 Listen 腿才是 `MissingInput`）；那格**字面没改** |
| §2.3.x 的清单实例、§2.7 的"进清单 / 不进清单"表、§4.1 / §4.2 / §4.4 | 未动 | 本轮只改**位与定义者的口径**，不动清单形状与验收命令。§2.6 的 R4/R5 与 §4.3-B 只加**状态注**（第六轮已落地），规则与断言清单一个字没改；§4.3-A / §4.4 的命令体也没改（`--print-composition` 第六轮时点仍未落地、那些命令当时跑不了；**第十五轮已两档落地**，见顶部状态行与 §4.3-A 的状态注） |
| §3.1 / §3.3 的其余行、§3.2 的**表行本身** | 未动（`ports.rs` 行见第 2 行；§3.2 只在表后加了一条"第六轮状态"注，见第 5 行） | 一个文件一个 owner；本轮不新派工——三位定义者的落点全在**已有文件**里（`overlay.rs` / `events.rs` / `vr_overlay.rs` / `platform/{mod,win}.rs` / `platform/linux/mod.rs`），owner 仍是一个（shell-dev），§3.2 的表行不必改 |
| §5.1 其余 12 条、§5.2、§5.3 其余行、§5.4 | 未动（§5.3 第 2 行加了一句 `mic` 例外，§5.1-6 改了状态） | 与本轮三条无关；§5.4 的 `DIRECTIONS.md` / `S1-AGENT-FACE.md` 两行**仍然有效**（本轮不碰别人的文件） |

---

## 修订记录（2026-09-22，第五轮）

> 触发：**第九轮（悬浮窗阻断项）的独立复核**把 §2.5.4 末尾那句"这三条各有一个单测钉住"读成了与实情不符的措辞——
> 判据是**第九轮**才从函数体里抽成纯函数的，钉子也是那时补的（第九轮阻断项 **D1 / D2 / D3**）。
> 本轮**只动本文件**、**只动 §2.5.4 末尾那段 + 本记录 + 顶部状态行**：`crates/` 与 `app/` 一个字没动，
> 设计（字段、位表、定义者规则、验收命令）**一个字没改**。owner 仍是一个（shell-dev）。
> 五节结构不变。

| # | 对应 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | **第九轮阻断项 D3**（代码注释里标 **第七轮 D3**：`overlay.rs:224` / `events.rs:417` / `platform/mod.rs:302`）：判定散在 `overlay::start` / `events::sync_autostart` 的函数体里 → 把定义者改成恒 `ON` 单测全绿 | §2.5.4 末尾（原"这三条各有一个单测钉住"整段） | 措辞按第九轮实情重写：判据已抽成**纯函数** `overlay::captions_outcome` / `events::autostart_status` / `platform::captions_status_for` / `vr_captions_status_for`，并给出**四个判据 × 钉子单测**的表（用例名 + 落点，`grep` 于 2026-09-22 核），外加同模块的四条全链用例、Windows 专有一条、`#[ignore]` 真机快照一条。原文把"接线"与"钉子"混成一句，读者会以为钉子是第四轮接线时就有了 |
| 2 | **第九轮阻断项 D1 / D2**：`vox-overlay-linux` 的存活凭据是建窗线程的 `thread_local` → 帧线程秒退（字幕渲染次数恒 0）、轮询线程读到另一个答案（位翻 `off(busy)`） | §2.5.4 末尾（新增两段） | 补"**存活探针必须是进程级事实**"的教训：凭据改成进程级 `static ALIVE: AtomicBool`（`window.rs:51`），由 `vox_overlay_linux::window::tests::running_is_a_process_wide_fact` 从源头钉住；**并指向真机 example** `crates/vox-overlay-linux/examples/frame_loop_probe.rs`（`cargo run -p vox-overlay-linux --example frame_loop_probe -- 5`；5 秒 152 次渲染、截图像素红 4760 / 绿 6144、`hide()` 后 0 / 0）。另外**记下一条反面**：`platform::tests::captions_bit_is_the_same_answer_on_the_poll_thread` 抓不到这次回归（测试进程没有真窗），别把它当线程无关性的钉子 |
| 3 | 顶部状态行 | 顶部 | "修订第 4 版"→"**修订第 5 版**"，并加两行说明本轮只改 §2.5.4 末尾与修订记录。不改任何进度/测试数（那些仍是时点数字） |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §2.5.4 定义者表的**正文行**、§2.5.4 第四轮（F1）三条、本节末四条 `grep` 核法 | 未动 | 本轮只重写表**后**那段收口；`mic` / `captions` / `background_service` / `vr_captions` 各行的结论与第九轮一致，没有一个字需要改 |
| §2.5.0 六步链、§2.5.1 位表与读表须知、§2.5.2、§2.5.3 | 未动 | 第九轮改的是**判据的落点与钉子**，不是位值、不是上限、不是推导链 |
| §2.6 的 R1–R9 | 未动 | 界面降级规则与"探针必须进程级"无关；R6 的正文（"位为真必须由打开这条路的那段代码负责"）本轮反而多了一条实现侧前置，但那句话**字面没改** |
| §4.1 / §4.2 的测试清单 1–12、§4.3、§4.4 | 未动 | 第九轮的钉子用例已在 §2.5.4 列出，**不**另开验收条目（它们钉的是"定义者说真话"，不是清单形状）；§4.3-A / §4.4 的命令体仍是"`--print-composition` 落地后才跑得了"，一个字没改（**第十五轮已两档落地**，命令体至今仍一个字没改；见顶部状态行与 §4.3-A 的状态注） |
| §3.1 / §3.2 / §3.3 全部改动清单行 | 未动 | 第九轮的落点全在**已有文件**里（`overlay.rs` / `events.rs` / `platform/{mod,win}.rs` / `crates/vox-overlay-linux/src/window.rs`），owner 仍是一个（shell-dev），不新派工、不动表行 |
| §5.1 / §5.2 / §5.3 / §5.4 | 未动 | 第九轮的教训不是新的未决项（它已被实测钉住），不进 §5.1；§5.1-11 / §5.1-12 那两条"真机没验过"的说法仍成立（探针的真机凭据是 Linux 悬浮窗，不是 `vr_captions` 的真头显 / 组策略拒写） |
| **第八轮复核的两条星号**（★1 `crates/vox-mcp/src/ledger.rs::Grants::config_write_allowed` 的总闸没被用例钉住、★2 `app/ui/scripts/preview.mjs` 的就绪判定 TOCTOU / `qa-home.mjs` 固定端口 / `control.json` 不擦） | **本稿不写** | 两条都在 **S1** 射程内（`crates/vox-mcp/**` 与 `app/ui/scripts/**`），不是 S0 的组合清单或能力位模型。本稿不替它们做记录，也不在 §2.5.4 里引它们——免得下游以为 S0 稿管这两条 |

---

## 修订记录（2026-09-22，第六轮 · 文档修订）

> 触发：**第十六轮复核**的文档缺陷清单里属于本稿的部分（两条"重要"里的第 1 条 = `--print-composition` 的状态失实；一条"次要" = 一批行号引用过期）。
> 本轮是**文档修订第六轮**（项目第十八轮），**只动本文件**：`crates/` 与 `app/` 一个字没动，清单字段、位表、断言与**命令体**都没改。
> 下面逐条对到派工里的两条（第 1 行 = 四处状态注；第 2 行 = 引用耐久）；第 3、4 行是顺带对齐（都只改注释/标题，**不动命令体**）。

| # | 对应 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | 第十六轮复核**【重要】**：四处仍写「`--print-composition` 仍未落地」，而**两档均已落地** | 顶部状态行（第六轮状态回填那段）、第四轮修订记录第 5 行、第四轮"没改的节"表、第五轮"没改的节"表 | **不改命令体，只加状态注**：四处**保留当时的字面**（改成"第六轮那个时点仍未落地 / 当时跑不了"，history 不改口），紧跟一句实况——**第十五轮已两档落地**：无屏档 `crates/vox-headless/src/cli.rs` 的 `--print-composition`（真跑 exit=0、stdout 合法 JSON）、桌面档 `app/src-tauri/src/composition.rs`（排在 Tauri 之前、只 `Builder::build()` 不 `run()`）。出处：`docs/architecture/DIRECTIONS.md` §10.7「第十五轮回填」 |
| 2 | 第十六轮复核**【次要】**：一批行号引用过期（`catalog.rs` 的 4 个 `supports_*` 已删、`supports_audio_output` 现在 `:111`、`speak.rs:15-48` 覆盖不到 `composition`） | §0.2 第 1/2 行、§1.1、§1.5、§2.2、§2.3.1 的"现状代码"列、§2.5.1 的 provider 行、§2.7、§3.1 的六行、§4.2 第一层与第 6 条 | 按 `.omp/AGENTS.md` 的"**引用要耐久**"办：**符号在前、行号退成"写稿当时"的括注**——`catalog.rs::ProviderCapabilities` / `::ProviderInfo` / `::supports_audio_output`、`catalog.rs` 的 4 个 `supports_*` 自由函数（**已删**：现为 `catalog.rs::provider_capabilities` + `catalog.rs::supports(provider, bit)`）、`pipeline/speak.rs::composition`（写稿当时 `fn plan`，`:15-48`；现 `:26-130`）、`pipeline/listen.rs::composition`（写稿当时 `fn plan`，`:16-44`；现 `:30-109`）、`pipeline/mod.rs::Plan::build`（写稿当时 `:106-111`；现 `:114`）、`speak.rs` / `listen.rs` 的 `mod tests` 不再按行号指。**结论、字段与断言一个都没改。** |
| 3 | 顺带：§4.3-A 的标题原本写"建议新增，可裁"——那是**提案时点**的说法，命令落地后留着会让下游重复实现 | §4.3-A 标题 + 新增一段 **第十五轮状态** | 标题改成"（第十五轮已两档落地；下面命令体一字未改）"；新增的状态注写明两档各自的落点、两档**同一个派生**（`vox_mcp::endpoints::manifest`，与 S1 的 `describe_endpoint` 是同一个函数）、退出码 `0` / `2`、无屏档怎么跑（`vox-headless --config <settings.json>`），以及**本轮实测**的那一条（见 §4.3-A）。**命令体与下面的读法一个字没改。** |
| 4 | 顺带：§4.1 的 `npm run verify` 链注释漏了 `check:agent`（与 `docs/platform/LINUX.md` 同一个缺陷类型，权威源 `app/ui/package.json` 的 `verify` 是 **9** 步） | §4.1 的链注释 + 紧跟那句验收说明 | **只改注释里的枚举**（命令体一字未改）：`… check:cable + check:capabilities + **check:agent** + qa:home`，与 `app/ui/package.json` 逐项对齐。紧跟那句"上面三条**我没有跑**"改成"**写稿当时**我没有跑"，并补一句"第十五轮以后第三条已两档落地、无屏档那一档本轮真跑过"——否则它与第 3 行新增的 §4.3-A 状态注**互相打架** |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §2.3.1 / §2.3.2 / §2.3.3 的清单实例本身、§2.7 的"进清单 / 不进清单"表、§2.5.3 的映射表 | 未动（只把"现状代码"列的行号换成符号） | 本轮只碰**引用方式**，清单形状与结论不变 |
| §4.2 的测试清单 1–12、§4.3-B、§4.4 的两态检查 | 未动（§4.2 第一层与第 6 条只换引用方式；§4.1 只改注释枚举） | 断言与命令一条没改；§4.4 的"态 A / 态 B"照旧 |
| §5.1 / §5.2 / §5.3 / §5.4 | 未动 | 与本轮两件事无关；§5.1-9 那条"`(i)` / `(ii)` 的实测结论仍未回填"仍成立 |
| §1 的其余行号（`event.rs` / `runtime.rs` / `mod.rs` / `ports.rs` / `settings.rs` 等） | 未动 | 复核点名的只有 `catalog.rs` 与 `speak.rs` 那一批；**没有重排**，仍是"写稿当时"的行号 |

---

## 修订记录（2026-09-22，第七轮 · 文档修订）

> 触发：**第十八/十九轮复核**点在**本稿**上的剩余一处——第六轮只改了**四处**「`--print-composition` 仍未落地」，§3.2 表后「第六轮状态」注的末句（本轮改前在第 1183 行）还是裸的。
> 本轮是**文档修订第七轮**，**只动本文件**：`crates/` 与 `app/` 一个字没动，清单字段、位表、断言与**命令体**都没改。

| # | 对应 | 改了哪几节 | 改了什么 / 为什么 |
| --- | --- | --- | --- |
| 1 | 第十八/十九轮复核**【重要】**：第六轮那四处改完后，§3.2 表后「第六轮状态」注的末句仍是**裸的**「`--print-composition` 仍未落地」（两档早已落地） | §3.2 表后注的末句 + 顶部状态行（`修订第 6 版` → `修订第 7 版`） | **不动命令体、不改第六轮的历史记录**：该句按第六轮同一口径补状态注 —— 「**第六轮时点仍未落地**：`--print-composition`（§4.3-A / §4.4 的命令当时跑不了；**第十五轮已两档落地**，见 §4.3-A 的状态注与顶部状态行——本句保留第六轮当时的记录）」。顶部状态行与第六轮修订记录里当年记的「四处」**不改口**（那次复核确实只点出四处，第五处是第十八/十九轮才发现）。核法：`grep -n '\*\*仍未落地\*\*' docs/plans/S0-COMPOSITION-MANIFEST.md` 改前命中 1 行（就是 §3.2 表后注那处）、改后 **0 行**；正文里其余 5 处「仍未落地」第六轮起就都带状态注（顶部状态行的两段、第四轮修订记录第 5 行、第四轮"没改的节"表、第六轮修订记录第 1 行），本段记录自身的引用不算断言 |

**本次没改的节（避免下游误以为改过）**：

| 节 | 状态 | 为什么 |
| --- | --- | --- |
| §3.2 表后注的**其余两句**（`VirtualDeviceStatus` 没删、`virtual_cable_status` 没改名）、§3.2 的表行本身、§4.3-A / §4.4 的命令体 | 未动 | 本轮只补一处**状态注**；那两句今天仍然成立；命令体从第十五轮起就没改过，本轮继续一字不动 |
| 顶部状态行与第六轮修订记录里当年记的「四处」、第五轮与第六轮两段修订记录、本稿其余全部行 | 未动 | history 不改口；本轮没动清单字段、位表、断言、owner，也没动别人的文件 |

---

## 0. 本稿的边界

### 0.1 做什么 / 不做什么

**做**（S0 的三件，`docs/architecture/DIRECTIONS.md:509`）：

1. 定稿**组合清单**的字段与取值（不含 mcu）；
2. 定稿**统一能力位模型**（provider 位与宿主位同一套机制）；
3. 给出现状的**文件:行级**映射、按文件分组的改动清单、可执行的验收标准。

**不做**（越界即算改坏）：

| 不做的事 | 为什么 |
| --- | --- |
| 动算子算法 / 音频链行为 | S0 要求"不改行为"（`DIRECTIONS.md:509`）；算子可换档是路线五，未拍板 |
| MCP / CLI / HTTP 出口 | S1（`DIRECTIONS.md:510`） |
| Android 外壳、无屏外壳 | S2 / S3（`DIRECTIONS.md:511-512`） |
| 清单里预留 `mcu` 字段 | 已拍板砍掉（`DIRECTIONS.md:469`）。本稿遵守：**不定义 mcu 变体、不定义任何 mcu 专属子字段** |
| 按 model 重建 provider 表 | `DIRECTIONS.md:315` 那条是更远的目标，S0 不碰表层级 |

**唯一一处例外**：§3.2 末尾那条 **Linux 虚拟麦接线**（`DIRECTIONS.md` §10.5 功能处置表 + §10.6 已拍）**是一次显式行为变更**——它让 Linux 上"译音进虚拟麦、对方在通话里听到译音"真的成立。除它之外，本稿不改行为：清单只是把现状**描述**出来（§2.7），Worker 一行不动。

**两条流水线都不删**（§10.5-1）：本稿对"对外说话"与"听人说话"给出同等地位的清单形状（§2.3.1 / §2.3.2 / §2.3.3）；某档宿主做不到的，是**位报 `false` + 那一格的条目进不了清单 + 界面说清**，不是把这条腿从架构里去掉。

### 0.2 本稿与既有方向的关系（推翻了什么）

| # | 议题 | 旧说法 | 本稿 | 依据 |
| --- | --- | --- | --- | --- |
| 1 | `out` 的取值表 | `out: [virtual_mic \| speaker \| net_out \| captions \| host_sink]`（`DIRECTIONS.md:254`） | **改成两个字段**：`kind` + `role`。`virtual_mic` / `speaker` / `monitor` 是 `playback` 的**角色**，不是三种组件 | 代码里三者是同一个 trait、同一个函数、只有设备名不同：`crates/vox-core/src/ports.rs:93-105`（`PlaybackSink::open(device, rate)`）、`crates/vox-core/src/pipeline/speak.rs::composition`（`playback_device` 一个 `Option` 搞定虚拟麦与耳机）、`crates/vox-core/src/pipeline/mod.rs:1378-1390`（回听是**第二个** `PlaybackSink`，设备传 `None`） |
| 2 | 顶层字段数 | 7 格（`DIRECTIONS.md:250-257`） | **7 格 + 1 个可空格 `session`**（**已拍**，2026-09-22） | "有没有云端会话"是现状里两条腿的**唯一结构性差别**（`crates/vox-core/src/pipeline/speak.rs::composition` 的 `passthrough`），塞进 7 格里没有一处能放得干净（见 §2.2）。备选方案留档在 §5.1-2 |
| 3 | `ops` 里的 `chunk` | 当作一个算子（`DIRECTIONS.md:253`） | 现状**不放 `ops`**，放 `in[].block_ms` | 代码就是这么传的：`crates/vox-core/src/pipeline/mod.rs::INPUT_BLOCK_MS`（第三轮核到 `:47`；写稿当时 `:42`）→ `mod.rs:719-722` 交给采集源；切块实现在输入组件内部：`crates/vox-dsp/src/chunk.rs:11-22`（`Blocker`），Windows 侧 `crates/vox-audio-win/src/capture/shared.rs:193-195`、Linux 侧 `crates/vox-audio-linux/src/capture.rs:342` |
| 4 | `host` 的取值 | `host: native \| wasm \| browser \| mcu`（`DIRECTIONS.md:250`，**未拍板草图**） | **`host` = 宿主档位**：`windows` / `linux_desktop` / `android` / `linux_headless` | 能力位表按**档位**索引（§2.5.1），而 native/wasm 那条轴在 mcu 砍掉（§10.0 第 2 条）、插件宿主排到"本次不承诺"（§10.2 末行）之后**没有取值可用**。桌面在这里就是**一档宿主**（§10.5-3），与 Android / 无屏并列，不是"旧版" |
| 5 | 能力位的性质 | "补能力位让界面按位降级"（`DIRECTIONS.md:298-300`，当时标**未拍板方向**） | **位 = 事实，不是期望**：位为 `true` 必须由"真的把这条路打开的那段代码"负责（§2.5.4 的 R6） | §10.5-2。这条直接改掉了 Linux 虚拟麦的处置：不再是"位说 ON、功能不存在"，见 §1.4 与 §3.2 |
| 6 | 虚拟麦是不是"可选能力" | `DIRECTIONS.md` §2.5.4 的"它应该写成**可选能力**，而不是产品的必需项" | **功能不删**：`PlaybackRole::VirtualMic` 留在类型里，某档宿主做不到就位报 `false`、界面降级 | §10.5-1（"'不是不做'，是硬件不支持就没办法"）。"可选能力"这个说法保留，但**只指能力位**，不再读成"可以不实现" |

> 口径说明：第 1、4、6 条的"推翻"按 §8 处理（新者胜 + 以代码为准），**需要回头把 `DIRECTIONS.md:250-257` 那张草图与 §2.5.4 那句改对**——本稿不改那份文件（一文件一 owner）。
> 第 2、3 条是**新增**而非推翻：原文用 `{...}` 表示"字段候选"，本稿是收敛结果。
> 第 5 条不是推翻本稿早先的写法，而是把 `DIRECTIONS.md` §10.5-2 的原则落进位表（原来只有"档位上限 × 事实"的类型，没有"谁是定义者、为 true 时凭什么"的规则）。

---

## 1. 现状证据

每一条都用 `grep` / `read` 亲自核过；行号是写稿当时的。

### 1.1 两条流水线写死在哪

| 位置 | 写死了什么 |
| --- | --- |
| `crates/vox-core/src/event.rs:15-22` | `Pipeline` 枚举 = `Speak` / `Listen` 两个值。加第三条要动这个 enum 与它所有的 `match`（`event.rs:24-38` 的 `label`/`track`），以及账本里的 `BTreeMap<Pipeline, _>`（`crates/vox-core/src/runtime.rs:181-187`） |
| `crates/vox-core/src/pipeline/mod.rs::Plan::build`（写稿当时 `:106-111`） | `Plan::build` 里 `match config.pipeline { Speak => speak::plan(config), Listen => listen::plan(config) }` —— 骨架只认两条 |
| `crates/vox-core/src/pipeline/speak.rs::composition`（写稿当时是 `fn plan`，`:15-48`；现 `:26-130`） | Speak 的作业单：`in = Microphone(input_device)`、`denoise = config.denoise`、`passthrough = !translate`、`playback_device = output_device`、`monitor_translation = …`、`hot_update = !passthrough` |
| `crates/vox-core/src/pipeline/listen.rs::composition`（写稿当时是 `fn plan`，`:16-44`；现 `:30-109`） | Listen 的作业单：`in = ProcessLoopback{executable, include_tree}`、`denoise = false`、`monitor_translation = false`、`hot_update = false`；没选程序直接报错 |
| `crates/vox-core/src/runtime.rs:700-768` | `session_config` 按 pipeline 写死：语言（speak 用设置、listen 恒 `LISTEN_TARGET_LANGUAGE`）、音色、门（listen 恒 `GateConfig::level(0.0)`、`gate_active: true`）、设备、`source_language` 有没有 |
| `crates/vox-core/src/runtime.rs:67-100` | `PipelineCommand` 枚举（Start/Stop/SetGateActive/…）：控制面也按"两条腿固定"设计 |

**算子链本身也是写死的顺序**（这是 S0 要保住的行为，必须原样表达）：

```
采集(输入组件内含切块) → to_mono → denoise? → gate → resample? → 上传
```
- `crates/vox-core/src/pipeline/mod.rs:1053-1071`（`gated_blocks`：`chunk.to_mono()` → `denoiser.process` → `gate.process`）
- `crates/vox-core/src/pipeline/mod.rs:1017-1037`（`feed`：过了阀门才 `resampler.process` → `upload`；`status.ended` 时 `resampler.flush()`）
- 降噪只在 48 kHz 生效：`mod.rs:50`（`DENOISE_RATE = 48_000`）+ `mod.rs:731-741`
- 重采样只在**非直通**时建：`mod.rs:748-767`（直通分支 `sink.open(device, format.sample_rate)`；否则 `resampler(capture_rate, session.input_sample_rate())`）
- 直通是**另一条循环**：`mod.rs:855-857`（`pump` 分流）、`mod.rs:906-913`、`mod.rs:1040-1048`（`feed_passthrough` 把放行样本直接 `sink.push`，不经过重采样、不接云端）

**下行也是两条固定出口**：`mod.rs:1197-1204`（`ServerEvent::AudioDelta` → `sink.push` + `monitor_sink.push`）、`mod.rs:1166-1190`（文字进字幕轨）。

### 1.2 平台差异现在怎么表达：命令式，不是数据

| 位置 | 内容 |
| --- | --- |
| `app/src-tauri/src/platform/mod.rs:6-30` | 平台差异的**全部**表达方式：一张对照表（`:6-15`）+ `#[cfg] mod / pub use win::* / linux::*`（`:22-30`）。**两边必须实现同一组函数名**，`lib.rs` 不写 `cfg` |
| `app/src-tauri/src/platform/win.rs:38-52` | 采集 / 播放 / 设备目录三件套的 Windows 实现注入 |
| `app/src-tauri/src/platform/linux/mod.rs:35-46` | 同三件套的 Linux 实现注入 |
| `app/src-tauri/src/lib.rs:149-241` | `assemble()`：命令式地把每一件拼起来，编号 1–12；每一步失败就发一条 `Notice`，不致命 |
| `app/src-tauri/src/lib.rs:71-97` | `invoke_handler!` 注册的 **26 个命令**（计数正确；`#[tauri::command]` 属性在 `lib.rs` 里**零命中**，都写在函数上、全在 `app/src-tauri/src/commands.rs`）——**这是唯一的控制面**（无头/CLI/MCP 的欠债源头） |
| `app/src-tauri/src/lib.rs:218-223` | 热键起不来 → `Notice::error("全局热键起不来，只能用界面上的开关：{e}")`。**这是"可选能力"的现成先例**，但它是命令式的，不是数据 |
| `app/src-tauri/src/lib.rs:236-238` | `platform::startup_notes()` → 一串 `Notice::warning`。同样是命令式 |
| `app/src-tauri/src/tray.rs:144-157` | 托盘：`TrayIconBuilder::build()` 成功 ≠ 有宿主在画它；`can_hide_to_tray()` 现问平台 |
| `app/src-tauri/src/platform/linux/mod.rs:132-145` | `startup_notes()`：PipeWire 不在、托盘宿主不在 → 两条文案 |
| `app/src-tauri/src/platform/win.rs:112-114` | Windows 上恒为空 Vec（没有前置检查） |

### 1.3 "半个能力位"的现有先例

| 位置 | 内容 |
| --- | --- |
| `app/src-tauri/src/platform/mod.rs:32-43` | `VirtualDeviceStatus { status: &'static str, multichannel_status: &'static str }` —— **全仓唯一一处成形的宿主机能力表达**，但：只覆盖一个能力、值是字符串字面量、注释里已经承认"字段按界面要显示什么定，而不是按系统 API 长什么样" |
| `app/src-tauri/src/platform/win.rs::virtual_device_status()` | Windows：`vox_audio_win::cable::detect()` → `installed` / `install_pending_reboot` / `uninstall_incomplete` / `not_installed`（写稿当时 `:93-109`；第三轮核到时已漂到 `:167` 起，外壳仍在改） |
| `app/src-tauri/src/platform/linux/mod.rs:123-130` | Linux：恒 `not_applicable` |
| `app/src-tauri/src/dto.rs:174-187` | 每次做快照都**现问平台**（`crate::platform::virtual_device_status()`），塞两个字符串进 `devices` |
| `app/src-tauri/src/dto.rs:119-125` | `DeviceSnapshotDto` 的三个虚拟麦字段；`dto.rs:8` 自己写着"`virtual_cable_installed` 保留着不去掉" |
| `app/ui/src/types.snapshot.ts:48-60` | 前端知道的**唯一**能力：`virtual_cable_status` 五态字符串 |
| `app/ui/src/sections/CableManager.tsx:83-95` | 界面唯一的"按位降级"：`not_applicable` 时整块换成一句 hint |
| `app/ui/src/i18n/zh.ts:144-150`（en.ts:142-147、ja.ts:145-151 同构） | 那句 hint 的文案：让用户去目标程序里选「Vox Virtual Mic」 |
| `app/ui/scripts/check-cable.mjs:64-78` | 这条降级**有自动化断言**（Linux 上不许出现"安装/卸载/多声道"按钮） |

> 结论：能力位这件事**已经有形状了，只是只有一个位、且值用字符串表达**。S0 是把它一般化，不是从零发明。

### 1.4 已实现但没接线的能力：Linux 虚拟麦（**最重要的一条**）

事实链：

1. 实现存在：`crates/vox-audio-linux/src/virtual_sink.rs:35-53`（`VirtualSink::create()` 在 PipeWire 图里建 `Audio/Sink` 节点，名字 `vox_virtual_mic`，`virtual_sink.rs:18,23`），真机验证脚本 `virtual_sink.rs:102-117`、`examples/virtual_mic.rs:21-31`。
2. 文档说已落地：`docs/platform/LINUX.md:314`。
3. 界面按"不需要装、你去别的程序里选它"来引导：`app/src-tauri/src/platform/linux/mod.rs:123-130` + `app/ui/src/i18n/zh.ts:146-147`。
4. **但装配层从未调用它**：全仓 `grep -rn "VirtualSink"` 只命中 `crates/vox-audio-linux/src/{lib.rs,virtual_sink.rs}` 与 `crates/vox-audio-linux/examples/*`；`app/src-tauri/src/` 里对 `vox_audio_linux::` 的引用只有 `audio.rs:14,20,26`（capture/playback/registry）与 `mod.rs:134`（`pipewire_available()`）。`PlaybackSink::open` 只是把设备名塞进 `target.object`（`crates/vox-audio-linux/src/playback.rs:227-231`），不会建节点。

→ **现状是"位说 ON、功能不存在"**：Linux 上默认 `speak.output_device = None`（`crates/vox-core/src/settings.rs` 的 `SpeakSettings::output_device` 缺省；写稿当时 `:135`，第三轮核到 `:137`），译音会播到系统默认输出；用户按界面指引去找「Vox Virtual Mic」时，系统里根本没有这个设备。
（产品意图当时**没有**找到一手文件记载；**本轮已拍**：接线——见 §3.2 末尾与 §5.1-1。）

> **第三轮时点补记**：上面这条事实链**今天已不成立**——Linux 虚拟麦**已接线**（`app/src-tauri/src/platform/linux/mod.rs::virtual_mic_ensure()`，本机实测 `virtual_mic: enabled=true`）。本节保留为**写稿当时**的现状证据（"位说 ON、功能不存在"就是这个模型的起因），**不要再当成当前故障**。

这条正是能力位模型要防的缺陷类型，也决定了 §2.5.4 的**定义者规则 R6** 与 §4.4 的真机检查点。

### 1.5 provider 侧能力位的现状与缺口

| 位置 | 内容 |
| --- | --- |
| `crates/vox-core/src/catalog.rs::ProviderCapabilities`（写稿当时 `:18-23`；现 `:21`） | `ProviderCapabilities { voice_selection, voice_clone, source_language, hot_update_language }`（4 个布尔，`Serialize`） |
| `crates/vox-core/src/catalog.rs::ProviderInfo`（写稿当时 `:25-32`；现 `:29`） | `ProviderInfo` 把上面那 4 位挂在 provider 上（不是 model 上） |
| `crates/vox-core/src/catalog.rs` 的 **4 个 `supports_*` 自由函数**（写稿当时 `:59-73`）—— 这 4 个**已被删**：现在只有 `catalog.rs::provider_capabilities()` + `catalog.rs::supports(provider, bit)` | `supports_voice_selection` / `supports_voice_clone` / `supports_source_language` / `supports_hot_update_language`（写稿当时各是一个自由函数，各问一个布尔） |
| `crates/vox-core/src/catalog.rs::supports_audio_output(language)`（写稿当时 `:98-100`；现 `:111`） | 这条不是 provider 位，是"某语言的位"，本稿不动它 |
| `crates/vox-core/build.rs:55-61` | 编译期把 `catalog/*.json` 的 `capabilities` 读成 Rust 常量（`Capabilities` 结构体字段**全必填**，无 `#[serde(default)]`） |
| `catalog/aliyun.json:40-45` / `catalog/gpt.json:30-36` / `catalog/gemini.json:26-32` | 三份 JSON 的 `capabilities` 块（GPT 另有一行文字字段 `languages`） |
| 消费点（全仓） | `crates/vox-core/src/cloud/gpt.rs:39`（源语言）、`:53`（音色选择）、`:59`（声音复刻）；`crates/vox-core/src/cloud/mod.rs:283`（热更新语言）；`crates/vox-core/src/cloud/protocol.rs:99`（语音输出语言） |
| 前端 | `app/ui/src/catalog.ts:52-55`（TS 类型）、`:96-101`（读同一份 JSON，缺字段按 `?? false`）、`:258-269`（4 个查询函数）；界面消费点 `app/ui/src/sections/PipelineCard.tsx:189-192,262-266`、`app/ui/src/mock/merge.ts:64-65` |

缺口（`docs/architecture/DIRECTIONS.md:314` 自己列的）：缺"会不会回报用量""有没有回合结束""有没有断句"。
现状后果（`DIRECTIONS.md:298-300`）：GPT 上用量页与部分延迟指标**恒为空**。

### 1.6 芯里唯一一处平台泄漏

`crates/vox-core/src/settings.rs` 的 `DEFAULT_FONT_FAMILY`（写稿当时 `:255-258`，第三轮核到 `:257-260`）：

```rust
#[cfg(windows)]
pub const DEFAULT_FONT_FAMILY: &str = "Microsoft YaHei UI";
#[cfg(not(windows))]
pub const DEFAULT_FONT_FAMILY: &str = "";
```

全 `crates/vox-core/src` 只有这一处 `cfg(windows)` / `cfg(target_os)`（grep 实测一处命中）。它不影响本稿结论，但说明"平台事实"缺一个正经的家。

---

## 2. 目标形状

### 2.1 组合清单：类型定义

新增 `crates/vox-core/src/composition.rs`（**平台无关**：不引用任何平台 API；`HostKind` 是**档位名字**这张数据表，不是平台调用）。

```rust
//! 组合清单：一台设备 / 一个端点怎么被装配。
//!
//! 清单是**数据**：能序列化、能比对、能被界面显示、能被 Agent 生成。
//! 它只描述"用哪些组件、按什么顺序、往哪去"，不含任何平台 API。

use serde::{Deserialize, Serialize};
use crate::cloud::SessionParams;          // 现有类型，见 §3 core-dev #9
use crate::gate::GateConfig;               // 现有类型（crates/vox-core/src/gate.rs:24-31）
use crate::settings::ModelProvider;        // 现有类型

/// 清单格式版本。沿用 `catalog/*.json` 的既有做法（`catalog/gemini.json:37`）。
pub const COMPOSITION_SCHEMA_VERSION: u32 = 1;

// 注意：**没有 `Eq`**——`ops` 里的 `Op::Gate` 带 `f32` 阈值，`Eq` 落不下来。
// （原稿那行 derive 里写了 `Eq`，是笔误；以类型系统为准，§2.1 的其余类型不受影响。）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub schema_version: u32,
    /// 哪一档宿主这份清单是给谁的（见 `HostKind`）。装到别的档位上会被 `missing_on` 挡下。
    pub host: HostKind,
    pub r#in: Vec<Input>,          // JSON 键名固定为 "in"
    pub ops: Vec<Op>,              // 数组顺序 = 执行顺序
    pub out: Vec<Output>,
    pub life: Life,
    pub ui: Ui,
    pub control: Vec<Control>,
    /// `None` = 不接云端（原声直通 / 纯中继端点）。见 §0.2 第 2 条（**已拍为第 8 格**）。
    pub session: Option<SessionSpec>,
}

/// **哪一档宿主**（不是"哪个进程形态"）。S0 只定义这四档，且它们是**并列**的：
/// 桌面（Windows / Linux）、手机（Android）、无屏（Linux ARM64）。
/// 不定义 wasm / browser / mcu（MCU 已砍，§10.0 第 2 条；插件宿主排后，§10.2 末行）。
/// 档位由**外壳自己**声明（`platform::host_kind()`，§3.2）——只有外壳知道自己是哪一份构建。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind { Windows, LinuxDesktop, Android, LinuxHeadless }

/// 声音从哪来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// 本机麦克风。`device = None` 时用系统默认。
    Mic {
        device: Option<String>,
        /// 切块粒度（ms）。现状由采集组件实现（`vox-dsp` 的 `Blocker`）。
        block_ms: u32,
    },
    /// 抓某个程序放出来的声音（进程环回）。
    ProcessLoopback { executable: String, include_tree: bool, block_ms: u32 },
    /// 网络声音进（端点用）。**[S3 目标，现状未实现]**
    NetIn { pipe: String, block_ms: u32 },
    /// 宿主喂进来的声音（插件/网页用）。**[目标，现状未实现]**
    HostFeed { pipe: String, block_ms: u32 },
}

/// 算子链的一节。数组顺序即执行顺序，缺项即"不装这一节"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    /// 交织多声道 → 单声道（`ports.rs:46` 的 `AudioChunk::to_mono`）。
    Mono,
    /// 降噪。`Listen` 现状不装（数字源本来就干净）。
    Denoise,
    /// 音量阀门。字段就是 `GateConfig`（`crate/vox-core/src/gate.rs:24-31`）。
    Gate { config: GateConfig },
    /// 重采样。`from` 是来源记号，`to` 是协议要的率。
    Resample { from: RateRef, to: RateRef },
}

/// 采样率引用。不写死数字：数字由 provider 决定（`cloud/mod.rs:168-173`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateRef {
    /// 采集协商到的率（`CaptureFormat::sample_rate`，`ports.rs:85-88`）。
    Capture,
    /// `Session::input_sample_rate()`（Aliyun/Gemini 16k、GPT 24k）。
    Session,
    /// `OUTPUT_SAMPLE_RATE`（24k，`cloud/protocol.rs:28`）。
    Playback,
}

/// 声音/文字往哪去。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Output {
    /// 播放汇。虚拟麦 / 耳机 / 回听**是同一个组件的三种角色**（§0.2 第 1 条）。
    Playback { role: PlaybackRole, device: Option<String>, source: EdgeSource },
    /// 字幕轨（文本出口）。
    Captions { track: crate::subtitle::Track, source: EdgeSource },
    /// 网络对端（端点用）。**[S3 目标，现状未实现]**
    NetOut { pipe: String, source: EdgeSource },
    /// 宿主收走（插件用）。**[目标，现状未实现]**
    HostSink { pipe: String, source: EdgeSource },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackRole {
    /// 译音灌进虚拟麦，给别的程序当麦克风（Windows: VB-CABLE / Linux: PipeWire sink）。
    VirtualMic,
    /// 普通出声（耳机 / 系统默认设备）。
    Speaker,
    /// 额外回听一份到系统默认设备（`pipeline/mod.rs:1382-1390`）。
    Monitor,
}

/// 这条出口的数据从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeSource {
    /// 算子链直接产出（直通原声走这条）。
    Chain,
    /// 云端会话返回（译音 / 译文走这条）。
    Session,
}

/// 谁管它的命。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Life { Interactive, Daemon, ForegroundService, Hosted }

/// 界面形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ui { Gui, Tui, Web, None }

/// 谁在操控它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control { InprocApi, Ipc, Cli, Mcp, Http, ConfigFile }

/// 云端会话：链的终点，也是 `EdgeSource::Session` 那些出口的源头。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub provider: ModelProvider,
    /// 认不认不重连的热更新（`Plan::hot_update`：只有 Speak 认）。
    pub hot_update: bool,
    /// 上行率。由 provider 决定，不写死数字。
    pub uplink_rate: RateRef,
    /// 下行率。
    pub downlink_rate: RateRef,
    /// 协议参数。**直接复用现有 `SessionParams`**（`cloud/protocol.rs::SessionParams`；写稿当时 `:72-89`，第三轮核到 `:79`）。
    pub params: SessionParams,
}
```

#### 校验与查询（同文件）

```rust
/// 清单装不上 / 不合法的地方。**一次给全部**，不是遇错就返回（界面与 Agent 都要可读的错误）。
///
/// **待补（core-dev，S1 提出）**：今天只有 `Debug/Clone/PartialEq/Eq`，**没有 `Serialize`**，
/// 而 S1 要把 `data.errors` 送上线路 → 需要补一个**带判别键**（`kind`）的 serde。
/// **线上名字以 S1 为准**（`docs/plans/S1-AGENT-FACE.md` §2.1.2 与 §3.2：`tests/protocol.rs`
/// 已按 `{"kind":"host_mismatch", …}` 断形）——S0 不在这里另定义第二套。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositionError {
    /// 某个条目的实现依赖一个这台机器没有的能力位。
    MissingCapability { bit: Capability, entry: String },
    /// `ops` 顺序不是规定顺序的子序列（例如 resample 跑到 mono 前面）。
    BadOpOrder { at: usize },
    /// 同一角色出现两次（两个 `virtual_mic`）、或该有出口却没有。
    DuplicateRole(PlaybackRole),
    /// 端点/直通清单里出现了 `EdgeSource::Session`，但没有 `session`。
    SessionEdgeWithoutSession,
    /// 有 `session` 却没有任何 `EdgeSource::Session` 的出口（译文凭空消失）。
    SessionWithoutConsumer,
    /// 一个输入口都没有（`in: []`）：声音从哪来都没说。
    ///
    /// 外部提交的清单（S1 的 `compose_endpoint`）在**第一道闸 `validate()`** 就被它挡下，
    /// 不许拖到 `Plan::from` 才炸（**D4**，2026-09-22 已落地）。
    MissingInput,
    /// 这份清单不是给这台机器的档位的（拿错了清单）。
    HostMismatch { manifest: HostKind, machine: HostKind },
}

impl Composition {
    /// 规定顺序：mono → denoise → gate → resample。缺项合法，换序不合法。
    pub const OP_ORDER: [&'static str; 4] = ["mono", "denoise", "gate", "resample"];

    /// 本机派生（§2.5.3 上下文①）：`SessionConfig`（设置）+ **这台机器的事实** → 清单。
    ///
    /// 实参是**事实**（`HostFacts`）而不是算好的报告（`CapabilityReport`）：位从事实现算
    /// （`capability::effective`），"译音往哪送才算虚拟麦"的设备名也只存在于事实里。
    /// **这是唯一签名**——S1 的投影也从 `Runtime::host_facts()` 取**同一份**事实，不另立第二份。
    pub fn of(config: &SessionConfig, facts: &HostFacts) -> PortResult<Self>;

    /// 这条清单需要哪些能力位（条目 → 位的映射表见 §2.5.3）。
    pub fn required_capabilities(&self) -> Vec<Capability>;

    /// 结构性校验。一次给**全部**问题，不是遇错就返回（界面与 Agent 都要可读的错误）。
    pub fn validate(&self) -> Result<(), Vec<CompositionError>>;

    /// 这条清单**装不到**这台机器上的地方（空 = 可以装配）。
    ///
    /// 两类：`MissingCapability`（条目要的位这台机器没有）与 `HostMismatch`（清单的
    /// `host` 不是这台机器的档位）。**不替调用方改写清单**——见 §2.5.3 的两种装配上下文。
    ///
    /// **实参是报告、不是事实**：别把报告类型的引用抄到 `of` 上——`of` 吃 `&HostFacts`，
    /// 这是**另一个函数**（`missing_on` 问的是"拿一份算好的位，这条清单装得上吗"）。
    pub fn missing_on(&self, caps: &CapabilityReport) -> Vec<CompositionError>; // ← **另一个函数**：吃报告，不是 `of` 的事实

    /// 端点 = 同一模型的最简实例：只有 in + out，没有 ops、没有 ui、没有 session。
    pub fn endpoint(pipe_in: &str, pipe_out: &str) -> Self;
}
```

**序列化形态**（`in`/`ops`/`out` 条目**一律是带 `kind` 的对象**，不做裸字符串简写——清单必须能原样序列化回读，两种形态会逼出自定义 `Deserialize`，不值）：

```json
{ "kind": "mic", "device": null, "block_ms": 20 }
{ "kind": "gate", "config": { "kind": "level", "threshold": 0.012, "tail_ms": 600, "preroll_ms": 200 } }
{ "kind": "playback", "role": "virtual_mic", "device": "CABLE Input (VB-Audio Virtual Cable)", "source": "session" }
```
（`gate` 的配置放在 `config` 子对象下：`GateConfig` 自己有一个叫 `kind` 的字段（`gate.rs:26`），内联会和 tag 撞名。）

### 2.2 字段逐条依据

| 字段 | 取值 | 为什么必须有（现状依据） |
| --- | --- | --- |
| `host` | `windows` / `linux_desktop` / `android` / `linux_headless` | **哪一档宿主**（§10.5-3：桌面只是其中一档）。它决定两件事：① `host_ceiling` 查哪一行（§2.5.1）；② 这份清单该不该装在这台机器上（§2.5.3 的 `HostMismatch`）。现状的四份外壳实现（`app/src-tauri/src/platform/`）覆盖前两档；Android / 无屏是 S2 / S3 的目标值 |
| `in[]` | `mic` / `process_loopback` / `net_in` / `host_feed` | `CaptureTarget` 现状只有前两种（`ports.rs:61-69`）；后两种是端点/宿主要用的值，**本稿只占名，S0 不实现** |
| `ops[]` | `mono` / `denoise` / `gate` / `resample` | 顺序与可选性都来自现状代码（§1.1）；`chunk` 按 §0.2 第 3 条落在 `in[].block_ms` |
| `out[]` | `playback{role,device,source}` / `captions{track,source}` / `net_out` / `host_sink` | Speak 现状 = `playback(role=virtual_mic)` + `captions(track=speak)`；Listen 现状 = `playback(role=speaker)` + `captions(track=listen)`；回听 = 第三个 `playback(role=monitor)`。**`captions` 只在有 `session` 时存在**（没接云端就没有文字）；"要不要显示"是**视图开关**（`settings.*.show_translation`），不进清单——见 §2.7 |
| `life` | `interactive` / `daemon` / `foreground_service` / `hosted` | 现状恒 `interactive`（进程 + 托盘，`lib.rs:125-140`）；后三个是 S2/S3 的目标值（`agent://EmbeddedFirst` systemd、`agent://AndroidPath` FGS、`agent://HostShells` 插件宿主） |
| `ui` | `gui` / `tui` / `web` / `none` | 现状恒 `gui`（Tauri + React）；`none` 是无屏档 |
| `control[]` | `inproc_api` / `ipc` / `cli` / `mcp` / `http` / `config_file` | 现状 = `inproc_api`（芯的公开 API，`crates/vox-core/src/lib.rs:21-37`）+ `ipc`（`lib.rs:71-97` 注册的 26 个命令，函数体在 `app/src-tauri/src/commands.rs`）。**不包含 `config_file`**：设置文件能读不能当控制面（改 `settings.json` 不会热加载），这条差别必须能被清单说清 |
| `session` | `null` 或 `SessionSpec` | Speak 关掉翻译 = `null`（`speak.rs::composition` 的 `passthrough`）；Speak 开着 / Listen = `Some`。它是"同一条腿有/无云端"的唯一差别 |

### 2.3 两条流水线的清单实例（按宿主档并列）

> **两条腿都在**（§10.5-1）。四档宿主各有一份：Windows 桌面是**已发货的现状**（§2.3.1），
> 四档的差异见 §2.3.2，Android 的那一份见 §2.3.3，无屏端点见 §2.4。
> Linux 桌面的清单与 Windows **同形**，只差 `host` / `device` 两格（见 §2.3.2）。

#### 2.3.1 Windows 桌面：两条腿的完整实例（= 现状）

**对外说话（翻译开、有音色、降噪开、开关模式电平门、不回听）**

```json
{
  "schema_version": 1,
  "host": "windows",
  "in": [{ "kind": "mic", "device": null, "block_ms": 20 }],
  "ops": [
    { "kind": "mono" },
    { "kind": "denoise" },
    { "kind": "gate", "config": { "kind": "level", "threshold": 0.012, "tail_ms": 600, "preroll_ms": 200 } },
    { "kind": "resample", "from": "capture", "to": "session" }
  ],
  "out": [
    { "kind": "playback", "role": "virtual_mic", "device": "CABLE Input (VB-Audio Virtual Cable)", "source": "session" },
    { "kind": "captions", "track": "speak", "source": "session" }
  ],
  "life": "interactive",
  "ui": "gui",
  "control": ["inproc_api", "ipc"],
  "session": {
    "provider": "aliyun",
    "hot_update": true,
    "uplink_rate": "session",
    "downlink_rate": "playback",
    "params": {
      "model_name": "qwen3.5-livetranslate-flash-realtime",
      "target_language": "ja",
      "voice": "Tina",
      "clone_frequency": null,
      "source_language": null
    }
  }
}
```

**听人说话（Windows；选了程序、有音色）**

```json
{
  "schema_version": 1,
  "host": "windows",
  "in": [{ "kind": "process_loopback", "executable": "Discord.exe", "include_tree": true, "block_ms": 20 }],
  "ops": [
    { "kind": "mono" },
    { "kind": "gate", "config": { "kind": "level", "threshold": 0.0, "tail_ms": 600, "preroll_ms": 200 } },
    { "kind": "resample", "from": "capture", "to": "session" }
  ],
  "out": [
    { "kind": "playback", "role": "speaker", "device": null, "source": "session" },
    { "kind": "captions", "track": "listen", "source": "session" }
  ],
  "life": "interactive",
  "ui": "gui",
  "control": ["inproc_api", "ipc"],
  "session": {
    "provider": "aliyun",
    "hot_update": false,
    "uplink_rate": "session",
    "downlink_rate": "playback",
    "params": {
      "model_name": "qwen3.5-livetranslate-flash-realtime",
      "target_language": "zh",
      "voice": "Tina",
      "clone_frequency": null,
      "source_language": null
    }
  }
}
```

**对照现状的几个"不能变"的点**（这几条就是验收里的对比断言）：

> 上面两份实例里的常量都是**默认值本身**，不是编的：`provider=aliyun` / `model=qwen3.5-livetranslate-flash-realtime` / `voice=Tina`（`catalog/aliyun.json:10,19,49`）；speak 默认目标语言 `ja`、listen 恒 `zh`（`catalog/aliyun.json:47-48`，后者由 `runtime.rs:738` 写死读取）；门参数照 `crates/vox-core/src/gate.rs:34-51`；`block_ms=20` 照 `pipeline/mod.rs::INPUT_BLOCK_MS`（第三轮核到 `:47`）。

| 清单里 | 现状代码 |
| --- | --- |
| Speak 有 `denoise`，Listen 没有 | `speak.rs::composition` 的 `denoise: config.denoise`；`listen.rs::composition` 的 `denoise: false` |
| Listen 的门阈值恒 0（无条件放行） | `runtime.rs:748-750` `gate: GateConfig::level(0.0)` + `gate_active: true` |
| Speak 的 `hot_update: true`，Listen 的 `false` | `speak.rs::composition` / `listen.rs::composition` 的 `hot_update` |
| Listen 的 `target_language` 恒中文、不看设置 | `runtime.rs:738` `catalog::LISTEN_TARGET_LANGUAGE` |
| Speak 无 `source_language`，Listen 有 | `speak.rs::composition`（`None`）/ `listen.rs::composition`（`config.source_language`） |
| Speak 的 `out` 有 `virtual_mic`，Listen 是 `speaker` | `speak.rs::composition` 的 `role`（译文推进 VIRTUAL CABLE）；`listen.rs::composition`（回放走默认设备） |
| `captions` 恒跟着 `session` 出现，与"要不要看"无关 | Worker 无条件 `push_text`（`mod.rs:1293-1300`）；显示开关在账本里早退（`runtime.rs:958-975`），**不在** `SessionConfig`/`Plan` 里 |

#### 2.3.2 同样的两条腿，在四档宿主上各是什么样

> 这张表就是 §10.5-1 的落地形态：**功能不删**——每一格都有东西；某个硬件档做不了时，是"这台机器上的入口/出口换一种写法（或这一格进不了清单）"，
> 不是"这条腿不存在"。位为 `false` 时清单里对应的**条目不进清单**，同时把 `(位, reason)` 交给界面（§2.5.0 第 5–6 步）。

| 宿主档 | 对外说话（Speak） | 听人说话（Listen） |
| --- | --- | --- |
| **Windows 桌面** | `in: mic` → `ops` → `out: playback{role: virtual_mic}`（设备 = VB-CABLE 输入端；`virtual_mic` 位取决于装了没）+ `captions{track: speak}` | `in: process_loopback{executable}` → `ops`（无 denoise、门恒开）→ `out: playback{role: speaker}`（默认输出；**不能**推虚拟麦，理由写在 `crates/vox-core/src/pipeline/listen.rs:6-11`）+ `captions{track: listen}`。`program_tap` 位：build ≥ 20348 为 `true`，否则 `false(unsupported)` |
| **Linux 桌面** | 与 Windows **同形**：`in: mic` → `ops` → `out: playback{role: virtual_mic}` + `captions`。差在两格：`device` 是 PipeWire 节点名（`vox_virtual_mic`），`virtual_mic` 位在**接线前 / 接线后**分别是 `false(not_wired)` / `true`（§3.2） | `in: process_loopback` → 同 Windows 的听法。`program_tap` 位 `true`（PipeWire 按程序抓，`docs/platform/LINUX.md` 已实测） |
| **Android（S2）** | `in: mic`（`mic` 位：要 `RECORD_AUDIO` + `microphone` 型前台服务，没授权时 `false(permission)`）→ `ops` → `out: playback{role: speaker}`（耳机 / 外放）+ `captions`。**`virtual_mic` 位 `false(unsupported)`** → 这一格不进清单，界面照实说"这台设备不能给别的 App 当麦克风"；产品形态 = 戴耳机听译音 + 屏幕字幕 | `in: process_loopback` **这一格进不了清单**：`program_tap` 位 `false(unsupported)`（通话类 `VOICE_COMMUNICATION` 结构性拿不到；媒体类即便允许被录也要每次授权）。**腿仍在架构里**——它在这档宿主上的实际形态要等 S2 真机定位（§5.1-4） |
| **无屏 ARM64（S3）** | 端点形态（§2.4）：`in: net_in` → `out: net_out`；挂了 USB 声卡时可换成 `in: mic` / `out: playback{role: speaker}`（位由"设备在不在"决定）。`captions` / `virtual_mic` / `tray` / `global_hotkey` 位均为 `false` | 同左：无屏档的"听人说话"= **从网络收别人的声音**（`net_in`），不是本机抓程序（`program_tap` 位 `false`）；腿通过 `net_in` 还在 |

四档共用的形状（两条腿都是）：

```
in[]（现状只有一个） → ops[]（mono → denoise? → gate → resample?） → out[]（一个播放 + 一个字幕） + session?
```

#### 2.3.3 Android 那一档的完整实例（S2 目标）

`host` 换 `android`、播放角色从 `virtual_mic` 换 `speaker`、`life` / `ui` / `control` 换成手机那几格：

```json
{
  "schema_version": 1,
  "host": "android",
  "in": [{ "kind": "mic", "device": null, "block_ms": 20 }],
  "ops": [
    { "kind": "mono" },
    { "kind": "denoise" },
    { "kind": "gate", "config": { "kind": "level", "threshold": 0.012, "tail_ms": 600, "preroll_ms": 200 } },
    { "kind": "resample", "from": "capture", "to": "session" }
  ],
  "out": [
    { "kind": "playback", "role": "speaker", "device": null, "source": "session" },
    { "kind": "captions", "track": "speak", "source": "session" }
  ],
  "life": "foreground_service",
  "ui": "gui",
  "control": ["inproc_api", "http"],
  "session": { "…": "与 §2.3.1 的 Speak 那份逐格相同" }
}
```

- `ops` 与 Windows 的 Speak **逐项相同**：S2 只换音频后端与外壳，算子链不动（`DIRECTIONS.md` §10.2 的 S2 行）。
- `life: foreground_service`：Android 14 起麦克风型前台服务必须**从可见 Activity 启动**（`agent://AndroidPath`）。
- `control` 没有 `ipc` / `cli`（手机没有 CLI），有 `http`（S1 的本机 loopback 通道，`DIRECTIONS.md` §10.1-3）。
- 这份里**看不到 `virtual_mic`**，但那不是"删了功能"：`PlaybackRole::VirtualMic` 仍在类型里、仍是 Windows / Linux 的取值；Android 这档的 `virtual_mic` 位报 `false(unsupported)`，所以那一格进不了清单。

### 2.4 端点：同一模型的最简实例

```json
{
  "schema_version": 1,
  "host": "linux_headless",
  "in": [{ "kind": "net_in", "pipe": "default", "block_ms": 20 }],
  "ops": [],
  "out": [{ "kind": "net_out", "pipe": "default", "source": "chain" }],
  "life": "daemon",
  "ui": "none",
  "control": ["mcp", "cli", "config_file"],
  "session": null
}
```

`Composition::endpoint("default", "default")` 产出上面这一份，`host` 由调用方填（这里是 `linux_headless`）。
它与桌面那份**共用全部类型、全部校验、全部能力位**——不存在第二套抽象。
`life: daemon` + `ui: none` 就是无屏这档的全部"外壳属性"；`out` 里没有 `captions`，因为没有屏幕可显示（`captions` 位 `false`）。

### 2.5 统一能力位模型

新增 `crates/vox-core/src/capability.rs`（平台无关）。

#### 2.5.0 位是怎么来的：`HostFacts` → 位 → 快照 → 界面降级

一条链，六步，每步一个 owner、一步一份可观察证据。**没有第七步**——位不许有第二个来源。

| # | 谁做 | 做什么 | 代码落点 | 可观察证据 |
| --- | --- | --- | --- | --- |
| 1 | **shell-dev** | 外壳**声明自己是哪一档**：`platform::host_kind() -> HostKind`（四档各一份实现；档位 = "这是哪一份构建"，只有外壳知道） | `app/src-tauri/src/platform/{mod.rs,win.rs,linux/mod.rs}`（§3.2） | `--print-composition` 打出的 `host`（§4.3-A） |
| 2 | **shell-dev** | 外壳**报事实**：`platform::host_facts() -> HostFacts { host, off, virtual_mic_device }`。**只报关掉的位**（`captions` 起不来 → `off[captions] = unsupported`、起来了又没了 → `busy`；没有 `input` 组 → `off[global_hotkey] = permission`；VB-CABLE 没装 → `off[virtual_mic] = not_installed`；Linux 没接线 → `off[virtual_mic] = not_wired`；**`mic` 不在这里写**——第四轮 F1：这一位是"按上限报开"的有意偏离，见 §2.5.4 那一行），外加一个"译音往哪送才算虚拟麦"的设备名。每个位的定义者见 §2.5.4 | 同上一行 | `jq '.capabilities.host'`（§4.3-A） |
| 3 | **shell-dev** | 装配时**注入一次**：`assemble()` 里 `runtime.set_host_facts(facts)`；这台机器的事实变了（虚拟麦节点掉了、悬浮窗被关了 / 又起来了、SteamVR 连上或掉了、自启注册被策略改掉）就再调一次（今天走 `devices.rs` 的 4 秒 tick） | `app/src-tauri/src/lib.rs` 的 `assemble`（§3.2）；`Runtime::set_host_facts` 与 `set_control` / `set_hotkey_host` / `set_secret_store` 同款（`crates/vox-core/src/runtime.rs:270-284`） | 单测：`set_host_facts` 之后 `capabilities()` 跟着变 |
| 4 | **core-dev** | **求值**：`effective(facts) = host_ceiling(facts.host) − facts.off`。`off` 里出现**上限之外**的位 = 外壳 bug → 装配时校验失败 + 一条 `Notice` | `crates/vox-core/src/capability.rs`（§3.1） | 单测 `facts_can_only_turn_bits_off`（§4.2-10） |
| 5 | **core-dev** | **进快照**：`Snapshot.capabilities = Runtime::capabilities()`（`CapabilityReport { tier, host, speak, listen }`）。**同一个值**也喂给 `Composition::missing_on()` → "清单能不能装"和"界面降不降级"用的是同一份位 | `runtime.rs` 的 `Snapshot`（`:132-145`）、`composition.rs`（§3.1） | `describe_endpoint.capabilities`（S1 §2.1.3）；`--print-composition` |
| 6 | **shell-dev / agent-face-dev** | **消费**：界面按 `(位, reason)` 渲染或降级（§2.6 的 R1–R9，文案在 `app/ui/src/i18n/*`）；S1 的 `describe_endpoint` 直接返回同一份报告，**不另立** | `app/ui/**`；S1 的 `endpoints.rs` | `check-capabilities.mjs`（§4.3-B） |

两条**不变量**（本轮拍板落进来的）：

- **R6（定义者）**：位为 `true` ⇔ "真的把这条路打开的那段代码"认为它开着（§2.5.4）。位不是"平台有没有这个 API"，而是"**这次启动有没有把它打开**"。
- **上限是硬的**：事实只能在档位上限**里面**关。把上限之外的位塞进 `off` 是外壳 bug，不是"新能力"——这样"谎报能力"在结构上被封住。

**一条容易读错的推论（第四轮 F2 的口径，写在这里免得被当成"缺省事实下会话起不来"）**：

位为假**不等于**会话起不来，两条腿的失败模式**不一样**：

- **Speak 腿**：`virtual_mic` 关着只把那格出口的角色**降级成 `Speaker`**（`crates/vox-core/src/pipeline/speak.rs::composition` 里的 `role`），`in` 仍有 `Mic`、`session` 仍有值 → `validate()` 通过、`Plan::from` 成功 → **译音静默播到系统默认输出**（"静默"是指没人按位报出来；快照里位与 reason 是有的，是界面那一侧要按 §2.6 R1 说清）。
- **Listen 腿**：`program_tap` 关着 → `in` 那一格进不了清单 → `in` 为空 → **`MissingInput`**（§2.1 的 `validate()` 是第一道闸），这才是唯一的"清单不合法"路径。

于是："缺省事实窗口里 `Plan::build` 会以 `PortError` 拒掉 Speak 会话"这个说法**不成立**（它把 Listen 的失败模式当成了普遍行为；Speak 是降级、Listen 才是 `MissingInput`）。装配第 10–13 步之间那条热键窄窗口（`lib.rs`：热键第 10 步早于注入第 13 步）里，按热键起来的是**一条播到默认输出的 Speak 会话**，不是"起不来"。

#### 2.5.1 位表

**怎么读这张表**：格子里是**这台机器今天会报的有效值**（= 档位上限 − 关掉的）。`false(reason)` 里的 reason 就是 `off` 表里那一条；
`false(unsupported)` 表示**档位上限里根本没有这一位**（结构上做不到）。**本仓库没有外壳的档位（Android / 无屏）标 `[未核实]`**——它们是设计输入，不是实测。

**宿主/设备位（11 个，全部为新增）**

| 位 | 意思 | Windows | Linux 桌面 | Android（S2） | 无屏 ARM64（S3） | 依据 |
| --- | --- | --- | --- | --- | --- | --- |
| `mic` | 本机麦克风采集 | `true` | `true`（PipeWire 是前提：连不上时启动期只发一条 `Notice::warning`、**不致命**——`platform::startup_notes()`（`app/src-tauri/src/lib.rs` 的 `assemble` 第 12 步调它，实现在 `platform/linux/mod.rs`）；真正抓不到声音是**起流时**的事，这一位不因此翻假） | `false(permission)`，授权 + 前台服务起来后 `true` **`[未核实]`** | `false(unsupported)`；挂 USB 声卡且枚举得到时 `true` **`[未核实]`** | `ports.rs:61-63`；`crates/vox-audio-linux/src/lib.rs:31`（`pipewire_available`）；Android 见 `agent://AndroidPath`；**第四轮 F1**：这一位**按档位上限报开**——没有装配期定义者，是**有意偏离 R6**（代价见 §2.5.4 那一行与 §5.3 第 2 行） |
| `program_tap` | 抓指定程序的声音 | `true`（build ≥ 20348）；更老的系统 `false(unsupported)` | `true`（PipeWire 按程序抓） | `false(unsupported)`：通话类 `VOICE_COMMUNICATION` 结构性拿不到 **`[未核实]`** | `false(unsupported)`（无屏档不需要本机抓程序，走 `net_in`） | `crates/vox-audio-win/src/osver.rs:14`（门限常量）+ `:38`（探针）；`platform/win.rs:37-38`（"采集用**严格版**……不偷偷退化成整机环回"）；`crates/vox-audio-linux/src/capture.rs`；`DIRECTIONS.md` §10.1-2 |
| `virtual_mic` | 别的程序能把我们当麦克风 | `true` iff VB-CABLE 已装（`cable::detect() == Installed`）；未装 `false(not_installed)`；装/卸待重启 `false(pending_reboot)` | **第三轮时点：已接线，实测 `true`**（`virtual_mic_ensure()` 把节点建出来；接线前的中间态是 `false(not_wired)`，§3.2）；`true` iff 句柄建成功，建失败 `false(unsupported)` | `false(unsupported)`：没有官方虚拟麦接口（要 root 或系统签名）**`[未核实]`** | `false(unsupported)` | `platform/win.rs::virtual_mic_ensure()`；`platform/linux/mod.rs::virtual_mic_ensure()`；`crates/vox-audio-linux/src/virtual_sink.rs:35-53`；§3.2 |
| `captions` | 屏幕 / 窗口字幕 | `true` | `true`（纯 Wayland 无 XWayland 时**形态退化**成窗口内字幕，位仍为真——字幕照样有） | `true`（**应用内**字幕可用）；**跨 App** 悬浮窗是另一件事，要 `SYSTEM_ALERT_WINDOW`，且按 §10.5 排在"实现靠后" **`[未核实]`** | `false(unsupported)`（没有屏幕） | `ports.rs:211-218`；`DIRECTIONS.md:51`；§10.5 功能处置表；**第四轮 F1**：定义者 = `overlay::start` 的返回值 × `platform::overlay_running()`（**已接线**，§2.5.4） |
| `global_hotkey` | 全局热键 | `true` | `false(permission)`：不在 `input` 组就起不来；加组后 `true` | `false(unsupported)` **`[未核实]`** | `false(unsupported)` | `ports.rs:190-193`；`app/src-tauri/src/platform/linux/mod.rs` 的热键错误文案 |
| `tray` | 有东西在**显示**托盘图标 | `true` iff 图标装上（`tray.rs::AVAILABLE`）——Windows 的通知区域恒在，所以这一位等于**安装是否成功** | `true` iff 装上 **且** D-Bus 上有 StatusNotifier 宿主；否则 `false(unsupported)` | `false(unsupported)` **`[未核实]`** | `false(unsupported)` | `app/src-tauri/src/tray.rs:155-157`（`AVAILABLE × tray_host_available()`）；`platform/linux/mod.rs:154-183` |
| `background_service` | 无人值守常驻 / 开机自启 | `true`（`tauri-plugin-autostart`） | `true`（同上；systemd `--user` 也成立） | `false(permission)`：前台服务必须**从可见 Activity 启动** **`[未核实]`** | `true`（systemd `--user`，S3 目标）**`[未核实]`** | `settings.rs:65-66`；`agent://AndroidPath`；`agent://EmbeddedFirst`；**第四轮 F1**：定义者 = `events::sync_autostart` 的返回值（查不到 → `unsupported`、写不进 → `permission`；**已接线**，§2.5.4） |
| `vr_captions` | 头显里那份字幕 | 构建带 `steamvr-overlay` **且** `vr_overlay::status()` 说连上了才 `true`（装配期线程没起来 → `not_wired`；HMD/运行期不在 → `unsupported`；都在但没连上 → `busy`）；feature 没编进去 → `false(not_built)` | `false(unsupported)` | `false(unsupported)` | `false(unsupported)` | `app/src-tauri/Cargo.toml:23-24`；`lib.rs:43-44,203-208,264-266`；`vr_overlay::{status,hmd_ready}`（**第四轮 F1 已接线**，§2.5.4） |
| `net_in` | 从网络收声音 | `false(unsupported)`（未实现） | `false(unsupported)` | `false(unsupported)` | **S3 目标**：实现前恒 `false(unsupported)`（§2.6 R8） | 无代码依据——**新增位** |
| `net_out` | 往网络发声音 | `false(unsupported)` | `false(unsupported)` | `false(unsupported)` | 同 `net_in` | 同上 |
| `file_config` | 配置 / 状态能不经界面进出（无屏可运维） | `false(unsupported)`：文件能读**不能当控制面**（改 `settings.json` 不热加载，§2.7） | 同 Windows | `false(unsupported)` **`[未核实]`** | **S3 目标**（systemd 环境/状态目录），实现前恒假 | `app/src-tauri/src/lib.rs` 的 `app_config_dir()`；`agent://EmbeddedFirst` §6 |

> 三条读表须知：
> 1. **`false(unsupported)` ≠ "这条腿不做了"**（§10.5-1）。它只说明**这一档宿主**这条入口/出口进不了清单；别的档位照旧，
>    界面必须把"这台设备做不到"说出来（§2.6 R9）。
> 2. **Windows / Linux 两列的"今天"是实测过的**（除 `program_tap` 的 build 门限只核到注释）；Android / 无屏两列**没有本仓库外壳**，
>    凡是没有官方文档直接支撑的格子一律标了 `[未核实]`，S2 / S3 真机时必须回来改这张表。
> 3. **上限与事实分两层**：表里写的是**有效值**；`host_ceiling(tier)` 只回答"结构上有没有这一位"，
>    而"今天有没有"由外壳的 `HostFacts.off` 决定（§2.5.0 第 2、4 步）。
> 4. **"位为真"的凭据逐个查 §2.5.4**：每一位都该有一句"真的把它打开了的那段代码"。**`mic` 是唯一的例外**
>    （有意"按上限报开"，代价写在那一行）；`captions` / `background_service` / `vr_captions` 三位在第四轮
>    之前属于"报开但没定义者"，现在已接上（§2.5.4 第四轮三条）。


**provider 位（同一套词汇，值有两个来源）**

| 位 | 现状值 | 值从哪来 | 依据 |
| --- | --- | --- | --- |
| `voice_selection` / `voice_clone` / `source_language` / `hot_update_language` | **已有**（4 个布尔） | `catalog/*.json` 的 `capabilities` 块（编译期烘焙） | `catalog.rs::ProviderCapabilities`（4 个布尔；映射见 `catalog.rs::provider_capabilities`） |
| `usage_reporting` | **占名**，值恒 `false` | **只在代码枚举里**（不进 JSON）；且它的界面消费者（用量页）已按 `DIRECTIONS.md` §10.5「功能处置表」砍掉 → 这一位**暂时没有消费点** | `DIRECTIONS.md:298-300,314`；§10.5 功能处置表 |
| `speech_activity`（服务端报说话起止） | **占名**，恒 `false` | 同上一行 | 同上 |
| `turn_end`（服务端报回合结束） | **占名**，恒 `false`（GPT 无） | 同上 | 同上 |
| `source_transcript`（回报源文） | **占名**，恒 `false` | 同上 | `mod.rs:1194-1196` 有 `ServerEvent::SourceTranscriptDelta`，说明是可选事件 |

> 后 4 位的处理**已拍（2026-09-22）**：只进 `Capability` **代码枚举**占名，**不进 `catalog/*.json`**、
> `vox-core/build.rs` 的 `Capabilities` 结构体**不动**（所以三份 JSON 也不用加字段、不用 bump `schema_version`）。
> `supports(provider, bit)` 对这 4 位一律返回 `false`，直到这条方向另行拍板。
> 原稿写的是"位表占名 + JSON 加字段（全 false）"——**那一条被本轮推翻**（原因：给未拍板的方向落外部数据文件，等于把没定的东西变成契约）。

#### 2.5.2 数据位置：位住在哪

| 位的种类 | 存哪 | 谁写 | 谁读 | 为什么 |
| --- | --- | --- | --- | --- |
| 宿主：**哪一档** | `HostKind`，外壳装配时声明 | shell-dev（`platform::host_kind()`） | 芯（查上限表）+ 快照 + 界面 | 它决定查 `host_ceiling` 的哪一行，也是"这份清单该不该装在这台机器上"的判据（§2.5.3 的 `HostMismatch`）。**只有外壳知道自己是哪一份构建**，但档位本身是数据（字符串枚举），芯读它不违反"芯不碰平台 API" |
| provider 位：**已有那 4 个** | `catalog/*.json` 的 `capabilities` 块 | core-dev（编译期烘焙进 `vox-core/build.rs`） | 芯（`cloud/*`）+ 界面（`catalog.ts` 直接读同一份 JSON） | 它已经是"单数据源前后端共读"（`DIRECTIONS.md:306`），且支持运行期覆盖；**不动层级**（按 model 建表是更远的事） |
| provider 位：**待补那 4 个** | **没有存储**：只在 `Capability` 枚举里占名，值恒 `false` | —（core-dev 只写枚举） | — | 已拍（§2.5.1）：未拍板的方向不落数据文件，否则等于提前把没定的东西变成契约 |
| 宿主位：**档位上限** | `crates/vox-core/src/capability.rs::host_ceiling(HostKind) -> CapabilitySet` | core-dev | 芯 + 界面 + S1 的 `describe` 出口 | S1 要回答"**别的**档位能不能做虚拟麦"，问的是另一个档位而不是本机 → 表必须在芯里（数据，不是平台 API）。**上限只此一份**，外壳不许自带一张 |
| 宿主位：**这台机器现在** | `HostFacts { host, off }`，外壳装配时构造 → 注入账本 → 出现在快照 | shell-dev | 界面 / CLI / MCP / `Composition::missing_on` | 它是**运行期事实**（PipeWire 在不在、D-Bus 有没有托盘宿主、VB-CABLE 装没装、虚拟麦句柄建没建起来），静态 JSON 表达不了；而 `catalog` 是编译期烘焙的，不能承载每秒都可能变的东西 |

> 两个方向都不能少：**上限在芯**（回答"另一个档位行不行"），**事实在外壳**（回答"这台机器现在行不行"）。
> 有效值 = 两者相减，只在芯里算一次（§2.5.0 第 4 步），界面与 S1 拿到的都是同一个结果。

细化：

```rust
// crates/vox-core/src/capability.rs

/// 位的名字。JSON 键名 = `snake_case` 名字，和 provider 侧现有做法一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability { /* §2.5.1 的那 19 个 */ }

impl Capability {
    pub const ALL: &'static [Capability];
    /// 这位属于宿主还是 provider。查询 API 按它分派。
    pub fn scope(self) -> CapabilityScope;
    /// 面向用户的短标签（英文标识，不是给人看的句子；文案在界面 i18n 里）。
    pub fn id(self) -> &'static str;
}

/// 位集：u64 位图。19 个位，留够余量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "Vec<Capability>", from = "Vec<Capability>")]   // 序列化成"开着的位"数组
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const fn empty() -> Self;
    pub const fn of(bits: &[Capability]) -> Self;   // 便于写表
    pub const fn contains(self, bit: Capability) -> bool;
    pub const fn union(self, other: Self) -> Self;
    pub const fn difference(self, other: Self) -> Self;
    pub fn single(bit: Capability) -> Self;
    pub fn iter(self) -> impl Iterator<Item = Capability>;
}

/// 为什么没有这一位。**不是句子**——芯不许带界面文案，也不许带 i18n 依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// 结构上做不到（Android 没有虚拟麦；GPT 不回报用量）。
    Unsupported,
    /// 需要用户先装个东西（VB-CABLE）。
    NotInstalled,
    /// 需要用户先授权 / 加组 / 去系统设置页（`input` 组、`RECORD_AUDIO`、`SYSTEM_ALERT_WINDOW`）。
    Permission,
    /// 这个构建里没编进去（cargo feature 没开：`steamvr-overlay`）。
    NotBuilt,
    /// 实现存在、但**装配层还没把这段路接上**（Linux 虚拟麦接线前的中间态，§3.2）。
    /// 专用来说明"不是平台做不到，是我们还没接"——位必须报假，直到持有句柄的那段代码
    /// 真的把节点建出来（`§2.5.4` 的 R6）。
    NotWired,
    /// 装了或卸了，但要重启才生效（VB-CABLE 的 `install_pending_reboot` / `uninstall_incomplete`）。
    PendingReboot,
    /// 暂时不可用（麦克风被别的程序占着）。
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub enabled: bool,
    /// `enabled == true` 时必须为 `None`（校验函数保证）。
    pub reason: Option<UnavailableReason>,
}

impl CapabilityStatus {
    pub const ON: Self = Self { enabled: true, reason: None };
    pub const fn off(reason: UnavailableReason) -> Self;
}

/// 这台机器报上来的事实：**只报"关掉的位"**，其余按档位上限算开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostFacts {
    pub host: HostKind,
    pub off: std::collections::BTreeMap<Capability, UnavailableReason>,
    /// 这台机器上"译音该往哪个设备送才算虚拟麦"。`virtual_mic` 位为假时是 `None`。
    ///
    /// 由外壳填：Linux = 节点名 `vox_virtual_mic`（**接线后才非空**，§3.2）；
    /// Windows = 用户在设置里选的 VB-CABLE 端点，所以这里恒 `None`，仍走 `settings.output_device`。
    /// 它是**数据**（一个字符串），芯读它不违反"芯不碰平台 API"。
    pub virtual_mic_device: Option<String>,
}

/// 某个 host 档位结构上能有什么。数据表，不含平台 API。
pub fn host_ceiling(host: HostKind) -> CapabilitySet;

/// 有效位 = 档位上限 − 这台机器关掉的。
pub fn effective(facts: &HostFacts) -> CapabilitySet;

/// 给界面/出口看的一份完整报告（也进 `Snapshot`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityReport {
    /// 这份报告是哪一档宿主算出来的（§2.5.0 第 1 步）。S1 的 `describe_endpoint` 用它回答
    /// "这台设备是什么"；`missing_on` 用它判 `HostMismatch`。
    /// **注意字段名**：`host` 这个键在本结构里一直是**宿主机位表**（S1 稿已按这个名字引用，
    /// 不改），档位叫 `tier`。
    pub tier: HostKind,
    /// 宿主/设备位：每个位都给状态（开着的也在，界面要按"有几位、关了几位"排版）。
    pub host: std::collections::BTreeMap<Capability, CapabilityStatus>,
    /// provider 位，按当前 Speak 选中的 provider 解析。
    pub speak: std::collections::BTreeMap<Capability, CapabilityStatus>,
    /// provider 位，按当前 Listen 选中的 provider 解析。
    pub listen: std::collections::BTreeMap<Capability, CapabilityStatus>,
}
```

序列化出来长这样（TS 侧一眼可用）：

```json
"capabilities": {
  "tier": "windows",
  "host":   { "mic": {"enabled":true,"reason":null},
              "virtual_mic": {"enabled":false,"reason":"not_installed"} },
  "speak":  { "voice_clone": {"enabled":true,"reason":null} },
  "listen": { "source_language": {"enabled":false,"reason":"unsupported"} }
}
```

Linux 桌面**接线前**的那一份（§3.2 的中间态，这就是它该长成的样子）：

```json
"capabilities": {
  "tier": "linux_desktop",
  "host":   { "mic": {"enabled":true,"reason":null},
              "virtual_mic": {"enabled":false,"reason":"not_wired"} }
}
```

**关键边界**：`off` 表里出现档位上限之外的位置 = 外壳 bug（芯在装配时校验并发一条 `Notice`）。这样"谎报能力"在结构上被封住——上限是硬的，事实只能在它里面关。

#### 2.5.3 查询 API 形状

**芯内**（一处解析、一处存放，符合"单一账本"）：

```rust
impl Runtime {
    /// 装配时注入一次；这台机器的事实变了再调（例如设备轮询线程发现麦克风被占）。
    pub fn set_host_facts(&self, facts: HostFacts);   // 与现有 set_control/set_hotkey_host/set_secret_store 同款（runtime.rs:270-284）

    /// 这台机器报上来的**同一份事实**（`crates/vox-core/src/runtime.rs::host_facts`）。
    /// S1 的投影拿它去调 `Composition::of`——**不另立第二份事实**（§3.1 的 D1）。
    pub fn host_facts(&self) -> HostFacts;

    /// 有效能力位报告。快照里那份就是它。
    pub fn capabilities(&self) -> CapabilityReport;
}
```

provider 侧沿用现有习惯（自由函数 + 一个"点名"入口），把 4 个 `supports_*` 收敛成 1 个：

```rust
// crates/vox-core/src/catalog.rs
/// provider 会什么。名字与 §2.5.1 的位表一致。
pub fn provider_capabilities(provider: ModelProvider) -> CapabilitySet;

/// 点名查询。旧的 4 个 `supports_*` 由它取代（调用点见 §3）。
pub fn supports(provider: ModelProvider, bit: Capability) -> bool;
```

**条目 → 需要的位**（这张表是整个模型的枢纽：让"清单能不能装"和"界面降不降级"变成同一个问题）：

| 清单条目 | 需要的位 |
| --- | --- |
| `in: mic` | `mic` |
| `in: process_loopback` | `program_tap` |
| `in: net_in` | `net_in` |
| `in: host_feed` | 无（宿主自己给的，永远可用） |
| `out: playback{role:virtual_mic}` | `virtual_mic` |
| `out: playback{role:speaker/monitor}` | 无 |
| `out: captions` | `captions` |
| `out: net_out` | `net_out` |
| `out: host_sink` | 无 |
| `session` + provider 的 `voice` 非空 | provider 的 `voice_selection` |
| `session.params.clone_frequency` 非空 | provider 的 `voice_clone` |
| `session.params.source_language` 非空 | provider 的 `source_language` |
| `session.hot_update == true` | provider 的 `hot_update_language` |
| 用量 / 延迟面板（**该页面已按 `DIRECTIONS.md` §10.5 功能处置表砍掉**） | provider 的 `usage_reporting`——占名、恒 `false`；砍掉页面后**这一位暂时没有消费点** |

**装配时的行为——先分清两种上下文**（原文只写了后一种，会与"按能力位开门/关门"打架，这里说清）：

| 上下文 | 谁产生清单 | 位为假时怎么办 | 依据 |
| --- | --- | --- | --- |
| **① 本机派生**：`Settings → Composition::of(&config, &facts)` | 我们自己的外壳 | **开门/关门**：位为假的条目**不进清单**（Android 那份里就没有 `virtual_mic`，§2.3.3），同时把 `(位, reason)` 交给界面（§2.6 R1/R2/R9）。**这不是"偷偷降级"**——降级是被**报出来**的：快照里有位和 reason，界面必须说清"这台设备做不到" | §10.5-1（"硬件不支持我们就没办法，但不是不做"） |
| **② 外部提交**：S1 的 `compose_endpoint`、手写 / Agent 生成的清单 | 别人 | **硬错误**：`missing_on(&caps)` 非空 → 拒绝并回报（S1 映射成 `endpoint_unavailable`）。**绝不替调用方改写清单**、绝不悄悄少装一格。（更早的一道闸是 `validate()`：`in: []` 在那里就报 `MissingInput`，见 §2.1 与 D4——顺序是先 `validate()` 再 `missing_on()`，与 S1 §2.1.3-③ 的第 1 / 第 2 步一致） | `app/src-tauri/src/platform/win.rs:37-38` 的注释——"采集用**严格版**：系统不支持进程环回就报错，不偷偷退化成整机环回"（**门限本身**在 `crates/vox-audio-win/src/osver.rs:14`）。S1 的 `compose_endpoint` 用法见 `docs/plans/S1-AGENT-FACE.md` §2.1.3-③ **第 2 步（`missing_on`）**——那一步排在校验之后、diff 之前，正是为了让 `HostMismatch` / `MissingCapability` 说得比"这一格不能改"准 |

`HostMismatch` 只可能在 ② 出现（① 的 `host` 就是外壳自己声明的档位）。

#### 2.5.4 位由谁负责说实话（**R6 定义者规则**）

每一个位必须有**唯一的定义者**，且定义者必须是"真的把这条路打开的那段代码"所在的模块：

| 位 | 定义者（**就是"把这条路打开"的那段代码**） | "为 `true`"凭的是什么（可观察证据） |
| --- | --- | --- |
| `mic` | **没有定义者，而且这是有意写下来的**（F1 / ★1，第四轮）：采集源是**开会话时**才由 `platform::capture_factory()` → `CaptureSource::start()` 起流，那是**会话期**的事；`assemble()` 第 13 步注入事实时装配层拿不到任何否定证据。于是这一位**按档位上限报开**——外壳的 `HostFacts.off` 里**不写** `mic`（第四轮实测：全 `app/src-tauri/src` 的 `off.insert(Capability::…)` 里**没有它**——接线前 5 处、三位定义者接线后 Windows 7 处 / Linux 6 处，仍没有它） | 真起了流当然为真，但那条意见**不在装配期**。**这是一处偏离 R6 的代价**：麦克风被独占 / 没授权时位仍报 `true`，界面不会按位降级，用户要到"按了开始没反应"才发现；`UnavailableReason::Busy` 在**麦克风这一路**没有任何产生点（它只在本表的 `captions` / `vr_captions` 两行产生，§5.1-6）。要收口只剩一条路：把定义者挪到**起流结果**上（会话期回写事实），S0 不做。**这条偏离有钉子**：`platform::tests::mic_is_reported_on_the_ceiling_until_there_is_a_definer`（钉住"外壳不替芯关门"） |
| `program_tap`（Windows） | **已有**：`vox_audio_win::process_loopback_available()`（`crates/vox-audio-win/src/osver.rs:38`；门限常量 `MIN_PROCESS_LOOPBACK_BUILD = 20348` 在 `osver.rs:14`；`crates/vox-audio-win/src/lib.rs:39` 已重导出，`capture/mod.rs:331,369` 已在用）。S0 只要求把它的结果写进 `HostFacts.off`，**不许新增同名/同义函数** | 它返回 `false` → `off[program_tap] = unsupported` |
| `program_tap`（Linux） | `vox_audio_linux::pipewire_available()`（`crates/vox-audio-linux/src/lib.rs:31`） | PipeWire 会话在 |
| `virtual_mic`（Windows） | `platform/win.rs` 读 `vox_audio_win::cable::detect()`（现 `platform/win.rs::virtual_mic_ensure()`，写稿当时 `:93-109`）。**这个方法只描述"VB-CABLE 装没装"（安装器状态）；"能不能用"的唯一真相是能力位**（§3.1 的 `ports.rs` 行，Main D2） | `CableStatus::Installed` → `true`；`NotInstalled` → `off(not_installed)`；`InstalledPendingReboot` / `UninstallIncomplete` → `off(pending_reboot)` |
| `virtual_mic`（Linux） | `platform/linux/` 里**持有 `VirtualSink` 的那一处**（`platform/linux` 的 `virtual_mic_ensure()`；§3.2） | 装配时 `VirtualSink::create()` 成功（句柄活到退出，`destroy()` 时才为假）。**接线前**没有这条 → `off(not_wired)`（第三轮时点已接线，本机实测 `true`） |
| `captions` | **两段证据，缺一不可**（owner shell-dev，第四轮 F1）：① **装配期** `overlay::start(&state)` 的结果——它同时盖住"悬浮窗起来了"与"字幕帧线程起来了"，由 `platform::record_captions(..)` 记进观测槽（与 `platform::record_hotkeys` 同一套做法）；② **活体探针** `platform::overlay_running()`（帧线程发现"窗没了"用的就是同一个探针）。**只看 `overlay_running()` 不够**：窗口从没起来时它也返回 `false`，而那句必须报假时用的 `reason` 只有 `overlay::start` 给得出 | 起来且活着 → `true`；起不来 → Windows `off(unsupported)`（分层窗建不出来）/ Linux `off(not_wired)`（GTK 建窗只有"不在 GTK 主线程"这一条失败路，`crates/vox-overlay-linux/src/window.rs:62-68`），失败原因载体是 `platform::spawn_overlay` 的 `OverlayFailure { reason, error }`；起来了后来又没了 → `off(busy)`。**窗口内字幕**（纯 Wayland 无 XWayland 时的退化形态）算"能画"，位仍为真；Android 跨 App 需 `SYSTEM_ALERT_WINDOW` → 否则 `off(permission)`。**还有第三种半开状态**：窗口起来了但**字幕帧线程**没起来（`overlay::start` 里那个 `started == false`）→ `off(not_wired)`（只有窗、没人往里推帧，画面永远是空的，"能画"是假的）。新符号：`platform::{CAPTIONS, record_captions, captions_status, OverlayFailure}` `【第四轮：已接线，核法见本节末】` |
| `global_hotkey` | `platform/*::start_hotkeys` 的 `Ok`/`Err`（现有 `app/src-tauri/src/lib.rs:218-223` 那条 `Notice` 旁边） | 返回 `Ok(host)` → `true`；`Err` → `off(permission)`（Linux 少了 `input` 组） |
| `tray` | **`tray.rs::can_hide_to_tray()`**（= 图标装上 × 有宿主；`app/src-tauri/src/tray.rs:155-157`） | 它返回 `true` 才报 `true`。**这条比原稿严**：原稿写"`platform/*::tray_host_available`"，而 Windows 上那个函数恒 `true`（`platform/win.rs::tray_host_available()`）——图标没装上时位会说谎 |
| `background_service` | **`events::sync_autostart(app, desired) -> CapabilityStatus`** 的返回值：两个调用点（`events::wire` 的初始同步 + `SettingsChanged` 分支）各调一次 `platform::record_background_service(..)`（与 `record_hotkeys` 同款观测槽——`sync_autostart` 跑在 `assemble()` **第 9 步**，**早于**第 13 步注入事实，不记就没人读得到）；无屏档 = systemd unit 在不在 | 快路径命中、或 `app.autolaunch().is_enabled()` 对齐成功 → ON；`is_enabled()` 报错 → `off(unsupported)`；`enable()` / `disable()` 写失败（企业组策略锁了启动项，现在只 `tracing::warn!`）→ `off(permission)`。**用户把自启开关关掉不让这一位翻假**（R5：位是能不能、开关是要不要）——只有"查不到 / 写不进"才报假。新符号：`platform::{BACKGROUND_SERVICE, record_background_service}` `【第四轮：已接线，核法见本节末】` |
| `vr_captions` | `lib.rs` 里 `cfg(feature = "steamvr-overlay")` 的**同一处**，**加上** `vr_overlay` 自己报的状态 `vr_overlay::status()`（内部是 `RUNNING` / `CONNECTED` 两个 `AtomicBool`，`CONNECTED` **只有 `Backend::connect()` 成功才置位**）。注意 `vr_overlay::start` **不是**打开的那一步——它只起线程 | feature 关着 → `off(not_built)`（现有那一半不变）；feature 开着时再分四种：装配期线程没起来 → `off(not_wired)`；`CONNECTED` → `true`；否则看 `vr_overlay::hmd_ready()`（= `openvr::is_runtime_installed() && is_hmd_present()`，`run()` 里等的是同一对探针）——假 → `off(unsupported)`（没装 SteamVR / 头显不在），探针过了但还没连上 → `off(busy)`。装配层读法：`platform::vr_captions_status()`。`run()` 是**后台重试**的 → 这一位会随重试翻真，按 §2.5.0 第 3 步重注入事实。新符号：`vr_overlay::{status, hmd_ready}` + `RUNNING`/`CONNECTED` `【第四轮：已接线，核法见本节末】` |
| `net_in` / `net_out` | （没有定义者 ⇒ 不许报 `true`，§2.6 R8） | 实现落地前恒 `false(unsupported)` |
| `file_config` | 无屏外壳的配置文件 / 状态出口（S3） | 改配置能真的生效（不是"只读"） |
| provider 位（已有 4 个） | `catalog/*.json` 的 `capabilities`（编译期烘焙） | 唯一的静态来源 |
| provider 位（待补 4 个） | （没有值 ⇒ 恒 `false`，§2.5.1） | — |

这一条直接来自 §1.4 的教训：**位不是"平台有没有这个 API"，而是"这次启动有没有把它打开"**。
配套纪律：**没有定义者的位不许进表**（§5.3 第 2 行的兜底）。

**第四轮（F1）补的口径——这条纪律在"上限能回答的那几位"上曾经没有真的满足：**

1. **`captions` / `background_service` / `vr_captions` 三位此前在产品路径上恒报开、没有任何定义者**（接线前复核时点：全 `app/src-tauri/src` 只有 5 处写 `off` —— `program_tap` / `virtual_mic` / `vr_captions` 的 `cfg` 那一半 / `global_hotkey` / `tray`），与本表第一句话直接冲突。**已按上面三行逐条接上定义者**（owner shell-dev，第四轮接线后 Windows 7 处 / Linux 6 处）：报 `true` 的凭据必须是一段**真的打开了它**的代码（`record_captions` × `overlay_running()` / `sync_autostart` 的返回值 / `vr_overlay::status()`），**不是**"这一档理论上做得到"。
2. **`mic` 是有意偏离，不是漏了**（owner shell-dev 已确认：**不**接 `busy`）：它唯一可能的定义者在**会话期**（起流），装配期拿不到否定证据，所以选择"按档位上限报开"，**代价写在那一行里**。这是本表**唯一**一处"位为真而没有装配期定义者"的位——**新增位不许照抄这一条**。
3. **这三位（连同 `program_tap` / `virtual_mic` / `tray` / `global_hotkey` 这些已有定义者的位）都搭 `devices.rs` 的 4 秒复核**（`refresh_host_facts`）重算，所以"窗口被关 / SteamVR 起来 / 自启动注册被组策略拒 / 虚拟麦节点掉了"这些变化**最迟 4 秒**反映到位与快照——位是**运行期事实**，不是启动时的快照（§2.5.0 第 3 步 / §2.6 R7）。**`mic` 是例外**：它没有定义者，复核也变不出事实来。

**上面三条"已接线"怎么核（第四轮复核时点，逐条命中）：**

```bash
grep -rn "record_captions(" app/src-tauri/src              # lib.rs 第 6 步：platform::record_captions(overlay::start(&state))
grep -rn "record_background_service(" app/src-tauri/src    # events.rs 两个调用点（初始同步 + SettingsChanged）
grep -n  "pub fn status()\|pub fn hmd_ready()" app/src-tauri/src/vr_overlay.rs   # RUNNING × CONNECTED
grep -n  "captions_status()\|background_service_status()\|vr_captions_status()" app/src-tauri/src/platform/win.rs app/src-tauri/src/platform/linux/mod.rs
# 期望：两个 host_facts() 都读观测槽（不自己重算）；`mic` 不在任何一行 off.insert 里。
```

**这一节的"已接线"指的是"定义者真的写进 `HostFacts.off`"**，不是"跑过一次真机"：`captions` 的 Linux/Windows 真机窗口、`vr_captions` 的真头显、`background_service` 的组策略拒写，都还没在真机上验过（§5.1-11 / §5.1-12 同款口径）。

**判据是纯函数，各有一条钉子单测**（owner shell-dev；第四轮接线、**第九轮**把判据从函数体里抽出来并补钉子）。
第四轮稿（本节上一版）在这个位置写的是"**这三条各有一个单测钉住**"，读起来像"接线时就顺手钉了"——**不是**：第九轮之前判定散在
`overlay::start` / `events::sync_autostart` 的函数体里，把定义者改成恒 `ON` 单测**全绿**（第九轮阻断项 D3），
所以才有下面这四行：

| 判据（**纯函数**，不碰 IO、不碰线程） | 落在哪 | 钉子单测（`grep` 于 2026-09-22 核） |
| --- | --- | --- |
| `overlay::captions_outcome(window, frame_thread_up)` | `app/src-tauri/src/overlay.rs` | `overlay::tests::captions_definer_needs_both_the_window_and_the_frame_thread`（`overlay.rs:228`） |
| `events::autostart_status(is_enabled, write)` | `app/src-tauri/src/events.rs` | `events::tests::autostart_status_maps_the_read_and_the_write_to_the_bit`（`events.rs:421`） |
| `platform::captions_status_for(started, running)` | `app/src-tauri/src/platform/mod.rs` | `platform::tests::captions_status_for_maps_the_two_evidence_to_the_bit`（`mod.rs:353`） |
| `vr_captions_status_for(built, thread_up, connected, hmd_ready)` | `app/src-tauri/src/platform/mod.rs` | `platform::tests::vr_captions_status_for_maps_the_three_probes_to_the_bit`（`mod.rs:425`） |

同一模块里的**全链**用例（判据 → 观测槽 → `host_facts()`）：
`platform::tests::captions_bit_follows_the_overlay_start_result`（`mod.rs:307`）、
`platform::tests::background_service_bit_follows_the_autostart_result`（`mod.rs:466`）、
`platform::tests::host_facts_decide_the_bits_and_the_snapshot`（`mod.rs:261`）、
`platform::tests::mic_is_reported_on_the_ceiling_until_there_is_a_definer`（`mod.rs:505`，钉 `mic` 那条有意偏离）；
Windows 专有 `platform::win::tests::vr_captions_bit_follows_the_build_and_the_openvr_probe`（`platform/win.rs:251`）；
真机快照 `platform::linux::tests::the_four_star_bits_on_this_machine`（`#[ignore]`，`platform/linux/mod.rs:318`，看这台机器四位实际报什么）。
变异自证（**第九轮复核独立重跑**）：三处定义者各改成恒 `ON`，对应用例变红——`captions_outcome` 红 2 条 /
`autostart_status` 红 2 条 / `vr_captions_status_for` 红 1 条；`platform::captions_status_for` 那条的自证写在用例自己的文档注释里（`mod.rs:350`）。

**一条钉子抓不到的东西（第九轮复核实测，别把它当钉子）**：`platform::tests::captions_bit_is_the_same_answer_on_the_poll_thread`（`mod.rs:394`）
**抓不到**"探针读 `thread_local`"那次回归——测试进程里没有真窗，主线程与轮询线程都读到 `false`，把探针改回线程相关它**仍然全绿**。
线程无关性真正的钉子在下一段（`running_is_a_process_wide_fact` + 真机探针）。

**教训：存活探针必须是进程级事实**（第九轮阻断项 D1/D2，owner shell-dev；**同一个根因在代码注释里标的是"第七轮 D1 / D2"**——
`crates/vox-overlay-linux/src/window.rs:345` 与 `app/src-tauri/src/platform/mod.rs:129`，两处说的是同一件事：它在第七轮就存在，第九轮被重新报成阻断项）。
`crates/vox-overlay-linux` 的存活凭据原来是**建窗线程**的 `thread_local! INNER`（GTK 对象只有建窗线程能碰，那是它存在的理由），
而读它的两条线程——`app/src-tauri/src/overlay.rs` 的字幕帧线程与 `devices.rs` 的 4 秒轮询线程——**都不是建窗线程**：
帧线程第一句就 `break` → 字幕永不渲染（渲染次数恒 0）；轮询线程读到另一个答案 → `captions` 位启动约 4 秒后翻成
`off(busy)`，reason 还写着"设备被别的程序占着"，与真实原因完全不搭。现在改成**进程级** `static ALIVE: AtomicBool`
（`crates/vox-overlay-linux/src/window.rs:51`），`Overlay::is_running()` 读它（`window.rs:182`）——**任何线程读到的都是同一个答案**；
对端 Windows 是 `vox-overlay-win` 的 `Shared::alive`，同一个形状。这条由
`vox_overlay_linux::window::tests::running_is_a_process_wide_fact`（`window.rs:350`；把 `alive()` 换回 `INNER` → 红）从源头钉住。
**"位 = 事实"要成立，探针本身得先是事实**——这是本稿 R6 在实现侧的前置条件。

**真机凭据 = `crates/vox-overlay-linux/examples/frame_loop_probe.rs`**（第九轮跑过；这是本稿 §2.5.4 唯一一处"非建窗线程真的把字画上屏"的实测）：

```bash
cargo run -p vox-overlay-linux --example frame_loop_probe -- 5
# 前置：能连上显示（DISPLAY，X11 / XWayland）、xwininfo、ImageMagick 的 import（截图用）
```

它复刻产品那条循环的形状（帧线程每 33 ms 读存活探针 → 塞邮箱 → 主线程 GTK 画），报三件事：5 秒 **152 次渲染**
（约 30 帧/秒 ⇒ 帧循环没有秒退）、建窗线程 / 帧线程 / 第三条线程读到的 `is_running()` **一致**、
以及两张截图的像素差——`/tmp/vox-frame-loop-on.png` 里 `#FF0000` **4760** 像素 / `#00FF00` **6144** 像素
（听人说话 / 对外说话两行真的画到了屏幕上），`hide()` 之后那张 **0 / 0**（`convert … txt:- | grep -c '#FF0000'`）。
**这条 example 就是"位说 ON 必须有人在画"的真机凭据**：判据再纯、单测再绿，也不代替"字真的上屏"。
（数字来自第九轮真机跑、Main 汇总；本轮只回填，不重跑。）

### 2.6 界面降级规则

| # | 规则 | 现状先例（要么照抄，要么是现状的缺口） |
| --- | --- | --- |
| R1 | **不静默**：位为假就**不渲染**该区块，并**必须**给一句说明（区块内联 hint 或一条 `Notice`）。禁止"灰掉之后什么都不说" | `CableManager.tsx:83-95`（整块换成 hint）；`.omp/AGENTS.md` 明写"界面按能力位降级，不许灰掉之后什么都不说" |
| R2 | 文案要说清**三件事**：现在为什么不行 + 要用户做什么 + 做完之后去哪 | `zh.ts:146-147`（Linux 虚拟麦）；`linux/mod.rs:78-79`（热键错误里带 `usermod -aG input`） |
| R3 | 文案**不进芯**：芯只给 `(位, reason)`，句子在 `app/ui/src/i18n/*.ts` 里按 `(位, reason)` 查 | 现状芯里已经有中文串（`crates/vox-core/src/pipeline/listen.rs` 的 `PortError::new("还没选择监听程序。")`——**写稿当时在 `:21`，core-dev 落地 S0 后已漂到 `:34`**，所以这里给符号不给行号），这条是**新增纪律**：能力位的文案不走那条路 |
| R4 | `(位, reason)` 组合必须有 i18n 条目，缺了算 bug（脚本可查） | **第六轮已落地**：`check:cable` 这种脚本已存在（`package.json`），`check:capabilities`（`app/ui/scripts/check-capabilities.mjs`）已挂进 `npm run verify`，`en` 缺键回落到 `zh` |
| R5 | 位是**能不能**，用户开关是**要不要**——两者不许混。位为假时不渲染开关，位为真时开关照旧 | **第六轮已修**（原缺口，留档）：`Vrchat.tsx` 的 SteamVR 开关曾经**无条件渲染**，而 Rust 侧整块被 `#[cfg(all(windows, feature = "steamvr-overlay"))]` 门控（`lib.rs:43-44`）→ 没有该 feature 的构建上，用户会点到一个不存在的功能；现在 `Vrchat.tsx` 先读 `hostBit(snapshot, "vr_captions")` |
| R6 | 位为真必须由"打开这条路的那段代码"负责（§2.5.4） | §1.4 |
| R7 | 位变了的刷新走既有通道：`devices.rs` 的 4 秒轮询 → `Runtime::set_devices` → `devices_changed` 事件；能力位同理接在同一个 tick 上 | `app/src-tauri/src/devices.rs:38-40,77-84`；`crates/vox-core/src/runtime.rs:1049-1058` 的 `set_devices` 去重 |
| R8 | 未落地的位目标（`net_in` / `net_out`）恒定假，界面不渲染对应区块 | `virtual_cable_status: not_applicable` 是同一个套路 |
| R9 | 位为假的那些**架构内功能**（Android 的虚拟麦、Android 的抓通话、无屏的字幕），文案必须说"**这台设备做不到**"，**不许**写成"没有这个功能"，也不许静默撤掉入口 | §10.5-1（"不是不做"）。现状反例正是 §1.4：文案让用户去选一个不存在的设备 |

### 2.7 清单 → Plan：怎么保证"不改行为"

```
Settings ──Runtime::session_config()──► SessionConfig ─┐
                                                       ├─► Composition::of(&config, &facts) ──► Composition
platform::host_facts() ──► Runtime::set_host_facts() ──┘        （§2.5.0 第 1–3 步）                │
                                                        Plan::from(&Composition) ◄──────────────────┘
                                                                  │
                                                                  ▼
                                                           Plan ──► Worker（一行不动）
```

- `SessionConfig`（`runtime.rs:38-64`）保持原样：它是"设置 + 会话号 + 密钥"的载体，清单从它派生。
- `Plan`（`pipeline/mod.rs:85-101`）保持原样：它是 Worker 认的作业单，**字段一个不加、一个不减**。
- 变更只在 `Plan::build` 内部（写稿当时 `mod.rs:106-111`；现 `:114`）：先 `Composition::of(config, &facts)`，再 `Plan::from(&composition)`。
  `facts` 只影响两处：① 位为假的条目**不进清单**（§2.5.3 上下文①）；② Speak 走虚拟麦时 `device` 的缺省解析
  （Linux 接线后是 `vox_virtual_mic`；见 §3.2 的第 ② 件事）。**除这两处外清单只由 `SessionConfig` 决定**。
- **Worker 主体（`mod.rs:657-1499`）一行都不动** → 现有测试（`pipeline/mod.rs` 的 `mod tests`、`pipeline/speak.rs` 与 `pipeline/listen.rs` 的 `#[cfg(test)] mod tests`；写稿当时 `mod.rs:2157-2298`、`speak.rs:50-117`、`listen.rs:46-…`）**必须一个字不改地继续通过**。这就是"清单能表达现状"的硬证据，而不是"看着对"。

**清单只吃 `SessionConfig`（`runtime.rs:38-64`），所以有一类设置项天然进不来。这条边界要写死**，否则清单会变成"设置的第二份拷贝"：

| 进清单的设置 | 落在哪一格 | **不进清单**的设置 | 为什么 |
| --- | --- | --- | --- |
| `input_device` / `output_device` / `monitor_translation` | `in[]` / `out[]` | `show_translation` | 它是**视图开关**，执行点在账本（`runtime.rs:958-975`：早退，`SubtitleDelta` 根本不发），不在 Worker。`captions` 出口在"有 session"时**恒存在**（`mod.rs:1293-1300` 无条件 `push_text`） |
| `translate` | `session` 有没有 | `subtitle.*`（`visible` / 字体 / 几何 / `vr_overlay_enabled`） | 悬浮窗与头显的**外观**，属"界面"，不是"装配" |
| `denoise` / `activation_mode` / `gate_threshold` | `ops`（`GateConfig` 原样带过去） | `autostart` / `start_minimized` / `ui_language` | 生命周期与界面语言，将来归 `life` / `ui` 两格的整体取值，不落条目 |
| `provider` / `model_name` / 目标语言 / 音色 / `voice_clone_frequency` / `source_language` | `session`（`params` 原样带过去） | `voice_by_language` | 它是"按语言记住上次音色"的界面记忆，不是装配 |

（反过来说：如果哪天要让无屏档**根本不产出**字幕文本，那是 `out[captions]` 的有无，而不是给清单加一个开关。）

**一处"只落一半"要说明**：`activation_mode` 在清单里**只落一半**——`ops[gate].config.kind`（`Hold ⇒ manual`、`Toggle ⇒ level`，`crates/vox-core/src/runtime.rs:599-603`）。两个预设的常量照抄 `crates/vox-core/src/gate.rs:34-51`（**别自己推导**）：

| 预设 | kind | threshold | tail_ms | preroll_ms |
| --- | --- | --- | --- | --- |
| `GateConfig::MANUAL`（`Hold`） | `manual` | 0.012 | 150 | 100 |
| `GateConfig::level(t)`（`Toggle` / Listen） | `level` | `t.max(0.0)`；Listen 恒 0.0 | 600 | 200 |

另一半是**控制通道语义**：`Toggle` 是"按一下切换开麦"、`Hold` 是"按下为真、松开为假"（`runtime.rs:826-835`），加上切换激活方式时重置开麦状态（`runtime.rs:493-496`）；清单里没有对应格（`control` 只列通道，不列键位）。所以 `kind ⇒ activation_mode` 的反写是**充分且一对一**的，但 forward 方向不许宣称"清单完整描述了 `activation_mode`"。

---

## 3. 改动清单

> 一个文件一个 owner。跨 owner 的改动找 Main 排期。

### 3.1 core-dev

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| `crates/vox-core/src/composition.rs` | **新增** | §2.1 的全部类型（含 `HostKind` 四档 + `CompositionError::HostMismatch`）+ `validate` / `required_capabilities` / `missing_on(&CapabilityReport) -> Vec<CompositionError>`（**另一个函数**：`missing_on` 吃"算好的报告"，与 `of` 的实参不同——别抄混） / `endpoint` / `of(&SessionConfig, &HostFacts)`（**唯一签名**：事实不是报告；S1 的投影从 `Runtime::host_facts()` 取同一份事实，不另立第二份） + 内联 `#[cfg(test)] mod tests`（§4.2）。**`schemars` 已拍：锁 `0.8.22`**——类型上加 `#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]`，`vox-core/Cargo.toml` 加**可选**依赖 + feature `json-schema`（默认关、`vox-mcp` 打开）；内部标签枚举在 0.8 里可能要补 `#[schemars(...)]` 注解（代价见 `docs/plans/S1-AGENT-FACE.md` §2.6） |
| `crates/vox-core/src/capability.rs` | **新增** | §2.5 的全部类型 + `host_ceiling(HostKind)` 的**四档上限表** + `effective` + `CapabilityReport { tier, host, speak, listen }` + `UnavailableReason`（含新增的 `NotWired`） |
| `crates/vox-core/src/lib.rs:8-19,21-37` | 修改 | 加 `pub mod capability; pub mod composition;` 与导出 |
| `crates/vox-core/src/pipeline/mod.rs::Plan::build`（写稿当时 `:106-111`；现 `:114`） | 修改 | `Plan::build` 改成"经清单中转"；**`Plan` 结构体与 Worker 不动** |
| `crates/vox-core/src/pipeline/speak.rs::composition`（写稿当时是 `fn plan`，`:15-48`；现 `:26-130`） | 修改 | `fn plan(config)` → `fn composition(config, facts) -> Composition`（Speak 的清单构造）。**唯一一处吃到 `facts` 的逻辑**：`role: VirtualMic` 那条的 `device` 缺省解析（§3.2 第 ② 件事） |
| `crates/vox-core/src/pipeline/listen.rs::composition`（写稿当时是 `fn plan`，`:16-44`；现 `:30-109`） | 修改 | 同上（Listen 的清单构造，保留"没选程序就报错"）。Listen **不**吃虚拟麦缺省（它的角色恒 `speaker`） |
| `crates/vox-core/src/catalog.rs::ProviderCapabilities`（写稿当时 `:18-23`；现 `:21`） | 修改 | `ProviderCapabilities`（4 布尔结构体）→ 由 `CapabilitySet` 承载。**只有这 4 位有值**：`catalog.rs::provider_capabilities` 只映射 JSON 里那 4 个布尔，待补的 4 位一律 `false`（§2.5.1） |
| `crates/vox-core/src/catalog.rs` 的 4 个 `supports_*` 自由函数（写稿当时 `:59-73`） | **删除** | 4 个 `supports_*` 自由函数 → `catalog.rs::supports(provider, bit)` |
| `crates/vox-core/src/catalog.rs::supports_audio_output`（写稿当时 `:98-100`；现 `:111`） | 不动 | `supports_audio_output(language)` 是"语言的位"，不是 provider 位 |
| `crates/vox-core/build.rs:55-61` | **不动** | 已拍：provider 的 4 个待补位**只进代码枚举**（§2.5.1）→ `Capabilities` 结构体**不加字段**，`CapabilitySet` 的构造仍只吃这 4 个布尔 |
| `catalog/{aliyun,gemini,gpt}.json`（`:40-45` / `:26-32` / `:30-36`） | **不动** | 同上：三份 JSON **不加**那 4 个字段，也不 bump `schema_version`。前端 `?? false`（`app/ui/src/catalog.ts:96-101`）与后端"三份必填"的约束因此都不用动 |
| `crates/vox-core/src/cloud/protocol.rs::SessionParams`（写稿当时 `:72-89`，第三轮核到 `:79`） | 修改 | `SessionParams` 加 `Serialize, Deserialize`（纯 derive，不改字段与行为）。**兜底**：`cloud/gpt.rs:190-196`、`cloud/protocol.rs:586-593`、`cloud/mod.rs:560-564,603-606,697-700` 五处报文断言必须原样通过——它们就是"三家报文逐字节没变"的检查 |
| `crates/vox-core/src/cloud/gpt.rs:39,53,59`、`cloud/mod.rs:283`、`cloud/protocol.rs:99` | 修改 | 5 个消费点改用 `supports(provider, bit)` / `supports_audio_output(language)` |
| `crates/vox-core/src/runtime.rs:132-145` | 修改 | `Snapshot` 加 `capabilities: CapabilityReport`（含 `tier`）——这是 §2.5.0 第 5 步的唯一出口 |
| `crates/vox-core/src/runtime.rs:270-284` | 修改 | 同组加 `set_host_facts(facts)` / `capabilities()`；`SessionConfig` 与 `session_config`（`:38-67` / `:700-768`）**不动** |
| `crates/vox-core/src/ports.rs::DeviceRegistry` | 修改（**口径改动，方法不删**） | **`virtual_cable_installed()` 保留，降级为"仅 Windows 安装管理用"**（Main **D2**，2026-09-22）：VB-CABLE 的下载 / 安装 UI 还要它。trait 文档注释里要写明两句话：① 这个方法**只描述安装器状态**（装了没 / 装到一半待重启）；② **"能不能用"的唯一真相是能力位 `virtual_mic`**——任何"译音能不能灌进虚拟麦"的判断都不许读它。**推翻**原稿的"从 trait 上删掉——平台事实不该挂在设备枚举上"（删了安装态就没有来源；"能不能用"交给位之后，两个来源不再打架）。**本条指的是** `crates/vox-core/src/ports.rs::DeviceRegistry::virtual_cable_installed`（**符号优先**：行号只作参考，现场用 `grep -n virtual_cable_installed crates/vox-core/src/ports.rs` 核）。**行号自伤记录**：写稿当时 `:170`，第四轮核到 `:178`——把它推走 8 行的**正是 D2 自己要求补的那段 trait 文档注释**（`git diff crates/vox-core/src/ports.rs` 可复现）。同批的 `DeviceRegistry` trait 头仍在 `:164`（未动） |
| `crates/vox-core/src/settings.rs::DEFAULT_FONT_FAMILY`（写稿当时 `:255-258`，第三轮核到 `:257-260`） | 不动（**本稿不要求**） | 那处 `cfg(windows)` 的唯一归属是"宿主事实"，但 `Settings::normalize()` 在注入事实之前跑（`runtime.rs` 构造早于 `assemble` 的第 5 步）→ 顺序有风险。留给后续，**位表里不占名** |

### 3.2 shell-dev

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| `app/src-tauri/src/platform/mod.rs:32-43` | **删除** | `VirtualDeviceStatus` → `CapabilityStatus` |
| `app/src-tauri/src/platform/mod.rs` | 修改 | 新增三件**两边同名**的函数（沿用现有做法）：① `pub fn host_kind() -> HostKind`（§2.5.0 第 1 步）；② `pub fn host_facts() -> HostFacts`（第 2 步）；③ 虚拟麦三件 `virtual_mic_ensure() -> CapabilityStatus` / `virtual_mic_device() -> Option<&'static str>` / `virtual_mic_shutdown()`（§3.2 末尾） |
| `app/src-tauri/src/platform/win.rs`（`virtual_device_status` / `virtual_mic_ensure` / `host_facts` / `startup_notes` / `tray_host_available`；写稿当时 `:93-114,120-122`，第三轮核到时 `virtual_device_status` 已漂到 `:167`、`tray_host_available` 在 `:194`） | 修改 | `host_kind() = Windows`；`virtual_device_status()` → 往 `off` 表里填；`virtual_mic_ensure()` 读 `vox_audio_win::cable::detect()`（**不建节点**）；`startup_notes()` 改成位驱动 |
| `app/src-tauri/src/platform/linux/mod.rs:123-130,132-145,154-183` | 修改 | `host_kind() = LinuxDesktop`；同上（原来的 `not_applicable` 变成"位开着、无 reason"，**接线后**才成立——接线前是 `off(not_wired)`，见 §3.2 末尾） |
| `app/src-tauri/src/lib.rs:149-241` | 修改 | `assemble` 里注入 `host_facts()`；按 §2.5.4 在**同一处** `cfg` 旁边把 `steamvr-overlay` 翻成 `off(NotBuilt)`；虚拟麦接线调用与 `shutdown()` 里的 `destroy`（§3.2 末尾） |
| `app/src-tauri/src/lib.rs:218-223`, `:232-238` | 修改 | 热键失败、启动提醒从"命令式 Notice"改成"位 + Notice"（Notice 保留，位是新增的机器可读面） |
| `app/src-tauri/src/dto.rs:119-125,174-187` | 修改 | `devices` 里三个虚拟麦字段：`virtual_cable_installed` **保留但降级成"安装器状态"**（与 §3.1 保留的 trait 方法同一口径：它只说明 VB-CABLE 装没装，**不是**"能不能用"的判据；前端本来就不读，删掉也不影响位）；`virtual_cable_status` 降级成 `virtual_mic_detail`（只在有管理动作的平台上非空），并**新增 `capabilities` 字段** |
| `app/src-tauri/src/devices.rs:77-84` | 修改 | `virtual_cable_installed` 继续从 registry 取（**安装器状态**，§3.1 的 D2 口径）——但**能力位不许从它推**：`virtual_mic` 只认 §2.5.4 的定义者（持有句柄的那段代码 / `cable::detect()`） |
| `app/src-tauri/src/tray.rs:144-157` | 修改 | `can_hide_to_tray()` 改成读 `tray` 位（行为不变，来源换成正账本） |
| `app/src-tauri/src/events.rs:283` | 修改 | `vr_overlay_enabled` 的设置比较保留（设置项还在），但渲染前必须过 `vr_captions` 位 |
| `app/ui/src/types.snapshot.ts:48-60` | 修改 | 加 `Capabilities` 类型；`virtual_cable_status` 五态删除 |
| `app/ui/src/catalog.ts:52-55,96-101,258-269` | 修改 | 位名与芯对齐（同一个位在两侧同名，`app/ui/src/mock/merge.ts:64-65` 跟着改） |
| `app/ui/src/sections/CableManager.tsx:83-95` | 修改 | 按 `virtual_mic` 位 + reason 渲染：`not_installed` → Windows 的安装引导（现状不变）；`not_wired` → **"这个平台还没接上"**（§3.2 末尾的中间态）；位为真 → 设备名引导（现状文案保留） |
| `app/ui/src/sections/Vrchat.tsx:123-131` | 修改 | `vr_captions` 位为假时不渲染开关，改渲染 `NotBuilt` / `Unsupported` 文案（**修掉 §2.6 R5 的现状缺口**） |
| `app/ui/src/i18n/{zh,en}.ts` | 修改 | 每个 `(位, reason)` 一条文案（§2.6 R4）。**只扩 zh + en**：三语已按 `DIRECTIONS.md` §10.5「功能处置表」收敛为两语，`ja.ts` 不再扩（新增条目时不必补） |
| `app/ui/src/mock/backend.ts:299-309` | 修改 | `?platform=linux` 扩成 `?host=windows\|linux\|android\|embedded`（**四档**，含 `not_wired` 那个中间态），让每种降级都能在浏览器里看 |
| `app/ui/scripts/check-capabilities.mjs` | **新增** | Playwright 断言（§4.3） |
| `app/ui/package.json:16-17` | 修改 | `check:capabilities` 挂进 `verify` 链 |
| `crates/vox-audio-win/src/osver.rs`（探针，**已存在**） | **接线，不新增** | `process_loopback_available()`（`osver.rs:38`）与门限常量 `MIN_PROCESS_LOOPBACK_BUILD`（`osver.rs:14`）**都已经有了**，`crates/vox-audio-win/src/lib.rs:39` 已重导出，`capture/mod.rs:331,369` 已在用。S0 要做的只是把它的结果写进 `HostFacts.off[program_tap]` —— **不许再造一个同名/同义的探针**（原稿写"没有探针"是错的，已按 verifier 复核改正） |

> **第六轮状态**：`app/ui/**` 的能力位消费面**已经落地**——`app/ui/src/capabilities.ts` + `components/Capability.tsx`，各 section 按位降级，`i18n/{zh,en}.ts` 的位与 reason 文案，`check-capabilities.mjs` 挂进 `npm run verify`，`mock/backend.ts` 的四档 `?host=`；`Snapshot.capabilities` 字段（`dto.rs`）也已在。
>
> **本表有两行写了、实现里没做（别照本表去找）**：① `platform/mod.rs` 的 `VirtualDeviceStatus` **没删**（`virtual_device_status()` 两个平台照旧在，`dto.rs` 仍读它）；② `dto.rs` / `types.snapshot.ts` / `CableManager.tsx` 里的 `virtual_cable_status` **没改名**成 `virtual_mic_detail`。实际做法是**语义降级**：这个字段只决定"这一页摆哪些管理动作"，"能不能用"一律读 `hostBit(snapshot, "virtual_mic")`（`CableManager.tsx` 的模块头注释与 §3.2 末段同一口径）。**第六轮时点仍未落地**：`--print-composition`（§4.3-A / §4.4 的命令当时跑不了；**第十五轮已两档落地**，见 §4.3-A 的状态注与顶部状态行——本句保留第六轮当时的记录）。
>
> **第四轮状态**：§2.5.4 新增的 `captions` / `background_service` / `vr_captions` 三个定义者**也已接线**（核法见 §2.5.4 末的命令）。本表**没有**为它们新增文件行：落点全在**已有文件**里（`overlay.rs` 的 `start` 返回值 / `events.rs::sync_autostart` 的返回值 + 两个调用点 / `vr_overlay::{status,hmd_ready}` / `platform/mod.rs` 的观测槽 / 两个 `host_facts()` 里读槽），owner 仍是一个（shell-dev），不另派工。

#### 关于 §1.4 的 Linux 虚拟麦：**已拍 = 接线**（不是二选一）

`DIRECTIONS.md` §10.5 功能处置表把"虚拟麦（Win VB-CABLE / Linux PipeWire sink）**含 Linux 接线**"列进"实现排期靠后（能力位先报真实值）"，
Main 又单独拍了第 ① 项："Linux 虚拟麦 → **接线**，接线前位报 false、文案撤下"。所以原稿的 (a)/(b) 二选一**作废**，落成下面两段。

**① 接线本体（owner: shell-dev）**

| 要改的 | 做什么 |
| --- | --- |
| `platform/linux/mod.rs`（或同目录新开 `virtual_mic.rs`，同 owner） | 进程级持有一个 `OnceLock<VirtualSink>`：`virtual_mic_ensure()` 先 `VirtualSink::exists()`（上次没退干净的残留），需要时 `VirtualSink::create()`；成功 → `CapabilityStatus::ON`，失败 → `off(unsupported)`；`virtual_mic_device()` 返回 `VirtualSink::node_name()`（= `vox_virtual_mic`，`crates/vox-audio-linux/src/virtual_sink.rs:18,81-83`）；`virtual_mic_shutdown()` 调 `destroy()`（`virtual_sink.rs:74-78`） |
| `app/src-tauri/src/lib.rs` 的 `assemble`（第 5 步附近） | 调 `virtual_mic_ensure()`，结果进 `HostFacts.off`；**位为 ON 的唯一凭据就是"这个句柄真的建出来了"**（§2.5.4 的 R6） |
| `app/src-tauri/src/lib.rs` 的 `shutdown` | 排在 `engine.shutdown()` **之后**调 `virtual_mic_shutdown()`：播放流还挂着节点时先删节点会留下悬挂的 stream |
| `platform/win.rs` | `virtual_mic_ensure()` 只读 `cable::detect()`，**不建任何节点**（Windows 的设备由 VB-CABLE 驱动提供） |

**② 让"译音真的进得去"（owner: **shell-dev**，已拍）**

> 拍板依据（Main，2026-09-22）：这一件改的是**装配 / 设备选择**，不是芯 → owner = shell-dev。
> `(i)` / `(ii)` **不预先选**：落地时按 §4.4 的真机检查实测后二选一；**落地前不改文案**（在实测出结论之前，界面文案不动）。
> 若最后选 `(i)`，那处解析代码落在 core 的 `speak.rs` / `Composition::of` 里——**由 shell-dev 提改动、core-dev 按"一文件一 owner"排期落地**，不是 shell-dev 直接改 core。

节点建出来只解决"系统里有这个设备"；要"译音真的进得去"，Speak 的 `out[playback{role: virtual_mic}]` 那格的 `device` 必须指向它。现状链路是：
`settings.speak.output_device`（Linux 默认 `None`，`crates/vox-core/src/settings.rs:135`）→ `Plan.playback_device` → `PlaybackSink::open(device, rate)` → `crates/vox-audio-linux/src/playback.rs:227-231` 把设备名塞进 `target.object`。
两个候选做法，**落地时必须实测选一个**（这是 §5.1-9 的未核实项）：

- **(i) 缺省解析**：`Composition::of(&config, &facts)` 在 `role == VirtualMic` 且 `config.output_device == None` 时，用 `facts.virtual_mic_device` 补上（core-dev 一处，§3.1 的 `speak.rs` 行；字段定义见 §2.5.2 的 `HostFacts`）。**后果**：Linux 的"对外说话"默认就往虚拟麦送（Windows 行为不变，因为 Windows 那份 `HostFacts.virtual_mic_device` 是 `None`，缺省仍是系统默认输出）——这正是产品要的，但要写进 §4.3 的验收。
- **(ii) 让用户选**：`LinuxDeviceRegistry::output_devices()`（`crates/vox-audio-linux/src/registry.rs:51-57`）本来就把 sink 列出来（`label_of` 取 `node.description` = "Vox Virtual Mic"，`:86-92`），用户在界面里选它即可。**风险**：`DeviceInfo` 只有 `name` / `is_default` 两个字段（`crates/vox-core/src/ports.rs:149-152`），存的可能是 description 而不是 `node.name`，而 `target.object` 认的是节点名 → 需要给 `DeviceInfo` 加一个稳定 id（core-dev 的 `ports.rs`）。

**中间态怎么表达（接线落地之前）**

> **第三轮时点**：接线**已落地**（本机实测 `virtual_mic: enabled=true`）。下面这段保留为**接线前**的口径与验收依据（它同时是"位=事实"的样板：中间态只许报 `false(not_wired)`，不许报 ON）。

- Linux 的 `virtual_mic` 位报 **`false(not_wired)`**（新加的 `UnavailableReason`，§2.5.2）——"实现存在、装配层没接上"，不是"平台做不到"。
- **"去目标程序里选 Vox Virtual Mic"那句引导必须撤下**（`app/ui/src/i18n/zh.ts:146-147` 那一类），换成 `not_wired` 的文案：**"这个平台还没接上"**。否则就是现状那种"位说 ON、功能不存在、文案还指着不存在的设备"（§1.4）。
- 验收把这一条钉住：位为假时界面上**不许**出现"去选 Vox Virtual Mic"的字样（§4.4）。
- **不许**出现第三种状态：位 ON 而设备不存在（那就是现状）。

### 3.3 agent-face-dev

| 文件 | 动作 | 改什么 |
| --- | --- | --- |
| （S0 内不产生文件改动） | — | 承接**三条约束**并写进 `docs/plans/S1-AGENT-FACE.md`：① `Composition` 的 serde 形态**就是** `compose_endpoint` 的 `inputSchema`，S1 不再另写一份 schema；`Composition::validate()` 就是工具的参数校验；字段名以本稿为准，不自立第二套。② 清单**外面**的控制格（`endpoint` / `apply` / `token`）合法，不进清单。③ **本轮的字段值变化**：`host` 现在是**档位**（`windows` / `linux_desktop` / `android` / `linux_headless`）；`CapabilityReport` 多一个 `tier` 键（`host` 仍是宿主机位表）；`missing_on` 现在返回 `Vec<CompositionError>`（含 `HostMismatch`），S1 的"非空即 `endpoint_unavailable`"逻辑不变。**这三条 S1 稿已自行对齐**（按**节**引，行号已漂：S1 **§1.4** 的上游结论表、**§2.1.3** 的 `device.tier` 与 `manifest.host` 与 `capabilities.tier`/`host` 与字段说明、**§2.1.3-③** 第 2 步的 `missing_on`）——**本稿不再催改**。**跨 owner 提醒**：`schemars` **已拍锁 `0.8.22`**（可选依赖 + feature `json-schema`，默认关、`vox-mcp` 打开），derive 要加在 `crates/vox-core` 的 `Composition` / `Input` / `Op` / `Output` / `GateConfig` / `SessionParams` / `Track` / `ModelProvider` 上——那些都是 **core-dev** 的文件，**找 Main 排期，不要自己动** |

### 3.4 明确不做（写下来防止被顺手做掉）

- 不动 `Pipeline` 枚举本身（`event.rs:15-22`）：加第三条腿是清单落地**之后**的事，且属于"内部逻辑架构重构"（`DIRECTIONS.md:33`）。
- 不动 `ports.rs` 的 `CaptureTarget` 加 `net_in` 变体：清单先占名，实现归 S3。
- 不删 `app/src-tauri/src/commands.rs` 的 26 条命令：控制面下沉是 S1。
- 不动算子里程碑（`vox-dsp`）。
- **不把 Linux 虚拟麦接线记成"不做"**：它已排在"虚拟麦"这一项里（§3.2 末尾），接线前只是**位如实报假 + 文案撤下**（§10.5 功能处置表 + Main 第 ① 项）。
- **不新增"宿主档位"之外的引擎抽象**：`HostKind` 只表达"哪一档宿主"；`wasm` / `browser` / 进程形态那条轴（`DIRECTIONS.md` §3.5 的 `host: native|wasm|browser|mcu`）在 S0–S3 没有取值，**不占位、不留半成品枚举**。

---

## 4. 验收标准

### 4.1 命令

```bash
cargo test -p vox-core              # 芯：清单 + 能力位 + 既有行为全绿
cd app/ui && npm run verify         # 前端：build + check:classes + check:preview + a11y + qa:narrow + check:cable + check:capabilities + check:agent + qa:home
cargo run -p vox -- --print-composition   # 人眼/脚本可观察的清单 + 有效能力位（见 §4.3-A）
```

（本稿是设计稿，上面三条**写稿当时我没有跑**——它们属于实现完成后的验收。§4.4 那条 Linux 真机检查同理；**第十五轮以后**第三条 `--print-composition` 已两档落地，无屏档那一档本轮真跑过，见 §4.3-A 的状态注。）

### 4.2 "清单能表达现状"的测试形状

**第一层：既有测试一个字不改地继续通过**（这就是"不改行为"的证据，比任何新断言都硬）：

- `crates/vox-core/src/pipeline/mod.rs:2157-2298`：`audio_flows_through_denoise_gate_resample_then_uploads`、`a_closed_manual_gate_uploads_nothing`、`the_initial_gate_state_takes_effect_inside_the_worker_thread`、`listen_upload_is_unconditional_and_skips_denoise`、`denoise_is_skipped_when_the_capture_rate_is_not_48k`、直通两条、队列丢弃、阀门序号、节流……
- `crates/vox-core/src/pipeline/speak.rs` 的 `#[cfg(test)] mod tests`（写稿当时 `:50-117`；现 `:146-214`）（6 条）
- `crates/vox-core/src/pipeline/listen.rs` 的 `#[cfg(test)] mod tests`（写稿当时 `:46-…`；现 `:125-170`）（3 条）

**第二层：新增正交测试**（放 `crates/vox-core/src/composition.rs` 的 `#[cfg(test)] mod tests`）：

1. `the_manifest_round_trips` —— `Composition::of(&cfg, &facts)` → JSON → 读回 → 相等。（清单是数据：能存、能传、能比。）
2. `plan_is_derived_from_the_manifest`（**对比断言**，核心）—— 枚举 `pipeline × translate × voice.has_voice` 的组合，逐个断言：

   | `Plan` 字段 | 清单里对应项 |
   | --- | --- |
   | `plan.target` | `composition.r#in[0]` |
   | `plan.denoise` | `composition.ops` 里有没有 `Denoise` |
   | `plan.passthrough` | `composition.session.is_none()` |
   | `plan.playback_device` | `composition` 里 `role=VirtualMic`（Speak）或 `role=Speaker`（Listen）那条的 `device` |
   | `plan.monitor_translation` | `ops`/`out` 里有没有 `role=Monitor` |
   | `plan.hot_update` | `composition.session.map(\|s\| s.hot_update)` |
   | `plan.params` | `composition.session.map(\|s\| s.params)` |

3. `speak_manifest_names_the_virtual_mic` —— `ops` 顺序 == `[mono, denoise, gate, resample]`；存在 `role: virtual_mic`。
4. `listen_manifest_has_no_denoise_and_an_always_open_gate` —— `ops` == `[mono, gate(level,0.0), resample]`；**没有** `role: virtual_mic`。
5. `passthrough_manifest_drops_the_session_and_the_captions` —— 关掉翻译后 `session == None`、无 `captions`、`denoise` 仍在、`resample` 消失。
6. `text_only_leg_has_no_playback` —— `voice = None` 时没有 `playback`（对应 `speak.rs::composition` 与 `listen.rs::composition` 的 `voice.map` 语义）。
7. `an_endpoint_is_the_minimal_instance` —— `Composition::endpoint(..)` 的 `ops.is_empty()`、`ui == None`、`session.is_none()`、`control` 不含 `Ipc`。
8. `op_order_must_be_a_subsequence_of_the_canonical_order` —— 把 `resample` 挪到 `mono` 前面 → `validate()` 返回 `BadOpOrder`。
9. `a_session_edge_without_a_session_is_rejected` / `a_session_without_a_consumer_is_rejected` / `a_manifest_without_an_input_is_rejected`（`in: []` 必须**在第一道闸 `validate()`** 就被拒 → `MissingInput`，不许拖到 `Plan::from`；**D4**，2026-09-22 已落地）。
10. 能力位：
    - `facts_can_only_turn_bits_off`（`off` 表里出现档位上限之外的位 → 校验失败）
    - `android_ceiling_has_no_virtual_mic` / `linux_ceiling_has_virtual_mic_without_an_install_step`
    - `missing_capabilities_are_reported_per_entry`（清单要 `process_loopback` + 机器没有 `program_tap` → 报 `MissingCapability { bit: ProgramTap, .. }`）
11. **宿主档位**（本轮新增）：
    - `four_tiers_each_have_a_ceiling_row` —— `HostKind::ALL` 四档都能查到上限，且**四档互不相同**（`windows` / `linux_desktop` / `android` / `linux_headless`；`android` 与 `linux_headless` 的上限里没有 `virtual_mic`）
    - `a_manifest_for_another_tier_is_rejected` —— 拿 `host: android` 的清单对 `tier: windows` 的报告调 `missing_on` → 恰好一条 `HostMismatch`
    - `the_report_carries_its_tier` —— `CapabilityReport.tier` 等于注入的 `HostFacts.host`（§2.5.0 第 5 步的唯一出口）
12. **中间态**（本轮新增，Linux 虚拟麦接线前）：
    - `not_wired_turns_the_bit_off_but_keeps_the_tier` —— `off[virtual_mic] = NotWired` 时有效位里没有 `virtual_mic`，而 `host_ceiling(linux_desktop)` 里**有**（说明是"没接上"而不是"平台做不到"）
    - `the_speak_manifest_drops_the_virtual_mic_entry_on_android` —— `Composition::of(&speak_cfg, &android_facts)` 的 `out` 里**没有** `role: VirtualMic`，且**有** `role: Speaker`（§2.3.3：门关掉的是那一格，不是这条腿）

### 4.3 可观察的行为

**A. `--print-composition`（第十五轮已两档落地；下面命令体一字未改）**

> **第十五轮状态**：两档都已落地 —— 无屏档 `crates/vox-headless/src/cli.rs` 的 `--print-composition`（`Mode::PrintComposition`）
> → `crates/vox-headless/src/status.rs::composition_json`；桌面档 `app/src-tauri/src/composition.rs`
> （`FLAG = "--print-composition"`，`requested()` / `print_and_exit()`；**排在 Tauri 之前、只 `Builder::build()` 不 `run()`**，
> 不建窗口 / 不注册命令 / 不起托盘热键）。两档**同一个派生**：`vox_mcp::endpoints::manifest`（与 S1 的 `describe_endpoint` 是同一个函数），
> 退出码 `0` 成功 / `2` 打不出来。下面命令里的 `cargo run -p vox` 是**桌面档**；无屏档把二进制换成
> `vox-headless --config <settings.json>`，**读法（键名）逐字相同**。
> **本轮实测（2026-09-22，本机）**：`./target/debug/vox-headless --print-composition` → 退出码 `0`、stdout 是一份**合法 JSON**
> （顶层 `capabilities` / `speak` / `listen` / `errors`；本机实测 `.speak.session.provider` = `"aliyun"`、`.capabilities.tier` = `"linux_headless"`）。
> 出处：`docs/architecture/DIRECTIONS.md` §10.7「第十五轮回填」；无屏档钉子用例
> `crates/vox-headless/tests/headless_entry.rs::print_composition_prints_the_two_manifests`（断言退出码 0 + stdout 可解析）。

带这个参数时打印两份清单 JSON + 当前有效能力位（`CapabilityReport`）就退出，**不建 Tauri、不改任何状态**。
先例：`app/src-tauri/src/lib.rs:54-58` 的 `platform::pre_main()` 已经是同类短路（`--vox-restore-defaults`）。

```bash
cargo run -p vox -- --print-composition | jq '.speak.session.provider'   # => "aliyun"
cargo run -p vox -- --print-composition | jq '.listen.ops[].kind'        # => "mono","gate","resample"
cargo run -p vox -- --print-composition | jq '.capabilities.tier'        # => "windows"（本机档位）
cargo run -p vox -- --print-composition | jq '.capabilities.host.mic'    # => {"enabled":true,"reason":null}
# Linux 桌面，接线前（§3.2 的中间态）—— 这两条就是"位如实报假"的证据：
cargo run -p vox -- --print-composition | jq '.capabilities.host.virtual_mic'
#   => {"enabled":false,"reason":"not_wired"}
cargo run -p vox -- --print-composition | jq '[.speak.out[] | select(.role=="virtual_mic")] | length'
#   => 0（位为假 → 那一格不进清单）
```

这是"清单能表达现状 + 位是事实"最直接的人眼证据，也是 S1 `describe` 出口的原型。

**B. 界面**（`cd app/ui && npm run verify` 全绿），其中新增 `check-capabilities.mjs` 断言：

> **第六轮状态**：`app/ui/scripts/check-capabilities.mjs` **已落地**并挂进 `npm run verify`（`app/ui/package.json` 的 `verify` 链）。它用的 URL 开关就是下面这几条（`app/ui/src/mock/backend.ts` 的 `seed()`：`?host=` 四档、`?off=<位>:<reason>,…`、`?on=<位>,…`，另有 `?virtual_mic=<reason>` 是 `off=virtual_mic:<reason>` 的简写）。下面这份清单仍是**验收形状**（脚本覆盖不到的新位要补进来）。

- `?mock=1&host=windows`：虚拟麦区块给安装引导（与现状一致）；`vr_captions` 为假时不渲染 SteamVR 开关；
- `?mock=1&host=linux`：虚拟麦区块**只给设备名引导**、**不出现**"安装/卸载"按钮（与现有 `check-cable.mjs:66-78` 一致，行为不许回归）；
- `?mock=1&host=linux&virtual_mic=not_wired`（**中间态**）：不渲染"去目标程序里选 Vox Virtual Mic"那句话，改渲染 `not_wired` 文案；
- `?mock=1&host=android`：虚拟麦区块**不渲染**、"抓程序"选择器**不渲染**，且各自的 reason 文案各出现一次（**不许**出现"没有这个功能"这种说法，§2.6 R9）；
- `?mock=1&host=embedded`：`ui` 相关区块（字幕、托盘、热键）全不渲染；
- 任一 `(位, reason)` 组合缺 i18n 条目 → 脚本失败（`zh` / `en` 两份都要有）。

### 4.4 一条防"位 ON 而功能不存在"的真机检查（Linux）

位表说 `virtual_mic` 为 `true`，就必须在真机上看得见设备、听得见声音；**位为假时，界面不许再说"去选虚拟麦"**。
两态分开验（**接线前后各跑一遍**，这就是 §3.2 中间态的验收）：

> **第三轮时点**：接线已落地 → **当前该验「态 B」**；「态 A」保留为"接线未落地时位必须为假"的回归依据（一旦虚拟麦建不起来，它就该回来）。

```bash
# ── 态 A：接线前（今天）—— 位必须为假，且界面文案已撤下 ──
cargo run -p vox -- --print-composition | jq -r '.capabilities.host.virtual_mic.reason'   # => not_wired
wpctl status | grep -q "Vox Virtual Mic" && echo "不该有设备" || echo "OK：没有幽灵设备"
# 界面：`?mock=1&host=linux&virtual_mic=not_wired` 下不得出现"去目标程序里选 Vox Virtual Mic"

# ── 态 B：接线后 —— 位为真，且设备真的在、声音真的进得去 ──
# 起「对外说话」（说话让译音产生）之后：
wpctl status | grep -q "Vox Virtual Mic"    # 必须成立
pw-link -l | grep -q vox_virtual_mic        # 端口必须在
pw-record /tmp/vmic.wav &                         # 从虚拟麦的监听端录一段
pw-link vox_virtual_mic:monitor_FL pw-record:input_FL
pw-link vox_virtual_mic:monitor_FR pw-record:input_FR
# 停录，用 sox/ffmpeg 看不是静音（"译音真的进得去"，而不只是"设备在"）
sox /tmp/vmic.wav -n stat 2>&1 | grep -q "Maximum amplitude: 0.0" && echo "静音 = 没接上" || echo "OK"
# 退出应用之后：
wpctl status | grep -q "Vox Virtual Mic"    # 必须不成立（不许留幽灵设备，virtual_sink.rs:28 的 object.linger 故意不开）
```

现有可复现路径：`cargo test -p vox-audio-linux -- --ignored virtual_sink_lifecycle`（`crates/vox-audio-linux/src/virtual_sink.rs:102-117`）、
`cargo run -p vox-audio-linux --example virtual_mic`（`examples/virtual_mic.rs:21-31`）、
`cargo run -p vox-audio-linux --example smoke`（`examples/smoke.rs:15-17` 那三行 `pw-link` 就是上面态 B 的录法）、
`docs/platform/LINUX.md:84-88` 的手工 `pw-cli` 复现。

**现状（态 A 之前）这条检查全不成立**（§1.4：位说 ON、设备不存在、文案还指着它）——所以它同时是本轮的验收项与一个已存在的产品缺口。

---

## 5. 风险与未决

### 5.1 不确定之处（**末尾清单**；每条自带状态标记——`[未核实]` 的才是未决，`[已拍]` / `[已定]` / `[已收口]` 的留作口径留痕）

1. **`[已拍，不再是未决]` Linux 虚拟麦的处置 = 接线。**
   原稿把"未接线是缺陷还是有意设计"列为未决。`DIRECTIONS.md` §10.5 功能处置表（"虚拟麦……**含 Linux 接线**"）+ Main 第 ① 项（"**接线**，接线前位报 false、文案撤下"）已答。
   原稿那句"如果产品意图确实是用户手工建，那 (b) 就是正解"**作废**——(b) 现在只是**接线落地前的中间态**，不是终点（§3.2）。
   留下的真问题变成了技术性的，见下面第 9 条。

2. **`[已拍]` `session` 就是第 8 格。**
   备选方案（把它做成 `out[]` 里一条 `kind: "cloud"`、其余出口写 `source: "cloud"`）**留档但不再采用**：那样"云既是一个出口、又是另几个出口的来源"这条特殊规则要进校验器，而独立一格让 §4.2 的对比断言一一对应（`Plan.passthrough`，`mod.rs:90-92`）。Main 已拍。

3. **`[未核实]` `host_ceiling` 放芯里对不对。**
   我放芯里，理由是 S1 的 `describe`/`compose` 要回答"**别的**档位能不能做某件事"，而那时可能不是在外壳进程里跑。
   代价：芯里多一张"关于平台的数据表"。若判断为不妥，改成"外壳声明自己的上限"也能工作，但 S1 要去问每个外壳。

4. **`[未核实]` Android 与无屏两列的位值来源。**
   §2.5.1 那两列的依据是外部官方文档 + `agent://AndroidPath` / `agent://EmbeddedFirst`，**不是本仓库代码**（本仓库还没有这两个外壳）。真机实测前它们只能算设计输入——本轮已按这个口径**逐格**标了 `[未核实]`（§2.5.1 的表与"三条读表须知"第 2 条）。

5. **`[已拍]` provider 的 4 个待补位只进代码枚举。**
   原稿的做法（"位表占名 + `catalog/*.json` 加字段（全 false）"）**已作废**：Main 拍"只进代码枚举"。
   现在的写法见 §2.5.1：`build.rs` 与三份 JSON **都不动**，`supports()` 对这 4 位恒 `false`。
   附带结论：`usage_reporting` 的界面消费者（用量页）已按 §10.5 砍掉 → 这一位暂时没有消费点（保留占名是为了将来补"按位降级"时不必再动枚举）。

6. **`[第四轮已部分收口]` `Busy` 这个 reason 的实例。**
   原稿写"本仓库代码里没有任何一处产生 `Busy`"——**第四轮起不再成立**：`captions`（窗口起来了后来又没了）与 `vr_captions`（HMD 探针过了但还没连上）两处定义者会报它（§2.5.4）。**仍然没有产生点的是"麦克风被占"那一路**：它今天只以 `PortError` 出现（`pipeline/mod.rs:1700-1702` 的测试替身），因为 `mic` 位有意不接定义者（§2.5.4 第四轮第 2 条，见下面第 13 条）。原稿"没人用的变体"这个判断**作废**。

7. **`[未核实]` 前端 owner。**
   三角色里没有"ui-dev"，我把 `app/ui/**` 全部记在 shell-dev 名下（`DIRECTIONS.md` 与 `.omp/AGENTS.md` 都没写 UI 归谁）。如果界面另有 owner，§3.2 的最后几行要拆出去。

8. **`[未核实]` 给 `SessionParams` 加 `Serialize/Deserialize` 有没有副作用。**
   它是纯数据（`crates/vox-core/src/cloud/protocol.rs::SessionParams`；写稿当时 `:72-89`，第三轮核到 `:79`，目前**没有**任何 serde derive），加两个 derive 理论上零风险（只加 impl、不动字段）。风险只在"报文体是从它**手工拼**出来的"这条路径上：`cloud/gpt.rs:32-62`、`cloud/mod.rs:211-217` 分派的 `aliyun_session_update` / `gemini::setup_frame` / `gpt::session_update` 各自拼 JSON，**我没有逐条跑过**加 derive 之后这三家的报文是否逐字节不变。落地时用现有的报文断言兜底即可——`cloud/gpt.rs:190-196`（`session_update_uses_translation_config_shape`）、`cloud/protocol.rs:586-593`（`session_update_wraps_the_config_with_an_event_id`）、`cloud/mod.rs:560-564`（Gemini setup 首帧）、`:603-606` 与 `:697-700`（Aliyun `session.update` / 热更新后语言）。

9. **`[未核实]` 虚拟麦的设备名怎么对上（§3.2 第 ② 件事的 (i)/(ii)）。**
   两条链各有缺口：① `PlaybackSink::open(device)` 把名字塞进 PipeWire 的 `target.object`（`crates/vox-audio-linux/src/playback.rs:227-231`），而 `target.object` 认的是**节点名**（`vox_virtual_mic`）；② 设备目录报给界面的却是 `label_of()` 取的 `node.description`（"Vox Virtual Mic"，`crates/vox-audio-linux/src/registry.rs:86-92`），且 `DeviceInfo` **只有 `name` / `is_default`**（`crates/vox-core/src/ports.rs:149-152`），没有稳定 id。
   → 落地时**必须实测**：若 `target.object` 吃 description，就什么都不用改；否则要么走 §3.2 的 (i)（缺省解析），要么给 `DeviceInfo` 加 id（core-dev 的 `ports.rs`）。**这条不实测就会重演"位说 ON、声音进不去"**。

10. **`[未核实]` `host` 从常量改成档位，有没有别的下游假设。**
    **S1 稿已闭环**：`docs/plans/S1-AGENT-FACE.md` 已按四档对齐（按**节**引：S1 §1.4 的上游结论表、§2.1.3 的 `device.tier` / `manifest.host` / `capabilities.tier`/`host` / 字段说明、§2.1.3-③ 第 2 步的 `missing_on`——**S1 前 250 行第三轮被改过，行号一律别引**），S0 侧不用再催。
    仍**未核**的是另外两处：S2 / S3 的外壳设计稿（还没写）与 `app/ui` 侧有没有硬编码过 `native`。

11. **`[未核实]` `program_tap`（Windows）的 build 门限没有真机跑过。**
    探针**已经存在**（`crates/vox-audio-win/src/osver.rs:38` 的 `process_loopback_available()` + `osver.rs:14` 的门限常量 `MIN_PROCESS_LOOPBACK_BUILD = 20348`；`crates/vox-audio-win/src/lib.rs:39` 重导出、`capture/mod.rs:331,369` 在用）——原稿写"没有探针"是**错的**，本轮已按 verifier 复核改正（§2.5.4 / §3.2）。
    仍未核的只剩一条：**20348 这个门限在真机上对不对**（我只核到代码常量与 `DIRECTIONS.md` 的表述）。

12. **`[未核实]` `tray` 位在 Windows 上的语义被我收紧了**（定义者从 `platform/*::tray_host_available` 改成 `tray.rs::can_hide_to_tray()`，即"图标装上 × 有宿主"）。
    理由是 `platform/win.rs::tray_host_available()` 那个函数恒 `true`（通知区域永远在；写稿当时 `:120-122`，第三轮核到 `:194`），图标没装上时位会说谎（§2.5.4）。**行为影响**：托盘安装失败（`lib.rs:232-238` 那条 `Notice`）现在会同时让位为假 → 界面/Agent 会看到 `tray: false`。这是"位=事实"的正确方向，但落地时要确认没有别处依赖"Windows 上 tray 恒真"。

13. **`[已定，写下来免得被当成漏项]` `mic` 位"按档位上限报开"是有意偏离 R6（第四轮 F1 收口）。**
   owner shell-dev 已确认**不**接那条 `busy` 路。代价（一句话）：装配期没有任何否定证据——此刻没开流，"被占 / 没授权"只有真去开流才知道，所以位只能按上限报开；界面**不会提前**把麦克风那一块灰掉，失败推迟到用户按"对外说话"那一刻，由 `PortError` → `Notice` 兜底。要收口只剩一条路：把定义者挪到**起流结果**上（会话期回写事实）——那时 `mic` 会变成"会话期事实"，与其余"装配期事实"不同类，S0 不做。**新增位不许照抄这一条**（§2.5.4 第四轮第 2 条）。

### 5.2 本轮已无待裁项；落地时必须实测的三件事

`DIRECTIONS.md` §10.6 的结论是"**本轮无待拍项**"。原稿列的三条（Linux 虚拟麦走 (a) 还是 (b)、`session` 独立一格、provider 4 位进不进 JSON）**全部已拍**，见 §5.1-1 / §5.1-2 / §5.1-5。
剩下的是**实现期的实测项**，不是决策：

| # | 实测什么 | 怎么算过 | 对应 |
| --- | --- | --- | --- |
| 1 | `target.object` 吃 description 还是 `node.name`（决定 §3.2 第 ② 件事走 (i) 还是 (ii)） | 起一次「对外说话」：`pw-link -l` 里播放流接在 `vox_virtual_mic` 上，且监听端录到**非静音**（§4.4 态 B 的三条命令） | §5.1-9 |
| 2 | Windows 那两个位（`program_tap` 的 build 门限、`tray` 收紧成"装上 × 有宿主"） | 老 build 上 `program_tap` 报 `false(unsupported)` 且界面说清；托盘安装失败时 `tray` 报 `false` | §5.1-11、§5.1-12 |
| 3 | Android / 无屏两列的位值 | S2 / S3 真机跑 §4.3-A，把 §2.5.1 里的 `[未核实]` 逐格换成实测值 | §5.1-4 |

### 5.3 已知风险（非"未核实"，是判断）

| 风险 | 说明 | 兜底 |
| --- | --- | --- |
| 清单成为"第二份真源" | 如果 `Composition` 与 `Plan` 各存一份状态，就会漂移 | `Plan` **只能**由 `Composition` 构造（§2.7），`Plan::build` 是唯一入口；对比断言钉死这条 |
| 位越多越没人维护 | 11 个宿主位里 3 个（`net_in` / `net_out` / `file_config`）是"目标档"、今天恒假；8 个 provider 位里 4 个是"占名" | 位表**必须**由 §2.5.4 的"定义者"一一对应；没有定义者的位不许进表（`net_in` / `net_out` 就是"没有定义者 ⇒ 恒假"的样板）。**唯一写下来的例外是 `mic`**（按上限报开，代价见 §2.5.4）——例外只许这一条，且必须写在定义者表里 |
| 界面降级文案散落 | 每个 `(位, reason)` 一条 i18n：**zh + en 两语** × 最多 19 位 ≈ 上限 38 条 | 只有**关掉的位**需要文案；`check-capabilities.mjs` 覆盖四档宿主即可 |
| 与 S1/S2 并行时字段漂移 | S1 稿已引用本清单字段 | S1 稿**已按本轮的四档 + `tier` + `missing_on` 返回类型对齐**（见 §3.3 ③）；约定"以本稿为准，不自立第二套" |

### 5.4 本轮改动**波及**的别的文件（我一行都没改，交给对应 owner）

一个文件一个 owner——下面是本轮改了**输入**、但本稿没有权限去动的地方：

| 文件 | 哪一处已经过时 | 该谁改 |
| --- | --- | --- |
| `docs/architecture/DIRECTIONS.md` | ① §3.5 的 `host: native \| wasm \| browser \| mcu`（本稿改成**宿主档位**，§0.2 第 4 行）；② §2.5.4 的"虚拟麦……应该写成可选能力，而不是产品的必需项"（§0.2 第 6 行）；③ §10.4 的"[待裁 1/2/3]"（**已拍**，见 §10.5 / §10.6，那三条待裁项可以撤了） | Main（方向总表） |
| `docs/plans/S1-AGENT-FACE.md` | **第三轮（D1）已闭环**：S1 稿已按 D1 把三处 `Composition::of` 统一成 `of(&SessionConfig, &HostFacts)`（事实从 `Runtime::host_facts()` 取，不另立第二份）；`missing_on` 一律吃 `Runtime::capabilities()`，两者没混。第二轮的对齐按**节**引（行号已漂，S1 前 250 行第三轮被改过）：`host` 四档与 `missing_on(...) -> Vec<CompositionError>`、`schemars` 锁 0.8.22 → S1 **§1.4 / §2.6**；`device.tier` / `manifest.host` / `capabilities.tier`/`host` / 字段说明 / `missing_on` 第 2 步 → S1 **§2.1.3 与 §2.1.3-③**；`host` 必须逐字相同 → S1 **§2.1.4** 的 `editable` 表。**S0 新增一条留档（跨 owner）**：S1 要把 `CompositionError` 送上线路（`data.errors`），而它今天**没有 `Serialize`**（`crates/vox-core/src/composition.rs::CompositionError`）→ **core-dev 待补**一个带判别键的 serde，**线上名字以 S1 §2.1.2 为准**（§2.1 已记这条） | agent-face-dev（已完成）/ core-dev（待补 serde） |
| `.omp/AGENTS.md` | 无需改：它只写"宿主/设备侧要按同一套做"，没有写死任何字段值 | — |
