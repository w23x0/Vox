//! `voxctl`：Vox 控制面 CLI。
//!
//! **CLI 不是第二份实现**（设计稿 §2.2.2）：它只做三件事——把命令翻译成一条 JSON-RPC
//! 请求、交给协议层、打印结果。动作子命令走的是**本机 HTTP**（`vox_mcp::client` 那条瘦客户端
//! 路径）：`tools/call` 由控制面那一侧（桌面 / 无屏档，注入真账本）执行，所以"CLI 的输出 ==
//! MCP 的输出"仍然是**同一个 `vox_mcp::handle`** 出来的同一串字节。
//!
//! **命令面只有 5 个动作子命令**（`list-endpoints` / `describe-endpoint` / `compose-endpoint` /
//! `session-open` / `session-close`，名字由 [`vox_mcp::actions::ACTIONS`] 生成、参数与用法由各自的
//! `inputSchema` 生成）。协议层方法（`server/discover` / `tools/list` / `resources/read`…）
//! **不进命令面**——验协议用 curl / Inspector，CLI 只有"设备能做什么"这一个面。
//!
//! 除了动作子命令，本二进制还有三条**传输/调试**入口（都不是动作，也都不改"动作恰好 5 个"
//! 这条契约）：
//!
//! ```text
//! voxctl serve --state-file <path> [--port <n>]      # 起本机控制面 HTTP（127.0.0.1，单路径 /mcp）
//! voxctl serve-stdio --state-file <path>             # stdio ⇄ 本机 HTTP 的桥（给只会读写 stdio 的宿主）
//! voxctl --probe server/discover [--json]            # 在本进程内跑一遍协议层（不连 app、不碰账本）
//! voxctl --probe tools/list      [--json]
//! ```
//!
//! **每个连本机控制面的入口都要显式给 `--state-file`**（握手文件路径，即装配层写的那个
//! `<app_config_dir>/control.json`）。CLI **不猜目录**：那个路径由装配层决定（Tauri 的标识符
//! 说了算，设计稿 §5.1 第 5 条把它列为未核实项），猜一个只会猜错；`serve` 更是不许往猜出来的
//! 目录里写 token。
//!
//! `serve` 起的就是装配层用的同一个 [`vox_mcp::serve`]，**同一个 `handle`**：HTTP 面与
//! `--probe` 出来的字节因此逐字节相同（`tests/http.rs` 里有一条用例钉住这件事）。
//! 它不注入后端（`backend = None`），所以 `tools/call` 会如实回 `-32603`"后端未接入"——
//! 账本与设备由外壳（桌面装配层 `app/` 里的 `mcp.rs` / 无屏档 `crates/vox-headless`）注入，
//! CLI 只是协议面的第二个人口：**动作子命令要打的是那个有账本的控制面，不是 `serve` 起的这个**。
//!
//! `serve-stdio` 是**桥**，不是第二个服务端（设计稿 §2.2.1）：它把 stdin 上换行分隔的 JSON-RPC
//! 转成对本机 `/mcp` 的 POST，把响应（含 `subscriptions/listen` 的 SSE 事件）原样写回 stdout。
//! 它同样不拥有账本——第二个 Vox 实例会抢声卡。
//!
//! 退出码：`0` 成功 ｜ `1` 领域失败（`isError`）｜ `2` 传输/协议失败 ｜ `3` 用法错误。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Map, Value};

use vox_mcp::actions::{Action, ActionId, ACTIONS};
use vox_mcp::client::{ControlPlane, Reply};
use vox_mcp::mcp::{self, meta};
use vox_mcp::transport::http::{self, ServerOptions, PATH};

fn usage() -> String {
    let commands = ACTIONS
        .iter()
        .map(|action| action.cli_command())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\
voxctl —— Vox 控制面 CLI

用法：voxctl <子命令> [选项]

动作子命令（{} 个：{}）——把命令翻成一条 `tools/call`，打给**跑着的**本机控制面
（桌面 / 无屏档那个有账本的控制面；`serve` 起的这个没有账本）。用法与参数由各自的
inputSchema 生成：`voxctl <子命令> --help`。
  --state-file <path>   握手文件（装配层写的 <app_config_dir>/control.json）。**必填**：
                        CLI 不猜目录（那个路径由装配层决定）
  --json                只打 structuredContent（一行合法 JSON）；缺省打一行中文摘要

传输：
  serve --state-file <path> [--port <n>]   起本机控制面 HTTP（只绑 127.0.0.1，单路径 {PATH}，
                                           端口 0 = 系统分配）。端口与 token 写在握手文件里，
                                           Ctrl-C 停服。动作子命令要的账本后端由外壳注入，
                                           这里没有 → tools/call 回 -32603。
  serve-stdio --state-file <path>          stdio ⇄ 本机 HTTP 的桥：stdin 上换行分隔的 JSON-RPC
                                           转成对 /mcp 的 POST，响应（含订阅流的 SSE 事件）原样
                                           写回 stdout（stdout 只有 MCP 消息，日志走 stderr）。
                                           不拥有账本、不开设备。stdin 关了就是收工。

调试（本进程内跑一遍协议层；不连 app、不碰账本）：
  --probe server/discover   协议探测：支持的版本、能力、服务端身份
  --probe tools/list        工具清单（由动作清单生成）

选项：
  --json      动作子命令：只打 structuredContent；--probe：打印完整 JSON-RPC 响应一行
  -h, --help  显示本帮助

退出码：0 成功 ｜ 1 领域失败（isError）｜ 2 传输/协议失败 ｜ 3 用法错误
",
        ACTIONS.len(),
        commands
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run(&args))
}

fn run(args: &[String]) -> u8 {
    let Some(first) = args.first() else {
        eprint!("{}", usage());
        return 3;
    };

    if first == "-h" || first == "--help" {
        print!("{}", usage());
        return 0;
    }
    if first == "serve" {
        return serve(&args[1..]);
    }
    if first == "serve-stdio" {
        return serve_stdio(&args[1..]);
    }
    if let Some(action) = ACTIONS
        .iter()
        .find(|action| action.cli_command() == first.as_str())
    {
        return action_command(action, &args[1..]);
    }
    // `--probe`（以及 `--json --probe …` 这种今天就在用的顺序）都走调试那条路。
    if first.starts_with("--") {
        return probe(args);
    }

    eprint!("不认识的子命令：{first}\n\n{}", usage());
    3
}

// --- 动作子命令 ----------------------------------------------------------------

/// 一个动作的入参格：**从 `inputSchema` 现读**（不写第二份 schema，也不写第二份 flag 表）。
struct Field {
    /// 线上键名（JSON 属性名，如 `wait_ready_ms`）。
    name: String,
    /// 命令行开关（属性名的 kebab-case，如 `--wait-ready-ms`）。
    flag: String,
    kind: Kind,
    required: bool,
    /// `enum` 的取值（用法里列出来；**不在这里判对错**——见 [`value_for`]）。
    values: Vec<String>,
    description: String,
}

/// schema 里的四种类型（本清单只用到这四种；`$ref` 先解到 `$defs` 再判）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    String,
    Integer,
    Boolean,
    Object,
}

/// 读一份 `inputSchema` 里的格。顺序 = schema 里 `properties` 的顺序（serde_json 的默认 map
/// 是按键排序的，所以这里对同一份 schema 恒定），用法与请求体因此都可复现。
fn fields_of(action: &Action) -> Vec<Field> {
    let schema: Value = serde_json::from_str(action.input_schema)
        .expect("inputSchema 是编译期常量，必须是合法 JSON");
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new(); // 无参数的工具（`list_endpoints`）
    };

    properties
        .iter()
        .map(|(name, property)| {
            let resolved = resolve_ref(&schema, property);
            Field {
                name: name.clone(),
                flag: format!("--{}", name.replace('_', "-")),
                kind: match resolved.get("type").and_then(Value::as_str) {
                    Some("integer") => Kind::Integer,
                    Some("boolean") => Kind::Boolean,
                    Some("object") => Kind::Object,
                    // `string` 以及没写 type 的都按字符串处理：值照原样送，判对错是服务端的事。
                    _ => Kind::String,
                },
                required: required.contains(name),
                values: resolved
                    .get("enum")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                description: property
                    .get("description")
                    .or_else(|| resolved.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }
        })
        .collect()
}

/// 解一层本地 `$ref`（`compose_endpoint` 的 `composition` 写成 `#/$defs/composition`）。
/// 解不开就把原来那份拿回去——它至少还有 `description`。
fn resolve_ref<'a>(root: &'a Value, property: &'a Value) -> &'a Value {
    let Some(reference) = property.get("$ref").and_then(Value::as_str) else {
        return property;
    };
    let Some(name) = reference.strip_prefix("#/$defs/") else {
        return property;
    };
    root.get("$defs")
        .and_then(|defs| defs.get(name))
        .unwrap_or(property)
}

/// 一个动作子命令的用法（**由 `inputSchema` 生成**：必填、类型、枚举取值、说明都从那里来）。
fn action_usage(action: &Action) -> String {
    let fields = fields_of(action);
    let arguments = fields
        .iter()
        .map(|field| match field.kind {
            Kind::Boolean => format!(
                "[{}|--no-{}]",
                field.flag,
                field.flag.trim_start_matches("--")
            ),
            _ => format!("{} <值>", field.flag),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut lines = format!(
        "voxctl {} —— {}\n\n{}\n\n用法：voxctl {}{}\n\n参数（由 inputSchema 生成）：\n",
        action.cli_command(),
        action.title,
        action.description,
        action.cli_command(),
        if arguments.is_empty() {
            String::new()
        } else {
            format!(" {arguments}")
        },
    );
    if fields.is_empty() {
        lines.push_str("  （没有参数）\n");
    }
    for field in &fields {
        let kind = match field.kind {
            Kind::String => "字符串",
            Kind::Integer => "整数",
            Kind::Boolean => "布尔（`--k` = true，`--no-k` = false）",
            Kind::Object => "对象（JSON 文本，或 @文件.json）",
        };
        let required = if field.required { "（必填）" } else { "" };
        let values = if field.values.is_empty() {
            String::new()
        } else {
            format!("；取值：{}", field.values.join(" / "))
        };
        lines.push_str(&format!(
            "  {} <{}>{}{}{}\n",
            field.flag,
            kind,
            required,
            values,
            if field.description.is_empty() {
                String::new()
            } else {
                format!(" —— {}", field.description)
            }
        ));
    }
    lines.push_str(
        "\n选项：\n  --state-file <path>  握手文件（装配层写的 <app_config_dir>/control.json）。必填。\n  \
         --json               只打 structuredContent（一行合法 JSON）\n  -h, --help           显示本帮助\n\n\
         退出码：0 成功 ｜ 1 领域失败（isError）｜ 2 传输/协议失败 ｜ 3 用法错误\n",
    );
    lines
}

/// 跑一个动作子命令。
fn action_command(action: &Action, args: &[String]) -> u8 {
    let fields = fields_of(action);
    let mut arguments = Map::new();
    let mut state_file: Option<PathBuf> = None;
    let mut json_out = false;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => {
                print!("{}", action_usage(action));
                return 0;
            }
            "--json" => json_out = true,
            "--state-file" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    return usage_error(action, "--state-file 后面要跟路径。");
                };
                state_file = Some(PathBuf::from(path));
            }
            other => {
                let negative = other.starts_with("--no-");
                let flag = if negative {
                    format!("--{}", other.trim_start_matches("--no-"))
                } else {
                    other.to_string()
                };
                let Some(field) = fields.iter().find(|field| field.flag == flag) else {
                    return usage_error(action, &format!("不认识的参数：{other}"));
                };

                match field.kind {
                    Kind::Boolean => {
                        // `--k` / `--no-k` / `--k true|false`（显式取值只为脚本好写）。
                        let value = if negative {
                            false
                        } else if args.get(index + 1).map(String::as_str) == Some("true") {
                            index += 1;
                            true
                        } else if args.get(index + 1).map(String::as_str) == Some("false") {
                            index += 1;
                            false
                        } else {
                            true
                        };
                        arguments.insert(field.name.clone(), Value::Bool(value));
                    }
                    _ => {
                        if negative {
                            return usage_error(
                                action,
                                &format!("{other}：`--no-` 只用于布尔开关"),
                            );
                        }
                        index += 1;
                        let Some(raw) = args.get(index) else {
                            return usage_error(
                                action,
                                &format!("{} 后面要跟值（{}）", field.flag, kind_label(field.kind)),
                            );
                        };
                        match value_for(field, raw) {
                            Ok(value) => {
                                arguments.insert(field.name.clone(), value);
                            }
                            Err(message) => return usage_error(action, &message),
                        }
                    }
                }
            }
        }
        index += 1;
    }

    // 缺必填 = 用法错误（schema 里 `required` 说了算）。**值的对错不在这里判**：那是服务端按同一份
    // schema 的活儿，`--endpoint nope` 必须走协议层的 `-32602`（退出码 2）。
    for field in fields.iter().filter(|field| field.required) {
        if !arguments.contains_key(&field.name) {
            return usage_error(action, &format!("缺必填参数：{}", field.flag));
        }
    }

    let Some(state_file) = state_file else {
        return usage_error(
            action,
            "缺 --state-file <path>（握手文件路径，CLI 不猜目录）。",
        );
    };

    let message = request(
        1,
        mcp::METHOD_TOOLS_CALL,
        json!({ "name": action.name, "arguments": Value::Object(arguments) }),
    );
    let response = match call(&state_file, &message) {
        Ok(response) => response,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };

    if let Some(error) = response.get("error") {
        eprintln!("{} 被协议层拒了：{error}", action.name);
        return 2;
    }
    let result = &response["result"];
    let structured = &result["structuredContent"];

    // 领域失败（`isError`）：模型看得见、改得动的那一类（没授权、设备不支持、token 过期…）。
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        if json_out {
            println!("{structured}");
        } else {
            let error = &structured["error"];
            eprintln!("{} 领域失败：{}", action.name, domain_message(error));
        }
        return 1;
    }

    if json_out {
        println!("{structured}");
    } else {
        println!("{}", summary(action.id, structured));
    }
    0
}

/// 打印用法错误（一行中文原因 + 该子命令的用法）。
fn usage_error(action: &Action, reason: &str) -> u8 {
    eprint!("{reason}\n\n{}", action_usage(action));
    3
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::String => "字符串",
        Kind::Integer => "整数",
        Kind::Boolean => "布尔",
        Kind::Object => "JSON 对象",
    }
}

/// `--k <原文>` → 线上那一格的值。
///
/// 只有一条规矩：**能按 schema 的类型解析就解析，解析不了就把原文送上去**。
///
/// - `integer`：是整数就送整数（`7000` → `7000`），不是就送字符串（服务端按 schema 回 `-32602`）；
/// - `object`：能解析成 JSON 就送解析后的值；`@file.json` 先读文件（**读不到 = 用法错误**：
///   那是"连参数都形不成"）；文件里不是 JSON 就送那串文本；
/// - 其余一律送字符串。
///
/// 这条规矩是为了让"值的对错"只有**一个**判定者：服务端（`handlers::parse_call` 按同一份
/// `inputSchema` 手写校验）。CLI 自己判成用法错误会让验收 §4-20（`--endpoint nope` → 退出码 2）
/// 落空。
fn value_for(field: &Field, raw: &str) -> Result<Value, String> {
    let text = match raw.strip_prefix('@') {
        Some(path) if field.kind == Kind::Object => std::fs::read_to_string(path)
            .map_err(|error| format!("{} 读不到 {}：{error}", field.flag, path))?,
        _ => raw.to_string(),
    };

    Ok(match field.kind {
        Kind::Integer => text
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(text)),
        Kind::Object => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Kind::String | Kind::Boolean => Value::String(text),
    })
}

/// 一条请求（三个出口共用同一份 `_meta` 形状）。
fn request(id: i64, method: &str, params: Value) -> Value {
    let mut params = params.as_object().cloned().unwrap_or_default();
    params.insert("_meta".to_string(), client_meta());
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// 每请求 `_meta`（规范要求必带 `protocolVersion` 与 `clientCapabilities`）。
fn client_meta() -> Value {
    json!({
        meta::key::PROTOCOL_VERSION: meta::PROTOCOL_VERSION,
        meta::key::CLIENT_INFO: { "name": "voxctl", "version": env!("CARGO_PKG_VERSION") },
        meta::key::CLIENT_CAPABILITIES: {},
    })
}

/// 打给**跑着的**本机控制面，取回一条 JSON-RPC 响应。失败一律一句中文原因（调用方打 stderr）。
fn call(state_file: &Path, message: &Value) -> Result<Value, String> {
    let plane = ControlPlane::from_state_file(state_file).map_err(|error| {
        format!("{error}（控制面在跑吗？--state-file 指向它写的那个 control.json）")
    })?;

    match plane.post(message) {
        Ok(Reply::Message(response)) => Ok(response),
        // `tools/call` 的回应只可能是这一格；另外三种是"我们跟上游说的不是同一件事"，如实报。
        Ok(Reply::Accepted) => Err(format!(
            "控制面把 tools/call 当成了通知（202 无 body）：{}",
            plane.addr()
        )),
        Ok(Reply::Stream(_)) => Err(format!(
            "控制面回了一条长流，而 tools/call 该回一条响应：{}",
            plane.addr()
        )),
        Ok(Reply::Unexpected { status, body }) => Err(format!(
            "控制面回了看不懂的响应（HTTP {status}）：{}",
            body.trim()
        )),
        Err(error) => Err(format!("连不上本机控制面（{}）：{error}", plane.addr())),
    }
}

/// 领域失败的一行中文：`code —— message（detail.hint）`。
fn domain_message(error: &Value) -> String {
    let code = error["code"].as_str().unwrap_or("?");
    let message = error["message"].as_str().unwrap_or("");
    match error["detail"]["hint"].as_str() {
        Some(hint) => format!("{code} —— {message}（{hint}）"),
        None => format!("{code} —— {message}"),
    }
}

/// 一行中文摘要（缺省输出）。`--json` 想看的就是 `structuredContent` 本身。
fn summary(action: ActionId, structured: &Value) -> String {
    match action {
        ActionId::ListEndpoints => {
            let endpoints = structured["endpoints"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|endpoint| {
                            let state = if endpoint["available"].as_bool() == Some(true) {
                                if endpoint["running"].as_bool() == Some(true) {
                                    "可用·在跑"
                                } else {
                                    "可用·未跑"
                                }
                            } else {
                                "不可用"
                            };
                            format!("{}（{}）", endpoint["id"].as_str().unwrap_or("?"), state)
                        })
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_default();
            format!(
                "{} 个端点：{}；本机档位 {}；控制面通道 {}",
                structured["endpoints"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(0),
                endpoints,
                structured["device"]["tier"].as_str().unwrap_or("?"),
                strings(&structured["device"]["control"]).join("+"),
            )
        }
        ActionId::DescribeEndpoint => format!(
            "{}（{}）：{}·{}；本机档位 {}；可改 {} 格",
            structured["endpoint"].as_str().unwrap_or("?"),
            structured["title"].as_str().unwrap_or("?"),
            if structured["available"].as_bool() == Some(true) {
                "可用"
            } else {
                "不可用"
            },
            if structured["running"].as_bool() == Some(true) {
                "在跑"
            } else {
                "未跑"
            },
            structured["capabilities"]["tier"].as_str().unwrap_or("?"),
            structured["editable"].as_array().map(Vec::len).unwrap_or(0),
        ),
        ActionId::ComposeEndpoint => {
            let changed = structured["changed"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|change| {
                            format!(
                                "{}: {} → {}",
                                change["path"].as_str().unwrap_or("?"),
                                change["from"],
                                change["to"]
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .unwrap_or_default();
            if structured["applied"].as_bool() == Some(true) {
                format!(
                    "{} 已落进账本：改了 {} 格（{}）",
                    structured["endpoint"].as_str().unwrap_or("?"),
                    structured["changed"].as_array().map(Vec::len).unwrap_or(0),
                    changed
                )
            } else {
                format!(
                    "{} dry-run：改了 {} 格（{}）；token {}（{} ms 内有效，apply 时带回来）",
                    structured["endpoint"].as_str().unwrap_or("?"),
                    structured["changed"].as_array().map(Vec::len).unwrap_or(0),
                    changed,
                    structured["token"].as_str().unwrap_or("?"),
                    structured["expires_in_ms"].as_u64().unwrap_or(0),
                )
            }
        }
        ActionId::SessionOpen => format!(
            "{} 已开：{}（state={}，等了 {} ms）；字幕 {}",
            structured["endpoint"].as_str().unwrap_or("?"),
            structured["session"].as_str().unwrap_or("?"),
            structured["state"].as_str().unwrap_or("?"),
            structured["wait_ms"].as_u64().unwrap_or(0),
            structured["transcript"].as_str().unwrap_or("?"),
        ),
        ActionId::SessionClose => format!(
            "{} 已关（stopped={}）",
            structured["session"].as_str().unwrap_or("?"),
            structured["stopped"],
        ),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    Value::Object(object) => object.get("name")?.as_str().map(str::to_string),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

// --- serve / serve-stdio ------------------------------------------------------

/// `voxctl serve`：起本机控制面 HTTP。
///
/// **不注入后端**（`backend = None`）：账本与设备只在装配层注入，CLI 起
/// 这份只是协议面的第二个人口——`server/discover` / `tools/list` 直接可用，`tools/call` 会
/// 如实回 `-32603`。这样"CLI 能起服务"与"CLI 不拥有账本"两条同时成立。
///
/// 只绑 `127.0.0.1`：连参数面都不给地址，只给端口（`0` = 系统分配）。
fn serve(args: &[String]) -> u8 {
    let mut state_file: Option<PathBuf> = None;
    let mut port: u16 = 0;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            "--state-file" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    eprint!("--state-file 后面要跟路径。\n\n{}", usage());
                    return 3;
                };
                state_file = Some(PathBuf::from(path));
            }
            "--port" => {
                index += 1;
                match args.get(index).map(|text| text.parse::<u16>()) {
                    Some(Ok(value)) => port = value,
                    _ => {
                        eprint!("--port 后面要跟 0-65535 的端口。\n\n{}", usage());
                        return 3;
                    }
                }
            }
            other => {
                eprint!("serve 不认识的参数：{other}\n\n{}", usage());
                return 3;
            }
        }
        index += 1;
    }

    // 握手文件路径**不给默认值**：它由装配层的 `app_config_dir` 决定（Tauri 的标识符说了算，
    // 设计稿 §5.1 第 5 条把它列为未核实项），CLI 不许自己猜一个目录写 token。
    let Some(state_file) = state_file else {
        eprint!(
            "serve 要 --state-file <path>（握手文件位置）。\n\n{}",
            usage()
        );
        return 3;
    };

    let options = ServerOptions::new(state_file.clone())
        .bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
    let handle = match http::serve(options, None) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("起控制面失败：{error}");
            return 2;
        }
    };

    eprintln!(
        "控制面已监听 http://{}{}（token 与 pid 写在 {}）",
        handle.addr(),
        PATH,
        state_file.display()
    );
    eprintln!("未注入后端：tools/call 回 -32603；server/discover 与 tools/list 可直接调。");
    park_forever()
}

/// `voxctl serve-stdio`：stdio ⇄ 本机 HTTP 的桥（**不是**第二个服务端）。
///
/// 读握手文件拿地址与凭据（跟动作子命令同一条瘦客户端路径），之后由
/// [`vox_mcp::transport::stdio::serve`] 转发：stdin 一行一条 JSON-RPC → 一条 POST → 响应/事件
/// 写回 stdout。stdin 关了就是收工（宿主收摊）。
fn serve_stdio(args: &[String]) -> u8 {
    let mut state_file: Option<PathBuf> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            "--state-file" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    eprint!("--state-file 后面要跟路径。\n\n{}", usage());
                    return 3;
                };
                state_file = Some(PathBuf::from(path));
            }
            other => {
                eprint!("serve-stdio 不认识的参数：{other}\n\n{}", usage());
                return 3;
            }
        }
        index += 1;
    }

    let Some(state_file) = state_file else {
        eprint!(
            "serve-stdio 要 --state-file <path>（握手文件位置）。\n\n{}",
            usage()
        );
        return 3;
    };

    match ControlPlane::from_state_file(&state_file) {
        Ok(plane) => vox_mcp::transport::stdio::serve(plane),
        Err(error) => {
            eprintln!("{error}（控制面在跑吗？--state-file 指向它写的那个 control.json）");
            2
        }
    }
}

/// 停在这里等 Ctrl-C。**刻意不写信号处理**：那要额外依赖（本 crate 不许为新包买单），
/// 而 SIGINT 的默认处置已经就是终止进程。代价是握手文件会留在原处（下次启动覆盖它，
/// 里面的 `pid` 也能让人判断它是不是活的）。
///
/// `park` 会被虚假唤醒打断，所以套一层循环。
fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

// --- --probe（离线调试）--------------------------------------------------------

/// `voxctl --probe <方法> [--json]`：在本进程内跑一遍协议层。
///
/// 走的是与 HTTP 面**同一个** `handle`，所以输出逐字节一致（`tests/http.rs` 钉着）。
/// `backend` 传 `None`：这两条方法不需要账本（要账本的是 `tools/call`）。
fn probe(args: &[String]) -> u8 {
    let mut method: Option<&str> = None;
    let mut json_out = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            "--json" => json_out = true,
            "--probe" => {
                index += 1;
                let Some(name) = args.get(index) else {
                    eprint!("--probe 后面要跟方法名。\n\n{}", usage());
                    return 3;
                };
                method = Some(name);
            }
            other => {
                eprint!("不认识的参数：{other}\n\n{}", usage());
                return 3;
            }
        }
        index += 1;
    }

    let Some(method) = method else {
        eprint!("{}", usage());
        return 3;
    };

    if method != mcp::METHOD_DISCOVER && method != mcp::METHOD_TOOLS_LIST {
        eprintln!(
            "--probe 只支持 {} 与 {}（要账本的方法走动作子命令 / HTTP 面）。",
            mcp::METHOD_DISCOVER,
            mcp::METHOD_TOOLS_LIST
        );
        return 3;
    }

    let message = request(1, method, json!({}));
    let response = vox_mcp::handle(&message, None)
        .response()
        .unwrap_or_else(|| {
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32603, "message": "协议层没有回响应（通知与长流不该出现在这里）" },
            })
        });

    if json_out {
        println!("{response}");
    } else if let Some(error) = response.get("error") {
        eprintln!("{method} 失败：{error}");
        return 2;
    } else {
        println!("{}", probe_summary(method, &response["result"]));
    }

    // 协议层报错（`-32601` / `-32602` / `-32022`…）→ 退出码 2。
    u8::from(response.get("error").is_some()) * 2
}

/// 一行中文摘要（缺省输出）。`--json` 想看的原始形状就是 `result`。
fn probe_summary(method: &str, result: &Value) -> String {
    match method {
        mcp::METHOD_DISCOVER => {
            let versions = strings(&result["supportedVersions"]).join(", ");
            let capabilities: Vec<&str> = result["capabilities"]
                .as_object()
                .map(|map| map.keys().map(String::as_str).collect())
                .unwrap_or_default();
            let info = &result["_meta"][meta::key::SERVER_INFO];
            format!(
                "{} {} · 协议 {} · 能力 {} · 缓存 {}/{}",
                info["name"].as_str().unwrap_or("?"),
                info["version"].as_str().unwrap_or("?"),
                versions,
                capabilities.join("+"),
                result["ttlMs"].as_u64().unwrap_or(0),
                result["cacheScope"].as_str().unwrap_or("?"),
            )
        }
        mcp::METHOD_TOOLS_LIST => {
            let names = strings(&result["tools"]);
            format!("{} 个工具：{}", names.len(), names.join(", "))
        }
        other => format!("{other} 的 result：{result}"),
    }
}
