//! 给用户看的消息：稳定的 key + 可插值的参数 + 服务端/系统原文。
//!
//! 芯里**不存任何一种语言的句子**。通知（[`crate::event::Notice`]）和命令的错误都是
//! [`Msg`]：`key` 是点分 id，`params` 只放可插值的值（设备名、模型名…），`detail` 放
//! 服务端或系统给的**原文**，不翻译、原样接在译文后面。前端按 key 查
//! `catalog/notices.json`（zh / en / ja），缺 key 时回退 English，再回退 key 本身。
//!
//! 无屏档没有前端，用 [`Msg::render_en`] 渲染——读的是**同一个** `catalog/notices.json`，
//! 所以与前端 en 文案不可能漂移。
//!
//! 新增一条消息：在 [`declare_keys!`] 里加一行 + 在 `catalog/notices.json` 里写三语。
//! 漏了任何一边，`notice::tests` 与 `npm run check:i18n` 都会红。

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Serialize;

/// 三语文案表。前端与 Rust 读同一个文件。
pub const CATALOG_JSON: &str = include_str!("../../../catalog/notices.json");

/// 一条给用户看的消息。线上形状 `{ key, params, detail? }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Msg {
    pub key: &'static str,
    pub params: BTreeMap<&'static str, String>,
    /// 服务端 / 系统给的原文。不翻译。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Msg {
    pub fn new(key: &'static str) -> Self {
        Self {
            key,
            params: BTreeMap::new(),
            detail: None,
        }
    }

    pub fn param(mut self, name: &'static str, value: impl ToString) -> Self {
        self.params.insert(name, value.to_string());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// 用 English 渲染（无屏档日志、MCP 账本）。查不到 key 就返回 key 本身——与前端同一条回退。
    pub fn render_en(&self) -> String {
        self.render_lang("en")
    }

    /// 按界面语言码（`zh-CN` / `ja-JP` / `en`，看前缀）渲染；缺这一语言就回退 English，
    /// 再缺就是 key 本身——不回退中文，与前端同一条回退。托盘菜单、启动失败对话框这类
    /// 前端还没起来的地方用它。
    pub fn render_lang(&self, ui_language: &str) -> String {
        let table = table();
        let lang = if ui_language.starts_with("zh") {
            "zh"
        } else if ui_language.starts_with("ja") {
            "ja"
        } else {
            "en"
        };
        let pick = |l: &str| table.text.get(&(l.to_string(), self.key.to_string()));
        let template = pick(lang).or_else(|| pick("en")).map(String::as_str);
        let mut text = interpolate(template.unwrap_or(self.key), &self.params);
        if let Some(detail) = &self.detail {
            text.push_str(table.separators.get(lang).map_or(": ", String::as_str));
            text.push_str(detail);
        }
        text
    }
}

impl std::fmt::Display for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render_en())
    }
}

/// 把模板里的 `{name}` 换成参数；没有对应参数的原样留着。
fn interpolate(template: &str, params: &BTreeMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match params.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

struct Table {
    /// `(语言, key) -> 模板`。
    text: HashMap<(String, String), String>,
    separators: HashMap<String, String>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        // 文件是 `include_str!` 进来的、由 `notice::tests` 钉死形状，解析失败 = 构建产物坏了。
        let root: serde_json::Value =
            serde_json::from_str(CATALOG_JSON).unwrap_or(serde_json::Value::Null);
        let mut text = HashMap::new();
        if let Some(messages) = root["messages"].as_object() {
            for (key, langs) in messages {
                for lang in ["zh", "en", "ja"] {
                    if let Some(t) = langs[lang].as_str() {
                        text.insert((lang.to_string(), key.clone()), t.to_string());
                    }
                }
            }
        }
        let separators = ["zh", "en", "ja"]
            .iter()
            .filter_map(|lang| {
                root["detail_separator"][*lang]
                    .as_str()
                    .map(|t| (lang.to_string(), t.to_string()))
            })
            .collect();
        Table { text, separators }
    })
}

/// 声明全部 key：生成 `key::NAME` 常量和 [`ALL_KEYS`]。
/// `check-i18n.mjs` 靠正则读这个宏里的 `= "…"`，所以**一行一个**、字面量写在行内。
macro_rules! declare_keys {
    ($($name:ident = $id:literal,)*) => {
        /// 全部 key 常量。用常量而不是字面量，写错 key 编译不过。
        pub mod key {
            $(pub const $name: &str = $id;)*
        }
        /// 声明过的全部 key（给测试与前端检查脚本核对用）。
        pub const ALL_KEYS: &[&str] = &[$($id),*];
    };
}

declare_keys! {
    HOST_FACTS_EXCESS = "notice.hostFactsExcess",
    KEY_READ_FAILED = "notice.keyReadFailed",
    KEY_SAVE_FAILED = "notice.keySaveFailed",
    PLAINTEXT_KEYS = "notice.plaintextKeys",
    RESTART_SPEAK = "notice.restartSpeak",
    RESTART_LISTEN = "notice.restartListen",
    API_KEY_MISSING = "notice.apiKeyMissing",
    LISTEN_TARGET_MISSING = "notice.listenTargetMissing",
    HOTKEY_REGISTER_FAILED = "notice.hotkeyRegisterFailed",
    HOTKEY_HOST_FAILED = "notice.hotkeyHostFailed",
    PIPELINE_FAILED = "notice.pipelineFailed",
    SESSION_PLAN_INVALID = "notice.sessionPlanInvalid",
    NO_CAPTURE_INPUT = "notice.noCaptureInput",
    CONNECT_FAILED = "notice.connectFailed",
    PLAYBACK_OPEN_FAILED = "notice.playbackOpenFailed",
    CAPTURE_START_FAILED = "notice.captureStartFailed",
    CHAIN_BUILD_FAILED = "notice.chainBuildFailed",
    PASSTHROUGH_OUTPUT_FAILED = "notice.passthroughOutputFailed",
    MONITOR_FAILED = "notice.monitorFailed",
    TRANSLATION_OUTPUT_FAILED = "notice.translationOutputFailed",
    DENOISE_DISABLED = "notice.denoiseDisabled",
    SERVER_RATE_LIMITED = "notice.server.rateLimited",
    SERVER_QUOTA_EXHAUSTED = "notice.server.quotaExhausted",
    SERVER_MODEL_UNAVAILABLE = "notice.server.modelUnavailable",
    SERVER_INVALID_KEY = "notice.server.invalidKey",
    SERVER_ERROR = "notice.server.error",
    SERVER_ERROR_CODE = "notice.server.errorCode",
    SERVER_CLOSE_QUOTA = "notice.server.closeQuota",
    SERVER_CLOSE_REFUSED = "notice.server.closeRefused",
    CONTROL_PLANE_DOWN = "notice.controlPlaneDown",
    OVERLAY_FAILED = "notice.overlayFailed",
    OVERLAY_THREAD_CRASHED = "notice.overlayThreadCrashed",
    OVERLAY_THREAD_MISSING = "notice.overlayThreadMissing",
    DEVICE_SCAN_CRASHED = "notice.deviceScanCrashed",
    TRAY_FAILED = "notice.trayFailed",
    TRAY_MISSING = "notice.trayMissing",
    PIPEWIRE_MISSING = "notice.pipewireMissing",
    PIPEWIRE_MISSING_HEADLESS = "notice.pipewireMissingHeadless",
    HEADLESS_NEEDS_LINUX = "notice.headlessNeedsLinux",
    CABLE_HIDE_FAILED = "notice.cableHideFailed",
    CABLE_RESTORE_FAILED = "notice.cableRestoreFailed",
    TRAY_SPEAK = "tray.speak",
    TRAY_LISTEN = "tray.listen",
    TRAY_SUBTITLE = "tray.subtitle",
    TRAY_OPEN_SETTINGS = "tray.openSettings",
    TRAY_QUIT = "tray.quit",
    STARTUP_FAILED_TITLE = "startup.failedTitle",
    STARTUP_FAILED_BODY = "startup.failedBody",
    INPUT_DEVICE_NOT_FOUND = "notice.inputDeviceNotFound",
    OUTPUT_DEVICE_NOT_FOUND = "notice.outputDeviceNotFound",
    PROGRAM_SILENT = "notice.programSilent",
    PROGRAM_NOT_RUNNING = "notice.programNotRunning",
    INPUT_PERMISSION = "notice.inputPermission",
    CAPTURE_BY_PROGRAM_UNSUPPORTED = "notice.captureByProgramUnsupported",
    ERR_MODEL_LIST_TASK = "error.modelListTask",
    ERR_SETTINGS_SERIALIZE = "error.settingsSerialize",
    ERR_SETTINGS_INVALID = "error.settingsInvalid",
    ERR_OSC_NOT_STARTED = "error.oscNotStarted",
    ERR_OSC_AVATAR_PARAM_MISSING = "error.oscAvatarParamMissing",
    ERR_OSC_FAILED = "error.oscFailed",
    ERR_NET_TEST_TASK = "error.netTestTask",
    ERR_TAKEOVER_TASK = "error.takeoverTask",
    ERR_DEVICE_SCAN_TASK = "error.deviceScanTask",
    ERR_UNKNOWN_PIPELINE = "error.unknownPipeline",
    ERR_UNKNOWN_PROVIDER = "error.unknownProvider",
    ERR_CONFIG_DIR = "error.configDir",
    ERR_OPEN_BROWSER = "error.openBrowser",
    ERR_CATALOG_CLIENT = "error.catalog.client",
    ERR_CATALOG_CONNECT = "error.catalog.connect",
    ERR_CATALOG_STATUS = "error.catalog.status",
    ERR_CATALOG_READ = "error.catalog.read",
    ERR_CATALOG_INVALID_JSON = "error.catalog.invalidJson",
    ERR_CATALOG_TOO_OLD = "error.catalog.tooOld",
    ERR_CATALOG_NO_VERIFIED_AT = "error.catalog.noVerifiedAt",
    ERR_CATALOG_WRITE = "error.catalog.write",
    ERR_CATALOG_READBACK = "error.catalog.readback",
    ERR_CATALOG_PARSE_APPLIED = "error.catalog.parseApplied",
    ERR_VIRTUAL_CABLE_NOT_NEEDED = "error.virtualCableNotNeeded",
    ERR_CABLE_TASK = "error.cableTask",
    ERR_CABLE_BLOCKERS_TASK = "error.cableBlockersTask",
    ERR_CABLE_ENDPOINT_TASK = "error.cableEndpointTask",
    ERR_CABLE_BUSY = "error.cableBusy",
    ERR_CABLE_REBOOT_BEFORE_INSTALL = "error.cableRebootBeforeInstall",
    ERR_CABLE_REBOOT_BEFORE_UNINSTALL = "error.cableRebootBeforeUninstall",
    ERR_CABLE_ELEVATION_DECLINED = "error.cableElevationDeclined",
    ERR_CABLE_ENDPOINT_ELEVATION_DECLINED = "error.cableEndpointElevationDeclined",
    ERR_CABLE_INSTALL_FAILED = "error.cableInstallFailed",
    ERR_CABLE_UNINSTALL_FAILED = "error.cableUninstallFailed",
    ERR_CABLE_ENDPOINT_FAILED = "error.cableEndpointFailed",
    ERR_CABLE_CLOSE_APP = "error.cableCloseApp",
    ERR_CABLE_BLOCKERS_CHECK = "error.cableBlockersCheck",
    ERR_OPEN_CABLE_SITE = "error.openCableSite",
    ERR_OPEN_CABLE_DONATION = "error.openCableDonation",
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn root() -> serde_json::Value {
        serde_json::from_str(CATALOG_JSON).expect("catalog/notices.json 是合法 JSON")
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            found.insert(after[..close].to_string());
            rest = &after[close + 1..];
        }
        found
    }

    /// 声明过的每个 key 三语齐全；占位符三语一致；文件里没有没人声明的 key。
    #[test]
    fn every_declared_key_has_all_three_languages_and_no_orphans() {
        let root = root();
        let messages = root["messages"].as_object().expect("messages 是对象");
        let mut problems = Vec::new();

        for key in ALL_KEYS {
            let Some(entry) = messages.get(*key) else {
                problems.push(format!("{key}：notices.json 里没有这条"));
                continue;
            };
            let mut sets = Vec::new();
            for lang in ["zh", "en", "ja"] {
                match entry[lang].as_str() {
                    Some(text) if !text.trim().is_empty() => sets.push((lang, placeholders(text))),
                    _ => problems.push(format!("{key}：缺 {lang} 文案")),
                }
            }
            if let Some((_, first)) = sets.first() {
                if sets.iter().any(|(_, set)| set != first) {
                    problems.push(format!("{key}：三语占位符不一致 {sets:?}"));
                }
            }
        }
        for key in messages.keys() {
            if !ALL_KEYS.contains(&key.as_str()) {
                problems.push(format!("{key}：notices.json 里有，但 declare_keys! 没声明"));
            }
        }
        let mut seen = BTreeSet::new();
        for key in ALL_KEYS {
            if !seen.insert(*key) {
                problems.push(format!("{key}：重复声明"));
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    /// 英文渲染：参数插值 + `detail` 原样接在后面（不翻译、不改动）。
    #[test]
    fn render_en_interpolates_params_and_keeps_detail_verbatim() {
        let original = "Free tier of the model has been exhausted. 余额不足 <&>";
        let rendered = Msg::new(key::SERVER_CLOSE_QUOTA)
            .detail(original)
            .render_en();
        assert!(rendered.ends_with(original), "{rendered}");
        assert!(
            rendered.starts_with("This model's quota is used up"),
            "{rendered}"
        );

        let with_param = Msg::new(key::PROGRAM_NOT_RUNNING)
            .param("name", "chrome")
            .render_en();
        assert_eq!(
            with_param,
            "\"chrome\" is not running (the program is closed, or the name is wrong)"
        );
    }

    #[test]
    fn render_lang_picks_the_language_and_falls_back_to_english() {
        let m = Msg::new(key::TRAY_QUIT);
        assert_eq!(m.render_lang("zh-CN"), "退出");
        assert_eq!(m.render_lang("ja-JP"), "終了");
        assert_eq!(m.render_lang("en"), "Quit");
        assert_eq!(m.render_lang("fr"), "Quit");
    }

    /// 查不到的 key：渲染成 key 本身（不是空串、不是中文）。
    #[test]
    fn unknown_key_renders_as_the_key_itself() {
        assert_eq!(Msg::new("notice.nope").render_en(), "notice.nope");
    }

    #[test]
    fn wire_shape_is_key_params_detail() {
        let json = serde_json::to_value(
            Msg::new(key::INPUT_DEVICE_NOT_FOUND)
                .param("name", "x")
                .detail("os says no"),
        )
        .unwrap();
        assert_eq!(json["key"], "notice.inputDeviceNotFound");
        assert_eq!(json["params"]["name"], "x");
        assert_eq!(json["detail"], "os says no");
        let bare = serde_json::to_value(Msg::new(key::API_KEY_MISSING)).unwrap();
        assert!(bare.get("detail").is_none(), "{bare}");
    }
}
