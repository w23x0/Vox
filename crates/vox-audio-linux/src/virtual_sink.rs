//! 虚拟麦克风：PipeWire 原生 sink，翻译语音往里写，别的程序把它当麦克风。
//!
//! 这是 Windows 上 VB-CABLE 的对应物，但**干净得多**：不需要下载、不需要安装、
//! 不需要管理员权限、不会在系统里留下驱动。做法就是在 PipeWire 图里建一个
//! `Audio/Sink` 节点（`support.null-audio-sink` 适配器），然后把译文播进它；
//! 它自带 monitor 端口，别的程序（VRChat / Discord / OBS）在录音设备里选
//! 「Vox Virtual Mic」即可。
//!
//! 用户那一步跟 Windows 上选 VB-CABLE 是同一个动作，所以引导文案可以照搬。

use std::time::{Duration, Instant};

use pipewire as pw;
use pw::properties::properties;
use vox_core::ports::{PortError, PortResult};

use crate::probe;

/// 虚拟设备在系统里的稳定标识。用户在录音设置里看到的是 `description`。
pub const NODE_NAME: &str = "vox_virtual_mic";
/// 给人看的名字（会出现在 GNOME/KDE 的录音设备列表里）。
///
/// 用英文跟图里其它设备保持一致（本机的 `GB206 High Definition Audio Controller
/// Digital Stereo (HDMI)` 之类都是英文），Windows 上的 VB-CABLE 也是英文名。
pub const DESCRIPTION: &str = "Vox Virtual Mic";

/// 建好的虚拟 sink。
///
/// 生命周期跟这个句柄绑：drop / `destroy()` 时把图里的节点删掉。
/// `object.linger` **故意不开**——进程退了设备就该消失，不留幽灵设备。
pub struct VirtualSink {
    node: pw::node::Node,
    core: pw::core::CoreRc,
    main_loop: pw::main_loop::MainLoopRc,
}

impl VirtualSink {
    /// 在系统里建一个虚拟 sink。已有同名节点时会失败——调用方先查 `exists()`。
    ///
    /// **返回 `Ok` 就意味着节点在图里真的看得见**（不是"我们的请求发出去了"）：
    /// 装配层拿"这个句柄建出来了"当能力位的凭据（S0 §2.5.4 的 R6），所以这里必须
    /// 等到另一个客户端也查得到它才敢说建成了。
    pub fn create() -> PortResult<Self> {
        probe::init();
        let (main_loop, core) = probe::connect()?;
        let props = properties! {
            *pw::keys::FACTORY_NAME => "support.null-audio-sink",
            *pw::keys::NODE_NAME => NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => DESCRIPTION,
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            // 声道布局交给 `support.null-audio-sink` 的默认值（立体声）。pipewire-rs
            // 没导出 `audio.position` 这个键，手写字符串容易写错格式（dict 里是
            // "FL,FR"，不是 Spa JSON 的数组），不值得为它冒险。
            // 不做默认设备：用户得自己去目标程序里选，偷偷抢默认输出是耍流氓。
        };
        let node = core
            .create_object::<pw::node::Node>("adapter", &props)
            .map_err(|e| PortError::new(format!("创建虚拟麦克风失败：{e}")))?;

        // 建的请求要跑到服务端、再把节点信息收回来，才算真的存在。
        probe::roundtrip(&main_loop, &core)?;
        probe::roundtrip(&main_loop, &core)?;

        let sink = Self {
            node,
            core,
            main_loop,
        };
        sink.wait_until_visible()?;
        Ok(sink)
    }

    /// 等节点在**图里**真的看得见（另开一个客户端收一轮快照来确认）。
    ///
    /// 为什么要等：`create_object` 返回、两轮 roundtrip 跑完，只说明**我们的请求**被
    /// 服务端处理了；适配器节点被登记成全局对象可能还排在后面。本机实测（2026-09-22）
    /// 约四分之一的启动里，紧接着的一次图快照还看不到它（10 ms 后就看得到）。
    /// 不等就会出现"我们这边说建好了、别的程序（界面、`wpctl`、VRChat）却还看不到这个设备"
    /// 的窗口——而"位说 ON、设备不存在"正是这一位要防的东西（S0 §1.4）。
    ///
    /// 超时还是看不见就报错（**不返回一个可能不存在的设备**）：调用方据此把能力位报假。
    fn wait_until_visible(&self) -> PortResult<()> {
        const TIMEOUT: Duration = Duration::from_millis(1_000);
        const STEP: Duration = Duration::from_millis(20);

        let started = Instant::now();
        loop {
            if Self::exists()? {
                return Ok(());
            }
            if started.elapsed() >= TIMEOUT {
                return Err(PortError::new(format!(
                    "虚拟麦克风建出来了，但 {} ms 内图里都查不到它",
                    TIMEOUT.as_millis()
                )));
            }
            std::thread::sleep(STEP);
        }
    }

    /// 系统里是不是已经有这个名字的 sink（比如上次没退干净）。
    pub fn exists() -> PortResult<bool> {
        let snapshot = probe::snapshot()?;
        Ok(snapshot
            .nodes_of_class(probe::CLASS_SINK)
            .iter()
            .any(|node| node.name == NODE_NAME))
    }

    /// 删掉虚拟 sink（退出时调用）。
    pub fn destroy(self) {
        let _ = self.core.destroy_object(self.node);
        let _ = probe::roundtrip(&self.main_loop, &self.core);
    }

    /// 监听端口名，播放侧要往这些端口灌数据（`monitor_FL` / `monitor_FR`）。
    pub fn node_name(&self) -> &'static str {
        NODE_NAME
    }
}

/// 直接给"某程序是不是把我当麦克风了"这类 UI 提示用：节点描述。
pub fn description() -> &'static str {
    DESCRIPTION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_name_is_stable_and_namespaced() {
        // 这个名字是用户要在别的程序里找的，改了就是行为变更，别顺手改。
        assert_eq!(NODE_NAME, "vox_virtual_mic");
        assert!(!DESCRIPTION.is_empty());
    }

    /// 建 → 查得到 → 删 → 查不到。默认 `#[ignore]`：要连真机的 PipeWire。
    /// 本机验收：`cargo test -p vox-audio-linux -- --ignored virtual_sink_lifecycle`
    #[test]
    #[ignore = "需要真机上跑着 PipeWire"]
    fn virtual_sink_lifecycle() {
        if VirtualSink::exists().expect("查不到 PipeWire 图") {
            // 上一次跑残留了，先清掉再测，避免同名冲突。
            VirtualSink::create().expect("清理残留失败").destroy();
        }
        assert!(!VirtualSink::exists().unwrap(), "起始状态不该有虚拟麦");

        let sink = VirtualSink::create().expect("创建虚拟麦克风失败");
        assert!(VirtualSink::exists().unwrap(), "建完应当能查到");
        sink.destroy();
        assert!(!VirtualSink::exists().unwrap(), "删完不该还在");
    }
}
