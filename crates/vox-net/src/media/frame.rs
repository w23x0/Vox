//! 媒体面的帧格式：**20 字节定长头 + 载荷**（S3 设计稿 §2.1.3，逐字节定死）。
//!
//! 纯数据：不碰 socket、不碰线程、不读时钟。分工照 `vox_core::cloud::protocol`。
//!
//! ```text
//! 偏移   0..2    2     3      4..6      6..10     10..14    14..18    18..20
//! 字段   magic   ver   kind   flags     seq       ts_ms     rate      ch
//! 类型   "VB"    u8    u8     u16 LE    u32 LE    u32 LE    u32 LE    u16 LE
//! ```
//!
//! 载荷 = 帧长 − 20，上限 [`MAX_PAYLOAD`]。只认二进制帧；文本帧不是"坏帧内容"而是
//! 协议错误（[`MediaError::TextFrame`]，由 WS 读循环在拿到文本帧时报出来）。

use std::fmt;

/// 帧头魔数。
pub const MAGIC: [u8; 2] = *b"VB";
/// 协议版本。不符即协议错误（关连接，不许猜）。
pub const VERSION: u8 = 1;
/// 头长（定长）。
pub const HEADER_LEN: usize = 20;
/// 载荷上限：8 KiB = 4096 个 PCM16 样本 = 256 ms @16 kHz。
pub const MAX_PAYLOAD: usize = 8 * 1024;

const OFF_VERSION: usize = 2;
const OFF_KIND: usize = 3;
const OFF_FLAGS: usize = 4;
const OFF_SEQ: usize = 6;
const OFF_TS_MS: usize = 10;
const OFF_RATE: usize = 14;
const OFF_CHANNELS: usize = 18;

/// 帧的种类。v1 只认这两个；将来的 Opus 靠这个字节扩，不动头长与清单。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// 载荷是 PCM16LE 单声道。
    Pcm16Le,
    /// 保活帧，载荷必须为空。
    KeepAlive,
}

impl FrameKind {
    /// 线上的那个字节。
    pub fn code(self) -> u8 {
        match self {
            Self::Pcm16Le => 0,
            Self::KeepAlive => 1,
        }
    }

    /// 线上的那个字节 → 种类；不认识的取值是协议错误。
    pub fn from_code(code: u8) -> Result<Self, MediaError> {
        match code {
            0 => Ok(Self::Pcm16Le),
            1 => Ok(Self::KeepAlive),
            other => Err(MediaError::Kind(other)),
        }
    }
}

/// 帧头（载荷不进这里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    /// u32 环绕，**每条连接从随机值起**。只用来检测丢帧/乱序（TCP 下不该发生）。
    pub seq: u32,
    /// 发送侧单调毫秒（连接建立时的零点）。只进统计与日志，**不参与播放调度**。
    pub ts_ms: u32,
    /// 载荷采样率（Hz）。入站必须等于声明率，不符即协议错误（**不静默重采样**）。
    pub rate: u32,
    /// v1 必须 = 1（单声道）。
    pub channels: u16,
}

/// 协议错误。每一种都能让连接死掉——这正是它的用途（不静默忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    /// 帧长 < [`HEADER_LEN`]。
    Short,
    /// 魔数不是 `b"VB"`。
    Magic,
    /// 版本不是 [`VERSION`]。
    Version(u8),
    /// `kind` 不是 [`FrameKind`] 里的取值。
    Kind(u8),
    /// v1 的 `flags` 必须为 0。
    Flags(u16),
    /// 载荷采样率与声明率不符。
    Rate { got: u32, want: u32 },
    /// v1 必须单声道。
    Channels(u16),
    /// 载荷超过 [`MAX_PAYLOAD`]。
    Oversize(usize),
    /// 保活帧带了载荷（设计稿：`keepalive` 的载荷必须为空）。
    ///
    /// 设计稿的 `MediaError` 列表没列这一条，但同一节写了这条 MUST；拒收比默默
    /// 忽略安全，所以补一个取值，语义与其它错误一样：计数 + 关连接。
    KeepAlivePayload(usize),
    /// 收到文本帧。音频管子不认协议方言。
    TextFrame,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => write!(f, "frame is shorter than the {HEADER_LEN}-byte header"),
            Self::Magic => f.write_str("magic is not VB"),
            Self::Version(v) => write!(
                f,
                "unknown protocol version {v} (this build only speaks {VERSION})"
            ),
            Self::Kind(k) => write!(f, "unknown frame kind {k}"),
            Self::Flags(flags) => write!(f, "flags must be 0, got {flags}"),
            Self::Rate { got, want } => write!(
                f,
                "payload sample rate {got} Hz does not match the declared {want} Hz"
            ),
            Self::Channels(ch) => write!(f, "channel count must be 1, got {ch}"),
            Self::Oversize(len) => {
                write!(f, "payload of {len} bytes exceeds the limit {MAX_PAYLOAD}")
            }
            Self::KeepAlivePayload(len) => {
                write!(f, "keep-alive frames carry no payload, got {len} bytes")
            }
            Self::TextFrame => {
                f.write_str("the audio pipe only accepts binary frames, got a text frame")
            }
        }
    }
}

impl std::error::Error for MediaError {}

/// 编码一帧到复用的缓冲：`out` 先清空，再写头 + 载荷。
///
/// 热路径（出站攒块）复用同一个 `Vec`，不新建分配。
pub fn encode(header: &FrameHeader, payload: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(header.kind.code());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&header.seq.to_le_bytes());
    out.extend_from_slice(&header.ts_ms.to_le_bytes());
    out.extend_from_slice(&header.rate.to_le_bytes());
    out.extend_from_slice(&header.channels.to_le_bytes());
    out.extend_from_slice(payload);
}

/// 解码一帧：校验头，返回头 + 载荷切片（载荷借用入参，不拷贝）。
///
/// `want_rate` 是这条连接的声明率（`Settings.net.rate_hz`）——**每一帧都要相符**，
/// keepalive 也不例外（率是这条会话的身份，不是每帧的选项）。
pub fn decode(bytes: &[u8], want_rate: u32) -> Result<(FrameHeader, &[u8]), MediaError> {
    if bytes.len() < HEADER_LEN {
        return Err(MediaError::Short);
    }
    if bytes[0..2] != MAGIC {
        return Err(MediaError::Magic);
    }
    let version = bytes[OFF_VERSION];
    if version != VERSION {
        return Err(MediaError::Version(version));
    }
    let kind = FrameKind::from_code(bytes[OFF_KIND])?;
    let flags = u16::from_le_bytes([bytes[OFF_FLAGS], bytes[OFF_FLAGS + 1]]);
    if flags != 0 {
        return Err(MediaError::Flags(flags));
    }
    let channels = u16::from_le_bytes([bytes[OFF_CHANNELS], bytes[OFF_CHANNELS + 1]]);
    if channels != 1 {
        return Err(MediaError::Channels(channels));
    }
    let rate = u32::from_le_bytes([
        bytes[OFF_RATE],
        bytes[OFF_RATE + 1],
        bytes[OFF_RATE + 2],
        bytes[OFF_RATE + 3],
    ]);
    let payload = &bytes[HEADER_LEN..];
    if payload.len() > MAX_PAYLOAD {
        return Err(MediaError::Oversize(payload.len()));
    }
    if kind == FrameKind::KeepAlive && !payload.is_empty() {
        return Err(MediaError::KeepAlivePayload(payload.len()));
    }
    if rate != want_rate {
        return Err(MediaError::Rate {
            got: rate,
            want: want_rate,
        });
    }
    let header = FrameHeader {
        kind,
        seq: u32::from_le_bytes([
            bytes[OFF_SEQ],
            bytes[OFF_SEQ + 1],
            bytes[OFF_SEQ + 2],
            bytes[OFF_SEQ + 3],
        ]),
        ts_ms: u32::from_le_bytes([
            bytes[OFF_TS_MS],
            bytes[OFF_TS_MS + 1],
            bytes[OFF_TS_MS + 2],
            bytes[OFF_TS_MS + 3],
        ]),
        rate,
        channels,
    };
    Ok((header, payload))
}

/// 单个样本 f32 → PCM16。夹取语义与 `vox_core::cloud::protocol::float_to_pcm16`
/// **逐字相同**：先夹到 ±1.0，负半轴 ×32768、正半轴 ×32767。
#[inline]
pub fn sample_to_pcm16(sample: f32) -> i16 {
    let clipped = sample.clamp(-1.0, 1.0);
    let scaled = if clipped < 0.0 {
        clipped * 32768.0
    } else {
        clipped * 32767.0
    };
    scaled as i16
}

/// 单个样本 PCM16 → f32（`vox_core::cloud::protocol::pcm16_to_float` 的同一条式子）。
#[inline]
pub fn sample_from_pcm16(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

/// f32 样本 → PCM16LE 字节，**追加**到 `out`。
///
/// 与 `vox_core::cloud::protocol::float_to_pcm16` 的区别只有一个：输出缓冲由调用方
/// 复用，热路径不新建 `Vec`。两者的等价由单测钉住（`pcm16_helpers_match_the_core`）。
pub fn push_pcm16(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &sample in samples {
        out.extend_from_slice(&sample_to_pcm16(sample).to_le_bytes());
    }
}

/// PCM16LE 字节 → f32 样本，**追加**到 `out`；半个样本的尾巴直接丢掉
/// （与 `pcm16_to_float` 同款）。
pub fn push_pcm16_samples(bytes: &[u8], out: &mut Vec<f32>) {
    out.reserve(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        out.push(sample_from_pcm16(i16::from_le_bytes(*pair)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::cloud::protocol::{float_to_pcm16, pcm16_to_float};

    fn header(kind: FrameKind) -> FrameHeader {
        FrameHeader {
            kind,
            seq: 0x0102_0304,
            ts_ms: 0x0a0b_0c0d,
            rate: 16_000,
            channels: 1,
        }
    }

    #[test]
    fn the_wire_layout_is_byte_for_byte_the_design() {
        let mut out = Vec::new();
        encode(&header(FrameKind::Pcm16Le), &[0xaa, 0xbb], &mut out);
        assert_eq!(out.len(), HEADER_LEN + 2);
        assert_eq!(&out[0..2], b"VB");
        assert_eq!(out[2], VERSION);
        assert_eq!(out[3], 0, "pcm16le 的种类码是 0");
        assert_eq!(&out[4..6], &[0, 0], "flags 必须是 0");
        assert_eq!(&out[6..10], &[4, 3, 2, 1], "seq 是 u32 LE");
        assert_eq!(&out[10..14], &[13, 12, 11, 10], "ts_ms 是 u32 LE");
        assert_eq!(&out[14..18], &[0x80, 0x3e, 0, 0], "16000 的 u32 LE");
        assert_eq!(&out[18..20], &[1, 0], "声道数是 u16 LE");
        assert_eq!(&out[20..], &[0xaa, 0xbb]);
    }

    #[test]
    fn a_round_trip_gives_back_the_same_header_and_payload() {
        let mut out = Vec::new();
        let payload: Vec<u8> = (0..640u16).map(|i| (i % 251) as u8).collect();
        encode(&header(FrameKind::Pcm16Le), &payload, &mut out);
        let (head, back) = decode(&out, 16_000).expect("自己编的自己该能解");
        assert_eq!(head, header(FrameKind::Pcm16Le));
        assert_eq!(back, &payload[..]);
    }

    #[test]
    fn every_protocol_error_has_a_case() {
        let base = header(FrameKind::Pcm16Le);
        let build = |mutate: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = Vec::new();
            encode(&base, &[1, 2], &mut bytes);
            mutate(&mut bytes);
            bytes
        };
        let cases: Vec<(Vec<u8>, MediaError)> = vec![
            (vec![b'V'; HEADER_LEN - 1], MediaError::Short),
            (build(&|b| b[0] = b'X'), MediaError::Magic),
            (build(&|b| b[2] = 2), MediaError::Version(2)),
            (build(&|b| b[3] = 9), MediaError::Kind(9)),
            (build(&|b| b[4] = 1), MediaError::Flags(1)),
            (build(&|b| b[18] = 2), MediaError::Channels(2)),
            (
                {
                    let mut bytes = Vec::new();
                    encode(&base, &[0; MAX_PAYLOAD + 1], &mut bytes);
                    bytes
                },
                MediaError::Oversize(MAX_PAYLOAD + 1),
            ),
            (
                {
                    let mut bytes = Vec::new();
                    encode(&header(FrameKind::KeepAlive), &[7], &mut bytes);
                    bytes
                },
                MediaError::KeepAlivePayload(1),
            ),
            (
                {
                    let mut bytes = Vec::new();
                    encode(&header(FrameKind::Pcm16Le), &[1, 2], &mut bytes);
                    let rate = 8_000u32.to_le_bytes();
                    bytes[OFF_RATE..OFF_RATE + 4].copy_from_slice(&rate);
                    bytes
                },
                MediaError::Rate {
                    got: 8_000,
                    want: 16_000,
                },
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(decode(&bytes, 16_000), Err(want), "输入 {bytes:?}");
        }
    }

    #[test]
    fn a_keepalive_frame_is_twenty_bytes_with_no_payload() {
        let mut out = Vec::new();
        encode(&header(FrameKind::KeepAlive), &[], &mut out);
        assert_eq!(out.len(), HEADER_LEN);
        let (head, payload) = decode(&out, 16_000).expect("保活帧合法");
        assert_eq!(head.kind, FrameKind::KeepAlive);
        assert!(payload.is_empty());
        assert_eq!(FrameKind::KeepAlive.code(), 1);
    }

    #[test]
    fn pcm16_helpers_match_the_core() {
        // 芯里的编码器是这条语义的唯一定义方：两边必须逐字节相同（含 ±1.0 与越界值）。
        let samples = [
            -1.0f32,
            -0.999_969_5,
            -0.5,
            -0.000_03,
            0.0,
            0.000_03,
            0.5,
            0.999_969_5,
            1.0,
            1.5,
            -1.5,
        ];
        let mut mine = Vec::new();
        push_pcm16(&samples, &mut mine);
        assert_eq!(mine, float_to_pcm16(&samples));

        let mut back = Vec::new();
        push_pcm16_samples(&mine, &mut back);
        assert_eq!(back, pcm16_to_float(&mine));

        // 奇数尾巴（半个样本）直接丢，不许 panic 也不许读越界。
        let mut odd = Vec::new();
        push_pcm16_samples(&mine[..mine.len() - 1], &mut odd);
        assert_eq!(odd.len(), samples.len() - 1);
    }
}
