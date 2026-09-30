//! 无屏档配置目录这一侧的"本地口味"：**密钥文件名**。
//!
//! 目录三级回落（`--config` → `$VOX_CONFIG_DIR` → `$XDG_CONFIG_HOME/vox` →
//! `$HOME/.config/vox`）、`Paths` 的路径运算、读 `settings.json` / `usage.json` 全都在
//! `vox_host::paths`（S4-A W3，与桌面档共用一份）。**它们在这里都不留第二份。**
//!
//! 剩下的是共享层**故意不管**的那一件：密钥文件名。
//!
//! - 桌面档（Windows）：`secret.bin`，DPAPI 密文，见 `app/src-tauri/src/lib.rs`；
//! - 无屏档（本文件）：`secret.json`，0600 明文。
//!
//! 两个外壳指同一个目录时也各读各的——内容格式不一样，同一个文件名会互相读不懂。
//! 所以 [`secret_path`] 把这条差异写死成一句代码，而不是散在两处字面量里。

use std::path::PathBuf;

use vox_host::Paths;

/// 无屏档的密钥文件：`<配置目录>/secret.json`。文件名本身在 `vox_host::secrets::file`
/// （**只有那一份定义**，S4-A §9 M5.4），这里只负责"配到目录上去"。
pub fn secret_path(paths: &Paths) -> PathBuf {
    paths.dir.join(vox_host::secrets::file::SECRET_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无屏档的密钥文件**留在这一档**：`<配置目录>/secret.json`。
    ///
    /// 共享层的 `Paths` 不再管密钥文件名（两个外壳各叫各的），所以这条断言的归属地就是
    /// 本文件——它是"无屏档的密钥是 `secret.json`"这句话唯一被钉住的地方。
    /// 谁哪天把它挪进 `Paths` 或者"顺手统一"成 `secret.bin`，这条会红。
    #[test]
    fn config_file_decides_the_directory() {
        let dir = std::env::temp_dir().join(format!("vb-headless-paths-{}", std::process::id()));
        let paths = Paths::from_settings_file(&dir.join("settings.json")).expect("解析路径");
        assert_eq!(paths.dir, dir);
        assert_eq!(paths.settings, dir.join("settings.json"));
        assert_eq!(paths.control(), dir.join("control.json"));
        assert_eq!(secret_path(&paths), dir.join("secret.json"));
        assert_eq!(paths.usage(), dir.join("usage.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
