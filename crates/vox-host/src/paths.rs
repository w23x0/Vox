//! 配置从哪进：目录三级回落 + 读 `settings.json` / `usage.json`。
//!
//! 桌面档的目录由 Tauri 给（`app.path().app_config_dir()`，由 identifier 决定），
//! 无屏设备没有 Tauri，而且**同一个盒子上"服务跑在哪个用户下"会决定目录**（systemd 服务
//! 常常不是桌面用户）——所以目录必须能被显式指定（`docs/platform/EMBEDDED.md` §3.5-①）：
//!
//! ```text
//! --config <settings.json 路径>      （装配层直接用它，并取它的父目录当配置目录）
//! $VOX_CONFIG_DIR
//! $XDG_CONFIG_HOME/vox
//! $HOME/.config/vox
//! ```
//!
//! 后三级就是 [`dir_from`]。都拿不到 → 报错退出，**不猜一个目录**：往错误的地方写
//! `control.json` / `secret.json` 比不写糟得多（CLI 会照着废凭据反复重试）。
//!
//! 读文件一律**读不出来就用默认值**（跟桌面侧同口径）：配置坏了不该让服务起不来——起不来连
//! 控制面都没有，用户就没法远程修它了。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use vox_core::usage::UsageLedger;
use vox_core::Settings;

/// 配置目录里的文件名。**两个外壳逐字相同**（搬自 `app/src-tauri/src/persist.rs` 与
/// `app/src-tauri/src/mcp.rs::STATE_FILE`、`crates/vox-headless/src/config.rs`）：
/// 同一个盒子上两个外壳指同一个目录时，看到的是同一份配置与同一份控制面握手文件。
pub const SETTINGS_FILE: &str = "settings.json";
pub const USAGE_FILE: &str = "usage.json";
pub const CONTROL_FILE: &str = "control.json";

/// 配置目录的环境变量（无屏回落的第一优先）。桌面不用这条（由 Tauri 给）。
pub const ENV_CONFIG_DIR: &str = "VOX_CONFIG_DIR";
/// 配置目录在 XDG / HOME 下的名字。跟桌面档的 identifier 同源（`com.vox.app`）。
pub const APP_DIR: &str = "vox";

/// 纯函数：三级回落的后三级（`--config` 由调用方优先，它只看父目录）。
///
/// 纯函数：参数就是"那三个环境变量各自的值"，读环境的事交给 [`dir_from_env`]。
/// 空值当没设（`XDG_CONFIG_HOME=` 这种写法不该把目录顶成相对路径）。
pub fn dir_from(
    vox: Option<&Path>,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let nonempty = |path: Option<&Path>| {
        path.filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
    };
    if let Some(dir) = nonempty(vox) {
        return Some(dir);
    }
    if let Some(xdg) = nonempty(xdg_config_home) {
        return Some(xdg.join(APP_DIR));
    }
    nonempty(home).map(|home| home.join(".config").join(APP_DIR))
}

/// 从当前进程的环境取配置目录。
pub fn dir_from_env() -> Option<PathBuf> {
    let var = |name: &str| env::var_os(name).map(PathBuf::from);
    dir_from(
        var(ENV_CONFIG_DIR).as_deref(),
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
    )
}

/// 这次进程要用的路径：配置目录 + settings.json。
///
/// 其余几件（usage / control）都是配置目录下的固定文件名，由方法推出来——
/// 所以**只有一处**能决定"配置在哪"。
///
/// **密钥文件不在这里**：桌面是 `secret.bin`、无屏是 `secret.json`（内容格式不一样，混在
/// 一起会互相读不懂），由 [`crate::secrets::SecretBackend`] 各自显式带自己的 `path`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub dir: PathBuf,
    pub settings: PathBuf,
}

impl Paths {
    /// 桌面档用：Tauri 给了目录，直接定下路径。**纯路径运算，不碰盘**。
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let settings = dir.join(SETTINGS_FILE);
        Self { dir, settings }
    }

    /// 无屏档 `--config <settings.json>`：给的是**文件**，取它的父目录当配置目录。
    ///
    /// **纯路径运算，不碰盘**——报告三模式（`--print-capabilities` / `--print-composition` /
    /// `--dry-run`）走的就是这条路，它们一个字节都不写，所以连配置目录都不该建出来
    /// （`--print-composition` 打一份 JSON 就在别人机器上留一个空目录，那是副作用）。
    /// 要写盘的模式自己先调 [`Paths::ensure_dir`]。
    pub fn from_settings_file(path: &Path) -> Result<Self, String> {
        if path.is_dir() {
            return Err(format!(
                "--config 要的是 settings.json 的路径，给的是目录：{}（目录请用 {ENV_CONFIG_DIR}）",
                path.display()
            ));
        }
        if path.file_name().is_none() {
            return Err(format!("--config 的路径没有文件名：{}", path.display()));
        }
        let settings = path.to_path_buf();
        Self::from_settings(settings)
    }

    /// 无屏档缺省：三级回落。**纯路径运算，不碰盘**，同 [`Paths::from_settings_file`]。
    pub fn from_env() -> Result<Self, String> {
        let dir = dir_from_env().ok_or_else(|| {
            format!(
                "拿不到配置目录：请给 --config <settings.json> 或设 {ENV_CONFIG_DIR}（\
                 XDG_CONFIG_HOME / HOME 都没有）"
            )
        })?;
        Self::from_settings(dir.join(SETTINGS_FILE))
    }

    /// 把配置目录建出来（首次启动、或者被清掉之后）。**只给会写盘的那条路调**：
    /// 目录是"要往里写东西"才需要的东西。
    ///
    /// 建不出来也不算致命：读会走默认值、写会在真正落盘时再报一次（`persist` 的原子写里
    /// 有 warn），所以这里只记一条警告，不把服务拦住。
    pub fn ensure_dir(&self) {
        if let Err(error) = fs::create_dir_all(&self.dir) {
            tracing::warn!(dir = %self.dir.display(), error = %error, "配置目录建不出来（后面写盘会失败）");
        }
    }

    pub fn settings(&self) -> &Path {
        &self.settings
    }

    pub fn usage(&self) -> PathBuf {
        self.dir.join(USAGE_FILE)
    }

    /// 控制面的握手文件（端口 / token 写在这里，CLI 与宿主照着它连）。
    /// 文件名与桌面档一致，见 `vox_mcp::transport::http::ServerOptions::state_file`。
    pub fn control(&self) -> PathBuf {
        self.dir.join(CONTROL_FILE)
    }

    /// 两条无屏入口（`from_settings_file` / `from_env`）共用的后半段：定下 settings 之后，
    /// 取它的父目录当配置目录。**父目录没有就报错**——那说明给的不是一条能落盘的路。
    fn from_settings(settings: PathBuf) -> Result<Self, String> {
        let dir = settings
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| format!("配置路径没有父目录：{}", settings.display()))?
            .to_path_buf();

        Ok(Self { dir, settings })
    }
}

/// 读设置。读不出来 / 坏了 → 默认值（不让一份坏配置把服务拦住）。
pub fn load_settings(path: &Path) -> Settings {
    match fs::read_to_string(path) {
        Ok(text) => Settings::from_json(&text),
        Err(error) => {
            if path.exists() {
                tracing::warn!(path = %path.display(), error = %error, "读设置失败，用默认值");
            }
            Settings::default()
        }
    }
}

/// 读用量账本。同口径：读不出来 → 空账本。
pub fn load_usage(path: &Path) -> UsageLedger {
    match fs::read_to_string(path) {
        Ok(text) => UsageLedger::from_json(&text),
        Err(error) => {
            if path.exists() {
                tracing::warn!(path = %path.display(), error = %error, "读用量失败，用空账本");
            }
            UsageLedger::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_env_dir_wins() {
        let dir = dir_from(
            Some(Path::new("/srv/vox")),
            Some(Path::new("/home/u/.config")),
            Some(Path::new("/home/u")),
        );
        assert_eq!(dir.as_deref(), Some(Path::new("/srv/vox")));
    }

    #[test]
    fn xdg_then_home_are_the_fallbacks() {
        assert_eq!(
            dir_from(None, Some(Path::new("/xdg")), Some(Path::new("/home/u"))).as_deref(),
            Some(Path::new("/xdg/vox"))
        );
        assert_eq!(
            dir_from(None, None, Some(Path::new("/home/u"))).as_deref(),
            Some(Path::new("/home/u/.config/vox"))
        );
        assert_eq!(dir_from(None, None, None), None, "一个都没有就不猜目录");
    }

    #[test]
    fn empty_env_values_are_unset() {
        // `VOX_CONFIG_DIR=` / `XDG_CONFIG_HOME=` 是"设了个空"，不是"设了这个目录"：
        // 空字符串会让后面的 join 变成一个相对路径，配置就落到进程的 cwd 里去了。
        assert_eq!(
            dir_from(
                Some(Path::new("")),
                Some(Path::new("")),
                Some(Path::new("/home/u"))
            )
            .as_deref(),
            Some(Path::new("/home/u/.config/vox"))
        );
    }

    #[test]
    fn config_file_decides_the_directory() {
        let dir = std::env::temp_dir().join(format!("vb-headless-paths-{}", std::process::id()));
        let paths = Paths::from_settings_file(&dir.join("settings.json")).expect("解析路径");
        assert_eq!(paths.dir, dir);
        assert_eq!(paths.settings, dir.join("settings.json"));
        assert_eq!(paths.control(), dir.join("control.json"));
        assert_eq!(paths.usage(), dir.join("usage.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解析路径**不碰盘**：报告三模式（`--print-composition` 等）靠这一条做到"一个空目录都不留"。
    /// 目录是"要往里写"才需要的东西，所以只有 [`Paths::ensure_dir`] 建它。
    #[test]
    fn resolving_paths_does_not_create_the_directory() {
        let dir = std::env::temp_dir().join(format!("vb-headless-pure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let paths = Paths::from_settings_file(&dir.join(SETTINGS_FILE)).expect("解析路径");
        assert!(!dir.exists(), "解析路径不该建目录：{}", dir.display());

        paths.ensure_dir();
        assert!(dir.is_dir(), "要写盘的模式调 ensure_dir 才建");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_is_rejected_with_a_hint() {
        let dir = std::env::temp_dir().join(format!("vb-headless-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let error = Paths::from_settings_file(&dir).unwrap_err();
        assert!(error.contains(ENV_CONFIG_DIR), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_settings_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("vb-headless-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, "{ 这不是 JSON").expect("写坏配置");
        // 契约：坏配置**等价于空配置**（走同一套 `migrate` + `normalize`），
        // 而不是"读到半截"或"起不来"。
        assert_eq!(load_settings(&path), Settings::from_json("{}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
