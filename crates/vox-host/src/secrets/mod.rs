//! 密钥后端的**选择**这一层。
//!
//! 三个后端（DPAPI / Secret Service / 0600 文件）机制完全不同，但"按平台挑一个、挂进账本、
//! 顺手报一条'盘上真的有明文密钥'"这个**决策**两个入口各写了一遍。这里就是那一份决策：
//! 机制全在各自的实现里（[`dpapi`] / [`service`] / [`file`]），这一层**无状态**，
//! 也不 `impl SecretStore`。

#[cfg(windows)]
pub mod dpapi;
pub mod file;
#[cfg(all(target_os = "linux", feature = "secret-service"))]
pub mod service;

use std::path::PathBuf;
use std::sync::Arc;

use vox_core::ports::{PortError, PortResult, SecretStore};

use crate::secrets::file::SecretFile;

/// 密钥后端。**这是"选择"这一层**，机制全在各自的实现里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretBackend {
    /// Windows：DPAPI 加密落盘（`#[cfg(windows)]`；别的平台上选它会得到一个明确报错的
    /// `PortError`，不是静默降级）。
    Dpapi { path: PathBuf },
    /// 桌面 Linux：Secret Service（feature `secret-service`，默认**关**）。
    SecretService,
    /// 无 D-Bus / 无 keyring 的机器：配置目录下的 0600 明文文件。
    File { path: PathBuf },
}

impl SecretBackend {
    /// 造出 `Arc<dyn SecretStore>`。**不实现 `SecretStore`** —— 这一层没有状态。
    pub fn build(&self) -> Arc<dyn SecretStore> {
        match self {
            #[cfg(windows)]
            Self::Dpapi { path } => {
                Arc::new(crate::secrets::dpapi::DpapiSecretStore::new(path.clone()))
            }
            #[cfg(not(windows))]
            Self::Dpapi { .. } => Arc::new(UnavailableStore("DPAPI 只在 Windows 上可用")),
            #[cfg(all(target_os = "linux", feature = "secret-service"))]
            Self::SecretService => Arc::new(crate::secrets::service::SecretServiceStore::new()),
            #[cfg(not(all(target_os = "linux", feature = "secret-service")))]
            Self::SecretService => Arc::new(UnavailableStore(
                "Secret Service 需要 Linux + `secret-service` feature（keyring）",
            )),
            Self::File { path } => Arc::new(SecretFile::at_path(path.clone())),
        }
    }

    /// "盘上真的存了明文密钥"这条提示的判据。**只有 `File` 后端可能为真**
    /// （DPAPI 是密文、Secret Service 不落盘），所以其余两个恒 `false`。
    pub fn has_plaintext_keys(&self) -> bool {
        match self {
            Self::File { path } => SecretFile::at_path(path.clone()).stored_keys(),
            _ => false,
        }
    }
}

/// 这个平台上拿不到的那个后端的**确定**行为：每次调用都报错。
///
/// 不静默换成别的后端（`RULES.md` #4）——"这台机器上密钥存不了"必须让人知道，
/// 而不是悄悄退到明文文件。
struct UnavailableStore(&'static str);

impl SecretStore for UnavailableStore {
    fn load_api_key(&self) -> PortResult<Option<String>> {
        Err(PortError::new(self.0))
    }

    fn store_api_key(&self, _key: &str) -> PortResult<()> {
        Err(PortError::new(self.0))
    }

    fn clear_api_key(&self) -> PortResult<()> {
        Err(PortError::new(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::settings::ModelProvider;

    #[test]
    fn a_file_backend_says_yes_when_the_key_is_on_disk() {
        let dir = std::env::temp_dir().join(format!("vox-host-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let backend = SecretBackend::File {
            path: dir.join(file::SECRET_FILE),
        };

        assert!(
            !backend.has_plaintext_keys(),
            "盘上还没有密钥，就不该说'存了明文'——那会让用户白担心一次"
        );

        backend
            .build()
            .store_api_key_for(ModelProvider::Aliyun, "sk-plain")
            .expect("写密钥");
        assert!(
            backend.has_plaintext_keys(),
            "写进去一份就该说有：明文兜底唯一的代价说明，漏了就等于没提示"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 两个外壳的密钥**文件名不同**（桌面 `secret.bin`、无屏 `secret.json`）：内容格式不一样，
    /// 混在同一个位置会互相读不懂。这条把"各带各的路径，一个字节都不改"钉住——
    /// 谁哪天想"顺手统一一下"，这条会红。
    #[cfg(windows)]
    #[test]
    fn file_and_dpapi_backends_keep_their_own_filenames() {
        use std::collections::HashMap;

        let dir = std::env::temp_dir().join(format!("vox-host-dpapi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");

        let mut written: HashMap<String, String> = HashMap::new();
        for (name, backend) in [
            (
                "secret.bin",
                SecretBackend::Dpapi {
                    path: dir.join("secret.bin"),
                },
            ),
            (
                "secret.json",
                SecretBackend::File {
                    path: dir.join("secret.json"),
                },
            ),
        ] {
            backend
                .build()
                .store_api_key_for(ModelProvider::Aliyun, "sk-two-names")
                .expect("写密钥");
            written.insert(name.to_string(), backend.has_plaintext_keys().to_string());
        }

        // 桌面那份落的是 DPAPI 密文（所以"明文"那条提示对它恒假）；无屏那份是 0600 明文。
        assert_eq!(written.get("secret.bin").map(String::as_str), Some("false"));
        assert_eq!(written.get("secret.json").map(String::as_str), Some("true"));
        // 两个文件都在，各叫各的名。
        assert!(dir.join("secret.bin").is_file());
        assert!(dir.join("secret.json").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
