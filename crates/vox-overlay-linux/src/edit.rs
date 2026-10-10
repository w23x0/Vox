//! 「调整位置」模式里不碰 GTK 的纯逻辑：边缘命中、离屏判断、去抖/超时判定。
//!
//! 拆出来是为了能在没有显示的环境下单测；`window.rs` 只负责把这些结果接到 GTK 上。

use std::time::Duration;

use vox_core::settings::OverlayGeometry;
use vox_overlay_core::geom::RectI;

/// 边缘命中带宽（px）。Windows 侧是 10（`HIT_TEST_BORDER`），GTK 这边手感偏小放到 12。
pub const EDGE_BORDER: i32 = 12;
/// 拖完/缩放完到回写几何的去抖时间。
pub const REPORT_DEBOUNCE: Duration = Duration::from_millis(200);
/// 编辑模式无任何交互多久后自动退出。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 八个方向。和 `gdk::WindowEdge` 一一对应（映射在 `window.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    North,
    South,
    West,
    East,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

/// 鼠标按下的位置该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Move,
    Resize(Edge),
}

/// 窗口内坐标 `(x, y)` → 命中结果。判定顺序与 Windows `WM_NCHITTEST` 同一套：
/// 先角后边，四个条件都不满足就是移动。
pub fn hit_test(x: f64, y: f64, width: i32, height: i32) -> Hit {
    let (x, y) = (x.floor() as i32, y.floor() as i32);
    let near_left = x < EDGE_BORDER;
    let near_right = x >= width - EDGE_BORDER;
    let near_top = y < EDGE_BORDER;
    let near_bottom = y >= height - EDGE_BORDER;
    match (near_left, near_right, near_top, near_bottom) {
        (true, false, true, false) => Hit::Resize(Edge::NorthWest),
        (false, true, true, false) => Hit::Resize(Edge::NorthEast),
        (true, false, false, true) => Hit::Resize(Edge::SouthWest),
        (false, true, false, true) => Hit::Resize(Edge::SouthEast),
        (true, false, false, false) => Hit::Resize(Edge::West),
        (false, true, false, false) => Hit::Resize(Edge::East),
        (false, false, true, false) => Hit::Resize(Edge::North),
        (false, false, false, true) => Hit::Resize(Edge::South),
        _ => Hit::Move,
    }
}

/// 保存的几何还落在某块显示器里吗。
///
/// 判据：窗口与某块显示器的重叠至少有 `min(宽,64) × min(高,32)`——够用户看见并抓到它。
/// 否则（拔了外接屏、分辨率变小、存档被改坏）调用方回落默认位置。
pub fn geometry_on_screen(geometry: OverlayGeometry, monitors: &[RectI]) -> bool {
    let need_w = (geometry.width as i64).min(64);
    let need_h = (geometry.height as i64).min(32);
    let left = geometry.x as i64;
    let top = geometry.y as i64;
    let right = left + geometry.width as i64;
    let bottom = top + geometry.height as i64;
    monitors.iter().any(|m| {
        let w = right.min(m.right() as i64) - left.max(m.x as i64);
        let h = bottom.min(m.bottom() as i64) - top.max(m.y as i64);
        w >= need_w && h >= need_h
    })
}

/// 位置+大小：`(x, y, w, h)`。
pub type Geo = (i32, i32, i32, i32);

/// 去抖到点后要不要回写：这一次定时器仍是最新一次 configure（`scheduled == current`），
/// 并且几何确实跟上次回写的不同。
pub fn should_report(scheduled_gen: u64, current_gen: u64, last: Geo, now: Geo) -> bool {
    scheduled_gen == current_gen && last != now
}

/// 距上次交互 `idle` 了，该自动退出吗。
pub fn idle_expired(idle: Duration) -> bool {
    idle >= IDLE_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_maps_eight_edges_and_move() {
        let (w, h) = (880, 170);
        assert_eq!(hit_test(440.0, 80.0, w, h), Hit::Move);
        assert_eq!(hit_test(2.0, 80.0, w, h), Hit::Resize(Edge::West));
        assert_eq!(hit_test(878.0, 80.0, w, h), Hit::Resize(Edge::East));
        assert_eq!(hit_test(440.0, 3.0, w, h), Hit::Resize(Edge::North));
        assert_eq!(hit_test(440.0, 168.0, w, h), Hit::Resize(Edge::South));
        assert_eq!(hit_test(1.0, 1.0, w, h), Hit::Resize(Edge::NorthWest));
        assert_eq!(hit_test(879.0, 1.0, w, h), Hit::Resize(Edge::NorthEast));
        assert_eq!(hit_test(1.0, 169.0, w, h), Hit::Resize(Edge::SouthWest));
        assert_eq!(hit_test(879.0, 169.0, w, h), Hit::Resize(Edge::SouthEast));
    }

    #[test]
    fn hit_test_border_is_twelve_pixels() {
        let (w, h) = (880, 170);
        assert_eq!(hit_test(11.9, 80.0, w, h), Hit::Resize(Edge::West));
        assert_eq!(hit_test(12.0, 80.0, w, h), Hit::Move);
        assert_eq!(hit_test(868.0, 80.0, w, h), Hit::Resize(Edge::East));
        assert_eq!(hit_test(867.9, 80.0, w, h), Hit::Move);
    }

    #[test]
    fn tiny_window_never_panics_and_prefers_corners() {
        assert_eq!(hit_test(5.0, 5.0, 20, 20), Hit::Resize(Edge::NorthWest));
    }

    #[test]
    fn geometry_far_offscreen_is_rejected() {
        let monitors = [RectI::new(0, 0, 1920, 1080)];
        let bogus = OverlayGeometry {
            x: -900_000,
            y: -900_000,
            width: 880,
            height: 170,
        };
        assert!(!geometry_on_screen(bogus, &monitors));
    }

    #[test]
    fn geometry_on_any_monitor_is_kept() {
        let monitors = [
            RectI::new(0, 0, 1920, 1080),
            RectI::new(1920, 0, 1280, 1024),
        ];
        let on_second = OverlayGeometry {
            x: 2000,
            y: 800,
            width: 880,
            height: 170,
        };
        assert!(geometry_on_screen(on_second, &monitors));
        // 只剩一条边探在屏内：抓不到，丢掉。
        let sliver = OverlayGeometry {
            x: -870,
            y: 100,
            width: 880,
            height: 170,
        };
        assert!(!geometry_on_screen(sliver, &monitors));
        // 没有显示器信息时不能把所有存档都判成离屏之外的东西：空列表 = 离屏。
        assert!(!geometry_on_screen(on_second, &[]));
    }

    #[test]
    fn report_only_for_latest_configure_and_real_change() {
        let a = (10, 10, 880, 170);
        let b = (50, 10, 880, 170);
        assert!(should_report(3, 3, a, b));
        assert!(
            !should_report(2, 3, a, b),
            "后面又来了新的 configure，这次作废"
        );
        assert!(!should_report(3, 3, a, a), "没变就不写");
    }

    #[test]
    fn idle_timeout_is_thirty_seconds() {
        assert!(!idle_expired(Duration::from_secs(29)));
        assert!(idle_expired(Duration::from_secs(30)));
    }
}
