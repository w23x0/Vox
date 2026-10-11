# S4-C · 半双工门 + 廉价重采样档 · 施工设计稿

> 状态：**[已落地]**（原 r3 修订稿，2026-10-10；落地回填 2026-10-11）。**G1–G4 与 R1–R3 七张工单本轮全部收口**
> （G5 的 `app/ui/src/types.ts` 那一行按 §4.6 并进 G3，同批落地）；施工后的门禁实测见 §5.1，
> 稿子自身被复核证伪的 **9 处**见 §0.4。**AEC（`Op::Aec`）不在本稿范围**，§7.3 的 `[未决]` 项
> 一条都**没有**因为落地而自动结案。
> 对应 [`S4-EMBEDDED-REFACTOR.md`](S4-EMBEDDED-REFACTOR.md) §3「S4-C 新模块」表里
> 「半双工门」与「廉价重采样档」两行，§2.2（回声消除三档）、§2.3（轻量化与 A53 设计预算），以及 §4 的两行验收。
>
> 上游：[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md)（算子链已落地，**B3 逐节计量也已落地**，本稿按**代码**写）、
> [`S4-C-ALSA.md`](S4-C-ALSA.md) §5.2（"能不重采样就不重采样"）与 §12（M1–M6 拍板）。
> 方向：[`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §8 第 16/17 行、§10.9 第 4/7 条。
>
> **口径**：代码与本稿打架以代码为准。**引用一律给符号名，不给行号**（`.omp/RULES.md` #11）。
>
> **r3 的核对基线**：本稿 rebase 到本地 `main`，核对时 `git rev-parse main` = `4bc6e8a`。
> r3 本轮（2026-10-10）**真跑过**的东西，逐条列清（`.omp/RULES.md` #10）：
>
> | 跑了的 | 命令 | 结果 |
> | --- | --- | --- |
> | 金样基线 | `cargo test -p vox-core pipeline::golden` | **16 passed, 0 failed**（15 场景 + 1 条 `goldens_are_deterministic_when_regenerated`，见 §5.2 的更正） |
> | `mod.rs` 用例实数 | `grep -c '#\[test\]' crates/vox-core/src/pipeline/mod.rs` | **44** |
> | `Resampler::new` 调用方实数 | `grep -rn 'Resampler::new' crates/ tools/ app/` | **17** 处（§1.5） |
> | `Linear` 越界条件（E2） | `/tmp/opencode/lin` 里三个独立小程序，逐字复刻 `bench-dsp` 的 `Linear::process` 后扫块长 | 见 §3.2 的表；**本稿在这一条上从"纸上推导"升级为"实跑"** |
>
> **r3 没跑**：`cargo test --workspace` / `clippy` / `npm run verify` 全没跑（本稿只改文档，`git diff --stat` 只有一个 `.md`）。
> `tools/bench-dsp` 本身也叫复核者跑过（§3.1），r3 没有重跑它。
>
> **r2 → r3 的变化**（详见 §0.3）：① 修 6 处与现状不符或数量数错的事实（§0.3 的 E10–E15）；
> ② **§3.2 的越界条件从推导升级为实跑**，`44.1k→16k` 那条"[未核实]"随之结案；
> ③ **两张在途稿（`OVERLAY-CAPTIONS` / `MODEL-TABLE`）的施工部分已经合进 `main`**，"排在谁之后"的约束随之消解，§4.5 整节重写；
> ④ §5.2 的金样期望条数从 16 改成"现在 16、G4 之后 17"（`pipeline::golden` 这个过滤器会多匹配一条确定性自检）。

---

## 0. 一句话

**半双工门 = 给 `ActivationGate` 加一个"按采集率样本倒数的压制窗口"，压制窗口在"刚把译音推进播放汇"的那一刻
由 `Worker` 量着"设备里还剩多少毫秒没放"装满；清单侧落在 `GateConfig` 的一个新字段上，桌面档一个字节都不变。
廉价重采样档 = `vox-dsp` 加一档**只对升采样方向生效**的线性插值实现，装配期由**入口**选，
`ports.rs` / `vox-core` / 清单 / 能力位**一行都不动。**

两件事**互不重叠文件**（§4.4），可以并行派；但两者都撞相邻稿子（§4.5）。

### 0.1 本稿对上位稿的收窄与推翻（`DIRECTIONS.md` §8「新者胜」）

| # | 上位稿写的 | 本稿改成什么 | 为什么 |
| --- | --- | --- | --- |
| **N1** | §2.2「在现有 `vox_core::gate::ActivationGate` 上加"**播放中**输入" | 不是 `bool` 输入，是**一个按采集率样本倒数的计数器**（`ActivationGate::set_mute_for_ms(u64)`） | ① 门内部**没有时钟**（`tail_ms` / `preroll_ms` 都按 `self.sample_rate` 换算成样本数，见 `gate.rs::samples_for_ms`），加一个 `bool` 就得给它塞一个时钟进来；② 差分台要的是**与墙钟无关的确定性**（`golden.rs` 模块头把两个 `Instant::now()` 来源都排除了），时钟会让新金样依赖 `clock.advance` 的时机；③ 计数器让整条逻辑能被 `gate.rs` 的单测单独钉住，不牵动 `Worker` |
| **N2** | §2.3「重采样加一档廉价实现（**线性/多相**）」 | **只做线性、且只对升采样方向**；**多相 FIR 这一档不做** | §3.1 实测：降采样方向上多相 FIR **不比 rubato 便宜**（P=32×T=32：0.63 vs 0.50 ms/s；T=48：1.00 vs 0.50），而线性便宜 12 倍但**混叠 0 dBFS**。升采样方向上镜像落在输出带外，喇叭上听不见，线性便宜 12.7 倍 |
| **N3** | §2.2「回声消除三档**按能力位自动选**」 | 半双工**不新增能力位**，判据落在清单已有的两格上（麦克风采集 + 有出声的播放出口 + 无屏档） | 这一档只由"哪一档构建"决定，而"哪一档构建"已经是 `HostKind` 这条硬事实（`composition::HostKind`），再加一位是同一件事的第二份表达。**加位的时机是 AEC 落地时**（三档互斥需要一个共同的选档入口），见 §2.5.4 |
| **N4** | §2.2 半双工「`Op::Gate` 可配」 | 配置落在 **`GateConfig` 的新字段**上（`Op::Gate` 仍然是同一格，**不加新 `Op`、不加新 `GateKind`**） | 半双工与 `Manual` / `Level` 是**正交**的，不是第三种门；加成 `GateKind` 就得表达 `HalfDuplex×{Manual,Level}` 的叉积。见 §2.5 |

### 0.2 本稿没有推翻的

S4-B 的 **INV-1**（任何 `Note` 都不许重建算子链）、**INV-2**（断线不重建链）、契约 C1–C6、
`DenoiseSkip` 的两支路可观察行为、15 份金样"逐字不变"——**一条都不动**，本稿的全部设计都与它们相容（§2.5.5、§4.1）。

### 0.3 r1 → r3 的修订清单（复核报告逐条对应）

| # | 复核报告提的问题 | r2 怎么修的 |
| --- | --- | --- |
| **E1**（重要） | §2.2.4 与 §2.2.3 自相矛盾：压制分支只判 `mute_samples > 0`，而 `set_config` 保留该窗口 ⇒ 没装半双工的会话会被继续压住 | 分支加判 `config.half_duplex.is_some()`（§2.2.3），`set_config` 在 `half_duplex` 变 `None` 时把窗口一并归零（§2.2.4）。**两道**机制各自一条用例钉住 |
| **E2**（重要） | R3 的越界触发条件写错：不是"非整数步长"而是 `step > 2` 撞特定余数；整数步长 3.0 在块长 320 时同样炸；`44.1k→16k` bench-dsp 根本没构造过 | §3.2 整段重写，给出推导、三个反例，并在 r3 换成**实跑**结果（§3.2 的表）；R3 的范围按真实条件重排，并**新增"补 44.1k→16k 行"这一条**（§3.2、§3.6、§4.2、§4.3） |
| **E3**（次要） | `Speaker::rendered` 不是"测试可拨的计数器"（私有 `AtomicU64`，全仓库零写入、无 setter） | §1.6 改写为"只读、恒为 0" |
| **E4**（次要） | "41 条既有测试"数错 | §1.6 / §4.1 改为 `pipeline/mod.rs::tests` 实数 **44** 条，并说明 B3 新增 2 条 |
| **E5**（次要） | `Resampler::new` 的"全部调用方"漏 8 处 | §1.5 给出 **17 处**完整清单（复核报告找到 6 处，我另外找到 2 处：`vox-audio-alsa/examples/loopback.rs`、`vox-audio-alsa/tests/aloop_roundtrip.rs`） |
| **E6**（次要） | §2.5.2 正文说判据侧要有 session，代码里没有 | 判据加 `&& !passthrough`（等价于读 `composition.session.is_some()`），并把"两道保险"那段写实（§2.5.2） |
| **E7**（次要） | 行号漂移：`mod.rs:1130` 是 `chain.run()` 而非 `take_gate_status`；`resample.rs::process`、`golden.rs::start_speak`、`dto.rs` 三处漂移 | 全部行号删除、改符号名（`RULES.md` #11）。⚠️ **r3 更正**：r2 在这里多写了一句话——"`Chain::take_gate_status` 在 B3 之后不复存在"——**这是假的**，它还在，`Chain::run` 就是调它的那一方（见下面 E10） |
| **E8**（待裁决） | Q2 引用了不存在的 `docs/plans/S4-C-AEC.md` | 保留引用，注明"见 AEC 稿（另行合入）"，并写清该文件截至 2026-10-10 尚不存在（§7.3 Q2、§8） |
| **E9**（r1 漏、r2 新发现） | —— | **R2 的前提被 S4-A W4 推翻了**：`Deps.resample` 的唯一出处已经收进 `vox_host::entry::HostPorts::deps`，桌面与无屏共用一行。无屏档**没有**自己的装工厂的地方，必须给 `HostPorts` 加一格。改法见 §3.4 / §4.2 |

**r3 新查出的（E10–E15）**——都是"对着 `main` @ `4bc6e8a` 重新核代码引用"时浮出来的，一条都不是复核报告提的：

| # | r3 发现的问题 | r3 怎么改的 |
| --- | --- | --- |
| **E10** | §1.3 写"`Chain::take_gate_status` 在 B3 之后**不复存在**"——**假的**。它在 `chain.rs` 里活得好好的（`pub(crate)`），`Chain::run` 的返回表达式就是 `(scratch.take_current(), self.take_gate_status())` | §1.3 改写：`run` 的返回值带出状态这件事没错，但 `take_gate_status` 是**被 `run` 调用**、不是消失。`Worker` 侧仍然只认 `run` 的返回值，所以对 G2 的结论一个字不变 |
| **E11** | §1.4 写"有**两条**逐字比 JSON 的既有测试"——**数错**。`composition.rs` 里只有**一条** `the_manifests_match_the_design_documents_instances`，它的函数体里有**两份**期望值（对外说话那份 / 听人说话那份） | §1.4、§4.3（G3-⑪）、§5.2 的变异表、§7.1 的 GR-1 四处统一改成"一条测试、两份期望值"。⚠️ **r4 再更正**：那条"去掉 `skip_serializing_if` ⇒ 一条红"的说法**不完整**——实测是 **3 条**红（见 §0.4 **F9**） |
| **E12** | §5.2 / §4.3 的金样期望条数写 16 | **现在跑出来就是 16**（15 条场景测试 + 1 条 `goldens_are_deterministic_when_regenerated`，后者也被 `pipeline::golden` 这个过滤器命中）。改成"G4 之前 16、G4 之后 17" |
| **E13** | §4.2 给 `crates/vox-headless/src/platform/linux.rs` 标了"（第 34 行那一句）"——**行号**，违反 `RULES.md` #11 | 改成符号名：`platform::platform` 的 `playback` 闭包里那一句 `let resample = vox_dsp::ports::resample_factory();` |
| **E14** | §4.5 写"在途的两张稿子都还没开工"——**过期**。`OVERLAY-CAPTIONS`（`690efef` + `33927d3`）与 `MODEL-TABLE`（`73e7c58` + `6b91a9a` + `0a04e19` + `7ca4333`）的施工部分**都已经合进 `main`** | §4.5 整节重写：文件冲突仍在，但"排在谁之后"的约束**已经消解**；逐张工单改标"落点上已经是什么" |
| **E15** | §3.2 把 `44.1k→16k` 会不会炸标成 `[未核实]`（"取决于 `pos` 的轨迹"）——r3 实跑之后这一条可以结案 | §3.2 换成实跑结果，`step > 2` 的口径也从"推导"升级为"测量"；§7.3 Q10 对应的一条未决项一起结案 |

### 0.4 r4 订正清单（2026-10-11，**施工落地后回填**）

落地之后由**两个独立 verifier** 逐条复核本稿，查出下面 9 处"稿子与代码/实测打架"（F1–F8 是第一轮，**F9 是验收回填补的**）。**这 9 处都是稿子自己写错的，
代码没有按稿子改**（口径：`docs/STRUCTURE.md` 沿用 `DECISIONS.md`——代码与文档打架以代码为准，回头改文档）。
每条都写明"谁核到的、怎么核的"（`.omp/RULES.md` #10）。

| # | 稿子原来写的 | 实测（谁核的 · 怎么核的） | 改成什么 |
| --- | --- | --- | --- |
| **F1** | §5.2 变异行 ①"删压制分支 ⇒ **5 条**红" | 复核者删掉 `ActivationGate::process` 里的压制分支后重跑 `cargo test -p vox-core` ⇒ **8 条红** | 5 → 8（§5.2） |
| **F2** | §5.2 变异行 ④"去掉 `&& config.half_duplex.is_some()` ⇒ G1-⑤ 红" | 复核者逐条做变异：单独去掉 §2.2.4 三道机制里的**任何一道**都**全绿（0 红）**——三道是**故意冗余**的；要变红必须**同时**去掉 `process` 那一判**与**（`set_mute_for_ms` 见 `None` 写 0 **或** `set_config` 在新配置无半双工时归零）之一，实测"同时去掉 ①+③ ⇒ **3 条红**" | 整条作废，改写为冗余说明（§2.2.4、§5.2） |
| **F3** | §5.2 变异行 ②/③ 说"G3-⑩ 的**第三条**红 / **第二条**红" | 与 §4.3 ⑩ 的枚举序号对不上。按 `crates/vox-core/src/pipeline/speak.rs::tests::speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker` 的函数体逐条核：去掉 `facts.host == HostKind::LinuxHeadless` ⇒ 第 ② 条断言（`"桌面档即使退成普通出声也不许自动切话"`）红；去掉 `&& !passthrough` ⇒ 第 ④ 条断言（`"直通推的是原声，关掉上行等于自噬"`）红 | 改成按**断言原文 + 源位置**说，不用序号（§5.2） |
| **F4** | §5.2"播放结束 + 尾巴之后恢复…**块号逐字钉住**" | 金样的 `wire` 段**没有块号**。钉住的是 **append 计数与零尾指纹**：`wire` 段 1–3 是压制前的 `audio_append` 0/1/2（各 640 样本）、`wire` 4 是 `audio_append 3 2400`（`MANUAL.tail_ms = 150` × 16 kHz 的零尾）、`wire` 5–16 是 append 4–15（恢复的 12 块，各 640） | 措辞改成"append 计数与零尾指纹"（§5.2） |
| **F5** | §2.2.2 正文说窗口换算"**取上界**" | 同节代码块与实现都是**整数截断**：`gate.rs::set_mute_for_ms` 是 `sample_rate.saturating_mul(ms) / 1000`，与既有的 `ActivationGate::samples_for_ms` 同一口径（`(sample_rate * ms / 1000)`），**不是上取整** | 正文与代码块注释统一成"整数截断"（§2.2.2） |
| **F6** | §4.1 / §4.3 / §4.6 写 G1"新增 **7** 条单测 / 净 +7"，§4.3 结尾"净 **+17**" | 实际 **+10**：`grep -c '#\[test\]' crates/vox-core/src/gate.rs` = **15** = 既有 5 + 新 10。多出来的三条是钉**可观察面**的用例：`reset_clears_the_mute_window`、`the_playback_mute_state_goes_on_the_wire_as_snake_case`、`half_duplex_is_absent_from_the_gate_config_wire_when_unset` | G1 改成 +10；§4.3 结尾净 **+20**（G 10 + G2 2 + G3 2 + G4 1 + R1 5）（§4.1、§4.3、§4.6） |
| **F7** | §4.3「既有用例的真实条数」表 `crates/vox-core/src/pipeline/mod.rs::tests` 写 **44** | 44 是**稿子基线** `main@4bc6e8a` 的数；按**本轮基线** `main@708b769` 是 **48**（`grep -c '#\[test\]' crates/vox-core/src/pipeline/mod.rs` = **48**），本轮改动对 `mod.rs` 的**增删为 0** | 两个数都写、各自注明来由（§2.8.2、§4.1、§4.3） |
| **F8** | §4.2 / §4.4 的 R2 文件清单漏了 `crates/vox-headless/src/platform/mod.rs` | 档位策略住在那儿：`vox_headless::platform::{RESAMPLE_QUALITY_ENV, resample_quality_from_env, resample_factory_for}` 与 `Platform` 的新字段 `quality`（`platform()` 的签名未变） | 补进 §4.2 / §4.4 / §4.6 的 R2 清单，并写明"环境变量仍只在无屏档读"（§4.2、§4.4） |
| **F9**（D1 补） | §5.2 变异行 ⑤ 与 §7.1 GR-1 都写"去掉 `skip_serializing_if` ⇒ **那一条**测试红" | 复核者做该变异后重跑 ⇒ **3 条**红：`gate::tests::half_duplex_is_absent_from_the_gate_config_wire_when_unset`、`pipeline::speak::tests::the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset`、`composition::tests::the_manifests_match_the_design_documents_instances`（最后这一条自身有两份期望值，两处一起红） | 两处都改成"**3 条** + 列三个用例名"（§5.2、§7.1） |

**本轮真跑过的命令与原始结果**（`.omp/RULES.md` #10；照抄，不加工）：

```bash
cargo test -p vox-core                                              # 364 passed / 0 failed
cargo test -p vox-core pipeline::golden                             # 17 passed（15 旧场景 + 1 新场景 + 1 确定性自检）
cargo test -p vox-dsp                                               # 34 passed
cargo fmt --all -- --check                                          # 静默
cargo clippy --workspace --all-targets --exclude vox-audio-alsa -- -D warnings    # 0 warning
cargo test --workspace --exclude vox-audio-alsa                     # 872 passed / 0 failed / 6 ignored
cd tools/bench-dsp && cargo run --release                           # exit 0
grep -c '#\[test\]' crates/vox-core/src/gate.rs crates/vox-core/src/pipeline/mod.rs   # 15 / 48
git diff --stat crates/vox-core/src/pipeline/golden/                # 空（15 份旧金样一个字节没动；第 16 份是新增的未跟踪文件，不进 diff）
```

> ⚠️ **`cargo test --workspace`（不含 `--exclude`）在本机跑不了**：`alsa-sys` 的 build script 找不到系统的
> `alsa.pc`（本机只装了 `libasound2t64`，**没有 `libasound2-dev`**）。这是**环境缺口，不是本轮改动的后果**
> （本轮没碰 `crates/vox-audio-alsa/**`）。`vox-audio-alsa` 是 workspace 成员，`.github/workflows/ci-windows.yml`
> 里对它用 `--exclude`，`release.yml` 装 `libasound2-dev`。

---


## 1. 现状证据（r3 于 2026-10-10 对着 `main` @ `4bc6e8a` 核，全部按符号名）

> **本节是施工前的快照**：`gate.rs` 的 5 条、`vox-dsp::resample::tests` 的 5 条、`pipeline/mod.rs::tests` 的 44 条
> 都是**落地前**的数。落地后的实数见 §0.4 的 F6 / F7（`gate.rs` = 15、`mod.rs` = 48）。

### 1.1 门：`ActivationGate` 的形状与状态机

全部在 `crates/vox-core/src/gate.rs`（384 行）。

| 事实 | 符号 |
| --- | --- |
| 两种门：`Manual` / `Level` | `GateKind` |
| 参数是 `Copy` 的四格：`kind / threshold / tail_ms / preroll_ms` | `GateConfig` |
| 两个预设：`MANUAL`（tail 150 / preroll 100）与 `level(t)`（tail 600 / preroll 200） | `GateConfig::MANUAL`、`GateConfig::level` |
| 缺省实例走 `MANUAL` | `impl Default for GateConfig` |
| 门的**状态**：一个 `config` + 一个 `sample_rate` + `active` + `silent_samples` + preroll 缓冲（`prebuffer` + `prebuffer_samples`）+ `external_active` | `ActivationGate` |
| 门的构造 / 读配置 | `ActivationGate::new`、`ActivationGate::config` |
| **门里没有时钟**：时长一律按 `sample_rate` 换算成样本数 | `ActivationGate::samples_for_ms` |
| 手动门的**收尾边**：从激活转为未激活那一拍发一段 `tail_ms` 的零样本，`ended = true` | `ActivationGate::process_manual` |
| 电平门的收尾：静音期**照常放行**直到 tail 走完，`ended` 在最后一拍 | `ActivationGate::process_level` |
| preroll 是 `VecDeque<Vec<f32>>`，上升沿 `accept` 时一次冲出来 | `ActivationGate::prebuffer`（字段）、`ActivationGate::push_prebuffer` |
| `set_config` 复位内部状态但**保留 `external_active`**，且有专门的钉子用例 | `ActivationGate::set_config`、`gate.rs::tests::set_config_keeps_external_active` |
| `reset()` 会把 `external_active` 清成 `false`（所以断线路径**不许**调它） | `ActivationGate::reset`；`chain::Chain::on_disconnect` 的注释与实现 |
| 既有 **5** 条单测，全在 `gate.rs::tests` | `gate.rs::tests` |

**要点**：`ActivationGate::process` 的分派是 `match self.config.kind { Manual => …, Level => … }`，
RMS 的两行计算之后紧接着就是这个 `match`。**新逻辑要插在 RMS 之后、`match` 之前**，这样两个 kind
天然被一起覆盖，不需要在两个分支里各抄一遍。

### 1.2 播放汇在哪、`push` 在哪调 —— **与门在同一个线程**（本稿最关键的一条现状）

| 事实 | 符号 |
| --- | --- |
| 播放汇是 `Worker` 的字段 `sink` / `monitor_sink`，**不是链上的一节** | `Worker`（`pipeline/mod.rs`） |
| 译音进播放汇的**唯一**生产点：`ServerEvent::AudioDelta` 分支里连续两次 `sink.push(&samples)` | `Worker::handle_server_event` |
| 直通模式下也有一次 `sink.push(block)`，但那是**麦克风原声**（在 `feed_chain` 的 `passthrough` 臂里） | `Worker::feed_chain` |
| `push` 与门 `ActivationGate::process` **跑在同一个 `Worker` 线程上**：`Worker::pump` 里先 `feed_chain`（走链、过门），后 `drain_messages`（→ `handle_server_event` → `push`）；`handle_message` / `handle_server_event` / `feed_chain` 都是 `&mut self` 方法 | `Worker::pump` |
| `sink.push` 跑在**同一个 `&mut self`** 上 ⇒ 不需要 `Arc` / 原子 / 任何跨线程同步 | 同上 |
| "播放汇里还压着多少毫秒"**已经有一个函数算它** | `Worker::playback_queue_ms`，用 `sink.stats().queued_samples / channels / sample_rate` |
| `PlaybackSink::stats()` 的 `queued_samples` 是"环里还没被渲染线程取走的样本数"；`rendered_samples` 是渲染线程累计取走的 | `ports::PlaybackStats`；`ports::PlaybackSink::stats`（**trait 上有默认实现**，测试替身可以只填一部分） |
| 现有播放侧遥测**已经在**用 `stats()` | `Worker::poll_playback` 读 `rendered_samples`，与 `Worker::playback_probe`（`PlaybackProbe::rendered_baseline`）配对 |

**⇒ 推论（本稿全部设计的地基）**：

1. "正在播"这件事**在芯里是现成的、零分配的、同一线程的事实**，不需要新通道、不需要 `Arc<AtomicU64>`、不需要 `Cell`。
2. 播完没播完**也已经有一个测法**（`playback_queue_ms`），本稿复用同一个式子。
3. S4-B §2.7 给 AEC 预留的参考信号口（`Worker::handle_server_event` 的 `AudioDelta` 分支、两次 `sink.push` 附近）
   与本稿的"标记正在播"落点是**同一处**——两者要串行派（都改 `pipeline/mod.rs`），见 §4.5。

### 1.3 算子链的接线点（S4-B 已落地的代码，以代码为准；**B3 已合入**）

| 事实 | 符号 |
| --- | --- |
| 链执行器，**两个**字段：`stages: Vec<ChainStage>` + `timings: [OpTiming; MAX_OPS]` | `chain::Chain` |
| 四种节，枚举分派 | `ChainStage`、`ChainStage::process` |
| 门这一节把 `ActivationGate` **整个搬进来**，另带一个 `status: Option<GateStatus>` 槽位（`take` 语义） | `ChainStage::Gate { gate, rate, status }` |
| 建链时门建在**采集率**上，参数逐字取清单的 `Op::Gate { config }` | `Chain::build` 的 `Op::Gate { config }` 臂 |
| 链尾率 / 无门时的 `flush_resample` / 断线只清有流状态的节 | `Chain::out_rate`、`Chain::flush_resample`、`Chain::on_disconnect` |
| **B3 已落地（r1 写稿时还没有）**：`Chain` 里的定长计量数组、逐节自计时、`Chain::timings()` / `Chain::stage_names()` | `Chain::build`、`Chain::run`、`Chain::timings`、`Chain::stage_names`；`chain::OpTiming` |
| 门状态出口：`Chain::run` 的返回值 `(BlockOut, Option<GateStatus>)`，调用方 `Worker::feed_chain` 就地 `let Some(status) = … else` 早退 | `Chain::run`、`Chain::take_gate_status`、`Worker::feed_chain` |
| 剩下两个门的外部口**仍在 `Chain` 上**，`Worker` 不直接碰门 | `Chain::set_gate_active`、`Chain::set_gate_config` |
| 门的外部口在 `Worker` 侧的调用点 | `Worker::boot`（`set_gate_active`）、`Worker::handle_note` 的 `Note::GateConfig` 臂（`set_gate_config`）、`Worker::feed_chain`（消费 `Chain::run` 的返回值） |

> **对 G2 的影响**：`Chain::run` 的循环体现在每节两侧夹着计量（两次 `Instant::now()`）。
> G2 只在 `impl Chain` 的**末尾**加两个方法、不碰 `Chain::run`、不碰 `ChainStage::process`，
> 冲突面为零。
>
> **r3 更正（E10）**：`Chain::take_gate_status` **仍然存在**（`pub(crate)`，§1.3 表里那一行）。
> r1/r2 说它"已消失"是错的——消失的只是"`Worker` 直接调它"这条路：现在 `Worker` 只读
> `Chain::run` 的返回值，而 `run` 内部**调** `take_gate_status`。这一点对 G2 的落点没有影响
> （新方法照样加在 `impl Chain` 末尾，照样只被 `Worker` 调），但**别再告诉施工的人它没了**。

### 1.4 `GateConfig` 的下游（决定"加一个字段"要动哪些地方）

| 下游 | 符号 | 加字段的影响 |
| --- | --- | --- |
| **本机派生**：`Runtime::gate_for(&Settings)` 从设置折算成 `GateConfig` | `runtime::Runtime::gate_for` | 用构造函数（`MANUAL` / `level(t)`）→ **加字段不用改这里**（前提：`MANUAL` / `level()` 两个预设自己填新字段） |
| 会话配置 | `runtime::SessionConfig` 的 `gate` 字段 | 不动 |
| 控制面命令 | `runtime::PipelineCommand::SetGateConfig { config }`、账本侧的自动派发 | 不动（`GateConfig` 整体传） |
| 账本事件 | `runtime::Runtime::on_gate_status` → `PipelineStatus.gate` | 不动 |
| 清单 JSON | `composition::Op::Gate { config: GateConfig }` | ⚠️ **一条**逐字比 JSON 的既有测试：`composition.rs::tests::the_manifests_match_the_design_documents_instances`，它的函数体里有**两份**期望值（对外说话那份 + 听人说话那份），两份里的 gate 分别是 `{ "kind": "level", "threshold": 0.012, "tail_ms": 600, "preroll_ms": 200 }` 与 `{ "kind": "level", "threshold": 0.0, "tail_ms": 600, "preroll_ms": 200 }` ⇒ **新字段必须 `skip_serializing_if`**，否则这一条红（r3 更正：是"一条测试两份期望值"，不是"两条测试"） |
| 前端类型 | `app/ui/src/types.ts::GateState`（9 个 `snake_case` 字面量的联合类型） | 加 `GateState` 变体要同步加一行 |
| 前端渲染 | 全 `app/ui/src` grep：**`live.gates` 只在 `app/ui/src/store.tsx` 被写，没有任何组件读 `gate.state`** | 不用加 i18n、不用改组件 |
| DTO | `app/src-tauri/src/dto.rs`：`PipelineStatusDto`（`gate: Option<GateStatus>`）与 `SnapshotDto`（同样一格） | `GateStatus` 加字段会改 DTO 的线上形状 ⇒ **本稿不给 `GateStatus` 加字段**（见 §2.2.1） |

### 1.5 重采样：一个工厂同时服务上行与播放侧 —— **r2 重大更正：出处在 S4-A W4 之后已经变了**

| 事实 | 符号 |
| --- | --- |
| 芯只认一个工厂签名 | `pipeline::ResampleFactory = Box<dyn Fn(u32, u32) -> Box<dyn Resample> + Send + Sync>` |
| 唯一实现是 `SincFixedIn`（128 点 sinc + cubic 插值，`f_cutoff 0.95`，oversampling 128） | `vox_dsp::resample::sinc_params`、`Resampler::new` |
| 同率时零开销透传 | `ResamplerInner::Passthrough` 的三个臂（`process` / `flush` / `reset`） |
| **r2 更正**：上行 `Deps.resample` 的**唯一出处**已经收进共享宿主层，桌面与无屏**共用这一行** | `vox_host::entry::HostPorts::deps`（`crates/vox-host/src/entry.rs`），函数体里直接 `resample: vox_dsp::ports::resample_factory()` |
| `HostPorts` 结构体里**没有** `resample` 字段（`denoise` / `resample` 两个字段连影子都没有，注释明说"入口对这两节没有发言权"） | `vox_host::entry::HostPorts` |
| 两个 `HostPorts` 的装配点 | `app/src-tauri/src/lib.rs`（桌面）、`crates/vox-headless/src/headless.rs::host_ports`（无屏） |
| **播放端口内部**的率转换是**注入进来的工厂**，不自己造 | `vox_audio_alsa::playback::AlsaPlayback::new(resample_factory)` 存进字段、率转换处 `(self.resample_factory)(source_rate, rate)`；`vox_audio_linux::playback::LinuxPlayback` 与 `vox_audio_win::playback::WinPlayback` 同形 |
| **播放工厂的装配点共三处**（各自的播放端口内部都拿同一个工厂闭包） | `crates/vox-headless/src/platform/linux.rs::platform`（无屏）、`app/src-tauri/src/platform/linux/audio.rs`（桌面 Linux）、`app/src-tauri/src/platform/win.rs`（桌面 Windows） |
| 端口 trait `Resample` 的三个方法 | `ports::Resample`：`process -> Vec<f32>` / `flush -> Vec<f32>` / `reset` |
| 既有 **5** 条单测全在 `vox_dsp::resample::tests` | `vox_dsp::resample::tests` |

**`Resampler::new` 的完整调用方清单（r2 更正为 17 处；r1 漏了 8 处）**：

| # | 位置 | 性质 |
| --- | --- | --- |
| 1 | `vox_dsp::ports::resample_factory` | 生产代码（工厂） |
| 2–7 | `vox_dsp::resample::tests` 六条 | `resample.rs` 自测（r1 全漏） |
| 8 | `vox_audio_linux::examples/smoke` | example |
| 9 | `vox_audio_alsa::playback::tests` 两处 | `#[cfg(test)]` / `#[ignore]` |
| 10 | `vox_audio_alsa::examples/loopback` | example（**r1 漏**） |
| 11 | `vox_audio_alsa::tests/aloop_roundtrip` | 集成测试（**r1 漏**） |
| 12–16 | `tools/bench-dsp` 五处 | 基准工程（三个测量点 + 两处对比） |

> 复核报告只找到 r1 漏的 6 处（`resample.rs` 自测）；另外 2 处（`loopback.rs`、`aloop_roundtrip.rs`）是本轮自己找到的。
> 结论不变：**`Resampler::new` 的签名与行为一个字都不能动**，新档是加法。

**⇒ R 的关键结论（r2 改写）**：换档只需要换"入口装的那个工厂闭包"，播放端口侧一行都不用动，
`ports.rs` 与 `ResampleFactory` 的签名**一个字都不用改**。但"入口"现在有**两个**（§3.3）。

### 1.6 差分台现状（S4-B B0 落的 15 份金样）

| 事实 | 符号 |
| --- | --- |
| 场景表 15 条，id 与顺序照 `S4-B-OP-CHAIN.md` §6.4 | `pipeline::golden::SCENARIOS`；`crates/vox-core/src/pipeline/golden/` 下 **15** 个 `.txt` |
| 逐字比对用 `include_str!`（缺文件 = 编译错）；`VOX_GOLDEN_REGEN=1` 覆写 | `golden.rs` 的 `verify!` 宏 |
| 清单是由**账本**在 `Start` 时派生的（`Plan::build(&config, &runtime.host_facts())`），夹具注入的是 `HostFacts::all_wired(HostKind::Windows)` | `pipeline::Plan::build`；`pipeline::tests::Rig::build` |
| 15 份金样的 Speak 场景：`output_device = Some("CABLE Input")`、`voice = Some("Tina")`、`monitor_translation = false` ⇒ 角色是 **`virtual_mic`**（`all_wired(Windows)` 里 `Capability::VirtualMic` 开着） | `golden.rs::start_speak`、`speak::composition` 的 `role` 那一段 |
| 假播放汇的 `stats()` **真的报了 `queued_samples`**（= `played.len()`，只被 `flush` 清空），`sample_rate` 写死 48 kHz、`channels` 写死 1 | `pipeline::tests::SpeakerHandle` 的 `PlaybackSink::stats` 实现 |
| **r2 更正 / r3 复核**：`rendered_samples` **不是"测试可拨的计数器"** | `pipeline::tests::Speaker::rendered` 是 `pub(crate) struct Speaker` 里的一个**私有**字段 `AtomicU64`，**全仓库只有一处碰它**——`SpeakerHandle::stats` 里的 `.load()`；没有任何一处写它、没有 setter、没有 `store`。它的唯一读者就是那个 `stats()` 实现 ⇒ 差分台里它**恒为 0**。文档注释那句"测试拨它模拟渲染前进"是**一厢情愿**（注释跑在代码前面了）。**本稿不碰它**（`Worker::poll_playback` 因此在差分台里永远不触发，这是既有事实，不是回归） |
| `LatencyChanged` 的**内容**不进金样（有两个墙钟来源） | `golden.rs` 模块头 |

---

## 2. 半双工门

### 2.1 一句话机制

**译音被推进播放汇的那一刻**，`Worker` 量出"设备里还剩多少毫秒没放"（两个出声口取大），
加上配置的尾巴，换算成**采集率下的样本数**交给门；门在那之前一直照常工作，
从那一拍起**整块不放行**（连 preroll 都不进），计数走完才回到自己那套逻辑。

压制期间如果门本来是开着的，**照手动门的收尾边**发一段零样本尾巴并 `ended = true`（触发服务端断句）。

### 2.2 目标形状

#### 2.2.1 `gate.rs` 新增的类型与字段

```rust
// crates/vox-core/src/gate.rs

/// **半双工门**：回声消除三档里的保底档（概要稿 §2.2 第三行）。
///
/// 外放的译音是我们自己播的，麦克风会把它录回去。硬件 AEC / 软件 AEC 拿不到时，
/// 这一档的做法是**播译音期间关掉上行，播完再压一段尾巴才重新开门**。
/// 代价是**会切断正在说的话**——这是"保底"两个字的实价，界面要照实说。
///
/// 这一档与 [`GateKind`] 的两种门**正交**：`Manual` 与 `Level` 照旧决定"什么时候算在说话"，
/// 本结构只决定"这段时间里一律不许上行"。所以它是 [`GateConfig`] 的一个字段，不是第三种门。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HalfDuplex {
    /// 播放停下之后还要压多久才重新开门（ms）。
    ///
    /// 压的是**房间里的回声尾巴**：最后一声译音出喇叭之后，麦克风还要再听一会儿
    /// 自己那间屋子的反射。默认 300 ms 的来由见 S4-C 稿 §2.3。
    pub tail_ms: u32,
}

impl HalfDuplex {
    /// 缺省尾巴。**改这个值会重录 `speak_48k_speaker_half_duplex` 那份金样**——这是它该干的活。
    pub const DEFAULT_TAIL_MS: u32 = 300;

    pub const fn with_tail_ms(tail_ms: u32) -> Self {
        Self { tail_ms }
    }

    /// 清单 / `Composition::of` 用的缺省实例。
    pub const DEFAULT: Self = Self { tail_ms: Self::DEFAULT_TAIL_MS };
}
```

```rust
// GateConfig 加一格。**serde 两个属性缺一不可**：
//   `default`           —— 老清单里没有这个键，要能读回来（`Composition` 是可反序列化的数据）。
//   `skip_serializing_if` —— 为假时**不写这个键**。漏了它，实测**3 条**逐字比 JSON 的
//     用例会红（见 §0.4 F9，r3 在这里写"那两条"是数错）；既有测试逐字未改是 S4-A/S4-B
//     立下的纪律。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GateConfig {
    pub kind: GateKind,
    pub threshold: f32,
    pub tail_ms: u32,
    pub preroll_ms: u32,
    /// 半双工门。`None` = 不装这一档（**桌面缺省**），门的行为与今天逐字相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_duplex: Option<HalfDuplex>,
}
```

两个预设各自填新格（`GateConfig::MANUAL`、`GateConfig::level`），`impl Default for GateConfig` 走 `MANUAL`：

```rust
pub const MANUAL: Self = Self { kind: GateKind::Manual, threshold: 0.012,
                               tail_ms: 150, preroll_ms: 100, half_duplex: None };
// level() 同理补一行 half_duplex: None
```

**不给 `GateStatus` 加字段**：`GateStatus` 会进 `PipelineStatusDto` / `SnapshotDto` 的线上形状
（`app/src-tauri/src/dto.rs`），加字段等于改前端拿到的 JSON。状态由新 `GateState` 变体表达就够。

```rust
// GateState 加一变体。serde rename_all = "snake_case" ⇒ 线上是 "playback_mute"。
    /// 半双工门：正在外放译音（或还在回声尾巴里），上行整块压住不放行。
    PlaybackMute,
```

#### 2.2.2 门里的状态与新入口

```rust
pub struct ActivationGate {
    config: GateConfig,
    sample_rate: u32,
    active: bool,
    silent_samples: usize,
    prebuffer: std::collections::VecDeque<Vec<f32>>,
    prebuffer_samples: usize,
    external_active: bool,
    /// **半双工压制窗口：还剩多少个采集样本不许上行。`0` = 上行开着。**
    ///
    /// 单位是**采集率下的样本数**，不是毫秒、不是 `bool`：门里没有时钟
    /// （`samples_for_ms` 的既有约定），而差分台要求与墙钟无关的确定性。
    ///
    /// **不变式**：`config.half_duplex.is_none()` ⇒ 这一格恒为 0。
    /// 由两道机制保证：`set_mute_for_ms` 见到 `None` 直接写 0；`set_config` 换门时
    /// 若新配置没有这一档也把它清 0。`process` 的压制分支**还**要自己再判一次
    /// （§2.2.3）——三道里任何一道被后人改坏，另外两道还在。
    mute_samples: usize,
}

impl ActivationGate {
    /// 装满压制窗口：`ms` 毫秒 = `ms × sample_rate / 1000` 个采集样本（**整数截断**，
    /// 与既有的 `ActivationGate::samples_for_ms` 同一口径——**不是上取整**；
    /// 窗口只会被后面的 `set_mute_for_ms` 重新装满，不会自己变长）。
    ///
    /// 门**没装** `half_duplex` 时这一句就是空操作（`mute_samples` 恒 0）——
    /// 桌面档为此**一个整数都不多算**。
    pub fn set_mute_for_ms(&mut self, ms: u64) {
        self.mute_samples = if self.config.half_duplex.is_some() {
            ((self.sample_rate as u64).saturating_mul(ms) / 1000).min(usize::MAX as u64) as usize
        } else {
            0
        };
    }
}
```

#### 2.2.3 `process` 里的压制分支（插在 `match self.config.kind` **之前**）

```rust
    pub fn process(&mut self, samples: &[f32]) -> (Vec<Vec<f32>>, GateStatus) {
        if samples.is_empty() { /* 原样照抄现有的空块早退 */ }

        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();

        // —— 半双工：新增，插在 RMS 这两行之后、原 `match self.config.kind` 之前 ——
        //
        // `&& self.config.half_duplex.is_some()` **不许省**（r2 修正 E1）：它是
        // "没装这一档的会话绝不会被压住"这条不变式的最后一道。少了它，一个
        // `mute_samples` 因为换门参数而残留的窗口会把没装半双工的会话也压住。
        if self.mute_samples > 0 && self.config.half_duplex.is_some() {
            self.mute_samples = self.mute_samples.saturating_sub(samples.len());
            // 压制**刚开始**的这一拍：把 preroll 倒掉，压制期间也不往里存。
            // 理由见 §2.4.3——冲出来的会是我们自己译音的回声。
            self.prebuffer.clear();
            self.prebuffer_samples = 0;
            if self.active {
                // 门本来是开着的：照手动门的收尾边断句（与 `process_manual` 逐条同构，
                // 尾巴用 `config.tail_ms`——**不新增字段**）。
                self.active = false;
                self.silent_samples = 0;
                let tail = vec![0.0f32; self.samples_for_ms(self.config.tail_ms).max(1)];
                return (vec![tail], self.status(GateState::PlaybackMute, rms, false, true));
            }
            return (Vec::new(), self.status(GateState::PlaybackMute, rms, false, false));
        }

        match self.config.kind { /* 一行不动 */ }
    }
```

**热路径代价**（`half_duplex` 为假时也是）：`if self.mute_samples > 0` —— 一次 `usize` 比较，
50 次/秒/腿，**可忽略**。（`&&` 的第二个条件在 `short-circuit` 下**不求值**，桌面档是零代价。）
`prebuffer.clear()` 掉的是 `VecDeque` 的元素（保留已分配的 `Vec`），只在压制期间发生，不在稳态热路径上。

#### 2.2.4 `reset` / `set_config` 的处置（r2 修正 E1）

- `ActivationGate::reset` **照旧清干净**，包括 `mute_samples = 0`——"reset 就是清干净"的既有语义不为例外。
- `ActivationGate::set_config` 在 reset **之后把压制窗口装回去**，但**只在新的配置仍然装着半双工时**：

```rust
    pub fn set_config(&mut self, config: GateConfig) {
        let external_active = self.external_active;
        // 压制窗口只在"换完之后仍然装着这一档"时保留。新配置没有 `half_duplex`
        // ⇒ 有人把这一档关掉了 ⇒ 窗口归零，上行立刻恢复（这正是关它的意思）。
        let mute = if config.half_duplex.is_some() { self.mute_samples } else { 0 };
        self.config = config;
        self.reset();
        self.external_active = external_active;
        self.mute_samples = mute;   // ← 新增这一行
    }
```

  **为什么保留窗口**：换 `tail_ms` / `threshold` / `preroll_ms` 是一条 `Note`
  （`Worker::handle_note` 的 `Note::GateConfig` 臂 → `Chain::set_gate_config`），
  它**必须**在压制中途也不许把上行放开了。有专门的钉子用例（§4.3 的 G1-⑥）。
  **为什么换档时归零**：`half_duplex` 由 `Some` 变 `None` 是"关掉这一档"的语义，
  带着窗口继续压就是"关了还在生效"，自相矛盾。

  **三道机制的关系**（r2 把这条写死，免得后人只留一道）：

  | 机制 | 位置 | 单独成立够不够 |
  | --- | --- | --- |
  | ① `set_mute_for_ms` 见到 `None` 写 0 | `ActivationGate::set_mute_for_ms` | 够——**只要**没人绕过它直接改 `mute_samples` |
  | ② `set_config` 在新配置无半双工时归零 | `ActivationGate::set_config` | 够——**只要**压制分支不单看 `mute_samples` |
  | ③ 压制分支自己再判 `half_duplex.is_some()` | `ActivationGate::process` | 够——**只要**上面两处的语义不被改掉 |
  | 合起来 | | `half_duplex = None` ⇒ 上行**一定**不被压住，且 §2.2.2 字段注释里那条不变式**由构造保证** |

  r1 把 ①② 当成了全部，于是 §2.2.4 正文写"变 None 时分支就进不去了"——**按 r1 的代码这句话是假的**（分支只判 `mute_samples > 0`）。
  r2 加上 ③，这句话才成立。

  **r4 补实测（2026-10-11，复核者逐条做变异）**：这三道的**冗余是故意的**，所以
  **单独去掉任何一道都全绿（0 红）**——另两道还兜得住。要变红必须**同时**去掉 `process` 里那一判
  **与**（① `set_mute_for_ms` 见 `None` 写 0 **或** ② `set_config` 无半双工时归零）之一：
  实测"同时去掉 ①+③ ⇒ **3 条红**"。⇒ **不许**照本稿 r3 那句"去掉 `&& …is_some()` ⇒ G1-⑤ 红"去写期望（原文已作废，见 §0.4 F2）。

### 2.3 尾巴时长：怎么定、谁配

| 问题 | 答案 |
| --- | --- |
| 谁配 | **清单**：`Op::Gate.config.half_duplex.tail_ms`。本机派生时由 `speak::composition` 填 `HalfDuplex::DEFAULT`（§2.5.2）；外部提交的清单（S1 / Agent 面 / 无屏档的配置文件）可以自己填。**不进 `Settings`**（理由见 §2.5.3） |
| 生效范围 | 整条会话（`Chain::build` 时进 `ActivationGate`），**运行期不热改**（改清单 = 新会话 = `Stop`+`Start`，与 INV-1 不冲突，见 §2.5.5） |
| 缺省 300 ms 的来由 | ① 门本来就有两个尾巴量级可参照：`MANUAL.tail_ms = 150`（断句用）、`level().tail_ms = 600`（断句用），半双工尾巴夹在中间；② 面对面轮流说的一轮约 500–800 ms，尾巴每多 100 ms 就少接得上半句；③ 常见办公室的混响 RT60 在 300–500 ms，尾巴短于它就等于没关。**这是一个估算值，不是实测值**——`[未核实]`：真机上到底要多少，取决于箱子与房间，得等有板子时听（见 §7.3 Q1） |
| 与 `config.tail_ms` 的关系 | **两个不同的东西，别混**：`config.tail_ms` 是"门关掉时发多少零样本去触发服务端断句"（`process_manual` 里的 `vec![0.0; …]`），`half_duplex.tail_ms` 是"播完之后压多久才重新开门"。§2.2.3 的收尾边用前者，压制窗口用后者 |

### 2.4 与手动门 / 电平门的组合语义

#### 2.4.1 决策表

| `kind` | 压制中（`mute_samples > 0` 且装了半双工） | 压制刚结束（下一拍） |
| --- | --- | --- |
| `Manual`（按键） | 整块不放行；`external_active` **原样保留**（用户可能还按着） | 若还按着 → `accept()` 上升沿，冲 preroll（此时 preroll 是空的，见 §2.4.3）+ 当前块；**用户压制期间说的话一律丢掉** |
| `Level(0.0)`（Listen 的恒开门） | 整块不放行 | 立刻恢复恒放行（Listen 走进程环回，**根本不该装半双工**，见 §2.5.2） |
| `Level(t > 0)` | 整块不放行（**不评估 RMS**） | 从这一拍起重新按 RMS 判；`silent_samples` / `active` 在压制开始的收尾边已清零，等于从零开始 |

#### 2.4.2 三条正交性保证（都要有用例钉住）

1. **压制分支在 `match self.config.kind` 之前** ⇒ 两种 kind 被同一条路径覆盖，不存在"只在 Manual 上生效"的写法。
2. **压制分支不碰 `external_active`** ⇒ 用户松手/按住的状态跨越压制不变。
3. **压制分支不碰 `config`** ⇒ 换门参数只影响压制**结束之后**的判据，不影响压制本身。

#### 2.4.3 压制期间为什么不填 preroll、且开始时要清空（一条要写进代码注释的决策）

三个选项：

| 选项 | 后果 |
| --- | --- |
| **A. 照常填 preroll**，重新开门时冲出来 | preroll 是 `preroll_ms`（Manual 100 / Level 200）长度的环形缓冲，**冲出来的全是压制窗口末尾的音频**。而压制窗口的**前半段设备还在放译音**（窗口 = 队列里剩下的毫秒 + 尾巴），那几毫秒麦克风听到的是**我们自己的译音**。⇒ 重新开门的第一件事就是把 100–200 ms 的译音回声上传给服务端。这一档存在的意义当场被推翻 |
| **B. 压制开始时清空 + 期间不填**（**本稿采纳**） | 重新开门后听到的第一块是压制窗口**之外**的干净音频。代价：用户起音最多被削掉 `preroll_ms`（≤200 ms）。但他是在**听完译音之后**才开口的，preroll 那点提前量本来也不够用（爆破音在起音后 10–20 ms 内） |
| C. 只在压制**结束时**清空 | 等价于 A（冲出来的还是压制期间的音频），且多一次清空 |

**采纳 B。** 一处 `clear()`、一处"不调 `push_prebuffer`"，两个都是既有方法，**零新增状态**。

### 2.5 清单与能力位怎么表达

#### 2.5.1 为什么是 `GateConfig` 的新字段（不是新 `GateKind`、不是新 `Op`）

| 备选 | 为什么不选 |
| --- | --- |
| 新 `GateKind::HalfDuplex` | 半双工与 `Manual`/`Level` **正交**（§2.4 的决策表）。加成 kind 就得表达 `HalfDuplex × {Manual, Level}` 的叉积：3 个 kind、6 份参数、两条状态机。而且 `Op::kind()` → `Chain::stage_names()` / 计量行名全是按 `Op` 的种类取的，新增 kind 会牵动那一列 |
| 新 `Op::HalfDuplex` 一节 | 它**不改样本**：没有自己的 `process(&[f32]) -> Vec<f32>` 语义，不满足链的契约 C1/C2（`S4-B-OP-CHAIN.md` §2.2）。而且它需要的是"播放参考信号"，那正是 S4-B §2.7 给 AEC 留的口子——同一张嘴开两次不如开一次。⇒ **AEC 与半双工若同时在链上，应共用一个口**（§7.3 Q2） |
| **`GateConfig` 新字段**（**本稿采纳**） | ① `Op::Gate` 已在 `Composition::OP_ORDER` 里，清单不用动；② `Chain::build` 已经**逐字**把 `Op::Gate.config` 喂给 `ActivationGate::new`（S4-B B2 修掉了 D1/D2/D3 那个"清单说了代码不听"的缺陷），本稿只是让这一格里多一个字段被读；③ `ChainStage` **不加变体** ⇒ 执行器的 `match` 一个字不动；④ `GateConfig` 是 `Copy`，热路径代价是 §2.2.3 的一次比较 |

#### 2.5.2 本机派生：什么时候自动开

判据全部落在**清单里已有的两格**上，**不新增能力位**（理由见 §2.5.4）：

```rust
// crates/vox-core/src/pipeline/speak.rs::composition

    // —— 纯代码位移：把构造 `ops` 的那一段整体挪到构造 `out` 的那段之后 ——
    // 两段互不依赖（`ops` 只看 passthrough / config.denoise / config.gate），
    // 而半双工的判据要看 `out` 里有没有"会出声的播放出口"。挪完之后判据**直接读 `out`**，
    // 不另写一份推导（另写一份就是第二份真源）。
    // ⚠️ 挪动是纯位移：不改任何一行逻辑，`OP_ORDER` 管的只是 `ops` 数组的顺序，
    //    与 `out` 在清单里的相对位置无关。

    // r2 修正 E6：`&& !passthrough` **不许省**。
    // 直通模式下 `out` 里照样有一个 `Output::Playback`（role 是 Speaker/Monitor，
    // `source` 是 `EdgeSource::Chain`），`audible` 会为真；但那时推的是**麦克风原声**，
    // 不是译音，把上行关掉就是自噬。`composition` 里 `session` 字段正是
    // `(!passthrough).then(...)`，所以 `!passthrough` ⟺ 有会话，两句话是同一件事。
    let audible = !passthrough
        && out.iter().any(|output| matches!(output,
            Output::Playback { role: PlaybackRole::Speaker | PlaybackRole::Monitor, .. }));

    let mut gate = config.gate;
    // 半双工门（保底档）：只在"译音真的会从房间里放出来"+"无屏档"两条同时成立时开。
    // （"采集是麦克风"这一条**不用写成判据**：这里是 `speak::composition`，采集天生是
    //  `Input::Mic`——抓进程环回的那条腿在 `listen::composition`，本稿一个字都不改。）
    if audible && facts.host == HostKind::LinuxHeadless {
        gate.half_duplex = Some(HalfDuplex::DEFAULT);
    }
    ops.push(Op::Gate { config: gate });
```

三个条件各自的理由：

| 条件 | 为什么必须有 |
| --- | --- |
| `audible`（角色是 `Speaker` 或 `Monitor`，**且不是直通**） | 译音进 `VirtualMic`（VB-CABLE / PipeWire sink）时声音在**线缆里**，不在房间里，麦克风听不到 ⇒ 无回声、无需关门。`Monitor`（回听）例外：它进的是系统默认设备，是**真出声**的。直通那一支见上面的修正注释 |
| 采集是麦克风（**结构性成立，不写成判据**） | 走 `Input::Mic` 才有"房间"这回事。Listen 抓的是**进程环回**（数字源），根本录不到回声 ⇒ `listen::composition` 一个字都不改。在 `speak::composition` 里这一条**不用判**：这一格的采集天生就是 `Input::Mic`（`bits.contains(Capability::Mic)` 才 push），抓环回的那条腿在 `listen::composition` |
| `facts.host == HostKind::LinuxHeadless` | **这一条是纯保守，为了不动已经发货的桌面产品。** 桌面 Linux 用户若没装 VB-CABLE，角色会退成 `Speaker`（`speak::composition` 的 `role` 那一段），此时开了半双工就是**在用户没要求的情况下切断他说话**。概要稿 §3 的原则是"先搭骨架、行为不变，再插新模块"；无屏盒子是本轮目标（§10.9 第 1 条），桌面保持逐字不变。**r3 补核**：`host_ceiling(LinuxHeadless)` = `Mic` + `BackgroundService`，**里面没有 `VirtualMic`** ⇒ 无屏档的 `role` 永远是 `Speaker`，`audible` 只可能被 `!passthrough` 翻掉 |

**调用侧的保险**：`Worker::mark_playing` 只从 `Worker::handle_server_event` 的
`ServerEvent::AudioDelta` 分支调用——直通时那个分支根本不会被走到（直通走 `Worker::pump_passthrough`），
所以即使判据侧漏了，直通也不会自噬。**判据侧与调用侧是两道，各自独立成立**（r1 正文说"调用侧只认译音"是对的，
但判据侧的代码当时并没有 session 条件，E6 修的就是这个不一致）。

**逃生口**：这一格在清单里，外部提交的清单可以自己开或关（`Op::Gate.config.half_duplex`），
S1 / Agent 面 / 无屏档的配置文件都能写，**零代码改动**。所以"无屏档自动开"是**缺省**，不是**唯一入口**。

#### 2.5.3 为什么不进 `Settings`、不做运行期切换

- `GateConfig` 的既有路径就是"设置 → `Runtime::gate_for` → `SessionConfig.gate` → 清单"。
  要让用户能开关，就得在 `Settings` 加一格（`speak.echo_guard: Off | Auto | On`），
  外加 `gate_for` 的一支、三份 i18n、一个界面开关、**以及一条运行期怎么换档的答案**。
- 而换档在运行期是**做不到**的：清单在 `Start` 时由 `Plan::build` 派生成，
  运行期唯一的门控口是 `Note::GateConfig`（INV-1 允许的），它改的是 `ActivationGate.config`——
  **改 `half_duplex` 字段恰好是 `set_config` 能做的、不重建链的事**（§2.2.4）。
  ⇒ 技术上"用户热开关"是零成本的；**本稿不做的理由是它会让半双工在没有耳机/扬声器信息的机器上乱切话**，
  而这一轮的目标机器（无屏盒子）压根不需要这个开关。留作后续：`[未决]` §7.3 Q3。

#### 2.5.4 为什么不新增能力位（以及什么时候该补）

- 能力位模型的三条纪律（`capability.rs` 模块头）：**位 = 事实、由真的把路打开的那段代码负责、
  芯只给 `(位, reason)` 句子不在芯里**。
- "要不要开半双工"这件事**已经**完全由 `HostKind`（一条硬事实）+ 清单形状决定，没有第三个信息源。
  再加一位 `Capability::HalfDuplex`，`host_ceiling` 的四张表、`Capability::ALL` / `HOST` / `id()`、
  JSON schema 的 properties 钉子用例、界面 i18n 全要动，换来的是"把 `HostKind` 再说一遍"。
- **什么时候该补**：AEC 落地时。三档（`hw_aec` / `soft_aec` / 半双工）是**互斥的三选一**，
  选档入口应当是 `Composition::of` 里的一处 `match`，
  那时才值得把"这机器有硬件 AEC"做成位。本稿的 `audible && host == LinuxHeadless`
  到那时**整条替换**掉（§7.3 Q2）。

#### 2.5.5 与 INV-1 的相容性（逐条核）

`S4-B-OP-CHAIN.md` §3.4 的 **INV-1 = 任何 `Note` 都不许重建算子链**。

| 核对项 | 结论 |
| --- | --- |
| 半双工要不要新的 `PipelineCommand`？ | **不要。** 它是每拍从播放汇现算出来的事实，不是用户命令 |
| 要不要新的 `ChainStage` 变体？ | **不要**（§2.5.1）⇒ `Chain::stages` 的长度与形状不变 |
| 要不要新的 `Op` 种类 / 改 `OP_ORDER`？ | **不要** ⇒ `Composition::validate` 的子序列校验不变 |
| 门在热路径上还是装配期建的？ | 装配期（`Chain::build` → `ActivationGate::new`），与今天一致 |
| 用户改门参数（`Note::GateConfig`）会重建链吗？ | 不会，走 `Chain::set_gate_config` → `ActivationGate::set_config`，与今天逐字相同 |
| 断线（`Chain::on_disconnect`）会清掉压制窗口吗？ | 不会——`Chain::on_disconnect` 只清**有流状态的节**（`Denoise` / `Resample`），**门一动不动**（INV-2）。压制窗口是一个 `usize`，重连后接着数，**这正是要的**：断线时译音可能还在播 |

### 2.6 播放状态从哪来（零分配，逐行）

```rust
// crates/vox-core/src/pipeline/mod.rs

/// 某个播放汇里还压着多少毫秒没放出去。没开或率/声道是 0 时给 0。
///
/// **本函数是 `Worker::playback_queue_ms` 的逐字搬移**，
/// 差别只是从"只看主出口"变成"传进来的这一个"，返回值逐字相同 ⇒ 延迟快照一个数都不变。
fn sink_queue_ms(sink: &dyn PlaybackSink) -> u64 {
    let stats = sink.stats();
    if stats.sample_rate == 0 || stats.channels == 0 {
        return 0;
    }
    let frames = stats.queued_samples / stats.channels as usize;
    (frames as u64).saturating_mul(1000) / stats.sample_rate as u64
}

impl Worker {
    fn playback_queue_ms(&self) -> u64 {
        match self.sink.as_ref() {
            Some(sink) => sink_queue_ms(sink.as_ref()),
            None => 0,
        }
    }

    /// **半双工**：译音刚被推进播放汇，把门的压制窗口重新装满。
    ///
    /// 窗口 = 设备里还没放出来的毫秒（`stats().queued_samples` 量出来的真值）
    ///        + `half_duplex.tail_ms`（房间里的回声尾巴）。
    ///
    /// **为什么在 `push` 之后量**：量的是"这一刻起还要播多久"，所以必须是推完之后。
    /// **为什么在同一个 `&mut self` 上**：`handle_server_event` 与 `feed_chain` 同线程（§1.2），
    /// 零原子、零 `Arc`、零 `Cell`。
    /// **两条出声的路取大**：主出口 + 回听（回听也进系统默认设备，是真出声的）。
    ///
    /// 门没装半双工 → 第一句就返回，**桌面档一个整数都不多算**。
    fn mark_playing(&mut self) {
        let Some(tail_ms) = self.chain.echo_guard_tail_ms() else {
            return;
        };
        let mut ms = 0u64;
        if let Some(sink) = self.sink.as_ref() {
            ms = ms.max(sink_queue_ms(sink.as_ref()));
        }
        if let Some(sink) = self.monitor_sink.as_ref() {
            ms = ms.max(sink_queue_ms(sink.as_ref()));
        }
        self.chain
            .set_echo_guard_ms(ms.saturating_add(u64::from(tail_ms)));
    }
}
```

**唯一调用点**（`Worker::handle_server_event` 的 `ServerEvent::AudioDelta` 分支，紧跟两次 `sink.push` 之后）：

```rust
            ServerEvent::AudioDelta { pcm } => {
                let samples = pcm16_to_float(&pcm);
                if let Some(sink) = self.sink.as_mut() {
                    sink.push(&samples);
                }
                if let Some(sink) = self.monitor_sink.as_mut() {
                    sink.push(&samples);
                }
                // ← 新增这一行（紧跟两次 push 之后）。它就是 S4-B §2.7 给 AEC
                //   预留的参考信号口的同一个位置，将来 AEC 落在这两行之间。
                self.mark_playing();
                /* 下面 turn_probe / playback_probe 两段一行不动 */
            }
```

**`Chain` 侧两个新方法**（`chain.rs`，加在 `impl Chain` 的**末尾**，与 `set_gate_active` 同一形状；
`pipeline/mod.rs` 侧不新增任何门相关的口）：

```rust
impl Chain {
    /// 链里装了半双工门吗？`None` = 没装（**含**"链里压根没有门"）。
    /// `Worker::mark_playing` 用它在一句 `if` 里短路掉整段计算。
    pub(crate) fn echo_guard_tail_ms(&self) -> Option<u32> {
        self.stages.iter().find_map(|stage| match stage {
            ChainStage::Gate { gate, .. } => gate.config().half_duplex.map(|hd| hd.tail_ms),
            _ => None,
        })
    }

    /// 装满压制窗口。链里没有门 / 门没装半双工 → **静默忽略**
    /// （与 `Chain::set_gate_active` 同一个口径）。
    pub(crate) fn set_echo_guard_ms(&mut self, ms: u64) {
        for stage in &mut self.stages {
            if let ChainStage::Gate { gate, .. } = stage {
                gate.set_mute_for_ms(ms);
            }
        }
    }
}
```

**残差风险（如实记）**：`stats().queued_samples` 是**推完之后那一刻**的快照。若渲染线程此后卡住，
译音会**比预计更晚**放出来，而压制窗口按采集样本倒数，会**提前**结束 ⇒ 上行早开了一点。
反向（队列溢出丢样本）只会让窗口偏长，**保守安全**。缓解：`mark_playing` 在**每一次** `AudioDelta`
都重新装满，所以只要还有译音在推进，窗口就被续上；真正只靠倒数的只有"最后一次 push 之后"那一段。
`[未核实]` 真机上渲染卡顿多久算一次事件——有板子后用自计时（`Chain::timings` 的 `max_ns`）看
（`op_timings` 读取口 **B3 已落地**，见 §7.3 Q5）。

### 2.7 热路径零新增分配（`RULES.md` #6）逐条记账

| 位置 | 新增分配 | 说明 |
| --- | --- | --- |
| `Worker::mark_playing` | **0** | 只读 `stats()`（返回 `Copy` 的 `PlaybackStats`）、几次 `u64` 比较；两个 `Option::as_ref` 都不克隆 |
| `Chain::echo_guard_tail_ms` / `Chain::set_echo_guard_ms` | **0** | 一个 `find_map` / 一个 `for` + 一次 `if let`；`ActivationGate` 是 `ChainStage` 里的值，`&mut` 就够 |
| `ActivationGate::set_mute_for_ms` | **0** | 一个 `u64` 乘除 |
| `process` 的压制分支 | **0**（稳态） | 分支不进时是"一次 `usize` 比较"（`&&` 的后半截在 `short-circuit` 下不求值）；进去时 `vec![0.0; n]` 与今天 `process_manual` 的收尾边**同一处同一种** |
| `prebuffer.clear()` | **0** | `VecDeque::clear` 只丢元素，已分配的 `Vec` 还在队列的槽里；下一个 `push_prebuffer` 重新 `to_vec()`，分配次数与今天一样 |

**净结论：每块音频新增分配 0。**（B3 的 `timings_are_monotonic_counters_with_no_allocation`
已经把链层的分配计数钉子装好了，本稿这一格是它旁边的一格，验收见 §5.2。）

### 2.8 差分台

#### 2.8.1 15 份金样必须逐字不变 —— 机制逐条核

| 可能让它变的机制 | 为什么不会 |
| --- | --- |
| 清单多了一个键 | ① `skip_serializing_if` ⇒ 为假时**不写**这个键；② 15 份的 `meta` 段与 `event` 段都不含清单（§1.6） |
| 15 份金样的 Speak 会不会自动开半双工 | 它们的角色是 `virtual_mic`（`all_wired(Windows)` 有 `VirtualMic` 位，`output_device = CABLE Input`，`monitor_translation = false`）⇒ `audible == false`；**外加** `host == Windows` ⇒ 即使 `audible` 为真也不开。**两条独立的理由** |
| `GateState` 加了变体 | 老变体的 serde 值一个都没变（`rename_all = "snake_case"` 作用于**名字**，新增变体不改老的） |
| `Worker::mark_playing` 插进了 `AudioDelta` 分支 | 门没装半双工时第一句 `let Some(tail_ms) = … else { return }` 就返回 ⇒ `AudioDelta` 分支的可观察行为逐字不变（`play` 段的 push 计数与指纹不变） |
| `playback_queue_ms` 重构成 `sink_queue_ms` | 同一个式子、同一份数据 ⇒ `LatencyChanged` 的内容不变（而且它本来就**不进**金样，`golden.rs` 模块头） |
| `chain.rs` 加两个方法 | 只加不改；`ChainStage` / `ChainStage::process` / `Chain::run` 一个字不动（B3 的自计时代码也不动） |
| 既有单测 | `gate.rs::tests` 的 5 条用的都是 `GateConfig::MANUAL` / `level(t)`（`half_duplex = None`）⇒ 走的老路径一字未改，**不需要动任何一条** |

**验收**：`cargo test -p vox-core pipeline::golden` 之后
`git status --porcelain crates/vox-core/src/pipeline/golden/` 必须是**空**
（`VOX_GOLDEN_REGEN` 不许出现在验收命令里，[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md) §6.1）。

#### 2.8.2 新增金样场景（**新增，不改旧的**）

| id | 脚本 | 钉的是什么 |
| --- | --- | --- |
| **`speak_48k_speaker_half_duplex`** | ① `Rig::with_facts(48_000, HostFacts::all_wired(HostKind::LinuxHeadless))`；② 设置：Speak / 开麦（门开）/ 降噪开 / 翻译开 / `voice = "Tina"` / `output_device = Some("hw:0,0")`（⇒ 角色退成 `Speaker`，因为 `host_ceiling(LinuxHeadless)` 只有 `Mic` + `BackgroundService`，没有 `VirtualMic`）；③ 灌 3 块 `loud`（应当照常上行，`wire` 段有 3 个 `audio_append`）；④ 推一条 960 个 i16 样本的 `AudioDelta`（**假播放汇把 `stats().sample_rate` 写死成 48 kHz、`channels` 写死 1**，所以 960 个样本 = 20 ms 的队列）；⑤ 再灌 **20 块** `loud`（每块 1920 样本 = 40 ms，`golden.rs::BLOCK_48`）⇒ 压制窗口 = 20 ms 队列 + 300 ms 尾巴 = 320 ms = 15 360 个采集样本 = **正好 8 块** ⇒ 前 8 块一块都不许上行，第 9 块起恢复）；⑥ 收尾 | ① `meta` 段里 `gate=manual+half_duplex(300)`；② 第 ④ 步之后 `event` 段出现 `gate_status` 且 `state` 是 `playback_mute`；③ `wire` 段**逐字**钉住"append 计数与零尾指纹"（`wire` 段**没有块号**）：压制前 `audio_append` 0/1/2（各 640 样本）⇒ 压制开始那一拍 `audio_append 3` 是 **2400 样本的零尾**（`MANUAL.tail_ms = 150` ms × 16 kHz），随后是恢复的 12 块 append 4–15（各 640）；④ 第 ④ 步那一拍发出**一段 150 ms 的零样本**并把重采样缓冲冲掉（`MANUAL.tail_ms = 150`）⇒ `wire` 段因此多出一个 **2400 样本**的 `audio_append`（就是 ③ 里 append 3 那一行，它的长度与恢复的 12 块不同）；⑤ `play` 段不受影响（播放照旧推） |

**为什么这个场景是确定性的（不靠墙钟）**：压制窗口按**采集样本**倒数（§2.2.2），
假播放汇的 `queued_samples` = `played.len()` 在第 ④ 步之后是一个**常数**
（没有渲染线程去取它，见 §1.6 关于 `Speaker::rendered` 的更正），所以窗口 = 一个可手算的常数。
场景脚本里**不许 `clock.advance`**（`TestClock` 只用来驱动 200 ms 的门状态节流，`golden.rs` 模块头那条规矩不变）。

**夹具要加一个构造子**（`pipeline/mod.rs::tests`）：

```rust
    impl Rig {
        /// 按给定的事实建夹具。现有 `Rig::build` 的函数体**一行不改**，
        /// 只是把 `set_host_facts` 的参数从写死的 `all_wired(Windows)` 换成入参。
        ///
        /// 既有 `Rig::build` / `Rig::new` / `Rig::with_rate` 全部转调这里，行为逐字不变
        /// （`pipeline/mod.rs::tests` 稿子基线 `4bc6e8a` 上是 44 条、本轮基线 `708b769` 上是 48 条，
        /// 本次改动对 `mod.rs` 增删为 0，见 §4.3 / §0.4 F7）。
        pub(crate) fn with_facts(rate: u32, facts: HostFacts) -> Self { /* Rig::build 现有的函数体 */ }
    }
```

#### 2.8.3 这份新金样的证据强度（**比旧的弱，如实说**）

15 份旧金样是**重构前录的**，所以"逐字不变"= 差分证据。
新场景是**实现之后录的**，它是**回归钉子**不是差分证据。真正的逻辑证明在那 6 条 `gate.rs` 单测里
（§4.3），它们是对着**规格**写的、不依赖金样。两层加起来才够，**别只拿金样当证明**。

---

## 3. 廉价重采样档

### 3.1 实测（复核者 2026-09-30 真跑过；数字已由复核者在 2026-10-09 重跑复现）

#### 表一：`tools/bench-dsp`（仓库内基准工程）

命令与结果（2026-09-30，`cd tools/bench-dsp && cargo run --release`；工具链在 PATH；
`README.md` 建议的 `taskset -c 2` **没用**——沙箱不放行 `taskset`）。
**复核者 2026-10-09 重跑了一遍**，量级全部复现，个别数字有 5% 出入，下表取复核者的实测值：

| 算子 | ms / 1s 音频 | % 单核 | 极值 |
| --- | --- | --- | --- |
| RNNoise 降噪（`vox_dsp::Denoiser`, 48k） | **2.584**（r1 写 2.724） | 0.258% | — |
| rubato sinc 48k→16k（上行） | 0.563 | 0.056% | 0.552–0.582 |
| rubato sinc 24k→48k（播放侧真路径） | **1.496**（r1 写 1.624） | 0.150% | — |
| rubato sinc 16k→48k | 1.775 | 0.178% | 1.734–1.834 |
| 线性插值 48k→16k（对照） | **0.045** | 0.004% | 0.044–0.049 |
| 线性插值 16k→48k（对照） | 0.120 | 0.012% | 0.120–0.122 |
| 下混 2ch→1ch | 0.037 | 0.004% | — |
| `Blocker` 切块 | 0.008 | 0.001% | — |
| `DropRing` 写+读 | 0.219 | 0.022% | — |
| `duplicate_mono` 铺声道 | 0.033 | 0.003% | — |
| **采集路径合计**（下混+降噪+48k→16k+切块） | **3.332** | **0.333%** | — |
| **播放路径合计**（24k→48k+铺声道+环缓冲） | **1.876** | **0.188%** | — |

这张表与 `AGENTS.md` 里记的"采集链 3.03 ms/s、播放链 1.73 ms/s、RNNoise 2.5 ms/s"同量级，**可互相印证**。
**比值口径不受复核者重跑影响**：升采样方向上线性比 sinc 便宜约 12.5×（表二 12.7×），
而"省下的这一档是播放链总成本的绝大部分"（1.496 / 1.876 = **80%**，r1 用 1.624 算出来的 87% 量级相同）这个结论**不变**。

#### 表二：候选算法对照（**当时临时基准，不在仓库里；复核者未复现这一张**）

- 工程：`/tmp/resample-probe`（独立 crate，path 依赖当时的 `vox-dsp`，**不写仓库任何文件**），
  命令 `cargo run --release --manifest-path /tmp/resample-probe/Cargo.toml`。
- 口径：30 s 音频、20 ms 块（960 样本，三条路径**共用同一块长**）、5 次中位、x86。
  **与表一不是同一套协议**（表一按各自真实块长喂），所以**两张表的绝对值不可互相比较，只有各自表内的比值有意义**。
- `Linear` 与 `Poly` 是当时写的**参考实现**（未调优），`Poly` 的相位量化到 32 相、窗用 Kaiser、
  截止取输出奈奎斯特 × 0.70、目标阻带 70 dB。

| 实现 | 48k→16k | 44.1k→16k | 24k→48k | （ms / 1s 音频） |
| --- | --- | --- | --- | --- |
| rubato sinc（现网） | 0.496 | 0.608 | 2.971 | |
| 线性插值（无滤波） | **0.040** | 0.044 | **0.234** | |
| 多相 FIR P=32×T=24 | 0.400 | 0.436 | 2.398 | |
| 多相 FIR P=32×T=32 | 0.630 | 0.685 | 3.778 | |
| 多相 FIR P=32×T=48 | 0.998 | 1.086 | 5.997 | |

**音质**（48k→16k 降采样，1 s 正弦，取后半段稳态，单位 dBFS）：

| 实现 | 带内 1 kHz | 混叠：10 kHz → 6 kHz | 混叠：14 kHz → 2 kHz |
| --- | --- | --- | --- |
| rubato sinc（现网） | −0.00 | **−120.0** | **−160.3** |
| 线性插值（无滤波） | −0.00 | **−0.0** | **−0.0** |
| 多相 FIR P=32×T=24 | −0.71 | −22.6 | −53.8 |
| 多相 FIR P=32×T=32 | −0.18 | −32.3 | −94.2 |
| 多相 FIR P=32×T=48 | −0.01 | −60.4 | −165.4 |

> **r2 补一条**：复核者**没有**复现这张表（`/tmp/resample-probe` 不在仓库里，也不是 workspace 成员），
> 只复现了表一。所以表二是**单一来源、不可复现**的数字。§3.1.1 的结论里，
> 只有"降采样方向上线性会混叠到 0 dBFS"这一条是**结构性事实**（线性插值没有低通，镜像能量必然全量折进语音带），
> 不依赖具体数字；其余比值（12.7×、多相不划算）依赖这张表，**标 `[未核实]`**（见 §7.3 Q10）。

#### 3.1.1 三条能直接下结论的读法

1. **降采样方向上，线性插值不可用**：10 kHz 的输入被原样折到 6 kHz，**−0.0 dBFS**（= 全幅），
   而 6 kHz 正落在语音带里。麦克风的高频（齿音、风噪）折进来会直接干扰识别。
   **多相 FIR 也不划算**：要做到 −60 dB 得 48 抽头，那是 rubato 的**两倍**开销。
2. **升采样方向上，线性插值便宜约 12 倍**（表二），而它的问题只是**镜像**
   （24 kHz 带限的信号被插值到 48 kHz 后在 24–48 kHz 出现镜像），落在一只小喇叭基本发不出来的频段，
   且译音本来已经过了一趟云端编解码往返。**省下的这一档是播放链总成本的八成**
   （表一：播放链 1.876 里 1.496 是这一档）——**这是 R 这张工单唯一的、也是全部的收益来源**。
3. **真正的大头是降噪，不是重采样**（表一：2.584 / 3.332 = 78%）。
   要在 A53 上省 CPU，**第一刀应该砍降噪，第二刀是"根本不重采样"**（`S4-C-ALSA.md` §5.2 的
   `rate_ladder` 第 1/2 档：设备直接给 16 kHz 采集 / 24 kHz 播放时，`Resampler` 走
   `ResamplerInner::Passthrough`，**零开销**——这是既有行为）。
   ⇒ 本稿在 §3.3 里把这条写成"先做这个，再做 R"。

`[未核实]`：表二的 `Poly` 是未调优的参考实现（每输出样本一次 `Vec::push`、`pos` 用 `f64`、
历史缓冲用 `copy_within`）。一个定长相位累加器 + `process_into` 的版本
**有可能**在 T=32 档追平甚至超过 rubato。本稿**不**据此改结论，因为：即使追平，
它也只值 0.5 ms/s（A53 口径约 2 ms/s），而 R 的收益全在升采样那一侧。

### 3.2 `bench-dsp` 的对照 `Linear` 越界 —— **r2 整段重写（E2）、r3 换成实跑（E15）**

`tools/bench-dsp/src/main.rs` 里的对照用 `Linear` 的 `process` 是这样写的（逐字照抄现状）：

```rust
fn process(&mut self, input: &[f32]) -> Vec<f32> {
    self.buf.extend_from_slice(input);
    let mut out = Vec::new();
    while (self.pos as usize + 1) < self.buf.len() {
        let i = self.pos as usize;
        let frac = (self.pos - i as f64) as f32;
        let a = self.buf[i];
        let b = self.buf[i + 1];
        out.push(a + (b - a) * frac);
        self.pos += self.step;
    }
    let done = self.pos as usize;
    self.buf.copy_within(done.., 0);      // ← 这里会 panic
    self.buf.truncate(self.buf.len() - done);
    self.pos -= done as f64;
    out
}
```

#### 3.2.1 纸上推导（r2 写的，r3 保留）

把 `L = buf.len()`、`p = pos` 代进去推一遍：

1. 循环能继续的条件是 `floor(pos) + 1 < L`，即 `floor(pos) ≤ L-2`，也就是 **`pos < L-1`**；
2. 循环退出后 `done = floor(pos_last + step)`，而 `pos_last < L-1`；
3. `copy_within(done..)` 在 `done > L` 时 panic（`done == L` 是合法的空区间，**不炸**）；
4. 所以炸的条件是 `pos_last + step ≥ L+1`，即 **`step > 2`** 且 **`pos` 恰好落在
   `[L+1-step, L-1)` 这段宽度为 `step-2` 的窗口里**。

⇒ **一句话口径：降采样倍数大于 2 时，`Linear` 有机会越界；不是"步长非整数"的问题。**
`step ≤ 2` 的路径（16k→48k 的 1/3、24k→48k 的 1/2、48k→24k 的 2）**结构上不可能**炸。

#### 3.2.2 实跑（r3 新增，E15）——把上面那句话从推导变成测量

r3 把上面那个 `process` 逐字抄进 `/tmp` 里的三个独立小程序（**不在仓库里、不是 workspace 成员**，
`rustc -O` 直接编），扫"输入率 → 输出率 × 块长"，每个组合连喂 128 块看它炸不炸。
**跑出来的原始结果**：

| 路径 | step | 扫的块长范围 | 炸的次数 | 不炸的块长 |
| --- | --- | --- | --- | --- |
| 48k→16k | 3.00000 | 300–1000（701 个） | **467** | 恰好是 **3 的倍数的那 234 个**（300/303/306/…/999） |
| 44.1k→16k | 2.75625 | 300–1000（701 个） | **693** | 只有 **8** 个：328 / 441 / 554 / 588 / 735 / 769 / 882 / 995 |
| 16k→48k | 0.33333 | 960 | 0 | 全部 |
| 24k→48k | 0.50000 | 960 | 0 | 全部 |
| 48k→24k | 2.00000 | 960 | 0 | 全部 |

panic 原文（`RUST_BACKTRACE=1` 不设时的 core 信息）：

```
thread 'main' panicked at library/core/src/slice/index.rs:1020:55:
range start index 321 out of range for slice of length 320
```

**从这张表能直接下的四条结论**：

1. **`step ≤ 2` 一笔勾销**：三条（0.333 / 0.5 / 2.0）全部不炸，与推导一致。
2. **整数步长照样炸，而且比推导说的更宽**。`step = 3.0` 时"不炸"的充要条件不是
   "`L ≡ 0 (mod 3)`"这么一句静态的话——**每一次调用**的 `L` 都必须是 3 的倍数。
   而 `process` 退出时会留下 `L - done` 个残留样本，**残留会攒**：块长 `N ≡ 1 (mod 3)` 时
   第一轮 `L = N ≡ 1` 侥幸不炸（`done = L-1`，残留 1），第二轮 `L = 1+N ≡ 2` **必炸**。
   ⇒ **实测口径：`step = 3.0` 时，块长必须是 3 的倍数；`320 ≡ 2` 必炸，`319 ≡ 1` 也炸（只是晚一轮），`960 ≡ 0` 不炸。**
3. **`44.1k→16k` 会炸——r1 说对了，只是理由说错了**。701 个块长里炸 693 个，
   活的 8 个是"`pos` 的分数部分恰好躲开窗口"的幸运残差，**没有任何结构性的理由**。
   r2 把它标成 `[未核实]`（"取决于 `pos` 的轨迹"）是过于保守，r3 按实跑结果**结案：会炸**。
4. **`bench-dsp` 今天不炸，是因为它喂的是 960**。`chunks()` 按 `per_block` 切，
   `b48 = mono48.chunks(960)`、`b16 = mono24.chunks(320)`、`b24 = mono24.chunks(480)`——
   48k→16k 的线性对照只喂 `b48`（960，3 的倍数），所以这条 bug **在仓库里是潜伏的，
   从来没被触发过**。这也解释了为什么 `bench-dsp` 一直"跑得过"。

**⇒ 对 R3 的直接要求**：① 修 `Linear::process` 的守卫（循环退出后把 `done` 夹到 `buf.len()`，
或把循环条件改成"`pos + 1 < len` 用同一个取整口径"）；② **补一条 44.1k→16k 的测量**
（今天这个率在 `tools/` 里 `grep 44_100` / `grep 44100` 零命中，**根本没被构造过**），
修完之后它就是"越界条件确实是 `step > 2` 而不是非整数步长"这条说法的**可执行证据**；
③ 补便宜档的行（§3.6）。

> **证据强度自评**：上面的表是 r3 用一个 `/tmp` 小程序跑出来的，**不是仓库里的测试**。
> R3 落地时应该把其中至少一条钉成 `tools/bench-dsp` 里的可执行用例（见 §4.2 R3 的范围），
> 否则这条结论仍然是"某个人的一次运行"。

### 3.3 算法选择

| 档 | 算法 | 用在哪 | 理由 |
| --- | --- | --- | --- |
| `Sinc`（**缺省，今天的行为**） | `rubato::SincFixedIn`，128 点 sinc + cubic 插值 | **降采样（`out < in`）、同率、桌面全部** | 抗混叠是硬的：混叠不可逆，且直接折进语音带（表二 −120 dB vs 线性 −0.0 dB） |
| `Linear`（**便宜档**） | 线性插值，无滤波 | **仅升采样（`out > in`）** | 便宜约 12 倍（表二），代价是镜像而非混叠；镜像落在输出带外，喇叭上听不见 |
| 多相 FIR | —— | **不做** | 降采样方向上不比 rubato 便宜（§3.1.1-1）；升采样方向上便宜的那部分线性已经拿到了，留着只会多一套要维护的系数表与一份"这个阻带够不够"的争论 |

**用"能不能构造出来"表达这条规则，而不是运行时 `if`**（`Option` 让不成立的那一半在类型上就不存在）：

```rust
// crates/vox-dsp/src/resample.rs

/// 构造一档**便宜**的升采样器。
///
/// 返回 `None` 的两种情形，都是"这一档在这条路上不成立"，**不是失败**：
///   * `output_rate <= input_rate` —— 降采样没有抗混叠，线性插值会把高频原样折进
///     语音带（实测 10 kHz → 6 kHz 残留 −0.0 dBFS，见 S4-C 稿 §3.1 表二）。
///     降采样只有 sinc 一档。
///   * `input_rate == output_rate` —— 同率是透传，根本没有插值这回事。
///
/// 本模块刻意不提供静默降级（`RULES.md` #4：不许留假实现），由调用方决定要不要退回 sinc。
pub fn cheap_upsampler(input_rate: u32, output_rate: u32) -> Option<Self> {
    if output_rate <= input_rate {
        return None;
    }
    Some(Self { inner: ResamplerInner::Linear { .. } })
}
```

### 3.4 选档规则（**r2 重写：S4-A W4 之后入口只有两个，且都在共享宿主层里**）

**本稿采纳：装配期由入口选，不新增能力位 / `HostFacts` 字段 / `Settings` 字段 / 运行期切换。**

r1 写的是"无屏 `vox-headless` 装 `cheap_resample_factory()`"。**这条老路今天走不通**——
`Deps.resample`（上行）已经不在无屏档手里了（§1.5）：

| 装配点 | 装的是什么 | 谁能选档 |
| --- | --- | --- |
| `vox_host::entry::HostPorts::deps` | `Deps.resample`（**上行重采样**，桌面与无屏共用这一个出处） | **今天不能选**——函数体里写死 `vox_dsp::ports::resample_factory()`，`HostPorts` 结构体里没有这一格 |
| `vox_headless::platform::linux::platform` / `app/src-tauri::platform::linux::audio` / `app/src-tauri::platform::win` | `PlaybackFactory`（**播放端口内部**的率转换，闭包在各自入口里造） | **能选**，改一个闭包即可 |

⇒ **R2 必须先给 `HostPorts` 加一格重采样工厂**，否则无屏档改不了上行那一半。
这是本轮相对 r1 唯一的结构性新增，也是 R2 从"改两个文件"变成"改四个文件"的原因：

```rust
// crates/vox-host/src/entry.rs

pub struct HostPorts {
    pub kind: fn() -> HostKind,
    pub facts: fn() -> HostFacts,
    pub clock: Arc<dyn Clock>,
    pub secret: SecretBackend,
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    pub registry: Arc<dyn DeviceRegistry>,
    pub startup_notes: fn() -> Vec<String>,
    /// **新增**：上行重采样工厂。桌面传 `vox_dsp::ports::resample_factory()`（逐字等于今天），
    /// 无屏传 §3.5 的 `cheap_resample_factory()`。
    ///
    /// 为什么它是字段而不是 `deps()` 里的一个参数：`deps(self, transport)` 消费掉 `self`，
    /// 而 `Core::assemble(&ports, …)` 之后还要拿同一份 `ports` 装引擎。加参数就得把
    /// `assemble` 的签名也改一遍（它要一路把这个工厂传下去）——那是**两份装配点都要动**的形状。
    /// 做成字段则是**每个装配点自己决定**，与 `capture` / `playback` 两个工厂同一个形状。
    pub resample: ResampleFactory,
}

impl HostPorts {
    pub fn deps(self, transport: TransportFactory) -> Deps {
        Deps {
            transport,
            capture: self.capture,
            playback: self.playback,
            denoise: vox_dsp::ports::denoise_factory(),
            resample: self.resample,   // ← 原来是写死的 resample_factory()
        }
    }
}
```

**桌面侧**（`app/src-tauri/src/lib.rs` 装配 `HostPorts` 的那一处）加一行
`resample: vox_dsp::ports::resample_factory(),`——**桌面行为逐字不变**。

**三条禁令**（避免两处真源）：

| 禁令 | 为什么 |
| --- | --- |
| **不许**在 `HostPorts::deps` 里读环境变量 | 那里是共享层，桌面与无屏共用；读了就变成"桌面也被无屏的环境变量影响" |
| **不许**让桌面也走 `cheap_resample_factory()` | 桌面是发货产品，音质按 sinc 出厂校验（§7.2 第 2 条的镜像代价） |
| **不许**同时留 `VOX_RESAMPLE_QUALITY` 与一个 `Settings` 字段 | 两条真源（`RULES.md` #3） |

**其余备选与结论**：

| 备选 | 结论与理由 |
| --- | --- |
| 能力位（`Capability::CheapDsp` + `host_ceiling(LinuxHeadless)`） | **不做**：与 `HostKind` 重复表达同一件事，而代价是四张上限表 + `ALL`/`HOST`/`id()` + JSON schema 钉子用例 + 界面 i18n 全动。等真有"高档位无屏机器"时再补 |
| `Settings` 字段（用户可强制） | **不做**：运行期换档要么被 INV-1 挡住（不许塞进 `Note`），要么得 `Stop`+`Start` 整轮（用户可感知的重连延迟）。**这正是 `S4-B-OP-CHAIN.md` §9.3 的 O-5，本稿的答复是"不做运行期切换"** |
| 环境变量（**只由无屏入口读**） | **采纳（作为开发/现场 A/B 的口子）**：`VOX_RESAMPLE_QUALITY=sinc\|cheap`，缺省 `cheap`（无屏档）。非法值 → 启动时报错退出，**不许静默回落**（`RULES.md` #4）。这不是占位钩子：它有一个真实的用户（拿着盒子的开发本人），且缺省路径不依赖它 |

**先做什么的次序（§3.1.1-3 的直接结论）**：

```
① 让设备直接给协议要的率（ALSA rate_ladder 第 1/2 档 → Resampler 走 Passthrough）  ← 省 0.5 ms/s 且音质最好
② 播放侧的升采样换便宜档（本稿 R）                                              ← 省 1.5 ms/s（x86 口径）
③ 降噪分档（大纲 §2.3「降噪可关」，已经是清单里的 Op::Denoise）                    ← 省 2.6 ms/s
④ 上行降采样换便宜档                                                            ← **不做**，收益不抵风险
```

### 3.5 `ResampleFactory` 怎么分派（**不改 `ports.rs` 一个方法**）

```rust
// crates/vox-dsp/src/resample.rs

/// 重采样档。**缺省 [`Self::Sinc`]**，与本模块引入前一模一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResampleQuality {
    /// 128 点 sinc + 抗混叠（`rubato::SincFixedIn`）。**降采样唯一的合格档。**
    #[default]
    Sinc,
    /// 线性插值。**只对升采样方向成立**（见 [`Self::cheap_upsampler`] 的文档）。
    Linear,
}

impl Resampler {
    /// **缺省 sinc**。既有调用方一个都不改（`RULES.md` #5：不留 shim）。
    pub fn new(input_rate: u32, output_rate: u32) -> Self { /* 与今天逐字相同 */ }

    /// 便宜档。降采样 / 同率 → `None`（见 [`Self::cheap_upsampler`]）。
    pub fn cheap_upsampler(input_rate: u32, output_rate: u32) -> Option<Self> { /* §3.3 */ }
}
```

```rust
// crates/vox-dsp/src/ports.rs

/// 重采样工厂。**同率时 `Resampler` 内部零开销透传**。逐字照抄今天。
pub fn resample_factory() -> ResampleFactory {
    Box::new(|from, to| Box::new(Resampler::new(from, to)))
}

/// 便宜档工厂：**只在升采样方向**给线性插值，其余情况给 sinc。
///
/// 落回 sinc **不是降级**，是"这一档在这条路上不存在"（降采样需要抗混叠，见 S4-C 稿 §3.3）。
/// 到底是哪一档，由装这个工厂的入口在启动时记一行日志说清楚（vox-dsp 不依赖 tracing，
/// 所以日志在 `vox-headless` / `app/src-tauri` 那一头打）。
pub fn cheap_resample_factory() -> ResampleFactory {
    Box::new(|from, to| {
        match Resampler::cheap_upsampler(from, to) {
            Some(resampler) => Box::new(resampler) as Box<dyn Resample>,
            None => Box::new(Resampler::new(from, to)) as Box<dyn Resample>,
        }
    })
}
```

**`ports::Resample` trait（`crates/vox-core/src/ports.rs`）一个方法都不加。**
理由：加一个 `fn quality(&self)` 的默认方法确实能拿到"这条链用的是哪一档"，
但**这一格在 R1 里没有消费者**：真要说清楚，只有两个地方能说——装工厂的入口（它自己知道）与日志。
为一个"以后可能要用"的读取口改 `pub` trait，按 `RULES.md` #4 属于半成品。
> `[未决]` §7.3 Q4：自计时读取口（B3 的 `Runtime::op_timings`）**已经落地**了，
> 若 `Snapshot` 要显示"本条链的重采样档"，那时才加 `ports::Resample::quality()` 的默认方法，
> 并同步 `vox-dsp` 与各测试替身。**这一格今天仍然没有消费者**，所以仍然不加。

### 3.6 差分台

- **15 份金样一个字都不受影响**：R 的改动**一个文件都不在 `vox-core` 里**（§4.2），
  金样用的是 `pipeline/mod.rs::tests::Rig` 里的假 `Decimate`（假重采样），
  它按整数比抽点、跟 `vox-dsp` 一点关系都没有。
- `vox-dsp` 的 **5** 条既有单测（`vox_dsp::resample::tests`）测的是 `Resampler::new` = sinc 路径，
  **一条都不用改**（`RULES.md` #5：新 API 是加法）。
- **新增 5 条** `vox_dsp::resample::tests`（§4.3）。
- **`tools/bench-dsp` 四件事**（R3，§3.2.1/§3.2.2 指的那两件 + 便宜档 + 证据固化）：
  ① 把对照 `Linear` 的越界守卫修掉；
  ② **补一条 44.1k→16k 的测量**（今天这个率在 `tools/` 里 `grep 44_100` / `grep 44100` 双双零命中，
     **根本没被构造过**），修完之后它就是"越界条件确实是 `step > 2` 而不是非整数步长"这条说法的**可执行证据**；
  ③ 把便宜档加进基准表，让"这个数是怎么来的"留在仓库里而不是留在 `/tmp`；
  ④ 把 §3.2.2 里"`step = 3.0` 扫块长"至少一条钉成 bench-dsp 自己能跑的用例——
     r3 那张表来自 `/tmp` 小程序，**不是仓库里的测试**（§3.2 末尾的自评）。
- **为什么这条 bug 一直在仓库里潜伏**：`chunks()` 的 `b48 = mono48.chunks(960)` / `b16 = …chunks(320)` /
  `b24 = …chunks(480)`，48k→16k 的线性对照只喂 `b48`（960 = 3 的倍数，恰好躲开），
  所以 `bench-dsp` 从来跑得过。**R3 补 44.1k 那一行会当场炸**——这不是"补了一条测量"，
  是"补了一条会 panic 的测量"，守卫必须先修（§3.2.2 的第 4 条结论）。

---

## 4. 改动清单与工单

### 4.1 半双工门（G）

| 动作 | 文件 | 改什么 | owner |
| --- | --- | --- | --- |
| 修改 | `crates/vox-core/src/gate.rs` | ① 新增 `HalfDuplex`（§2.2.1）；② `GateConfig` 加 `half_duplex`（含两个 serde 属性）+ 两个预设各补一行；③ `GateState` 加 `PlaybackMute`；④ `ActivationGate` 加 `mute_samples` 字段 + `set_mute_for_ms`；⑤ `process` 加压制分支（§2.2.3，**含 `&& half_duplex.is_some()`**）；⑥ `reset` 清它、`set_config` 装回它（§2.2.4）；⑦ 模块头补一段"第三种门不是 kind，是正交的一格"；⑧ **新增 10 条单测**（§4.3 的 ①–⑦ ＋ 三条钉可观察面的：`reset_clears_the_mute_window`、`the_playback_mute_state_goes_on_the_wire_as_snake_case`、`half_duplex_is_absent_from_the_gate_config_wire_when_unset`；落地后 `grep -c '#\[test\]' crates/vox-core/src/gate.rs` = **15** = 既有 5 + 新 10），既有 5 条一字不动 | core-dev |
| 修改 | `crates/vox-core/src/pipeline/chain.rs` | ① `Chain::echo_guard_tail_ms` + `Chain::set_echo_guard_ms`（§2.6，**加在 `impl Chain` 末尾**）；② `ChainStage` / `ChainStage::process` / `Chain::run`（含 B3 的自计时代码）/ `on_disconnect` **一个字不动**；③ **新增 2 条单测** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/mod.rs` | ① 拆出自由函数 `sink_queue_ms`（`Worker::playback_queue_ms` 改为逐字转调）；② `Worker::mark_playing` + `handle_server_event` 的 `AudioDelta` 分支插一行；③ `tests::Rig` 加 `with_facts`（`build` 函数体行为不变）；④ **既有 `#[test]` 断言一字不动**（稿子基线 `4bc6e8a` 上是 **44** 条、本轮基线 `708b769` 上是 **48** 条；本次改动对 `mod.rs` 的增删为 **0**，两个数的来由见 §4.3） | core-dev |
| 修改 | `crates/vox-core/src/pipeline/speak.rs` | ① `composition` 里 `ops` 段**纯位移**到 `out` 段之后；② 算 `audible`（**含 `&& !passthrough`**）+ 填 `half_duplex`；③ `use` 补 `HostKind`；④ **新增 2 条单测** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/golden.rs` + 新增 `golden/speak_48k_speaker_half_duplex.txt` | `SCENARIOS` 加一条 + 一个 `#[test]` + 一个 `verify!` 调用 | core-dev |
| 修改 | `crates/vox-core/src/lib.rs` | `pub use gate::{…}` 补 `HalfDuplex` | core-dev |
| 修改 | `app/ui/src/types.ts` | `GateState` 联合类型加一行 `"playback_mute"`（**一行**；§1.4 已核：`live.gates` 只在 `store.tsx` 被写、没有任何组件读 `gate.state`，不用改 i18n 与组件）。⚠️ **与 G3 的 `types.ts` 是同一处** ⇒ 按 `#8` 与 G3 并轨或串行（§4.5 / §4.6） | shell-dev |
| **不动** | `composition.rs` / `capability.rs` / `ports.rs` / `runtime.rs` / `vox-dsp/**` / `app/src-tauri/**` / `vox-host/**` / `vox-audio-*/**` | 见 §4.4 的自证表 | — |

### 4.2 廉价重采样档（R）

| 动作 | 文件 | 改什么 | owner |
| --- | --- | --- | --- |
| 修改 | `crates/vox-dsp/src/resample.rs` | ① `ResampleQuality`；② `ResamplerInner` 加 `Linear` 变体（`step`/`pos`/`buffer` + 复用 `chunk`）；③ `Resampler::cheap_upsampler`；④ `process` / `flush` / `reset` 各加一个 `Linear` 臂（**`Passthrough` 与 `Sinc` 两臂的函数体一字不动**）；⑤ 模块头改写"选 `SincFixedIn` 而非 `FastFixedIn`"那段，补"为什么便宜档只对升采样"；⑥ **新增 5 条单测，既有 5 条一字不动** | core-dev（vox-dsp 归芯侧） |
| 修改 | `crates/vox-dsp/src/ports.rs` | 加 `cheap_resample_factory()`；`resample_factory()` / `denoise_factory()` / 两个 `impl` **一字不动** | core-dev |
| 修改 | **`crates/vox-host/src/entry.rs`** | **`HostPorts` 加 `resample` 字段；`HostPorts::deps` 把写死的 `resample_factory()` 改成读该字段**（§3.4，r2 新增） | shell-dev |
| 修改 | **`app/src-tauri/src/lib.rs`** | **装配 `HostPorts` 的那一处加 `resample: vox_dsp::ports::resample_factory(),`（桌面行为逐字不变，r2 新增）** | shell-dev |
| 修改 | `crates/vox-headless/src/headless.rs` | `host_ports()` 从 `platform::platform()` 的返回值里取 `platform.resample`（上行那一半），并按 `platform.quality` 打一行启动日志；**不自己读环境变量**（读它的是 `platform`，见下一行） | shell-dev |
| 修改 | **`crates/vox-headless/src/platform/mod.rs`** | **档位策略住这儿**：`Platform` 加 `quality: ResampleQuality` 字段 + `RESAMPLE_QUALITY_ENV` / `resample_quality_from_env()` / `resample_factory_for()`（`platform()` 的**签名不变**）。**环境变量仍只在无屏档读**——桌面档（`app/src-tauri`）不碰它（§3.4 的三条禁令） | shell-dev |
| 修改 | `crates/vox-headless/src/platform/linux.rs` | `platform::platform` 读一次 `resample_quality_from_env()`，用同一个决定填两处：`playback` 闭包里那一句（`let resample = …`）与返回的 `Platform.quality`/`Platform.resample`（与上一条**同一个 owner，同一张工单**） | shell-dev |
| 修改 | `tools/bench-dsp/src/main.rs` | ① 修对照 `Linear` 的越界守卫（§3.2.1 的推导 / §3.2.2 的实测）；② **补一条 44.1k→16k 的测量**；③ 加便宜档两行测量；④ **把 §3.2.2 至少一条扫块长的结果钉成可执行用例**（现在那条证据只是 `/tmp` 里的一次运行） | core-dev |
| **不动** | `crates/vox-core/src/ports.rs` / `**/pipeline/**` / `composition.rs` / `capability.rs` / `crates/vox-audio-*/**` / `app/src-tauri/src/platform/**` | 见 §4.4 | — |

### 4.3 测试清单（逐条可数）

| 工单 | 新增用例 | 净增 |
| --- | --- | --- |
| **G1** `gate.rs` | ① `half_duplex_silences_the_uplink_until_the_window_runs_out` ② `a_mute_arriving_mid_sentence_ends_it_with_a_zero_tail` ③ `the_prebuffer_is_cleared_when_the_mute_begins_and_not_refilled` ④ `the_window_ends_after_exactly_its_tail_in_capture_samples` ⑤ `a_gate_without_half_duplex_never_mutes`（**变异验证**：`ActivationGate` 直接调 `set_mute_for_ms` 之后仍不压任何一块——钉 §2.2.3 的 `&&` 那一半）⑥ `set_config_keeps_the_mute_window`（同形于既有 `set_config_keeps_external_active`）**＋ ⑦ `set_config_dropping_half_duplex_ends_the_mute_immediately`**（钉 §2.2.4 的第二道机制与那张三行表）**＋ 三条钉可观察面的**：`reset_clears_the_mute_window`、`the_playback_mute_state_goes_on_the_wire_as_snake_case`（`GateState::PlaybackMute` 的线上值就是 `"playback_mute"`）、`half_duplex_is_absent_from_the_gate_config_wire_when_unset`（两个 serde 属性的钉子） | **+10** |
| **G2** `chain.rs` | ⑧ `a_chain_without_a_half_duplex_gate_ignores_the_echo_guard` ⑨ `the_echo_guard_reaches_the_gate_through_the_manifest` | **+2** |
| **G3** `speak.rs` | ⑩ `speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker`（四条断言，**按 `speak.rs` 里的书写顺序**：① Windows+virtual_mic → `None`；② Windows+普通出声口 → `None`（"桌面档一律不开"）；③ LinuxHeadless+speaker → `Some(HalfDuplex::DEFAULT)`；④ 直通（`translate = false`）→ `None`，钉 E6 的 `&& !passthrough`）⑪ `the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset`（拿一份 `LinuxHeadless` + `Speaker` 的清单去比 `composition.rs::tests::the_manifests_match_the_design_documents_instances` 里那**两份**期望值的形状，钉 `skip_serializing_if`） | **+2** |
| **G4** `golden.rs` | ⑫ `speak_48k_speaker_half_duplex_matches_its_golden` + 1 份 `.txt` | **+1** |
| **G5** `app/ui` | 0（`npm run verify` 里 `tsc` 过即可） | 0 |
| **R1** `vox-dsp` | ⑬ `cheap_upsampler_exists_only_for_upsampling` ⑭ `a_cheap_upsampler_hits_the_target_rate_within_a_few_samples`（流式散碎块 vs 一次性的总量差 ≤ 5，与既有 `streaming_odd_chunks_matches_single_big_chunk` 同口径）⑮ `flush_drains_the_cheap_path` ⑯ `reset_clears_the_cheap_path` ⑰ `the_cheap_path_stays_linear_where_linear_is_better`（同一段 1 kHz，线性档与 sinc 档的带内电平差 ≤ 0.1 dB，**不给它带任何抗混叠的承诺**） | **+5** |
| **R3** `bench-dsp` | 0（这个 crate 没有 `#[test]`；越界修复的证据是"44.1k→16k 与 48k→16k 的两种块长都跑得出来、不 panic"这条**可执行事实**，见 §3.6-②④） | 0 |
| | | **净 +20**（G1 **+10**、G2 +2、G3 +2、G4 +1、R1 +5；G5 与 R3 各 0） |

**既有用例改动数：0。**（唯一有风险的是 `composition.rs::tests::the_manifests_match_the_design_documents_instances`，
它**必须**在 G 之后仍然绿——那正是 `skip_serializing_if` + `host == LinuxHeadless` 两条机制的价值，
它就是"15 份金样不变"在清单侧的对偶证据。）

**既有用例的真实条数**（r3 用命令重数过一遍；复核报告提的"41 实为 39"，我按 `main` @ `4bc6e8a` 数出来对不上那个 39，所以这条**只写我数出来的数、不解释别人的数**）：

| 文件 | 条数 | 数法（`.omp/RULES.md` #10：可复现） |
| --- | --- | --- |
| `crates/vox-core/src/pipeline/mod.rs::tests` | **44**（稿子基线 `main@4bc6e8a`）/ **48**（本轮基线 `main@708b769`） | 同一个命令在两条基线上各跑一次：`grep -c '#\[test\]' crates/vox-core/src/pipeline/mod.rs` = 44 / **48**（44 与 `git show 4bc6e8a:crates/vox-core/src/pipeline/mod.rs \| grep -c '#\[test\]'` 一致，48 是本轮工作区实测）。⚠️ **两个数都是真的，来由不同**：44 是 r3 写稿时 `4bc6e8a` 上的数；本轮基线 `708b769` 高 4 条，中间动过这个文件的 commit 是 `794d8fd`（NET）/ `632ba1a`（NOTICE-I18N）/ `5a318aa` / `708b769`（`git log 4bc6e8a..708b769 -- crates/vox-core/src/pipeline/mod.rs`）。**本次改动对 `mod.rs` 的增删为 0**（G3 只加 `Rig::with_facts` 一个构造子，不加用例）。其中 B3（`85324b9`）加的两条是 `no_note_rebuilds_the_chain` 与 `op_timings_reports_one_row_per_installed_stage`（`git log -S` 逐条查过，都只命中 B3 这一个 commit） |
| `crates/vox-core/src/gate.rs::tests` | **5**（落地前）/ **15**（落地后） | `grep -c '#\[test\]' crates/vox-core/src/gate.rs` = 5 / **15**（既有 5 + G1 新增 10，见 §0.4 F6） |
| `crates/vox-dsp/src/resample.rs::tests` | **5**（落地前）/ **10**（落地后） | `grep -c '#\[test\]' crates/vox-dsp/src/resample.rs` = 5 / **10**（既有 5 + R1 新增 5） |

⚠️ **别把 44 当"净增的分母就用 44"**：第 5.2 节的期望条数是**过滤后**的条数，`pipeline::golden` 这个过滤器还会多命中一条 `goldens_are_deterministic_when_regenerated`（见 E12）。

### 4.4 平行性自证（G ∥ R）

| 文件 | G 碰吗 | R 碰吗 |
| --- | --- | --- |
| `crates/vox-core/src/{gate,lib}.rs` | ✅ | ❌ |
| `crates/vox-core/src/pipeline/{mod,chain,speak,golden}.rs` + `golden/*.txt` | ✅ | ❌ |
| `app/ui/src/types.ts` | ✅ | ❌ |
| `crates/vox-dsp/src/{resample,ports}.rs` | ❌ | ✅ |
| `crates/vox-host/src/entry.rs`、`app/src-tauri/src/lib.rs` | ❌ | ✅ |
| `crates/vox-headless/src/{headless.rs,platform/mod.rs,platform/linux.rs}` | ❌ | ✅ |
| `tools/bench-dsp/src/main.rs` | ❌ | ✅ |
| `crates/vox-core/src/{composition,capability,ports,runtime}.rs` | ❌ | ❌ |
| `crates/vox-audio-*/**`、`app/src-tauri/src/platform/**` | ❌ | ❌ |

⇒ **G ∥ R 完全平行**（零交集）。

**r2 删掉的一段**：r1 用整节论证"G 与 S4-B B3 抢 `chain.rs` / `pipeline/mod.rs`，建议 G 排在 B3 之后"。
**B3 已经合入了**（`op_timings` 读取口、`Chain::timings`、B3 的两条用例都在树上），
这个冲突**不存在了**。取而代之的是 §4.5 的相邻稿子冲突。

### 4.5 与相邻稿子的文件冲突（**r3 整节重写：E14**）

> **先说结论，别再按 r2 的排期派工单**：r2 写这一节时说"在途的两张稿子都还没开工"。
> **这句话现在已经过期了。** 截至本稿 rebase 后的 `main`（`4bc6e8a`）：
>
> | 稿子 | 落地情况（对着 `git log` 核的） |
> | --- | --- |
> | `OVERLAY-CAPTIONS.md` | **施工部分已全部合入**：`690efef`（C1 句子序列 + C4/C5b/C6/C7/C8 + C2 的 `pipeline/mod.rs` 字幕喂入）+ `33927d3`（C5b 行首锚点）。`replace_text` 已删、`update_sentence` / `finish_sentence` 在位、`Worker::push_text` 已是新签名 |
> | `MODEL-TABLE.md` | **主体已合入**：`73e7c58`（M1/M2/M4/M8）+ `6b91a9a` + `0a04e19`（M9 的复核缺陷）+ `7ca4333`（M5/M6）。**只剩 M7（`AliyunRegion::Hk`）没落**——`settings.rs` 里 `AliyunRegion` 仍只有 `Cn` / `Intl`；它只碰 `settings.rs` 与前端，与本稿零交集 |
> | `LANG.md` | 也已合入（`4bc6e8a`），碰 `crates/vox-core/src/{settings,catalog,cloud/protocol}.rs`、`build.rs`、`app/ui/**`——与本稿零交集 |
>
> **⇒ 所以"排在 OVERLAY-CAPTIONS / MODEL-TABLE 之后"这个约束已经消解**：那些文件已经在树上被改过了，
> G / R 落在它们**上面**，不再需要等。真正还剩的约束只有两条：**同一张工单内部的前后置**
> （G3→G4、R1→R2/R3）与**本稿与 AEC 的串行**（§7.3 Q2）。
>
> 下表按 `.omp/RULES.md` #8 逐张列清楚：每张工单碰什么文件、那个文件**现在已经被谁改过**、
> 落点在哪。**"已经被谁改过"这一列的用处**：施工的人 rebase / merge 时要盯的冲突点。

| 工单 | 会碰的文件 | 那个文件现在已经被谁改过（`main` @ `4bc6e8a`） | 落点（按符号名） | 前置 |
| --- | --- | --- | --- | --- |
| **G1** | `crates/vox-core/src/gate.rs` | **`85324b9` 以来零改动**（`git log 85324b9..main -- crates/vox-core/src/gate.rs` 空） | 加 `HalfDuplex` + `GateConfig` 新格 + `mute_samples` + `process` 的分支 | 无 |
| **G2** | `crates/vox-core/src/pipeline/chain.rs` | `85324b9`（B3）之后只被 `73e7c58`（MT-1）碰过一次 | 加在 `impl Chain` 末尾 | G1 |
| **G3** | `crates/vox-core/src/pipeline/mod.rs` | `690efef` / `73e7c58` / `c2d525c` 三个 commit 都改过它（字幕喂入、协议方言、模型名） | 播放遥测段（`Worker::playback_queue_ms` → `sink_queue_ms`、`handle_server_event` 的 `AudioDelta` 臂）与 `tests::Rig::with_facts`——**与字幕段相距很远**，但 `#8` 仍要求不能并行 | G1 |
| **G3** | `crates/vox-core/src/pipeline/speak.rs` | **`85324b9` 以来零改动** | `composition` 里 `ops` 段纯位移 + `audible` 判据 | G1 |
| **G3** | `app/ui/src/types.ts` | `07ab9c9` / `c2c9b5b` / `690efef` / `7ca4333` 四个 commit 轮番改过（目标语言与地区、UI 重设计、`lines_per_track`、`aliyun_endpoint`） | `GateState` 联合类型加一行 `"playback_mute"` | G1 |
| **G4** | `crates/vox-core/src/pipeline/golden.rs` + 新 `.txt` | **`85324b9` 以来零改动** | `SCENARIOS` 加一条 + 一个 `#[test]` + 一个 `verify!` | G3 |
| **G5** | `app/ui/src/types.ts` | 同 G3 那一行 | 与 G3 的 `types.ts` 改动**同一个文件**⇒ 按 `#8`，**G5 与 G3 不许并行**，要么并进 G3，要么排在 G3 之后 | 无 |
| **R1** | `crates/vox-dsp/src/{resample,ports}.rs` | **`85324b9` 以来零改动** | `ResampleQuality` / `Linear` 臂 / `cheap_upsampler` / `cheap_resample_factory` | 无 |
| **R2** | `crates/vox-host/src/entry.rs` | `4694521`（S4-A W5）改过它；`7ca4333` 只在 `vox-host` 下加了 `models.rs`，**没动 `entry.rs`** | `HostPorts` 加 `resample` 字段 + `deps` 改一行 | R1 |
| **R2** | `app/src-tauri/src/lib.rs` | `7ca4333`（MT-2 的 `list_models`）+ `c2d525c`（可覆盖连接地址）改过它 | 装配 `HostPorts` 的那一处加 `resample:` 一行 | R1 |
| **R2** | `crates/vox-headless/src/headless.rs` | `4694521`（S4-A W5）改过它 | `host_ports()` 取 `platform.resample` + 记一行启动日志 | R1 |
| **R2** | `crates/vox-headless/src/platform/mod.rs` | **`85324b9` 以来零改动**（`git log 85324b9..main -- crates/vox-headless/src/platform/mod.rs` 空；**r4 补进清单，见 §0.4 F8**） | 加 `Platform.quality` + `RESAMPLE_QUALITY_ENV` / `resample_quality_from_env` / `resample_factory_for`；**环境变量只在这一档读** | R1 |
| **R2** | `crates/vox-headless/src/platform/linux.rs` | **`85324b9` 以来零改动** | `platform::platform` 读一次档位、填 `playback` 闭包与 `Platform.resample` 两处 | R1 |
| **R3** | `tools/bench-dsp/src/main.rs` | **`85324b9` 以来零改动** | 修守卫 + 补 44.1k→16k + 便宜档 + 固化用例 | R1 |
| **G6 / R4** 文档回填 | `docs/plans/S4-EMBEDDED-REFACTOR.md`、`docs/architecture/DIRECTIONS.md` | **`S4-EMBEDDED-REFACTOR.md` 自 `85324b9` 零改动；`DIRECTIONS.md` 被 `9207895`（AEC 稿）碰过一次**（`git log 85324b9..main --oneline -- <path>`；r4 更正：原写"两个文件都零改动"对 `DIRECTIONS.md` 是**假命题**） | 两张表都在 `docs/` 下，本稿与 AEC 稿将来都会动它们 ⇒ 一律串到最后 | G4 且 R3 |
| **AEC**（稿子 [`S4-C-AEC.md`](S4-C-AEC.md) 已在树上（`9207895`），**算子 `Op::Aec` 未落地**；见 §7.3 Q2） | `pipeline/mod.rs`（`AudioDelta` 分支）、`pipeline/chain.rs`（`Chain::push_playback_reference`）、`pipeline/speak.rs`（三档互斥的选档 `match`）、`composition.rs` / `capability.rs` | ——（**有稿子、无施工 commit 可列**） | 与 G3 / G4 / G2 **同一个区域**：`push_playback_reference` 要插在 `sink.push` **之前**，`mark_playing` 在两次 push **之后**，是相邻两行 | **排在 G4 之后**（本稿的拍板，§7.3 Q2） |

**给 Main 的一句话建议**：**七张工单现在零外部阻塞**（两张在途稿的施工部分都已经在树上）：
`G1` / `G2` / `R1` / `R2` / `R3` 可立即派；`G3` 只需等 `G1`；`G4` 等 `G3`；
`G5` 那一行 `types.ts` **建议直接并进 G3**（同一文件、同一处联合类型，拆开只会多一次 rebase）。

> ⚠️ 表里最后一行（AEC）**不是可派的工单**——它的稿子 [`S4-C-AEC.md`](S4-C-AEC.md) 在树上（`9207895`），
> 但**算子还没落地**（§7.3 Q2）。列在这里只是把
> "G 系列落完之后谁还会再动这几个文件"写清楚，免得施工的人以为 `pipeline/mod.rs` 从此就没人碰了。

### 4.6 工单

| # | 名字 | owner | 独占文件 | 前置 | 排在谁之后 |
| --- | --- | --- | --- | --- | --- |
| **G1** | `gate.rs`：半双工数据结构 + 状态机 + **10 条**单测 | core-dev | `crates/vox-core/src/gate.rs` | 无 | 无 |
| **G2** | `chain.rs`：两个门控口 + 2 条单测 | core-dev | `crates/vox-core/src/pipeline/chain.rs` | G1（`set_mute_for_ms` 要在） | 无 |
| **G3** | `Worker::mark_playing` + `Rig::with_facts` + `speak::composition` 的判据 | core-dev | `pipeline/mod.rs`、`pipeline/speak.rs`、`src/lib.rs` | G1 | 无（`690efef` / `33927d3` 已在树上，见 §4.5） |
| **G4** | 差分台：新场景 + 1 份金样 | core-dev | `pipeline/golden.rs`、`pipeline/golden/speak_48k_speaker_half_duplex.txt` | G3 | 无 |
| **G5** | `app/ui`：`GateState` 加一行 | shell-dev | `app/ui/src/types.ts` | 无 | **与 G3 同一个文件 ⇒ 要么并进 G3，要么排在 G3 之后**（`#8`） |
| **R1** | `vox-dsp` 便宜升采样档 + 新工厂 + 5 条单测 | core-dev | `crates/vox-dsp/src/{resample,ports}.rs` | 无 | 无 |
| **R2** | `HostPorts` 加重采样工厂 + 无屏装便宜档 + 环境变量 + 启动日志 | shell-dev | `crates/vox-host/src/entry.rs`、`app/src-tauri/src/lib.rs`、`crates/vox-headless/src/{headless.rs,platform/mod.rs,platform/linux.rs}` | R1 | 无 |
| **R3** | `bench-dsp`：修 `Linear` 越界守卫 + 补 44.1k→16k + 加便宜档 + 把扫块长的结果钉成用例 | core-dev | `tools/bench-dsp/src/main.rs` | R1 | 无 |
| **G6 / R4** | 文档回填（`S4-EMBEDDED-REFACTOR.md` §3 表这两行标"已落地"、`DIRECTIONS.md` §8 第 16/17 行旁注） | docs-scribe | `docs/plans/S4-EMBEDDED-REFACTOR.md`、`docs/architecture/DIRECTIONS.md` | G4 且 R3 | `LANG.md` 之外的所有 docs 稿子 |

依赖图：

```
G1 ──▶ G2 ──┐
  │          ├──▶ G3 ──▶ G4 ──▶ G6
  └──▶ G3 ───┘                （G5 的 types.ts 一行建议并进 G3）

 R1 ──▶ R2 ──▶ R4
  │
  └──▶ R3 ──▶ R4
```

**工单间文件不重叠**（G1/G2/G3/G4/G5/R1/R2/R3 逐对零交集），符合 `RULES.md` #8
——唯一的例外是 **G3 与 G5 都碰 `app/ui/src/types.ts`**，`#8` 要求它们串行，所以建议并轨。
已合入的相邻稿子落在哪些文件上，见 §4.5。

---

## 5. 验收标准

### 5.1 每张工单收口必跑

```bash
cargo fmt --all --check                                            # 期望：静默
cargo test -p vox-core                                             # 期望：全部 ok，0 failed
cargo test -p vox-dsp                                              # 期望：全部 ok，0 failed
cargo test --workspace                                             # 期望：全部 ok，0 failed
cargo clippy --workspace --all-targets -- -D warnings              # 期望：静默
cd app/ui && npm run verify                                        # G3 / G5 之后：tsc 过
```

**本轮（2026-10-11）七张工单收口时的实测**（Main 跑；数值见 §0.4 的命令块，这里只列结论）：

```bash
cargo fmt --all -- --check                                              # 静默
cargo clippy --workspace --all-targets --exclude vox-audio-alsa -- -D warnings   # 0 warning
cargo test --workspace --exclude vox-audio-alsa                         # 872 passed / 0 failed / 6 ignored
cargo test -p vox-core                                                  # 364 passed / 0 failed
cargo test -p vox-dsp                                                   # 34 passed
```

⚠️ **本机跑不了不含 `--exclude` 的 `cargo test --workspace`**：`alsa-sys` 的 build script 找不到系统
`alsa.pc`（本机只装了 `libasound2t64`、**没有 `libasound2-dev`**）。这是**环境缺口，与本轮改动无关**
（本轮没碰 `crates/vox-audio-alsa/**`）。CI 里对它用 `--exclude`（`.github/workflows/ci-windows.yml`），
`release.yml` 装 `libasound2-dev`。

### 5.2 G 专属（差分台）

```bash
# ① 15 份旧金样逐字不变（验收命令里**绝不含** VOX_GOLDEN_REGEN）
cargo test -p vox-core pipeline::golden                            # 期望（落地后）：**17 passed** = 15 旧场景 + 1 新场景 + 1 确定性自检
git status --porcelain crates/vox-core/src/pipeline/golden         # 期望：**空**（新 .txt 已在上一张工单提交）

# ② 新金样自己是确定的
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
git status --porcelain crates/vox-core/src/pipeline/golden         # 期望：空

# ③ 跨调度稳定
RUST_TEST_THREADS=1 cargo test -p vox-core pipeline::golden        # 期望：17 passed（落地后）
RUST_TEST_THREADS=8 cargo test -p vox-core pipeline::golden        # 期望：17 passed（落地后）
```

**可观察行为**（概要稿 §4 的原文「播放中上行为静音、播放结束 + 尾巴后恢复；去掉该逻辑即红」）怎么测：

| 断言 | 在哪 |
| --- | --- |
| 播放中上行**不出声**（`wire` 段没有新的 `audio_append`） | 新金样的 `wire` 段 |
| 播放结束 + 尾巴之后恢复 | 同上。钉的是 **append 计数与零尾指纹**（金样 `wire` 段没有块号）：`audio_append` 0/1/2 是压制前的 3 块、`audio_append 3` 是 2400 样本的零尾、append 4–15 是恢复的 12 块 |
| 没装半双工的会话绝不被压住（r2 新增，来自 E1） | G1-⑤ + G1-⑦ 两条用例（**注意 F2**：单独去掉任何一道机制都**不会**让它们红，见下一行的实测） |
| 去掉该逻辑即红（**本轮逐条实测过，不是推的**） | ① 删 `ActivationGate::process` 里的压制分支 ⇒ 重跑 `cargo test -p vox-core` **红 8 条**（r3 原写"5 条"，见 §0.4 F1）；② 把 `speak::composition` 里 `facts.host == HostKind::LinuxHeadless` 这一判去掉 ⇒ `speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker` 里**第 ② 条**断言（`"桌面档即使退成普通出声也不许自动切话"`）红；③ 把 `&& !passthrough` 去掉 ⇒ **同一用例第 ④ 条**断言（`"直通推的是原声，关掉上行等于自噬"`）红；④ 单独去掉 `&& config.half_duplex.is_some()` ⇒ **不成立（0 红）**——§2.2.4 的三道机制故意冗余，要红得**同时**去掉两道（实测：①+③ ⇒ **3 条红**，见 §0.4 F2）；⑤ 把 `skip_serializing_if` 去掉 ⇒ **3 条**红：`gate::tests::half_duplex_is_absent_from_the_gate_config_wire_when_unset`、`pipeline::speak::tests::the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset`、`composition::tests::the_manifests_match_the_design_documents_instances`（最后这一条自身有两份期望值，两处都会红） |
| 热路径零新增分配 | 代码论证（§2.7）+ G1-④ 的块数逐块可数；B3 已有的 `timings_are_monotonic_counters_with_no_allocation`（链层）与 G1 的用例（门层）合起来覆盖 |

### 5.3 R 专属

```bash
# ① 便宜档只对升采样存在
cargo test -p vox-dsp cheap_upsampler_exists_only_for_upsampling   # 期望：1 passed

# ② 基准工程里能看见三行，且越界守卫真的修好了（E2 / E15）
cd tools/bench-dsp && cargo run --release                          # 本轮实测：**exit 0**；表格里出现
                                                                      #   "线性插值 44.1k→16k（对照）"
                                                                      #   "线性插值 24k→48k（便宜档）"
                                                                      #   "线性插值 16k→48k（便宜档）"
                                                                      #   三行，且末尾"越界守卫自检"段通过、**进程不 panic**
                                                                      # （守卫没修时 44.1k→16k 那一行必炸：§3.2.2 第 3 条）
```

**可观察行为**：

- 无屏盒子上设 `VOX_RESAMPLE_QUALITY=cheap` 启动，日志里出现一行说清实际档；
  `=sinc` 启动则出现 sinc 那行；`=foo` **启动失败并说清合法取值**（不许静默回落）。
  **本轮没有板子，盒子上那两条未跑。**
- **桌面档零差异**（本轮实测）：`grep -rn cheap_resample_factory crates/ app/ --include=*.rs` 只命中
  `vox-dsp`（定义处）、`vox-headless/src/platform/mod.rs`（选档处）、`vox-host/src/entry.rs`（文档注释里提了一句），
  **`app/` 下一个命中都没有**；桌面 `app/src-tauri/src/lib.rs` 那一处是
  `resample: vox_dsp::ports::resample_factory(),`（**逐字就是引入这一格之前的写法**）。
  （`tools/bench-dsp` 里另有几处，是 R3 的便宜档测量，不在 `crates/` / `app/` 里。）

---

## 6. 明确不做的事（写下是为了不被"顺手做了"）

- **不**给 `ports::Resample` 加 `quality()`（§3.5）。
- **不**给 `GateStatus` 加字段（§2.2.1）。
- **不**新增 `Capability` 位（§2.5.4）、**不**加 `HostFacts` 字段、**不**加 `Settings` 字段（§2.5.3）。
- **不**做多相 FIR 那一档（§3.1.1-1）。
- **不**做半双工的用户开关（§2.5.3）。
- **不**改 `HostPorts::deps` 的签名去传那个新工厂（只加字段，§3.4）。
- **不**让桌面读 `VOX_RESAMPLE_QUALITY`（§3.4 的三条禁令）。
- **不**实现 AEC（`S4-B` §2.7 的参考信号口留给它那张工单；本稿只在 `AudioDelta` 分支留了位置，
  `Worker::mark_playing` 插在两次 `push` 之后，将来 AEC 的 `push_playback_reference` 插在 `push` 之前，
  **两者是相邻两行，串行派**）。

---

## 7. 风险与未决

> `[未核实]` = 本轮没核到 / 核不到 / 核不了。**不许在这些地方编。**

### 7.1 风险

| # | 风险 | 后果 | 处置 |
| --- | --- | --- | --- |
| **GR-1** | **`skip_serializing_if` 漏了** | **3 条**逐字比 JSON 的用例当场红（r4 实测；r3 原写"那一条"）：`gate::tests::half_duplex_is_absent_from_the_gate_config_wire_when_unset`、`pipeline::speak::tests::the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset`、`composition::tests::the_manifests_match_the_design_documents_instances`（最后这一条自身有两份期望值，两处一起红）；违反"既有测试逐字未改" | §2.2.1 把两个 serde 属性写成必带；G3-⑪ 就是专门钉它的 |
| **GR-2** | **`facts.host == LinuxHeadless` 这一条被评审当成"随便加的"删掉** | 桌面 Linux 无虚拟麦的用户**在没要求的情况下被切断说话** | §2.5.2 把两条判据（外加「采集是麦克风」这条结构性前提）的理由逐条写开；`speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker` 里那条 **Windows + 普通出声口 → `None`** 的断言（`"桌面档即使退成普通出声也不许自动切话"`）钉住它（r4 更正：原写"G3-⑩ 的第四条"，序号与代码顺序对不上）；`DIRECTIONS.md` §8 记一条"半双工只在无屏档自动开" |
| **GR-3** | **压制期间清空 preroll 的那条决策被改成"照常填"** | 重新开门第一件事就是把 100–200 ms 的**译音回声**上传，正是这一档要防的事 | §2.4.3 三个选项都列了；G1-③ 钉住"压制开始的这一拍 preroll 已被倒掉" |
| **GR-4** | **尾巴 300 ms 是估算** | 太短 → 回声尾巴漏进上行；太长 → 面对面轮流说接不上话 | §2.3 写清来由与三条依据；新金样把"append 计数与零尾指纹"逐字钉住（没有块号，见 §0.4 F4）⇒ **改这个值必然要重录金样**，这就是它的护栏。等有板子时按 §7.3 Q1 实测 |
| **GR-5** | **`stats().queued_samples` 是"推完之后那一刻"的快照** | 渲染线程随后卡住 ⇒ 压制提前结束，上行早开一点（反向：队列溢出只会让窗口偏长，保守安全） | §2.6 如实记；`mark_playing` 每一次 `AudioDelta` 都重新装满，只靠倒数的只有"最后一次 push 之后"那一段；`[未核实]` 见 Q5 |
| **GR-6**（r2 重写） | **R2 忘了 `HostPorts` 里没有 `resample` 字段这件事** | 照 r1 的方案改 `vox-headless` 会发现改不动；或者有人"顺手"在 `HostPorts::deps` 里读环境变量，把无屏的旋钮泄漏到桌面 | §3.4 把这一步摆在 R2 的最前面，并写明三条禁令；验收 §5.3 最后一条用 `grep` 钉住 |
| **GR-7** | **半双工在"译音不进扬声器"的机器上被打开**（例如用户把 `output_device` 选成蓝牙耳机，但耳机没戴） | 白切话 | 芯**无从知道**耳机戴没戴（Q6）。本轮靠 §2.5.2 的 `audible` 判据 + 无屏档限定兜住；用户开关列为后续 |
| **RR-1** | **线性升采样的镜像在小喇叭上其实听得见**（廉价喇叭的 12–20 kHz 响应并不为零） | 译音发闷/发毛 | `[未核实]`（Q7）：真机上听 A/B。退路是把便宜档的适用范围收窄到"设备率 ≥ 2× 源率"（本轮不收窄，因为镜像的听感靠耳朵判断，靠数字判断不了） |
| **RR-2** | **表二的 `Poly` 参考实现未调优，且该表不可复现**（r2 补） | 若有人据此说"多相 FIR 其实更便宜"，那是拿未调优的、且没人能重跑的实现比的 | §3.1 表二下加了"单一来源、不可复现"的标注，§3.1.1 的比值结论标 `[未核实]`（Q10）；§3.3 明确"不做"，理由不是"调不动"，而是"即使调得动也只值 0.5 ms/s，收益全在升采样那一侧" |
| **RR-3** | **`VOX_RESAMPLE_QUALITY` 变成事实上的长期配置** | 无屏档多一条要长期维护的旋钮 | 它只在无屏入口读，缺省 `cheap`，非法值启动失败。要收进 `Settings` 时**删掉这个环境变量**，不许两条真源并存（`RULES.md` #3） |
| **RR-4**（r2 新增，r3 升级） | **R3 只修了越界、没补 44.1k→16k 那一行** | "`step > 2` 才是条件"这条说法会**退回纸上推导**（r2 的状态）；下次谁动 `Linear` 又炸一次，而且没人知道为什么 | r3 已经把推导**实跑验证**（§3.2.2），但那次运行在 `/tmp` 里、不在仓库 ⇒ R3 的范围把"补 44.1k→16k"列为**独立一条**，并把"把扫块长的结果钉成 bench-dsp 用例"列为第四条（§4.2 / §3.6-④）；验收 §5.3-② 明确要求表格里出现这一行且不 panic |

### 7.2 有意接受的行为差异

1. **无屏档 + 译音走扬声器时，播译音期间上行是关的，用户会被切断。** 这是"保底档"的实价，
   概要稿 §2.2 已经写明。桌面档与直通模式**零差异**。
2. **无屏档的重采样（升采样方向）从 sinc 换成线性**：译音里会出现 24 kHz 以上的镜像。
   已在 §5.3 给出实机验收方法。
3. **清单的线上 JSON 在半双工为真时多一个 `half_duplex` 键**（为假时**没有**这个键）。

### 7.3 未决

| # | 事项 | 状态 |
| --- | --- | --- |
| **Q1** | **`HalfDuplex::DEFAULT_TAIL_MS = 300` 的实机值** | `[未核实]` 估算值，依据见 §2.3。有板子后：录一段"译音 → 立刻接话"的音频，测译音结束后多久麦克风上的自回声低于 −40 dB，取那个值再加 50 ms 余量 |
| **Q2** | **AEC 落地时，半双工的判据与 AEC 的参考信号口怎么合** | `[未决]` **见 AEC 稿** —— [`S4-C-AEC.md`](S4-C-AEC.md)。⚠️ **r4 补注（2026-10-11）**：该稿最初写这段时（基线 `4bc6e8a`）`docs/plans/` 下确实**没有** AEC 稿；**现在它在树上**（`9207895`，含 W0 尖峰 `tools/aec-spike/`）——但**算子 `Op::Aec` 仍未落地**，所以 Q2 还是未决。**以 AEC 稿为准**，本稿下面这段只是**本稿自己的形状**（保留作留痕）：AEC 的 `Chain::push_playback_reference` 插在 `mark_playing` 的**前一行**（都在 `AudioDelta` 分支），三档互斥的选档入口落在 `Composition::of` 的一处 `match` 上，并把 §2.5.2 的 `audible && host == LinuxHeadless` **整条替换**成位驱动。**G4 之后动 `pipeline/mod.rs` 的 AEC 工单必须与本稿串行** |
| **Q3** | **要不要给用户一个半双工开关** | `[未决]` §2.5.3 写了技术成本为零（`set_config` 已经能带），不做是**取舍**不是**做不到**。要做的完整清单：`Settings` 加一格 + `Runtime::gate_for` 一支 + 界面开关 + 三份 i18n + 一条新金样 |
| **Q4** | **`ports::Resample::quality()` 要不要加** | `[未决]` §3.5 给了"没有消费者就不加"的判据。触发条件：`Snapshot` 要显示"本条链的重采样档"。**注意 B3 的自计时读取口已落地，但这一格今天仍然没有消费者** |
| **Q5** | **渲染卡顿多久算一次事件** | `[未核实]` 影响 `GR-5` 的实际暴露面。有板子后用 `Chain::timings` 的 `max_ns` 观察（**读取口 `Runtime::op_timings` B3 已落地**，不再是"等它落地"） |
| **Q6** | **能不能知道"用户戴没戴耳机"** | `[未核实]` 三个端口里都没有这个信息（`PlaybackStats` 只有队列/已渲染/丢弃/设备延迟）。要做就得给 `PlaybackSink` 加一个"设备是不是耳机"的事实 —— 那是 §3.5 那种"没有消费者就先不加"的反面：**有消费者（GR-7）时可以加**，但要单独立项 |
| **Q7** | **线性升采样的镜像在真机喇叭上听感如何** | `[未核实]` 见 `RR-1`。退路已写 |
| **Q8** | **`ActivationGate::process_into`（S4-B 的 X3）落地后，半双工分支的形状要不要跟着改** | `[未决]` 本稿的压制分支是"要么发一块尾巴、要么发空"，`process_into` 化之后 `Vec` 变借用，**收尾边那块的分配会消失**。G1 若赶在 X3 之前，就要接受 X3 之后改一次。**建议 X3 在 G 之后**（X3 会改 `gate.rs` 的 5 条既有单测，越晚越省事） |
| **Q9** | **A53 上的实际倍率** | `[未核实]` 本稿全部性能数字是 **x86** 口径。S4 的纪律是"没有板子之前不写任何'够用'的结论"，本稿照办：§3.1 的数字只用来**比较候选之间**，不用来推算 A53 的绝对性能。转换系数 `[未核实]` |
| **Q10**（r2 新增） | **表二（`/tmp/resample-probe`）的数字能不能复现** | `[未核实]` 那个工程不在仓库里、不是 workspace 成员，**复核者没有复现它**。§3.1.1 的"降采样方向上线性会混叠"是结构性事实（线性插值没有低通），不依赖这张表；但"便宜 12.7×"与"多相 FIR 不划算"依赖它。**要长期依赖这个结论，就得把 `/tmp/resample-probe` 收进 `tools/`**（R3 的可选尾巴，本轮不排）。⚠️ r3 注：`44.1k→16k` 那条**已从这一项里拆出去结案**（§3.2.2 实跑：会炸），本项只剩"表二的比值不可复现"这一件事 |
| **Q11**（r2 新增） | **`Speaker::rendered` 到底要不要有 setter** | `[未决]` §1.6 核实了它"全仓库零写入、无 setter"，而 `Worker::poll_playback` 的 `first_playback` 遥测因此在差分台里永远不触发。它是 B3 之前就有的既存事实，**不是本稿造成的**，本稿不动它。要不要给测试替身补一个 setter，是 `pipeline/mod.rs` 夹具的独立改进，另立项 |

---

## 8. 参考与关联

- 上位稿：[`S4-EMBEDDED-REFACTOR.md`](S4-EMBEDDED-REFACTOR.md) §2.2（回声消除三档）、§2.3（轻量化 + A53 预算）、§3 S4-C 表两行、§4 验收两行
- 方向与裁决：[`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §8 第 15/16/17 行、§10.9 第 1/4/7 条
- 算子链（**已落地的代码，不是预想**）：[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md) §2.2（契约 C1–C6）、§2.7（参考信号口）、§3.4（INV-1/INV-2）、§9.3 O-5（本稿给了答复）、§11 M1–M4。B3（逐节计量 + `Runtime::op_timings`）**已合入本稿核对的 `main`**
- ALSA 后端：[`S4-C-ALSA.md`](S4-C-ALSA.md) §5.2（"能不重采样就不重采样"、三档率阶梯）、§12 M1–M6（**已合入**）
- AEC：**[`S4-C-AEC.md`](S4-C-AEC.md)**（在树上，`9207895`；含 W0 尖峰 `tools/aec-spike/`。**算子未落地**，见 Q2——r4 补注：本稿 r3 写这句时该文件尚未合入）
- **已合入的相邻稿子**（`main` @ `4bc6e8a`）：[`OVERLAY-CAPTIONS.md`](OVERLAY-CAPTIONS.md)（`690efef` / `33927d3`，改过 `pipeline/mod.rs` 的 `Worker::push_text`、`subtitle.rs`、`app/ui/src/types.ts`）、[`MODEL-TABLE.md`](MODEL-TABLE.md)（`73e7c58` / `6b91a9a` / `0a04e19` / `7ca4333`，改过 `cloud/**`、`settings.rs`、`catalog.rs`、`app/ui/src/types.ts`；**只剩 M7 `AliyunRegion::Hk` 未落**）、[`LANG.md`](LANG.md)（`4bc6e8a`）—— 逐张工单的落点见 §4.5
- 清单由来：[`S0-COMPOSITION-MANIFEST.md`](S0-COMPOSITION-MANIFEST.md) §2.1（`Composition` / `Op`）、§2.7（`Plan` 的边界）
- 性能事实：`tools/bench-dsp/`（**复核者实跑过**，§3.1 表一）、`AGENTS.md`「性能事实」一节（采集链 3.03 / 播放链 1.73 ms/s，与实测同量级）
- 临时基准：`/tmp/resample-probe`（**不在仓库里、不可复现**，§3.1 表二、Q10）
- 共享宿主层：[`S4-A-HOST-LAYER.md`](S4-A-HOST-LAYER.md)（`vox_host::entry::HostPorts` 加 `resample` 那一格是本稿结构性新增的一步，§3.4）
- **本稿 r3 的实跑记录**：`/tmp/opencode/lin`（三个独立小程序，逐字复刻 `bench-dsp` 的 `Linear::process` 后扫块长）——**不在仓库里**，结果见 §3.2.2
- 代码：`crates/vox-core/src/{gate,capability,composition,ports,runtime}.rs`、`crates/vox-core/src/pipeline/{mod,chain,speak,listen,golden}.rs`、`crates/vox-dsp/src/{resample,ports}.rs`、`crates/vox-audio-alsa/src/playback.rs`、`crates/vox-host/src/entry.rs`、`crates/vox-headless/src/{headless.rs,platform/mod.rs,platform/linux.rs}`、`app/src-tauri/src/lib.rs`、`app/ui/src/types.ts`

---

## 9. Main 拍板（2026-09-30 首次；2026-10-09 复核后追加第 6 条；2026-10-10 r3 再追加第 7 条）

- **Q1**：`HalfDuplex::DEFAULT_TAIL_MS = 300` 先用，实机校准后再改（改时新增金样，不改旧的）。
- **Q2**：AEC（**见 [`docs/plans/S4-C-AEC.md`](S4-C-AEC.md)**——**在树上**（`9207895`），**算子未落地**；r4 补注：本稿 r3 写这句时该文件尚未合入）与半双工 G 系列串行，G 先落（**G 已落**）。
- **Q3**：本轮**不**给用户半双工开关。
- **Q8**：`ActivationGate::process_into`（S4-B X3）排在 G 系列之后。
- **Q4/Q5/Q6/Q7/Q9**：记账，不在本轮。
- **Q10**：`/tmp/resample-probe` **本轮不收进仓库**（记为后续）。
- **E10–E15（r3 自查出的 6 条事实错误，2026-10-10）**：按 §0.3 的表逐条改。其中 **E14 推翻了上一行的排期**——
  `OVERLAY-CAPTIONS` 与 `MODEL-TABLE` 的施工部分**已合入 `main`**，G / R 七张工单现在零外部阻塞，
  **以 §4.5 / §4.6 为准，下面这段旧排期作废**：

  ~~（2026-10-09 的旧排期，保留以说明改了什么）~~
  ~~`G1` / `G2` / `R1` / `R2` / `R3` 零冲突立即可派；`G3` 排在 OVERLAY-CAPTIONS 之后；`G5` 排在 MODEL-TABLE 之后~~
  ~~⇒ **错在"在途"这个前提**：那两张稿子的施工部分已经在树上（§4.5 的 commit 清单），不存在"排在谁之后"。~~

  **r3 的新排期**：`G1` / `G2` / `R1` / `R2` / `R3` 可立即派；`G3` 等 `G1`；`G4` 等 `G3`；
  `G5`（`types.ts` 那一行）**并进 G3** 或排在它之后；`G6 / R4` 文档回填排最后。

- **本轮收口（2026-10-11）**：G1–G4 与 R1–R3 **七张工单已全部落地**（G5 的 `types.ts` 一行并进 G3，同批落地）。
  门禁实测：`cargo fmt --all -- --check` 静默、`cargo clippy --workspace --all-targets --exclude vox-audio-alsa -- -D warnings`
  **0 warning**、`cargo test --workspace --exclude vox-audio-alsa` **872 passed / 0 failed / 6 ignored**、
  `cargo test -p vox-core` **364 passed**、`cargo test -p vox-dsp` **34 passed**、`tools/bench-dsp` **exit 0**
  （逐条原始命令见 §0.4，口径见 §5.1）。
  ⚠️ **落地不等于把未决项结案**：**Q1**（实机 tail 值）、**Q3**（用户开关）、**Q4**（`ports::Resample::quality()`）、
  **Q5 / Q6 / Q7 / Q10** 仍然**未决 / 未核实**，**Q2** 也照旧要等 AEC 稿 —— 一条都不要标成"已解决"。
