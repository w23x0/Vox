//! 进程快照与“exe 名字 → 该抓哪个 PID”的选取策略。
//!
//! 背景：内核里存的目标是 `Discord.exe` 这种可执行文件名（存 PID 没意义，
//! 用户重启一下软件就失效了），所以每次开始抓之前都要现场解析成 PID。
//!
//! 难点在于现代软件是多进程的：Discord / Chrome / VRChat 都是一个主进程带一堆子进程，
//! 真正出声的往往是某个子进程（Chrome 是 audio service，Discord 是 renderer）。
//! 策略见 `choose_target_pid`。

use std::collections::HashMap;

use vox_core::ports::PortResult;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::com::{wide_to_string, WinContext};

/// 快照里的一个进程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessEntry {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    /// 不带路径的可执行文件名，例如 `Discord.exe`。
    pub(crate) name: String,
}

/// 拍一张当前进程快照。
pub(crate) fn snapshot_processes() -> PortResult<Vec<ProcessEntry>> {
    // SAFETY: 快照句柄拿到后立刻由下面的作用域负责关闭；失败时返回 INVALID_HANDLE_VALUE，
    // 由 `ctx` 转成带 HRESULT 的错误。
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .ctx("failed to snapshot the process list")?;

    let mut entries = Vec::new();
    let mut pe = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: dwSize 已按文档填好；Process32FirstW/NextW 只写这一个结构体，
    // 全程持有有效快照句柄，遍历结束后关闭。
    unsafe {
        if Process32FirstW(snapshot, &mut pe).is_ok() {
            loop {
                let name = wide_to_string(pe.szExeFile.as_ptr());
                entries.push(ProcessEntry {
                    pid: pe.th32ProcessID,
                    parent_pid: pe.th32ParentProcessID,
                    name,
                });
                if Process32NextW(snapshot, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(entries)
}

/// 名字是否匹配（大小写不敏感，允许调用方传带不带 .exe 的写法）。
pub(crate) fn name_matches(entry_name: &str, wanted: &str) -> bool {
    let a = entry_name.to_lowercase();
    let b = wanted.trim().to_lowercase();
    if b.is_empty() {
        return false;
    }
    // 传进来的可能是完整路径，只比文件名部分。
    let b = b.rsplit(['\\', '/']).next().unwrap_or(&b);
    if a == b {
        return true;
    }
    // `Discord` 和 `Discord.exe` 都认。
    let a_stem = a.strip_suffix(".exe").unwrap_or(&a);
    let b_stem = b.strip_suffix(".exe").unwrap_or(b);
    a_stem == b_stem
}

/// 一个候选进程 + 它的音频会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionHint {
    pub(crate) pid: u32,
    /// 这个 PID 现在有音频会话。
    pub(crate) has_session: bool,
    /// 会话处于 Active（真在出声），不只是挂着。
    pub(crate) active: bool,
}

/// 挑出该交给进程环回的 PID。
///
/// 规则（`include_tree = true`，也就是默认的“连子进程一起抓”）：
/// 1. 在同名进程里找有音频会话的，优先 Active 的；
/// 2. 从它顺着父进程往上爬，只要父进程还是同一个 exe 名就继续爬，爬到最顶那个；
/// 3. 抓这个根进程 + 整棵树，这样多个子进程同时出声也不会漏。
///
/// `include_tree = false` 时不爬，直接抓正在出声的那个子进程——
/// 因为此时只抓单个进程，抓主进程往往一点声音都没有。
///
/// 一个音频会话都找不到（软件开着但没出过声）就退到根进程，让调用方记一条日志。
pub(crate) fn choose_target_pid(
    candidates: &[ProcessEntry],
    hints: &[SessionHint],
    include_tree: bool,
) -> Option<u32> {
    if candidates.is_empty() {
        return None;
    }
    let hint_of: HashMap<u32, SessionHint> = hints.iter().map(|h| (h.pid, *h)).collect();

    // 会话优先级：Active > 有会话 > 没会话；同级里 PID 小的先来（启动得早，更可能是主进程）。
    let mut ranked: Vec<&ProcessEntry> = candidates.iter().collect();
    ranked.sort_by_key(|e| {
        let rank = match hint_of.get(&e.pid) {
            Some(h) if h.active => 0,
            Some(h) if h.has_session => 1,
            _ => 2,
        };
        (rank, e.pid)
    });
    let picked = ranked.first()?;
    let has_any_session = hint_of
        .get(&picked.pid)
        .map(|h| h.has_session || h.active)
        .unwrap_or(false);

    if include_tree || !has_any_session {
        Some(climb_to_root(picked.pid, candidates))
    } else {
        Some(picked.pid)
    }
}

/// 顺着父进程往上爬，直到父进程不再是同名进程。
///
/// `candidates` 只包含同名进程，所以“父进程在表里”就等于“父进程是同一个软件”。
/// 带环保护：进程表理论上不该有环，但 PID 回卷时可能出现自指。
pub(crate) fn climb_to_root(start: u32, candidates: &[ProcessEntry]) -> u32 {
    let by_pid: HashMap<u32, &ProcessEntry> = candidates.iter().map(|e| (e.pid, e)).collect();
    let mut current = start;
    let mut hops = 0;
    while hops < 32 {
        let Some(entry) = by_pid.get(&current) else {
            break;
        };
        let parent = entry.parent_pid;
        if parent == 0 || parent == current || !by_pid.contains_key(&parent) {
            break;
        }
        current = parent;
        hops += 1;
    }
    current
}

/// 找出所有同名进程。
pub(crate) fn matching_processes(all: &[ProcessEntry], executable: &str) -> Vec<ProcessEntry> {
    all.iter()
        .filter(|e| name_matches(&e.name, executable))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, name: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent_pid: parent,
            name: name.to_string(),
        }
    }

    #[test]
    fn name_matching_is_case_and_suffix_tolerant() {
        assert!(name_matches("Discord.exe", "discord.exe"));
        assert!(name_matches("Discord.exe", "Discord"));
        assert!(name_matches("discord.exe", "C:\\Users\\a\\Discord.exe"));
        assert!(!name_matches("Discord.exe", "chrome.exe"));
        assert!(!name_matches("Discord.exe", ""));
    }

    #[test]
    fn climbs_to_the_topmost_same_name_process() {
        // 4 是 renderer，父 2 是 GPU 进程，父 1 是主进程，1 的父 900 是 explorer（不在表里）。
        let procs = vec![
            p(1, 900, "Discord.exe"),
            p(2, 1, "Discord.exe"),
            p(4, 2, "Discord.exe"),
        ];
        assert_eq!(climb_to_root(4, &procs), 1);
        assert_eq!(climb_to_root(1, &procs), 1);
    }

    #[test]
    fn climb_survives_self_referencing_entry() {
        let procs = vec![p(7, 7, "weird.exe")];
        assert_eq!(climb_to_root(7, &procs), 7);
    }

    #[test]
    fn include_tree_targets_the_root_even_when_child_makes_sound() {
        let procs = vec![
            p(1, 900, "chrome.exe"),
            p(2, 1, "chrome.exe"),
            p(3, 1, "chrome.exe"),
        ];
        let hints = vec![SessionHint {
            pid: 3,
            has_session: true,
            active: true,
        }];
        assert_eq!(choose_target_pid(&procs, &hints, true), Some(1));
    }

    #[test]
    fn without_tree_targets_the_process_actually_playing() {
        let procs = vec![
            p(1, 900, "chrome.exe"),
            p(2, 1, "chrome.exe"),
            p(3, 1, "chrome.exe"),
        ];
        let hints = vec![SessionHint {
            pid: 3,
            has_session: true,
            active: true,
        }];
        assert_eq!(choose_target_pid(&procs, &hints, false), Some(3));
    }

    #[test]
    fn active_session_beats_idle_session() {
        let procs = vec![p(10, 900, "app.exe"), p(11, 900, "app.exe")];
        let hints = vec![
            SessionHint {
                pid: 10,
                has_session: true,
                active: false,
            },
            SessionHint {
                pid: 11,
                has_session: true,
                active: true,
            },
        ];
        assert_eq!(choose_target_pid(&procs, &hints, false), Some(11));
    }

    #[test]
    fn no_session_falls_back_to_root_process() {
        let procs = vec![p(5, 900, "vrchat.exe"), p(6, 5, "vrchat.exe")];
        // 没有任何会话提示：即使 include_tree=false 也退到根进程，
        // 因为此时无从判断谁会出声，抓根 + 树最保险。
        assert_eq!(choose_target_pid(&procs, &[], false), Some(5));
    }

    #[test]
    fn empty_candidates_gives_none() {
        assert_eq!(choose_target_pid(&[], &[], true), None);
    }

    #[test]
    fn matching_filters_by_name() {
        let all = vec![
            p(1, 0, "explorer.exe"),
            p(2, 0, "Discord.exe"),
            p(3, 2, "discord.exe"),
        ];
        let m = matching_processes(&all, "Discord");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].pid, 2);
    }

    #[test]
    fn real_snapshot_contains_this_process() {
        let procs = snapshot_processes().unwrap();
        let me = std::process::id();
        assert!(procs.iter().any(|p| p.pid == me), "快照里没有当前进程 {me}");
        assert!(procs.iter().all(|p| !p.name.is_empty()) || procs.len() > 1);
    }
}
