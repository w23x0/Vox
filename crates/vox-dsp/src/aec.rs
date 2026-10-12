//! 回声消除：`sonora-aec3`（WebRTC AEC3 的纯 Rust 移植）的封装。
//!
//! 上游是**频带 / 块级**接口，不是"喂多少样本出多少样本"的流式接口，所以这个文件
//! 大半是"把链上的 20 ms 音频块掰成 AEC3 认的形状"的那层胶水。三级流
//! （S4-C 稿 §6.1，W0 在源码里读出来并实跑确认）：
//!
//! ```text
//! 20 ms 采集块 ──攒──▶ 2 × 10 ms 全带帧
//!                          │ 48 k：ThreeBandFilterBank::analysis → 3 个 16 kHz 频带
//!                          │ 16 k：单频带，拆 / 合是恒等拷贝
//!                          ▼
//!                   num_bands × 160 ──拆──▶ 2 × 80 子帧
//!                          │ FrameBlocker::insert_sub_frame_and_extract_block（80 进 64 出）
//!                          ▼
//!                   64 样本块（4 ms）
//!                          │ 参考路 BlockProcessor::buffer_render
//!                          │ 采集路 BlockProcessor::process_capture（块**就地**换成输出）
//!                          ▼
//!                   BlockFramer::insert_block_and_extract_sub_frame（64 进 80 出）
//!                          ▼
//!                   num_bands × 160 ──synthesis──▶ 全带 10 ms 帧 ──▶ 返回值
//! ```
//!
//! 三条**必须照抄**的形状（S4-C 稿 §6.1 / §6.2，原稿 §5.5 的说法是错的）：
//!
//! 1. **每第 4 次子帧插入会攒出一个余块**，必须 `is_block_available()` 之后
//!    `extract_block()` 抽干；漏了它第 5 次插入就 panic（上游 `frame_blocker.rs` 的
//!    `debug_assert`）。
//! 2. **输入幅度是 int16 域，不是 f32 的 [-1,1]**：AEC3 的激励门限活在 int16 域
//!    （`poor_excitation_render_limit = 150` 折算成 matched filter 的门限
//!    `512 × 150² = 1.152e7`），喂 [-1,1] 连时延都估不出来（W0 实测 ERLE ≈ 0 dB）。
//!    `ports::Aec` 的口径是 f32 的 [-1,1]（与 `Denoise` 一样），**换算在这一层**
//!    （[`S16_SCALE`]，参考与采集两条路都要）。
//! 3. 只驱得动 16000 / 48000 两个采集率（[`AEC_RATES`]），其余率由
//!    `chain.rs::Chain::build` 降级成"装这一节但不消回声"。
//!
//! **分配**（实测口径与出处都钉在 `tests/aec_synthetic.rs` 的 T6 上）：
//!
//! * 所有**状态**缓冲（参考环、攒帧、两个块、framer 的出子帧）在 [`SonoraAec::new`]
//!   里一次分配好；`push_reference` **零分配**；子帧样本在栈上。
//! * `process` 里我们这一层只分配：**返回的那个 `Vec`**（1 次）+ **每帧**一个借用表
//!   （`num_bands` 个小 `Vec`，48 k = 3 次/帧 ⇒ 20 ms 一拍 6 次）——上游那个
//!   `&[Vec<&[f32]>]` 的形状只能这么喂（见 [`set_sub_frame_arg`]：跨帧存 = 自引用，
//!   Miri 判 UB）。
//! * **上游 `sonora-aec3` 自己每 4 ms 块还分配 11 份工作区**
//!   （`echo_remover.rs::EchoRemover::process_capture`：`e` / `y2` / `e2` / `r2` /
//!   `r2_unbounded` / `s2_linear` / 4 个 `FftData` / `subtractor_output`；一个 20 ms
//!   拍 = 5 块 ⇒ 55 次）。实测一拍 **62 = 55（上游）+ 1（返回 Vec）+ 6（借用表）**。
//!   要把它压到"只有返回那一个 `Vec`"得改上游或换实现（S4-C 稿 R2 的退路）。

use sonora::three_band_filter_bank::{
    ThreeBandFilterBank, FULL_BAND_SIZE, NUM_BANDS, SPLIT_BAND_SIZE,
};
use sonora_aec3::block::Block;
use sonora_aec3::block_framer::BlockFramer;
use sonora_aec3::block_processor::BlockProcessor;
use sonora_aec3::common::num_bands_for_rate;
use sonora_aec3::config::EchoCanceller3Config;
use sonora_aec3::frame_blocker::FrameBlocker;
use vox_core::cloud::OUTPUT_SAMPLE_RATE;
use vox_core::pipeline::AEC_RATES;
use vox_core::ports::{Aec, PortError, PortResult};

/// 每个频带每帧的样本数（16 kHz 下的 10 ms，所有率都一样）。
const BAND_FRAME: usize = SPLIT_BAND_SIZE;
/// 子帧长 = 半个频带帧，`FrameBlocker` 的输入单位。
const SUB: usize = BAND_FRAME / 2;
/// 参考环形缓冲的容量（毫秒）。
///
/// §5.3 定的 200 ms：要盖住"播放汇排队 + 声卡缓冲 + 空气"的全部可搜索范围。
/// 它同时是**内存预算的上界**（48 kHz → 9600 样本 ≈ 38 KB，A53 那 512 MB 里
/// 每一 KB 都要算）与"这条路的 ERLE 还值多少"的那条线（W0 §6.4：48 k 三频带下
/// ERLE 随时延单调下滑，200 ms 处 18.51 dB）。具体值 W4 在真板子上实测后再定。
const REF_RING_MS: usize = 200;
/// f32 的 [-1,1] → AEC3 认的 int16 域。见模块头第 2 条。
const S16_SCALE: f32 = 32768.0;

/// 一帧里**一个频带**那 10 ms 的样本数（子帧 = 一半）。
type SubFrame = [f32; SUB];

/// 把借用表填成"每个频带一根指回 `samples` 的引用"，只取前 `num_bands` 个频带。
///
/// **为什么借用表每帧现搭、不跨帧存**：`FrameBlocker` 的入参是 `&[Vec<&[f32]>]`
/// ——最里层是 `Vec` 装的**引用**，带生命周期。把它跨帧存进 `self` 就等于让结构体
/// 借用自己：Rust 的安全写法表达不了；而"扩成 `'static`"那条路（本项目第一版就是
/// 那么写的：`Box` 装样本 + 预先建好 `&'static [f32]`）**在 Miri 的 Stacked Borrows
/// 与 Tree Borrows 两套模型下都是 UB**——`SonoraAec` 构造 / 返回时那次移动会对
/// `Box` 做 `Unique` retag，先建好的借用全部作废，上游 `frame_blocker.rs` 再读就是
/// 许可证之外的访问。所以这里改成**每帧现搭、就地用完就丢**：
/// 48 kHz = 每帧 3 次（每频带一个小 `Vec`）、16 kHz = 1 次，**不是每次插入都搭**
/// （那样是 4 倍）。实测口径钉在 `tests/aec_synthetic.rs` 的 T6。
///
/// 另外：`refs` 只填 `num_bands` 个、也只把这 `num_bands` 个交给上游——
/// 上游 `frame_blocker.rs` 有 `debug_assert_eq!(self.num_bands, sub_frame.len())`，
/// 多给（例如 16 kHz 却给 3 个）在 debug 下直接 panic。
fn set_sub_frame_arg<'s>(
    refs: &mut [Vec<&'s [f32]>],
    samples: &'s [SubFrame],
    from: usize,
    num_bands: usize,
) {
    for band in 0..num_bands {
        refs[band].clear();
        refs[band].push(&samples[from + band][..]);
    }
}

/// 本帧的借用表：每频带一个 `Vec<&[f32]>`（只装 1 个元素）。见 [`set_sub_frame_arg`]。
fn sub_frame_refs() -> [Vec<&'static [f32]>; NUM_BANDS] {
    // `Vec::new()`：**用到哪个频带才分配哪个**。48 kHz（3 个频带都 push）与
    // `with_capacity(1)` 一样是 3 次；16 kHz 只 push 第 0 个 ⇒ 每帧 1 次、
    // 20 ms 一拍 2 次（不再白建那两个从不使用的）。
    std::array::from_fn(|_| Vec::new())
}

/// 参考环形缓冲：FIFO，**满了丢最旧**（与播放汇的 `vox_dsp::ring::DropRing`
/// 同一个口径：宁可丢，也不阻塞调用方、不分配）。
///
/// 为什么要有它：`push_reference` 收到的参考是**播放汇刚拿到的那一段**
/// （服务端 `AudioDelta` 一拍一段，颗粒度由网络决定），而 `process` 要按 **10 ms
/// 一帧**喂 AEC3。两边的颗粒度、相位都对不上（S4-C 稿 T7 钉的就是这条），
/// 环把这层解耦掉：写多少、什么时候写，与读的节拍无关。
///
/// 读指针的滞后量等于"还没被送进 AEC3 的那一段"——也就是 §5.3 说的
/// `ref_pushed − rendered_samples`（播放汇的排队量）。`ports::Aec` 的面上读不到
/// `PlaybackStats`（§5.2 定的形状就两个方法），而 W0 §6.4 实测 AEC3 的内建时延
/// 估计器自己就能跟上 0–400 ms 的真实时延（逐档估计值与真值吻合、误差 ≤ 1 格
/// 4 ms），外面再喂一个 D0 几乎不给 ERLE 收益（48 k、400 ms 处 12.47 对 12.66 dB）。
/// 所以这里**不做 D0 那一层**：`set_audio_buffer_delay()` 是"省 CPU 档"的事
/// （W0：外部时延估计器 16 k 省约一半、48 k 省约四分之一），要等 W4 在真板子上
/// 量出"排队量折合的等效 D"再开，现在开就是拿一个没测量过的常量换一点 CPU。
struct RefRing {
    /// 上一次 `read_frame` 是否**真的**取满（欠载补零时为 false）。
    hit: bool,
    buf: Vec<f32>,
    /// 累计写入 / 已取走的样本数。`write_abs - read_abs` 是环里的存量。
    read_abs: u64,
    write_abs: u64,
}

impl RefRing {
    fn new(samples: usize) -> Self {
        Self {
            buf: vec![0.0; samples],
            read_abs: 0,
            write_abs: 0,
            hit: false,
        }
    }

    fn clear(&mut self) {
        self.read_abs = 0;
        self.write_abs = 0;
    }

    fn push(&mut self, sample: f32) {
        let cap = self.buf.len() as u64;
        if self.write_abs - self.read_abs >= cap {
            // 满：丢最旧的，读指针跟着往前挪一格。
            self.read_abs = self.write_abs - cap + 1;
        }
        let at = (self.write_abs % cap) as usize;
        self.buf[at] = sample;
        self.write_abs += 1;
    }

    /// 取一帧 10 ms。**有多少取多少，缺的补零**，读指针只按真正取到的推进。
    ///
    /// 补零不是"糊弄"：参考还没推到这一格，说明那一刻外放的参考按我们知道的
    /// 就是静音（最开始那几拍恒如此——还没有任何译音被播出去）。缺到的那一段
    /// 不吞掉，于是参考的时间轴只会**滞后**，不会丢内容；真的积压到满由
    /// [`RefRing::push`] 丢最旧兜底。
    /// `hit` 供采集路判"这一拍到底有没有参考可消"：欠载 ⇒ `false` ⇒
    /// `run_frame` 跳过 `buffer_render`（见那里的长注与 T18）。
    fn read_frame(&mut self, out: &mut [f32]) {
        let cap = self.buf.len();
        let available = (self.write_abs - self.read_abs) as usize;
        let take = available.min(out.len());
        let mut at = (self.read_abs % cap as u64) as usize;
        for slot in out[..take].iter_mut() {
            *slot = self.buf[at];
            at += 1;
            if at == cap {
                at = 0;
            }
        }
        out[take..].fill(0.0);
        self.read_abs += take as u64;
        self.hit = take == out.len();
    }
}

/// 播放率 → 采集率的**线性插值**升采样。
///
/// §5.4 定的口径：参考是**我们自己**播出去的 TTS 译音，随后还要过云端那次编码
/// 往返（编解码器本身就是带限的），而 sinc（`resample.rs` 那只）在每一句话上跑、
/// 在 Cortex-A53 上是明确的净亏。W0 实测线性插值只掉 1.1–2.4 dB（§6.4），
/// 够不到原稿设的 3 dB 判据 ⇒ **R6 关闭，维持线性插值**。
///
/// 零分配：输出直接写进 [`RefRing`]，不建中间缓冲。同率时走直通，连插值那一个
/// 样本的延迟都不要。
struct LinearUpsampler {
    /// 每出一个输出样本，输入位置前进多少（`in_rate / out_rate`）。
    inc: f64,
    /// 下一个输出点落在"上一个输入样本 → 当前输入样本"之间的位置，`[0, 1)`。
    pos: f64,
    /// 上一个输入样本（插值的左端点）。
    prev: f32,
    same_rate: bool,
}

impl LinearUpsampler {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            inc: in_rate as f64 / out_rate as f64,
            // 相位从 1.0 起 = "这一格（上一个输入样本 → 当前输入样本之间）没有输出点"：
            // 于是**第一个输入样本不产出**，第二个输入样本一上来就产出第一个输出点
            // （正好落在输入样本自己身上）。这样输出栅格就是**标准的**
            // `out[m] = lerp(src[⌊m·in/out⌋], src[⌊m·in/out⌋+1], …)`：`out[0] == src[0]`。
            // 从 0.0 起会多出一个"流开始之前的假样本"（值 0），等于平白给参考加一个
            // 输入样本的延迟——那是**另一条**栅格，与别的重采样器（和真设备的 DAC 前面
            // 那一段）对不齐，AEC 的参考与回声就会差一个亚带样本、消不干净。
            pos: 1.0,
            prev: 0.0,
            same_rate: in_rate == out_rate,
        }
    }

    fn clear(&mut self) {
        self.pos = 1.0;
        self.prev = 0.0;
    }

    fn push_into(&mut self, samples: &[f32], ring: &mut RefRing) {
        if self.same_rate {
            for &x in samples {
                ring.push(x * S16_SCALE);
            }
            return;
        }
        for &x in samples {
            while self.pos < 1.0 {
                let y = self.prev + (x - self.prev) * self.pos as f32;
                ring.push(y * S16_SCALE);
                self.pos += self.inc;
            }
            self.pos -= 1.0;
            self.prev = x;
        }
    }
}

/// AEC3 那半边：算法本体 + 两级拆 / 拼帧 + 48 k 的滤波器组。
///
/// 单独成结构是为了 [`Aec::reset`] 能整只换掉它：上游没有公开的 `reset`，
/// 而 AEC 的状态（自适应滤波器的历史回声路径）跨会话无效（契约 C6）。
struct Engine {
    /// 48 k（3 频带）才要滤波器组；16 k 单频带的拆 / 合是恒等拷贝。
    split: Option<(ThreeBandFilterBank, ThreeBandFilterBank)>,
    render_blocker: FrameBlocker,
    capture_blocker: FrameBlocker,
    framer: BlockFramer,
    proc: BlockProcessor,
    render_block: Block,
    capture_block: Block,
}

impl Engine {
    fn new(rate: usize, num_bands: usize) -> Self {
        let mut config = EchoCanceller3Config::default();
        let _ = config.validate();
        Self {
            split: (num_bands > 1)
                .then(|| (ThreeBandFilterBank::new(), ThreeBandFilterBank::new())),
            render_blocker: FrameBlocker::new(num_bands, 1),
            capture_blocker: FrameBlocker::new(num_bands, 1),
            framer: BlockFramer::new(num_bands, 1),
            // 缺省 = **内建**时延估计器（W1-g：W0 实测两条路 ERLE 无差别，
            // `set_audio_buffer_delay()` 留给"省 CPU 档"）。
            proc: BlockProcessor::new(&config, rate, 1, 1),
            render_block: Block::new(num_bands, 1),
            capture_block: Block::new(num_bands, 1),
        }
    }

    /// 全带 10 ms 帧 → `num_bands` × 160 样本。
    fn split_bands(
        &mut self,
        full: &[f32; FULL_BAND_SIZE],
        bands: &mut [[f32; BAND_FRAME]; NUM_BANDS],
    ) {
        match self.split.as_mut() {
            Some((analysis, _)) => analysis.analysis(full, bands),
            None => bands[0].copy_from_slice(&full[..BAND_FRAME]),
        }
    }

    /// `num_bands` × 160 样本 → 全带 10 ms 帧。
    fn merge_bands(&mut self, bands: &[[f32; BAND_FRAME]; NUM_BANDS], out: &mut [f32]) {
        match self.split.as_mut() {
            Some((_, synthesis)) => {
                let mut full = [0.0f32; FULL_BAND_SIZE];
                synthesis.synthesis(bands, &mut full);
                out.copy_from_slice(&full[..out.len()]);
            }
            None => out.copy_from_slice(&bands[0][..out.len()]),
        }
    }

    /// 送一帧 10 ms 全带参考（播放侧）。
    ///
    /// `sub` 是本帧的子帧样本存储（**进来时先整块写满、之后只读**，这样借用表
    /// 只建一次就能撑过两次插入）。
    fn push_render<'s>(
        &mut self,
        num_bands: usize,
        full: &[f32; FULL_BAND_SIZE],
        bands: &mut [[f32; BAND_FRAME]; NUM_BANDS],
        sub: &'s mut [SubFrame],
        refs: &mut [Vec<&'s [f32]>],
    ) {
        self.split_bands(full, bands);
        // 两个半帧一次搬完（此后 `sub` 只读）。
        for half in 0..2 {
            let offset = half * SUB;
            for band in 0..num_bands {
                sub[half * NUM_BANDS + band].copy_from_slice(&bands[band][offset..offset + SUB]);
            }
        }
        let samples: &'s [SubFrame] = sub;
        for half in 0..2 {
            set_sub_frame_arg(refs, samples, half * NUM_BANDS, num_bands);
            self.render_blocker
                .insert_sub_frame_and_extract_block(&refs[..num_bands], &mut self.render_block);
            self.proc.buffer_render(&self.render_block);
        }
        // 第 4 次插入攒出的余块必须抽干，否则第 5 次插入越界 panic。
        if self.render_blocker.is_block_available() {
            self.render_blocker.extract_block(&mut self.render_block);
            self.proc.buffer_render(&self.render_block);
        }
    }

    /// 送一帧 10 ms 全带采集（上行侧），消完回声的 10 ms 全带结果写进 `out`。
    /// `sub` / `refs` 的约定同 [`Engine::push_render`]。
    #[allow(clippy::too_many_arguments, reason = "本帧的临时区都从外面借进来")]
    fn push_capture<'s>(
        &mut self,
        num_bands: usize,
        full: &[f32; FULL_BAND_SIZE],
        bands: &mut [[f32; BAND_FRAME]; NUM_BANDS],
        out_bands: &mut [[f32; BAND_FRAME]; NUM_BANDS],
        sub: &'s mut [SubFrame],
        refs: &mut [Vec<&'s [f32]>],
        framer_out: &mut [Vec<Vec<f32>>],
        out: &mut [f32],
    ) {
        self.split_bands(full, bands);
        for half in 0..2 {
            let offset = half * SUB;
            for band in 0..num_bands {
                sub[half * NUM_BANDS + band].copy_from_slice(&bands[band][offset..offset + SUB]);
            }
        }
        let samples: &'s [SubFrame] = sub;
        for half in 0..2 {
            let offset = half * SUB;
            set_sub_frame_arg(refs, samples, half * NUM_BANDS, num_bands);
            self.capture_blocker
                .insert_sub_frame_and_extract_block(&refs[..num_bands], &mut self.capture_block);
            self.proc
                .process_capture(false, false, None, &mut self.capture_block);
            self.framer
                .insert_block_and_extract_sub_frame(&self.capture_block, framer_out);
            for band in 0..num_bands {
                out_bands[band][offset..offset + SUB].copy_from_slice(&framer_out[band][0]);
            }
        }
        // 余块：先处理，再**只插不抽**——这次插进去的子帧下一帧才吐出来。
        if self.capture_blocker.is_block_available() {
            self.capture_blocker.extract_block(&mut self.capture_block);
            self.proc
                .process_capture(false, false, None, &mut self.capture_block);
            self.framer.insert_block(&self.capture_block);
        }
        self.merge_bands(out_bands, out);
        // 出口换算回 f32 的 [-1,1]：上面整条路径都在 int16 域（模块头第 2 条），
        // 而 `ports::Aec::process` 的返回值是 [-1,1] 域，与进来时同一个口径。
        for sample in out.iter_mut() {
            *sample /= S16_SCALE;
        }
    }
}

/// 回声消除器：`ports::Aec` 的 `sonora-aec3` 实现。
///
/// 装配期由 [`crate::ports::aec_factory`] 按采集率造一台；链上只有 `Op::Aec`
/// 那一节认它（`chain.rs::ChainStage::Aec`）。
/// 48 kHz 时内部走 3 频带（`ThreeBandFilterBank`），16 kHz 时单频带；
/// 参考按 [`OUTPUT_SAMPLE_RATE`]（今天 24 kHz）进来，内部线性插值到采集率。
pub struct SonoraAec {
    /// 采集率。也是 [`Aec::process`] 进出口的率（契约 C4：这一节不换率）。
    rate: usize,
    num_bands: usize,
    /// 一帧（10 ms）多少个样本 = `rate / 100`。
    frame: usize,
    engine: Engine,
    /// 参考环形缓冲（int16 域、采集率）。
    ring: RefRing,
    /// 播放率 → 采集率。
    upsampler: LinearUpsampler,
    /// 上行的攒帧缓冲（int16 域）。攒满 `frame` 就喂一帧。
    capture_buf: [f32; FULL_BAND_SIZE],
    capture_len: usize,
    /// 本帧的参考（全带、int16 域），从环里取。
    reference: [f32; FULL_BAND_SIZE],
    render_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    capture_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    out_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    /// `BlockFramer` 的出子帧（建链期一次分配，够 `num_bands` × 1 声道）。
    framer_out: Vec<Vec<Vec<f32>>>,
}

impl SonoraAec {
    /// 建一台采集率为 `rate` 的回声消除器。**所有缓冲在这里一次分配**。
    ///
    /// 采集率不在 [`AEC_RATES`] 里直接报 `Err`——链上（`Chain::build`）
    /// 已经按同一份常量降级过一次，这里只是实现自检，不让"喂一个 AEC3 驱不动
    /// 的率"变成运行期 panic。
    pub fn new(rate: u32) -> PortResult<Self> {
        if !AEC_RATES.contains(&rate) {
            return Err(PortError::new(format!(
                "回声消除只支持 {AEC_RATES:?} Hz 的采集率，收到 {rate} Hz"
            )));
        }
        let rate = rate as usize;
        let num_bands = num_bands_for_rate(rate);
        // 环要按采集率折毫秒。20 ms 一块进来，200 ms 的环够盖播放汇的排队。
        let ring_samples = REF_RING_MS * rate / 1000;
        Ok(Self {
            rate,
            num_bands,
            frame: rate / 100,
            engine: Engine::new(rate, num_bands),
            ring: RefRing::new(ring_samples),
            upsampler: LinearUpsampler::new(OUTPUT_SAMPLE_RATE, rate as u32),
            capture_buf: [0.0; FULL_BAND_SIZE],
            capture_len: 0,
            reference: [0.0; FULL_BAND_SIZE],
            render_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            capture_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            out_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            framer_out: vec![vec![vec![0.0; SUB]; 1]; num_bands],
        })
    }

    /// 跑一帧 10 ms：参考与采集各取一帧全带，消完的回声写进 `out`。
    ///
    /// 本帧的临时区（子帧样本 + 借用表）都在这儿现搭：`sub` 在栈上（零分配），
    /// 借用表 `refs` 是每帧 `num_bands` 个小 `Vec`（见 [`set_sub_frame_arg`] 的注释）。
    fn run_frame(&mut self, out: &mut Vec<f32>) {
        // 参考：环里取 10 ms（欠载补零），已经按 int16 域存好了。
        self.ring.read_frame(&mut self.reference[..self.frame]);
        // 采集：攒帧缓冲里那 10 ms，累加时已经乘过 `S16_SCALE`。
        let start = out.len();
        // 容量在 `process` 里按"本拍能出的帧数 × 帧长"一次留足，这里不会重新分配。
        out.resize(start + self.frame, 0.0);

        let num_bands = self.num_bands;
        // 参考路与采集路各占一段（各 2 个半帧 × num_bands 个频带），两段互不重叠。
        let mut sub = [[0.0f32; SUB]; 4 * NUM_BANDS];
        let (render_sub, capture_sub) = sub.split_at_mut(2 * NUM_BANDS);
        let mut refs = sub_frame_refs();

        let Self {
            engine,
            reference,
            render_bands,
            capture_buf,
            capture_bands,
            out_bands,
            framer_out,
            ..
        } = self;
        // 先参考后采集：AEC3 的块级路径要求每拍都先 `buffer_render` 再 `process_capture`
        // （S4-C 稿 §6.1 的顺序）。
        //
        // **但环欠载时一次都不推**（`RefRing::hit`）——这是 2026-10-12 修掉的一个真缺陷：
        // 原来这里无条件调 `push_render`，欠载时推的是**静音的渲染块**，实测 48 kHz 三频带上
        // 麦克风被压掉 **25.91 dB**（16 kHz 单频带碰巧不受影响）。对照：W0 尖峰在同样条件下
        // 一次 `buffer_render` 都不调，只压 **0.28 dB**。「远端安静、没有译音可播」恰是**最常见**
        // 的一格，那时候麦音本该原样过去。判据见 `aec_synthetic.rs::T18`。
        // 注意这里**只跳渲染路、不跳采集路**：采集照常过，只是这一拍没有参考可消。
        if self.ring.hit {
            engine.push_render(num_bands, reference, render_bands, render_sub, &mut refs);
        }
        engine.push_capture(
            num_bands,
            capture_buf,
            capture_bands,
            out_bands,
            capture_sub,
            &mut refs,
            framer_out,
            &mut out[start..],
        );
    }
}

impl Aec for SonoraAec {
    /// 送一块**采集率**的单声道 f32（上行音频），返回已消回声的部分。
    ///
    /// 攒够 10 ms 才出一帧（AEC3 的原生帧长），所以**输出长度不等于输入长度**、
    /// 首帧之前可能返回空——与 `Denoise::process` 同一条契约（契约 C2）。
    ///
    /// 分配：一次 `Vec::with_capacity(本拍能出的帧数 × 帧长)`，恰好装得下，之后
    /// 不再增长；本拍一帧都出不了时容量是 0，**一次都不分配**。这是本函数的
    /// **唯一**分配（与 `Denoise::process` 的返回值那一次同性质）。
    fn process(&mut self, capture: &[f32]) -> Vec<f32> {
        let frame = self.frame;
        let frames = (self.capture_len + capture.len()) / frame;
        let mut out = Vec::with_capacity(frames * frame);
        let mut rest = capture;
        while !rest.is_empty() {
            let room = frame - self.capture_len;
            let take = room.min(rest.len());
            let at = self.capture_len;
            // 幅度换算在这里：上行进 AEC3 也要 int16 域（模块头第 2 条）。
            for (slot, &x) in self.capture_buf[at..at + take]
                .iter_mut()
                .zip(&rest[..take])
            {
                *slot = x * S16_SCALE;
            }
            self.capture_len += take;
            rest = &rest[take..];
            if self.capture_len == frame {
                self.capture_len = 0;
                self.run_frame(&mut out);
            }
        }
        out
    }

    /// 播放汇刚拿到一段要外放的译音（**必须在 `sink.push()` 之前**调）。
    ///
    /// `samples` 是**播放率**（[`OUTPUT_SAMPLE_RATE`]，今天 24 kHz），与采集率
    /// 可以不同——线性插值到采集率再进环（§5.4）。
    ///
    /// **零分配**：只有一次「按输入样本数出输出样本、直接写环」的循环，
    /// 中间不建缓冲、不 `clone`、不格式化。
    fn push_reference(&mut self, samples: &[f32]) {
        self.upsampler.push_into(samples, &mut self.ring);
    }

    /// 丢掉内部缓冲和状态（换会话、断流后重来时用）。
    ///
    /// 自适应滤波器里的历史回声路径对下一条会话无效（契约 C6 的 AEC 半边）；
    /// 与门不同，AEC 的状态不是用户的按键，所以清。上游没有公开的 `reset`，
    /// 这里整只重建算法本体——**这件事只在断线重来时发生，不在热路径上**。
    fn reset(&mut self) {
        self.engine = Engine::new(self.rate, self.num_bands);
        self.ring.clear();
        self.upsampler.clear();
        self.capture_len = 0;
    }
}
