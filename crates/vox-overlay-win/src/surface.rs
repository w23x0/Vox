//! 分层窗的上屏通道：一块 32 位预乘 BGRA 的 DIB section + `UpdateLayeredWindow`。
//!
//! 分层窗不走 `WM_PAINT`：一次 `UpdateLayeredWindow` 同时把位置、大小和像素交给
//! 桌面合成器，所以尺寸变化不用先 `SetWindowPos` 再重绘，也就不会闪。
//!
//! 位图用**自上而下**（`biHeight` 取负）：GDI 默认自下而上，行序反了字就是上下颠倒的。

use vox_core::ports::{PortError, PortResult};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::geom::RectI;

/// 一块可以直接喂给 `UpdateLayeredWindow` 的位图。
pub struct LayeredSurface {
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl LayeredSurface {
    pub fn new() -> PortResult<Self> {
        // SAFETY: 传 None 表示以屏幕 DC 为模板，失败返回空句柄，紧接着就查。
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return Err(PortError::new(
                "failed to create the layered window memory DC",
            ));
        }
        Ok(Self {
            dc,
            bitmap: HBITMAP::default(),
            old_bitmap: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            width: 0,
            height: 0,
        })
    }

    /// 保证位图至少是 `width × height`。尺寸变了就重建。
    fn ensure(&mut self, width: i32, height: i32) -> PortResult<()> {
        if width <= 0 || height <= 0 {
            return Err(PortError::new("layered window size must be positive"));
        }
        if self.width == width && self.height == height && !self.bits.is_null() {
            return Ok(());
        }
        self.release_bitmap();

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // 负高度 = 自上而下，行序跟 Canvas 一致。
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: info 描述的尺寸就是下面记下来的尺寸；返回的像素指针由这块位图拥有，
        // 释放位图之前一直有效。
        let bitmap =
            unsafe { CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|e| {
                PortError::new(format!("failed to create the layered window bitmap: {e}"))
            })?;
        if bits.is_null() {
            // SAFETY: bitmap 刚建成功但没法用，这条路径上要还回去。
            unsafe {
                let _ = DeleteObject(bitmap.into());
            }
            return Err(PortError::new(
                "the layered window bitmap returned no pixel pointer",
            ));
        }
        // SAFETY: dc 有效，bitmap 刚建好且还没被选进任何 DC。
        let old_bitmap = unsafe { SelectObject(self.dc, bitmap.into()) };
        self.bitmap = bitmap;
        self.old_bitmap = old_bitmap;
        self.bits = bits.cast();
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// 把 `pixels`（预乘 BGRA，自上而下，长度必须是 `w*h*4`）贴到屏幕上。
    ///
    /// `rect` 是屏幕坐标下的窗口矩形。
    pub fn present(&mut self, hwnd: HWND, rect: RectI, pixels: &[u8]) -> PortResult<()> {
        self.ensure(rect.w, rect.h)?;
        let need = (rect.w as usize) * (rect.h as usize) * 4;
        if pixels.len() < need {
            return Err(PortError::new(format!(
                "pixel buffer too small: need {need} bytes, have {}",
                pixels.len()
            )));
        }
        // SAFETY: bits 指向 self.width*self.height*4 字节（刚由 ensure 保证等于 rect 尺寸），
        // 源切片长度已核对过，两块内存不重叠。
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits, need);
        }

        let pos = POINT {
            x: rect.x,
            y: rect.y,
        };
        let size = SIZE {
            cx: rect.w,
            cy: rect.h,
        };
        let src = POINT { x: 0, y: 0 };
        // AC_SRC_ALPHA 表示"源位图带 alpha 通道"，此时 GDI 要求颜色通道**已经预乘**过
        // alpha；没预乘的话半透明像素会偏亮，字周围就是一圈亮边。预乘统一在
        // color::Bgra::premultiplied 里做。
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: hwnd 是本线程创建的分层窗；所有指针都指向本函数栈上的结构，调用期间有效。
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
        .map_err(|e| PortError::new(format!("UpdateLayeredWindow failed: {e}")))
    }

    fn release_bitmap(&mut self) {
        // SAFETY: 先把 DC 里选中的位图换回原来的，再删自己建的——顺序反了会泄漏。
        unsafe {
            if !self.old_bitmap.is_invalid() {
                SelectObject(self.dc, self.old_bitmap);
                self.old_bitmap = HGDIOBJ::default();
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
                self.bitmap = HBITMAP::default();
            }
        }
        self.bits = std::ptr::null_mut();
        self.width = 0;
        self.height = 0;
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        self.release_bitmap();
        // SAFETY: dc 由本结构创建，此时已经没有选中的自建对象。
        unsafe {
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}
