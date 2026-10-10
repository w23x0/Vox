//! 模型目录在线更新（路径 B：只更新数据，不更新整个程序）。
//!
//! 目录文件同时被前端（`app/ui/src/catalog.ts` 编译期 import）和 Rust 构建脚本
//! （`crates/vox-core/build.rs` 编译期读）打成二进制。在线改模型只能靠「运行时拉取
//! → 校验 → 落到用户可写目录」，让前端下次启动优先读覆盖版。这里只负责：
//! 拉 GitHub raw 的最新 catalog → 校验 `schema_version`/`verified_at` → 写进
//! `app_config_dir/catalog/{file}.json` → 前端 `read_catalog_override` 读回来。
//!
//! 不碰签名、不碰 `tauri-plugin-updater`、不碰发布流水线（那是路径 A 的事）。

use serde::Deserialize;
use std::path::PathBuf;
use vox_core::notice::key;
use vox_core::Msg;

/// 线上仓库。owner/repo 固定（后端写死，不接受前端传），只有分支会变。
const RAW_BASE: &str = "https://raw.githubusercontent.com/w23x0/Vox/main/catalog";

/// 装了更新后，前端点名要的文件长这样（只校验最关键的两个字段，
/// 完整结构由前端解析时再校验；这里给到 Rust 侧能确信的底线）。
#[derive(Deserialize)]
struct RemoteCatalogHeader {
    schema_version: i64,
    verified_at: String,
}

/// `catalog/*.json` 里已知的文件名，跟内置副本一一对应。
pub fn catalog_file(provider: &str) -> Option<&'static str> {
    match provider {
        "aliyun" => Some("aliyun.json"),
        "gemini" => Some("gemini.json"),
        "gpt" => Some("gpt.json"),
        _ => None,
    }
}

/// 覆盖版落盘目录：`app_config_dir/catalog/`，跟 settings.json 同一个根，用户可写。
fn override_dir(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("catalog")
}

/// 远程这个 provider 目录文件的 raw URL。
fn remote_url(provider: &str) -> Option<String> {
    catalog_file(provider).map(|file| format!("{RAW_BASE}{file}"))
}

/// 用 reqwest 拉一个字节串。网络是唯一可能慢/失败的地方，超时兜底。
async fn fetch(url: &str) -> Result<String, Msg> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| Msg::new(key::ERR_CATALOG_CLIENT).detail(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Msg::new(key::ERR_CATALOG_CONNECT).detail(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(Msg::new(key::ERR_CATALOG_STATUS).param("status", status));
    }
    resp.text()
        .await
        .map_err(|e| Msg::new(key::ERR_CATALOG_READ).detail(e.to_string()))
}

/// 校验远程文本的底线字段（schema_version 与 verified_at）。
fn validate(text: &str) -> Result<RemoteCatalogHeader, Msg> {
    let parsed: RemoteCatalogHeader = serde_json::from_str(text)
        .map_err(|e| Msg::new(key::ERR_CATALOG_INVALID_JSON).detail(e.to_string()))?;
    if parsed.schema_version < 2 {
        return Err(Msg::new(key::ERR_CATALOG_TOO_OLD).param("version", parsed.schema_version));
    }
    if parsed.verified_at.is_empty() {
        return Err(Msg::new(key::ERR_CATALOG_NO_VERIFIED_AT));
    }
    Ok(parsed)
}

fn unknown_provider(provider: &str) -> Msg {
    Msg::new(key::ERR_UNKNOWN_PROVIDER).param("name", provider)
}

/// 取本地覆盖版文本（没有就 None）。
pub fn read_override(config_dir: &std::path::Path, provider: &str) -> Option<String> {
    let file = catalog_file(provider)?;
    let path = override_dir(config_dir).join(file);
    std::fs::read_to_string(path).ok()
}

/// 取当前生效的 verified_at：优先覆盖版，其次内置（build.rs 生成的常量）。
pub fn local_verified_at(config_dir: &std::path::Path, provider: &str) -> String {
    if let Some(text) = read_override(config_dir, provider) {
        if let Ok(header) = serde_json::from_str::<RemoteCatalogHeader>(&text) {
            return header.verified_at;
        }
    }
    match provider {
        "gemini" => vox_core::catalog::GEMINI_CATALOG_VERIFIED_AT.to_string(),
        "gpt" => vox_core::catalog::GPT_CATALOG_VERIFIED_AT.to_string(),
        _ => vox_core::catalog::CATALOG_VERIFIED_AT.to_string(),
    }
}

/// 下载最新目录，但不落盘，只返回其 `verified_at`。是否算「有更新」由调用方对比。
pub async fn check_update(provider: &str) -> Result<String, Msg> {
    let url = remote_url(provider).ok_or_else(|| unknown_provider(provider))?;
    let text = fetch(&url).await?;
    let header = validate(&text)?;
    Ok(header.verified_at)
}

/// 真正下载并覆盖本地覆盖版。写盘用临时文件 + rename，断电不留半个 JSON。
pub async fn apply_update(
    config_dir: &std::path::Path,
    provider: &str,
) -> Result<(String, String), Msg> {
    let file = catalog_file(provider).ok_or_else(|| unknown_provider(provider))?;
    let url = remote_url(provider).ok_or_else(|| unknown_provider(provider))?;
    let text = fetch(&url).await?;
    validate(&text)?;

    let dir = override_dir(config_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Msg::new(key::ERR_CATALOG_WRITE).detail(e.to_string()))?;
    let tmp = dir.join(format!("{file}.tmp"));
    let dst = dir.join(file);
    std::fs::write(&tmp, &text)
        .map_err(|e| Msg::new(key::ERR_CATALOG_WRITE).detail(e.to_string()))?;
    std::fs::rename(&tmp, &dst)
        .map_err(|e| Msg::new(key::ERR_CATALOG_WRITE).detail(e.to_string()))?;

    let applied =
        read_override(config_dir, provider).ok_or_else(|| Msg::new(key::ERR_CATALOG_READBACK))?;
    let applied_header: RemoteCatalogHeader = serde_json::from_str(&applied)
        .map_err(|e| Msg::new(key::ERR_CATALOG_PARSE_APPLIED).detail(e.to_string()))?;
    Ok((file.to_string(), applied_header.verified_at))
}
