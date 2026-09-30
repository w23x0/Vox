//! 设置和用量落盘。
//!
//! 两个外壳同一条设计，因为要防的是同样的两件事：
//!
//! - **去抖**：用量账本会跟着每条流水线的每一轮更新，几分钟一次、还可能两条腿同时来。
//!   在事件回调里同步落盘 = 在**会话线程**上做磁盘 IO（SD 卡上几十毫秒），音频就那么被卡住了。
//!   所以回调只把 JSON 攒进内存，后台线程 ~800 ms 醒一次统一写。
//! - **原子写**：先写 `.tmp` 再 `rename`，断电/崩溃不会留半个 JSON——无屏盒子直接断电
//!   是常态，桌面档也经不起一份被截断的用量账本。
//!
//! 读写口径也照旧：读不出来就用默认值（[`crate::paths::load_settings`]），绝不因为配置坏了
//! 就起不来。密钥**不在这两个文件里**，只给出路径由 [`crate::secrets`] 加密写。
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
use vox_core::event::Event;
use vox_core::runtime::{Listener, Runtime};
use vox_core::settings::Settings;
use vox_core::usage::UsageLedger;

use crate::paths::{SETTINGS_FILE, USAGE_FILE};

/// 后台去抖线程的唤醒间隔。不用太短——800ms 的延迟对用户无感，
/// 但能把几十次账本更新合并成一次磁盘 IO。
const FLUSH_INTERVAL: Duration = Duration::from_millis(800);
/// 分段睡的粒度：收摊时最多等这么久就醒，不用等满一个 flush 周期。
const SLEEP_SLICE: Duration = Duration::from_millis(250);

/// 待写入的脏数据。`None` 表示该项干净，不需要落盘。
struct Dirty {
    settings: Option<String>,
    usage: Option<String>,
}

pub struct Persist {
    dir: PathBuf,
    dirty: Mutex<Dirty>,
    /// 通知后台线程退出。
    stop: AtomicBool,
    /// 去抖线程句柄，[`Persist::flush`] 里 join。`None` = 没起线程（测试路径）。
    flusher: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Persist {
    /// 纯构造，**不起**后台线程、**不碰盘**。生产代码请用 [`Persist::start`]。
    ///
    /// 单独留一个不起线程的构造函数是给只读出口与测试用的：它们都显式调 `flush()`，
    /// 不需要去抖线程，也不希望测试进程里堆一串线程。
    ///
    /// **不建目录**是有意的：只读出口（`--print-composition`）也用它读设置，而"打一份
    /// JSON"不该在别人机器上留下一个空目录。要写盘的走 [`Persist::start`]（那里建目录），
    /// 真落盘时 [`atomic_write`] 还会再建一次兜底。
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            dirty: Mutex::new(Dirty {
                settings: None,
                usage: None,
            }),
            stop: AtomicBool::new(false),
            flusher: Mutex::new(None),
        }
    }

    /// 构造并起去抖线程，返回 `Arc`。
    ///
    /// **必须先进 `Arc` 再起线程**：线程要长期持有 `Persist`，只有让它持有一份
    /// `Arc` 才能保证对象活着。早先的写法是在 `new()` 的栈局部上起线程、把
    /// `&self` 转成 `usize` 带进去，然后把结构体按值 move 出去——线程手里那个
    /// 地址指向的是 `new()` 已经退栈的栈帧，`assemble()` 后面的局部变量立刻就
    /// 会复用那块内存。表现是去抖线程读到垃圾 `stop` 标志（静默罢工，改动只在
    /// 退出时那一次 `flush` 落盘）、或在垃圾 Mutex 上永久 park、或把任意堆字节
    /// 当 JSON 写进 settings.json。
    ///
    /// 目录可能还不存在（首次启动），要写盘的这条路先建好——[`Persist::new`] 只读、不建。
    /// 失败了也不 panic：读会走默认值，写会在 [`atomic_write`] 里再报一次。
    pub fn start(dir: PathBuf) -> Arc<Self> {
        let _ = fs::create_dir_all(&dir);
        let me = Arc::new(Self::new(dir));
        me.spawn_flusher();
        me
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    pub fn usage_path(&self) -> PathBuf {
        self.dir.join(USAGE_FILE)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn load_settings(&self) -> Settings {
        crate::paths::load_settings(&self.settings_path())
    }

    pub fn load_usage(&self) -> UsageLedger {
        crate::paths::load_usage(&self.usage_path())
    }

    /// 标记设置为脏。实际写入由后台线程在下一个 flush 周期执行。
    pub fn save_settings(&self, settings: &Settings) {
        let json = settings.to_json();
        self.dirty.lock().settings = Some(json);
    }

    /// 标记用量账本为脏。高频调用安全——只是写内存。
    pub fn save_usage(&self, usage: &UsageLedger) {
        let json = usage.to_json();
        self.dirty.lock().usage = Some(json);
    }

    /// 落盘监听：设置/用量一变就标脏（真写盘在去抖线程里）。
    ///
    /// 无屏入口走这条。桌面**不走**：它的 `events::wire` 一个监听器里既转发给前端又落盘，
    /// 拆成两个监听器会多发一遍 `SettingsChanged` 的处理。
    ///
    /// **必须排在 `Runtime::load_usage` 之后**：否则刚读出来的那份会被当成"变了"再写一遍。
    pub fn attach_to(self: &Arc<Self>, runtime: &Runtime) {
        let sink = Arc::clone(self);
        let listener: Listener = Arc::new(move |event: &Event| match event {
            Event::SettingsChanged { settings } => sink.save_settings(settings),
            Event::UsageChanged { usage } => sink.save_usage(usage),
            _ => {}
        });
        runtime.add_listener(listener);
    }

    /// 把欠着的写入立刻落盘。退出前调，保证不丢数据。
    ///
    /// 先竖停止旗并 join 去抖线程（最多等 [`SLEEP_SLICE`]），再同步写剩下的脏数据。
    /// 顺序很关键：如果不 join 就写，去抖线程可能在我们写完之后又醒一次，
    /// 把 `flush()` 之后新标脏的东西写进去——那反而是好事，但更常见的是它跟
    /// 我们同时 [`atomic_write`] 同一个路径，两个 `.tmp` 互相 rename。
    pub fn flush(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.flusher.lock().take() {
            let _ = h.join();
        }
        self.do_flush();
    }

    /// 取出所有脏数据并写入磁盘。持有锁的时间极短（只做 take），
    /// 真正的 IO 在锁外面。
    fn do_flush(&self) {
        let (settings, usage) = {
            let mut dirty = self.dirty.lock();
            (dirty.settings.take(), dirty.usage.take())
        };
        if let Some(json) = settings {
            atomic_write(&self.settings_path(), &json);
        }
        if let Some(json) = usage {
            atomic_write(&self.usage_path(), &json);
        }
    }

    /// 起一个后台线程做去抖写入。线程名带标识，方便在调试器/日志里认。
    ///
    /// 线程持有一份 `Arc<Persist>`，所以对象一定活得比线程长——不需要 unsafe。
    /// 句柄存起来给 [`Persist::flush`] join，保证它返回之后不会再有人写这两个文件。
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
            .ok(); // 线程起不来也不致命——数据只是攒到退出时一次性写。
        *self.flusher.lock() = handle;
    }
}

/// 原子写：先写临时文件再 `rename`。断电/崩溃不会留半截 JSON。
///
/// 写之前顺手把目录建出来（`create_dir_all` 幂等）：目录是"要往里写"才需要的东西，
/// 这样 [`Persist::new`] 那条只读路一个目录都不建。
fn atomic_write(path: &Path, content: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
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

// --- 测试 ---------------------------------------------------------------------
//
// 搬自两份装配层（`app/src-tauri/src/persist.rs` 与 `crates/vox-headless/src/persist.rs`），
// **逐字未改**。两份各带一个 `temp_dir` 助手且签名不同（`()` vs `&str`），Rust 不许重载，
// 所以无屏那三条整体放进 `mod headless_tests`——测试函数体一个字节都没动。
// 两份的用例名互不重名，所以合成一份时**没有一条被丢掉或改名**。

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::SystemTime;

    /// 建一个带唯一后缀的临时目录，测试结束时自动清理。
    fn temp_dir() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("vox_persist_test_{id}"));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn round_trip_settings() {
        let dir = temp_dir();
        let persist = Persist::new(dir.clone());

        let mut settings = Settings::default();
        settings.speak.enabled = true;
        settings.speak.target_language = "ko".to_string();
        settings.listen.source_language = Some("ja".to_string());
        settings.normalize();

        persist.save_settings(&settings);
        persist.flush();

        let loaded = persist.load_settings();
        assert!(loaded.speak.enabled);
        assert_eq!(loaded.speak.target_language, "ko");
        assert_eq!(loaded.listen.source_language.as_deref(), Some("ja"));

        cleanup(&dir);
    }

    #[test]
    fn round_trip_usage() {
        let dir = temp_dir();
        let persist = Persist::new(dir.clone());

        let mut usage = UsageLedger::default();
        usage.record(
            "test-model",
            &vox_core::usage::TurnUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
            vox_core::usage::Stamp {
                unix_secs: 1_700_000_000,
                year: 2026,
                month: 8,
                day: 5,
            },
        );

        persist.save_usage(&usage);
        persist.flush();

        let loaded = persist.load_usage();
        assert_eq!(loaded, usage);

        cleanup(&dir);
    }

    #[test]
    fn load_settings_returns_default_when_dir_missing() {
        // 给一个肯定不存在的路径：`Persist::new` 只读——不建目录、也不起线程。
        let dir = std::env::temp_dir().join("vox_persist_test_nonexistent_42");
        let _ = fs::remove_dir_all(&dir);

        let persist = Persist::new(dir.clone());

        let settings = persist.load_settings();
        assert_eq!(settings, Settings::default());
        assert!(!dir.exists(), "只读构造不该把目录建出来");

        // 确保没有残留。
        let _ = fs::remove_dir_all(&dir);
    }

    /// 第 7 条：**只读构造不碰盘**。`--print-composition` 用它读设置——打一份 JSON 不该在
    /// 别人机器上留下一个空目录；要写盘的 [`Persist::start`] 才建目录（首次启动时目录还没有）。
    #[test]
    fn new_does_not_create_the_directory_but_start_does() {
        let dir = temp_dir().join("not-yet");
        let _ = fs::remove_dir_all(&dir);

        let read_only = Persist::new(dir.clone());
        assert_eq!(
            read_only.load_settings(),
            Settings::default(),
            "读不出来就走默认值"
        );
        assert!(!dir.exists(), "只读构造不该建目录：{}", dir.display());

        let writing = Persist::start(dir.clone());
        assert!(
            dir.is_dir(),
            "要写盘的 start 该把目录建出来：{}",
            dir.display()
        );
        writing.flush();

        cleanup(&dir);
        cleanup(&dir.parent().expect("有父目录").to_path_buf());
    }

    #[test]
    fn garbage_json_returns_default() {
        let dir = temp_dir();
        fs::write(dir.join("settings.json"), "这不是 JSON!!!").unwrap();
        fs::write(dir.join("usage.json"), "{{{坏的").unwrap();

        let persist = Persist {
            dir: dir.clone(),
            dirty: Mutex::new(Dirty {
                settings: None,
                usage: None,
            }),
            stop: AtomicBool::new(true),
            flusher: Mutex::new(None),
        };

        let settings = persist.load_settings();
        assert_eq!(
            settings.speak.model_name,
            Settings::default().speak.model_name
        );

        let usage = persist.load_usage();
        assert_eq!(usage, UsageLedger::default());

        cleanup(&dir);
    }

    /// 去抖线程必须能真的自己落盘——不靠 `flush()`。
    ///
    /// 这条守的是"线程拿到的 `Persist` 是活对象"。早先 `spawn_flusher` 在
    /// `new()` 的栈局部上取地址、随后把结构体 move 出去，线程手里是悬垂指针；
    /// 那种写法下这条测试会挂死、读到垃圾 `stop` 直接罢工、或者写出乱码文件。
    #[test]
    fn background_thread_flushes_without_explicit_flush() {
        let dir = temp_dir();
        let persist = Persist::start(dir.clone());

        // 故意在这里放一串局部变量，模拟 assemble() 里 new() 之后紧跟着的那些
        // 局部——如果线程持的是悬垂栈指针，这些就是覆盖它的凶手。
        let _decoys = (
            [0xAAu8; 512],
            PathBuf::from("/nonexistent/decoy/path/that/is/long"),
            vec![0xDEADBEEFu64; 64],
        );

        let mut settings = Settings::default();
        settings.speak.target_language = "ko".to_string();
        settings.speak.enabled = true;
        persist.save_settings(&settings);

        // 去抖间隔 800ms，分段睡 250ms —— 给足两个周期。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let path = dir.join("settings.json");
        while !path.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }

        assert!(
            path.exists(),
            "去抖线程应该自己把 settings.json 写出来，不该等 flush()"
        );
        let loaded = persist.load_settings();
        assert_eq!(loaded.speak.target_language, "ko");
        assert!(
            loaded.speak.enabled,
            "去抖线程写出来的内容应该是我们存的那份"
        );

        persist.flush();
        cleanup(&dir);
    }

    /// `flush()` 之后去抖线程必须已经退出（join 过）。
    #[test]
    fn flush_joins_the_background_thread() {
        let dir = temp_dir();
        let persist = Persist::start(dir.clone());
        assert!(persist.flusher.lock().is_some(), "start() 应该起了去抖线程");

        persist.flush();
        assert!(
            persist.flusher.lock().is_none(),
            "flush() 应该 join 并取走线程句柄"
        );

        cleanup(&dir);
    }

    #[test]
    fn flush_writes_files_to_disk() {
        let dir = temp_dir();
        let persist = Arc::new(Persist::new(dir.clone()));

        persist.save_settings(&Settings::default());
        persist.save_usage(&UsageLedger::default());

        // flush 之前文件可能还没写（靠后台线程的话有延迟），但 flush 之后一定在。
        persist.flush();

        assert!(
            dir.join("settings.json").exists(),
            "flush 后 settings.json 应该存在"
        );
        assert!(
            dir.join("usage.json").exists(),
            "flush 后 usage.json 应该存在"
        );

        // 临时文件不该残留。
        assert!(!dir.join("settings.json.tmp").exists());
        assert!(!dir.join("usage.json.tmp").exists());

        cleanup(&dir);
    }
}

/// 无屏档那三条（搬自 `crates/vox-headless/src/persist.rs`，逐字未改）。单独一个模块只因为
/// 两份的 `temp_dir` 助手签名不同（`()` vs `&str`），Rust 不许重载。
#[cfg(test)]
mod headless_tests {
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
        let loaded = crate::paths::load_settings(&persist.settings_path());
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
            crate::paths::load_usage(&persist.dir.join(USAGE_FILE)),
            expected
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
