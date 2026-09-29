//! 无屏入口的端到端契约：**真起进程**跑一遍，验收看的就是这几条。
//!
//! 为什么单测不够：`--print-capabilities` / `--dry-run` / 常驻模式这三条都是"进程行为"
//! （退出码、stdout 上打什么、建不建目录、写不写盘、端口起没起），单元测试碰不到这些。
//!
//! 只在 Linux 上跑：这个二进制要 `vox-audio-linux`（PipeWire），别的宿主上装配本来就
//! 该明确失败（`platform/other.rs`）。
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_vox-headless");

/// 每个用例一个独立目录（用例并行跑，共用一个目录会互相踩）。
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vb-headless-e2e-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

/// 跑一次就退的模式（`--print-capabilities` / `--dry-run`）。
fn run_once(dir: &Path, flag: &str) -> (std::process::ExitStatus, String, String) {
    let output = Command::new(BIN)
        .arg("--config")
        .arg(settings_path(dir))
        .arg(flag)
        .output()
        .expect("跑得起来这个二进制");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 验收第一条：本机能跑，且打出来的能力报告符合 `linux_headless` 档。
#[test]
fn print_capabilities_reports_the_headless_tier() {
    let dir = temp_dir("print");
    let (status, stdout, stderr) = run_once(&dir, "--print-capabilities");
    assert!(status.success(), "退出码该是 0：{status}；stderr：{stderr}");

    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout 应该是 JSON：{error}\n{stdout}"));
    assert_eq!(report["tier"], "linux_headless");
    // 上限里那两位：`mic` 按上限报开（装配期没有否定证据），`background_service` 还没接线。
    assert_eq!(report["host"]["mic"]["enabled"], true);
    assert_eq!(report["host"]["background_service"]["reason"], "not_wired");
    // 上限之外的那些：无屏设备没有屏/键盘/托盘宿主，也不给别的程序当麦克风。
    for bit in [
        "captions",
        "tray",
        "global_hotkey",
        "virtual_mic",
        "program_tap",
    ] {
        assert_eq!(report["host"][bit]["enabled"], false, "{bit}");
        assert_eq!(report["host"][bit]["reason"], "unsupported", "{bit}");
    }
    // 报告**没起控制面**（缺省 `control.enabled = false`，fail-closed）。
    assert!(!dir.join("control.json").exists(), "开关关着不该写握手文件");

    let _ = fs::remove_dir_all(&dir);
}

/// 验收第一条（S0 §4.3-A）：`--print-composition` 打**两份清单 + 有效能力位**，退出 0 且不写盘。
///
/// 清单是**本机派生**的（`Composition::of` + 这一档的事实），所以这份 stdout 同时证明了三件事：
/// 档位是 `linux_headless`、清单里位为假的格子被关掉了（`virtual_mic` → 角色退成 `speaker`）、
/// 以及"听人说话"在这一档里 `in` 是空的（`program_tap` 在上限之外）。
#[test]
fn print_composition_prints_the_two_manifests() {
    let dir = temp_dir("composition");
    fs::write(
        settings_path(&dir),
        r#"{"listen":{"target":{"executable":"Discord","display_name":"Discord"}}}"#,
    )
    .expect("写设置");

    let (status, stdout, stderr) = run_once(&dir, "--print-composition");
    assert!(status.success(), "退出码该是 0：{status}；stderr：{stderr}");
    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout 应该是 JSON：{error}\n{stdout}"));

    assert_eq!(document["capabilities"]["tier"], "linux_headless");
    // 对外说话：麦克风进、译文出声（无屏档没有虚拟麦位 → 角色退成普通出声）。
    assert_eq!(document["speak"]["in"][0]["kind"], "mic");
    assert_eq!(document["speak"]["out"][0]["role"], "speaker");
    assert_eq!(document["speak"]["session"]["provider"], "aliyun");
    // 听人说话：也打得出来，但**输入是空的**——这一档不本机抓程序（S3 的 `net_in` 还没实现）。
    assert_eq!(document["listen"]["in"].as_array().map(Vec::len), Some(0));
    assert_eq!(document["listen"]["ops"][0]["kind"], "mono");
    assert_eq!(document["listen"]["session"]["provider"], "aliyun");
    assert_eq!(document["errors"].as_array().map(Vec::len), Some(0));

    // 跟 `--dry-run` 一样不写盘：目录里只有用例自己写的那一份设置。
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .expect("读临时目录")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().to_string()))
        .collect();
    assert_eq!(
        leftovers,
        vec!["settings.json".to_string()],
        "打印清单不该动盘"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// 没选要抓的程序时，`listen` 那一格是 `null` + 一句为什么：**不许**打一份看起来像清单的
/// 假清单（无屏设备上没人能核对，那正是"广告了做不到的事"的另一种形态）。
#[test]
fn print_composition_says_why_a_leg_is_null_instead_of_faking_one() {
    let dir = temp_dir("composition-null");
    let (status, stdout, stderr) = run_once(&dir, "--print-composition");
    assert!(status.success(), "退出码该是 0：{status}；stderr：{stderr}");

    let document: serde_json::Value = serde_json::from_str(&stdout).expect("stdout 应该是 JSON");
    assert!(document["speak"].is_object(), "{document}");
    assert!(document["listen"].is_null(), "{document}");
    assert_eq!(document["errors"][0]["endpoint"], "listen");
    assert_eq!(document["errors"][0]["code"], "endpoint_unavailable");

    let _ = fs::remove_dir_all(&dir);
}

/// 验收第二条的"只走装配"：`--dry-run` 不监听、不碰 PipeWire、**不建配置目录**、不写盘——
/// `--config` 指到一个还不存在的目录，跑完那个目录**还是不存在**（报告命令不该留副作用）。
#[test]
fn dry_run_assembles_without_side_effects() {
    let parent = temp_dir("dry-run");
    let dir = parent.join("not-yet");
    let (status, stdout, stderr) = run_once(&dir, "--dry-run");
    assert!(status.success(), "退出码该是 0：{status}；stderr：{stderr}");
    assert!(
        stderr.contains("试装完成"),
        "试装结论要打在日志里：{stderr}"
    );

    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout 应该是 JSON：{error}\n{stdout}"));
    assert_eq!(report["tier"], "linux_headless");

    assert!(
        !dir.exists(),
        "试装不该建配置目录，更不该往里写东西：{}",
        dir.display()
    );

    let _ = fs::remove_dir_all(&parent);
}

/// 第 7 条：`--print-composition` 是**只读**的——`--config` 指到一个还不存在的目录时，
/// 打一份 JSON 就走，**不许**把那个目录建出来（一份打印命令不该在别人机器上留副作用）。
#[test]
fn print_composition_does_not_create_the_config_directory() {
    let parent = temp_dir("composition-readonly");
    let dir = parent.join("not-yet");
    let (status, stdout, stderr) = run_once(&dir, "--print-composition");
    assert!(status.success(), "退出码该是 0：{status}；stderr：{stderr}");

    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout 应该是 JSON：{error}\n{stdout}"));
    assert_eq!(document["capabilities"]["tier"], "linux_headless");
    assert!(!dir.exists(), "打印清单不该建配置目录：{}", dir.display());

    let _ = fs::remove_dir_all(&parent);
}

/// 验收第三条的冒烟：常驻模式真跑起来、控制面按开关起来、收摊把凭据带走。
///
/// 顺带证明"起/停"这条命令路径是通的：`--run-for 2` 到点自己收摊（不是被 kill 掉的，
/// 所以握手文件是**正常路径**清掉的，不是靠下次启动清理）。
#[test]
fn the_daemon_serves_the_control_plane_and_cleans_up_on_exit() {
    let dir = temp_dir("daemon");
    // 控制面开关开着（用户明确打开；缺省是关的）。
    fs::write(
        settings_path(&dir),
        r#"{"control":{"enabled":true,"port":0}}"#,
    )
    .expect("写设置");

    let child: Child = Command::new(BIN)
        .arg("--config")
        .arg(settings_path(&dir))
        .arg("--run-for")
        .arg("2")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("起常驻进程");

    let state_file = dir.join("control.json");
    let deadline = Instant::now() + Duration::from_secs(4);
    while !state_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(state_file.exists(), "开关开着就该写握手文件（等超时了）");

    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).expect("读握手文件"))
            .expect("握手文件是 JSON");
    assert_eq!(
        document["pid"].as_u64(),
        Some(u64::from(child.id())),
        "凭据记的是这个进程"
    );
    assert!(
        document["port"].as_u64().is_some_and(|port| port > 0),
        "端口该是真绑上的那个：{document}"
    );
    assert_eq!(
        document["token"].as_str().map(str::len),
        Some(43),
        "token 是 32 字节 base64url"
    );

    let output = child.wait_with_output().expect("等它退");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "收摊该是正常退出：{stderr}");
    assert!(stderr.contains("已收摊"), "收摊要有日志：{stderr}");
    assert!(
        !state_file.exists(),
        "正常退出该把握手文件收走（下次启动才不用清死凭据）"
    );

    let _ = fs::remove_dir_all(&dir);
}
