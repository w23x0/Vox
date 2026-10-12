//! `DeviceRegistry` 的 Linux 实现。
//!
//! 跟 Windows 侧的 `WinDeviceRegistry` 语义对齐（`crates/vox-audio-win/src/sessions.rs`）：
//! **按程序名合并、正在出声的排前面、自己不列出来**。三处差异都是平台事实造成的，
//! 不是口味问题：
//!
//! 1. `executable` 用的是 PipeWire 报的 `application.process.binary`（`Discord`、`chrome`），
//!    Linux 上进程名不带 `.exe`；
//! 2. `display_name` 直接用这个二进制名，**不用** `application.name`——后者是"流的名字"，
//!    不是程序名，Discord 报的是 `WEBRTC VoiceEngine`、Chrome 报的是 `Chromium input`，
//!    拿给人看只会让人认不出要选哪个；
//! 3. `virtual_cable_installed` 在 Linux 上语义变成"PipeWire 在不在"：虚拟声卡不需要安装，
//!    随时能建（见 `virtual_sink.rs`）。

use std::collections::HashMap;

use vox_core::ports::{AudioApp, DeviceInfo, DeviceRegistry, PortResult};

use crate::probe::{
    self, GraphSnapshot, NodeRecord, CLASS_OUTPUT_STREAM, CLASS_SINK, CLASS_SOURCE,
    CLASS_SOURCE_VIRTUAL,
};

/// 设备目录。无状态：每次调用连一轮 PipeWire，用完就散。
pub struct LinuxDeviceRegistry;

impl LinuxDeviceRegistry {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinuxDeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry for LinuxDeviceRegistry {
    fn input_devices(&self) -> PortResult<Vec<DeviceInfo>> {
        let snapshot = probe::snapshot()?;
        let mut out = devices_of_class(&snapshot, CLASS_SOURCE, snapshot.default_source.as_deref());
        out.extend(devices_of_class(
            &snapshot,
            CLASS_SOURCE_VIRTUAL,
            snapshot.default_source.as_deref(),
        ));
        Ok(out)
    }

    fn output_devices(&self) -> PortResult<Vec<DeviceInfo>> {
        let snapshot = probe::snapshot()?;
        Ok(devices_of_class(
            &snapshot,
            CLASS_SINK,
            snapshot.default_sink.as_deref(),
        ))
    }

    fn audio_apps(&self) -> PortResult<Vec<AudioApp>> {
        let snapshot = probe::snapshot()?;
        Ok(merge_apps(&snapshot))
    }

    fn virtual_cable_installed(&self) -> bool {
        // 见模块头第 3 条：Linux 上这不是"装没装"，是"PipeWire 在不在"。
        probe::available()
    }
}

/// PipeWire 节点名里的**卡名段**（`docs/plans/S4-C-AEC.md` §4.3 的 B2）。
///
/// 第一段必须是方向段且以 `_input` / `_output` 结尾（`alsa_input.` /
/// `alsa_output.` / `bluez_output.` …）；虚拟麦的监视节点
/// （`vox_virtual_mic.monitor`）没有方向段、也没有卡 ⇒ `None`。
/// 剩下的**最后一段是 profile**（`analog-stereo` / `hdmi-stereo` / `pro-input-0`），
/// 它前面那一整段是卡名——**卡名里可能带点**（PCI 地址 `pci-0000_00_1f.3`），
/// 所以按"最后一段"切，不是"第一段"。
///
/// 认不出 ⇒ `None`，与"是同一张卡"区分开（调用方据此判 B2 不成立）。
pub fn card_key_of_node(node: &str) -> Option<&str> {
    let (direction, rest) = node.split_once('.')?;
    if !direction.ends_with("_input") && !direction.ends_with("_output") {
        return None;
    }
    let card = rest.rsplit_once('.')?.0;
    (!card.is_empty()).then_some(card)
}

fn devices_of_class(
    snapshot: &GraphSnapshot,
    class: &str,
    default_name: Option<&str>,
) -> Vec<DeviceInfo> {
    snapshot
        .nodes_of_class(class)
        .into_iter()
        .map(|node| DeviceInfo {
            name: label_of(node),
            is_default: default_name.is_some_and(|d| d == node.name),
        })
        .collect()
}

/// 给人看的设备名：优先 `node.description`，退到 `node.name`。
fn label_of(node: &NodeRecord) -> String {
    if node.description.is_empty() {
        node.name.clone()
    } else {
        node.description.clone()
    }
}

/// 正在出声（或曾经出过声）的程序，按程序名合并。
fn merge_apps(snapshot: &GraphSnapshot) -> Vec<AudioApp> {
    let me = std::process::id();
    let mut merged: HashMap<String, AudioApp> = HashMap::new();

    for node in snapshot.nodes_of_class(CLASS_OUTPUT_STREAM) {
        // 二进制名优先（`Discord`、`chrome`）；个别客户端不填 binary，退到 `application.name`，
        // 宁可显示得难看点，也不能把这条流整个丢掉。
        let Some(executable) = snapshot
            .binary_of(node)
            .or_else(|| snapshot.app_name_of(node))
        else {
            continue;
        };
        let pid = snapshot.pid_of(node).unwrap_or(0);
        if pid != 0 && pid == me {
            // 自己不列出来：用户要是选了自己，译文会绕回自己形成回路。
            continue;
        }
        let key = executable.to_ascii_lowercase();
        let active = node.running;
        match merged.get_mut(&key) {
            Some(existing) => {
                // 同一程序多条流：只要有一条在出声就算在出声（Chrome 每个标签页一条）。
                existing.active |= active;
                // 展示出声的那个 pid，方便用户对上 `ps`。
                if active {
                    existing.pid = pid;
                }
            }
            None => {
                merged.insert(
                    key,
                    AudioApp {
                        executable: executable.to_owned(),
                        display_name: executable.to_owned(),
                        pid,
                        active,
                    },
                );
            }
        }
    }

    let mut apps: Vec<AudioApp> = merged.into_values().collect();
    // 出声的排前面，其余按名字，列表顺序才稳定。
    apps.sort_by(|a, b| {
        b.active.cmp(&a.active).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ClientRecord;

    /// 造一条"某程序的播放流"节点，`id` 同时当 key 用。
    fn stream_node(id: u32, running: bool, client: Option<u32>) -> (u32, NodeRecord) {
        (
            id,
            NodeRecord {
                media_class: CLASS_OUTPUT_STREAM.to_owned(),
                name: format!("node-{id}"),
                description: String::new(),
                app_name: None,
                binary: None,
                client_id: client,
                running,
            },
        )
    }

    fn snapshot_with(
        nodes: Vec<(u32, NodeRecord)>,
        clients: Vec<(u32, ClientRecord)>,
    ) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes.into_iter().collect(),
            clients: clients.into_iter().collect(),
            ports: HashMap::new(),
            default_source: None,
            default_sink: None,
        }
    }

    fn client(id: u32, binary: &str, pid: u32) -> (u32, ClientRecord) {
        (
            id,
            ClientRecord {
                app_name: Some(format!("{binary} input")),
                binary: Some(binary.to_owned()),
                pid: Some(pid),
            },
        )
    }

    #[test]
    fn apps_merge_by_binary_and_keep_active_first() {
        // Discord 两条流（一条在放），Chrome 一条没在放，一个没有 client 的流被跳过。
        let snapshot = snapshot_with(
            vec![
                stream_node(1, false, Some(10)),
                stream_node(2, true, Some(11)),
                stream_node(3, false, Some(11)),
                stream_node(4, false, Some(12)),
                stream_node(5, true, None),
            ],
            vec![
                client(10, "Discord", 1000),
                client(11, "Discord", 1001),
                client(12, "chrome", 1002),
            ],
        );
        // 节点自己没报 binary，靠 client 补。
        let apps = merge_apps(&snapshot);
        let names: Vec<&str> = apps.iter().map(|a| a.executable.as_str()).collect();
        assert_eq!(names, vec!["Discord", "chrome"], "出声的排前面，其余按名字");
        assert!(apps[0].active, "有流在放的程序要排在前面");
        assert_eq!(apps[0].pid, 1001, "展示的是正在出声的那个 pid");
        assert!(!apps[1].active);
    }

    #[test]
    fn devices_prefer_description_and_mark_default() {
        let mut snapshot = snapshot_with(vec![], vec![]);
        snapshot.nodes.insert(
            1,
            NodeRecord {
                media_class: CLASS_SOURCE.to_owned(),
                name: "alsa_input.usb".to_owned(),
                description: "USB 麦克风".to_owned(),
                ..Default::default()
            },
        );
        snapshot.nodes.insert(
            2,
            NodeRecord {
                media_class: CLASS_SOURCE.to_owned(),
                name: "vox_virtual_mic.monitor".to_owned(),
                description: String::new(),
                ..Default::default()
            },
        );
        let devices = devices_of_class(&snapshot, CLASS_SOURCE, Some("vox_virtual_mic.monitor"));
        assert_eq!(devices[0].name, "USB 麦克风");
        assert!(!devices[0].is_default);
        assert_eq!(devices[1].name, "vox_virtual_mic.monitor");
        assert!(devices[1].is_default);
    }

    #[test]
    fn card_key_of_node_splits_on_the_last_segment_not_the_first() {
        // 同卡不同方向：B2 要的就是这个（笔记本内置 codec 的麦和喇叭）。
        assert_eq!(
            card_key_of_node("alsa_input.pci-0000_00_1f.3.analog-stereo"),
            Some("pci-0000_00_1f.3")
        );
        assert_eq!(
            card_key_of_node("alsa_output.pci-0000_00_1f.3.analog-stereo"),
            Some("pci-0000_00_1f.3")
        );
        // USB 声卡：卡名里带连字符，profile 不带点。
        assert_eq!(
            card_key_of_node("alsa_input.usb-Generic_USB_Audio-00.pro-input-0"),
            Some("usb-Generic_USB_Audio-00")
        );
        // 不同卡（HDMI 与板载）：卡名不同 ⇒ B2 不成立。
        assert_eq!(
            card_key_of_node("alsa_output.pci-0000_01_00.1.hdmi-stereo"),
            Some("pci-0000_01_00.1")
        );
        // 认不出的都给 `None`：虚拟麦监视节点（无方向段）、空卡名、缺 profile。
        assert_eq!(card_key_of_node("vox_virtual_mic.monitor"), None);
        assert_eq!(card_key_of_node("alsa_input."), None);
        assert_eq!(card_key_of_node("alsa_input.pci"), None);
        assert_eq!(card_key_of_node("alsa_output.usb-Audio-00"), None);
        assert_eq!(card_key_of_node(""), None);
    }
}
