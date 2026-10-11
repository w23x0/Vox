# bench-dsp — Vox 算子 CPU 成本实测台

一次性性能实测台，现在收编进仓库，方便以后回归对比。**只读**依赖 [`crates/vox-dsp`](../../crates/vox-dsp)
与 [`crates/vox-core`](../../crates/vox-core)（`path` 依赖），不写仓库任何文件。

## 它测什么

按真实音频路径的块大小，把合成输入流式喂进各算子，逐格计时。每格 = 一轮 **30 秒**音频的处理耗时，
重复 5 次取中位数。覆盖：

| 类别 | 算子 |
|---|---|
| 采集链 | `AudioChunk::to_mono` 下混 2ch→1ch、`vox_dsp::Denoiser` RNNoise 降噪、`vox_dsp::Resampler` 48k→16k、`Blocker` 切块（对齐 / 非对齐两种喂法） |
| 播放链 | `vox_dsp::Resampler` 24k→48k、`vox_dsp::channels::duplicate_mono` 铺声道、`DropRing` 环缓冲写+读 |
| 对照（普通） | 自写线性插值重采样 48k→16k / 16k→48k（无抗混叠滤波，仅作量级参照） |
| 对照（越界守卫证据） | 自写线性插值 **44.1k→16k**（`step = 2.75625 > 2`，见下面"越界守卫自检"；另一条越界路径是 48k→16k 的 `step = 3.0`，它只喂 960 的倍数所以历史上一直没炸） |
| 便宜档（芯里的真档） | `vox_dsp::ports::cheap_resample_factory()` 装在 **24k→48k** 与 **16k→48k** 两条升采样路径上（走 `ResamplerInner::Linear`；降采样方向它会落回 sinc，见 [`docs/plans/S4-C-GATE-RESAMPLE.md`](../../docs/plans/S4-C-GATE-RESAMPLE.md) §3.3） |

输入是确定性 xorshift64 生成的"语音样噪声"（220 Hz 基音 + 5 个谐波 + 2.5 Hz 音节包络 + 白噪底），
左右声道差 2%，保证每轮跑的数据完全一致、下混不是"抄一列"。

程序还会打印**样本数自检**与**工作量核对**（降噪输入/输出样本、下混样本、环缓冲读写配平、
Blocker 喂入/产出样本），防止"空跑得低分"。

### 越界守卫自检（`linear_guard_sweep()`，跑完基准表自动跑）

对两条**降采样**路径（`48k→16k`、`44.1k→16k`）扫块长 `300..=1000`（701 个，每个连喂 128 块），
用 `catch_unwind` 数"炸过几个块长"。它是对照 `Linear::process` 收尾那段**夹取**的可执行证据
（[`docs/plans/S4-C-GATE-RESAMPLE.md`](../../docs/plans/S4-C-GATE-RESAMPLE.md) §3.2.2 那次 `/tmp` 实验的仓库化版本）：

- **钉住的这一行**：`let done = (self.pos as usize).min(self.buf.len());` 之后才 `copy_within(done.., 0)`。
  `step > 2` 时 `pos` 能一步跨出缓冲（`pos >= len + 1`）——不夹就是 `range start index … out of range for slice`。
- **去掉夹取会怎样**：`48k→16k` 有 **467** 个块长 panic、`44.1k→16k` 有 **693** 个（就是 §3.2.2 那张表的两个数）；
  自检段打印失败原因并 `std::process::exit(1)`。⇒ **基准跑得过 = 守卫在**，`exit 0` 是它给的。
- **为什么只扫这两条**：`step ≤ 2` 的路径（`16k→48k` 的 1/3、`24k→48k` 的 1/2、`48k→24k` 的 2）
  结构上碰不到这一步——两条降采样正是 §3.2.2 表里炸得最多的那两条。

## 怎么跑

```bash
cd tools/bench-dsp
cargo run --release
```

**建议 `taskset` 绑核**，减少调度抖动：

```bash
cd tools/bench-dsp
taskset -c 2 cargo run --release
```

本目录自带 `[workspace]` 空表，是**独立 crate**，不参与仓库 workspace 的 `cargo build/test --workspace`，
也不会被根 `Cargo.toml` 收录；根 `Cargo.toml` / `Cargo.lock` 不受影响。编译产物进本目录 `target/`（已 gitignore）。

## 口径

- **ms / 1s 音频**：中位耗时 ÷ 30。
- **% 单核**：`ms / 1s 音频 ÷ 10`，即该算子在实时处理一路音频时占掉单核的百分比。
- 计时只包住算子处理，算子**构造**在计时区间之外（"处理 1 秒音频"不该摊上一次性构造开销）。
- 采集路径合计 = 下混 + 降噪 + 48k→16k + Blocker(2048)；播放路径合计 = 24k→48k + 铺声道 + DropRing。

## 上次实测（2026-09-21，Ryzen 5 9600X，`--release`）

- 采集路径合计：**3.03 ms / 1s 音频 ≈ 0.3% 单核**
- 播放路径合计：**1.73 ms / 1s 音频 ≈ 0.17% 单核**

结论：全链路 DSP 的 CPU 成本约 **0.5% 单核**，瓶颈在 RNNoise 降噪（≈2.6 ms/s，约 0.26% 单核），
重采样次之。嵌入式/小主板档位有充足余量。

> 收编进仓库后原地复跑复核：采集 **3.08** / 播放 **1.74** ms/s，与上次数字同一波动区间
> （运行间抖动约 ±2%，与 RNNoise 单独耗时波动一致）。

> **2026-10-11 复跑（S4-C「廉价重采样档」R 系列收口）**：表格里新增
> 「线性插值 44.1k→16k（对照）」「线性插值 24k→48k（便宜档）」「线性插值 16k→48k（便宜档）」三行，
> 末尾「越界守卫自检」段**通过**（两条降采样路径一个 panic 都没有），进程 **exit 0**。
