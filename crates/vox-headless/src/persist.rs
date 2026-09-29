//! 设置与用量落盘。
//!
//! 跟桌面档（`app/src-tauri/src/persist.rs`）**同一条设计**，因为要防的是同样的两件事：
//!
//! - **去抖**：用量账本会跟着每条流水线的每一轮更新，几分钟一次、还可能两条腿同时来。
//!   在事件回调里同步落盘 = 在**会话线程**上做磁盘 IO（SD 卡上几十毫秒），音频就那么被卡住了。
//!   所以回调只把 JSON 攒进内存，后台线程 ~800 ms 醒一次统一写。
//! - **原子写**：先写 `.tmp` 再 `rename`，断电/崩溃不会留半个 JSON——无屏盒子直接断电
//!   是常态，这一条在这是刚需。
//!
//! 读写口径也照旧：读不出来就用默认值（[`crate::config::load_settings`]），绝不因为
//! 配置坏了就起不来。
//!
//! **已知代价**：被 SIGKILL / 断电打断时，去抖窗口内（≤800 ms）的改动会丢。桌面档有
//! Tauri 的退出事件兜底，无屏档走正常退出路径（`--run-for` 到的、或者收摊返回）会
//! [`Persist::flush`]，但**没有信号处理**——systemd 的 SIGTERM 会直接终止进程。
//! 代价是可接受的：丢的是"最后一轮用量"或"最后一次改设置"，不是配置本身（`settings.json`
//! 上一次写的还在）。要更硬就得接信号处理 + 一次 flush，那要额外依赖，留给下一轮。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use vox_core::usage::UsageLedger;
use vox_core::Settings;

use crate::config::{SETTINGS_FILE, USAGE_FILE};

/// 后台去抖线程的唤醒间隔。800 ms 对用户无感，但能把一串更新合并成一次磁盘 IO。
/// 与桌面档同值——同一条理由。
const FLUSH_INTERVAL: Duration = Duration::from_millis(800);
/// 分段睡的粒度：收摊时最多等这么久就醒，不用等满一个 flush 周期。
const SLEEP_SLICE: Duration = Duration::from_millis(250);

/// 待写入的脏数据。`None` = 该项干净。
struct Dirty {
    settings: Option<String>,
    usage: Option<String>,
}

pub struct Persist {
    dir: PathBuf,
    dirty: Mutex<Dirty>,
    stop: AtomicBool,
    /// 去抖线程句柄，[`Persist::flush`] 里 join。`None` = 线程没起来（也没关系：
    /// 数据只是攒到 [`Persist::flush`] 一次性写）。
    flusher: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Persist {
    /// 起去抖线程。**必须先进 `Arc` 再起线程**：线程要长期持有 `Persist`，
    /// 只有 `Arc` 才能保证对象活得比线程长（桌面档 `persist.rs` 的头注释记着这条血泪）。
    pub fn start(dir: PathBuf) -> Arc<Self> {
        let me = Arc::new(Self {
            dir,
            dirty: Mutex::new(Dirty {
                settings: None,
                usage: None,
            }),
            stop: AtomicBool::new(false),
            flusher: Mutex::new(None),
        });
        me.spawn_flusher();
        me
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    /// 标脏（回调线程只做这一步：克隆一份 JSON 进内存，不碰磁盘）。
    pub fn save_settings(&self, settings: &Settings) {
        self.dirty.lock().settings = Some(settings.to_json());
    }

    pub fn save_usage(&self, usage: &UsageLedger) {
        self.dirty.lock().usage = Some(usage.to_json());
    }

    /// 把欠着的写入立刻落盘。退出前调，保证不丢数据。
    ///
    /// 先竖停止旗并 join 去抖线程，再同步写剩下的脏数据——反过来的话，去抖线程可能在我们
    /// 写完之后又醒一次，两个 `.tmp` 互相 rename。
    pub fn flush(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.flusher.lock().take() {
            let _ = handle.join();
        }
        self.do_flush();
    }

    fn do_flush(&self) {
        // 持锁时间极短（只做 take），真正的 IO 在锁外面。
        let (settings, usage) = {
            let mut dirty = self.dirty.lock();
            (dirty.settings.take(), dirty.usage.take())
        };
        if let Some(json) = settings {
            atomic_write(&self.settings_path(), &json);
        }
        if let Some(json) = usage {
            atomic_write(&self.dir.join(USAGE_FILE), &json);
        }
    }

    fn spawn_flusher(self: &Arc<Self>) {
        let me = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("vox-persist".into())
            .spawn(move || {
                loop {
                    // 分段睡：退出时最多等一个 `SLEEP_SLICE`。
                    let mut slept = Duration::ZERO;
                    while slept < FLUSH_INTERVAL {
                        if me.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(SLEEP_SLICE);
                        slept += SLEEP_SLICE;
                    }
                    me.do_flush();
                }
            })
            .ok(); // 线程起不来不致命：数据攒到 `flush()` 一次性写。
        *self.flusher.lock() = handle;
    }
}

/// 原子写：先写临时文件再 `rename`。断电/崩溃不会留半截 JSON。
fn atomic_write(path: &Path, content: &str) {
    let tmp = path.with_extension("json.tmp");
    if let Err(error) = fs::write(&tmp, content) {
        tracing::warn!(path = %tmp.display(), error = %error, "写临时文件失败（这次改动没落盘）");
        return;
    }
    if let Err(error) = fs::rename(&tmp, path) {
        tracing::warn!(path = %path.display(), error = %error, "rename 失败（这次改动没落盘）");
        // tmp 留着也没事，下次会覆盖。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::usage::{Stamp, TurnUsage};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vb-headless-persist-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn flush_writes_settings_and_usage_atomically() {
        let dir = temp_dir("flush");
        let persist = Persist::start(dir.clone());

        let mut settings = Settings::default();
        settings.speak.target_language = "ja".to_string();
        persist.save_settings(&settings);
        persist.save_usage(&UsageLedger::default());
        persist.flush();

        // 读回来的是刚写进去的那一份（不是默认值）。
        let loaded = crate::config::load_settings(&persist.settings_path());
        assert_eq!(loaded.speak.target_language, "ja");
        // 临时文件不留（rename 吃掉它了）。
        assert!(!dir.join("settings.json.tmp").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_is_idempotent_and_leaves_nothing_dirty() {
        let dir = temp_dir("idempotent");
        let persist = Persist::start(dir.clone());
        persist.flush();
        persist.flush();
        assert!(!dir.join(USAGE_FILE).exists(), "没脏数据就不该写盘");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_round_trips_through_the_file() {
        let dir = temp_dir("usage");
        let persist = Persist::start(dir.clone());
        let mut ledger = UsageLedger::default();
        ledger.record(
            "qwen3.5-livetranslate-flash-realtime",
            &TurnUsage {
                input_tokens: 120,
                output_tokens: 30,
                total_tokens: 150,
            },
            Stamp {
                unix_secs: 1_777_000_000,
                year: 2026,
                month: 4,
                day: 17,
            },
        );
        let expected = ledger.clone();
        persist.save_usage(&ledger);
        persist.flush();

        assert_eq!(
            crate::config::load_usage(&persist.dir.join(USAGE_FILE)),
            expected
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
