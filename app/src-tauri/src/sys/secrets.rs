//! `SecretStore` 的 Windows 实现：DPAPI 加密后落盘。
//!
//! 密钥**绝不进 settings.json**，按服务商写入独立文件，
//! 用 `CryptProtectData` 按当前 Windows 用户加密。
//! 加了 `CRYPTPROTECT_UI_FORBIDDEN`——后台进程绝不能弹 UI 卡住。
//! `pOptionalEntropy` 传固定盐 `b"Vox/api-key/v1"`，
//! 这样别的程序就算拿到密文文件也不能直接 unprotect。

use std::fs;
use std::path::PathBuf;
use std::ptr;

use vox_core::ports::{PortError, PortResult, SecretStore};
use vox_core::settings::ModelProvider;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// 应用相关盐：加解密两边必须一致，拦截别的程序直接 unprotect。
const ENTROPY: &[u8] = b"Vox/api-key/v1";

pub struct DpapiSecretStore {
    path: PathBuf,
}

impl DpapiSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn path_for(&self, provider: ModelProvider) -> PathBuf {
        if provider == ModelProvider::Aliyun {
            self.path.clone()
        } else {
            let stem = self
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("secret");
            let extension = self
                .path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("bin");
            self.path
                .with_file_name(format!("{stem}-{}.{extension}", provider.as_id()))
        }
    }
}

impl SecretStore for DpapiSecretStore {
    fn store_api_key(&self, key: &str) -> PortResult<()> {
        let plaintext = key.as_bytes();
        let ciphertext = dpapi_protect(plaintext)?;

        // 原子写：先写 .tmp 再 rename，断电也不会留半截文件。
        let tmp_path = self.path.with_extension("tmp");
        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| PortError::new(format!("创建密钥目录失败: {e}")))?;
        }
        fs::write(&tmp_path, &ciphertext)
            .map_err(|e| PortError::new(format!("写入临时密钥文件失败: {e}")))?;
        fs::rename(&tmp_path, &self.path)
            .map_err(|e| PortError::new(format!("重命名密钥文件失败: {e}")))?;

        tracing::debug!("API 密钥已加密存储（{} 字节密文）", ciphertext.len());
        Ok(())
    }

    fn load_api_key(&self) -> PortResult<Option<String>> {
        // 文件不存在 → 没有可用的密钥，不是错误。
        let ciphertext = match fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(PortError::new(format!("读取密钥文件失败: {e}")));
            }
        };

        // 解密失败 → 换了用户/重装系统，旧密文注定解不开。
        // 这不是错误，是"没有可用的密钥"。删掉坏文件，返回 None。
        let plaintext = match dpapi_unprotect(&ciphertext) {
            Ok(bytes) => bytes,
            Err(_) => {
                tracing::warn!("DPAPI 解密失败（可能换了 Windows 账户），删除旧密钥文件");
                let _ = fs::remove_file(&self.path);
                return Ok(None);
            }
        };

        // UTF-8 解码——理论上只有我们自己写的数据，但防御性检查。
        let key = match String::from_utf8(plaintext) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("密钥文件解密后不是合法 UTF-8，删除");
                let _ = fs::remove_file(&self.path);
                return Ok(None);
            }
        };

        tracing::debug!("已加载 API 密钥（{} 字符）", key.len());
        Ok(Some(key))
    }

    fn clear_api_key(&self) -> PortResult<()> {
        // 幂等：文件本来就不存在也算成功。
        match fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::debug!("已删除密钥文件");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PortError::new(format!("删除密钥文件失败: {e}"))),
        }
    }

    fn load_api_key_for(&self, provider: ModelProvider) -> PortResult<Option<String>> {
        load_secret(&self.path_for(provider))
    }

    fn store_api_key_for(&self, provider: ModelProvider, key: &str) -> PortResult<()> {
        store_secret(&self.path_for(provider), key)
    }

    fn clear_api_key_for(&self, provider: ModelProvider) -> PortResult<()> {
        clear_secret(&self.path_for(provider))
    }
}

fn store_secret(path: &PathBuf, key: &str) -> PortResult<()> {
    let ciphertext = dpapi_protect(key.as_bytes())?;
    let tmp_path = path.with_extension("tmp");
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent).map_err(|e| PortError::new(format!("创建密钥目录失败: {e}")))?;
    }
    fs::write(&tmp_path, &ciphertext)
        .map_err(|e| PortError::new(format!("写入临时密钥文件失败: {e}")))?;
    fs::rename(&tmp_path, path).map_err(|e| PortError::new(format!("重命名密钥文件失败: {e}")))?;
    Ok(())
}

fn load_secret(path: &PathBuf) -> PortResult<Option<String>> {
    let ciphertext = match fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PortError::new(format!("读取密钥文件失败: {e}"))),
    };
    let plaintext = match dpapi_unprotect(&ciphertext) {
        Ok(bytes) => bytes,
        Err(_) => {
            tracing::warn!(path = %path.display(), "DPAPI 解密失败，删除旧密钥文件");
            let _ = fs::remove_file(path);
            return Ok(None);
        }
    };
    match String::from_utf8(plaintext) {
        Ok(s) => Ok(Some(s)),
        Err(_) => {
            let _ = fs::remove_file(path);
            Ok(None)
        }
    }
}

fn clear_secret(path: &PathBuf) -> PortResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PortError::new(format!("删除密钥文件失败: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// DPAPI 封装
// ---------------------------------------------------------------------------

/// 用 CryptProtectData 加密一块字节。返回密文。
fn dpapi_protect(plaintext: &[u8]) -> PortResult<Vec<u8>> {
    let mut input_buf = plaintext.to_vec();
    let mut entropy_buf = ENTROPY.to_vec();

    let data_in = CRYPT_INTEGER_BLOB {
        cbData: input_buf.len() as u32,
        pbData: input_buf.as_mut_ptr(),
    };
    let entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy_buf.len() as u32,
        pbData: entropy_buf.as_mut_ptr(),
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    // SAFETY: Win32 FFI。data_out.pbData 由 LocalAlloc 分配，我们负责 LocalFree。
    unsafe {
        CryptProtectData(
            &data_in,
            None,                      // szDataDescr
            Some(&entropy_blob),       // pOptionalEntropy
            None,                      // pvReserved
            None,                      // pPromptStruct
            CRYPTPROTECT_UI_FORBIDDEN, // 后台进程，不弹 UI
            &mut data_out,
        )
        .map_err(|e| PortError::new(format!("CryptProtectData 失败: {e}")))?;
    }

    // 把 LocalAlloc 出来的密文复制到 Vec，然后 LocalFree。
    let result =
        unsafe { std::slice::from_raw_parts(data_out.pbData, data_out.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(data_out.pbData as *mut _)));
    }

    Ok(result)
}

/// 用 CryptUnprotectData 解密一块密文。返回明文字节。
/// 解密后会先清零 LocalAlloc 缓冲再 LocalFree——防止明文在堆上留痕。
fn dpapi_unprotect(ciphertext: &[u8]) -> PortResult<Vec<u8>> {
    let mut input_buf = ciphertext.to_vec();
    let mut entropy_buf = ENTROPY.to_vec();

    let data_in = CRYPT_INTEGER_BLOB {
        cbData: input_buf.len() as u32,
        pbData: input_buf.as_mut_ptr(),
    };
    let entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy_buf.len() as u32,
        pbData: entropy_buf.as_mut_ptr(),
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    // SAFETY: Win32 FFI。data_out.pbData 由 LocalAlloc 分配，我们负责 LocalFree。
    unsafe {
        CryptUnprotectData(
            &data_in,
            None,                // ppszDataDescr
            Some(&entropy_blob), // pOptionalEntropy
            None,                // pvReserved
            None,                // pPromptStruct
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut data_out,
        )
        .map_err(|e| PortError::new(format!("CryptUnprotectData 失败: {e}")))?;
    }

    let len = data_out.cbData as usize;
    // 先复制明文到自己的 Vec。
    let result = unsafe { std::slice::from_raw_parts(data_out.pbData, len).to_vec() };

    // 内存卫生：把 LocalAlloc 缓冲里的明文清零再释放，
    // 防止密钥在进程堆上残留。
    unsafe {
        ptr::write_bytes(data_out.pbData, 0u8, len);
        let _ = LocalFree(Some(HLOCAL(data_out.pbData as *mut _)));
    }

    Ok(result)
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 每个测试用唯一路径，避免并行跑时打架。
    fn unique_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("vox_secret_test_{pid}_{id}.bin"))
    }

    /// 辅助：确保测试结束后文件被删除。
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            // .tmp 也顺手清一下
            let _ = fs::remove_file(self.0.with_extension("tmp"));
        }
    }

    #[test]
    fn roundtrip_ascii() {
        let path = unique_path();
        let _c = Cleanup(path.clone());
        let store = DpapiSecretStore::new(path);

        store.store_api_key("sk-abc123XYZ").unwrap();
        let loaded = store.load_api_key().unwrap();
        assert_eq!(loaded.as_deref(), Some("sk-abc123XYZ"));
    }

    #[test]
    fn providers_are_stored_independently() {
        let path = unique_path();
        let gemini_path = DpapiSecretStore::new(path.clone()).path_for(ModelProvider::Gemini);
        let _aliyun_cleanup = Cleanup(path.clone());
        let _gemini_cleanup = Cleanup(gemini_path);
        let store = DpapiSecretStore::new(path);

        store
            .store_api_key_for(ModelProvider::Aliyun, "sk-aliyun")
            .unwrap();
        store
            .store_api_key_for(ModelProvider::Gemini, "AIza-gemini")
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
            Some("AIza-gemini")
        );
    }

    #[test]
    fn gpt_uses_id_suffixed_secret_path_and_roundtrips() {
        let path = unique_path();
        let gpt_path = DpapiSecretStore::new(path.clone()).path_for(ModelProvider::Gpt);
        let _cleanup = Cleanup(gpt_path.clone());
        let store = DpapiSecretStore::new(path);

        let gpt_filename = gpt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        assert!(
            gpt_filename.ends_with("-gpt.bin"),
            "expected a secret-gpt.bin path, got {gpt_filename}"
        );
        store
            .store_api_key_for(ModelProvider::Gpt, "sk-gpt")
            .unwrap();
        assert_eq!(
            store
                .load_api_key_for(ModelProvider::Gpt)
                .unwrap()
                .as_deref(),
            Some("sk-gpt")
        );
    }

    #[test]
    fn roundtrip_unicode_and_emoji() {
        // 验证 UTF-8 往返：中日韩字符 + emoji
        let key = "密钥テスト키\u{1F511}\u{1F30D}";
        let path = unique_path();
        let _c = Cleanup(path.clone());
        let store = DpapiSecretStore::new(path);

        store.store_api_key(key).unwrap();
        let loaded = store.load_api_key().unwrap();
        assert_eq!(loaded.as_deref(), Some(key));
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let path = unique_path();
        // 确保文件不存在
        let _ = fs::remove_file(&path);
        let store = DpapiSecretStore::new(path);

        let result = store.load_api_key().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn load_returns_none_for_garbage_and_deletes_file() {
        let path = unique_path();
        let _c = Cleanup(path.clone());

        // 写一堆随机垃圾字节
        fs::write(
            &path,
            b"this is not valid dpapi ciphertext at all!!! \xff\xfe\x00",
        )
        .unwrap();

        let store = DpapiSecretStore::new(path.clone());
        let result = store.load_api_key().unwrap();
        assert_eq!(result, None, "解密垃圾应返回 None 而不是 panic");
        // 坏文件应该被删掉了
        assert!(!path.exists(), "坏文件应该被自动删除");
    }

    #[test]
    fn clear_is_idempotent() {
        let path = unique_path();
        // 确保文件不存在
        let _ = fs::remove_file(&path);

        let store = DpapiSecretStore::new(path.clone());
        // 文件本来就不存在，clear 应该成功
        store.clear_api_key().unwrap();
        // 写一个再删
        store.store_api_key("temp").unwrap();
        assert!(path.exists());
        store.clear_api_key().unwrap();
        assert!(!path.exists());
        // 再删一次也不报错
        store.clear_api_key().unwrap();
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        // 证明落盘的是真密文，不是明文。
        let key = "sk-live-SUPER-SECRET-KEY-12345678";
        let path = unique_path();
        let _c = Cleanup(path.clone());
        let store = DpapiSecretStore::new(path.clone());

        store.store_api_key(key).unwrap();
        let ciphertext = fs::read(&path).unwrap();

        // 密文里不应包含明文的任何连续 8 字节子串
        let key_bytes = key.as_bytes();
        for window in key_bytes.windows(8) {
            assert!(
                !ciphertext.windows(8).any(|w| w == window),
                "密文中发现明文子串——加密可能没生效"
            );
        }
    }
}
