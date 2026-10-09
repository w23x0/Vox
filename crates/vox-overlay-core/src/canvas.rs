//! CPU 画布：预乘 BGRA 像素缓冲 + 抗锯齿圆角矩形 + 覆盖率蒙版合成。
//!
//! 所有绘制都是纯运算，跟 Win32 无关，能脱离桌面单测。窗口线程画完一帧再把
//! 整块像素 memcpy 进 DIB 交给 `UpdateLayeredWindow`。
//!
//! 圆角和字形边缘都靠**覆盖率**上色（0..255 表示这个像素被形状盖了多少），
//! 覆盖率乘进 alpha 之后再预乘颜色——这样半覆盖的边缘像素同时变淡又变透，
//! 才能跟背后的桌面正确融合。

use crate::color::{alpha_to_u8, blend_over, mul_u8, Bgra, Rgb};
use crate::geom::RectI;

/// 一块灰度覆盖率蒙版（GDI 光栅出来的字形，或任何 8 位 mask）。
#[derive(Debug, Clone, Copy)]
pub struct Mask<'a> {
    pub w: i32,
    pub h: i32,
    /// 长度至少 `w * h`，每字节 0..255 的覆盖率。
    pub cov: &'a [u8],
}

/// 预乘 BGRA 画布，行优先、顶行在前（DIB 用负高度拿到的同一种排布）。
#[derive(Debug, Clone)]
pub struct Canvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
    /// 纵向裁剪带（`[顶, 底)` 行闭区间）。每条字幕区域画自己的那一段，
    /// 正在上浮/下落的那一行因此不会画到隔壁区域上去。
    clip: Vec<(i32, i32)>,
}

impl Canvas {
    /// 建一块全透明画布。宽高非正时退化成 0×0，后续绘制全部变成空操作。
    pub fn new(width: i32, height: i32) -> Self {
        let (width, height) = (width.max(0), height.max(0));
        let len = (width as usize) * (height as usize) * 4;
        Self {
            width,
            height,
            pixels: vec![0u8; len],
            clip: Vec::new(),
        }
    }

    /// 把绘制限制在 `[top, bottom)` 这几行里，直到 [`Canvas::pop_clip`]。
    ///
    /// 与已有的裁剪带求交，所以嵌套调用只会越来越窄。
    pub fn push_clip(&mut self, top: i32, bottom: i32) {
        let (top, bottom) = (top.max(0), bottom.min(self.height));
        let narrowed = match self.clip.last() {
            Some(&(old_top, old_bottom)) => (top.max(old_top), bottom.min(old_bottom)),
            None => (top, bottom),
        };
        self.clip.push(narrowed);
    }

    /// 弹出最近一层裁剪带。
    pub fn pop_clip(&mut self) {
        self.clip.pop();
    }

    /// 这一行现在画不画。
    fn row_visible(&self, y: i32) -> bool {
        self.clip
            .last()
            .is_none_or(|&(top, bottom)| y >= top && y < bottom)
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// 给 `UpdateLayeredWindow` 用的原始字节。
    pub fn bytes(&self) -> &[u8] {
        &self.pixels
    }

    /// 尺寸变了就重开一块；没变只清空。窗口高度随字幕区域数伸缩，这条路很常走。
    pub fn resize(&mut self, width: i32, height: i32) {
        let (width, height) = (width.max(0), height.max(0));
        if width == self.width && height == self.height {
            self.clear();
            return;
        }
        // 重新分配就等于新建一块（夹负和清零都在 `new` 里）。
        *self = Self::new(width, height);
    }

    /// 全部抹成透明。窗口范围内没画到的像素一律不上屏。
    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        Some(((y as usize) * (self.width as usize) + (x as usize)) * 4)
    }

    pub fn pixel(&self, x: i32, y: i32) -> Bgra {
        match self.index(x, y).and_then(|i| self.pixels.get(i..i + 4)) {
            Some(px) => Bgra {
                b: px[0],
                g: px[1],
                r: px[2],
                a: px[3],
            },
            None => Bgra::CLEAR,
        }
    }

    /// 直接写一个像素（不混合）。越界丢弃。
    pub fn set_pixel(&mut self, x: i32, y: i32, value: Bgra) {
        if let Some(i) = self.index(x, y) {
            if let Some(px) = self.pixels.get_mut(i..i + 4) {
                px[0] = value.b;
                px[1] = value.g;
                px[2] = value.r;
                px[3] = value.a;
            }
        }
    }

    /// source-over 混一个像素。越界丢弃。
    pub fn blend_pixel(&mut self, x: i32, y: i32, src: Bgra) {
        if src.a == 0 {
            return;
        }
        let dst = self.pixel(x, y);
        self.set_pixel(x, y, blend_over(dst, src));
    }

    /// 实心矩形，无抗锯齿（整数边界用不上）。
    pub fn fill_rect(&mut self, rect: RectI, color: Rgb, alpha: u8) {
        if rect.is_empty() || alpha == 0 {
            return;
        }
        let src = Bgra::premultiplied(color, alpha);
        for y in rect.y..rect.bottom() {
            if !self.row_visible(y) {
                continue;
            }
            for x in rect.x..rect.right() {
                self.blend_pixel(x, y, src);
            }
        }
    }

    /// 抗锯齿圆角实心矩形。字幕底衬使用它。
    pub fn fill_round_rect(&mut self, rect: RectI, radius: f32, color: Rgb, alpha: u8) {
        self.round_rect_coverage(rect, radius, color, alpha, None);
    }

    /// 抗锯齿圆角描边，线宽 `thickness` 像素，压在边界上。
    pub fn stroke_round_rect(
        &mut self,
        rect: RectI,
        radius: f32,
        thickness: f32,
        color: Rgb,
        alpha: u8,
    ) {
        self.round_rect_coverage(rect, radius, color, alpha, Some(thickness.max(0.1)));
    }

    /// 圆角矩形的公共实现：`stroke` 为 `None` 是填充，为 `Some(w)` 是描边。
    fn round_rect_coverage(
        &mut self,
        rect: RectI,
        radius: f32,
        color: Rgb,
        alpha: u8,
        stroke: Option<f32>,
    ) {
        if rect.is_empty() || alpha == 0 {
            return;
        }
        let half_w = rect.w as f32 / 2.0;
        let half_h = rect.h as f32 / 2.0;
        let radius = radius.clamp(0.0, half_w.min(half_h));
        let cx = rect.x as f32 + half_w;
        let cy = rect.y as f32 + half_h;

        // 描边要往外扩半个线宽才能把外侧那圈抗锯齿像素算进来。
        let pad = stroke.map_or(1, |t| (t / 2.0).ceil() as i32 + 1);
        let x0 = rect.x - pad;
        let x1 = rect.right() + pad;
        let y0 = rect.y - pad;
        let y1 = rect.bottom() + pad;

        for y in y0..y1 {
            if !self.row_visible(y) {
                continue;
            }
            for x in x0..x1 {
                // 取像素中心算距离，边界上就得到 0.5 的覆盖率。
                let d = round_rect_sdf(
                    x as f32 + 0.5 - cx,
                    y as f32 + 0.5 - cy,
                    half_w,
                    half_h,
                    radius,
                );
                let coverage = match stroke {
                    // 填充：内部(d<0)满覆盖，跨边界那 1 px 线性过渡。
                    None => 0.5 - d,
                    // 描边：|d| 落在半个线宽内算命中。
                    Some(t) => t / 2.0 + 0.5 - d.abs(),
                };
                let coverage = coverage.clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let px_alpha = mul_u8(alpha, alpha_to_u8(coverage));
                self.blend_pixel(x, y, Bgra::premultiplied(color, px_alpha));
            }
        }
    }

    /// 把一块灰度覆盖率蒙版当成某个颜色画上去，整块再乘一个 `alpha`。
    ///
    /// **这是逐字 alpha 的落点**：`alpha` 是这个字当前的不透明度，蒙版里的值是
    /// 字形抗锯齿覆盖率。两者相乘得到最终 alpha，再预乘颜色——所以边缘像素是
    /// "淡的字色 + 低 alpha"，而不是"字色混黑再全不透明"，不会有黑边。
    pub fn blend_mask(&mut self, origin: (i32, i32), mask: Mask<'_>, color: Rgb, alpha: u8) {
        if alpha == 0 || mask.w <= 0 || mask.h <= 0 {
            return;
        }
        let (ox, oy) = origin;
        for my in 0..mask.h {
            if !self.row_visible(oy + my) {
                continue;
            }
            let row = (my as usize) * (mask.w as usize);
            for mx in 0..mask.w {
                let coverage = match mask.cov.get(row + mx as usize) {
                    Some(c) => *c,
                    None => continue,
                };
                if coverage == 0 {
                    continue;
                }
                let px_alpha = mul_u8(coverage, alpha);
                if px_alpha == 0 {
                    continue;
                }
                self.blend_pixel(ox + mx, oy + my, Bgra::premultiplied(color, px_alpha));
            }
        }
    }

    /// 自检用：有没有违反预乘不变量的像素（会冒亮边的那种）。
    pub fn find_invalid_pixel(&self) -> Option<(i32, i32, Bgra)> {
        for y in 0..self.height {
            for x in 0..self.width {
                let px = self.pixel(x, y);
                if !px.is_valid_premultiplied() {
                    return Some((x, y, px));
                }
            }
        }
        None
    }
}

/// 圆角矩形的有符号距离场：以矩形中心为原点，负数在内、正数在外。
fn round_rect_sdf(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = px.abs() - (half_w - radius);
    let qy = py.abs() - (half_h - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_canvas_is_fully_transparent() {
        let c = Canvas::new(4, 3);
        assert_eq!(c.bytes().len(), 4 * 3 * 4);
        assert!(c.bytes().iter().all(|b| *b == 0));
        assert_eq!(c.pixel(0, 0), Bgra::CLEAR);
    }

    #[test]
    fn out_of_bounds_access_is_ignored() {
        let mut c = Canvas::new(2, 2);
        c.set_pixel(-1, 0, Bgra::premultiplied(Rgb::WHITE, 255));
        c.set_pixel(0, 99, Bgra::premultiplied(Rgb::WHITE, 255));
        assert_eq!(c.pixel(-5, -5), Bgra::CLEAR, "越界读要给透明，不许 panic");
        assert!(c.bytes().iter().all(|b| *b == 0));
    }

    #[test]
    fn degenerate_size_is_harmless() {
        let mut c = Canvas::new(-4, 10);
        assert_eq!((c.width(), c.height()), (0, 10));
        c.fill_rect(RectI::new(0, 0, 5, 5), Rgb::WHITE, 255);
        c.fill_round_rect(RectI::new(0, 0, 5, 5), 3.0, Rgb::WHITE, 255);
        assert!(c.bytes().is_empty());
    }

    #[test]
    fn fill_rect_writes_premultiplied_pixels() {
        let mut c = Canvas::new(8, 8);
        c.fill_rect(RectI::new(2, 2, 4, 4), Rgb::new(0, 0, 0), 165);
        assert_eq!(c.pixel(0, 0), Bgra::CLEAR, "矩形外面必须还是透明");
        let inside = c.pixel(3, 3);
        assert_eq!(inside.a, 165);
        assert!(inside.is_valid_premultiplied());
        assert!(c.find_invalid_pixel().is_none());
    }

    #[test]
    fn round_rect_corners_are_antialiased_not_square() {
        let mut c = Canvas::new(40, 40);
        c.fill_round_rect(RectI::new(4, 4, 32, 32), 10.0, Rgb::WHITE, 255);
        assert_eq!(c.pixel(20, 20).a, 255, "正中间应当满不透明");
        assert_eq!(c.pixel(4, 4).a, 0, "圆角处的角点不该被填上");
        // 圆角弧线上必然存在部分覆盖的像素。
        let has_partial = (4..36)
            .flat_map(|y| (4..36).map(move |x| (x, y)))
            .any(|(x, y)| {
                let a = c.pixel(x, y).a;
                a > 0 && a < 255
            });
        assert!(has_partial, "圆角边缘应当有抗锯齿的半透明像素");
        assert!(c.find_invalid_pixel().is_none(), "抗锯齿也不许破坏预乘");
    }

    #[test]
    fn stroke_round_rect_leaves_interior_untouched() {
        let mut c = Canvas::new(40, 40);
        c.stroke_round_rect(RectI::new(4, 4, 32, 32), 8.0, 1.0, Rgb::WHITE, 200);
        assert_eq!(c.pixel(20, 20).a, 0, "描边不该填内部");
        let edge_hit = (4..36).any(|x| c.pixel(x, 4).a > 0);
        assert!(edge_hit, "上边缘应当被描上");
        assert!(c.find_invalid_pixel().is_none());
    }

    #[test]
    fn mask_alpha_multiplies_coverage() {
        let mut c = Canvas::new(4, 1);
        // 覆盖率 0 / 半 / 满，整体再乘 50%。
        let cov = [0u8, 128, 255, 64];
        c.blend_mask(
            (0, 0),
            Mask {
                w: 4,
                h: 1,
                cov: &cov,
            },
            Rgb::WHITE,
            128,
        );
        assert_eq!(c.pixel(0, 0).a, 0, "零覆盖不画");
        assert_eq!(c.pixel(1, 0).a, mul_u8(128, 128));
        assert_eq!(c.pixel(2, 0).a, 128, "满覆盖时 alpha 就是逐字 alpha");
        assert!(c.find_invalid_pixel().is_none());
    }

    #[test]
    fn mask_edge_pixels_keep_text_color_over_transparency() {
        // 最关键的一条：半覆盖的字形边缘反解回来还应当是字色，不能偏黑。
        // 偏黑就是"先跟黑底混再当成不透明"的经典黑边症状。
        let mut c = Canvas::new(1, 1);
        let color = Rgb::new(0xee, 0xf6, 0xff);
        c.blend_mask(
            (0, 0),
            Mask {
                w: 1,
                h: 1,
                cov: &[96],
            },
            color,
            255,
        );
        let px = c.pixel(0, 0);
        assert_eq!(px.a, 96, "覆盖率应当变成 alpha 而不是被丢掉");
        let back = px.unpremultiplied();
        let drift = (back.r as i32 - color.r as i32)
            .abs()
            .max((back.g as i32 - color.g as i32).abs())
            .max((back.b as i32 - color.b as i32).abs());
        assert!(drift <= 3, "边缘像素色偏 {drift}，说明混进了底色");
    }

    #[test]
    fn mask_out_of_range_coverage_buffer_does_not_panic() {
        let mut c = Canvas::new(4, 4);
        // 蒙版声明 4×4 但只给了 2 个字节：缺的部分跳过，不许越界。
        c.blend_mask(
            (0, 0),
            Mask {
                w: 4,
                h: 4,
                cov: &[255, 255],
            },
            Rgb::WHITE,
            255,
        );
        assert_eq!(c.pixel(0, 0).a, 255);
        assert_eq!(c.pixel(3, 3).a, 0);
    }

    #[test]
    fn mask_clips_at_canvas_edges() {
        let mut c = Canvas::new(2, 2);
        let cov = [255u8; 16];
        c.blend_mask(
            (-1, -1),
            Mask {
                w: 4,
                h: 4,
                cov: &cov,
            },
            Rgb::WHITE,
            255,
        );
        assert_eq!(c.pixel(0, 0).a, 255);
        assert_eq!(c.pixel(1, 1).a, 255);
    }

    #[test]
    fn resize_reallocates_and_clears() {
        let mut c = Canvas::new(4, 4);
        c.fill_rect(RectI::new(0, 0, 4, 4), Rgb::WHITE, 255);
        c.resize(8, 2);
        assert_eq!((c.width(), c.height()), (8, 2));
        assert!(c.bytes().iter().all(|b| *b == 0));
        c.fill_rect(RectI::new(0, 0, 8, 2), Rgb::WHITE, 255);
        c.resize(8, 2);
        assert!(
            c.bytes().iter().all(|b| *b == 0),
            "同尺寸 resize 也要清干净"
        );
    }

    #[test]
    fn sdf_sign_matches_inside_outside() {
        // 中心在内（负），远处在外（正），边界上约等于 0。
        assert!(round_rect_sdf(0.0, 0.0, 10.0, 10.0, 4.0) < 0.0);
        assert!(round_rect_sdf(20.0, 0.0, 10.0, 10.0, 4.0) > 0.0);
        assert!(round_rect_sdf(10.0, 0.0, 10.0, 10.0, 4.0).abs() < 0.001);
    }

    #[test]
    fn clip_keeps_drawing_inside_the_band_only() {
        let mut c = Canvas::new(8, 8);
        c.push_clip(2, 5);
        c.fill_rect(RectI::new(0, 0, 8, 8), Rgb::WHITE, 255);
        c.pop_clip();
        assert_eq!(c.pixel(0, 1).a, 0, "带子上方不画");
        assert_eq!(c.pixel(0, 3).a, 255, "带子里照画");
        assert_eq!(c.pixel(0, 5).a, 0, "带子下方不画");
    }

    #[test]
    fn nested_clips_intersect() {
        let mut c = Canvas::new(4, 10);
        c.push_clip(0, 10);
        c.push_clip(3, 6);
        c.fill_rect(RectI::new(0, 0, 4, 10), Rgb::WHITE, 255);
        c.pop_clip();
        c.pop_clip();
        assert_eq!(c.pixel(0, 2).a, 0);
        assert_eq!(c.pixel(0, 4).a, 255);
        assert_eq!(c.pixel(0, 7).a, 0);
    }

    #[test]
    fn clip_also_covers_masks_and_rounded_plates() {
        let mut c = Canvas::new(4, 8);
        c.push_clip(4, 8);
        c.fill_round_rect(RectI::new(0, 0, 4, 8), 2.0, Rgb::WHITE, 255);
        c.blend_mask(
            (0, 0),
            Mask {
                w: 4,
                h: 8,
                cov: &[255; 32],
            },
            Rgb::WHITE,
            255,
        );
        c.pop_clip();
        for y in 0..4 {
            assert_eq!(c.pixel(1, y).a, 0, "第 {y} 行在带子外，底衬和字都不该出现");
        }
        assert_eq!(c.pixel(1, 6).a, 255);
    }
}
