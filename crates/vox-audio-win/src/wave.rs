//! 波形格式的构造与解析，外加样本转换。
//!
//! WASAPI 共享模式给什么格式我们就得吃什么格式：常见是 32 位浮点，但也见过
//! 16 位整数、24-in-32 的设备。所以这里把 `WAVEFORMATEX` 解析成一个枚举，
//! 再统一转成内部通用的 f32。

use vox_core::ports::{PortError, PortResult};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};

/// 设备实际吐出来的样本类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleKind {
    /// 32 位浮点，[-1, 1]。
    F32,
    /// 16 位有符号整数。
    I16,
    /// 32 位有符号整数（含 24-in-32：低位是 0，按 32 位读没问题）。
    I32,
    /// 紧凑排列的 24 位有符号整数，每样本 3 字节。
    I24,
}

impl SampleKind {
    pub(crate) fn bytes(self) -> usize {
        match self {
            SampleKind::F32 | SampleKind::I32 => 4,
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
        }
    }
}

/// 从 `WAVEFORMATEX` 里抽出我们关心的那几项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaveInfo {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) kind: SampleKind,
    pub(crate) block_align: usize,
}

/// 解析 `WAVEFORMATEX`（含 `WAVEFORMATEXTENSIBLE`）。
///
/// # Safety
/// `fmt` 必须指向一个合法的 `WAVEFORMATEX`；若 `wFormatTag` 是
/// `WAVE_FORMAT_EXTENSIBLE`，其后必须真的跟着扩展字段（`cbSize >= 22`）。
pub(crate) unsafe fn parse_format(fmt: *const WAVEFORMATEX) -> PortResult<WaveInfo> {
    if fmt.is_null() {
        return Err(PortError::new(
            "the device returned no audio format (null pointer)",
        ));
    }
    // SAFETY: 调用方保证指针合法，这里只读基础字段。
    // WAVEFORMATEX 是 packed(1)，字段必须先拷成局部变量才能取引用（比如塞进 format!）。
    let base = unsafe { *fmt };
    let tag = base.wFormatTag as u32;
    let bits = base.wBitsPerSample;
    let cb_size = base.cbSize;
    let channels = base.nChannels;
    let sample_rate = base.nSamplesPerSec;
    let block_align_raw = base.nBlockAlign;

    let kind = if tag == WAVE_FORMAT_EXTENSIBLE {
        if (cb_size as usize) < 22 {
            return Err(PortError::new(format!(
                "the device returned an incomplete extended format header (cbSize={cb_size})"
            )));
        }
        // SAFETY: wFormatTag 为 EXTENSIBLE 且 cbSize >= 22，按约定后面就是
        // WAVEFORMATEXTENSIBLE 的剩余字段，可以整体重解释。
        let ext = unsafe { *(fmt as *const WAVEFORMATEXTENSIBLE) };
        let sub = ext.SubFormat;
        if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            SampleKind::F32
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            kind_from_bits(bits)?
        } else {
            return Err(PortError::new(format!(
                "the device uses an unknown audio sub-format ({sub:?}); only PCM and float are supported"
            )));
        }
    } else if tag == WAVE_FORMAT_IEEE_FLOAT {
        SampleKind::F32
    } else if tag == WAVE_FORMAT_PCM {
        kind_from_bits(bits)?
    } else {
        return Err(PortError::new(format!(
            "the device uses an unknown audio format tag ({tag}); only PCM and float are supported"
        )));
    };

    if channels == 0 || sample_rate == 0 {
        return Err(PortError::new(format!(
            "the device returned an implausible format: {channels} channels / {sample_rate} Hz"
        )));
    }

    let block_align = if block_align_raw == 0 {
        kind.bytes() * channels as usize
    } else {
        block_align_raw as usize
    };

    Ok(WaveInfo {
        sample_rate,
        channels,
        kind,
        block_align,
    })
}

fn kind_from_bits(bits: u16) -> PortResult<SampleKind> {
    match bits {
        16 => Ok(SampleKind::I16),
        24 => Ok(SampleKind::I24),
        32 => Ok(SampleKind::I32),
        other => Err(PortError::new(format!(
            "the device uses {other}-bit integer samples, which are not supported (only 16/24/32-bit and float)"
        ))),
    }
}

/// 手搓一个 32 位浮点的 `WAVEFORMATEXTENSIBLE`。
///
/// 进程环回那个伪设备的 `GetMixFormat` 不能用，格式只能自己写死，
/// 所以这个函数必须能独立造出完整的头。
pub(crate) fn float_format(sample_rate: u32, channels: u16) -> WAVEFORMATEXTENSIBLE {
    let bits = 32u16;
    // 饱和运算：release 是 panic = "abort"，debug 下溢出会直接炸。
    // 现实里声道数就是 1 或 2，但这两个乘法不值得留成隐患。
    let block_align = channels.saturating_mul(bits / 8);
    let mut fmt = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate.saturating_mul(block_align as u32),
            nBlockAlign: block_align,
            wBitsPerSample: bits,
            // cbSize 只算 WAVEFORMATEX 之后那 22 个字节。
            cbSize: 22,
        },
        Samples: Default::default(),
        dwChannelMask: channel_mask(channels),
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };
    fmt.Samples.wValidBitsPerSample = bits;
    fmt
}

/// 声道掩码。单声道给前中置，双声道给左右，其它按低位铺满。
///
/// 进程环回必须给 0x3（双声道左右），掩码不对会直接激活失败。
pub(crate) fn channel_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4, // SPEAKER_FRONT_CENTER
        2 => 0x3, // FRONT_LEFT | FRONT_RIGHT
        n => (1u32 << n.min(18)) - 1,
    }
}

/// 把设备给的原始字节按 `kind` 转成 f32，追加到 `out`。
pub(crate) fn bytes_to_f32(bytes: &[u8], kind: SampleKind, out: &mut Vec<f32>) {
    let step = kind.bytes();
    let count = bytes.len() / step;
    out.reserve(count);
    match kind {
        SampleKind::F32 => {
            for chunk in bytes.as_chunks::<4>().0 {
                let v = f32::from_le_bytes(*chunk);
                // 设备偶尔给出非法浮点（驱动 bug），直接抹成静音比让 NaN 传染下游好。
                out.push(if v.is_finite() { v } else { 0.0 });
            }
        }
        SampleKind::I16 => {
            for chunk in bytes.as_chunks::<2>().0 {
                let v = i16::from_le_bytes(*chunk);
                out.push(v as f32 / 32768.0);
            }
        }
        SampleKind::I24 => {
            for chunk in bytes.as_chunks::<3>().0 {
                // 24 位补码：搬到 i32 的高 24 位再右移，符号位自动跟着走。
                let v = ((chunk[0] as i32) << 8)
                    | ((chunk[1] as i32) << 16)
                    | ((chunk[2] as i32) << 24);
                out.push((v >> 8) as f32 / 8_388_608.0);
            }
        }
        SampleKind::I32 => {
            for chunk in bytes.as_chunks::<4>().0 {
                let v = i32::from_le_bytes(*chunk);
                out.push(v as f32 / 2_147_483_648.0);
            }
        }
    }
}

/// 把内部 f32 样本写成设备要求的格式。播放实时线程使用，不能分配内存。
pub(crate) fn f32_to_bytes(samples: &[f32], kind: SampleKind, out: &mut [u8]) {
    let step = kind.bytes();
    let count = samples.len().min(out.len() / step);
    for (sample, bytes) in samples[..count]
        .iter()
        .zip(out[..count * step].chunks_exact_mut(step))
    {
        let value = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match kind {
            SampleKind::F32 => bytes.copy_from_slice(&value.to_le_bytes()),
            SampleKind::I16 => {
                let value = if value <= -1.0 {
                    i16::MIN
                } else {
                    (value * i16::MAX as f32).round() as i16
                };
                bytes.copy_from_slice(&value.to_le_bytes());
            }
            SampleKind::I24 => {
                let value = if value <= -1.0 {
                    -8_388_608
                } else {
                    (value * 8_388_607.0).round() as i32
                };
                let packed = value.to_le_bytes();
                bytes.copy_from_slice(&packed[..3]);
            }
            SampleKind::I32 => {
                let value = if value <= -1.0 {
                    i32::MIN
                } else {
                    (value * i32::MAX as f32).round() as i32
                };
                bytes.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    out[count * step..].fill(0);
}

pub(crate) use vox_dsp::channels::duplicate_mono;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_mask_follows_channel_count() {
        assert_eq!(channel_mask(1), 0x4);
        assert_eq!(channel_mask(2), 0x3);
        assert_eq!(channel_mask(6), 0x3F);
    }

    #[test]
    fn float_format_header_is_consistent() {
        let f = float_format(48_000, 2);
        // packed 结构体的字段要先拷出来再比较。
        let (tag, align, avg, cb, mask, sub) = (
            f.Format.wFormatTag,
            f.Format.nBlockAlign,
            f.Format.nAvgBytesPerSec,
            f.Format.cbSize,
            f.dwChannelMask,
            f.SubFormat,
        );
        // 伪设备上这个头是我们自己写死的，tag 必须是 WAVE_FORMAT_EXTENSIBLE。
        // 写成 WAVE_FORMAT_IEEE_FLOAT(3) 的话 dwChannelMask/SubFormat 就不会被读。
        assert_eq!(tag, 0xFFFE);
        assert_eq!(align, 8);
        assert_eq!(avg, 48_000 * 8);
        assert_eq!(cb, 22);
        assert_eq!(mask, 0x3);
        assert_eq!(sub, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        // SAFETY: 联合体里只写过 wValidBitsPerSample，读同一个字段是有效的。
        let valid = unsafe { f.Samples.wValidBitsPerSample };
        assert_eq!(valid, 32);
    }

    #[test]
    fn parse_handmade_float_format() {
        let f = float_format(24_000, 1);
        // SAFETY: 指针来自本地合法结构体，cbSize=22 满足扩展格式要求。
        let info = unsafe { parse_format(&f.Format as *const WAVEFORMATEX) }.unwrap();
        assert_eq!(
            info,
            WaveInfo {
                sample_rate: 24_000,
                channels: 1,
                kind: SampleKind::F32,
                block_align: 4,
            }
        );
    }

    #[test]
    fn i16_converts_to_float() {
        let bytes = [0x00, 0x80, 0xFF, 0x7F, 0x00, 0x00];
        let mut out = Vec::new();
        bytes_to_f32(&bytes, SampleKind::I16, &mut out);
        assert_eq!(out.len(), 3);
        assert!((out[0] + 1.0).abs() < 1e-6);
        assert!((out[1] - 0.999_97).abs() < 1e-4);
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn i24_keeps_sign() {
        // -1.0 = 0x800000（小端字节序 00 00 80）
        let bytes = [0x00, 0x00, 0x80];
        let mut out = Vec::new();
        bytes_to_f32(&bytes, SampleKind::I24, &mut out);
        assert!((out[0] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn non_finite_float_becomes_silence() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&f32::NAN.to_le_bytes());
        bytes.extend_from_slice(&0.5f32.to_le_bytes());
        let mut out = Vec::new();
        bytes_to_f32(&bytes, SampleKind::F32, &mut out);
        assert_eq!(out, vec![0.0, 0.5]);
    }

    #[test]
    fn f32_writes_integer_and_float_device_formats() {
        let samples = [-1.0, 0.0, 1.0];

        let mut i16_bytes = [0u8; 6];
        f32_to_bytes(&samples, SampleKind::I16, &mut i16_bytes);
        assert_eq!(i16::from_le_bytes([i16_bytes[0], i16_bytes[1]]), i16::MIN);
        assert_eq!(i16::from_le_bytes([i16_bytes[2], i16_bytes[3]]), 0);
        assert_eq!(i16::from_le_bytes([i16_bytes[4], i16_bytes[5]]), i16::MAX);

        let mut i24_bytes = [0u8; 9];
        f32_to_bytes(&samples, SampleKind::I24, &mut i24_bytes);
        assert_eq!(&i24_bytes[..3], &[0, 0, 128]);
        assert_eq!(&i24_bytes[3..6], &[0, 0, 0]);
        assert_eq!(&i24_bytes[6..], &[255, 255, 127]);

        let mut f32_bytes = [0u8; 12];
        f32_to_bytes(&samples, SampleKind::F32, &mut f32_bytes);
        assert_eq!(&f32_bytes[..4], &(-1.0f32).to_le_bytes());
        assert_eq!(&f32_bytes[8..], &(1.0f32).to_le_bytes());
    }
}
