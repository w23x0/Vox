//! 隐藏 CLI：`--print-composition`（S0 §4.3-A 的验收出口）。
//!
//! 打**两份清单 + 当前有效能力位**的 JSON 到 stdout 就退：不建窗口、不注册命令、不起线程、
//! 不改任何状态、**不建配置目录**（只读：读设置走 [`crate::persist::Persist::new`]，那条路
//! 不碰盘）。无人值守时它是"这台机器现在会怎么装"的唯一可读出口（无屏档是同一条命令，
//! 见 `crates/vox-headless/src/status.rs::composition_json`——两边同形、同一份组装
//! `vox_mcp::endpoints::document`：形状逐字相同；取值随档位与宿主事实本就不同）。
//!
//! **为什么排在 Tauri 之前**（跟 `platform::pre_main()` 的 `--vox-restore-defaults` 同一条理由）：
//! `.setup()` 已经太晚——单实例插件是第一个注册的，第二份进程会被它当成"重复启动"、
//! 发个事件就悄悄退掉，那样这个命令有人的时候就不打东西。同理，命令注册、托盘、热键
//! 这些"应用起来之后的事"一件都不做。
//!
//! **为什么要 `Builder::build()`（只 build 不 run）**：配置目录的唯一真源是 Tauri 的
//! `app.path().app_config_dir()`（`dirs::config_dir()/identifier`）——`assemble()` 用的就是它。
//! 在这个文件里自己拼一份 `$XDG_CONFIG_HOME/…` / `%APPDATA%\…` 就等于把"settings.json 在哪"
//! 变成两份会漂移的规矩：打出来的清单可能根本不是这个应用在用的那份配置，而验收恰恰靠它。
//! `build()` 不建窗口（窗口在 `run()` 的 setup 里才建）、不跑事件循环，所以不会闪界面；
//! 代价是这条路仍要初始化一次平台事件循环（GTK / Win32）——桌面档的机器上本来就有。
//!
//! Windows 的**发布构建没有控制台**（`main.rs` 的 `windows_subsystem = "windows"`），
//! 所以要看这份 JSON 就重定向到文件：`vox.exe --print-composition > composition.json`
//! （跟 `--vox-restore-defaults` 一样，它是给脚本/诊断用的出口，不是给人双击的）。

use tauri::Manager;
use vox_core::runtime::Runtime;
use vox_mcp::Ledger;

use crate::platform;

/// 触发这个模式的参数。
pub const FLAG: &str = "--print-composition";

/// 参数里带没带这个开关（argv 的位置不限：unit / 快捷方式都能随手加）。
pub fn requested() -> bool {
    std::env::args().skip(1).any(|arg| arg == FLAG)
}

/// 装配（只到"设置 + 事实"这一步）→ 打 JSON → 退出。**不返回**。
///
/// 退出码跟无屏入口同一套：`0` 成功 ｜ `2` 打不出来（路径解析失败 / 序列化失败）。
pub fn print_and_exit() -> ! {
    let code = match print() {
        Ok(()) => 0,
        Err(error) => {
            // 没有界面可以弹框（窗口都没建），错误就落在 stderr 上。
            eprintln!("打印清单失败：{error}");
            2
        }
    };
    std::process::exit(code)
}

fn print() -> Result<(), Box<dyn std::error::Error>> {
    // 只 build 不 run：要的是路径解析器（`app.path()`），不是那个应用。
    let app = tauri::Builder::default().build(tauri::generate_context!())?;
    let config_dir = app.path().app_config_dir()?;
    tracing::info!(
        config_dir = %config_dir.display(),
        "打印清单（不建窗口、不注册命令、不改任何状态）"
    );
    let persist = crate::persist::Persist::new(config_dir);
    let settings = persist.load_settings();

    // 事实先注入再派生：位由芯算（档位上限 − 关掉的），外壳只报事实（S0 §2.5.0）。
    // 这里**没有装配过任何东西**，所以那几位"装配期定义者"（悬浮窗 / 热键 / 自启）与 Linux 的
    // 虚拟麦节点都如实报 `not_wired`——那正是这个命令的意思："还没跑起来时"的清单。
    // 后果要说清：Linux 上 `speak.out[0].role` 因此是 `speaker` 而不是 `virtual_mic`
    // （真跑起来时节点由 `virtual_mic_ensure()` 建出来、位才为真）。要**活的**那一份清单，
    // 问跑着的应用：S1 的 `describe_endpoint` 报的就是它。
    let runtime = Runtime::new(settings, platform::clock());
    runtime.set_host_facts(platform::host_facts());

    println!("{}", document(&runtime)?);
    Ok(())
}

/// 这份文档的**形状**（键名 / 嵌套 / 键顺序）与无屏档的 `status::composition_json` 逐字相同；
/// **取值随档位与宿主事实本就不同**（位上限、`host`、设备名都该不一样——同形说的是骨架，不是内容）：
///
/// ```text
/// {
///   "capabilities": <CapabilityReport>,   // 当前**有效**位
///   "speak":  <Composition> | null,       // 这条腿现在派出来的清单（派不出来就是 null）
///   "listen": <Composition> | null,
///   "errors": [ { endpoint, code, message, detail? } ]   // 派不出来的理由（`endpoint_unavailable`）
/// }
/// ```
///
/// 组装**只有一条路**：`vox_mcp::endpoints::document`——它内部走 `manifest`（**与 S1 的
/// `describe_endpoint` 同一个函数**，所以这份打印与 Agent 面看到的是同一份清单）与 `wire`
/// （文本往返一趟，与报给客户端的线上形态逐字同形；`to_value` 会把 `f32` 摊成 `f64`）。
/// 两个外壳共用它，各自只留一句日志（vox-mcp 的依赖表里没有 tracing，所以"往哪记"留在入口）。
fn document(runtime: &Runtime) -> Result<String, serde_json::Error> {
    let ledger: &dyn Ledger = runtime;
    vox_mcp::endpoints::document(ledger, &mut |endpoint, error| {
        tracing::warn!(endpoint = %endpoint.as_str(), reason = %error.message, "这条腿现在派不出清单");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::Settings;

    /// 形状契约：两条腿的键都在，能力位就是这一档的，派不出来的那条腿进 `errors`。
    /// （无屏档那份**形状**（键名/嵌套）由 `vox-headless` 的用例钉住；这里钉桌面这一边。
    /// **键顺序没有任何用例断言**——两个外壳调的是同一个 `vox_mcp::endpoints::document`，
    /// 顺序由那一次 `serde_json` 序列化（缺省不开 `preserve_order`，顶层是 `BTreeMap` 的
    /// 名字序）决定，两边同形是**构造上的**，不是被钉住的。）
    #[test]
    fn the_document_carries_both_legs_and_the_bits() {
        let runtime = Runtime::new(Settings::default(), platform::clock());
        runtime.set_host_facts(platform::host_facts());

        let json = document(&runtime).expect("清单该能序列化");
        let document: serde_json::Value = serde_json::from_str(&json).expect("合法 JSON");

        assert_eq!(
            document["capabilities"]["tier"],
            serde_json::to_value(platform::host_kind()).expect("档位名是数据")
        );
        assert!(document["speak"].is_object(), "{document}");
        for key in ["speak", "listen"] {
            assert!(
                document.get(key).is_some(),
                "两条腿的键都得在（哪怕值是 null）：{document}"
            );
        }
        // 缺省没有要抓的程序 → `listen` 派不出来，如实 null + 一个带 reason 的错误。
        assert!(document["listen"].is_null(), "{document}");
        let errors = document["errors"].as_array().expect("errors 是数组");
        assert_eq!(errors[0]["endpoint"], "listen");
        assert_eq!(errors[0]["code"], "endpoint_unavailable");
    }
}
