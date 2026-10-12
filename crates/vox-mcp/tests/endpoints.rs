//! 端点投影与四步校验链的用例（`endpoints.rs` + `session.rs`）。
//!
//! 全部用**真 `Runtime` + 构造的 `Settings` / `HostFacts`**（不必连 GUI、不必连设备）：
//! 投影只走公开面，所以这里断言的全是外部可见的契约——清单是不是芯那条唯一派生路径算出来的、
//! `editable` 与 §2.1.4 对不对得上、改 `editable` 之外的格会不会被拒、`in: []` 走不走协议错误
//! 通道、`role` 跟不跟事实走、token 是不是一次性且**到点就废**（TTL 边界靠推进假时钟测）。
//!
//! 为什么用真账本而不是假账本：这一层的**全部价值**就是"清单与事实只有一份"。拿假账本测，
//! 测的是假账本自己的映射——那正是本设计要排除的东西。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use vox_core::capability::{Capability, HostFacts, UnavailableReason};
use vox_core::composition::{Composition, HostKind};
use vox_core::event::{Pipeline, PipelineState};
use vox_core::ports::{Clock, PortResult};
use vox_core::runtime::{PipelineCommand, PipelineControl, Runtime};
use vox_core::settings::{ListenTarget, ModelProvider, Settings};
use vox_core::usage::Stamp;

use vox_mcp::actions::{DomainErrorCode, EndpointId, Permission, COMPOSE_TOKEN_TTL_MS};
use vox_mcp::endpoints::{self, ENDPOINTS};
use vox_mcp::handlers::CallFailure;
use vox_mcp::ledger::{Denied, Grants, Ledger};
use vox_mcp::session::LedgerBackend;
use vox_mcp::ControlBackend;

// --- 测试用的本机 ------------------------------------------------------------------

/// 单调时钟。compose token 的 TTL 靠**推进它**来测（真等 120 s 不现实），所以它得是可控的：
/// 假时钟 + 真 `Runtime`，`Tokens` 那侧的时间全从 [`Ledger::now_ms`] 来。
#[derive(Default)]
struct TestClock(AtomicU64);

impl TestClock {
    /// 把假时钟拨到 `ms`（用例只关心"两次读之间的差"）。
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            unix_secs: 0,
            year: 2026,
            month: 9,
            day: 22,
        }
    }
}

/// 桌面档、一个位都不关（= 已发货的那两档在采集 / 播放 / 字幕上的口径）。
fn desktop_facts() -> HostFacts {
    HostFacts {
        host: HostKind::Windows,
        off: BTreeMap::new(),
        virtual_mic_device: None,
    }
}

/// 没装虚拟麦的机器：`role` 必须跟着退成 `speaker`（位假却写 `virtual_mic` 就是撒谎）。
fn no_virtual_mic_facts() -> HostFacts {
    HostFacts {
        host: HostKind::Windows,
        off: BTreeMap::from([(Capability::VirtualMic, UnavailableReason::NotInstalled)]),
        virtual_mic_device: None,
    }
}

/// 一份"本机真实快照"的构造版：两条腿都配好（`listen` 没目标就派不出清单）。
fn settings() -> Settings {
    let mut settings = Settings::default();
    settings.speak.input_device = Some("Yeti Stereo Microphone".to_string());
    settings.speak.output_device = Some("CABLE Input (VB-Audio Virtual Cable)".to_string());
    settings.listen.target = Some(ListenTarget {
        executable: "Discord.exe".to_string(),
        display_name: "Discord".to_string(),
        include_process_tree: true,
    });
    settings
}

/// 真账本：芯的 `Runtime` + 注入的事实 + **可控的假时钟**。**没有任何第二份映射**——投影只认它。
fn clocked(facts: HostFacts) -> (Arc<TestClock>, Runtime) {
    let clock = Arc::new(TestClock::default());
    let runtime = Runtime::new(settings(), clock.clone());
    runtime.set_host_facts(facts);
    // 密钥只影响"能不能开云端会话"，不影响清单（`Composition::of` 一格都不读它）。
    runtime.set_api_key_for(ModelProvider::Aliyun, "test-key");
    (clock, runtime)
}

/// 同上，只是不关心时钟（绝大多数用例不看时间）。
fn ledger(facts: HostFacts) -> Runtime {
    clocked(facts).1
}

/// 全开的授权位：模拟"用户在设置里把四个位都打开了"。
///
/// 这是**对照**用的假实现——真实现是芯自己的 `impl Grants for Runtime`（授权位现读
/// `Settings.control.allow_*`），由 `the_runtime_grants_read_the_control_settings` 钉着。
struct Granted;

impl Grants for Granted {
    fn user_granted(&self, _permission: Permission) -> bool {
        true
    }

    fn config_write_allowed(&self) -> bool {
        true
    }
}

/// 假的流水线控制面：接到 `Start` 立刻报 `Ready`（芯的 `on_pipeline_state` 是公开面）。
struct ReadyControl {
    runtime: Runtime,
}

impl PipelineControl for ReadyControl {
    fn apply(&self, command: PipelineCommand) -> PortResult<()> {
        if let PipelineCommand::Start(config) = command {
            self.runtime.on_pipeline_state(
                config.pipeline,
                config.session_id,
                PipelineState::Ready,
            );
        }
        Ok(())
    }
}

fn harness(facts: HostFacts) -> (Runtime, LedgerBackend<Runtime, Granted>) {
    let runtime = ledger(facts);
    let backend = LedgerBackend::new(runtime.clone(), Granted);
    (runtime, backend)
}

/// 这一格的 `unsupported_field` 细节。
fn unsupported(error: &CallFailure) -> Value {
    match error {
        CallFailure::Domain(error) => {
            assert_eq!(
                error.code,
                DomainErrorCode::UnsupportedField,
                "期望 unsupported_field：{error:?}"
            );
            error.detail.clone().expect("unsupported_field 必带 detail")
        }
        other => panic!("期望领域失败，拿到 {other:?}"),
    }
}

/// 一次领域失败的 `(code, detail)`——TTL 那几条要看 `detail.expires_in_ms` 与重新算的 diff。
fn domain_failure(error: CallFailure) -> (DomainErrorCode, Value) {
    match error {
        CallFailure::Domain(error) => (error.code, error.detail.unwrap_or(Value::Null)),
        other => panic!("期望领域失败，拿到 {other:?}"),
    }
}

/// "改目标语言 → dry-run"（TTL 三条各要一份干净的 token，所以抽出来）。返回 `(改过的清单, token)`。
fn dry_run_edited<G: Grants>(backend: &mut LedgerBackend<Runtime, G>) -> (Value, String) {
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    let mut edited = described["manifest"].clone();
    edited["session"]["params"]["target_language"] = json!("en");
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, edited.clone(), false, None)
        .expect("dry-run");
    assert_eq!(
        dry_run["changed"][0]["path"],
        json!("session.params.target_language")
    );
    let token = dry_run["token"].as_str().expect("token").to_string();
    (edited, token)
}

// --- 用例 --------------------------------------------------------------------------

/// 投影往返：`describe` 给的清单**逐字**等于芯那条唯一派生路径算出来的那一份；
/// 原样发回零差异；改一格恰好一条差异；apply 真落进账本；同一个 token 重放必须失败。
#[test]
fn the_projection_round_trips_on_a_real_settings_snapshot() {
    let (runtime, mut backend) = harness(desktop_facts());

    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");

    // 唯一签名：`Composition::of(&runtime.session_config(…), &runtime.host_facts())`。
    let expected = Composition::of(
        &runtime.session_config(Pipeline::Speak),
        &runtime.host_facts(),
    )
    .expect("本机派生的清单");
    assert_eq!(
        described["manifest"],
        endpoints::wire(&expected),
        "describe 的 manifest 必须就是芯那条派生路径算出来的那一份"
    );
    assert_eq!(described["endpoint"], json!("speak"));
    assert_eq!(described["available"], json!(true));
    assert_eq!(described["running"], json!(false));
    assert_eq!(described["capabilities"]["tier"], json!("windows"));
    assert!(described["capabilities"]["host"]["mic"]["enabled"].is_boolean());
    assert_eq!(
        described["permissions"][0]["permission"],
        json!("microphone")
    );
    assert_eq!(described["permissions"][0]["user_granted"], json!(true));
    assert_eq!(described["permissions"][0]["os_granted"], json!(true));

    // 原样发回 = 零差异（投影与反方向表自洽）。
    let round_trip = backend
        .compose_endpoint(
            EndpointId::Speak,
            described["manifest"].clone(),
            false,
            None,
        )
        .expect("dry-run");
    assert_eq!(round_trip["changed"], json!([]));
    assert_eq!(round_trip["applied"], json!(false));
    assert_eq!(round_trip["expires_in_ms"], json!(120_000));
    assert_eq!(round_trip["manifest"], described["manifest"]);

    // 改一格：恰好一条差异，`from` 是当前值。
    let mut edited = described["manifest"].clone();
    edited["session"]["params"]["target_language"] = json!("en");
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, edited.clone(), false, None)
        .expect("dry-run");
    let changed = dry_run["changed"].as_array().expect("changed 是数组");
    assert_eq!(changed.len(), 1, "只改了一格：{changed:?}");
    assert_eq!(changed[0]["path"], json!("session.params.target_language"));
    assert_eq!(changed[0]["from"], json!("ja"));
    assert_eq!(changed[0]["to"], json!("en"));

    // apply：带 token 才落进账本，返回的是**账本重新派生**的那份。
    let token = dry_run["token"]
        .as_str()
        .expect("dry-run 必须签 token")
        .to_string();
    assert!(token.starts_with("c_"), "token 形如 c_…：{token}");
    let applied = backend
        .compose_endpoint(EndpointId::Speak, edited.clone(), true, Some(token.clone()))
        .expect("apply");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(
        applied["manifest"]["session"]["params"]["target_language"],
        json!("en")
    );
    assert_eq!(runtime.settings().speak.target_language, "en");
    assert!(applied.get("token").is_none(), "apply 不回 token");

    // 一次性：同一个 token 再打一次 → compose_token_stale（附重新算好的 diff）。
    let replay = backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect_err("重放必须失败");
    match replay {
        CallFailure::Domain(error) => {
            assert_eq!(error.code, DomainErrorCode::ComposeTokenStale);
            assert!(error.detail.expect("detail")["changed"].is_array());
        }
        other => panic!("期望 compose_token_stale，拿到 {other:?}"),
    }

    // 落盘之后 describe 报的是新值（写真的生效了）。
    let after = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    assert_eq!(
        after["manifest"]["session"]["params"]["target_language"],
        json!("en")
    );
}

/// compose token 的 TTL 是**硬边界**，而且**失败即作废**（fail-closed）。
///
/// 三条各钉一件事，缺一条就有一个洞：
///
/// - `TTL - 1` ms 还认 → 它不是"随便过一会儿就失效"；
/// - 正好 `TTL` ms 不认 → 判据是 `now < expires_at`（`<` 不是 `<=`），且 `expires_in_ms` 如实报 0；
/// - 一次**错**的 `apply` 会把待用的 token 一起烧掉 → 客户端必须重新 dry-run，不能拿旧 token 试。
///
/// 时间靠 [`TestClock`] 推进（真等 120 s 不现实），token 的两次读写都走 [`Ledger::now_ms`]。
#[test]
fn a_compose_token_expires_exactly_at_its_ttl_and_a_failed_apply_burns_it() {
    // ① 边界内侧：TTL - 1 ms 还认，而且真落进账本。
    let (clock, runtime) = clocked(desktop_facts());
    let mut backend = LedgerBackend::new(runtime.clone(), Granted);
    let (edited, token) = dry_run_edited(&mut backend);
    clock.set(COMPOSE_TOKEN_TTL_MS - 1);
    let applied = backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect("TTL 之内的 token 必须认");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(runtime.settings().speak.target_language, "en");

    // ② 正好 TTL ms：不认（这一毫秒就是"过期"），剩余有效期报 0，且**照旧给重新算好的 diff**。
    let (clock, runtime) = clocked(desktop_facts());
    let mut backend = LedgerBackend::new(runtime.clone(), Granted);
    let (edited, token) = dry_run_edited(&mut backend);
    clock.set(COMPOSE_TOKEN_TTL_MS);
    let (code, detail) = domain_failure(
        backend
            .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
            .expect_err("正好到 TTL 就该过期"),
    );
    assert_eq!(code, DomainErrorCode::ComposeTokenStale);
    assert_eq!(detail["expires_in_ms"], json!(0));
    assert_eq!(
        detail["changed"][0]["path"],
        json!("session.params.target_language"),
        "过期也要把差异给出去：{detail}"
    );
    assert_eq!(runtime.settings().speak.target_language, "ja", "没写进账本");

    // 过期不把这条腿锁死：重新 dry-run 拿的新 token 从新的一刻起算，照用。
    let (edited, token) = dry_run_edited(&mut backend);
    clock.set(COMPOSE_TOKEN_TTL_MS + 1);
    let applied = backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect("新签的 token 照用");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(runtime.settings().speak.target_language, "en");

    // ③ 失败即作废：先用**错**的 token 打一次（此刻这格还剩整份 TTL，所以失败的理由不是过期），
    //    再拿**对**的 token 打一次——照样不认（待用那一格已经被上一次失败摘掉了）。
    let (clock, runtime) = clocked(desktop_facts());
    let mut backend = LedgerBackend::new(runtime.clone(), Granted);
    let (edited, token) = dry_run_edited(&mut backend);
    let (code, detail) = domain_failure(
        backend
            .compose_endpoint(
                EndpointId::Speak,
                edited.clone(),
                true,
                Some("c_0000".to_string()),
            )
            .expect_err("错的 token 不认"),
    );
    assert_eq!(code, DomainErrorCode::ComposeTokenStale);
    assert_eq!(
        detail["expires_in_ms"],
        json!(COMPOSE_TOKEN_TTL_MS),
        "此刻还没过期：失败的理由是「不匹配」"
    );
    assert_eq!(clock.now_ms(), 0, "这一条不推进时钟");

    let (code, detail) = domain_failure(
        backend
            .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
            .expect_err("错的尝试把正确的 token 一起烧掉了"),
    );
    assert_eq!(code, DomainErrorCode::ComposeTokenStale);
    assert_eq!(detail["expires_in_ms"], json!(0), "待用格已摘掉 → 没得可用");
    assert_eq!(
        runtime.settings().speak.target_language,
        "ja",
        "三次尝试一个字节都没写进账本"
    );
}

/// token 绑的是**那一份清单的字节**，不只是端点：dry-run A 签的 token，拿去 apply B 必须被拒。
///
/// 钉的是 `Tokens::redeem` 里 `pending.manifest == manifest` 那一跳。丢了它，token 就只绑到
/// 端点——"用户看过并同意的是 A"这句话就落空：同一个 token 能落进任何 B。拒绝的理由必须是
/// **不匹配**（此刻整份 TTL 都还在），所以 `expires_in_ms` 报满额而不是 0。
#[test]
fn a_compose_token_is_bound_to_the_manifest_it_was_signed_for() {
    let (runtime, mut backend) = harness(desktop_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");

    // A：dry-run 改目标语言 → 拿到绑在 A 上的 token。
    let mut a = described["manifest"].clone();
    a["session"]["params"]["target_language"] = json!("en");
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, a.clone(), false, None)
        .expect("dry-run A");
    assert_eq!(
        dry_run["changed"][0]["path"],
        json!("session.params.target_language")
    );
    let token = dry_run["token"].as_str().expect("token").to_string();

    // B：**另一份同样合法、同样可改**的清单（拆掉降噪那一步），只是不是 A。
    let mut b = described["manifest"].clone();
    b["ops"].as_array_mut().expect("ops").remove(1); // denoise

    let (code, detail) = domain_failure(
        backend
            .compose_endpoint(EndpointId::Speak, b, true, Some(token.clone()))
            .expect_err("token 绑的是 A，不能落到 B 上"),
    );
    assert_eq!(code, DomainErrorCode::ComposeTokenStale);
    assert_eq!(
        detail["expires_in_ms"],
        json!(COMPOSE_TOKEN_TTL_MS),
        "整份 TTL 都还在：拒绝的理由是「不匹配」，不是过期"
    );
    assert_eq!(
        detail["changed"][0]["path"],
        json!("ops[denoise]"),
        "照旧把 B 的差异给出去：{detail}"
    );
    assert!(
        runtime.settings().speak.denoise,
        "B 的降噪一个字节都没落进账本"
    );

    // fail-closed：这次失败的尝试把待用格摘掉了，连**对的** A 也再用不了。
    let (code, _) = domain_failure(
        backend
            .compose_endpoint(EndpointId::Speak, a, true, Some(token))
            .expect_err("上一次失败已经把 token 一起烧掉了"),
    );
    assert_eq!(code, DomainErrorCode::ComposeTokenStale);
    assert_eq!(runtime.settings().speak.target_language, "ja");
}

/// `editable` 就是 §2.1.4 那张表（逐字），而且**不含 `ops[gate]` 那两格**（D6）。
#[test]
fn editable_is_the_design_table_and_excludes_the_gate_cells() {
    let (_runtime, mut backend) = harness(desktop_facts());

    let speak = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    assert_eq!(
        speak["editable"],
        json!([
            "in[0].device",
            "ops[denoise] 有无",
            "ops[aec] 有无",
            "out[playback(primary)].device",
            "out[playback(monitor)] 有无",
            "session.provider",
            "session.params.target_language",
            "session.params.voice",
            "session.params.clone_frequency",
            "session 有无（= 直通）",
        ])
    );

    let listen = backend
        .describe_endpoint(EndpointId::Listen)
        .expect("describe_endpoint");
    assert_eq!(
        listen["editable"],
        json!([
            "in[0].executable",
            "in[0].include_tree",
            "out[playback(primary)].device",
            "session.provider",
            "session.params.voice",
            "session.params.source_language",
        ])
    );

    // D6：闸门那两格不在任何端点的 `editable` 里。
    for entry in ENDPOINTS {
        for key in endpoints::editable(entry.id) {
            assert!(
                !key.contains("gate"),
                "{} 的 editable 里不该有 gate：{key}",
                entry.id.as_str()
            );
        }
    }

    // 真去改闸门 → `unsupported_field`，`why` 指到 D6 那一条（不是"泛泛不可改"）。
    let mut edited = speak["manifest"].clone();
    // 按 `kind` 找门那一格，不写死下标：W5 在 `denoise` 与 `gate` 之间插了 `aec`，
    // 下标会把这一格指到别人身上（测试就会变成"改 aec 被拒"，测的东西就没了）。
    let gate = edited["ops"]
        .as_array_mut()
        .expect("ops 是数组")
        .iter_mut()
        .find(|op| op["kind"] == json!("gate"))
        .expect("Speak 的清单恒有门");
    gate["config"]["threshold"] = json!(0.02);
    let failure = backend
        .compose_endpoint(EndpointId::Speak, edited, false, None)
        .expect_err("改闸门必须被拒");
    let detail = unsupported(&failure);
    assert_eq!(detail["path"], json!("ops[gate].config.threshold"));
    assert!(
        detail["why"].as_str().expect("why").contains("D6"),
        "why 要指到那一条：{detail}"
    );
}

/// 缺输入在**第一道闸**就被拒：`in: []` 走协议错误通道（`-32602` + `data.errors[0].kind ==
/// "missing_input"`），不是 `isError`。
#[test]
fn a_manifest_without_an_input_is_rejected_on_the_first_gate() {
    let (_runtime, mut backend) = harness(desktop_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");

    let mut edited = described["manifest"].clone();
    edited["in"] = json!([]);
    let failure = backend
        .compose_endpoint(EndpointId::Speak, edited, false, None)
        .expect_err("in: [] 必须在第一道闸被拒");

    match failure {
        CallFailure::InvalidParams { errors, .. } => {
            assert_eq!(errors[0]["kind"], json!("missing_input"), "{errors}");
        }
        other => panic!("缺输入属结构性问题，必须走 -32602 通道，拿到 {other:?}"),
    }
}

/// 拿错档位的清单：`missing_on` 排在 diff **前面**，所以报的是 `host_mismatch`
/// （反过来的话会先撞"`host` 这一格不能改"）。
#[test]
fn a_manifest_from_another_tier_is_reported_before_the_diff() {
    let (_runtime, mut backend) = harness(desktop_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");

    let mut edited = described["manifest"].clone();
    edited["host"] = json!("android");
    let failure = backend
        .compose_endpoint(EndpointId::Speak, edited, false, None)
        .expect_err("拿错档位必须被拒");
    match failure {
        CallFailure::Domain(error) => {
            assert_eq!(error.code, DomainErrorCode::EndpointUnavailable);
            let detail = error.detail.expect("detail");
            assert_eq!(
                detail["errors"][0]["kind"],
                json!("host_mismatch"),
                "{detail}"
            );
            assert_eq!(detail["errors"][0]["manifest"], json!("android"));
            assert_eq!(detail["errors"][0]["machine"], json!("windows"));
        }
        other => panic!("拿错档位是领域失败（isError），拿到 {other:?}"),
    }
}

/// 三处"联动格"的不一致：改 `life`、删字幕出口、`voice = null` 却留着播放汇。
/// 前两个由 diff 逮住，第三个由**重新派生**逮住（单看某一格判不出来）。
#[test]
fn inconsistent_or_immutable_cells_are_refused_with_a_reason() {
    let (_runtime, mut backend) = harness(desktop_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    let manifest = described["manifest"].clone();

    // ① 必须逐字相同的格。
    let mut edited = manifest.clone();
    edited["life"] = json!("daemon");
    let detail = unsupported(
        &backend
            .compose_endpoint(EndpointId::Speak, edited, false, None)
            .expect_err("改 life 必须被拒"),
    );
    assert_eq!(detail["path"], json!("life"));
    assert_eq!(detail["expected"], json!("interactive"));

    // ② 恒定的结构：字幕出口恒在（有会话时）。
    let mut edited = manifest.clone();
    edited["out"].as_array_mut().expect("out").remove(1);
    let detail = unsupported(
        &backend
            .compose_endpoint(EndpointId::Speak, edited, false, None)
            .expect_err("删字幕出口必须被拒"),
    );
    assert_eq!(detail["path"], json!("out[captions]"));

    // ③ 对外说话不能"只要文字不要语音"（`Settings::normalize` 把 `speak_translation` 钉死为真），
    // 所以 `voice = null` 落不下去——连带那条播放汇一起删也不行。
    let mut edited = manifest.clone();
    edited["session"]["params"]["voice"] = json!(null);
    let detail = unsupported(
        &backend
            .compose_endpoint(EndpointId::Speak, edited.clone(), false, None)
            .expect_err("voice=null 必须被拒"),
    );
    assert_eq!(detail["path"], json!("session.params.voice"));
    assert!(
        detail["why"]
            .as_str()
            .expect("why")
            .contains("speak_translation"),
        "why 要指到具体那一条：{detail}"
    );

    let mut edited = manifest.clone();
    edited["session"]["params"]["voice"] = json!(null);
    edited["out"].as_array_mut().expect("out").remove(0);
    let detail = unsupported(
        &backend
            .compose_endpoint(EndpointId::Speak, edited, false, None)
            .expect_err("连带删掉播放汇也不行"),
    );
    assert_eq!(detail["path"], json!("out[playback(primary)]"));
    assert!(
        detail["why"]
            .as_str()
            .expect("why")
            .contains("speak_translation"),
        "why 要指到具体那一条：{detail}"
    );

    // 听人说话可以只要字幕：`voice = null` **且**那条播放汇一起消失（§2.1.4 的联动格）。
    let (listen_runtime, mut listen) = harness(desktop_facts());
    let described = listen
        .describe_endpoint(EndpointId::Listen)
        .expect("describe_endpoint");
    let mut edited = described["manifest"].clone();
    edited["session"]["params"]["voice"] = json!(null);
    edited["out"].as_array_mut().expect("out").remove(0);
    let dry_run = listen
        .compose_endpoint(EndpointId::Listen, edited.clone(), false, None)
        .expect("voice=null 且删掉播放汇");
    let paths: Vec<&str> = dry_run["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .map(|change| change["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        vec!["out[playback(primary)]", "session.params.voice"],
        "两条一起改：{dry_run}"
    );
    let token = dry_run["token"].as_str().expect("token").to_string();
    listen
        .compose_endpoint(EndpointId::Listen, edited, true, Some(token))
        .expect("apply");
    assert!(!listen_runtime.settings().listen.speak_translation);
    assert_eq!(
        listen_runtime.settings().listen.voice,
        "Tina",
        "音色留着，只是不念了"
    );
}

/// `role` 跟**事实**走：虚拟麦位为假时 `out[0].role` 是 `speaker`（不是按设备名或按腿硬编码）。
#[test]
fn role_follows_the_virtual_mic_bit() {
    let (_runtime, mut backend) = harness(no_virtual_mic_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    assert_eq!(described["manifest"]["out"][0]["role"], json!("speaker"));
    assert_eq!(
        described["capabilities"]["host"]["virtual_mic"]["enabled"],
        json!(false)
    );
    // 事实让清单降级 → 一条**数据**说明（不是文案）。
    assert_eq!(
        described["notes"][0],
        json!("virtual_mic=off(not_installed) → out[playback(primary)].role=speaker")
    );

    // 位假的那份清单照样能原样发回（反方向表不按设备名判 role）。
    let round_trip = backend
        .compose_endpoint(
            EndpointId::Speak,
            described["manifest"].clone(),
            false,
            None,
        )
        .expect("dry-run");
    assert_eq!(round_trip["changed"], json!([]));

    // 往"位开着才能有的 role"上改：**第 2 步**（`missing_on`）先答，报的是 `missing_capability`
    // 而不是"这一格不能改"——顺序就是设计（拿错能力的清单要拿到准确的错误码）。
    let mut edited = described["manifest"].clone();
    edited["out"][0]["role"] = json!("virtual_mic");
    match backend
        .compose_endpoint(EndpointId::Speak, edited, false, None)
        .expect_err("这台机器没有虚拟麦")
    {
        CallFailure::Domain(error) => {
            assert_eq!(error.code, DomainErrorCode::EndpointUnavailable);
            let detail = error.detail.expect("detail");
            assert_eq!(detail["errors"][0]["kind"], json!("missing_capability"));
            assert_eq!(detail["errors"][0]["bit"], json!("virtual_mic"));
        }
        other => panic!("期望 endpoint_unavailable，拿到 {other:?}"),
    }

    // 位开着的那台机器：同一个端点，role 就该是 virtual_mic；这时候把它改回 speaker 才是
    // "改 `editable` 之外的格"（`speaker` 不需要任何位，过得了第 2 步）。
    let (_runtime, mut wired) = harness(desktop_facts());
    let described = wired
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    assert_eq!(
        described["manifest"]["out"][0]["role"],
        json!("virtual_mic")
    );
    assert!(described.get("notes").is_none(), "没有降级就不给 notes");

    let mut edited = described["manifest"].clone();
    edited["out"][0]["role"] = json!("speaker");
    let detail = unsupported(
        &wired
            .compose_endpoint(EndpointId::Speak, edited, false, None)
            .expect_err("改 role 必须被拒"),
    );
    assert_eq!(detail["path"], json!("out[playback(primary)].role"));
    assert!(
        detail["why"].as_str().expect("why").contains("role"),
        "why 要指到 role 的由来：{detail}"
    );
}

/// 有没有那一格 = 那一条设置：加回听、去掉降噪，都按"存在性"落进账本。
#[test]
fn presence_cells_write_the_matching_setting() {
    let (runtime, mut backend) = harness(desktop_facts());
    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");

    let mut edited = described["manifest"].clone();
    let outputs = edited["out"].as_array_mut().expect("out");
    outputs.insert(
        1,
        json!({ "kind": "playback", "role": "monitor", "device": null, "source": "session" }),
    );
    edited["ops"].as_array_mut().expect("ops").remove(1); // denoise

    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, edited.clone(), false, None)
        .expect("dry-run");
    let paths: Vec<&str> = dry_run["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .map(|change| change["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths, vec!["ops[denoise]", "out[playback(monitor)]"]);

    let token = dry_run["token"].as_str().expect("token").to_string();
    let applied = backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect("apply");
    assert_eq!(applied["applied"], json!(true));
    let settings = runtime.settings();
    assert!(settings.speak.monitor_translation);
    assert!(!settings.speak.denoise);
    assert_eq!(
        applied["manifest"]["out"][1]["role"],
        json!("monitor"),
        "apply 之后账本派生的清单必须与提交的一致：{applied}"
    );
}

/// `ops[aec] 有无` 那一格：清单里有没有这一节 → `settings.speak.aec`（§2.1.4，R13）。
///
/// 与 `ops[denoise]` 那条同性质——写的是**设置**，不是"清单保证"：`pipeline/speak.rs::composition`
/// 在设置之外还有 `soft_aec` 位那道闸（`config.aec && bits.contains(Capability::SoftAec)`），
/// 所以位为假的机器上"设置是真的、清单里仍没有这一节"。这条用例用一位都不关的桌面档
/// （两道闸都开着），才谈得上"清单里有这一节 ⇔ 设置是真的"。
#[test]
fn the_aec_cell_writes_the_setting() {
    /// `ops` 里 `aec` 那一节的下标。**按 `kind` 找，不写死下标**——算子链的节数会变
    /// （W5 就是在 `denoise` 与 `gate` 之间插的这一节），下标会把断言指到别人身上。
    fn aec_index(manifest: &Value) -> Option<usize> {
        manifest["ops"]
            .as_array()?
            .iter()
            .position(|op| op["kind"] == json!("aec"))
    }

    let (runtime, mut backend) = harness(desktop_facts());
    assert!(
        runtime.settings().speak.aec,
        "缺省是开的（`SpeakSettings::default().aec`）"
    );

    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    let with_aec = described["manifest"].clone();
    let index = aec_index(&with_aec).expect("位开 + 设置开 ⇒ 清单里该有 aec 那一节");
    let aec = with_aec["ops"][index].clone();

    // ① 去掉那一节：dry-run 恰好这一条差异，apply 之后设置真的关掉。
    let mut without_aec = with_aec;
    without_aec["ops"]
        .as_array_mut()
        .expect("ops 是数组")
        .remove(index);
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, without_aec.clone(), false, None)
        .expect("dry-run");
    assert_eq!(
        dry_run["changed"],
        json!([{ "path": "ops[aec]", "from": aec, "to": null }]),
        "去掉 `ops[aec]` 就是这一格的一条差异：{dry_run}"
    );
    let token = dry_run["token"].as_str().expect("token").to_string();
    let applied = backend
        .compose_endpoint(EndpointId::Speak, without_aec, true, Some(token))
        .expect("apply");
    assert_eq!(applied["applied"], json!(true));
    assert!(
        !runtime.settings().speak.aec,
        "清单里没有 aec ⇒ `speak.aec = false`"
    );
    assert_eq!(
        aec_index(&applied["manifest"]),
        None,
        "账本重新派生的清单也没有那一节：{applied}"
    );

    // ② 把它照原位加回去：同一个机制的另一面（设置真的开回来）。
    let mut back = applied["manifest"].clone();
    back["ops"]
        .as_array_mut()
        .expect("ops 是数组")
        .insert(index, aec.clone());
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, back.clone(), false, None)
        .expect("dry-run");
    assert_eq!(
        dry_run["changed"],
        json!([{ "path": "ops[aec]", "from": null, "to": aec }]),
        "把 `ops[aec]` 加回去是反方向的一条差异：{dry_run}"
    );
    let token = dry_run["token"].as_str().expect("token").to_string();
    let applied = backend
        .compose_endpoint(EndpointId::Speak, back, true, Some(token))
        .expect("apply");
    assert_eq!(applied["applied"], json!(true));
    assert!(
        runtime.settings().speak.aec,
        "清单里有 aec ⇒ `speak.aec = true`"
    );
}

/// `session_open` 幂等、`session_close` 也幂等，不认识的 handle 报 `unknown_session`。
#[test]
fn session_open_and_close_are_idempotent() {
    let runtime = ledger(desktop_facts());
    runtime.set_control(Arc::new(ReadyControl {
        runtime: runtime.clone(),
    }));
    let mut backend = LedgerBackend::new(runtime.clone(), Granted);

    let opened = backend
        .session_open(EndpointId::Speak, 5_000)
        .expect("open");
    assert_eq!(opened["state"], json!("ready"));
    assert_eq!(opened["endpoint"], json!("speak"));
    let handle = opened["session"].as_str().expect("handle").to_string();
    assert!(handle.starts_with("s_"), "handle 形如 s_…：{handle}");
    assert_eq!(
        opened["transcript"],
        json!(format!("vox://session/{handle}/transcript"))
    );
    assert!(opened["wait_ms"].as_u64().expect("wait_ms") <= 5_000);

    // 幂等：已经在跑 → 同一个 handle（对齐芯的"已经在跑就什么也不做"）。
    let again = backend
        .session_open(EndpointId::Speak, 5_000)
        .expect("open");
    assert_eq!(again["session"], json!(handle));

    let closed = backend.session_close(&handle).expect("close");
    assert_eq!(closed["state"], json!("idle"));
    assert_eq!(closed["stopped"], json!(true));
    assert_eq!(closed["endpoint"], json!("speak"));

    // 再关一次：成功 + stopped:false（幂等，不是错误）。
    let closed_again = backend.session_close(&handle).expect("close");
    assert_eq!(closed_again["stopped"], json!(false));

    // 从来没签发过的 handle → unknown_session。
    match backend.session_close("s_nope").expect_err("未知 handle") {
        CallFailure::Domain(error) => assert_eq!(error.code, DomainErrorCode::UnknownSession),
        other => panic!("期望 unknown_session，拿到 {other:?}"),
    }
}

/// 缺省授权（[`Denied`]）：一位都不开。开麦、改配置都被如实挡住——**fail-closed 不是占位**，
/// 缺省设置里那四格本来就是 `false`。
#[test]
fn the_default_grants_deny_everything() {
    let runtime = ledger(desktop_facts());
    let mut backend = LedgerBackend::new(runtime.clone(), Denied);

    let described = backend
        .describe_endpoint(EndpointId::Speak)
        .expect("describe_endpoint");
    assert_eq!(described["permissions"][0]["user_granted"], json!(false));

    match backend
        .session_open(EndpointId::Speak, 0)
        .expect_err("没授权")
    {
        CallFailure::Domain(error) => {
            assert_eq!(error.code, DomainErrorCode::PermissionDenied);
            assert_eq!(
                error.detail.expect("detail")["permission"],
                json!("microphone")
            );
        }
        other => panic!("期望 permission_denied，拿到 {other:?}"),
    }

    // 只读的 describe 不受影响；写配置才要那一位。
    let mut edited = described["manifest"].clone();
    edited["session"]["params"]["target_language"] = json!("en");
    let dry_run = backend
        .compose_endpoint(EndpointId::Speak, edited.clone(), false, None)
        .expect("dry-run 不写任何东西，不该被授权位挡住");
    let token = dry_run["token"].as_str().expect("token").to_string();
    match backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect_err("没允许改配置")
    {
        CallFailure::Domain(error) => assert_eq!(error.code, DomainErrorCode::ConfigWriteDenied),
        other => panic!("期望 config_write_denied，拿到 {other:?}"),
    }
}

/// 授权位的**真源**：`impl Grants for Runtime` 逐项读 `Settings.control.allow_*`，fail-closed。
///
/// 这条是产品路径的护栏：装配层把 `Runtime` 自己当 `Grants` 交给 `LedgerBackend`，所以
/// "用户在设置里拨一位"到"控制面放行"之间没有第二份真源、也没有需要手工同步的中间层；
/// 最后一段真跑一次 `apply`，证明位开着时写门确实开（而不只是 `describe` 里那个布尔好看）。
#[test]
fn the_runtime_grants_read_the_control_settings() {
    let runtime = ledger(desktop_facts());
    // 缺省全关（老配置文件里没有 `control` 这一段，读出来也是全关）。
    assert!(!Grants::user_granted(&runtime, Permission::Microphone));
    assert!(!Grants::config_write_allowed(&runtime));

    // 总开关开着、只拨麦克风一位：只有那一位通。
    Ledger::update_settings(&runtime, &mut |settings| {
        settings.control.enabled = true;
        settings.control.allow_microphone = true;
    });
    assert!(Grants::user_granted(&runtime, Permission::Microphone));
    assert!(!Grants::user_granted(&runtime, Permission::SystemAudio));
    assert!(!Grants::user_granted(&runtime, Permission::AudibleOutput));
    assert!(!Grants::config_write_allowed(&runtime), "写门没开");

    // 总开关关掉：拨过的位不生效（服务万一还在跑，也已经一位都不开）。
    Ledger::update_settings(&runtime, &mut |settings| settings.control.enabled = false);
    assert!(!Grants::user_granted(&runtime, Permission::Microphone));

    // 四个位全开 → 逐个通，而且**真能走完一次 apply**（真 `Runtime` 自己当 `Grants`）。
    Ledger::update_settings(&runtime, &mut |settings| {
        settings.control.enabled = true;
        settings.control.allow_microphone = true;
        settings.control.allow_system_audio = true;
        settings.control.allow_audible_output = true;
        settings.control.allow_config_write = true;
    });
    for permission in Permission::ALL {
        assert!(
            Grants::user_granted(&runtime, *permission),
            "{} 应该通",
            permission.as_str()
        );
    }
    let mut backend = LedgerBackend::new(runtime.clone(), runtime.clone());
    let (edited, token) = dry_run_edited(&mut backend);
    let applied = backend
        .compose_endpoint(EndpointId::Speak, edited, true, Some(token))
        .expect("位开着就该落进账本");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(runtime.settings().speak.target_language, "en");
}

/// 总闸 `control.enabled` 是**每道闸前面的那道**：位拨着也白拨——写配置与开麦都先过它。
///
/// 两条各钉一位：位本身是 `true`、**只有**总闸是 `false`，所以红了就一定是 `impl Grants for
/// Runtime` 里 `control.enabled && …` 那个 `&&` 丢了（位本身那一格另有用例钉着）。第二条是
/// **真走一遍 `session_open`**，不是只看 trait 方法：总闸关着 → `permission_denied`，打开 →
/// 同一个进程、同一份设置立刻 `ready`——那一跳同时证明总闸不是摆设。
#[test]
fn the_control_master_switch_gates_every_grant_bit() {
    let runtime = ledger(desktop_facts());
    runtime.set_control(Arc::new(ReadyControl {
        runtime: runtime.clone(),
    }));
    let mut backend = LedgerBackend::new(runtime.clone(), runtime.clone());

    // 四个位全拨上，**只关总闸**。
    Ledger::update_settings(&runtime, &mut |settings| {
        settings.control.enabled = false;
        settings.control.allow_microphone = true;
        settings.control.allow_system_audio = true;
        settings.control.allow_audible_output = true;
        settings.control.allow_config_write = true;
    });

    // ① 写门：`allow_config_write` 是开的，但总闸关着 → 不许写。
    assert!(
        !Grants::config_write_allowed(&runtime),
        "总闸关着时写门必须关（`enabled` 在 `allow_config_write` 前面）"
    );

    // ② 设备位：`allow_microphone` 是开的，但总闸关着 → 会话起不来，走的是授权失败。
    match backend
        .session_open(EndpointId::Speak, 0)
        .expect_err("总闸关着就想开麦")
    {
        CallFailure::Domain(error) => {
            assert_eq!(error.code, DomainErrorCode::PermissionDenied);
            assert_eq!(
                error.detail.expect("detail")["permission"],
                json!("microphone")
            );
        }
        other => panic!("期望 permission_denied，拿到 {other:?}"),
    }

    // 对照：同一个进程、同一份设置，**只把总闸打开**，两道闸立刻都放行。
    Ledger::update_settings(&runtime, &mut |settings| settings.control.enabled = true);
    assert!(Grants::config_write_allowed(&runtime));
    let opened = backend
        .session_open(EndpointId::Speak, 5_000)
        .expect("总闸开了就该开得起来");
    assert_eq!(opened["state"], json!("ready"));
}

/// `list_endpoints`：两个端点、各自的 `available` / `running` / `summary`，档位与通道来自事实
/// 与清单（不是另立的一张表）。
#[test]
fn list_endpoints_reports_both_legs_and_the_control_channels() {
    let (_runtime, mut backend) = harness(desktop_facts());
    // 端点目录 ↔ `EndpointId::ALL` 配对（编译器证不了配对，这里钉住：漏一个端点会在这里红）。
    assert_eq!(
        ENDPOINTS
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<EndpointId>>(),
        EndpointId::ALL.to_vec()
    );

    let listed = backend.list_endpoints().expect("list_endpoints");

    assert_eq!(listed["device"]["tier"], json!("windows"));
    assert_eq!(
        listed["device"]["control"],
        json!(["inproc_api", "ipc"]),
        "控制面通道取清单自己那一格"
    );
    let endpoints = listed["endpoints"].as_array().expect("endpoints");
    assert_eq!(endpoints.len(), 2);
    assert_eq!(endpoints[0]["id"], json!("speak"));
    assert_eq!(endpoints[0]["title"], json!("对外说话"));
    assert_eq!(endpoints[0]["available"], json!(true));
    assert_eq!(endpoints[0]["running"], json!(false));
    assert!(endpoints[0].get("unavailable_reason").is_none());
    assert_eq!(endpoints[1]["id"], json!("listen"));
    assert_eq!(endpoints[1]["available"], json!(true));

    // 抓不了程序的机器：listen 报 false + 原因（`program_tap` 那一位）。
    let (_runtime, mut backend) = harness(HostFacts {
        host: HostKind::Windows,
        off: BTreeMap::from([(Capability::ProgramTap, UnavailableReason::Unsupported)]),
        virtual_mic_device: None,
    });
    let listed = backend.list_endpoints().expect("list_endpoints");
    let listen = &listed["endpoints"][1];
    assert_eq!(listen["available"], json!(false));
    assert!(
        listen["unavailable_reason"]
            .as_str()
            .expect("reason")
            .contains("program_tap"),
        "原因要指到位：{listen}"
    );

    // 还没选要抓的程序：listen 连清单都派不出来 → 照实说，不编一个 available。
    let runtime = Runtime::new(Settings::default(), Arc::new(TestClock::default()));
    runtime.set_host_facts(desktop_facts());
    let mut backend = LedgerBackend::new(runtime, Granted);
    let listed = backend.list_endpoints().expect("list_endpoints");
    assert_eq!(listed["endpoints"][1]["available"], json!(false));
    assert!(listed["endpoints"][1]["unavailable_reason"]
        .as_str()
        .expect("reason")
        .contains("program"));
    match backend
        .describe_endpoint(EndpointId::Listen)
        .expect_err("没选程序就派不出清单")
    {
        CallFailure::Domain(error) => assert_eq!(error.code, DomainErrorCode::EndpointUnavailable),
        other => panic!("期望 endpoint_unavailable，拿到 {other:?}"),
    }
}

/// 端口就是账本：`Ledger` 的每一格都能从真 `Runtime` 答出来（这层转发没有自己的状态）。
#[test]
fn the_ledger_port_forwards_to_the_core_runtime() {
    let runtime = ledger(desktop_facts());
    assert_eq!(Ledger::settings(&runtime).speak.target_language, "ja");
    assert_eq!(Ledger::host_facts(&runtime), desktop_facts());
    assert_eq!(Ledger::capabilities(&runtime).tier, HostKind::Windows);
    // 端口那一格给的就是芯 `Runtime::session_config` 的同一份（`EndpointId` → `Pipeline` 是
    // 唯一多出来的一步，没有第二份映射）。
    assert_eq!(
        Ledger::session_config(&runtime, EndpointId::Speak),
        runtime.session_config(Pipeline::Speak)
    );
    assert_eq!(
        Ledger::pipeline_state(&runtime, EndpointId::Speak),
        PipelineState::Idle
    );
    assert_eq!(Ledger::pipeline_error(&runtime, EndpointId::Speak), None);
    assert_eq!(Ledger::now_ms(&runtime), 0);
    assert!(Ledger::audio_apps(&runtime).is_empty());
    // 写路径也走同一处：改一格真的落进账本。
    Ledger::update_settings(&runtime, &mut |settings| settings.speak.translate = false);
    assert!(!Ledger::settings(&runtime).speak.translate);
}

// --- 清单文档（两个 `--print-composition` 入口共用的那一份组装） --------------------

/// 形状契约（S0 §4.3-A）：四个顶层键一个不少、两条腿都打、派得出来的腿就是 [`wire`] 那一份。
///
/// 这份 JSON 就是无屏档与桌面档的 `--print-composition` 打到 stdout 的那一份——两个入口都调
/// [`document`]，所以这里钉住的就是那两条命令的输出形状。
#[test]
fn the_document_carries_the_bits_and_both_legs() {
    let mut reported = Vec::new();
    let json = endpoints::document(&ledger(desktop_facts()), &mut |endpoint, error| {
        reported.push((endpoint, error.code))
    })
    .expect("清单该能序列化");
    let document: Value = serde_json::from_str(&json).expect("合法 JSON");

    let keys: BTreeSet<&str> = document
        .as_object()
        .expect("顶层是对象")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["capabilities", "errors", "listen", "speak"])
    );

    // 位是**当前有效**的那一份（档位上限 − 关掉的），不是档位表。
    assert_eq!(document["capabilities"]["tier"], json!("windows"));
    // 两条腿都是真清单：说话走麦克风，听人说话抓那个程序（线上形态，`f32` 不摊成 `f64`）。
    assert_eq!(document["speak"]["in"][0]["kind"], json!("mic"));
    assert_eq!(
        document["listen"]["in"][0]["kind"],
        json!("process_loopback")
    );
    assert_eq!(document["listen"]["ops"][0]["kind"], json!("mono"));

    // 两条腿都派得出来 → 没有理由要报，日志闭包一次都不响。
    assert_eq!(document["errors"].as_array().map(Vec::len), Some(0));
    assert!(
        reported.is_empty(),
        "两条腿都派得出来，不该报失败：{reported:?}"
    );
}

/// 派不出来的那条腿：键还在、值是 `null`、理由进 `errors`（**不**打一份看起来像清单的假清单），
/// 同一份失败也逐条过给入口——`--print-composition` 的那行日志就是它。
#[test]
fn a_leg_that_cannot_be_composed_is_null_with_a_reason() {
    // 缺省设置：还没选要抓的程序 → `listen` 派不出来，`speak` 那条腿照样打得出来。
    let runtime = Runtime::new(Settings::default(), Arc::new(TestClock::default()));
    runtime.set_host_facts(desktop_facts());

    let mut reported = Vec::new();
    let json = endpoints::document(&runtime, &mut |endpoint, error| {
        reported.push((endpoint, error.code))
    })
    .expect("清单该能序列化");
    let document: Value = serde_json::from_str(&json).expect("合法 JSON");

    let keys: BTreeSet<&str> = document
        .as_object()
        .expect("顶层是对象")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["capabilities", "errors", "listen", "speak"])
    );
    assert!(
        document["speak"].is_object(),
        "派得出来的腿照打：{document}"
    );
    assert!(document["listen"].is_null(), "{document}");

    let errors = document["errors"].as_array().expect("errors 是数组");
    assert_eq!(errors.len(), 1, "{document}");
    assert_eq!(errors[0]["endpoint"], json!("listen"));
    assert_eq!(errors[0]["code"], json!("endpoint_unavailable"));
    assert!(errors[0]["message"].is_string(), "{errors:?}");
    assert_eq!(
        reported,
        vec![(EndpointId::Listen, DomainErrorCode::EndpointUnavailable)],
        "入口拿到的就是 JSON 里那一份失败（同一个 `manifest` 的错）"
    );
}
