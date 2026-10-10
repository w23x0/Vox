//! 音频会话枚举：现在有哪些软件在出声。
//!
//! 用途有两个：
//! 1. 给 UI 列“听谁说话”的候选（`audio_apps`）；
//! 2. 进程环回开始前判断同名进程里哪个真在出声（`session_hints`）。
//!
//! PID → exe 名字走进程快照而不是 `OpenProcess`：快照拿名字不需要任何权限，
//! 而 `OpenProcess` 对某些受保护进程会失败，那样列表里就会莫名少几个软件。

use std::collections::HashMap;

use vox_core::ports::{AudioApp, PortResult};
use windows::core::Interface;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};

use crate::com::{wide_to_string, WinContext};
use crate::devices;
use crate::proc::{self, ProcessEntry, SessionHint};

/// 一条会话记录（内部用，比 `AudioApp` 多一点东西）。
#[derive(Debug, Clone)]
pub(crate) struct SessionRecord {
    pub(crate) pid: u32,
    pub(crate) display_name: String,
    pub(crate) active: bool,
}

/// 枚举所有输出端点上的会话。
///
/// 为什么要遍历所有端点而不只是默认端点：用户可能把游戏声音单独指到 HDMI，
/// 只看默认端点会漏掉。
fn enumerate_sessions() -> PortResult<Vec<SessionRecord>> {
    let enumerator = devices::enumerator()?;
    let mut records = Vec::new();
    // SAFETY: enumerator 有效；下面所有接口都由 windows-rs 管引用计数，
    // 手工释放的只有 GetDisplayName 返回的 COM 字符串。
    unsafe {
        let collection = enumerator
            .EnumAudioEndpoints(devices::RENDER, DEVICE_STATE_ACTIVE)
            .ctx("failed to enumerate output devices")?;
        let count = collection
            .GetCount()
            .ctx("failed to read the device count")?;
        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                continue;
            };
            collect_device_sessions(&device, &mut records);
        }
    }
    Ok(records)
}

/// 只枚举 VB-CABLE 两端上的会话，用于卸载前提示哪些应用还占着设备。
pub(crate) fn cable_sessions() -> PortResult<Vec<AudioApp>> {
    use windows::Win32::Media::Audio::{eCapture, eRender};

    let enumerator = devices::enumerator()?;
    let mut records = Vec::new();
    for flow in [eRender, eCapture] {
        // SAFETY: enumerator 有效；只枚举当前活动端点。
        let collection = unsafe {
            enumerator
                .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
                .ctx("failed to enumerate virtual cable endpoints")?
        };
        // SAFETY: collection 有效。
        let count = unsafe {
            collection
                .GetCount()
                .ctx("failed to read the virtual cable endpoint count")?
        };
        for index in 0..count {
            // SAFETY: index 小于 count。
            let Ok(device) = (unsafe { collection.Item(index) }) else {
                continue;
            };
            let Ok(name) = devices::friendly_name(&device) else {
                continue;
            };
            let is_cable = if flow == eRender {
                crate::cable::is_cable_render(&name)
            } else {
                crate::cable::is_cable_capture(&name)
            };
            if is_cable {
                // SAFETY: device 来自有效集合。
                unsafe { collect_device_sessions(&device, &mut records) };
            }
        }
    }

    let processes = proc::snapshot_processes().unwrap_or_default();
    let names: HashMap<u32, String> = processes
        .iter()
        .map(|process| (process.pid, process.name.clone()))
        .collect();
    let me = std::process::id();
    let mut by_pid: HashMap<u32, AudioApp> = HashMap::new();
    for record in records {
        if record.pid == 0 || record.pid == me {
            continue;
        }
        let Some(executable) = names.get(&record.pid) else {
            continue;
        };
        let display_name = if record.display_name.trim().is_empty() {
            pretty_name(executable)
        } else {
            record.display_name.clone()
        };
        by_pid
            .entry(record.pid)
            .and_modify(|app| app.active |= record.active)
            .or_insert(AudioApp {
                executable: executable.clone(),
                display_name,
                pid: record.pid,
                active: record.active,
            });
    }
    let mut apps: Vec<_> = by_pid.into_values().collect();
    apps.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    Ok(apps)
}

/// 收集单个端点上的会话。单个端点出问题就跳过，不影响其它端点。
///
/// # Safety
/// `device` 必须是有效的 `IMMDevice`，且调用线程已初始化 COM。
unsafe fn collect_device_sessions(device: &IMMDevice, out: &mut Vec<SessionRecord>) {
    // SAFETY: 由调用方保证 device 有效；Activate 拿会话管理器，失败就直接返回。
    let manager: IAudioSessionManager2 = match unsafe { device.Activate(CLSCTX_ALL, None) } {
        Ok(m) => m,
        Err(_) => return,
    };
    // SAFETY: manager 有效。
    let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
        return;
    };
    // SAFETY: sessions 有效。
    let Ok(count) = (unsafe { sessions.GetCount() }) else {
        return;
    };
    for i in 0..count {
        // SAFETY: 下标在 [0, count) 内。
        let Ok(control) = (unsafe { sessions.GetSession(i) }) else {
            continue;
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        // SAFETY: control2 有效；IsSystemSoundsSession 返回裸 HRESULT，
        // S_OK 才表示“这是系统提示音会话”（S_FALSE 表示不是）。
        let is_system_sounds = unsafe { control2.IsSystemSoundsSession() } == S_OK;
        if is_system_sounds {
            continue;
        }
        // SAFETY: control2 有效。
        let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        // SAFETY: control 有效；GetState 只读状态。
        let active = matches!(unsafe { control.GetState() }, Ok(s) if s == AudioSessionStateActive);
        // SAFETY: control 有效；返回的宽字符串是 COM 分配的，读完立刻释放。
        let display_name = unsafe {
            match control.GetDisplayName() {
                Ok(raw) => {
                    let s = wide_to_string(raw.0);
                    CoTaskMemFree(Some(raw.0 as *const _));
                    s
                }
                Err(_) => String::new(),
            }
        };
        out.push(SessionRecord {
            pid,
            display_name,
            active,
        });
    }
}

/// 给进程环回用的会话提示：同名进程里谁有会话、谁在出声。
pub(crate) fn session_hints(candidates: &[ProcessEntry]) -> Vec<SessionHint> {
    let Ok(records) = enumerate_sessions() else {
        return Vec::new();
    };
    let mut hints: HashMap<u32, SessionHint> = HashMap::new();
    for r in records {
        if !candidates.iter().any(|c| c.pid == r.pid) {
            continue;
        }
        let entry = hints.entry(r.pid).or_insert(SessionHint {
            pid: r.pid,
            has_session: true,
            active: false,
        });
        entry.has_session = true;
        // 同一个进程可能有多个会话，只要有一个在出声就算在出声。
        entry.active |= r.active;
    }
    hints.into_values().collect()
}

/// 列出正在出声（或曾经出过声）的软件，按 exe 去重。
pub(crate) fn audio_apps() -> PortResult<Vec<AudioApp>> {
    let records = enumerate_sessions()?;
    let processes = proc::snapshot_processes().unwrap_or_default();
    let names: HashMap<u32, String> = processes.iter().map(|p| (p.pid, p.name.clone())).collect();
    let me = std::process::id();

    // 按 exe 名合并：Chrome 那种十几个进程的，用户只想看到一个“Chrome”。
    let mut merged: HashMap<String, AudioApp> = HashMap::new();
    for r in records {
        if r.pid == me {
            // 自己不列出来。用户要是选了自己，译文会绕回自己形成回路。
            continue;
        }
        let Some(exe) = names.get(&r.pid) else {
            // 会话还在但进程已经退了（拖尾会话），跳过。
            continue;
        };
        let key = exe.to_lowercase();
        let display = if r.display_name.trim().is_empty() {
            pretty_name(exe)
        } else {
            r.display_name.clone()
        };
        match merged.get_mut(&key) {
            Some(existing) => {
                existing.active |= r.active;
                // 选出声的那个 PID 展示，方便用户对上任务管理器。
                if r.active {
                    existing.pid = r.pid;
                }
            }
            None => {
                merged.insert(
                    key,
                    AudioApp {
                        executable: exe.clone(),
                        display_name: display,
                        pid: r.pid,
                        active: r.active,
                    },
                );
            }
        }
    }

    let mut apps: Vec<AudioApp> = merged.into_values().collect();
    // 在出声的排前面，其余按名字，列表顺序才稳定。
    apps.sort_by(|a, b| {
        b.active.cmp(&a.active).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    Ok(apps)
}

/// exe 名转个人样：`Discord.exe` → `Discord`。
///
/// 会话自己没填显示名的时候用（大多数游戏都不填）。
pub(crate) fn pretty_name(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").unwrap_or(exe);
    let stem = stem.strip_suffix(".EXE").unwrap_or(stem);
    if stem.is_empty() {
        exe.to_string()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ComGuard;

    #[test]
    fn pretty_names_strip_exe_suffix() {
        assert_eq!(pretty_name("Discord.exe"), "Discord");
        assert_eq!(pretty_name("VRChat.EXE"), "VRChat");
        assert_eq!(pretty_name("weird"), "weird");
        assert_eq!(pretty_name(".exe"), ".exe");
    }

    #[test]
    fn enumerating_real_sessions_does_not_error() {
        let _com = ComGuard::mta().unwrap();
        let apps = audio_apps().unwrap();
        for app in &apps {
            assert!(!app.executable.is_empty());
            assert!(!app.display_name.is_empty());
            assert_ne!(app.pid, 0);
            assert_ne!(app.pid, std::process::id());
        }
        // 去重：同一个 exe 不该出现两次。
        let mut keys: Vec<String> = apps.iter().map(|a| a.executable.to_lowercase()).collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len());
    }

    #[test]
    fn hints_only_cover_requested_candidates() {
        let _com = ComGuard::mta().unwrap();
        let hints = session_hints(&[]);
        assert!(hints.is_empty());
    }
}
