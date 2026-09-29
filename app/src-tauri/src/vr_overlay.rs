//! SteamVR/OpenVR 头显字幕。
//!
//! 这里只复制 Listen 轨道；字幕内容、字体、颜色、底衬和逐字淡出全部复用
//! `vox-core` 与 `vox-overlay-win` 的现有渲染路径。SteamVR 不可用时线程保持
//! 低频等待，不影响桌面悬浮窗或 VRChat OSC。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openvr::overlay::OverlayHandle;
use openvr::pose::Matrix3x4;
use openvr::tracked_device_index;
use openvr::{ApplicationType, Context, Overlay};
use parking_lot::Mutex;
use vox_core::capability::CapabilityStatus;
use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::runtime::Runtime;
use vox_core::subtitle::Track;
use vox_overlay_win::canvas::Canvas;
use vox_overlay_win::render::{FrameInput, Renderer};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const WIDTH: i32 = 880;
const HEIGHT: i32 = 170;
const DPI: u32 = 96;
const OVERLAY_KEY: &str = "org.vox.overlay.listen\0";
const OVERLAY_NAME: &str = "Vox Listen Subtitles\0";

static STOP: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// 线程起没起来（`start` / `stop` 写）。
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Overlay 真的建出来了没有（`run` 里 `Backend::connect()` 成功才置位，掉线就清）。
///
/// 这是 `vr_captions` 这一位的定义者（§2.5.4）：位为真凭的是**真的连上了 OpenVR**，
/// 而不是"这个构建编了 feature"。
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// OpenVR 运行期在不在 + HMD 在不在。`run()` 等的是同一对探针，[`status()`] 也用它们
/// 区分"这台机器没有头显"（`unsupported`）与"有头显但还没连上"（`busy`）。
pub fn hmd_ready() -> bool {
    openvr::is_runtime_installed() && openvr::is_hmd_present()
}

/// `vr_captions` 这一位现在该不该开着（§2.5.4）。
///
/// **不**看 `settings.vr_overlay_enabled`：位是"能不能"，那个开关是"要不要"（§2.6 R5）。
///
/// 判据本体在装配层的纯函数 `platform::vr_captions_status_for` 里（四件事实 → 位）：
/// 那个函数在任何平台上都编得到、也被单测钉住；这里只负责把三个探针读出来
/// （`RUNNING` / `CONNECTED` 两个原子 + 运行期与头显探针），`built` 恒为真——
/// 这个模块只在编了 `steamvr-overlay` 时才存在。
pub fn status() -> CapabilityStatus {
    crate::platform::vr_captions_status_for(
        true,
        RUNNING.load(Ordering::Acquire),
        CONNECTED.load(Ordering::Acquire),
        hmd_ready(),
    )
}

pub fn start(runtime: Runtime) {
    stop();
    STOP.store(false, Ordering::Release);
    let handle = std::thread::Builder::new()
        .name("vox-vr-overlay".into())
        .spawn(move || run(runtime))
        .ok();
    RUNNING.store(handle.is_some(), Ordering::Release);
    *THREAD.lock() = handle;
}

pub fn stop() {
    STOP.store(true, Ordering::Release);
    if let Some(handle) = THREAD.lock().take() {
        let _ = handle.join();
    }
    // 线程停了就什么都没连着：位必须跟着翻假（§2.5.4）。
    RUNNING.store(false, Ordering::Release);
    CONNECTED.store(false, Ordering::Release);
}

struct Backend {
    _context: Context,
    overlay: Overlay,
    handle: OverlayHandle,
}

impl Backend {
    fn connect() -> Result<Self, String> {
        let context = unsafe { openvr::init(ApplicationType::Background) }
            .map_err(|error| format!("OpenVR 初始化失败: {error:?}"))?;
        let mut overlay = context
            .overlay()
            .map_err(|error| format!("OpenVR Overlay 接口失败: {error:?}"))?;
        let handle = overlay
            .create_overlay(OVERLAY_KEY, OVERLAY_NAME)
            .map_err(|error| format!("创建字幕 Overlay 失败: {error:?}"))?;
        overlay
            .set_width(handle, 1.2)
            .map_err(|error| format!("设置字幕 Overlay 尺寸失败: {error:?}"))?;
        let transform = Matrix3x4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, -0.28],
            [0.0, 0.0, 1.0, -1.2],
        ]);
        overlay
            .set_transform_tracked_device_relative(handle, tracked_device_index::HMD, &transform)
            .map_err(|error| format!("定位字幕 Overlay 失败: {error:?}"))?;
        Ok(Self {
            _context: context,
            overlay,
            handle,
        })
    }

    fn hide(&mut self) {
        let _ = self.overlay.set_visibility(self.handle, false);
    }
}

fn run(runtime: Runtime) {
    let mut backend: Option<Backend> = None;
    let mut next_retry = std::time::Instant::now();
    let mut renderer: Option<Renderer> = None;
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let mut rgba = vec![0_u8; (WIDTH * HEIGHT * 4) as usize];
    let mut last_hash = None;

    while !STOP.load(Ordering::Acquire) {
        let settings = runtime.settings().subtitle;
        if !settings.vr_overlay_enabled || !settings.visible {
            if let Some(active) = backend.as_mut() {
                active.hide();
            }
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        }

        if !hmd_ready() {
            if let Some(active) = backend.as_mut() {
                active.hide();
            }
            backend = None;
            renderer = None;
            last_hash = None;
            CONNECTED.store(false, Ordering::Release);
            std::thread::sleep(RETRY_INTERVAL);
            continue;
        }

        if backend.is_none() && std::time::Instant::now() >= next_retry {
            match Backend::connect() {
                Ok(active) => {
                    renderer = Renderer::new(&settings, DPI, vox_overlay_win::font_factory()).ok();
                    backend = Some(active);
                    last_hash = None;
                    // 真的建出 Overlay 了：这是 `vr_captions` 为真的唯一凭据。
                    CONNECTED.store(true, Ordering::Release);
                }
                Err(error) => {
                    tracing::warn!(%error, "Vox SteamVR Overlay 连接失败");
                    next_retry = std::time::Instant::now() + RETRY_INTERVAL;
                }
            }
        }

        let Some(active) = backend.as_mut() else {
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        };
        let Some(renderer_ref) = renderer.as_mut() else {
            active.hide();
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        };
        if renderer_ref.sync_font(&settings, DPI).is_err() {
            active.hide();
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        }

        let source = runtime.subtitle_frame();
        let frame = SubtitleFrame {
            lines: source
                .lines
                .into_iter()
                .filter(|line| line.track == Track::Listen)
                .collect::<Vec<SubtitleLine>>(),
        };
        if frame.lines.is_empty() {
            active.hide();
            last_hash = None;
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        }

        let input = FrameInput {
            frame: &frame,
            settings: &settings,
            client_width: WIDTH,
            client_height: HEIGHT,
            dpi: DPI,
        };
        renderer_ref.draw(&mut canvas, &input);
        premultiplied_bgra_to_rgba(canvas.bytes(), &mut rgba);
        let mut hasher = DefaultHasher::new();
        rgba.hash(&mut hasher);
        let hash = hasher.finish();
        if last_hash != Some(hash) {
            if let Err(error) = active.overlay.set_raw_data(
                active.handle,
                &rgba,
                WIDTH as usize,
                HEIGHT as usize,
                4,
            ) {
                tracing::warn!(?error, "上传 SteamVR 字幕纹理失败");
                active.hide();
                backend = None;
                renderer = None;
                next_retry = std::time::Instant::now() + RETRY_INTERVAL;
                continue;
            }
            last_hash = Some(hash);
        }
        let _ = active.overlay.set_visibility(active.handle, true);
        std::thread::sleep(FRAME_INTERVAL);
    }

    if let Some(mut active) = backend {
        active.hide();
    }
}

/// `vox-overlay-win` 使用预乘 BGRA；OpenVR 的原始 Overlay 接口要求 RGBA。
fn premultiplied_bgra_to_rgba(source: &[u8], target: &mut [u8]) {
    for (src, dst) in source.chunks_exact(4).zip(target.chunks_exact_mut(4)) {
        let alpha = src[3];
        if alpha == 0 {
            dst.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let unpremultiply = |value: u8| -> u8 {
            ((value as u16 * 255 + alpha as u16 / 2) / alpha as u16).min(255) as u8
        };
        dst[0] = unpremultiply(src[2]);
        dst[1] = unpremultiply(src[1]);
        dst[2] = unpremultiply(src[0]);
        dst[3] = alpha;
    }
}

#[cfg(test)]
mod tests {
    use super::premultiplied_bgra_to_rgba;

    #[test]
    fn converts_premultiplied_bgra_to_rgba() {
        let source = [20, 40, 60, 128];
        let mut target = [0; 4];
        premultiplied_bgra_to_rgba(&source, &mut target);
        assert_eq!(target, [120, 80, 40, 128]);
    }

    #[test]
    fn clears_fully_transparent_pixels() {
        let mut target = [1; 4];
        premultiplied_bgra_to_rgba(&[1, 2, 3, 0], &mut target);
        assert_eq!(target, [0, 0, 0, 0]);
    }
}
