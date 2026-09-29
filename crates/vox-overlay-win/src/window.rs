//! 悬浮窗线程：窗口类、消息泵、按内容切换穿透/交互、干净退出。
//!
//! 铁律：**HWND 只能在这个线程上碰**。外面的线程往邮箱里放最新状态，然后
//! `PostMessageW` 叫醒本线程，由本线程自己去改窗口。跨线程直接调 Win32 改窗口
//! 会在 DPI 变化、销毁竞争这些边界上出各种玄学问题。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use parking_lot::Mutex;
use vox_core::ports::{PortError, PortResult, SubtitleFrame};
use vox_core::settings::{OverlayGeometry, SubtitleSettings};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, KillTimer, PostQuitMessage, RegisterClassExW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, HTBOTTOM, HTBOTTOMLEFT,
    HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, HTTRANSPARENT,
    HWND_TOPMOST, MA_NOACTIVATE, MSG, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOWNOACTIVATE, WM_DESTROY, WM_DPICHANGED, WM_EXITSIZEMOVE, WM_MOUSEACTIVATE,
    WM_MOVE, WM_NCHITTEST, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};

use crate::canvas::Canvas;
use crate::geom::RectI;
use crate::layout::default_placement;
use crate::render::{FrameInput, Renderer};
use crate::surface::LayeredSurface;
use crate::WINDOW_CLASS;

/// 叫醒消息：邮箱里有新东西了。用 `WM_APP` 之上的编号，不跟系统消息撞。
pub const WM_VOX_WAKE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// 默认窗口宽度和离底边的距离，照搬旧版。
const DEFAULT_WIDTH: i32 = 880;
const DEFAULT_HEIGHT: i32 = 170;
const DEFAULT_BOTTOM_MARGIN: i32 = 80;
/// 窗口尺寸上下限。`lib.rs` 的 `sanitize` 也拿这一组去夹设置，别在那边另写一份数字。
pub(crate) const MIN_WIDTH: i32 = 160;
pub(crate) const MIN_HEIGHT: i32 = 60;
pub(crate) const MAX_WIDTH: i32 = 8192;
pub(crate) const MAX_HEIGHT: i32 = 4096;
const HIT_TEST_BORDER: i32 = 10;
const ANIMATION_TIMER_ID: usize = 0x5642;
const ANIMATION_TIMER_MS: u32 = 16;

/// 拖动或缩放结束时通知宿主，把用户的几何设置写回配置。
pub type GeometryCallback = std::sync::Arc<dyn Fn(OverlayGeometry) + Send + Sync + 'static>;

/// 邮箱：外面写、窗口线程读。**后写覆盖先写**——字幕帧一秒来几十次，
/// 排队只会让窗口画一堆已经过期的帧，留最新的那一份就够。
#[derive(Default)]
pub struct Mailbox {
    pub frame: Option<SubtitleFrame>,
    pub settings: Option<SubtitleSettings>,
    pub visible: Option<bool>,
    pub shutdown: bool,
}

/// 跨线程共享的那一小块状态。
pub struct Shared {
    pub mailbox: Mutex<Mailbox>,
    /// HWND 存成 isize：`windows::HWND` 是裸指针、不是 `Send`，但整数是。
    /// 而且这个句柄**只用来 PostMessage**，绝不拿去改窗口。
    hwnd: AtomicIsize,
    /// 已经投过叫醒消息但线程还没处理：避免 60 Hz 的 render() 把消息队列灌满。
    wake_pending: AtomicBool,
    /// 窗口线程还活着。
    pub alive: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            mailbox: Mutex::new(Mailbox::default()),
            hwnd: AtomicIsize::new(0),
            wake_pending: AtomicBool::new(false),
            alive: AtomicBool::new(false),
        }
    }

    fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::Release);
    }

    fn hwnd(&self) -> Option<HWND> {
        let raw = self.hwnd.load(Ordering::Acquire);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    /// 叫醒窗口线程。已经有一条在路上就不再投。
    pub fn wake(&self) {
        if self.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(hwnd) = self.hwnd() else {
            // 窗口还没建好；线程起来后会主动排空一次邮箱，不会漏。
            self.wake_pending.store(false, Ordering::Release);
            return;
        };
        // SAFETY: PostMessageW 是少数几个可以跨线程调的 Win32 函数——它只是往目标线程的
        // 消息队列里塞一条消息，不触碰窗口状态。句柄失效时它返回错误，不会崩。
        let posted = unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                WM_VOX_WAKE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if posted.is_err() {
            // 窗口已经没了：把标志放回去，别把后续的叫醒也堵死。
            self.wake_pending.store(false, Ordering::Release);
        }
    }
}

/// 窗口线程自己的状态，全部只在本线程访问。
struct WindowState {
    hwnd: HWND,
    shared: std::sync::Arc<Shared>,
    renderer: Renderer,
    canvas: Canvas,
    surface: LayeredSurface,
    settings: SubtitleSettings,
    frame: SubtitleFrame,
    /// 窗口矩形，屏幕坐标。位置、宽度和高度都由用户控制并持久化。
    rect: RectI,
    visible: bool,
    interactive: bool,
    dpi: u32,
    geometry_callback: Option<GeometryCallback>,
}

/// 建窗口 + 跑消息泵，直到收到退出。返回后线程就结束了。
pub fn run(
    shared: std::sync::Arc<Shared>,
    settings: SubtitleSettings,
    geometry_callback: Option<GeometryCallback>,
    ready: std::sync::mpsc::Sender<PortResult<()>>,
) {
    let mut state = match create(&shared, settings, geometry_callback) {
        Ok(s) => {
            // 成功信号发出前就标活：调用方收到成功后 `is_running()` 必须稳定为 true。
            shared.alive.store(true, Ordering::Release);
            let _ = ready.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // 建窗口期间外面可能已经写过邮箱了，先排空一次再进泵。
    state.drain_mailbox();

    let mut msg = MSG::default();
    loop {
        // SAFETY: msg 是本地结构；GetMessageW 返回 0 表示 WM_QUIT，-1 表示出错。
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        // SAFETY: msg 刚由 GetMessageW 填好。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    shared.alive.store(false, Ordering::Release);
    // state 在这里析构：DIB、字体、DC 全还给系统。
}

fn create(
    shared: &std::sync::Arc<Shared>,
    settings: SubtitleSettings,
    geometry_callback: Option<GeometryCallback>,
) -> PortResult<Box<WindowState>> {
    register_class()?;

    let class = wide(WINDOW_CLASS);
    let title = wide("Vox Subtitle");
    // 扩展样式各有分工：
    // LAYERED   —— per-pixel alpha 的前提，真透明只能靠它
    // TOOLWINDOW  —— 不进 Alt+Tab 和任务栏
    // NOACTIVATE  —— 点它不抢 VRChat 的焦点，不然游戏会掉输入
    // TOPMOST     —— 压在游戏窗口上面
    // 没有文字时会在运行中临时加回 WS_EX_TRANSPARENT，避免透明窗口挡住桌面。
    let ex_style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
    // SAFETY: 类已注册；两个字符串都是本地的以 0 结尾的缓冲，调用期间有效。
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            windows::core::PCWSTR(class.as_ptr()),
            windows::core::PCWSTR(title.as_ptr()),
            WS_POPUP | WS_THICKFRAME,
            0,
            0,
            DEFAULT_WIDTH,
            DEFAULT_HEIGHT,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| PortError::new(format!("创建悬浮窗失败: {e}")))?;

    // DPI 必须问，不能假设 96：进程的 DPI 感知由外层 Tauri 应用设置（预期
    // per-monitor v2），这里只查当前窗口所在显示器的实际值。
    // SAFETY: hwnd 刚创建成功。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };

    let renderer = Renderer::new(&settings, dpi, crate::font_factory())?;
    let surface = LayeredSurface::new()?;
    let rect = resolve_geometry(settings.geometry, dpi);

    let mut state = Box::new(WindowState {
        hwnd,
        shared: std::sync::Arc::clone(shared),
        renderer,
        canvas: Canvas::new(0, 0),
        surface,
        settings,
        frame: SubtitleFrame { lines: Vec::new() },
        rect,
        visible: false,
        interactive: true,
        dpi,
        geometry_callback,
    });

    // 把 state 的地址挂到窗口上，wndproc 靠它找回自己。Box 保证地址稳定。
    let ptr = (&mut *state) as *mut WindowState as isize;
    // SAFETY: hwnd 属于本线程，GWLP_USERDATA 是窗口类预留给我们的槽位
    // （类里 cbWndExtra 为 0，用的是标准的 USERDATA）。
    unsafe {
        SetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ptr,
        );
    }
    shared.set_hwnd(hwnd);

    state.set_interactive(false);

    state.redraw();
    Ok(state)
}

fn register_class() -> PortResult<()> {
    // 一个进程只能注册一次；`Overlay` 理论上可以被重建，所以第二次要容忍"已存在"。
    static ONCE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let result = ONCE.get_or_init(|| {
        // SAFETY: 传 None 取当前模块句柄，失败返回 Err。
        let module =
            unsafe { GetModuleHandleW(None) }.map_err(|e| format!("取模块句柄失败: {e}"))?;
        let class = wide(WINDOW_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: windows::core::PCWSTR(class.as_ptr()),
            // 背景刷子留空：分层窗自己全权负责每个像素，让系统刷底色只会闪。
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        // SAFETY: wc 是本地结构，字符串在本作用域内有效（RegisterClassExW 会自己拷走类名）。
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom != 0 {
            return Ok(());
        }
        // SAFETY: 只读线程最后的错误码。
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        if err == windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS {
            Ok(())
        } else {
            Err(format!("注册窗口类失败: {err:?}"))
        }
    });
    result.clone().map_err(PortError::new)
}

/// 恢复上次的位置，落在屏幕外就退回默认摆位。
fn resolve_geometry(saved: Option<OverlayGeometry>, dpi: u32) -> RectI {
    if let Some(g) = saved {
        let rect = RectI::new(
            g.x,
            g.y,
            clamp_width(g.width.min(MAX_WIDTH as u32) as i32),
            clamp_height(g.height.min(MAX_HEIGHT as u32) as i32),
        );
        // 显示器可能被拔了或者分辨率变了：拿窗口中心去问有没有显示器接着，
        // MONITOR_DEFAULTTONULL 保证问不到时返回空句柄而不是硬凑一个。
        let center = POINT {
            x: rect.center_x(),
            y: rect.y + rect.h / 2,
        };
        // SAFETY: 只查询，不改任何状态。
        let mon = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) };
        if !mon.is_invalid() {
            return rect;
        }
        tracing::warn!("上次的悬浮窗位置已不在任何显示器上, 退回默认摆位");
    }
    let work = primary_work_area(dpi);
    default_placement(work, DEFAULT_WIDTH, DEFAULT_HEIGHT, DEFAULT_BOTTOM_MARGIN)
}

/// 主显示器的工作区。用 `rcWork` 而不是 `rcMonitor`，天然避开任务栏。
fn primary_work_area(dpi: u32) -> RectI {
    let origin = POINT { x: 0, y: 0 };
    // SAFETY: 查询主显示器（原点所在的那块），DEFAULTTOPRIMARY 保证一定有结果。
    let mon = unsafe {
        MonitorFromPoint(
            origin,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        )
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mon 有效，info 是本地可写结构且 cbSize 已填。
    let ok = unsafe { GetMonitorInfoW(mon, &mut info) }.as_bool();
    if ok {
        let w = info.rcWork;
        RectI::from_edges(w.left, w.top, w.right, w.bottom)
    } else {
        // 查不到就按 1080p 兜一下，至少窗口还能出现在屏幕上。
        let scale = |v: i32| ((v as i64 * dpi as i64) / 96) as i32;
        RectI::new(0, 0, scale(1920), scale(1040))
    }
}

impl WindowState {
    /// 排空邮箱，把外面写进来的最新状态吃掉。
    fn drain_mailbox(&mut self) {
        let (frame, settings, visible, shutdown) = {
            let mut box_ = self.shared.mailbox.lock();
            (
                box_.frame.take(),
                box_.settings.take(),
                box_.visible.take(),
                box_.shutdown,
            )
        };
        // 先放开 pending，再处理：处理期间来的新内容会重新投一次叫醒，不会丢。
        self.shared.wake_pending.store(false, Ordering::Release);

        if shutdown {
            self.close_window();
            return;
        }
        let mut dirty = false;
        if let Some(s) = settings {
            self.settings = s;
            if let Err(e) = self.renderer.sync_font(&self.settings, self.dpi) {
                tracing::warn!("换字体失败, 沿用旧字体: {}", e.message);
            }
            // 几何设置包含位置、宽度和高度。None 是用户点了“恢复默认”。
            self.rect = resolve_geometry(self.settings.geometry, self.dpi);
            dirty = true;
        }
        if let Some(f) = frame {
            self.frame = f;
            self.set_interactive(self.visible && !self.frame.lines.is_empty());
            dirty = true;
        }
        if let Some(v) = visible {
            if v != self.visible {
                self.visible = v;
                self.apply_visibility();
                dirty = false; // apply_visibility 里已经重画过了
            }
        }
        if dirty && self.visible {
            self.redraw();
        }
    }

    fn apply_visibility(&mut self) {
        if self.visible {
            self.set_interactive(!self.frame.lines.is_empty());
            self.redraw();
            // SW_SHOWNOACTIVATE：显示但不抢焦点，配合 WS_EX_NOACTIVATE。
            // SAFETY: hwnd 属于本线程且尚未销毁。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                // 置顶偶尔会被别的全屏窗口顶掉，显示时重新声明一次。
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        } else {
            self.set_interactive(false);
            // SAFETY: 同上。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), ANIMATION_TIMER_ID);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// 画一帧并上屏。窗口尺寸是用户控制的固定视口，内容只在视口内换行和滚动。
    fn redraw(&mut self) {
        let input = FrameInput {
            frame: &self.frame,
            settings: &self.settings,
            client_width: self.rect.w.max(1),
            client_height: self.rect.h.max(1),
            dpi: self.dpi,
        };
        self.renderer.draw(&mut self.canvas, &input);
        self.update_animation_timer();

        if let Err(e) = self
            .surface
            .present(self.hwnd, self.rect, self.canvas.bytes())
        {
            tracing::warn!("悬浮窗上屏失败: {}", e.message);
        }
    }

    fn update_animation_timer(&self) {
        // The app frame loop normally supplies 30 fps frames, but the native overlay
        // also drives itself so a one-off `Overlay::render` cannot leave a transition
        // frozen halfway through.
        unsafe {
            if self.renderer.is_animating() {
                let _ = SetTimer(
                    Some(self.hwnd),
                    ANIMATION_TIMER_ID,
                    ANIMATION_TIMER_MS,
                    None,
                );
            } else {
                let _ = KillTimer(Some(self.hwnd), ANIMATION_TIMER_ID);
            }
        }
    }

    /// 窗口被拖到另一块 DPI 不同的屏上。
    fn on_dpi_changed(&mut self, dpi: u32, suggested: Option<RECT>) {
        let dpi = if (72..=1200).contains(&dpi) { dpi } else { 96 };
        if dpi == self.dpi {
            return;
        }
        self.dpi = dpi;
        if let Err(e) = self.renderer.sync_font(&self.settings, dpi) {
            tracing::warn!("DPI 变化后换字体失败: {}", e.message);
        }
        // 系统给的建议矩形已经按新 DPI 缩放过，采纳它的位置和尺寸。
        if let Some(r) = suggested {
            self.rect = rect_from_win32(r);
        }
        self.redraw();
    }

    fn set_interactive(&mut self, interactive: bool) {
        if self.interactive == interactive {
            return;
        }
        self.interactive = interactive;
        // 空帧/隐藏状态加回透明扩展样式，让透明窗口不挡住下面的应用；有字时
        // 去掉它，窗口过程才能收到拖动和缩放所需的鼠标消息。
        let mut style = unsafe {
            GetWindowLongPtrW(
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE,
            )
        } as usize;
        if interactive {
            style &= !(windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT.0 as usize);
        } else {
            style |= windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT.0 as usize;
        }
        unsafe {
            SetWindowLongPtrW(
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE,
                style as isize,
            );
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    fn sync_rect_from_window(&mut self) {
        let mut r = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut r) }.is_ok() {
            self.rect = rect_from_win32(r);
        }
    }

    fn persist_geometry(&self) {
        if let Some(callback) = &self.geometry_callback {
            callback(OverlayGeometry {
                x: self.rect.x,
                y: self.rect.y,
                width: self.rect.w as u32,
                height: self.rect.h as u32,
            });
        }
    }

    fn close_window(&mut self) {
        // SAFETY: hwnd 属于本线程；销毁会走到 WM_DESTROY，那里再 PostQuitMessage。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), ANIMATION_TIMER_ID);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// 窗口过程。所有消息都在悬浮窗线程上到达。
///
/// SAFETY 约定：`GWLP_USERDATA` 里存的是 `create` 里 `Box` 出来的 `WindowState` 地址，
/// 从建窗到 `WM_DESTROY` 之间一直有效，而且只有本线程会取。
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: 读窗口自己的 USERDATA 槽；建窗初期还是 0，下面判空。
    let ptr =
        unsafe { GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA) }
            as *mut WindowState;
    if ptr.is_null() {
        // SAFETY: 状态还没挂上（WM_CREATE 之类），交给系统默认处理。
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: 指针来自本线程的 Box，且窗口过程不会重入到同一条消息上；
    // 借用只活在这个函数体内。
    let state = unsafe { &mut *ptr };

    match msg {
        WM_VOX_WAKE => {
            state.drain_mailbox();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ANIMATION_TIMER_ID => {
            state.redraw();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // wParam 低 16 位是新的 X 轴 DPI，lParam 指向系统建议的新窗口矩形。
            let dpi = (wp.0 & 0xffff) as u32;
            let suggested = if lp.0 == 0 {
                None
            } else {
                // SAFETY: WM_DPICHANGED 保证 lParam 指向一个有效的 RECT。
                Some(unsafe { *(lp.0 as *const RECT) })
            };
            state.on_dpi_changed(dpi, suggested);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            if !state.interactive {
                return LRESULT(HTTRANSPARENT as isize);
            }
            let x = signed_low_word(lp.0 as usize);
            let y = signed_high_word(lp.0 as usize);
            let left = state.rect.x;
            let top = state.rect.y;
            let right = state.rect.right();
            let bottom = state.rect.bottom();
            let near_left = x < left + HIT_TEST_BORDER;
            let near_right = x >= right - HIT_TEST_BORDER;
            let near_top = y < top + HIT_TEST_BORDER;
            let near_bottom = y >= bottom - HIT_TEST_BORDER;
            let hit = match (near_left, near_right, near_top, near_bottom) {
                (true, false, true, false) => HTTOPLEFT,
                (false, true, true, false) => HTTOPRIGHT,
                (true, false, false, true) => HTBOTTOMLEFT,
                (false, true, false, true) => HTBOTTOMRIGHT,
                (true, false, false, false) => HTLEFT,
                (false, true, false, false) => HTRIGHT,
                (false, false, true, false) => HTTOP,
                (false, false, false, true) => HTBOTTOM,
                _ => HTCAPTION,
            };
            LRESULT(hit as isize)
        }
        WM_MOVE | WM_SIZE => {
            // 拖动/缩放过程中每个 WM_* 都同步本地矩形，避免下一帧重绘把
            // 窗口又贴回拖动开始前的位置。
            state.sync_rect_from_window();
            if msg == WM_SIZE {
                state.redraw();
            }
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            state.sync_rect_from_window();
            state.persist_geometry();
            LRESULT(0)
        }
        WM_DESTROY => {
            // 先把句柄从共享区摘掉，之后外面的 wake() 就不会再往这儿投消息。
            state.shared.hwnd.store(0, Ordering::Release);
            // SAFETY: 退出消息泵；state 由 run() 里的 Box 负责析构。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: 其余消息交给系统。
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn signed_low_word(value: usize) -> i32 {
    (value as u16 as i16) as i32
}

fn signed_high_word(value: usize) -> i32 {
    ((value >> 16) as u16 as i16) as i32
}

/// Win32 的 `RECT`（left/top/right/bottom）→ 本 crate 的矩形，宽高按窗口上下限夹好。
///
/// 拖动/缩放（`WM_MOVE`/`WM_SIZE`/`WM_EXITSIZEMOVE`）和 DPI 变化都会拿到系统给的
/// `RECT`，两处共用这一套换算。
fn rect_from_win32(r: RECT) -> RectI {
    RectI::new(
        r.left,
        r.top,
        clamp_width(r.right - r.left),
        clamp_height(r.bottom - r.top),
    )
}

fn clamp_width(width: i32) -> i32 {
    width.clamp(MIN_WIDTH, MAX_WIDTH)
}

fn clamp_height(height: i32) -> i32 {
    height.clamp(MIN_HEIGHT, MAX_HEIGHT)
}

/// 把设置里带的几何尺寸夹进窗口允许的范围。`lib.rs` 的 `sanitize` 用它，
/// 跟 `clamp_width`/`clamp_height` 共用同一组上下限。
pub(crate) fn clamp_geometry(width: u32, height: u32) -> (u32, u32) {
    (
        width.clamp(MIN_WIDTH as u32, MAX_WIDTH as u32),
        height.clamp(MIN_HEIGHT as u32, MAX_HEIGHT as u32),
    )
}

/// Rust 字符串转以 0 结尾的 UTF-16 缓冲。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_null_terminated() {
        let w = wide("Vox");
        assert_eq!(w.len(), "Vox".len() + 1);
        assert_eq!(w.last().copied(), Some(0));
    }

    #[test]
    fn wake_without_a_window_does_not_get_stuck() {
        // 窗口还没建好时 wake() 必须把 pending 放回去，否则真窗口起来后第一次
        // 叫醒会被吞掉，字幕就永远不刷新。
        let shared = Shared::new();
        shared.wake();
        assert!(
            !shared.wake_pending.load(Ordering::Acquire),
            "没有窗口时 pending 必须复位"
        );
        shared.wake();
        assert!(!shared.wake_pending.load(Ordering::Acquire));
    }

    #[test]
    fn default_geometry_is_used_when_nothing_is_saved() {
        // 需要真实显示器信息，但没有窗口也能跑：MonitorFromPoint 在有桌面的
        // 会话里总能给出主显示器。
        let rect = resolve_geometry(None, 96);
        assert_eq!(rect.w, DEFAULT_WIDTH);
        assert!(rect.h > 0);
    }

    #[test]
    fn saved_geometry_far_offscreen_falls_back() {
        let bogus = OverlayGeometry {
            x: -900_000,
            y: -900_000,
            width: 880,
            height: 170,
        };
        let rect = resolve_geometry(Some(bogus), 96);
        assert_ne!(rect.x, bogus.x, "屏幕外的旧位置必须被丢掉");
    }

    #[test]
    fn saved_geometry_on_screen_is_honored() {
        let work = primary_work_area(96);
        let g = OverlayGeometry {
            x: work.x + 40,
            y: work.y + 40,
            width: 700,
            height: 150,
        };
        let rect = resolve_geometry(Some(g), 96);
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (g.x, g.y, 700, 150));
    }
}
