//! 内核对外壳的要求，全是 trait。
//!
//! 内核不知道 WASAPI、Win32、Tauri 存在。要用平台能力时只认这里的 trait，
//! Windows 那边负责实现并在启动时注入。想搬到别的平台，重写实现即可。
//!
//! 音频那一组（[`Denoise`] / [`Resample`] / [`Aec`]）**一律是 f32 的 [-1,1]**：
//! 底层算法吃什么幅度域是实现自己的事——[`Aec`] 的实现内部就要把 [-1,1] 换算到
//! int16 域（×32768），因为 `sonora-aec3` 的激励门限活在那一边（S4-C 稿 §6.2）。

use std::fmt;

use crate::notice::Msg;
use crate::subtitle::{RenderedChar, Track};

/// 外壳侧的失败。内核只关心"失败了、原因是什么"，不关心是哪个 HRESULT。
///
/// `message` 是给开发者 / 日志看的诊断串（OS 错误、errno…），**不翻译**。
/// 要给用户看时，由 [`PortError::into_msg`] 变成 [`Msg`]：外壳认得出的失败
/// （找不到设备、程序没在出声…）用 [`PortError::user`] 直接带 key；其余由内核按
/// 失败发生的**阶段**补 key，诊断串作为 `detail` 原样跟在后面。
#[derive(Debug, Clone)]
pub struct PortError {
    pub message: String,
    /// 外壳已经给出的、用户能照着做的说法。
    pub user: Option<Msg>,
}

impl PortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            user: None,
        }
    }

    /// 带用户可读 key 的失败；`message` 仍是诊断串。
    pub fn user(user: Msg, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            user: Some(user),
        }
    }

    /// 标上"失败发生在哪一阶段"：外壳没给 key 就用 `stage`，诊断串当 `detail`；
    /// 外壳给过的保持不动。
    pub fn at(mut self, stage: &'static str) -> Self {
        if self.user.is_none() {
            self.user = Some(Msg::new(stage).detail(self.message.clone()));
        }
        self
    }

    /// 变成给用户看的消息：外壳给过 key 就用它（它自己的 detail 优先），
    /// 否则用 `stage` 这个 key，诊断串当 `detail`。
    pub fn into_msg(self, stage: &'static str) -> Msg {
        match self.user {
            Some(user) => user,
            None => Msg::new(stage).detail(self.message),
        }
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PortError {}

pub type PortResult<T> = Result<T, PortError>;

// --- 音频采集 --------------------------------------------------------------

/// 一块采集到的音频。交织的多声道数据，转单声道是内核的事。
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioChunk {
    /// 交织多声道混成单声道。
    pub fn to_mono(&self) -> Vec<f32> {
        if self.channels <= 1 {
            return self.samples.clone();
        }
        let ch = self.channels as usize;
        let scale = 1.0 / ch as f32;
        self.samples
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() * scale)
            .collect()
    }
}

/// 采集的来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// 麦克风。`None` = 自动选默认设备。
    Microphone(Option<String>),
    /// 抓某个程序放出来的声音（进程环回）。
    ProcessLoopback {
        executable: String,
        include_tree: bool,
    },
    /// 网络对端（媒体面）。`pipe` 是清单里那个逻辑名（v1 只有 `"default"`）。
    ///
    /// 实现方不属于任何平台采集 crate：它是 `vox-net` 媒体面监听侧的采集源
    /// （`vox_net::media::MediaListener::capture`）。设备采集实现遇到这个变体
    /// **必须报错**，不许把它当成默认设备（那是静默换源）。
    Net { pipe: String },
}

/// 采集源。`start` 之后音频块通过回调推给内核，`stop` 要能保证回调不再触发。
pub trait CaptureSource: Send {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat>;

    fn stop(&mut self);
}

/// 采集实际协商到的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

// --- 音频播放 --------------------------------------------------------------

/// 播放汇。内核推 24 kHz 单声道 f32，外壳负责重采样到设备率。
pub trait PlaybackSink: Send {
    /// 打开设备。`device` 为 `None` 时用系统默认。返回实际采样率。
    fn open(&mut self, device: Option<&str>, source_rate: u32) -> PortResult<u32>;
    /// 送一块要放的音频。队列满时应丢最旧的，绝不阻塞调用方。
    fn push(&mut self, samples: &[f32]);
    /// 当前播放侧的无锁统计。默认实现让测试替身和非 Windows 端口无需强制实现。
    fn stats(&self) -> PlaybackStats {
        PlaybackStats::default()
    }
    /// 立刻丢掉还没放出去的（停流水线时用）。
    fn flush(&mut self);
    fn close(&mut self);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    /// 环形缓冲中尚未被渲染线程取走的交错样本数。
    pub queued_samples: usize,
    pub sample_rate: u32,
    pub channels: u16,
    /// 渲染线程累计真正取走的（不含自动补零）交错样本数。
    pub rendered_samples: u64,
    pub dropped_samples: u64,
    /// WASAPI 报告的流延迟；拿不到时为 0。
    pub device_latency_ms: u64,
}

// --- 信号处理 --------------------------------------------------------------

/// 降噪器。内核只知道"送一块进去、拿一块出来"，具体算法在 `vox-dsp` 里。
///
/// **输出长度不等于输入长度**：实现内部按固定帧长（RNNoise 是 48 kHz/480 样本）
/// 攒够一帧才出货，样本不够时返回空切片；首帧也可能被吞掉。调用方不许假设长度守恒。
pub trait Denoise: Send {
    /// 送一块 48 kHz 单声道 f32，返回已经降噪好的部分（可能为空，可能长于输入）。
    fn process(&mut self, samples: &[f32]) -> Vec<f32>;
    /// 丢掉内部缓冲和状态（换会话、断流后重来时用）。
    fn reset(&mut self);
}

/// 回声消除器。**有流状态**，断线重来要清（同 [`Denoise::reset`]）。
///
/// 与 `Denoise` 同样的约定：**不保证输出长度等于输入长度**（内部攒帧），
/// 返回**已经消掉回声的部分**（可能为空）。
///
/// **两条时间线**：[`Aec::process`] 进来的是**采集率**的上行（麦克风听到的
/// 空气声，里面混着回声），[`Aec::push_reference`] 进来的是**播放率**的译音
/// （就是我们刚交给播放汇、马上要外放的那一段）。两个率可以不同、两路也
/// **不需要同一拍**送到——换率与对齐都是实现自己的事。
///
/// **幅度口径（W0 §6.2）**：本 trait 的口径与 `Denoise` 一样，是 **f32 的 [-1,1]**；
/// 但底层 `sonora-aec3` 的门限活在 **int16 域**，实现内部必须自己换算（×32768）。
/// 这是"芯里一律 f32"与"AEC3 只认 int16 域"之间必须由实现兜住的那道缝，
/// 别外泄到 trait 上。
pub trait Aec: Send {
    /// 送一块**采集率**的单声道 f32（上行音频），返回已消回声的部分。
    fn process(&mut self, capture: &[f32]) -> Vec<f32>;

    /// 播放汇刚拿到一段要外放的译音。**必须在 `sink.push()` 之前调**。
    ///
    /// `samples` 是**播放率**（`RateRef::Playback` 解析出来的那一档，今天恒 24 kHz），
    /// 与 `process` 的采集率可以不同——换率是本实现自己的事（出处是设计稿
    /// `S4-B-OP-CHAIN.md` §2.7，不是 `chain.rs` 的注释）。
    ///
    /// 参考与上行**不需要同一拍**送到：本实现内部有对齐缓冲（S4-C 稿 §5.3）。
    fn push_reference(&mut self, samples: &[f32]);

    /// 丢掉内部缓冲和状态（换会话、断流后重来时用）。
    fn reset(&mut self);
}

/// 重采样器。内核用它把采集率换成协议要的 16 kHz。
///
/// 同样**不保证输出长度等于输入长度**：内部会攒够一个 chunk 才算一批，
/// 采样率相同时是直通。
pub trait Resample: Send {
    /// 送一块单声道 f32，返回已经换好率的部分（可能为空）。
    fn process(&mut self, samples: &[f32]) -> Vec<f32>;
    /// 把缓冲里剩的零头挤出来（一段语音收尾时用）。
    fn flush(&mut self) -> Vec<f32>;
    /// 丢掉内部缓冲和状态。
    fn reset(&mut self);
}

// --- 设备目录 --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// 一个正在放声音的程序，给"听人说话"的选择器用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioApp {
    pub executable: String,
    pub display_name: String,
    pub pid: u32,
    /// 现在是不是正在出声。
    pub active: bool,
}

pub trait DeviceRegistry: Send + Sync {
    fn input_devices(&self) -> PortResult<Vec<DeviceInfo>>;
    fn output_devices(&self) -> PortResult<Vec<DeviceInfo>>;
    /// 正在放声音的程序列表。
    fn audio_apps(&self) -> PortResult<Vec<AudioApp>>;
    /// VB-CABLE **装了没** —— 只描述**安装器状态**，不回答"虚拟麦能不能用"。
    ///
    /// "能不能用"的唯一真相是能力位 [`Capability::VirtualMic`](crate::capability::Capability::VirtualMic)：
    /// 它由外壳注入的 [`HostFacts`](crate::capability::HostFacts) 算出来，由"真的把这条路打开的那段
    /// 代码"负责（装驱动 / 建节点 / 拿到句柄）。这个字段只留着给 Windows 下载 / 安装 VB-CABLE
    /// 的那套 UI 用（"要不要提示用户去装"），**不许**拿它当能力判据。
    ///
    /// 两处口径差别特别注意：装了 ≠ 能用（装了也可能没被选中 / 节点掉了）；没装 ≠ 不能用
    /// （Linux 上压根没有"装"这一步，这条恒为 `false`，而虚拟麦照样能在）。
    fn virtual_cable_installed(&self) -> bool;
}

// --- 热键 ------------------------------------------------------------------

/// 哪个热键、按下还是松开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    SpeakPressed,
    SpeakReleased,
    ListenPressed,
}

/// 要监听的热键集合。外壳按这份清单轮询，一个键只有一条生效路径。
#[derive(Debug, Clone, Default)]
pub struct HotkeyBindings {
    pub speak: Option<crate::hotkey::Hotkey>,
    pub listen: Option<crate::hotkey::Hotkey>,
}

pub trait HotkeyHost: Send + Sync {
    /// 换一套要监听的热键（设置改了就调）。
    fn rebind(&self, bindings: HotkeyBindings) -> PortResult<()>;
}

// --- 悬浮字幕窗 ------------------------------------------------------------

/// 一帧要画的字幕。
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleFrame {
    pub lines: Vec<SubtitleLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleLine {
    pub track: Track,
    /// `chars[0]` 的绝对下标（这条轨从队首丢掉过多少字，只增不减）。
    /// 渲染侧靠它把"某一行的行首"这类锚点跨帧对上，见 `SubtitleTrack::origin`。
    pub origin: u64,
    pub chars: Vec<RenderedChar>,
    /// `#rrggbb`。
    pub color: String,
}

pub trait SubtitleView: Send + Sync {
    fn show(&self);
    fn hide(&self);
    /// 推一帧。实现方要保证跨线程安全（内部靠通道 + PostMessage 叫醒窗口线程）。
    fn render(&self, frame: SubtitleFrame);
    /// 外观变了（字体、字号、底衬透明度）。
    fn restyle(&self, settings: &crate::settings::SubtitleSettings);
}

// --- 密钥存储 --------------------------------------------------------------

/// API 密钥不进配置文件，单独存，落盘时加密（Windows 上用 DPAPI）。
pub trait SecretStore: Send + Sync {
    fn load_api_key(&self) -> PortResult<Option<String>>;
    fn store_api_key(&self, key: &str) -> PortResult<()>;
    fn clear_api_key(&self) -> PortResult<()>;

    fn load_api_key_for(
        &self,
        _provider: crate::settings::ModelProvider,
    ) -> PortResult<Option<String>> {
        self.load_api_key()
    }
    fn store_api_key_for(
        &self,
        _provider: crate::settings::ModelProvider,
        key: &str,
    ) -> PortResult<()> {
        self.store_api_key(key)
    }
    fn clear_api_key_for(&self, _provider: crate::settings::ModelProvider) -> PortResult<()> {
        self.clear_api_key()
    }
}

// --- 时钟 ------------------------------------------------------------------

/// 内核不直接读系统时钟，方便测试注入时间。
pub trait Clock: Send + Sync {
    /// 启动至今的毫秒数，单调递增。
    fn now_ms(&self) -> u64;
    /// 当前日期，给用量分桶用。
    fn stamp(&self) -> crate::usage::Stamp;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_downmix_averages_channels() {
        let chunk = AudioChunk {
            samples: vec![1.0, 0.0, 0.5, 0.5],
            sample_rate: 48_000,
            channels: 2,
        };
        assert_eq!(chunk.to_mono(), vec![0.5, 0.5]);
    }

    #[test]
    fn mono_input_passes_through() {
        let chunk = AudioChunk {
            samples: vec![0.1, 0.2, 0.3],
            sample_rate: 16_000,
            channels: 1,
        };
        assert_eq!(chunk.to_mono(), chunk.samples);
    }
}
