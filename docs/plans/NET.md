# NET：每服务商代理 + 测速 + Clash TUN 检测引导

状态：Main 已拍板（任务书）。本稿是施工依据；"字段名逐字一致"指的是与本文 §4 的契约表一致。

## 0. 目标

| # | 目标 |
|---|---|
| N1 | `Settings` 加按服务商的 `network: { mode, proxy }`。缺省：阿里云 `direct`，GPT/Gemini `system`。`custom` 支持 `http://` / `socks5://` / `socks5h://`（远端解析 DNS），可带 `user:pass@`；**密码只住在设置账本里**，日志 / 通知 / 快照只给去掉账号的地址与"已设置账号"这一位。 |
| N2 | 两条连接都走它：WebSocket（`vox-net`）与 HTTP 列表（`vox-host`）共用同一份隧道实现。 |
| N3 | `test_network(provider)`：按当前网络设置测候选接入点的 DNS / TCP / 隧道 / TLS / HTTPS 各段耗时或错误分类；阿里云另一地区快 1.5 倍以上且差 80ms 以上给换地区建议。 |
| N4 | TUN / 假 IP 检测**只检测 + 引导**，绝不尝试绕过：域名解析落 `198.18.0.0/15` = 假 IP；网卡名像代理软件虚拟网卡。只对阿里云直连模式提示。 |
| N5 | 模型页每个服务商加「网络」卡：模式三选一、自定义代理输入（校验格式）、测速按钮与结果表、TUN 引导卡、地区建议。zh/en/ja 三语。 |
| N6 | 网络设置变化**下次启动流水线生效**（与 `aliyun_endpoint` 一致），界面一行说明。 |

## 1. 决定

| # | 决定 | 理由 |
|---|---|---|
| D1 | 新 crate `crates/vox-proxy`（**纯 std、同步、只依赖 vox-core + base64**）放"地址解析 + 系统代理探测 + 同步拨号（直连 / HTTP CONNECT / SOCKS5）"。 | 调用方有三类且互相不能依赖：`vox-net` 是 async（tokio）、`vox-host` 是**同步 ureq 且硬规矩不引 tokio**、测速要分段计时。放哪边都会把另一边拖进来（vox-net 引 vox-host 会把 ureq/vox-mcp 拖进 WS 传输；vox-host 引 vox-net 会把 tokio 拖进共享宿主层）。一份纯 std 实现 + `spawn_blocking` 是最小公共子集。 |
| D2 | N2 的"二选一"：**不用 ureq 自带的 `Proxy` 配置**，改用 ureq 的 `unversioned` 连接器把"解析 + 建 TCP"两步换成 `vox_proxy::dial`，TLS 仍交给 ureq 的 rustls 连接器。 | ① SOCKS 要开 `socks-proxy` feature，多拉一个 `socks` crate（Windows 上还带 winapi）；② 自带那套与 WebSocket 是两份实现，"测速通"与"列表通"、"真连得上"可能各说各话；③ 同一份 `dial` 让测速、列表、WS 三条路完全一致。代价是用了 `unversioned` 接口（官方说明不受 semver 承诺），Cargo.toml 钉死大版本 3，升级时编译器拦下。 |
| D3 | `ProxySpec::Debug` / `display()` **不含账号密码**；`NetworkSetting::Debug` 只给 `redacted`；脱敏统一走 `vox_core::settings::split_proxy_credentials`。 | 一处实现，日志与快照都从它取。 |
| D4 | 自定义代理**格式不合法 = 错误**，不静默改直连。`socks5://`（非 h）在本地解析目标域名，`socks5h://` 把域名交给代理。 | 静默降级会让用户以为走了代理、实际暴露了真实出口。远端解析是"绕过本地 DNS 污染"的唯一办法。 |
| D5 | 系统代理：Linux/macOS 读 `HTTPS_PROXY`→`ALL_PROXY`（大小写都认）+ `NO_PROXY`；Windows 读注册表 `Internet Settings` 的 `ProxyEnable`/`ProxyServer`/`ProxyOverride`（`reg query`，**不新增依赖**）；读不到就直连；回环地址恒直连。 | N1 拍板原文。`reg.exe` 解析是纯函数，在所有平台都有测试。 |
| D6 | 网卡枚举：Linux/macOS 读 `/sys/class/net`，Windows 跑 `netsh interface show interface`（`CREATE_NO_WINDOW`），其它平台只做假 IP 判定并给 `interfaces_scanned: false`。 | 任务书许可的"不新增重依赖"路线。 |
| D7 | 测速的 HTTP 那一步**不带密钥**，预期 401/403；收到任何 HTTP 应答即算通，量往返时间。 | 不拿用户密钥做探测。 |
| D8 | 网络设置只在**建连时**读取（`Runtime::session_config_for` 已把 `Settings::network.of(provider)` 塞进 `SessionParams`/`ConnectRequest`），运行中的会话不热更新；界面给一行说明。 | N6，与 `aliyun_endpoint` 同一条规矩。 |
| D9 | TUN 引导**只对阿里云 + 直连模式**显示；GPT/Gemini 不提示。 | 任务书原文：阿里云直连时被绕到海外才会延迟升高，另两家本来就要代理。 |
| D10 | 给前端的事件里 `SettingsChanged` 要先脱敏再转发；`SettingsDto` 的 `network` 只带脱敏地址 + `proxy_auth` 一位。 | 密码不许出芯。 |

## 2. 接口

### 2.1 芯：`vox-core`

- `settings::NetworkMode { Direct, System, Custom }`（serde `snake_case`）。
- `settings::NetworkSetting { mode, proxy }`（`#[serde(default)]`，老配置读出来 = 直连）。
  `proxy_display()` / `proxy_has_auth()` / `split_proxy_credentials(&str) -> (String, bool)`。
- `settings::NetworkSettings { aliyun, gpt, gemini }` + `of(provider)` / `entries_mut()`；`Default` = 阿里云 direct、另两家 system。
- `Settings.network: NetworkSettings`；`Settings::without_secrets()` 返回脱敏副本；`normalize()` 只给 `proxy` 裁空白（合法性交给建连处与界面）。
- `cloud::ConnectRequest.network: NetworkSetting`（`Session::with_network(...)`），`Session::connect_request()` 逐字带上。
- `pipeline::TransportFactory` 造出的传输从 `ConnectRequest.network` 拿出口；`Runtime::session_config_for` 按 pipeline 的 provider 选一家。

### 2.2 代理 crate：`vox-proxy`

```
ProxySpec::parse("http|socks5|socks5h://[user:pass@]host[:port]") -> Result<ProxySpec, ProxyError>
ProxySpec::display() -> String                       // 不含账号密码
resolve(&NetworkSetting, target_host) -> Result<Option<ProxySpec>, ProxyError>   // None = 直连
system::detect(target_host) -> Option<ProxySpec>     // 环境变量 / 注册表
dial(Option<&ProxySpec>, host, port, budget) -> Result<Dialed { stream, dns, connect, tunnel }, DialError>
```

- `DialError { stage: Dns|Connect|Tunnel, timed_out, message }`；`message` 不含账号密码。
- `testing` feature：本机假 HTTP CONNECT / SOCKS5 代理 + 回显服务，记录每条隧道请求的目标（供"域名真的交给代理解析"这类断言）。

### 2.3 测速 / 检测：`vox-host::netcheck`

```
test_network(provider: ModelProvider, network: &NetworkSetting, region: AliyunRegion) -> NetworkReport
detect_takeover(provider: ModelProvider) -> TakeoverReport
```

| 类型 | 字段（与前端逐字一致） |
|---|---|
| `NetworkReport` | `provider`、`mode`、`via: Option<String>`（脱敏代理地址）、`results: EndpointResult[]`、`suggestion: Option<Suggestion>` |
| `EndpointResult` | `id`、`host`、`current`、`dns_ms`、`connect_ms`、`tunnel_ms`、`tls_ms`、`http_ms`、`http_status`、`total_ms`、`error: Option<Failure{kind, message}>` |
| `FailureKind` | `dns` / `connect` / `tls` / `http` / `timeout` |
| `Suggestion` | `id`、`host`、`current_ms`、`nearest_ms` |
| `TakeoverReport` | `detected`、`fake_ip`、`fake_ip_hosts[]`、`interfaces[]`、`interfaces_scanned` |

候选接入点：阿里云 `cn`（`dashscope.aliyuncs.com`）/ `intl`（`dashscope-intl.aliyuncs.com`）/ `hk`（`cn-hongkong.dashscope.aliyuncs.com`）；OpenAI `api.openai.com`；Gemini `generativelanguage.googleapis.com`。无密钥 GET：阿里云 `/api/v1/models?page_size=1`、OpenAI `/v1/models`、Gemini `/v1beta/models`。

### 2.4 装配层命令（`app/src-tauri`）

| 命令 | 参数 | 返回 |
|---|---|---|
| `test_network` | `provider: String` | `vox_host::netcheck::NetworkReport` |
| `detect_takeover` | `provider: String` | `vox_host::netcheck::TakeoverReport` |

两条都读当前设置里的网络设置 / 地区，`spawn_blocking` 跑（阻塞式，不占命令线程）。`list_models` 多收一个入参来源不变——按 provider 从设置里取 `network`。

### 2.5 前端（`app/ui`）

- `types.ts`：`NetworkMode` / `NetworkSetting { mode, proxy, proxy_auth }` / `NetworkSettings { aliyun, gpt, gemini }`；`Settings.network`；`NetworkReport` / `EndpointResult` / `Failure` / `Suggestion` / `TakeoverReport` 镜像。
- `api.ts`：`testNetwork(provider)`、`detectTakeover(provider)`。
- `Models.tsx`：`ProviderDetail` 里加 `<NetworkCard provider>`（地区卡之后、可用模型卡之前）。mock 后端给假数据，`?mockTun=1` 触发"检测到假 IP"分支。

## 3. 文件清单

| 文件 | 改动 |
|---|---|
| `crates/vox-proxy/**` | 新 crate：`spec.rs`（地址解析 / 脱敏）、`system.rs`（系统代理探测）、`dial.rs` + `dial/tests.rs`（隧道）、`testing.rs`（测试替身）、`lib.rs` |
| `crates/vox-core/src/settings.rs` | `network` 字段、缺省、归一、`without_secrets`、脱敏助手与测试 |
| `crates/vox-core/src/cloud/mod.rs`、`pipeline/{mod,chain}.rs`、`runtime.rs` | 把 `NetworkSetting` 从设置带到建连处 |
| `crates/vox-net/src/ws.rs` | `connect_with_close_catcher` 之前插 `vox_proxy::dial`（`spawn_blocking`）；CloseCatcher 仍在 TLS 之上 |
| `crates/vox-net/Cargo.toml`、`crates/vox-host/Cargo.toml`、`Cargo.toml`、`Cargo.lock` | vox-proxy 进 workspace；vox-host 加 vox-proxy + rustls/webpki-roots（测速自己握手）；dev 依赖加 testing feature 与 rcgen |
| `crates/vox-host/src/http.rs` | ureq 的 `unversioned` 连接器走 `vox_proxy::dial` |
| `crates/vox-host/src/models.rs` | `list_models` 多收 `&NetworkSetting` |
| `crates/vox-host/src/netcheck.rs` + `netcheck/tests.rs` | 测速与接管检测的共享实现 + 测试 |
| `app/src-tauri/src/commands.rs`、`lib.rs`、`dto.rs`、`events.rs` | 两条新命令、`list_models` 传网络设置、`SettingsDto.network` 脱敏、`SettingsChanged` 转前端前脱敏 |
| `app/ui/src/{types.ts,types.snapshot.ts,api.ts,defaults.ts,mock/*,i18n/*,sections/Models.tsx}` | 网络卡 + 三语文案 + mock |

## 4. 测试

| 用例 | 会因为什么真实 bug 红 |
|---|---|
| `spec::parses_the_three_schemes_and_defaults` | 端口缺省 / IPv6 / 大小写猜错 |
| `spec::credentials_are_decoded_and_never_displayed` | 百分号解码错、Debug 漏出密码 |
| `spec::rejects_garbage_with_a_reason` | 空串 / 坏端口 / 坏协议被当成直连 |
| `dial::http_connect_tunnels_to_the_requested_authority` / `..._hands_the_hostname_to_the_proxy` | 隧道没建、域名被本地解析 |
| `dial::http_connect_sends_basic_credentials_and_reports_407_without_leaking_them` | 账号没发、407 错误串带密码 |
| `dial::socks5_resolves_locally_and_sends_an_ip` / `socks5h_hands_the_domain_to_the_proxy_and_never_resolves_locally` | socks5/socks5h 语义颠倒 |
| `dial::socks5_password_auth_accepts_the_right_one_and_refuses_the_wrong_one` | 认证帧写错 |
| `dial::a_dead_proxy_fails_in_the_connect_stage` / `a_proxy_that_never_answers_times_out_in_the_tunnel_stage` | 失败阶段归类错、卡死 |
| `ws::wss_goes_through_an_http_connect_tunnel` / `..._socks5h_...` / `plain_socks5_cannot_reach_a_name_only_the_proxy_knows` / `a_dead_proxy_does_not_fall_back_to_a_direct_connection` / `a_malformed_custom_proxy_is_an_error_not_a_direct_connection` / `plain_ws_also_tunnels` | 隧道层次错（CloseCatcher 位置）、静默降级直连 |
| `system::env_picks_https_then_all_proxy_in_either_case` / `no_proxy_excludes_hosts_and_subdomains` / `loopback_is_never_proxied_in_system_mode` / `windows_registry_dump_is_understood` / `windows_per_protocol_server_string` | 环境变量优先级、NO_PROXY/ProxyOverride 通配、注册表解析 |
| `settings::network_defaults_are_per_provider_and_the_password_never_prints` / `credentials_are_split_off_whatever_the_shape` | 缺省错、Debug / 快照漏密码 |
| `netcheck::a_direct_probe_reports_every_stage_and_counts_401_as_reachable` | 分段计时错、把 401 当失败 |
| `netcheck::the_probe_really_goes_through_an_http_connect_proxy` / `socks5h_probe_lets_the_proxy_resolve_the_name` | 测速没走当前网络设置 |
| `netcheck::failures_are_classified_by_stage` / `a_server_that_never_answers_is_a_timeout_not_a_hang` | 错误分类映射错、超时卡死 |
| `netcheck::suggest_needs_both_a_1_5x_ratio_and_an_80ms_gain` / `suggest_picks_the_fastest_other_region_and_ignores_failures` / `only_aliyun_gets_a_suggestion_and_the_candidates_match_the_spec` | 建议门槛与候选表 |
| `netcheck::fake_ip_range_is_exactly_198_18_0_0_slash_15` / `a_fake_ip_answer_is_flagged_even_without_a_tun_adapter` / `a_tun_adapter_alone_is_detected_and_unscannable_platforms_say_so` / `adapter_name_rules` / `netsh_output_yields_adapter_names_with_spaces` | 假 IP 段边界、网卡名规则 |
| `dto`：快照 JSON 里搜不到代理密码 | 脱敏漏了一条出芯的路（事件 / 快照） |

不联网：所有测试的代理 / TLS 服务端都是本机自签 + 本机 TCP（`vox-proxy` 的 `testing` feature 与 `rcgen`）。

## 5. 不做

- 不绕过代理软件、不改系统代理设置（N4 只检测 + 引导）。
- 不动 `catalog/**` 与语言相关代码（LANG 工单在地）。
- 不碰 `crates/vox-overlay-*`。
