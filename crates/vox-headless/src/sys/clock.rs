//! `Clock` 的无屏实现。
//!
//! 跟桌面 Linux 侧（`app/src-tauri/src/platform/linux/clock.rs`）同一套口径，两件事分开取：
//! - `now_ms` 用单调时钟（`Instant`），改系统时间不会让字幕的 TTL 算乱；
//! - `stamp` 要的是**本地日期**（用量按天/按月分桶），走挂钟时间 + 本地时区。
//!
//! 无屏盒子上没有界面可以改时间，但**NTP 会把时间往回拨**（一块刚开机的板子尤其常见），
//! 所以单调/挂钟这条分界在这儿更要紧。

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local};
use vox_core::ports::Clock;
use vox_core::usage::Stamp;

pub struct LocalClock {
    origin: Instant,
}

impl LocalClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for LocalClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn stamp(&self) -> Stamp {
        // chrono 的 `Local` 走 libc 的 `localtime_r`，任何线程都能安全调；
        // 拿不到本地时区时它自己退到 UTC，不会失败（无屏盒子上常常没设时区）。
        let now = Local::now();
        Stamp {
            unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            year: now.year(),
            month: now.month(),
            day: now.day(),
        }
    }
}

/// 装配层用的那一个：账本要的是 `Arc<dyn Clock>`。
pub fn local() -> Arc<dyn Clock> {
    Arc::new(LocalClock::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_monotonic() {
        let clock = LocalClock::new();
        let a = clock.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = clock.now_ms();
        assert!(b >= a, "单调时钟不该往回走：{a} -> {b}");
    }

    #[test]
    fn stamp_has_a_plausible_local_date() {
        let s = LocalClock::new().stamp();
        assert!(s.year >= 2024, "年份不对：{}", s.year);
        assert!((1..=12).contains(&s.month), "月份不对：{}", s.month);
        assert!((1..=31).contains(&s.day), "日不对：{}", s.day);
        // 2024-01-01 的 unix 秒。时钟没坏的话一定在这之后。
        assert!(s.unix_secs > 1_704_067_200, "unix 秒不对：{}", s.unix_secs);
    }
}
