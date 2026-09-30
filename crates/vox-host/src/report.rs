//! 两个外壳逐字同形的两个报告出口（机器读的那一面）。
//!
//! 无屏设备没有屏幕，所以"这台盒子能做什么、两条腿会怎么装"只能从 JSON 里读；
//! 桌面的 `--print-composition` 打的是同一份东西。**两个外壳共用它**，各自只留一句日志。

use vox_core::runtime::Runtime;
use vox_mcp::Ledger;

/// 能力位报告的 JSON（缩进过，方便人直接看；`jq -c` 一行照样能用）。
pub fn capabilities_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&runtime.capabilities())
}

/// 清单的 JSON：**两条腿 + 当前有效的能力位**（S0 §4.3-A，`--print-composition` 打的就是它）。
///
/// 形状（键名就是 S0 §4.3-A 里那几个，缩进过——`jq -c` 一行照样能用）：
///
/// ```text
/// {
///   "capabilities": <CapabilityReport>,   // 当前**有效**位（档位上限 − 关掉的）
///   "speak":  <Composition> | null,       // 这条腿现在派出来的清单
///   "listen": <Composition> | null,       //   派不出来就是 null，理由进 errors
///   "errors": [ { endpoint, code, message, detail? } ]
/// }
/// ```
///
/// **两条腿都打**：位为假的格子在这一步就已经被芯关掉了（`Composition::of` 的开门/关门），
/// 所以无屏档打出来的"听人说话"里 `in` 是空的（`program_tap` 在这一档的上限之外）——
/// 那正是"位是事实"的证据，不是打错了。清单**派不出来**（比如还没选要抓的程序）时那个键是
/// `null`、理由进 `errors`：打一份看起来像清单的假清单比打 `null` 糟得多。
///
/// 派生走 `vox_mcp::endpoints::manifest`——**与 S1 的 `describe_endpoint` 同一个函数**，
/// 所以这份打印与 Agent 面看到的是同一份清单（S0 §4.3-A：它同时是 describe 出口的原型）。
/// 组装（遍历两条腿、拼 `errors`、缩进 JSON）走 `vox_mcp::endpoints::document`——两个外壳
/// 打出来的**形状**（键名、嵌套、键顺序）逐字相同，**取值随档位与宿主事实本就不同**（位上限、
/// `host`、设备名都该不一样——同形说的是骨架，不是内容；把取值也读成"逐字相同"就会把两档
/// 该有的差别当成 bug）。清单的线上形态也由它走 `vox_mcp::endpoints::wire`（文本往返一趟），
/// 与 S1 报给客户端的那一份逐字同形。
///
/// 失败只有一种出口：`serde_json::Error`（清单本身派不出来是**领域失败**，由 `document`
/// 写进 `errors`，不是这里的错误）。
pub fn composition_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    let ledger: &dyn Ledger = runtime;
    vox_mcp::endpoints::document(ledger, &mut |endpoint, error| {
        tracing::warn!(endpoint = %endpoint.as_str(), reason = %error.message, "这条腿现在派不出清单");
    })
}
