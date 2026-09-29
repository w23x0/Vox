//! `SecretStore` 的 Linux 实现：Secret Service（gnome-keyring / KWallet / KeePassXC…）。
//!
//! 跟 Windows 的 DPAPI 落盘是同一个目标——密钥**绝不进 `settings.json`**，
//! 只有当前登录用户能取回来——只是换成了会话总线上的标准接口。
//!
//! `keyring` 4.x 默认带 `zbus-secret-service-keyring-store`（纯 Rust，不链 libsecret），
//! 所以这条路上没有额外的系统依赖。
//!
//! 没有 Secret Service 的机器（极简 WM、容器、SSH 会话）会拿到明确的错误字符串，
//! 由装配层翻成界面上的 `Notice`——**不做明文兜底**，宁可让用户知道存不了密钥。

use keyring::Entry;
use vox_core::ports::{PortError, PortResult, SecretStore};
use vox_core::settings::ModelProvider;

/// 服务名用应用的 bundle id，跟 `tauri.conf.json` 里的 identifier 一致，
/// 用户在 Seahorse/KWallet 里看到的就是这个。
const SERVICE: &str = "com.vox.app";

pub struct SecretServiceStore;

impl SecretServiceStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(provider: ModelProvider) -> PortResult<Entry> {
        Entry::new(SERVICE, &user_for(provider))
            .map_err(|e| PortError::new(format!("连接密钥服务失败：{e}")))
    }
}

impl Default for SecretServiceStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 每个服务商一条条目，跟 Windows 侧"每个服务商一个文件"对应。
fn user_for(provider: ModelProvider) -> String {
    format!("api-key.{}", provider.as_id())
}

fn load(provider: ModelProvider) -> PortResult<Option<String>> {
    let entry = SecretServiceStore::entry(provider)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        // 没有这条条目 → 还没配过，不是错误。
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(PortError::new(format!("读取密钥失败：{e}"))),
    }
}

fn store(provider: ModelProvider, key: &str) -> PortResult<()> {
    let entry = SecretServiceStore::entry(provider)?;
    entry
        .set_password(key)
        .map_err(|e| PortError::new(format!("写入密钥失败：{e}")))
}

fn clear(provider: ModelProvider) -> PortResult<()> {
    let entry = SecretServiceStore::entry(provider)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        // 本来就空 → 幂等成功。
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(PortError::new(format!("删除密钥失败：{e}"))),
    }
}

impl SecretStore for SecretServiceStore {
    fn load_api_key(&self) -> PortResult<Option<String>> {
        load(ModelProvider::Aliyun)
    }

    fn store_api_key(&self, key: &str) -> PortResult<()> {
        store(ModelProvider::Aliyun, key)
    }

    fn clear_api_key(&self) -> PortResult<()> {
        clear(ModelProvider::Aliyun)
    }

    fn load_api_key_for(&self, provider: ModelProvider) -> PortResult<Option<String>> {
        load(provider)
    }

    fn store_api_key_for(&self, provider: ModelProvider, key: &str) -> PortResult<()> {
        store(provider, key)
    }

    fn clear_api_key_for(&self, provider: ModelProvider) -> PortResult<()> {
        clear(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跟真机的 Secret Service 打一次来回：存 → 读 → 删。
    ///
    /// 默认 `#[ignore]`：CI / 容器里没有密钥服务，跑了必然失败。本机验收用
    /// `cargo test -p vox --lib -- --ignored secret_service_round_trip`。
    #[test]
    #[ignore = "需要真机上跑着 Secret Service（gnome-keyring / KWallet）"]
    fn secret_service_round_trip() {
        let store = SecretServiceStore::new();
        let key = format!("sk-test-round-trip-{}", std::process::id());
        store
            .store_api_key_for(ModelProvider::Gpt, &key)
            .expect("写入密钥失败（Secret Service 不可用？）");
        let loaded = store
            .load_api_key_for(ModelProvider::Gpt)
            .expect("读取密钥失败");
        assert_eq!(
            loaded.as_deref(),
            Some(key.as_str()),
            "读回来的不是刚写进去的"
        );
        store
            .clear_api_key_for(ModelProvider::Gpt)
            .expect("删除密钥失败");
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gpt)
                .expect("读取密钥失败"),
            None,
            "删掉之后不该还能读出来"
        );
    }

    #[test]
    fn entry_names_are_per_provider() {
        let aliyun = user_for(ModelProvider::Aliyun);
        for other in ModelProvider::ALL {
            if other == ModelProvider::Aliyun {
                continue;
            }
            assert_ne!(aliyun, user_for(other), "两个服务商不该共用一条密钥条目");
        }
    }
}
