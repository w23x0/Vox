//! 字幕窗摆放的纯逻辑（不碰 GTK，能在没有显示的环境里单测）：离屏回落、尺寸夹值、
//! 「内容需要的高度」撑出来的实际窗口矩形。`window.rs` 只负责把这些结果接到 GTK 上。

use vox_core::settings::OverlayGeometry;
use vox_overlay_core::geom::RectI;

/// 窗口尺寸下限，和设置夹值（`vox-core` 的 160×60）一致。
pub const MIN_WIDTH: i32 = 160;
pub const MIN_HEIGHT: i32 = 60;
/// 没存过几何时的默认窗口大小。
pub const DEFAULT_WIDTH: i32 = 880;
pub const DEFAULT_HEIGHT: i32 = 170;

/// 位置+大小：`(x, y, w, h)`。
pub type Geo = (i32, i32, i32, i32);

/// 把 `(x, y, w, h)` 夹进合法范围：宽高不低于下限（超大值也不溢出 `i32`）。
pub fn clamp_geo(x: i32, y: i32, w: i32, h: i32) -> Geo {
    (x, y, w.max(MIN_WIDTH), h.max(MIN_HEIGHT))
}

/// 设置里的几何（`u32`）→ 夹过的 [`Geo`]。
pub fn geo_of(geometry: OverlayGeometry) -> Geo {
    let cap = |v: u32| v.min(i32::MAX as u32) as i32;
    clamp_geo(
        geometry.x,
        geometry.y,
        cap(geometry.width),
        cap(geometry.height),
    )
}

/// 实际显示的窗口矩形：用户设的矩形 `base` 是起点，内容需要的高度 `required_h`
/// （区域数 × 行数 × 行距 + 边距）比它高就**往上**长，底边不动；内容矮了就缩回 `base` 的高度。
/// 这样"行数 / 字号 / 在跑轨数"变大变小，窗口都能跟上，不会把最上面一行裁掉。
pub fn shown_rect(base: Geo, required_h: i32) -> Geo {
    let h = base.3.max(required_h);
    (base.0, base.1 + base.3 - h, base.2, h)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn clamp_keeps_minimum_size() {
        assert_eq!(clamp_geo(5, 6, 10, 10), (5, 6, MIN_WIDTH, MIN_HEIGHT));
        assert_eq!(clamp_geo(5, 6, 900, 200), (5, 6, 900, 200));
        let huge = OverlayGeometry {
            x: 0,
            y: 0,
            width: u32::MAX,
            height: 0,
        };
        assert_eq!(geo_of(huge), (0, 0, i32::MAX, MIN_HEIGHT));
    }

    #[test]
    fn shown_rect_grows_upward_and_shrinks_back() {
        let base = (100, 500, 880, 170);
        // 内容矮于用户高度：原样。
        assert_eq!(shown_rect(base, 134), base);
        assert_eq!(shown_rect(base, 1), base);
        // 内容更高：底边（y+h=670）不动，向上长。
        assert_eq!(shown_rect(base, 226), (100, 444, 880, 226));
        assert_eq!(shown_rect(base, 226).1 + 226, base.1 + base.3);
        // 内容又矮了（行数调小）：回到用户设的矩形。
        assert_eq!(shown_rect(base, 100), base);
    }
}
