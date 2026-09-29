//! MCP 协议面：方法分派 + 每请求 `_meta` 校验 + 缓存定值。
//!
//! 本模块认方法名、认 `_meta`、认缓存提示；**不认账本**——需要账本的动作经
//! [`crate::handlers`] 转发给注入的后端。
//!
/// 方法实现范围（2026-07-28）：
///
/// | 方法 | 状态 |
/// | --- | --- |
/// | `server/discover` | 实现（规范 MUST） |
/// | `tools/list` | 实现（从 [`crate::actions::ACTIONS`] 生成） |
/// | `tools/call` | 实现分派；能否成功取决于是否注入了后端 |
/// | `resources/list` / `resources/read` | 实现（字幕资源，`vox://session/<handle>/transcript`） |
/// | `subscriptions/listen` | 实现：**不是一条响应，是一条长流**（见 [`Answer::Stream`]） |
/// | `initialize` | 不实现（本版已删握手），按规范 SHOULD 在错误里列出支持的版本 |
/// | `prompts/*`、`resources/templates/list`、`resources/subscribe` | 不实现、不广告 |
///
/// 不实现的东西**不广告**：`capabilities` 里只有 `tools` 与 `resources`；`tools` 不广告
/// `listChanged`（5 个工具编译期恒定），`resources` 的两位都真会发（见 [`capabilities`]）；
/// 没有 `extensions`（v1 不实现 Tasks）。
pub mod cache;
pub mod meta;
pub mod resources;
pub mod subscriptions;

use serde_json::{json, Map, Value};

use crate::actions::{Action, ACTIONS};
use crate::handlers::{self, CallFailure, ControlBackend};
use crate::jsonrpc::{self, code, ErrorObject, Incoming, Request};

/// `server/discover`。规范 MUST 实现：客户端拿它一次问清版本、能力与身份。
pub const METHOD_DISCOVER: &str = "server/discover";
pub const METHOD_TOOLS_LIST: &str = "tools/list";
pub const METHOD_TOOLS_CALL: &str = "tools/call";
pub const METHOD_RESOURCES_LIST: &str = "resources/list";
pub const METHOD_RESOURCES_READ: &str = "resources/read";
pub const METHOD_SUBSCRIPTIONS_LISTEN: &str = "subscriptions/listen";

/// 老式握手的方法名。2026-07-28 删了 `initialize` 与 `notifications/initialized`。
const LEGACY_INITIALIZE: &str = "initialize";

/// 服务端身份（结果 `_meta` 里的 `io.modelcontextprotocol/serverInfo`）。
///
/// 名字**不加工厂前缀**：规范没有前缀要求，工具名也一样不带 `vox_`；规范还明确说
/// `serverInfo` 不保证唯一、别拿它做去歧义。
pub const SERVER_NAME: &str = "vox";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `server/discover` 的 `instructions`：给模型的自然语言引导（设计稿 §2.3.3 逐字）。
const INSTRUCTIONS: &str = "Vox 本机控制面。典型流程：list_endpoints → describe_endpoint → compose_endpoint（先 dry-run 再 apply）→ session_open → 订阅 vox://session/<handle>/transcript 读字幕 → session_close。实时音频不走本协议。";

/// 协议层对一条消息的处理结果。
///
/// 为什么不是 `Option<Value>`：`subscriptions/listen` 的"回答"**不是一条消息**，而是一条长流
/// （规范：它的响应流一直开着，直到客户端或服务端收流）。把这件事写进类型里，传输面就不可能
/// 忘掉它——不处理 [`Answer::Stream`] 编译不过。
#[derive(Debug)]
pub enum Answer {
    /// 通知：按规范**不回响应**（接收方 MUST NOT 回通知）。
    Silence,
    /// 一条响应（成功或错误）。
    Response(Value),
    /// `subscriptions/listen` 被接受：传输面负责开流，先写
    /// [`Subscription::acknowledged`](subscriptions::Subscription::acknowledged)，之后按 tick 推
    /// [`Subscription::updates`](subscriptions::Subscription::updates)。
    Stream(subscriptions::Subscription),
}

impl Answer {
    /// 取出响应：通知与长流都**不是**响应（长流由传输面自己开）。
    pub fn response(self) -> Option<Value> {
        match self {
            Answer::Response(response) => Some(response),
            Answer::Silence | Answer::Stream(_) => None,
        }
    }
}

/// 处理一条 JSON-RPC 消息。三个出口（HTTP / stdio / `voxctl --probe`）共用这一个函数，
/// 所以"CLI 的输出 == HTTP 面的输出"是同一个函数出来的同一串字节。
///
/// `backend` 是唯一能碰账本/设备的入口。`server/discover`、`tools/list` 不需要它，
/// 所以传 `None` 是合法的（`voxctl` 的本地探测就这么用）；`tools/call` 与资源面传 `None` 会得到
/// `-32603`——那不是"假成功"，是"后端没接上"的真实状态。
pub fn handle(message: &Value, backend: Option<&mut dyn ControlBackend>) -> Answer {
    let request = match jsonrpc::parse(message) {
        Ok(Incoming::Request(request)) => request,
        // 通知一律不回。本轮没有需要处理的客户端通知（HTTP 上关流才是取消，
        // `notifications/cancelled` 只走 stdio），收到就丢掉；不认识的 notify 也不报错。
        Ok(Incoming::Notification { .. }) => return Answer::Silence,
        Err(error) => return Answer::Response(jsonrpc::error_result(None, &error)),
    };

    // 老式握手先答：老客户端根本没有 `_meta`，让它掉进 `-32602` 会掩盖真正的原因。
    // 规范：只支持现代版本的服务端 SHOULD 在给 `initialize` 的错误里列出自己支持的版本。
    if request.method == LEGACY_INITIALIZE {
        return Answer::Response(jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::new(
                code::METHOD_NOT_FOUND,
                format!(
                    "initialize 已被 2026-07-28 移除（本版协议无状态、无握手）；本服务端只支持 {}",
                    meta::PROTOCOL_VERSION
                ),
            ),
        ));
    }

    // 每请求的 `_meta`：缺必填 → `-32602`；版本不支持 → `-32022`。
    if let Err(error) = meta::validate(request.params.as_ref()) {
        return Answer::Response(jsonrpc::error_result(Some(&request.id), &error));
    }

    Answer::Response(match request.method.as_str() {
        METHOD_DISCOVER => respond(&request.id, discover_result()),
        METHOD_TOOLS_LIST => respond(&request.id, tools_list_result()),
        METHOD_TOOLS_CALL => tools_call(&request, backend),
        METHOD_RESOURCES_LIST => resources::list_call(&request, backend),
        METHOD_RESOURCES_READ => resources::read_call(&request, backend),
        // 唯一一条"回答不是响应"的方法：交给传输面开流。
        METHOD_SUBSCRIPTIONS_LISTEN => return listen(&request, backend),
        other => jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::new(code::METHOD_NOT_FOUND, format!("未知方法：{other}")),
        ),
    })
}

/// `subscriptions/listen`：接受就回一条流，不接受就回一条普通错误响应。
///
/// 两件"服务端现在推不了"的事一律回**一条普通错误**，不是一条永远不响的流：
///
/// - **没有后端**：ticker 没有账本可读，那条流会永远一条通知都不发——那是**假订阅**；
/// - **总闸关着**（`Settings.control.enabled == false`）：传输面的 ticker 下一个 tick 就会把
///   在册的流全收掉（[`ControlBackend::control_enabled`]），所以这里接受的流会**立刻**被收回，
///   客户端只会陷入"连上就被踢"的重连循环。拒绝比那个诚实。
///
/// 两者都用 `-32603`：请求本身没问题（不是 `-32602`），是服务端现在处在"推不了"的状态。
fn listen(request: &Request, backend: Option<&mut dyn ControlBackend>) -> Answer {
    let Some(backend) = backend else {
        return Answer::Response(jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::new(
                code::INTERNAL_ERROR,
                "控制面后端未接入：没有账本就没有资源变更可推，订阅会被接受但永远不响",
            ),
        ));
    };
    if !backend.control_enabled() {
        return Answer::Response(jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::new(
                code::INTERNAL_ERROR,
                "控制面总开关（Settings.control.enabled）关着：不接受新的订阅流",
            ),
        ));
    }
    // 水位在**回 ack 之前**对齐到"这一刻"的账本：客户端一收到 ack 就可能说话/关会话，
    // 那些变化必须落在基线**之后**才不会被吞掉（原因见 `subscriptions::accept` 的注释——
    // 拖到 ticker 第一拍才对齐，会丢掉这一窗口里的变化，高负载下曾偶发）。
    let baseline = backend.poll_resources();
    match subscriptions::accept(request, &baseline) {
        Ok(subscription) => Answer::Stream(subscription),
        Err(error) => Answer::Response(jsonrpc::error_result(Some(&request.id), &error)),
    }
}

/// `server/discover` 的结果（设计稿 §2.3.3 逐字）。
fn discover_result() -> Map<String, Value> {
    let mut result = Map::new();
    result.insert(
        "supportedVersions".to_string(),
        json!(meta::SUPPORTED_VERSIONS),
    );
    result.insert("capabilities".to_string(), capabilities());
    result.insert(
        "instructions".to_string(),
        Value::String(INSTRUCTIONS.to_string()),
    );
    cache::DISCOVER.apply(&mut result);
    result
}

/// 能力广告：**只广告真能应答的东西**（本项目硬规矩：位必须是事实，`DIRECTIONS.md` §10.5-2）。
///
/// - `tools`：有 `tools/list` 与 `tools/call` 两条应答，所以广告它；但**不广告 `listChanged`**——
///   5 个工具编译期恒定，而 `listChanged` 是"会发通知"的承诺，广告了却永不发就是撒谎。
///   S1.1 做目录驱动的工具时再打开（那时得真的发）。
/// - `resources`：**两位都是事实**（本轮资源面落地时才加上来的）——
///   `subscribe: true` = 会话里字幕一变就发 `notifications/resources/updated`（`transcript.rs`
///   的检测器 + 传输面的 ticker）；`listChanged: true` = 会话开/关就发
///   `notifications/resources/list_changed`（同一个 ticker 比 `list_revision`）。
///   两位都不带"以后再说"的意思：`crates/vox-mcp/tests/resources.rs` 里有一条用例把每个广告出去的位
///   和它承诺的那条通知钉在一起（"位 = 事实"）。
/// - **没有 `extensions`**：v1 不实现 Tasks 扩展，也就不用它（规范：只有客户端声明过才允许返回 task）。
fn capabilities() -> Value {
    json!({
        "tools": {},
        "resources": { "listChanged": true, "subscribe": true },
    })
}

/// `tools/list` 的结果：顺序 = [`ACTIONS`] 顺序（规范 SHOULD 确定顺序：客户端靠它缓存，
/// 也靠它吃 prompt 缓存），逐条带 `ttlMs` / `cacheScope`。
fn tools_list_result() -> Map<String, Value> {
    let mut result = Map::new();
    result.insert(
        "tools".to_string(),
        Value::Array(ACTIONS.iter().map(tool_definition).collect()),
    );
    cache::TOOLS_LIST.apply(&mut result);
    result
}

/// 一条 `tools/list` 条目。两份 schema 是**文本常量原样解析**——同一份常量既喂 MCP 也喂 CLI，
/// 不写第二份（解析失败是常量写错，`tests/protocol.rs` 会先抓到）。
fn tool_definition(action: &Action) -> Value {
    json!({
        "name": action.name,
        "title": action.title,
        "description": action.description,
        "inputSchema": schema(action.input_schema),
        "outputSchema": schema(action.output_schema),
        "annotations": action.annotations(),
    })
}

fn schema(text: &'static str) -> Value {
    serde_json::from_str(text).expect("schema 是编译期常量，必须是合法 JSON")
}

/// `tools/call`。
///
/// 分工（§2.1.2，按规范"模型可控"的口径分）：
///
/// - 参数的**结构**不合法（未知工具、缺必填、枚举越界、未知键、清单过不了
///   `Composition::validate()`）→ **JSON-RPC `-32602`**（清单结构性错误额外带 `data.errors`）；
/// - 参数合法但**世界不允许**（没授权、设备不支持、token 过期…）→ `isError:true` +
///   `structuredContent.error`，让模型看得见、改得动。
fn tools_call(request: &Request, backend: Option<&mut dyn ControlBackend>) -> Value {
    let Some(params) = request.params.as_ref().and_then(Value::as_object) else {
        return invalid_params(&request.id, "tools/call 的 params 必须是对象", json!({}));
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return invalid_params(&request.id, "tools/call 必须带 name", json!({}));
    };

    let call = match handlers::parse_call(name, params.get("arguments")) {
        Ok(call) => call,
        Err(error) => return jsonrpc::error_result(Some(&request.id), &error),
    };

    let Some(backend) = backend else {
        return jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::new(
                code::INTERNAL_ERROR,
                "控制面后端未接入：vox-mcp 只提供协议面，账本与设备由外壳注入",
            ),
        );
    };

    match handlers::invoke(call, backend) {
        Ok(structured) => tool_result(&request.id, structured),
        // 结构性不合法（清单过不了 `Composition::validate()`）：协议错误，`data.errors` 一次给全部问题。
        Err(CallFailure::InvalidParams { message, errors }) => jsonrpc::error_result(
            Some(&request.id),
            &ErrorObject::with_data(code::INVALID_PARAMS, message, json!({ "errors": errors })),
        ),
        // 世界不允许：工具执行错误，模型看得见也改得动。
        Err(CallFailure::Domain(error)) => tool_error(&request.id, &error),
    }
}

/// 成功的工具结果：`structuredContent` + 一份等价的文本块（规范 SHOULD：老客户端只认
/// `content`，让它也能读到同一串 JSON）。**不带 `isError`**——缺省即成功。
fn tool_result(id: &Value, structured: Value) -> Value {
    let text = structured.to_string();
    let mut result = Map::new();
    result.insert(
        "content".to_string(),
        json!([{ "type": "text", "text": text }]),
    );
    result.insert("structuredContent".to_string(), structured);
    respond(id, result)
}

/// 领域失败的工具结果：`isError:true` + `structuredContent.error`。
///
/// 领域失败**故意不走** JSON-RPC error：规范按"模型可控"设计工具，模型要能看见 `unsupported_field`
/// 这类反馈并改参数（`server/tools` 的 Error Handling）。
fn tool_error(id: &Value, error: &crate::actions::DomainError) -> Value {
    let structured = json!({ "error": error.to_json() });
    let text = structured.to_string();
    let mut result = Map::new();
    result.insert(
        "content".to_string(),
        json!([{ "type": "text", "text": text }]),
    );
    result.insert("structuredContent".to_string(), structured);
    result.insert("isError".to_string(), Value::Bool(true));
    respond(id, result)
}

/// 结果响应的公共包装：补 `resultType`（规范必填）与 `_meta.serverInfo`（规范 SHOULD，
/// 每一条结果都带，服务端身份不依赖任何连接状态）。资源面那两条方法（`mcp/resources.rs`）
/// 用的是同一个包装。
pub(super) fn respond(id: &Value, mut result: Map<String, Value>) -> Value {
    result.insert(
        "_meta".to_string(),
        json!({ meta::key::SERVER_INFO: { "name": SERVER_NAME, "version": SERVER_VERSION } }),
    );
    jsonrpc::complete_result(id, result)
}

fn invalid_params(id: &Value, message: &str, data: Value) -> Value {
    jsonrpc::error_result(
        Some(id),
        &ErrorObject::with_data(code::INVALID_PARAMS, message, data),
    )
}
