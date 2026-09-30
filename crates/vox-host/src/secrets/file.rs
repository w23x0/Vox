//! `SecretStore` 的文件实现：**配置目录下的 0600 文件** + 环境变量覆盖。
//!
//! 桌面档 Windows / Linux 两套都不用这一套：那边是 DPAPI、Secret Service（gnome-keyring /
//! KWallet，走 D-Bus 会话总线）。无屏盒子（Raspberry Pi OS Lite 这类纯命令行系统）
//! **大概率没有 Secret Service 守护进程**，而 systemd 服务又常常不是登录会话里的那个用户
//! ——照着桌面那套接，用户会拿到"密钥存不了"。所以走 `docs/platform/EMBEDDED.md` §3.6 给的
//! 那条兜底：**文件 + 权限 0600**（同一个 trait，接口一行没改）。
//!
//! **代价说清楚**：这不是加密，是"只有属主能读"。谁拿到那块盘 / 那个用户的 shell，谁就能
//! 拿到密钥。真要加密得有独立口令（systemd `LoadCredential=` / age），那是下一轮的事。
//! 所以：文件里**真的有**密钥时，装配层会发一条 `Notice` 把这件事说出来（[`SecretFile::stored_keys`]，
//! 判据在 [`crate::secrets::SecretBackend::has_plaintext_keys`]）。
//!
//! 环境变量覆盖（`VOX_API_KEY_<服务商>`，例 `VOX_API_KEY_ALIYUN`）是给
//! "一次性试跑"用的（EMBEDDED §3.5-①）：读的时候优先于文件，**写的时候不碰它**
//! ——环境变量改不了，`store` 写的一律是文件。两边都有时以环境变量为准，直到它被取消。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use vox_core::ports::{PortError, PortResult, SecretStore};
use vox_core::settings::ModelProvider;

/// 无屏档的密钥文件名。**不进 [`crate::paths::Paths`]**：桌面是 `secret.bin`（DPAPI 密文），
/// 无屏是这一个（明文 JSON），内容格式不一样，混在一起会互相读不懂。各后端自带自己的路径。
pub const SECRET_FILE: &str = "secret.json";

/// 环境变量前缀：`VOX_API_KEY_ALIYUN` / `VOX_API_KEY_GEMINI` / …
pub const ENV_API_KEY_PREFIX: &str = "VOX_API_KEY_";

/// 某个服务商的环境变量名（大写 `as_id()`）。**唯一**的取名处，测试也用这个。
pub fn env_var_for(provider: ModelProvider) -> String {
    format!(
        "{ENV_API_KEY_PREFIX}{}",
        provider.as_id().to_ascii_uppercase()
    )
}

pub struct SecretFile {
    path: PathBuf,
}

impl SecretFile {
    /// 密钥文件固定 `<config_dir>/secret.json`。
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join(SECRET_FILE),
        }
    }

    /// [`SecretBackend::File`](crate::secrets::SecretBackend::File) 带的是**完整文件名**
    /// （两个外壳各叫各的名），所以这一条不拼 [`SECRET_FILE`]。
    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 文件里到底存了没有（"明文"那条提示只在真有时才发）。
    ///
    /// 与 [`SecretStore::load_api_key_for`] 的差别：这里**不看环境变量**——环境变量给的密钥
    /// 没有落在盘上，不需要为它提示"明文"。
    pub fn stored_keys(&self) -> bool {
        read_file(&self.path)
            .map(|keys| !keys.is_empty())
            .unwrap_or(false)
    }

    fn load(&self, provider: ModelProvider) -> PortResult<Option<String>> {
        if let Ok(from_env) = std::env::var(env_var_for(provider)) {
            let from_env = from_env.trim().to_string();
            if !from_env.is_empty() {
                return Ok(Some(from_env));
            }
        }
        Ok(read_file(&self.path)?.remove(provider.as_id()))
    }

    fn store(&self, provider: ModelProvider, key: &str) -> PortResult<()> {
        let mut keys = read_file(&self.path)?;
        keys.insert(provider.as_id().to_string(), key.trim().to_string());
        write_file(&self.path, &keys)
    }

    fn clear(&self, provider: ModelProvider) -> PortResult<()> {
        let mut keys = read_file(&self.path)?;
        if keys.remove(provider.as_id()).is_none() {
            // 本来就没有 → 幂等成功，不为了"删一条不存在的记录"去写一次盘。
            return Ok(());
        }
        write_file(&self.path, &keys)
    }
}

impl SecretStore for SecretFile {
    // 不带服务商的那三个是"当前这一个"的旧口径：桌面 Linux 侧同样退到 `Aliyun`
    // （缺省服务商），保持一致——不然两个外壳对同一个调用的行为会不一样。
    fn load_api_key(&self) -> PortResult<Option<String>> {
        self.load(ModelProvider::Aliyun)
    }

    fn store_api_key(&self, key: &str) -> PortResult<()> {
        self.store(ModelProvider::Aliyun, key)
    }

    fn clear_api_key(&self) -> PortResult<()> {
        self.clear(ModelProvider::Aliyun)
    }

    fn load_api_key_for(&self, provider: ModelProvider) -> PortResult<Option<String>> {
        self.load(provider)
    }

    fn store_api_key_for(&self, provider: ModelProvider, key: &str) -> PortResult<()> {
        self.store(provider, key)
    }

    fn clear_api_key_for(&self, provider: ModelProvider) -> PortResult<()> {
        self.clear(provider)
    }
}

/// 读密钥文件。**文件不在 = 还没配过**（不是错误）；读得出来但不是我们要的形状 = 报错
/// （`Runtime::set_secret_store` 会把它翻成一条 `Notice`，不会静默当成"没配密钥"）。
fn read_file(path: &Path) -> PortResult<BTreeMap<String, String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(PortError::new(format!(
                "读密钥文件失败（{}）：{error}",
                path.display()
            )));
        }
    };
    let document: Value = serde_json::from_str(&text).map_err(|error| {
        PortError::new(format!(
            "密钥文件不是合法的 JSON（{}）：{error}",
            path.display()
        ))
    })?;
    let Some(object) = document.as_object() else {
        return Err(PortError::new(format!(
            "密钥文件应该是一个对象（服务商 → 密钥）：{}",
            path.display()
        )));
    };
    Ok(object
        .iter()
        .filter_map(|(provider, key)| Some((provider.clone(), key.as_str()?.to_string())))
        .collect())
}

/// 原子写 + 0600。**先写 `.tmp` 再 rename**：断电不会留半截 JSON（无屏盒子直接断电是常态）。
fn write_file(path: &Path, keys: &BTreeMap<String, String>) -> PortResult<()> {
    let json = serde_json::to_string_pretty(keys)
        .map_err(|error| PortError::new(format!("密钥序列化失败：{error}")))?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &json)
        .map_err(|error| PortError::new(format!("写密钥文件失败（{}）：{error}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(|error| {
        PortError::new(format!(
            "密钥文件改名失败（{} → {}）：{error}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

/// 只有属主能读写。Unix 上开文件时就带上模式（**不先建再 chmod**：中间那个瞬间是宽的）。
#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

/// 非 Unix（这个二进制产品上只跑 Linux / Windows，这里只为让 `cargo test --workspace`
/// 在别的宿主上也能编过）：没有统一的"0600"写法，交给文件系统默认的 ACL。
#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vb-headless-secret-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn missing_file_means_not_configured() {
        let dir = temp_dir("missing");
        let store = SecretFile::new(&dir);
        assert_eq!(store.load_api_key_for(ModelProvider::Aliyun).unwrap(), None);
        assert!(!store.stored_keys());
        // 删一条不存在的记录是幂等成功，也不该凭空建出文件来。
        store.clear_api_key_for(ModelProvider::Aliyun).unwrap();
        assert!(!store.path().exists(), "清空不该建文件");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_round_trip_per_provider() {
        let dir = temp_dir("round-trip");
        let store = SecretFile::new(&dir);
        store
            .store_api_key_for(ModelProvider::Aliyun, "sk-aliyun")
            .unwrap();
        store
            .store_api_key_for(ModelProvider::Gemini, "sk-gemini")
            .unwrap();

        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Aliyun)
                .unwrap()
                .as_deref(),
            Some("sk-aliyun")
        );
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gemini)
                .unwrap()
                .as_deref(),
            Some("sk-gemini")
        );
        assert!(store.stored_keys());

        store.clear_api_key_for(ModelProvider::Aliyun).unwrap();
        assert_eq!(store.load_api_key_for(ModelProvider::Aliyun).unwrap(), None);
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gemini)
                .unwrap()
                .as_deref(),
            Some("sk-gemini"),
            "清一个服务商不该动另一个"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 无屏盒子上这份密钥是**明文**存的，权限位是唯一的那道门——它掉了就等于把密钥
    /// 摆给同机器上的所有人看。
    #[cfg(unix)]
    #[test]
    fn the_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("mode");
        let store = SecretFile::new(&dir);
        store
            .store_api_key_for(ModelProvider::Aliyun, "sk-secret")
            .unwrap();

        let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "密钥文件权限应该是 0600，实际 {mode:o}");
        assert!(!dir.join("secret.json.tmp").exists(), "临时文件没清掉");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 环境变量优先于文件（一次性试跑用）——但**只影响读**，"盘上有明文"那条提示
    /// 不该因为环境变量而误报。
    #[test]
    fn env_var_wins_over_the_file_on_read() {
        let dir = temp_dir("env");
        let store = SecretFile::new(&dir);
        store
            .store_api_key_for(ModelProvider::Gemini, "from-file")
            .unwrap();

        let var = env_var_for(ModelProvider::Gemini);
        // SAFETY（这块的推理）：本用例是**唯一**读写这个变量名的用例，而且用的是
        // Gemini 这一格；同进程里并行跑的其他用例只碰 Aliyun。
        std::env::set_var(&var, "from-env");
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gemini)
                .unwrap()
                .as_deref(),
            Some("from-env")
        );
        // 空值当没设（`VOX_API_KEY_X=` 这种写法不该把文件里的密钥顶掉）。
        std::env::set_var(&var, "   ");
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gemini)
                .unwrap()
                .as_deref(),
            Some("from-file")
        );
        std::env::remove_var(&var);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_silent_empty() {
        let dir = temp_dir("corrupt");
        let store = SecretFile::new(&dir);
        fs::write(store.path(), "{ 这不是 JSON").unwrap();
        let error = store.load_api_key_for(ModelProvider::Aliyun).unwrap_err();
        assert!(error.to_string().contains("JSON"), "{error}");
        assert!(!store.stored_keys());
        let _ = fs::remove_dir_all(&dir);
    }
}
