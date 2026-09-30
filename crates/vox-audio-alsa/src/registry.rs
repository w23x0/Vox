//! `DeviceRegistry` 的 ALSA 实现：设备目录。
//!
//! 枚举走 alsa-lib 的 `pcm` hint（`alsa::device_name::HintIter`），过滤表见
//! `docs/plans/S4-C-ALSA.md` §5.4，三条方法的口径见 §5.6。三条要紧的纪律：
//!
//! 1. **`DeviceInfo.name` 就是能直接喂给 `PCM::new` 的名字**（§5.1）。不是我们自己编的
//!    短 id，也不是 hint 的 `DESC`。`settings.speak.input_device` 存的就是这一个字符串
//!    （`vox_core::settings`），中间再翻译一次就多一处会对不上的地方。
//! 2. **报的 id 一律是 `hw:CARD=<card 名>,DEV=<n>`**（§5.4）。`CARD=` 段是 udev 派生的
//!    卡名，声卡不换插槽就不变；`hw:0,0` 里的 index 插拔后会变。所以 id 稳定、
//!    能直接开、跨插拔不变，三条同时成立。
//! 3. **宁可没有星号，也不要指错设备**（§5.4）。`is_default` 靠"打开 `default` PCM、
//!    `info()` 读回硬件地址、与枚举结果逐条比对"得到；对不上就**谁都别标**，而不是
//!    退化成"标第一条"。指错的后果是用户没设设备时译文播到麦上，形成回声串扰。
//!
//! 本文件没有音频设备线程，所以 RULES #6 的热路径约束在这里退化成一条较弱的口径：
//! 全部工作发生在每次调用现构造的局部值上（无屏档 30 s 轮询一次，§1.1 C12），
//! 唯一的分配在构造返回的 `Vec<DeviceInfo>` 与上面两张哈希表里。
//! `sort_entries` 的比较函数刻意不分配（见 `cmp_ascii_lowercase`）。

use std::collections::HashMap;

use alsa::card::Iter as CardIter;
use alsa::device_name::HintIter;
use alsa::pcm::PCM;
use alsa::Direction;
use vox_core::ports::{AudioApp, DeviceInfo, DeviceRegistry, PortResult};

use crate::probe::map_err;

/// 枚举的 hint 接口名。alsa-lib 的 PCM hint 都在这一个命名空间下。
const PCM_IFACE: &str = "pcm";

/// 系统缺省 PCM 的名字，也是唯一一个**天生与方向无关**的条目（见 `keep_hint`）。
pub(crate) const DEFAULT_PCM: &str = "default";

/// 一块硬件设备的地址：`(card index, device)`。
///
/// 是 **index** 不是卡名：只有 index 能与 `PCM::info()` 的返回值直接比对
/// （`Info::get_card` / `::get_device`）。id 稳定性靠的是"先用卡名换 index、
/// 再由卡名拼出上报的 id"，见模块头第 2 条。
pub(crate) type HwAddr = (i32, u32);

/// 一条枚举结果。
///
/// `is_default` 由 [`apply_default`] 填，枚举时恒为 `false`——枚举与对号是两件事，
/// 分开才让对号能脱开声卡测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceEntry {
    /// 交给 `PCM::new` 的名字（`hw:CARD=Loopback,DEV=0` / `default`）。
    pub(crate) id: String,
    /// 给人看的名字（hint 的 `DESC` 首行，或所属卡的 longname）。
    pub(crate) label: String,
    /// 这条背后的硬件地址。纯逻辑设备（`default`）是 `None`——它指哪儿写在
    /// `alsa.conf` 里，见 [`resolve_default_addr`]。
    pub(crate) addr: Option<HwAddr>,
    /// 见模块头第 3 条：对不上就恒为 `false`。
    pub(crate) is_default: bool,
}

/// 设备目录。无状态：每次调用现构造几张表，用完就散。
pub struct AlsaDeviceRegistry;

impl AlsaDeviceRegistry {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AlsaDeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry for AlsaDeviceRegistry {
    fn input_devices(&self) -> PortResult<Vec<DeviceInfo>> {
        devices_of(Direction::Capture)
    }

    fn output_devices(&self) -> PortResult<Vec<DeviceInfo>> {
        devices_of(Direction::Playback)
    }

    /// ALSA **没有"谁在出声"这个概念**：它不记录应用与流的归属，`dmix`/`dsnoop`
    /// 那套也只给混音与监听，不给按程序抓音（§5.6 C13）。
    ///
    /// 返回空列表而不是报错：报错会让界面把「听人说话」这一格当故障显示，而空列表
    /// 与 `vox_core::capability::host_ceiling(HostKind::LinuxHeadless)` 不含
    /// `ProgramTap` 是同一件事的两种说法（§1.5）。要那一格请用 PipeWire 后端。
    fn audio_apps(&self) -> PortResult<Vec<AudioApp>> {
        Ok(Vec::new())
    }

    /// 恒为 `false`（§5.6 C14）。这条不是缺陷，是 `vox_core::ports::DeviceRegistry`
    /// 的文档已经写死的语义："Linux 上压根没有'装'这一步"——ALSA 是这句话的加强版，
    /// 连虚拟麦这个概念都没有。能不能用虚拟麦的唯一真相是能力位
    /// `Capability::VirtualMic`，不由这个字段回答。
    fn virtual_cable_installed(&self) -> bool {
        false
    }
}

// --- 纯逻辑：过滤、对号、排序 ------------------------------------------------

/// 一条 hint 该不该进列表。三个条件都要成立。
///
/// 1. **名字在保留表里**：只有 `hw:` 前缀与 `default` 留下。保留表是白名单而不是
///    黑名单——§5.4 那张表里要丢的（`plughw:`、`front:` / `surround51:` / `dsnoop:`
///    这些引脚别名与混音节点、`pulse` / `pipewire*` / `jack` 这些"经服务层"的入口、
///    `null`）全都被白名单一次挡住，不必逐个点名（点名反而漏得更快）。
///    `plughw:` 的丢弃还有个副作用要说清：它被丢了，但 §5.2 的协商阶梯第 2 档**照样用**
///    `plughw:`——那个名字是我们拿 `hw:` 的 id 自己拼出来去开的，不是从列表里选的。
/// 2. **它属于要列的那一面**：靠 hint 的 `direction`（`IOID`）分流。
/// 3. **`direction` 本身不为 `None`**：方向不明的 hint 两边都不列，宁可少列，
///    不要把输出设备列成麦克风。
///
/// **例外：`default` 两边都列。** 施工稿 §5.4 的过滤表把 `default` 收进来，
/// 末段又说方向不明的 hint 两边都不列——本机这两句会打架（实测 `default` 的
/// `IOID` 就是空的，desc 写的是 "Default ALSA Output (currently PipeWire Media Server)"）。
/// 这里的取舍是：`default` 不是"忘了填 `IOID` 的驱动"，它是一条**按打开方向解析**的
/// 路由别名——用 `PCM::new("default", Capture, ..)` 打开拿到的一定是采集侧的缺省，
/// 末段要防的"输出设备被列成麦克风"对它不成立。其余方向不明的 hint 照旧两边都不列
/// （本机实测 `hw:CARD=Loopback,DEV=0`、`hw:CARD=Generic_1,DEV=0` 就是这一类）。
pub(crate) fn keep_hint(name: &str, direction: Option<Direction>, side: Direction) -> bool {
    if name != DEFAULT_PCM && !name.starts_with("hw:") {
        return false;
    }
    if name == DEFAULT_PCM {
        return true;
    }
    direction == Some(side)
}

/// `hw:CARD=Loopback,DEV=1` 里 `DEV=1` 那个 1。**纯函数**：只认 `hw:` 前缀，
/// 其余前缀一律 `None`（`plughw:CARD=..,DEV=1` 的 device 号虽然也是 1，但它不是
/// 我们上报的那种条目，认它会让地址表里混进 plug 设备）。
///
/// `,DEV=` 取**最后**一段：卡名本身理论上也可以含逗号，取最后一段比取第一段稳。
pub(crate) fn hw_device_number(id: &str) -> Option<u32> {
    let device = id.strip_prefix("hw:")?.rsplit_once(",DEV=")?.1;
    // 显式只认十进制数字：`rsplit_once` 的尾巴可能是空串或带别的修饰，
    // 交给 `parse` 单独判断会漏掉 `DEV=3 ` 这种带空格的。
    if device.is_empty() || !device.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    device.parse().ok()
}

/// 给人看的名字：hint 的 `DESC` 首行 → 所属卡的 longname → `id` 本身。
///
/// 只取首行是因为 ALSA 的 `DESC` 是两行：首行是设备，第二行是
/// "Direct hardware device without any conversions" 这类**所有设备一样的样板说明**
/// （本机实测 60 条 hint 里逐字相同）。整段拿来当 label 等于把列表全变成同一句话。
///
/// **纯函数**，三个入参都是枚举时现成的，不回头碰 ALSA。
pub(crate) fn label_of(desc: Option<&str>, longname: Option<&str>, id: &str) -> String {
    let from_desc = desc
        .and_then(|text| text.lines().next())
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let from_card = longname.map(str::trim).filter(|line| !line.is_empty());
    from_desc.or(from_card).unwrap_or(id).to_owned()
}

/// 两个字符串比大小，**忽略 ASCII 大小写**。
///
/// 不用 `str::to_lowercase`：那是 Unicode 全量折叠（会把 `'İ'` 折成 `'i'` 加一个组合符、
/// 把 `'I'` 折成 `'ı'`），每次比较要分配两个 `String`，而设备名里除了厂商偶尔塞的
/// 非 ASCII（`声卡`）之外全是 ASCII。字节序对非 ASCII 部分退化成 UTF-8 字节序，
/// 稳定且全序，够用。
fn cmp_ascii_lowercase(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()))
}

/// 枚举出来的设备 + `default` PCM 实际指向的那个硬件地址 → 带 `is_default` 标记的列表。
///
/// **纯函数**（§3.5）：`resolved` 是"打开 `default` PCM 后 `info()` 读回来的
/// `(card, device)`"，拿不到就传 `None`。拿不到时**谁都别标**（列表里没有 `*`），
/// 由调用方记一条 `startup_notes`——宁可没有星号，也不要指错设备（模块头第 3 条）。
pub(crate) fn apply_default(
    mut devices: Vec<DeviceEntry>,
    resolved: Option<HwAddr>,
) -> Vec<DeviceEntry> {
    if let Some(addr) = resolved {
        for entry in &mut devices {
            // `addr == Some(addr)` 这一句同时排掉了纯逻辑条目（`default` 自己的
            // addr 是 `None`）：标它等于宣称"硬件地址就是这条"，而它没有硬件地址。
            entry.is_default = entry.addr == Some(addr);
        }
    }
    devices
}

/// 排序：缺省设备排第一，其余按 `label` 的小写字典序（§5.4 末段）。
///
/// ALSA 的 hint 顺序不保证稳定（它跟着 `alsa.conf` 的加载顺序走），所以要自己排。
/// 收尾再用 `id` 兜一次序，保证 label 撞车时顺序仍然确定。
///
/// 收 `&mut [..]` 而不是 `&mut Vec<..>`：后者会被 clippy 的 `ptr_arg` 判掉，
/// 而这里并不需要 `Vec` 特有的能力。
pub(crate) fn sort_entries(entries: &mut [DeviceEntry]) {
    entries.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| cmp_ascii_lowercase(&a.label, &b.label))
            .then_with(|| a.id.cmp(&b.id))
    });
}

// --- 碰硬件的部分 ----------------------------------------------------------

/// 枚举要用的两张表。**每次调用现构造，用完就散**（§5.4）。
///
/// `addrs` 的连接点是 **hint 名**而不是卡名，这一点是实测逼出来的：`CARD=` 段是
/// udev 派生的短名，与 `Card::get_name()` 对不上——本机上卡 0 的 `get_name()` 是
/// `"HDA NVidia"` 而 hint 里是 `CARD=NVidia`；卡 1 与卡 2 的 `get_name()` **都**是
/// `"HD-Audio Generic"`，hint 里却是 `CARD=Generic` 与 `CARD=Generic_1`。按卡名反查
/// 会把两张同型号卡混成一张、还漏掉所有被 udev 截短的名字。所以改成用
/// `HintIter::new(Some(&card), ..)` 按卡逐个问"你自己的 hint 有哪些"，连接点就是
/// hint 全名。
///
/// 单张卡问不动（枚举途中被拔掉、或它的 hint 读不出来）就跳过这一张，不让整个
/// 目录失败：设备列表少一张卡，远好过整页拿不到。
fn card_tables() -> (HashMap<String, HwAddr>, HashMap<i32, String>) {
    let mut addrs = HashMap::new();
    let mut longnames = HashMap::new();
    for card in CardIter::new() {
        let Ok(card) = card else {
            continue;
        };
        let index = card.get_index();
        if let Ok(longname) = card.get_longname() {
            longnames.insert(index, longname);
        }
        let Ok(hints) = HintIter::new_str(Some(&card), PCM_IFACE) else {
            continue;
        };
        for hint in hints {
            let Some(name) = hint.name.as_deref() else {
                continue;
            };
            if let Some(device) = hw_device_number(name) {
                addrs.insert(name.to_owned(), (index, device));
            }
        }
    }
    (addrs, longnames)
}

/// `default` PCM 实际指向哪块硬件。**纯逻辑之外的一步**：这里要真开一次 PCM。
///
/// 做法（§5.4 第 4 步）：打开 `default`、**不做任何 `hw_params`**（一设参数就可能被
/// 别的程序占住，而这里只想问"你指哪儿"）、`info()` 读回 `(card, device)`。
///
/// 两种情况都按"拿不到"处理并返回 `None`，让 `apply_default` 全表无星号：
///
/// - 开不了 / `info()` 报错；
/// - `card < 0`——**这不是错误，是本机的常态**：`default` 经服务层走时
///   `snd_pcm_info` 认不出底层硬件卡，报的 card 是 -1（本机实测，本机 `default`
///   指向 PipeWire）。这种"知道它是什么、但不知道它落在哪块卡上"的情况，按
///   §5.4 第 4 步的原文（`info()` 报错或没有一条命中 → 谁都别标）走同一条路。
pub(crate) fn resolve_default_addr(side: Direction) -> Option<HwAddr> {
    let pcm = PCM::new(DEFAULT_PCM, side, true).ok()?;
    let info = pcm.info().ok()?;
    let card = info.get_card();
    if card < 0 {
        return None;
    }
    Some((card, info.get_device()))
}

/// 枚举一个方向上的设备条目。**还没对号、还没排序**（那两步是纯函数）。
fn entries_of(side: Direction) -> PortResult<Vec<DeviceEntry>> {
    let (addrs, longnames) = card_tables();
    let hints = HintIter::new_str(None, PCM_IFACE).map_err(|e| map_err("枚举 PCM hint", e))?;
    let mut out = Vec::new();
    for hint in hints {
        let Some(id) = hint.name.as_deref() else {
            continue;
        };
        if !keep_hint(id, hint.direction, side) {
            continue;
        }
        let addr = addrs.get(id).copied();
        let longname = addr.and_then(|(card, _)| longnames.get(&card).map(String::as_str));
        out.push(DeviceEntry {
            id: id.to_owned(),
            label: label_of(hint.desc.as_deref(), longname, id),
            addr,
            is_default: false,
        });
    }
    Ok(out)
}

/// 一个方向上的设备列表：枚举 → 对号 → 排序 → 映射成端口层的 `DeviceInfo`。
fn devices_of(side: Direction) -> PortResult<Vec<DeviceInfo>> {
    let mut entries = apply_default(entries_of(side)?, resolve_default_addr(side));
    sort_entries(&mut entries);
    Ok(entries
        .into_iter()
        // 报出去的是 `id`（`hw:CARD=Loopback,DEV=0`），不是给人看的 `label`（§5.1）。
        // `id` 直接 move 过去，不 `clone`：`DeviceInfo` 只有一个字段，白 clone 一次
        // 纯属把列表的分配翻倍。
        .map(|entry| DeviceInfo {
            name: entry.id,
            is_default: entry.is_default,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机（`Loopback` 已加载、4 张卡）真枚举出来的 hint 样本，
    /// 逐字取自 `HintIter::new_str(None, "pcm")` 的输出（2026-09-30 核）。
    /// 故意把**该丢的**也一起收进来：过滤表的 bug 几乎都是"多收了一条"。
    const REAL_HINTS: &[(&str, Option<Direction>)] = &[
        // 服务层与工具：两端都不该列。
        ("null", None),
        ("jack", None),
        ("pulse", None),
        ("pipewire", None),
        ("speex", None),
        ("upmix", None),
        // 系统缺省：唯一一条方向为 None 也要列的。
        ("default", None),
        // 直通硬件 + 方向明确的，该列。
        ("hw:CARD=NVidia,DEV=3", Some(Direction::Playback)),
        ("hw:CARD=NVidia,DEV=7", Some(Direction::Playback)),
        ("hw:CARD=Generic_1,DEV=2", Some(Direction::Capture)),
        // 直通硬件但驱动没填 IOID 的：两边都不列。
        ("hw:CARD=Generic_1,DEV=0", None),
        ("hw:CARD=Loopback,DEV=0", None),
        ("hw:CARD=Loopback,DEV=1", None),
        // 同一张卡的另几种打开方式与引脚别名 / 混音节点：都不该列。
        ("plughw:CARD=NVidia,DEV=3", Some(Direction::Playback)),
        ("plughw:CARD=Loopback,DEV=1", None),
        ("hdmi:CARD=NVidia,DEV=0", Some(Direction::Playback)),
        ("dmix:CARD=NVidia,DEV=3", Some(Direction::Playback)),
        ("dsnoop:CARD=Generic_1,DEV=0", Some(Direction::Capture)),
        ("front:CARD=Generic_1,DEV=0", None),
        ("surround51:CARD=Generic_1,DEV=0", Some(Direction::Playback)),
        ("surround71:CARD=Loopback,DEV=0", Some(Direction::Playback)),
        ("sysdefault:CARD=Loopback", None),
        ("usbstream:CARD=NVidia", None),
    ];

    fn entry(id: &str, label: &str, addr: Option<HwAddr>) -> DeviceEntry {
        DeviceEntry {
            id: id.to_owned(),
            label: label.to_owned(),
            addr,
            is_default: false,
        }
    }

    #[test]
    fn hint_filter_keeps_hw_and_default_and_drops_the_rest() {
        for (name, direction) in REAL_HINTS {
            for side in [Direction::Capture, Direction::Playback] {
                let kept = keep_hint(name, *direction, side);
                let want = *name == DEFAULT_PCM
                    || (*name == "hw:CARD=NVidia,DEV=3"
                        && side == Direction::Playback
                        && *direction == Some(Direction::Playback))
                    || (*name == "hw:CARD=NVidia,DEV=7"
                        && side == Direction::Playback
                        && *direction == Some(Direction::Playback))
                    || (*name == "hw:CARD=Generic_1,DEV=2"
                        && side == Direction::Capture
                        && *direction == Some(Direction::Capture));
                assert_eq!(
                    kept, want,
                    "{name} 在 {side:?} 这一面被 keep_hint 判成了 {kept}"
                );
            }
        }
        // 白名单是"只有 hw: 与 default"，所以这张表里的东西一条都进不来。
        for name in [
            "plughw:CARD=Loopback,DEV=0",
            "front:CARD=Loopback,DEV=0",
            "dsnoop:CARD=Loopback,DEV=0",
            "dmix:CARD=Loopback,DEV=0",
            "hdmi:CARD=NVidia,DEV=0",
            "surround51:CARD=Loopback,DEV=0",
            "sysdefault:CARD=Loopback",
            "usbstream:CARD=Loopback",
            "pulse",
            "pipewire",
            "jack",
            "null",
            // 前缀相近但不是我们要的那个：认成 `hw:` 就等于把 plug / 自造名字放进来。
            "hwtrawCARD=Loopback,DEV=0",
            "hwbroken",
        ] {
            for side in [Direction::Capture, Direction::Playback] {
                assert!(
                    !keep_hint(name, Some(side), side),
                    "{name} 不该进列表，却进了 {side:?} 这一面"
                );
            }
        }
    }

    #[test]
    fn direction_none_is_listed_on_neither_side() {
        // 本机真有两张卡的 hw hint 方向是空的（驱动没填 IOID）。
        for name in [
            "hw:CARD=Loopback,DEV=0",
            "hw:CARD=Loopback,DEV=1",
            "hw:CARD=Generic_1,DEV=0",
        ] {
            assert!(
                !keep_hint(name, None, Direction::Capture),
                "{name} 方向不明却被列成麦克风了"
            );
            assert!(
                !keep_hint(name, None, Direction::Playback),
                "{name} 方向不明却被列成扬声器了"
            );
        }
        // 唯一的方向无关例外是系统缺省：它是按打开方向解析的路由别名，
        // 用它当麦克风拿到的仍然是采集侧的缺省（见 keep_hint 的文档）。
        for side in [Direction::Capture, Direction::Playback] {
            assert!(
                keep_hint(DEFAULT_PCM, None, side),
                "系统缺省在 {side:?} 这一面必须列得出来，否则用户选不了'跟随系统'"
            );
            // 方向填了的时候也照样列——`default` 的 IOID 在别的机器上未必是空的。
            assert!(keep_hint(DEFAULT_PCM, Some(side), side));
        }
        // 而"方向填错了"的另一面照样要丢：`default` 即便报成采集也不进播放列表之外的地方。
        assert!(!keep_hint(
            "hw:CARD=Generic_1,DEV=2",
            Some(Direction::Capture),
            Direction::Playback
        ));
        assert!(keep_hint(
            "hw:CARD=Generic_1,DEV=2",
            Some(Direction::Capture),
            Direction::Capture
        ));
    }

    #[test]
    fn apply_default_marks_only_the_matching_hw_address() {
        let devices = vec![
            entry("default", "默认", None),
            entry("hw:CARD=NVidia,DEV=3", "HDMI 0", Some((0, 3))),
            entry("hw:CARD=Loopback,DEV=1", "Loopback PCM", Some((3, 1))),
        ];
        let marked = apply_default(devices, Some((3, 1)));
        assert!(
            !marked[0].is_default,
            "default 自己没有硬件地址，不能靠 addr 匹配中标"
        );
        assert!(!marked[1].is_default, "card 对上了但 device 不对，不能标");
        assert!(marked[2].is_default);
    }

    #[test]
    fn apply_default_marks_nobody_when_unresolved() {
        // 退化"标第一条"就毁在这里：本机 `default` 经 PipeWire 走，info() 的 card 是 -1，
        // 全表无星号才是诚实的答案。
        let devices = vec![
            entry("default", "默认", None),
            entry("hw:CARD=NVidia,DEV=3", "HDMI 0", Some((0, 3))),
            entry("hw:CARD=Loopback,DEV=1", "Loopback PCM", Some((3, 1))),
        ];
        for resolved in [None, Some((-1, 0))] {
            let marked = apply_default(devices.clone(), resolved);
            assert!(
                marked.iter().all(|d| !d.is_default),
                "{resolved:?} 之下不该标任何设备"
            );
        }
    }

    #[test]
    fn the_default_device_sorts_first_and_the_rest_by_label() {
        let mut entries = vec![
            entry("hw:CARD=NVidia,DEV=9", "HDMI 3", Some((0, 9))),
            entry("hw:CARD=Generic_1,DEV=2", "alc897 alt analog", Some((2, 2))),
            entry("hw:CARD=NVidia,DEV=3", "G27M2Pro", Some((0, 3))),
            entry("hw:CARD=Loopback,DEV=1", "ALC897 Analog", Some((3, 1))),
        ];
        entries[2].is_default = true;
        sort_entries(&mut entries);
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![
                "hw:CARD=NVidia,DEV=3",
                "hw:CARD=Generic_1,DEV=2",
                "hw:CARD=Loopback,DEV=1",
                "hw:CARD=NVidia,DEV=9",
            ],
            "缺省排第一，其余按 label 忽略大小写字典序（g27m2pro < alc897 alt analog < alc897 analog < hdmi 3）"
        );
        // label 撞车时靠 id 兜底，顺序必须仍然确定。
        let mut tied = vec![
            entry("hw:CARD=Generic,DEV=7", "Same", Some((1, 7))),
            entry("hw:CARD=Generic,DEV=3", "Same", Some((1, 3))),
        ];
        sort_entries(&mut tied);
        assert_eq!(tied[0].id, "hw:CARD=Generic,DEV=3");
        // 非 ASCII 的 label 也要能排序（厂商名里会出现），不能 panic 也不能丢。
        let mut unicode = vec![entry("a", "声卡乙", None), entry("b", "声卡甲", None)];
        sort_entries(&mut unicode);
        assert_eq!(unicode.len(), 2);
    }

    #[test]
    fn hw_device_number_reads_the_dev_segment_and_only_from_hw() {
        assert_eq!(hw_device_number("hw:CARD=Loopback,DEV=1"), Some(1));
        assert_eq!(hw_device_number("hw:CARD=Generic_1,DEV=2"), Some(2));
        assert_eq!(hw_device_number("hw:CARD=NVidia,DEV=0"), Some(0));
        assert_eq!(hw_device_number("hw:CARD=Big,DEV=10"), Some(10));
        // 认成 plug / usb stream 就会把非硬件条目塞进地址表。
        assert_eq!(hw_device_number("plughw:CARD=Loopback,DEV=1"), None);
        assert_eq!(hw_device_number("dsnoop:CARD=Loopback,DEV=1"), None);
        assert_eq!(hw_device_number("usbstream:CARD=NVidia"), None);
        assert_eq!(hw_device_number("hw:CARD=Loopback"), None);
        assert_eq!(hw_device_number("hw:CARD=Loopback,DEV="), None);
        assert_eq!(hw_device_number("hw:CARD=Loopback,DEV=x"), None);
        assert_eq!(hw_device_number("default"), None);
        assert_eq!(hw_device_number(""), None);
    }

    #[test]
    fn label_takes_the_first_desc_line_and_never_the_boilerplate() {
        // 本机真 hint 的 DESC 是两行，第二行逐字相同；整段拿来当 label 等于整页同一句话。
        let desc = Some("HDA NVidia, G27M2Pro\nDirect hardware device without any conversions");
        assert_eq!(
            label_of(desc, None, "hw:CARD=NVidia,DEV=3"),
            "HDA NVidia, G27M2Pro"
        );
        // DESC 缺了退到卡的 longname，再缺退到 id 本身——列表里不许出现空标签。
        assert_eq!(
            label_of(None, Some("Loopback 1"), "hw:CARD=Loopback,DEV=0"),
            "Loopback 1"
        );
        assert_eq!(label_of(None, None, "default"), "default");
        assert_eq!(
            label_of(Some("   \n  "), Some("  "), "hw:CARD=X,DEV=0"),
            "hw:CARD=X,DEV=0"
        );
    }

    #[test]
    fn audio_apps_is_empty_and_the_virtual_cable_is_never_installed() {
        // C13 / C14：这两条是端口契约，不是实现口味。报成"故障"或"装了"都是错的。
        let registry = AlsaDeviceRegistry::new();
        assert_eq!(registry.audio_apps().unwrap_or_default(), Vec::new());
        assert!(!registry.virtual_cable_installed());
    }

    /// 真声卡用例：本机第 3 张卡是 `snd-aloop` 的 `Loopback`（需 `modprobe snd-aloop`）。
    /// 钉的是模块头那三条纪律里唯一一条能在运行期验的——**报出去的每个名字都真的开得了**，
    /// 以及 hint 名里的 `CARD=` 段真的能换回正确的 card index。
    #[test]
    #[ignore = "要真声卡：本机的 snd-aloop Loopback 卡（无卡机器上跑不了）"]
    fn published_names_really_open_and_pair_with_their_card() {
        let _card = crate::LOOPBACK_CARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = AlsaDeviceRegistry::new();

        for (side, devices) in [
            (
                Direction::Capture,
                registry.input_devices().expect("枚举输入设备失败"),
            ),
            (
                Direction::Playback,
                registry.output_devices().expect("枚举输出设备失败"),
            ),
        ] {
            assert!(!devices.is_empty(), "{side:?} 这一面一条设备都没有");
            assert!(
                devices.iter().filter(|d| d.is_default).count() <= 1,
                "{side:?} 这一面标出了多于一个缺省设备"
            );
            for device in &devices {
                let pcm = PCM::new(&device.name, side, true)
                    .unwrap_or_else(|e| panic!("报出来的名字 {:?} 打不开：{e}", device.name));
                assert!(
                    pcm.info().is_ok(),
                    "{:?} 开得了却读不回硬件地址，对号那一步会失效",
                    device.name
                );
            }
        }

        // Loopback 卡：上报的 hw 名字与 `info()` 读回来的 card index 对得上，
        // 且地址表里能反查到它（§5.4 的 id 稳定性口径）。
        let loopback_index = CardIter::new()
            .filter_map(|card| card.ok())
            .find(|card| matches!(card.get_name(), Ok(ref name) if name == "Loopback"))
            .map(|card| card.get_index())
            .expect("本机没有 Loopback 卡（需要 modprobe snd-aloop）");
        for (name, side, device) in [
            ("hw:CARD=Loopback,DEV=0", Direction::Playback, 0u32),
            ("hw:CARD=Loopback,DEV=1", Direction::Capture, 1u32),
        ] {
            let pcm = PCM::new(name, side, true)
                .unwrap_or_else(|e| panic!("{name} 打不开：{e}（这台的 snd-aloop 不对）"));
            let info = pcm
                .info()
                .unwrap_or_else(|e| panic!("{name} 读不回 info：{e}"));
            assert_eq!(info.get_card(), loopback_index, "{name} 指到了别的卡上");
            assert_eq!(info.get_device(), device, "{name} 指到了别的设备上");
        }
        let (addrs, _) = card_tables();
        assert_eq!(
            addrs.get("hw:CARD=Loopback,DEV=1").copied(),
            Some((loopback_index, 1))
        );
    }
}
