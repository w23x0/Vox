//! 协议面用例。
//!
//! 只走公开面（[`vox_mcp::handle`] 与 [`vox_mcp::actions`]），因为这里断言的全是**外部可见**的
//! 契约：错误码、`resultType`、`_meta`、字段齐不齐、schema 与手写校验是否还对得上。错一个错误码，
//! 宿主/模型就会换错补救动作。传输层（HTTP/stdio）、端点投影、字幕资源不在这里——它们还没落。

use serde_json::{json, Map, Value};

use vox_mcp::actions::{
    ActionId, DomainError, DomainErrorCode, EndpointId, Permission, ACTIONS, DEFAULT_WAIT_READY_MS,
    MAX_WAIT_READY_MS,
};
use vox_mcp::handlers::{CallFailure, ControlBackend};
use vox_mcp::jsonrpc::code;
use vox_mcp::mcp;
use vox_mcp::resources::ResourceTick;

const METHOD_NOT_IN_TABLE: &str = "no_such_tool";

fn meta_object() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": mcp::meta::PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

/// 一条带合法 `_meta` 的请求。
fn request(id: i64, method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": { "_meta": meta_object() },
    })
}

/// 一条 `_meta` 可控的请求（`None` = 没有 `_meta` 这一格）。
fn request_with_meta(id: i64, method: &str, meta: Option<Value>) -> Value {
    let params = match meta {
        Some(meta) => json!({ "_meta": meta }),
        None => json!({}),
    };
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// 一条 `tools/call`（`backend` 传 `None`：本轮没有后端实现，见 `handlers::ControlBackend`）。
fn tool_call(id: i64, name: &str, arguments: Value) -> Value {
    let mut params = Map::new();
    params.insert("_meta".to_string(), meta_object());
    params.insert("name".to_string(), json!(name));
    params.insert("arguments".to_string(), arguments);
    let message = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params });
    mcp::handle(&message, None)
        .response()
        .expect("请求必须有响应")
}

fn error_code(response: &Value) -> i32 {
    response["error"]["code"].as_i64().expect("这不是错误响应") as i32
}

fn parse_schema(text: &str) -> Value {
    serde_json::from_str(text).expect("schema 常量必须是合法 JSON")
}

#[test]
fn meta_missing_required_fields_is_invalid_params() {
    // 规范：`protocolVersion` 与 `clientCapabilities` 每请求必带，缺任一 → -32602（HTTP 上还必须是 400）。
    let cases: Vec<(&str, Value)> = vec![
        (
            "连 params 都没有",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        ),
        (
            "params 里没有 _meta",
            request_with_meta(1, "tools/list", None),
        ),
        (
            "_meta 里没有 protocolVersion",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": {
                "io.modelcontextprotocol/clientCapabilities": {} } } }),
        ),
        (
            "_meta 里没有 clientCapabilities",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } }),
        ),
        (
            "protocolVersion 不是字符串",
            request_with_meta(
                1,
                "tools/list",
                Some(json!({
                    "io.modelcontextprotocol/protocolVersion": 20260728,
                    "io.modelcontextprotocol/clientCapabilities": {},
                })),
            ),
        ),
        (
            "clientCapabilities 不是对象",
            request_with_meta(
                1,
                "tools/list",
                Some(json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": [],
                })),
            ),
        ),
    ];

    for (what, message) in cases {
        let response = mcp::handle(&message, None)
            .response()
            .expect("请求必须有响应");
        assert_eq!(error_code(&response), code::INVALID_PARAMS, "{what}");
        assert_eq!(response["id"], json!(1), "{what}：错误响应要带原 id");
    }

    // 缺的那一格要写进 message，宿主/模型才知道补什么。
    let response = mcp::handle(
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } }),
        None,
    )
    .response()
    .expect("请求必须有响应");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message 必须是字符串")
            .contains("clientCapabilities"),
        "错误要说清缺的是哪一格：{response}"
    );
}

#[test]
fn unsupported_protocol_version_lists_what_we_support() {
    let message = request_with_meta(
        7,
        "server/discover",
        Some(json!({
            "io.modelcontextprotocol/protocolVersion": "2025-06-18",
            "io.modelcontextprotocol/clientCapabilities": {},
        })),
    );
    let response = mcp::handle(&message, None)
        .response()
        .expect("请求必须有响应");
    assert_eq!(error_code(&response), code::UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(
        response["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );
    assert_eq!(response["error"]["data"]["requested"], json!("2025-06-18"));
}

#[test]
fn tools_list_is_generated_from_the_action_table() {
    let response = mcp::handle(&request(2, "tools/list"), None)
        .response()
        .expect("请求必须有响应");
    let result = &response["result"];

    assert_eq!(
        result["resultType"],
        json!("complete"),
        "所有结果都要有 resultType"
    );
    assert_eq!(result["ttlMs"], json!(3_600_000));
    assert_eq!(result["cacheScope"], json!("public"));
    assert!(result.get("nextCursor").is_none(), "v1 不分页，不许发游标");

    let tools = result["tools"].as_array().expect("tools 必须是数组");
    assert_eq!(
        tools.len(),
        ACTIONS.len(),
        "tools/list 必须与 actions::ACTIONS 一一对应"
    );
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ACTIONS.iter().map(|action| action.name).collect::<Vec<_>>(),
        "名字与顺序都要跟表一致（顺序影响客户端缓存与 prompt 缓存）"
    );

    for (tool, action) in tools.iter().zip(ACTIONS) {
        assert_eq!(tool["title"], json!(action.title));
        assert_eq!(tool["description"], json!(action.description));
        assert!(
            tool["inputSchema"].is_object(),
            "{} 的 inputSchema 必须是对象",
            action.name
        );
        assert!(
            tool["outputSchema"].is_object(),
            "{} 的 outputSchema 必须是对象",
            action.name
        );
        let annotations = &tool["annotations"];
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(
                annotations[hint].is_boolean(),
                "{} 的 annotations.{hint} 必须是布尔",
                action.name
            );
        }
        assert_eq!(annotations["readOnlyHint"], json!(action.read_only));
        assert_eq!(annotations["idempotentHint"], json!(action.idempotent));
        assert_eq!(
            annotations["destructiveHint"],
            json!(action.writes_config),
            "{}：只有改用户配置才算破坏性更新（起停设备不是）",
            action.name
        );
    }

    // 身份：规范 SHOULD 在每条结果的 `_meta` 里自报家门，而且别拿它做安全判断。
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        json!("vox")
    );
}

#[test]
fn tool_names_are_spec_conformant_and_cli_commands_are_kebab() {
    for action in ACTIONS {
        let name = action.name;
        assert!(!name.is_empty() && name.len() <= 128, "工具名长度：{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
            "工具名只允许 [A-Za-z0-9_.-]：{name}"
        );
        assert_eq!(
            action.cli_command(),
            name.replace('_', "-"),
            "CLI 子命令名 = 工具名的 kebab-case"
        );
    }

    let mut names: Vec<&str> = ACTIONS.iter().map(|action| action.name).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "工具名必须唯一");
}

/// 穷尽 `match` 只证"每个 `ActionId` 都有 handler"，**证不了**表里 id ↔ 工具名配对没错
/// （把某一行的 `id` 换成别的变体照样编译、12/12 照样绿）。这条就是那道闸门：
/// `ACTIONS` 的 id 排序后必须恰好等于 `ActionId::ALL`——覆盖全部、不多不少、不重复。
#[test]
fn action_table_ids_are_unique_and_match_every_action() {
    let mut ids: Vec<ActionId> = ACTIONS.iter().map(|action| action.id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids.as_slice(),
        ActionId::ALL,
        "ACTIONS 的 id 集合必须恰好是 ActionId::ALL（顺序按 Ord，即声明顺序）"
    );

    // 名字与 id 的顺序也必须一一对应：tools/list 的顺序就是表的顺序。
    for (action, id) in ACTIONS.iter().zip(ActionId::ALL) {
        assert_eq!(
            action.id, *id,
            "{} 这一行的 id 与它在表里的位置不一致",
            action.name
        );
    }
}

#[test]
fn discover_carries_versions_capabilities_and_identity() {
    let response = mcp::handle(&request(1, "server/discover"), None)
        .response()
        .expect("请求必须有响应");
    let result = &response["result"];

    assert_eq!(result["resultType"], json!("complete"));
    assert_eq!(result["supportedVersions"], json!(["2026-07-28"]));
    // 只广告真能应答的：`tools` 有 tools/list 与 tools/call 两条方法（但**不广告 listChanged**——
    // 5 个工具编译期恒定，广告了却永不发就是撒谎）；`resources` 两位都是事实（字幕变更 +
    // 会话开/关都真会发通知，`tests/resources.rs` 里有一条用例把位与通知钉在一起）。
    assert_eq!(
        result["capabilities"],
        json!({
            "tools": {},
            "resources": { "listChanged": true, "subscribe": true },
        })
    );
    assert!(
        result["capabilities"]["tools"].get("listChanged").is_none(),
        "5 个工具恒定，不广告 tools.listChanged：{result}"
    );
    assert!(
        result["capabilities"].get("prompts").is_none(),
        "v1 没有 prompts，就不广告它"
    );
    assert!(
        result["capabilities"].get("extensions").is_none(),
        "v1 不实现 Tasks 扩展，就不广告它"
    );
    assert!(
        result["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("list_endpoints")),
        "instructions 要能引导模型按顺序调工具"
    );
    assert_eq!(result["ttlMs"], json!(3_600_000));
    assert_eq!(result["cacheScope"], json!("public"));
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"],
        json!({ "name": "vox", "version": env!("CARGO_PKG_VERSION") })
    );
}

#[test]
fn unknown_method_and_legacy_initialize_are_method_not_found() {
    let response = mcp::handle(&request(3, "no/such/method"), None)
        .response()
        .expect("请求必须有响应");
    assert_eq!(error_code(&response), code::METHOD_NOT_FOUND);

    // 老式握手在本版被删了；规范：只支持现代版本的服务端 SHOULD 在错误里列出自己支持的版本。
    let response = mcp::handle(
        &json!({ "jsonrpc": "2.0", "id": 4, "method": "initialize", "params": {} }),
        None,
    )
    .response()
    .expect("请求必须有响应");
    assert_eq!(error_code(&response), code::METHOD_NOT_FOUND);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message 必须是字符串")
            .contains("2026-07-28"),
        "要告诉老客户端该换哪个版本：{response}"
    );
}

#[test]
fn notifications_get_no_response() {
    // 规范：通知没有 id，接收方 MUST NOT 回响应。
    let message = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {} });
    assert!(mcp::handle(&message, None).response().is_none());
}

#[test]
fn tool_call_argument_errors_are_invalid_params() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "未知工具",
            json!({ "name": METHOD_NOT_IN_TABLE, "arguments": {} }),
        ),
        (
            "endpoint 不在 enum 里",
            json!({ "name": "describe_endpoint", "arguments": { "endpoint": "nope" } }),
        ),
        (
            "缺必填 endpoint",
            json!({ "name": "describe_endpoint", "arguments": {} }),
        ),
        (
            "缺必填 session",
            json!({ "name": "session_close", "arguments": {} }),
        ),
        (
            "wait_ready_ms 超过 schema 上限",
            json!({ "name": "session_open", "arguments": { "endpoint": "speak", "wait_ready_ms": MAX_WAIT_READY_MS + 1 } }),
        ),
        (
            "apply=true 却不带 token",
            json!({ "name": "compose_endpoint", "arguments": { "endpoint": "speak", "composition": {}, "apply": true } }),
        ),
        (
            "composition 不是对象",
            json!({ "name": "compose_endpoint", "arguments": { "endpoint": "speak", "composition": [], "apply": false } }),
        ),
        (
            "未知键",
            json!({ "name": "session_close", "arguments": { "session": "s_1", "force": true } }),
        ),
        (
            "arguments 不是对象",
            json!({ "name": "session_close", "arguments": "s_1" }),
        ),
        (
            "无参数的工具被传了参数",
            json!({ "name": "list_endpoints", "arguments": { "endpoint": "speak" } }),
        ),
    ];

    for (what, arguments) in cases {
        let name = arguments["name"].as_str().expect("用例都带 name");
        let response = tool_call(9, name, arguments["arguments"].clone());
        assert_eq!(
            error_code(&response),
            code::INVALID_PARAMS,
            "{what}（{name}）"
        );
    }

    // 参数合法时不走 -32602：`session_open`（在 schema 上限之内）与无参数的 `list_endpoints`。
    // 本轮没有注入后端，所以它们会以 `-32603` 告终——这是**后端缺失**的真实状态，不是假成功；
    // 后端接上后这里会变成正常结果，那时 `!= INVALID_PARAMS` 依然成立。
    let response = tool_call(
        10,
        "session_open",
        json!({ "endpoint": "listen", "wait_ready_ms": MAX_WAIT_READY_MS }),
    );
    assert_ne!(
        error_code(&response),
        code::INVALID_PARAMS,
        "上限本身要合法"
    );
    let response = tool_call(11, "list_endpoints", json!({}));
    assert_ne!(error_code(&response), code::INVALID_PARAMS);
}

#[test]
fn schemas_and_handwritten_validation_agree() {
    let action = |name: &str| {
        ACTIONS
            .iter()
            .find(|action| action.name == name)
            .expect("表里有这个动作")
    };

    for action in ACTIONS {
        let input = parse_schema(action.input_schema);
        assert_eq!(
            input["type"],
            json!("object"),
            "{} 的入参是对象",
            action.name
        );
        assert_eq!(
            input["additionalProperties"],
            json!(false),
            "{} 的入参不许有额外键（未知键 → -32602）",
            action.name
        );
        let output = parse_schema(action.output_schema);
        assert!(
            output.is_object(),
            "{} 的输出 schema 必须是对象",
            action.name
        );
        // 失败结果的形状（`isError` 时只有 error 一格）必须在每份输出 schema 里成立。
        assert!(
            output["properties"]["error"]["$ref"].is_string(),
            "{} 的输出 schema 要允许 isError 形状",
            action.name
        );
        assert_eq!(
            output["$defs"]["error"]["required"],
            json!(["code", "message"]),
            "{} 的 error 定义",
            action.name
        );
    }

    // 必填集合：schema 文本里的 `required` 与 `handlers.rs` 手写校验的结构体必须一一对应
    // （删掉某一格 `required` 而校验仍在要求它 = "文档说可选、服务端拒收"）。
    let required: &[(&str, &[&str])] = &[
        ("list_endpoints", &[]),
        ("describe_endpoint", &["endpoint"]),
        ("compose_endpoint", &["endpoint", "composition", "apply"]),
        ("session_open", &["endpoint"]),
        ("session_close", &["session"]),
    ];
    for (name, expected) in required {
        let input = parse_schema(action(name).input_schema);
        let declared: Vec<&str> = input["required"]
            .as_array()
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            declared.as_slice(),
            *expected,
            "{name} 的 required 必须与手写校验的必填集合一致"
        );
    }

    // 端点 enum：schema 文本与 EndpointId 必须一致（写错一个字母 = 客户端发的合法值被服务端拒）。
    let expected: Vec<&str> = EndpointId::ALL
        .iter()
        .map(|endpoint| endpoint.as_str())
        .collect();
    let describe_input = parse_schema(action("describe_endpoint").input_schema);
    assert_eq!(
        describe_input["properties"]["endpoint"]["enum"],
        json!(expected)
    );
    let list_output = parse_schema(action("list_endpoints").output_schema);
    assert_eq!(
        list_output["properties"]["endpoints"]["items"]["properties"]["id"]["enum"],
        json!(expected)
    );
    let compose_input = parse_schema(action("compose_endpoint").input_schema);
    assert_eq!(
        compose_input["properties"]["endpoint"]["enum"],
        json!(expected)
    );
    assert_eq!(
        compose_input["properties"]["composition"]["$ref"],
        json!("#/$defs/composition"),
        "清单子 schema 只许本地 $ref（规范禁止自动解引用网络 $ref）"
    );
    assert_eq!(
        compose_input["then"]["required"],
        json!(["token"]),
        "apply=true 必须带 token"
    );
    assert!(compose_input["$defs"]["composition"].is_object());

    // 授权位的名字：schema 文本里的 `enum` 与 `Permission::as_str()` 必须一致
    // （`describe_endpoint.permissions[].permission` 与 `permission_denied.detail.permission`
    // 都往线上送这个名字，写错一个字母就是坏契约）。
    let describe_output = parse_schema(action("describe_endpoint").output_schema);
    let declared: Vec<&str> = describe_output["properties"]["permissions"]["items"]["properties"]
        ["permission"]["enum"]
        .as_array()
        .expect("enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let ours: Vec<&str> = Permission::ALL
        .iter()
        .map(|permission| permission.as_str())
        .collect();
    assert_eq!(declared, ours, "授权位名字与 schema 的 enum 漂了");

    // wait_ready_ms：schema 的默认值/上下限与手写校验共用同一份常量。
    let open_input = parse_schema(action("session_open").input_schema);
    let wait = &open_input["properties"]["wait_ready_ms"];
    assert_eq!(
        wait["default"].as_u64(),
        Some(u64::from(DEFAULT_WAIT_READY_MS))
    );
    assert_eq!(wait["maximum"].as_u64(), Some(u64::from(MAX_WAIT_READY_MS)));
    assert_eq!(wait["minimum"].as_u64(), Some(0));
}

/// **每一条 `$ref` 都要在本文件里解得出**：规范禁止自动解引用网络 `$ref`
/// （`basic/index#json-schema-usage`），而指到不存在的路径同样是坏 schema——客户端拿到一份
/// 解不开的 schema 就什么也校验不了。清单那一格是生成的，内部带一层 `definitions` 与十几条
/// `$ref`，所以这条从"整体可解"上钉住投影没把引用指到文档外面去。
#[test]
fn every_ref_resolves_inside_its_own_schema() {
    for action in ACTIONS {
        for (which, text) in [
            ("入参", action.input_schema),
            ("输出", action.output_schema),
        ] {
            let schema = parse_schema(text);
            for reference in references(&schema) {
                assert!(
                    reference.starts_with('#'),
                    "{} 的{which} schema 里有非本地的 $ref：{reference}",
                    action.name
                );
                assert!(
                    pointer(&schema, &reference).is_some(),
                    "{} 的{which} schema 里有解不开的 $ref：{reference}",
                    action.name
                );
            }
        }
    }
}

/// 一份 schema 里所有 `$ref` 的取值。
fn references(value: &Value) -> Vec<String> {
    fn walk(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    match (key.as_str(), child.as_str()) {
                        ("$ref", Some(reference)) => found.push(reference.to_string()),
                        _ => walk(child, found),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }

    let mut found = Vec::new();
    walk(value, &mut found);
    found
}

/// JSON Pointer（`#/$defs/composition/definitions/Input` 这种）在本文档里的落点。
fn pointer<'a>(schema: &'a Value, reference: &str) -> Option<&'a Value> {
    let path = reference.strip_prefix('#')?.trim_start_matches('/');
    let mut current = schema;
    for step in path.split('/') {
        if step.is_empty() {
            continue;
        }
        // JSON Pointer 的转义（`~1` = `/`、`~0` = `~`）：先 `~1` 再 `~0`，顺序反了会把 `~01` 解错。
        let step = step.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&step)?,
            Value::Array(items) => items.get(step.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// 清单那一格**是从类型生成的**：`compose_endpoint` 的入参、以及五份返回清单的输出 schema，
/// 用的都是 `build.rs` 那一次 `schema_for!(Composition)`（见 `actions.rs::composition_schema!`）。
///
/// 这里拿同一次 `schema_for!` 的**运行期真值**逐格比：手抄一份、或者生成物与类型漂了，
/// 这条就红。`embed` 是 `build.rs::project` 的规格复述（两处必须一致）。
#[cfg(feature = "json-schema")]
#[test]
fn the_composition_cell_is_generated_from_the_manifest_type() {
    let live = serde_json::to_value(schemars::schema_for!(vox_core::composition::Composition))
        .expect("schema 是数据");
    let expected = embed(&live);

    // 这一格真的描述了清单：八个必填格（`session` 可缺——直通清单没有会话），
    // 三个条目枚举各是一串带 `kind` 的对象。
    assert_eq!(
        expected["required"],
        json!([
            "control",
            "host",
            "in",
            "life",
            "ops",
            "out",
            "schema_version",
            "ui",
        ]),
        "`Composition` 的必填格"
    );
    for (definition, kinds) in [
        (
            "Input",
            vec!["mic", "process_loopback", "net_in", "host_feed"],
        ),
        ("Op", vec!["mono", "denoise", "gate", "resample"]),
        (
            "Output",
            vec!["playback", "captions", "net_out", "host_sink"],
        ),
    ] {
        let variants = expected["definitions"][definition]["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{definition} 该是 oneOf"));
        let actual: Vec<&str> = variants
            .iter()
            .filter_map(|variant| variant["properties"]["kind"]["enum"][0].as_str())
            .collect();
        assert_eq!(actual, kinds, "{definition} 的 kind 取值");
    }

    let action = |name: &str| {
        ACTIONS
            .iter()
            .find(|action| action.name == name)
            .expect("表里有这个动作")
    };
    // 收清单的那一格。
    let compose_input = parse_schema(action("compose_endpoint").input_schema);
    assert_eq!(compose_input["$defs"]["composition"], expected);
    // 五份往外发清单的输出 schema 用的是同一份生成物。
    for name in [
        "list_endpoints",
        "describe_endpoint",
        "compose_endpoint",
        "session_open",
        "session_close",
    ] {
        let output = parse_schema(action(name).output_schema);
        assert_eq!(
            output["$defs"]["composition"], expected,
            "{name} 的输出 schema 里的清单那一格"
        );
    }
}

/// `build.rs::project` 的规格复述：draft-07 文档 → 能嵌进 2020-12 `$defs.composition` 的样子。
///
/// 两处投影必须一致——生成物与这里对不上，上面那条用例就红（这也是"没漂"的证据本身）。
#[cfg(feature = "json-schema")]
fn embed(schema: &Value) -> Value {
    fn rescope(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    match (key.as_str(), child.as_str()) {
                        ("$ref", Some(reference)) => {
                            if let Some(name) = reference.strip_prefix("#/definitions/") {
                                *child = format!("#/$defs/composition/definitions/{name}").into();
                            }
                        }
                        _ => rescope(child),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(rescope),
            _ => {}
        }
    }

    let mut embedded = schema.clone();
    // 只有根节点能带 `$schema`，这一格是子 schema。
    embedded
        .as_object_mut()
        .expect("schemars 的产出是对象")
        .remove("$schema");
    // 子定义跟着这一格走（否则 `#/definitions/…` 会解析到外层文档的根）。
    rescope(&mut embedded);
    embedded
}

/// `json-schema` 关掉时那一格**如实放宽**：不假装自己描述了清单，`$comment` 里点名 feature
/// 没开（而不是承诺"将来接"）。开着的形状见上面那条用例。
#[cfg(not(feature = "json-schema"))]
#[test]
fn the_composition_cell_is_an_honest_placeholder_without_the_feature() {
    for action in ACTIONS {
        for (which, text) in [
            ("入参", action.input_schema),
            ("输出", action.output_schema),
        ] {
            let schema = parse_schema(text);
            let Some(cell) = schema["$defs"].get("composition") else {
                continue;
            };
            assert_eq!(cell["type"], json!("object"), "{} 的{which}", action.name);
            for key in ["properties", "required", "definitions", "oneOf"] {
                assert!(
                    cell.get(key).is_none(),
                    "{} 的{which}：没开 feature 就别广告字段级形状（有 {key}）",
                    action.name
                );
            }
            let comment = cell["$comment"].as_str().expect("占位的话写在 $comment 里");
            assert!(
                comment.contains("json-schema") && comment.contains("未开启"),
                "{} 的{which}：$comment 要点名 feature 没开，而且不能说成'将来接'：{comment}",
                action.name
            );
        }
    }
}

#[test]
fn error_codes_are_unique_snake_case_strings() {
    let mut codes: Vec<&str> = DomainErrorCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect();
    assert!(
        codes.contains(&"unsupported_field"),
        "compose_endpoint 的主要失败面"
    );
    assert!(
        codes.contains(&"compose_token_stale"),
        "两段式确认的全部意义"
    );
    for code in &codes {
        assert!(
            !code.is_empty()
                && code.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && !code.starts_with('_')
                && !code.ends_with('_'),
            "领域错误码是 snake_case 字符串：{code}"
        );
    }
    let unique = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), unique, "码表不许撞码");
}

#[test]
fn permissions_follow_the_endpoint_and_never_block_closing() {
    let action = |name: &str| {
        ACTIONS
            .iter()
            .find(|action| action.name == name)
            .expect("表里有这个动作")
    };

    let open = action("session_open").permissions;
    let speak = open.for_endpoint(Some(EndpointId::Speak));
    let listen = open.for_endpoint(Some(EndpointId::Listen));
    assert_ne!(speak, listen, "两个端点要的权限不同，不许写成并集");
    assert!(speak.contains(&vox_mcp::actions::Permission::Microphone));
    assert!(!speak.contains(&vox_mcp::actions::Permission::SystemAudio));
    assert!(listen.contains(&vox_mcp::actions::Permission::SystemAudio));
    assert!(!listen.contains(&vox_mcp::actions::Permission::Microphone));
    for set in [speak, listen] {
        assert!(set.contains(&vox_mcp::actions::Permission::AudibleOutput));
    }

    // 关麦永不被授权位挡住（设计稿 §2.1.3-⑤）；漏传端点时宁严不松。
    assert!(action("session_close")
        .permissions
        .for_endpoint(None)
        .is_empty());
    assert_eq!(
        open.for_endpoint(None).len(),
        3,
        "没给端点就按并集要权限，绝不静默放行"
    );
}

/// 测试用的最小后端：只为验**协议整形**（成功 / `isError` / `-32602` + `data.errors`）。
///
/// 它**不**代替真实现——真实现的规矩（四个闸门、`role` 跟事实走、compose 的四步顺序）
/// 写在 `ControlBackend` 的文档注释里，要真账本才验得了。这里五个动作回同一份答案，
/// 因为整形与动作无关。
struct StubBackend {
    reply: Reply,
}

enum Reply {
    Ok(Value),
    Domain(DomainError),
    /// `Composition::validate()` 失败时 S0 给的 `CompositionError` 列表。
    Composition(Value),
}

impl StubBackend {
    fn answer(&self) -> Result<Value, CallFailure> {
        match &self.reply {
            Reply::Ok(value) => Ok(value.clone()),
            Reply::Domain(error) => Err(error.clone().into()),
            Reply::Composition(errors) => Err(CallFailure::composition(errors.clone())),
        }
    }

    /// 资源面那两格要一份"数据"：失败那两种在资源面上不可达（`resources/*` 没有领域失败），
    /// 所以拿 `Reply::Ok` 里那份，别的回空对象。
    fn reply_value(&self) -> Value {
        match &self.reply {
            Reply::Ok(value) => value.clone(),
            Reply::Domain(_) | Reply::Composition(_) => json!({}),
        }
    }
}

impl ControlBackend for StubBackend {
    fn list_endpoints(&mut self) -> Result<Value, CallFailure> {
        self.answer()
    }
    fn describe_endpoint(&mut self, _endpoint: EndpointId) -> Result<Value, CallFailure> {
        self.answer()
    }
    fn compose_endpoint(
        &mut self,
        _endpoint: EndpointId,
        _composition: Value,
        _apply: bool,
        _token: Option<String>,
    ) -> Result<Value, CallFailure> {
        self.answer()
    }
    fn session_open(
        &mut self,
        _endpoint: EndpointId,
        _wait_ready_ms: u32,
    ) -> Result<Value, CallFailure> {
        self.answer()
    }
    fn session_close(&mut self, _session: &str) -> Result<Value, CallFailure> {
        self.answer()
    }
    fn list_resources(&mut self) -> Value {
        self.reply_value()
    }
    fn read_resource(&mut self, _uri: &str) -> Option<Value> {
        Some(self.reply_value())
    }
    fn poll_resources(&mut self) -> ResourceTick {
        ResourceTick {
            notify_ms: 250,
            list_revision: 0,
            resources: Vec::new(),
        }
    }
    /// 总闸开着：这个桩代表"控制面在正常工作"的服务端，总闸的语义（关掉 → 收流）由
    /// `tests/lifecycle.rs` 拿真账本 + 真 socket 验。
    fn control_enabled(&mut self) -> bool {
        true
    }
}

fn call_with(id: i64, name: &str, arguments: Value, backend: &mut dyn ControlBackend) -> Value {
    let mut params = Map::new();
    params.insert("_meta".to_string(), meta_object());
    params.insert("name".to_string(), json!(name));
    params.insert("arguments".to_string(), arguments);
    let message = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params });
    mcp::handle(&message, Some(backend))
        .response()
        .expect("请求必须有响应")
}

#[test]
fn backend_failure_reaches_the_client_on_the_right_channel() {
    // 成功：`structuredContent` 原样透传，`content` 给一份等价文本块，**不带 isError**。
    let structured = json!({
        "device": { "tier": "windows", "control": ["mcp", "cli", "http", "config_file"] },
        "endpoints": [{ "id": "speak", "title": "对外说话", "available": true, "running": false,
                        "summary": "麦克风 → 译音进虚拟麦 + 字幕" }],
    });
    let mut backend = StubBackend {
        reply: Reply::Ok(structured.clone()),
    };
    let response = call_with(1, "list_endpoints", json!({}), &mut backend);
    assert_eq!(response["result"]["resultType"], json!("complete"));
    assert_eq!(response["result"]["structuredContent"], structured);
    assert!(
        response["result"].get("isError").is_none(),
        "成功结果不带 isError：{response}"
    );
    assert_eq!(
        response["result"]["content"][0],
        json!({ "type": "text", "text": structured.to_string() }),
        "规范 SHOULD：结构化结果再给一份等价文本块"
    );

    // 领域失败：`isError:true` + `structuredContent.error`，**不是** JSON-RPC error。
    let mut backend = StubBackend {
        reply: Reply::Domain(DomainError::with_detail(
            DomainErrorCode::UnknownSession,
            "handle 不认识",
            json!({ "hint": "handle 只在本进程存活期内有效" }),
        )),
    };
    let response = call_with(
        2,
        "session_close",
        json!({ "session": "s_gone" }),
        &mut backend,
    );
    assert!(
        response.get("error").is_none(),
        "领域失败不许走 JSON-RPC error：{response}"
    );
    assert_eq!(response["result"]["resultType"], json!("complete"));
    assert_eq!(response["result"]["isError"], json!(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        json!("unknown_session")
    );
    assert_eq!(
        response["result"]["structuredContent"]["error"]["detail"]["hint"],
        json!("handle 只在本进程存活期内有效")
    );

    // 结构性不合法：协议错误 `-32602` + `data.errors`（一次给全部问题），且没有 `result`。
    let mut backend = StubBackend {
        reply: Reply::Composition(json!([
            { "kind": "host_mismatch", "manifest": "android", "machine": "windows" },
            { "kind": "missing_capability", "bit": "program_tap", "entry": "in[0]" },
        ])),
    };
    let response = call_with(
        3,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": {}, "apply": false }),
        &mut backend,
    );
    assert!(
        response.get("result").is_none(),
        "协议错误不许带 result：{response}"
    );
    assert_eq!(error_code(&response), code::INVALID_PARAMS);
    assert_eq!(
        response["error"]["data"]["errors"][0]["kind"],
        json!("host_mismatch")
    );
    assert_eq!(
        response["error"]["data"]["errors"]
            .as_array()
            .expect("数组")
            .len(),
        2
    );
}
