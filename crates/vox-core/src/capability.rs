//! 统一能力位模型：**位 = 事实，不是期望**（`DIRECTIONS.md` §10.5-2）。
//!
//! 三件事各归各的家（§2.5.2）：
//!
//! - **档位上限在芯里**（[`host_ceiling`]）：用来回答"**别的**档位能不能做某件事"
//!   （S1 的 `describe` 要问这个，而那时未必跑在那个外壳里）。
//! - **这台机器现在的事实在外壳里**（[`HostFacts`]）：装配时经 `Runtime::set_host_facts` 注入一次。
//!   它是运行期事实（PipeWire 在不在、D-Bus 有没有托盘宿主、VB-CABLE 装没装、虚拟麦句柄建没建起来），
//!   编译期烘焙的 `catalog/*.json` 承载不了。
//! - **有效位 = 上限 − 关掉的**，只在芯里算一次（[`effective`] / [`CapabilityReport::of`]），
//!   界面与出口拿到的都是同一个结果。
//!
//! 三条纪律：
//!
//! 1. **上限是硬的**：事实只能在档位上限**里面**关。`off` 里出现上限之外的位 = 外壳 bug
//!    （[`HostFacts::excess_off_bits`]），不是"新能力"——这样"谎报能力"在结构上就被封住。
//! 2. **位为 `true` 必须有定义者**：由"真的把这条路打开的那段代码"负责（§2.5.4）。没有定义者的位
//!    一律恒假（`net_in` / `net_out` / `file_config`，以及 4 个只占名的 provider 位）。
//! 3. **芯只给 `(位, reason)`，句子不在芯里**（§2.6 R3）：文案在 `app/ui/src/i18n/*`。
//!
//! 平台 API 一个都不出现：`HostKind` 是**档位名字**这张数据表，`HostFacts` 是外壳填进来的数据。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::composition::HostKind;
use crate::settings::ModelProvider;

/// 位的名字。JSON 键名 = `snake_case` 名字，和 provider 侧现有做法一致。
///
/// **判别式就是位图下标**：新位只许往后加（动前面的等于换协议）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    // --- 宿主 / 设备位（11 个，§2.5.1 上表） ---
    /// 本机麦克风采集。
    Mic = 0,
    /// 抓指定程序的声音（进程环回）。
    ProgramTap = 1,
    /// 别的程序能把我们当麦克风（Windows: VB-CABLE / Linux: PipeWire sink）。
    VirtualMic = 2,
    /// 屏幕 / 窗口字幕。
    Captions = 3,
    /// 全局热键。
    GlobalHotkey = 4,
    /// 有东西在**显示**托盘图标。
    Tray = 5,
    /// 无人值守常驻 / 开机自启。
    BackgroundService = 6,
    /// 头显里那份字幕（Windows 的 `steamvr-overlay` 构建）。
    VrCaptions = 7,
    /// 从网络收声音。**没有定义者 ⇒ 实现落地前恒假**（S3）。
    NetIn = 8,
    /// 往网络发声音。同上。
    NetOut = 9,
    /// 配置 / 状态能不经界面进出（无屏可运维）。**没有定义者 ⇒ 恒假**（S3）。
    FileConfig = 10,

    // --- provider 位（8 个；后 4 个只占名，值恒假） ---
    /// 服务端认音色选择。
    VoiceSelection = 11,
    /// 服务端认声音复刻。
    VoiceClone = 12,
    /// 服务端认源语言。
    SourceLanguage = 13,
    /// 换语言 / 换音色不重连。
    HotUpdateLanguage = 14,
    /// 回报用量。**占名**：只进这个枚举，不进 `catalog/*.json`（§2.5.1）。
    UsageReporting = 15,
    /// 服务端报说话起止。**占名**。
    SpeechActivity = 16,
    /// 服务端报回合结束。**占名**。
    TurnEnd = 17,
    /// 回报源文。**占名**。
    SourceTranscript = 18,
}

impl Capability {
    /// 全部 19 个位。顺序 = 位图下标顺序（= 判别式顺序）。
    pub const ALL: &'static [Capability] = &[
        Self::Mic,
        Self::ProgramTap,
        Self::VirtualMic,
        Self::Captions,
        Self::GlobalHotkey,
        Self::Tray,
        Self::BackgroundService,
        Self::VrCaptions,
        Self::NetIn,
        Self::NetOut,
        Self::FileConfig,
        Self::VoiceSelection,
        Self::VoiceClone,
        Self::SourceLanguage,
        Self::HotUpdateLanguage,
        Self::UsageReporting,
        Self::SpeechActivity,
        Self::TurnEnd,
        Self::SourceTranscript,
    ];

    /// 宿主 / 设备位：查 [`host_ceiling`] 与 `HostFacts` 的那几位。
    pub const HOST: &'static [Capability] = &[
        Self::Mic,
        Self::ProgramTap,
        Self::VirtualMic,
        Self::Captions,
        Self::GlobalHotkey,
        Self::Tray,
        Self::BackgroundService,
        Self::VrCaptions,
        Self::NetIn,
        Self::NetOut,
        Self::FileConfig,
    ];

    /// provider 位：查 `catalog::supports` 的那几位。
    pub const PROVIDER: &'static [Capability] = &[
        Self::VoiceSelection,
        Self::VoiceClone,
        Self::SourceLanguage,
        Self::HotUpdateLanguage,
        Self::UsageReporting,
        Self::SpeechActivity,
        Self::TurnEnd,
        Self::SourceTranscript,
    ];

    /// 位图里的那一位。
    pub const fn bit(self) -> u64 {
        1u64 << (self as u32)
    }

    /// 这位属于宿主还是 provider。查询 API 按它分派。
    pub fn scope(self) -> CapabilityScope {
        if (self as u32) <= Self::FileConfig as u32 {
            CapabilityScope::Host
        } else {
            CapabilityScope::Provider
        }
    }

    /// 面向用户的短标签（英文标识，不是给人看的句子；文案在界面 i18n 里）。
    ///
    /// 与 serde 的 `snake_case` 名字**必须一致**（有测试钉着），它是 JSON 键名、界面查文案的 key。
    pub fn id(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::ProgramTap => "program_tap",
            Self::VirtualMic => "virtual_mic",
            Self::Captions => "captions",
            Self::GlobalHotkey => "global_hotkey",
            Self::Tray => "tray",
            Self::BackgroundService => "background_service",
            Self::VrCaptions => "vr_captions",
            Self::NetIn => "net_in",
            Self::NetOut => "net_out",
            Self::FileConfig => "file_config",
            Self::VoiceSelection => "voice_selection",
            Self::VoiceClone => "voice_clone",
            Self::SourceLanguage => "source_language",
            Self::HotUpdateLanguage => "hot_update_language",
            Self::UsageReporting => "usage_reporting",
            Self::SpeechActivity => "speech_activity",
            Self::TurnEnd => "turn_end",
            Self::SourceTranscript => "source_transcript",
        }
    }
}

/// 位属于哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityScope {
    Host,
    Provider,
}

/// 位集：u64 位图。19 个位，留够余量。
///
/// 序列化成"开着的位"数组（读起来就是 §2.5.1 的表），构造入口只有 [`CapabilitySet::of`] /
/// [`CapabilitySet::single`] 这几个常量函数，写上限表时不用分配。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "Vec<Capability>", from = "Vec<Capability>")]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 从一串位拼一个集合（写档位上限表用）。
    pub const fn of(bits: &[Capability]) -> Self {
        let mut acc = 0u64;
        let mut i = 0;
        while i < bits.len() {
            acc |= bits[i].bit();
            i += 1;
        }
        Self(acc)
    }

    pub const fn single(bit: Capability) -> Self {
        Self(bit.bit())
    }

    pub const fn contains(self, bit: Capability) -> bool {
        self.0 & bit.bit() != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// 去掉一位。`effective()` 用它逐条减 `off`，不必为 `off` 的键另建一个集合。
    pub const fn without(self, bit: Capability) -> Self {
        Self(self.0 & !bit.bit())
    }

    /// 按 [`Capability::ALL`] 的顺序遍历开着的位（顺序稳定，界面与出口不用自己排）。
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(move |bit| self.contains(*bit))
    }
}

impl From<CapabilitySet> for Vec<Capability> {
    fn from(set: CapabilitySet) -> Self {
        set.iter().collect()
    }
}

impl From<Vec<Capability>> for CapabilitySet {
    fn from(bits: Vec<Capability>) -> Self {
        Self::of(&bits)
    }
}

/// 为什么没有这一位。**不是句子**——芯不许带界面文案，也不许带 i18n 依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// 结构上做不到（Android 没有虚拟麦；GPT 不回报用量）。
    Unsupported,
    /// 需要用户先装个东西（VB-CABLE）。
    NotInstalled,
    /// 需要用户先授权 / 加组 / 去系统设置页（`input` 组、`RECORD_AUDIO`、`SYSTEM_ALERT_WINDOW`）。
    Permission,
    /// 这个构建里没编进去（cargo feature 没开：`steamvr-overlay`）。
    NotBuilt,
    /// 实现存在、但**装配层还没把这段路接上**（Linux 虚拟麦接线前的中间态）。
    ///
    /// 专用来说明"不是平台做不到，是我们还没接"——位必须报假，直到持有句柄的那段代码真的
    /// 把节点建出来（§2.5.4 的 R6）。
    NotWired,
    /// 装了或卸了，但要重启才生效（VB-CABLE 的 `install_pending_reboot` / `uninstall_incomplete`）。
    PendingReboot,
    /// 暂时不可用（麦克风被别的程序占着）。
    Busy,
}

/// 一位的状态：开着 / 关着（关着必带 reason）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CapabilityStatus {
    pub enabled: bool,
    /// `enabled == true` 时必须为 `None`（只有 [`CapabilityStatus::ON`] 与
    /// [`CapabilityStatus::off`] 两个构造入口）。
    pub reason: Option<UnavailableReason>,
}

impl CapabilityStatus {
    pub const ON: Self = Self {
        enabled: true,
        reason: None,
    };

    pub const fn off(reason: UnavailableReason) -> Self {
        Self {
            enabled: false,
            reason: Some(reason),
        }
    }

    /// "开了就不能有 reason"这条不变量（构造入口已经保证，界面侧也能自检）。
    pub const fn is_consistent(self) -> bool {
        self.enabled == self.reason.is_none()
    }
}

/// 这台机器报上来的事实：**只报"关掉的位"**，其余按档位上限算开。
///
/// 由外壳装配时构造并注入（`Runtime::set_host_facts`）。字段都是**数据**：档位是个名字、
/// 设备名是个字符串——芯读它不违反"芯不碰平台 API"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HostFacts {
    /// 哪一档宿主。**只有外壳知道自己是哪一份构建**（§2.5.0 第 1 步）。
    pub host: HostKind,
    pub off: BTreeMap<Capability, UnavailableReason>,
    /// 这台机器上"译音该往哪个设备送才算虚拟麦"。`virtual_mic` 位为假时是 `None`。
    ///
    /// 由外壳填：Linux = 节点名 `vox_virtual_mic`（**接线后才非空**）；
    /// Windows = 用户在设置里选的 VB-CABLE 端点，所以这里恒 `None`，仍走 `settings.output_device`。
    pub virtual_mic_device: Option<String>,
}

impl HostFacts {
    /// **外壳还没注入事实**时的口径：**不冒认**——靠外壳真的接上才算数的位一律关掉。
    ///
    /// 产品路径上档位与事实都由外壳声明并注入（§2.5.0 第 1–3 步）；这份缺省只在"芯被单独
    /// 使用"时兜底（芯自己的单测、还没注入事实的库用户）。缺省**不许 fail-open**：位为 `true`
    /// 必须有定义者（§2.5.4 的 R6），而缺省值不是定义者——“没有事实 ⇒ 什么都不关”等于让缺省
    /// 替外壳冒认。
    ///
    /// 关掉的是**要外壳多做一步才有**的四位，报 [`UnavailableReason::NotWired`]：虚拟麦
    /// （Windows 要 VB-CABLE、Linux 要节点真的建出来）、进程环回（Windows 要 build ≥ 20348、
    /// Linux 要 PipeWire）、托盘（要有托盘宿主）、全局热键（Linux 要 `input` 组）。
    /// 其余位照档位上限算开：它们由"这一档有没有"决定（采集 / 字幕 / 后台存活），不是"接没接"，
    /// 缺省把它们也关掉只会让芯被单独使用时连一份清单都造不出来。
    ///
    /// 档位填 `LinuxDesktop`：本仓库四档里只有桌面两档有实据（§2.5.1），而这两档在**采集 /
    /// 播放 / 字幕**几格上上限完全一致（差的那一位 `vr_captions` 是"这个构建编没编进去"，
    /// 不该由缺省值拍）——所以这份缺省在"清单能不能装"这件事上不含任何平台冒认。
    pub fn uninjected() -> Self {
        Self {
            host: HostKind::LinuxDesktop,
            off: BTreeMap::from([
                (Capability::VirtualMic, UnavailableReason::NotWired),
                (Capability::ProgramTap, UnavailableReason::NotWired),
                (Capability::Tray, UnavailableReason::NotWired),
                (Capability::GlobalHotkey, UnavailableReason::NotWired),
            ]),
            virtual_mic_device: None,
        }
    }

    /// **单测专用**：一份"该接的都接上了"的事实（= 档位上限整份报开）。
    ///
    /// 与 [`HostFacts::uninjected`] 正好相反——那份是"还没有事实"（fail-closed），这份是
    /// "事实齐全且都好"。产品路径永远由外壳注入事实，这份只给"模拟一台已装配好的桌面机"
    /// 的测试当基准。
    #[cfg(test)]
    pub(crate) fn all_wired(host: HostKind) -> Self {
        Self {
            host,
            off: BTreeMap::new(),
            virtual_mic_device: None,
        }
    }

    /// `off` 里"档位上限之外"的位 = 外壳 bug（§2.5.0 第 4 步的边界）。
    ///
    /// 这么多位不可能被打开（上限是硬的），报出来让外壳自己修。
    pub fn excess_off_bits(&self) -> CapabilitySet {
        let ceiling = host_ceiling(self.host);
        Capability::ALL
            .iter()
            .copied()
            .filter(|bit| self.off.contains_key(bit) && !ceiling.contains(*bit))
            .fold(CapabilitySet::empty(), |set, bit| {
                set.union(CapabilitySet::single(bit))
            })
    }
}

/// 某个 host 档位结构上能有什么。**数据表，不含平台 API，也只此一份**（外壳不许自带一张）。
///
/// 四档是**并列**的（桌面 / 手机 / 无屏），不是"新与旧"：
///
/// - 表里是**结构上限**：这一档"有没有这一位"。今天有没有，由外壳的 [`HostFacts::off`] 决定。
/// - 不在表里的位，外壳就算报了也打不开（`false(unsupported)` 的含义）。
/// - Windows 独有的 `vr_captions` 是"这个构建编没编进去"——它在 Windows 上限里，由外壳用
///   [`UnavailableReason::NotBuilt`] 关掉。
pub fn host_ceiling(host: HostKind) -> CapabilitySet {
    match host {
        HostKind::Windows => CapabilitySet::of(&[
            Capability::Mic,
            Capability::ProgramTap,
            Capability::VirtualMic,
            Capability::Captions,
            Capability::GlobalHotkey,
            Capability::Tray,
            Capability::BackgroundService,
            Capability::VrCaptions,
        ]),
        HostKind::LinuxDesktop => CapabilitySet::of(&[
            Capability::Mic,
            Capability::ProgramTap,
            Capability::VirtualMic,
            Capability::Captions,
            Capability::GlobalHotkey,
            Capability::Tray,
            Capability::BackgroundService,
        ]),
        HostKind::Android => CapabilitySet::of(&[
            Capability::Mic,
            Capability::Captions,
            Capability::BackgroundService,
        ]),
        HostKind::LinuxHeadless => CapabilitySet::of(&[
            // 挂了 USB 声卡且枚举得到时这一位才为真（事实由外壳报）；上限里留着。
            Capability::Mic,
            Capability::BackgroundService,
        ]),
    }
}

/// 有效位 = 档位上限 − 这台机器关掉的。
///
/// `off` 里上限之外的位**不会**被打开（上限是硬的），也不会被当成新能力——它们只是
/// 在 [`HostFacts::excess_off_bits`] 里被报成外壳 bug。
pub fn effective(facts: &HostFacts) -> CapabilitySet {
    let mut set = host_ceiling(facts.host);
    for bit in facts.off.keys() {
        set = set.without(*bit);
    }
    set
}

/// 给界面 / 出口看的一份完整报告（也进 `Snapshot`）。serde 形态就是 S1 `describe_endpoint`
/// 输出里 `capabilities` 那一格的形状。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CapabilityReport {
    /// 这份报告是哪一档宿主算出来的（§2.5.0 第 1 步）。S1 的 `describe_endpoint` 用它回答
    /// "这台设备是什么"；`Composition::missing_on` 用它判 `HostMismatch`。
    ///
    /// **注意字段名**：`host` 这个键在本结构里一直是**宿主机位表**，档位叫 `tier`。
    pub tier: HostKind,
    /// 宿主 / 设备位：每个位都给状态（开着的也在，界面要按"有几位、关了几位"排版）。
    pub host: BTreeMap<Capability, CapabilityStatus>,
    /// provider 位，按当前 Speak 选中的 provider 解析。
    pub speak: BTreeMap<Capability, CapabilityStatus>,
    /// provider 位，按当前 Listen 选中的 provider 解析。
    pub listen: BTreeMap<Capability, CapabilityStatus>,
}

impl CapabilityReport {
    /// 从宿主事实 + 两条腿各自选的 provider 算出完整报告（§2.5.0 第 4–5 步）。
    pub fn of(facts: &HostFacts, speak: ModelProvider, listen: ModelProvider) -> Self {
        let ceiling = host_ceiling(facts.host);
        let available = effective(facts);
        let mut host = BTreeMap::new();
        for bit in Capability::HOST {
            host.insert(*bit, status_of(ceiling, available, facts, *bit));
        }
        Self {
            tier: facts.host,
            host,
            speak: provider_statuses(speak),
            listen: provider_statuses(listen),
        }
    }

    /// 这个宿主位开着没。报告里每位都有条目；真缺条目时按"关着"算（保守）。
    pub fn host_enabled(&self, bit: Capability) -> bool {
        self.host.get(&bit).is_some_and(|status| status.enabled)
    }
}

fn status_of(
    ceiling: CapabilitySet,
    available: CapabilitySet,
    facts: &HostFacts,
    bit: Capability,
) -> CapabilityStatus {
    if available.contains(bit) {
        return CapabilityStatus::ON;
    }
    // 上限里根本没有这一位：**结构上做不到**，外壳说什么都不算（`off` 里塞了也不算数）。
    let reason = if !ceiling.contains(bit) {
        UnavailableReason::Unsupported
    } else {
        // 上限里有、这台机器关掉了：用外壳报的原因。
        facts
            .off
            .get(&bit)
            .copied()
            .unwrap_or(UnavailableReason::Unsupported)
    };
    CapabilityStatus::off(reason)
}

fn provider_statuses(provider: ModelProvider) -> BTreeMap<Capability, CapabilityStatus> {
    let available = crate::catalog::provider_capabilities(provider);
    Capability::PROVIDER
        .iter()
        .map(|bit| {
            let status = if available.contains(*bit) {
                CapabilityStatus::ON
            } else {
                CapabilityStatus::off(UnavailableReason::Unsupported)
            };
            (*bit, status)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(host: HostKind) -> HostFacts {
        HostFacts {
            host,
            off: BTreeMap::new(),
            virtual_mic_device: None,
        }
    }

    fn report_for(host: HostKind) -> CapabilityReport {
        CapabilityReport::of(&facts(host), ModelProvider::Aliyun, ModelProvider::Aliyun)
    }

    #[test]
    fn every_bit_has_one_bucket_and_a_stable_id() {
        // 三个列表必须正好切分 ALL（漏一个位 = 位表与代码漂移）。
        assert_eq!(
            Capability::HOST.len() + Capability::PROVIDER.len(),
            Capability::ALL.len()
        );
        for bit in Capability::ALL {
            let expected = match bit.scope() {
                CapabilityScope::Host => Capability::HOST,
                CapabilityScope::Provider => Capability::PROVIDER,
            };
            assert!(expected.contains(bit), "{bit:?} 不在它自己那一桶里");
            // 判别式 = 位图下标，`ALL` 的顺序就是下标顺序：任何一位都不许指着别人。
            assert_eq!(Capability::ALL[*bit as usize], *bit);
            // `id()` 是 JSON 键名（界面按它查文案），与 serde 名字必须一字不差。
            let json = serde_json::to_value(bit).expect("位名要能序列化");
            assert_eq!(
                json.as_str(),
                Some(bit.id()),
                "{bit:?} 的 id 与 serde 名字漂了"
            );
        }
        assert_eq!(Capability::ALL.len(), 19);
    }

    #[test]
    fn four_tiers_each_have_a_ceiling_row() {
        let ceilings: Vec<CapabilitySet> = HostKind::ALL.into_iter().map(host_ceiling).collect();
        for (host, ceiling) in HostKind::ALL.into_iter().zip(&ceilings) {
            assert!(!ceiling.is_empty(), "{host:?} 的上限是空的");
        }
        // 四档互不相同：任何两档都能指出一个"你有我没有"的位。
        for (i, a) in ceilings.iter().enumerate() {
            for b in &ceilings[i + 1..] {
                assert!(
                    !a.difference(*b).union(b.difference(*a)).is_empty(),
                    "两档上限一模一样：{a:?} / {b:?}",
                );
            }
        }
        // 只有 Windows 编得进头显字幕：它是"这个构建编没编进去"，别的档位结构上就没有。
        assert!(host_ceiling(HostKind::Windows).contains(Capability::VrCaptions));
        for host in [
            HostKind::LinuxDesktop,
            HostKind::Android,
            HostKind::LinuxHeadless,
        ] {
            assert!(
                !host_ceiling(host).contains(Capability::VrCaptions),
                "{host:?}"
            );
        }
    }

    #[test]
    fn android_ceiling_has_no_virtual_mic() {
        let android = host_ceiling(HostKind::Android);
        assert!(!android.contains(Capability::VirtualMic));
        assert!(!android.contains(Capability::ProgramTap));
        // 但"这台设备能出声、能看字幕"照旧——不是把这条腿从架构里去掉。
        assert!(android.contains(Capability::Mic));
        assert!(android.contains(Capability::Captions));
    }

    #[test]
    fn linux_ceiling_has_virtual_mic_without_an_install_step() {
        let linux = host_ceiling(HostKind::LinuxDesktop);
        assert!(linux.contains(Capability::VirtualMic));
        // 没有"装个驱动"这一步：Linux 上虚拟麦就是 PipeWire 图里的一个节点。
        // 所以这一位只会被 `not_wired`（还没接）/ `unsupported`（节点建不出来）关掉，
        // 绝不会是 `not_installed`——那是 Windows 的 VB-CABLE 才有的说法。
        let report = CapabilityReport::of(
            &HostFacts {
                host: HostKind::LinuxDesktop,
                off: BTreeMap::from([(Capability::VirtualMic, UnavailableReason::NotWired)]),
                virtual_mic_device: None,
            },
            ModelProvider::Aliyun,
            ModelProvider::Aliyun,
        );
        assert_eq!(
            report.host[&Capability::VirtualMic].reason,
            Some(UnavailableReason::NotWired),
        );
    }

    #[test]
    fn not_wired_turns_the_bit_off_but_keeps_the_tier() {
        let host = HostKind::LinuxDesktop;
        let wired = report_for(host);
        assert!(
            wired.host_enabled(Capability::VirtualMic),
            "接线后这一位该是开的"
        );

        let pre_wiring = CapabilityReport::of(
            &HostFacts {
                host,
                off: BTreeMap::from([(Capability::VirtualMic, UnavailableReason::NotWired)]),
                virtual_mic_device: None,
            },
            ModelProvider::Aliyun,
            ModelProvider::Aliyun,
        );
        assert!(!pre_wiring.host_enabled(Capability::VirtualMic));
        assert_eq!(
            pre_wiring.host[&Capability::VirtualMic].reason,
            Some(UnavailableReason::NotWired)
        );
        // 上限里**有**这一位 ⇒ 这是"还没接上"，不是"平台做不到"（§2.5.1 读表须知第 1 条）。
        assert!(host_ceiling(host).contains(Capability::VirtualMic));
        assert_eq!(pre_wiring.tier, host);
    }

    #[test]
    fn the_uninjected_default_does_not_claim_unproven_bits() {
        let facts = HostFacts::uninjected();
        let report = CapabilityReport::of(&facts, ModelProvider::Aliyun, ModelProvider::Aliyun);

        // 这四位都要外壳多做一步（装驱动 / 建节点 / 有托盘宿主 / 拿到 `input` 组）才算数：
        // 没有事实就报"还没接上"，而不是默认全开。
        for bit in [
            Capability::VirtualMic,
            Capability::ProgramTap,
            Capability::Tray,
            Capability::GlobalHotkey,
        ] {
            assert_eq!(
                report.host[&bit],
                CapabilityStatus::off(UnavailableReason::NotWired),
                "{bit:?} 没有事实时不许报开着",
            );
            // 上限里有这一位 ⇒ 报的是"还没接上"，不是"这一档做不到"。
            assert!(
                host_ceiling(HostKind::LinuxDesktop).contains(bit),
                "{bit:?}"
            );
        }
        // 关的都是上限之内的位：缺省本身不是外壳 bug。
        assert!(facts.excess_off_bits().is_empty());

        // 不靠"接线的"几位照旧：缺省连采集 / 字幕都关掉的话，芯被单独使用时就没有基准了。
        for bit in [
            Capability::Mic,
            Capability::Captions,
            Capability::BackgroundService,
        ] {
            assert!(report.host_enabled(bit), "{bit:?} 该按上限算开着");
        }
    }

    #[test]
    fn facts_can_only_turn_bits_off() {
        let host = HostKind::Windows;
        let ceiling = host_ceiling(host);

        // 上限之内关掉：生效，且不会多出别的位。
        let facts = HostFacts {
            host,
            off: BTreeMap::from([
                (Capability::VirtualMic, UnavailableReason::NotInstalled),
                (Capability::VrCaptions, UnavailableReason::NotBuilt),
            ]),
            virtual_mic_device: None,
        };
        let available = effective(&facts);
        assert!(!available.contains(Capability::VirtualMic));
        assert!(!available.contains(Capability::VrCaptions));
        // 只关掉报上来的那两位：没被点名的位照旧开着。
        assert!(available.contains(Capability::Captions));
        assert!(
            available.difference(ceiling).is_empty(),
            "有效位不许跑到上限外面"
        );
        assert!(facts.excess_off_bits().is_empty());

        // 上限之外：关不掉（上限是硬的），但会被报成外壳 bug。
        let lying = HostFacts {
            host,
            off: BTreeMap::from([(Capability::NetIn, UnavailableReason::Busy)]),
            virtual_mic_device: None,
        };
        assert!(!effective(&lying).contains(Capability::NetIn));
        assert_eq!(
            lying.excess_off_bits(),
            CapabilitySet::single(Capability::NetIn)
        );
        // 外壳说"忙"也不算数：这一档结构上就没有网络进，报告只能说 `unsupported`。
        let report = CapabilityReport::of(&lying, ModelProvider::Aliyun, ModelProvider::Aliyun);
        assert_eq!(
            report.host[&Capability::NetIn],
            CapabilityStatus::off(UnavailableReason::Unsupported),
        );
        assert!(report.host.values().all(|status| status.is_consistent()));
    }

    #[test]
    fn the_report_carries_its_tier() {
        for host in HostKind::ALL {
            let facts = facts(host);
            let report = CapabilityReport::of(&facts, ModelProvider::Aliyun, ModelProvider::Gpt);
            assert_eq!(report.tier, facts.host, "报告的档位必须就是注入事实的档位");
            // 宿主机位表每位都有条目，provider 位表按腿各解析一次。
            assert_eq!(report.host.len(), Capability::HOST.len());
            assert_eq!(report.speak.len(), Capability::PROVIDER.len());
            assert_eq!(report.listen.len(), Capability::PROVIDER.len());
        }
    }

    #[test]
    fn the_four_placeholder_providers_report_off() {
        // 占名的 4 位：只进枚举，不进 `catalog/*.json`（§2.5.1 已拍）。谁都不许报 `true`。
        for provider in ModelProvider::ALL {
            for bit in [
                Capability::UsageReporting,
                Capability::SpeechActivity,
                Capability::TurnEnd,
                Capability::SourceTranscript,
            ] {
                assert!(
                    !crate::catalog::supports(provider, bit),
                    "{provider:?} 不该有 {bit:?}：它还没有定义者",
                );
            }
        }
    }

    #[test]
    fn the_report_serializes_into_the_describe_shape() {
        // S1 的 `describe_endpoint` 直接把它当 `capabilities` 那一格：`tier` 是档位字符串，
        // `host` / `speak` / `listen` 是位表（键 = 位名，值 = `{enabled, reason}`）。
        let report = report_for(HostKind::Android);
        let json = serde_json::to_value(&report).expect("报告要能送上线路");
        assert_eq!(json["tier"], serde_json::json!("android"));
        for (table, bits) in [
            ("host", Capability::HOST),
            ("speak", Capability::PROVIDER),
            ("listen", Capability::PROVIDER),
        ] {
            let entries = json[table]
                .as_object()
                .unwrap_or_else(|| panic!("{table} 该是位表"));
            assert_eq!(entries.len(), bits.len(), "{table} 每位都要有条目");
            assert!(
                bits.iter().all(|bit| entries.contains_key(bit.id())),
                "{table} 的键必须就是位名：{entries:?}",
            );
        }
        // 手机没有虚拟麦：位假 + 原因（界面与出口都按这一对降级，不是只看那个布尔）。
        assert_eq!(
            json["host"]["virtual_mic"],
            serde_json::json!({ "enabled": false, "reason": "unsupported" }),
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn the_report_and_the_facts_export_schemas_from_the_types() {
        let report =
            serde_json::to_value(schemars::schema_for!(CapabilityReport)).expect("schema 是数据");
        assert_eq!(
            report["properties"]
                .as_object()
                .map(|properties| properties.keys().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["host", "listen", "speak", "tier"]),
            "`tier` 是档位、其余三张是位表——四个键既不能混也不能少",
        );
        assert_eq!(
            report["definitions"]["HostKind"]["enum"],
            serde_json::json!(["windows", "linux_desktop", "android", "linux_headless"]),
        );
        assert_eq!(
            ref_target(&report["properties"]["host"]["additionalProperties"]),
            Some("#/definitions/CapabilityStatus"),
            "位表的键是位名、值是状态",
        );
        // 关掉的原因要枚举齐：界面按它查文案，S1 按 `permission` 判"OS 没授权"。
        let reasons: Vec<&str> = report["definitions"]["UnavailableReason"]["oneOf"]
            .as_array()
            .expect("原因该是枚举")
            .iter()
            .filter_map(|variant| variant["enum"][0].as_str())
            .collect();
        assert_eq!(
            reasons,
            vec![
                "unsupported",
                "not_installed",
                "permission",
                "not_built",
                "not_wired",
                "pending_reboot",
                "busy",
            ],
        );

        // 外壳填给账本的那份事实（S1 的 `describe` 要能把它当数据看）。
        // `virtual_mic_device` 可缺（`Option`）——所以不在 `required` 里，但属性本身要在。
        let facts = serde_json::to_value(schemars::schema_for!(HostFacts)).expect("schema 是数据");
        assert_eq!(facts["required"], serde_json::json!(["host", "off"]));
        assert_eq!(
            ref_target(&facts["properties"]["host"]),
            Some("#/definitions/HostKind"),
        );
        assert_eq!(
            ref_target(&facts["properties"]["off"]["additionalProperties"]),
            Some("#/definitions/UnavailableReason"),
        );
        assert_eq!(
            facts["properties"]["virtual_mic_device"]["type"],
            serde_json::json!(["string", "null"]),
            "设备名可缺（没接线时）",
        );
    }

    /// schemars 0.8 对带文档注释的字段有时包一层 `allOf`——两种形态都要取得到 `$ref`。
    #[cfg(feature = "json-schema")]
    fn ref_target(property: &serde_json::Value) -> Option<&str> {
        property
            .get("$ref")
            .or_else(|| property.get("allOf")?.get(0)?.get("$ref"))
            .and_then(serde_json::Value::as_str)
    }
}
