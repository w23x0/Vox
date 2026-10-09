//! 百炼实时翻译「现场探针」——维护表重构的第 0 步。**只加工具，不改产品行为。**
//!
//! 它回答的是动手重构之前那几个没核实过的猜测：qwen3.8 的音色到底收在
//! `audio.output.voice` 还是老的 `voice`；3.8 收不收 `input_audio_transcription.language`；
//! 国内旧地址 / 国际站（`dashscope-intl`）/ 工作空间专属地址分别能不能连 3.8、3.5。
//! **工具本身不预设答案**——它只把指定形状的 `session.update` 原样发出去，把服务端回的话原样摊开。
//!
//! 放在 `vox-net`：这里已经同时有 `vox_core::cloud::protocol`（编解码）和
//! `WsTransport`（`vox_core::cloud::Transport` 的生产实现）。探针报的东西因此就是
//! **产品会发的东西**，不是另抄一份报文。运行期依赖表一个包都不加——密钥那条腿走 dev-dependency。
//!
//! ## 用法
//!
//! ```text
//! cargo run -p vox-net --example aliyun_probe -- [开关]
//! ```
//!
//! 密钥：先看环境变量 `DASHSCOPE_API_KEY`；没有就给 `--keyring`，从 Secret Service 读
//! （service `com.vox.app`、user `api-key.aliyun`，跟 `vox-host` 的桌面档一致——
//! 键名直接复用那边的后端选择层，探针不另抄一份）。`--keyring` 那条腿要用
//! `--features probe-keyring` 重编一次。**密钥不出现在任何一行输出里，连接地址里也不带**
//! （地址里带了就直接拒绝跑，不静默发出去）。
//!
//! 不联网也能验证的部分：`--dry-run` 只打印最终连接地址与实际发出的 `session.update`，不连。
//!
//! ## 三个问题分别怎么跑
//!
//! ### 问题 1：qwen3.8 的音色放在 `audio.output.voice` 还是老的 `voice`
//!
//! 两个形状各存一个文件，先 `--dry-run` 看清报文，再一次一个地真连。
//! 哪一帧形状被服务端接受、译文语音（`response.audio.delta`）出不出来，就是那一条对；
//! 被拒的那次 `error` 里的原话是最硬的证据。
//!
//! ```bash
//! cat > /tmp/s38-audio-output-voice.json <<'JSON'
//! {
//!   "output_modalities": ["text", "audio"],
//!   "audio": {
//!     "input": { "format": { "type": "pcm", "sample_rate": 16000 } },
//!     "output": { "format": { "type": "pcm", "sample_rate": 24000 }, "voice": "Tina" }
//!   },
//!   "translation": { "language": "ja" }
//! }
//! JSON
//!
//! cat > /tmp/s38-flat-voice.json <<'JSON'
//! {
//!   "modalities": ["text", "audio"],
//!   "voice": "Tina",
//!   "input_audio_format": "pcm16",
//!   "output_audio_format": "pcm16",
//!   "response_modalities": ["text", "audio"],
//!   "translation": { "language": "ja" }
//! }
//! JSON
//!
//! cargo run -p vox-net --example aliyun_probe -- --dry-run --session-json /tmp/s38-audio-output-voice.json
//! cargo run -p vox-net --example aliyun_probe -- --dry-run --session-json /tmp/s38-flat-voice.json
//!
//! cargo run -p vox-net --example aliyun_probe -- --session-json /tmp/s38-audio-output-voice.json --send-audio probe.wav --listen 12
//! cargo run -p vox-net --example aliyun_probe -- --session-json /tmp/s38-flat-voice.json            --send-audio probe.wav --listen 12
//! ```
//!
//! ### 问题 2：qwen3.8 收不收 `input_audio_transcription.language`
//!
//! 给不给源语言各跑一次：不给 = 服务端自动识别（报文里没这个字段），给 = 报文里多这一段。
//! 真连之后看服务端认不认（认 = 没有 `error`，且源语种不再按自动识别那套走）。
//!
//! ```bash
//! cargo run -p vox-net --example aliyun_probe -- --dry-run --target-lang ja --source-lang en --voice Tina
//! cargo run -p vox-net --example aliyun_probe --            --target-lang ja --source-lang en --voice Tina --send-audio probe.wav --listen 12
//! ```
//!
//! ### 问题 3：三个地址分别能不能连 3.8 / 3.5
//!
//! `--region` 换地区缺省地址，`--endpoint` 直接给地址（工作空间专属地址就是它）。
//! 两个地址 × 两个模型：
//!
//! ```bash
//! # 国内旧地址
//! cargo run -p vox-net --example aliyun_probe -- --region cn   --model qwen3.8-livetranslate-flash-realtime --listen 8
//! cargo run -p vox-net --example aliyun_probe -- --region cn   --model qwen3.5-livetranslate-flash-realtime --listen 8
//! # 国际站
//! cargo run -p vox-net --example aliyun_probe -- --region intl --model qwen3.8-livetranslate-flash-realtime --listen 8
//! cargo run -p vox-net --example aliyun_probe -- --region intl --model qwen3.5-livetranslate-flash-realtime --listen 8
//! # 工作空间专属地址（<ws-id> 换成百炼控制台给的那串）
//! cargo run -p vox-net --example aliyun_probe -- \
//!   --endpoint wss://<ws-id>.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime \
//!   --model qwen3.8-livetranslate-flash-realtime --listen 8
//! ```
//!
//! **模型名一律原样进地址**，不过目录归一：`--model` 给了目录里没有的名字
//! （`gummy-realtime-v1` 就是）时，探针照样照发，只额外打一行提醒——产品会把它换成
//! 缺省模型，所以那条线报出来的地址只有探针会发。归一发生在哪一步、地址长什么样，
//! 报告里一眼能看见。
//!
//! ### 问题 4：这一族到底有哪些模型、哪些还在服务
//!
//! `--list-models` 不连实时翻译，走的是另一个服务：
//! `GET {base}/api/v1/models`，`Authorization: Bearer <key>`，带
//! `capabilities=Realtime-Audio-Translate`、`page_size=100`，按 `page_no` 翻到
//! `output.total` 取完。每个模型一行：
//! `model | name | published_time | equivalent_snapshot | offline_time | capabilities | request→response modality`。
//!
//! 字段名 2026-10-09 真连核过一遍（国内站密钥，`output.models[]`）：
//! 模态在 `inference_metadata.request_modality` / `response_modality`（**单数**），
//! 下线时间在 `inference_offline_info.offline_time`（可以是 `null`）。
//!
//! ```bash
//! # 国内站；--dry-run 只打将要请求的 URL（不带密钥），不连
//! cargo run -p vox-net --example aliyun_probe -- --list-models --dry-run
//! cargo run -p vox-net --example aliyun_probe -- --list-models
//! # 国际站 / 香港站（hk 只有列表这一档用；连 realtime 时给 hk 会当场报错）
//! cargo run -p vox-net --example aliyun_probe -- --list-models --region intl
//! cargo run -p vox-net --example aliyun_probe -- --list-models --region hk
//! # 核对 capabilities 那个过滤值有没有漏掉模型：不过滤，只打印名字像实时翻译的
//! cargo run -p vox-net --example aliyun_probe -- --list-models --all-capabilities
//! ```
//!
//! **响应结构对不上就报错**：整页结构认不出来时把（去密钥后的）原文摊开，照着改
//! `parse_models_page`；个别字段认不出打 `—`（当"服务端没给"）。
//!
//! ## 退出码
//!
//! | 码 | 含义 |
//! |----|------|
//! | 0 | 连上了，而且收到了 `session.updated`；或者列表取完了 |
//! | 2 | 参数错 / 密钥取不到 / 文件读不了（**没发出任何请求**）|
//! | 3 | 收到了 `error` / `response.error` |
//! | 4 | 连不上（握手失败），在 `session.updated` 之前连接被关；或列表取不到 |
//! | 5 | 连上了，但 `--listen` 到点还没等到 `session.updated` |

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use vox_core::catalog;
use vox_core::cloud::protocol::{
    self, ClientEvent, Decoder, ServerEvent, SessionParams, INPUT_SAMPLE_RATE,
};
use vox_core::cloud::{ConnectRequest, Incoming, Transport};
use vox_core::settings::ModelProvider;
use vox_net::WsTransport;

/// 音频分片的毫秒数。**探针自己的节奏选择**：协议只规定"一段 PCM16 append 上去"，
/// 不规定一段多长；产品那边这个长度由重采样链的块决定（`pipeline::feed_chain`），
/// 跟协议无关，所以这里选一个常见的 20 ms 并写死，不去猜产品内部的值。
const BLOCK_MS: u32 = 20;
/// 每片的样本数（16 kHz × 20 ms）。
const BLOCK_SAMPLES: usize = INPUT_SAMPLE_RATE as usize * BLOCK_MS as usize / 1000;
/// `recv` 的轮询步长。连接活着但没消息时安静地继续等，不当成出错。
const RECV_POLL_MS: u32 = 200;
/// `session.update` 的事件名。跟 `ClientEvent::SessionUpdate(..).type_name()` 一致，
/// `session_update_name_matches_the_protocol_enum` 那条把两者钉在一起。
const SESSION_UPDATE_TYPE: &str = "session.update";
/// 环境变量名，跟 `protocol::ENDPOINT_ENV` 一致——报出来是给人看的，原话更省事。
const ENDPOINT_ENV: &str = protocol::ENDPOINT_ENV;

/// 退出码。跟本文件头那张表、`--help` 一一对应。
mod exit {
    /// 连上了，而且收到了 `session.updated`。
    pub const OK: u8 = 0;
    /// 参数错 / 密钥取不到 / 文件读不了——**没发出任何请求**。
    pub const USAGE: u8 = 2;
    /// 收到了 `error` / `response.error`。
    pub const SERVER_ERROR: u8 = 3;
    /// 连不上，或在 `session.update` 之前连接被关。
    pub const NO_CONNECTION: u8 = 4;
    /// 连上了，但监听窗口到点还没等到 `session.updated`。
    pub const TIMEOUT: u8 = 5;
}

/// 探针自己的一份"地区"。**不去复用 `settings::AliyunRegion`**：那边只有 cn / intl
/// 两个值（产品目前没有香港站这条线），而 `--list-models` 要能问香港站。改 `vox-core`
/// 不在本工单范围内，所以在这里多带一档，两边各自挑自己那张表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeRegion {
    Cn,
    Intl,
    /// 香港站。**只有 `--list-models` 用它**；连 realtime 时给 hk 当场报错，
    /// 因为产品里根本没有这个地区的产品路径（`endpoint_url_with` 只有两档），
    /// 悄悄连到别处就是替 Main 回答了另一个问题。
    Hk,
}

impl ProbeRegion {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "cn" => Ok(Self::Cn),
            "intl" => Ok(Self::Intl),
            "hk" => Ok(Self::Hk),
            other => Err(format!("`--region` 只认 cn / intl / hk，收到 {other:?}")),
        }
    }

    /// realtime 的 WebSocket 基址。`--region hk` 在这里报错（见类型上的注释）。
    fn realtime_base(self) -> Result<&'static str, String> {
        match self {
            Self::Cn => Ok(catalog::API_BASE),
            Self::Intl => Ok(catalog::API_BASE_INTL),
            Self::Hk => Err(
                "`--region hk` 只是 `--list-models` 的事：产品的实时翻译只有国内站与国际站\
                 两档（`catalog::API_BASE` / `API_BASE_INTL`），没有香港站这条实时路径。\
                 连 realtime 请用 `--region cn|intl`，或者直接给 `--endpoint`。"
                    .to_string(),
            ),
        }
    }

    /// `--list-models` 的 HTTPS 基址。
    ///
    /// **刻意不从 `catalog::API_BASE` 推**：那边是 `wss://…/api-ws/v1/realtime`，
    /// 模型列表走的是另一个服务（`https://…` + `/api/v1/models`），协议与路径都不同。
    fn list_base(self) -> &'static str {
        match self {
            Self::Cn => "https://dashscope.aliyuncs.com",
            Self::Intl => "https://dashscope-intl.aliyuncs.com",
            Self::Hk => "https://cn-hongkong.dashscope.aliyuncs.com",
        }
    }
}

/// `--send-audio` 的两种来源。
#[derive(Debug)]
enum SendAudio {
    /// 读一个 16 kHz / PCM16 / 单声道 WAV。
    Wav(PathBuf),
    /// `silence:<秒>`：全零。
    Silence(f32),
}

impl SendAudio {
    /// 参数原样描一遍——`--dry-run` 要用它说明"本来会发多少"。
    fn describe(&self) -> String {
        match self {
            Self::Wav(path) => format!("WAV {}", path.display()),
            Self::Silence(seconds) => format!("{seconds} 秒静音"),
        }
    }
}

#[derive(Debug)]
struct Args {
    model: String,
    region: ProbeRegion,
    /// 覆盖地区的完整连接地址（工作空间专属地址就是它）。
    endpoint: Option<String>,
    /// 原样当 `session` 对象发出去的文件。
    session_json: Option<PathBuf>,
    /// `None` = 没给 `--target-lang`（用目录缺省）。
    target_lang: Option<String>,
    source_lang: Option<String>,
    voice: Option<String>,
    /// `--audio on` 要语音，`--audio off` 只要文字。
    audio: bool,
    send_audio: Option<SendAudio>,
    listen: Duration,
    keyring: bool,
    dry_run: bool,
    /// 不经 tungstenite 解帧，直接摊开服务端发来的原始字节（诊断“控制帧过大”之类解码层先炸的问题）。
    raw: bool,
    help: bool,
    // ── `--list-models` 那一档 ────────────────────────────────────────────
    /// 查阿里云官方的模型列表，不连 realtime。
    list_models: bool,
    /// 覆盖 `--list-models` 的 HTTPS 基址。
    list_base: Option<String>,
    /// 透传成 query 的 `service_site`。
    service_site: Option<String>,
    /// 不带 `capabilities` 过滤（用来核对过滤值有没有漏掉模型）。
    all_capabilities: bool,
    /// 命令行上**真的出现过**的开关，按出现顺序。
    ///
    /// 为什么要记这个：`--audio off`、`--model` 这几格都有缺省值，光看解析完的
    /// `Args` 分不出"用户没给"和"用户给了正好等于缺省"。`--list-models` 模式要
    /// 拒掉那一堆 realtime 开关，靠的就是这个——默默忽略一半会让人以为那一半生效了。
    given: Vec<String>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        Self::parse_from(std::env::args().skip(1))
    }

    /// 参数解析拆成"吃一段 argv"，好让单测能真的走一遍解析（`-h` 的归一、
    /// `--list-models` 的白名单都发生在这里）。
    fn parse_from<I: Iterator<Item = String>>(argv: I) -> Result<Self, String> {
        let mut argv = argv;
        let mut args = Args {
            // 缺省取目录里的当前缺省模型——探针默认要跑的就是产品现在跑的那条。
            model: catalog::DEFAULT_MODEL_NAME.to_string(),
            region: ProbeRegion::Cn,
            endpoint: None,
            session_json: None,
            target_lang: None,
            source_lang: None,
            voice: None,
            audio: true,
            send_audio: None,
            listen: Duration::from_secs(8),
            keyring: false,
            dry_run: false,
            raw: false,
            help: false,
            list_models: false,
            list_base: None,
            service_site: None,
            all_capabilities: false,
            given: Vec::new(),
        };
        while let Some(flag) = argv.next() {
            let mut value = |what: &str| -> Result<String, String> {
                argv.next().ok_or_else(|| format!("{what} 后面要跟值"))
            };
            match flag.as_str() {
                "--help" | "-h" => args.help = true,
                "--model" => args.model = value("--model")?,
                "--region" => args.region = ProbeRegion::parse(&value("--region")?)?,
                "--endpoint" => args.endpoint = Some(value("--endpoint")?),
                "--session-json" => {
                    args.session_json = Some(PathBuf::from(value("--session-json")?))
                }
                "--target-lang" => args.target_lang = Some(value("--target-lang")?),
                "--source-lang" => args.source_lang = Some(value("--source-lang")?),
                "--voice" => args.voice = Some(value("--voice")?),
                "--audio" => {
                    args.audio = match value("--audio")?.as_str() {
                        "on" => true,
                        "off" => false,
                        other => return Err(format!("`--audio` 只认 on / off，收到 {other:?}")),
                    };
                }
                "--send-audio" => {
                    args.send_audio = Some(parse_send_audio(&value("--send-audio")?)?)
                }
                "--listen" => {
                    let raw = value("--listen")?;
                    let seconds: f32 = raw
                        .parse()
                        .map_err(|e| format!("`--listen {raw}` 不是秒数：{e}"))?;
                    // `is_finite` 那半截是给 NaN 用的：`--listen nan` 会被 parse 收下，
                    // 而 `seconds <= 0.0` 对 NaN 恒假，那样就能混过去变成一个永不到点的窗口。
                    if !seconds.is_finite() || seconds <= 0.0 {
                        return Err("`--listen` 要是一个大于 0 的有限秒数".to_string());
                    }
                    args.listen = Duration::from_secs_f32(seconds);
                }
                "--keyring" => args.keyring = true,
                "--dry-run" => args.dry_run = true,
                "--raw" => args.raw = true,
                "--list-models" => args.list_models = true,
                "--list-base" => args.list_base = Some(value("--list-base")?),
                "--service-site" => args.service_site = Some(value("--service-site")?),
                "--all-capabilities" => args.all_capabilities = true,
                other => return Err(format!("不认识的开关 {other}")),
            }
            // `-h` 归一成 `--help` 记名，后面按名字查的那张白名单才对得上。
            args.given.push(if flag == "-h" {
                "--help".to_string()
            } else {
                flag
            });
        }
        Ok(args)
    }

    /// 语言与音色这几格跟 `--session-json` 互斥：`--session-json` 的意思是
    /// "整个 session 对象照抄"，同时又给要拼进 session 的开关，到底听谁的说不清
    /// → 当场问，不默默忽略一半。
    fn shape_flags(&self) -> Option<&'static str> {
        self.session_json.as_ref()?;
        [
            ("--target-lang", self.target_lang.is_some()),
            ("--source-lang", self.source_lang.is_some()),
            ("--voice", self.voice.is_some()),
        ]
        .into_iter()
        .find(|(_, given)| *given)
        .map(|(flag, _)| flag)
    }

    /// `--list-models` 模式下给了一个 realtime 专用开关 → 当场报错。
    ///
    /// 白名单式：只放行对"查模型列表"真的有意义的开关，其余一律拒。没给开关的那些
    /// 正是最容易出事的一格——`--audio off` 混进来会被默默忽略，而人会以为这一轮
    /// 问的是"文字模式的模型列表"，其实问的是全量。
    fn list_models_stray_flags(&self) -> Option<String> {
        const ALLOWED: [&str; 8] = [
            "--help",
            "--list-models",
            "--region",
            "--list-base",
            "--service-site",
            "--all-capabilities",
            "--keyring",
            "--dry-run",
        ];
        self.given
            .iter()
            .find(|flag| !ALLOWED.contains(&flag.as_str()))
            .cloned()
    }
}

fn parse_send_audio(raw: &str) -> Result<SendAudio, String> {
    // `silence:` 优先：路径里带冒号的写法太反常，不为它开口子。
    if let Some(seconds) = raw.strip_prefix("silence:") {
        let seconds: f32 = seconds
            .parse()
            .map_err(|e| format!("`--send-audio silence:{seconds}` 的秒数认识不了：{e}"))?;
        // 同上：NaN 也要拒，否则 `silence:nan` 会变成一段 0 样本的音频。
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err("`silence:` 后面要是一个大于 0 的有限秒数".to_string());
        }
        return Ok(SendAudio::Silence(seconds));
    }
    if raw.is_empty() {
        return Err("`--send-audio` 要跟 WAV 路径或 `silence:<秒>`".to_string());
    }
    Ok(SendAudio::Wav(PathBuf::from(raw)))
}

/// 拼出来的 session 对象，外加要提醒人的那几行。
#[derive(Debug)]
struct SessionDraft {
    object: Value,
    notes: Vec<String>,
}

fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("参数有问题：{message}");
            usage();
            return ExitCode::from(exit::USAGE);
        }
    };
    if args.help {
        usage();
        return ExitCode::from(exit::OK);
    }
    if let Some(flag) = args.shape_flags() {
        eprintln!(
            "`--session-json` 已经覆盖了整个 session 对象，不能再跟 {flag} 一起给\
             （`--model` / `--region` / `--endpoint` 只管地址，仍然有效）。"
        );
        return ExitCode::from(exit::USAGE);
    }
    if !args.audio && args.voice.is_some() {
        eprintln!("`--audio off` 是只要文字，又给了 `--voice`——这两条互相打架，二选一。");
        return ExitCode::from(exit::USAGE);
    }
    if args.list_models {
        return run_list_models(&args);
    }

    let url = match build_endpoint(&args) {
        Ok(url) => url,
        Err(message) => {
            eprintln!("连接地址有问题：{message}");
            return ExitCode::from(exit::USAGE);
        }
    };
    let draft = match build_session_object(&args) {
        Ok(draft) => draft,
        Err(message) => {
            eprintln!("session 对象有问题：{message}");
            return ExitCode::from(exit::USAGE);
        }
    };

    // ── 密钥与凭据检查，**在第一行 println 之前** ─────────────────────────
    //
    // 下面紧接着就是 `println!("最终连接地址：{url}")`。凭据检查排在它后面等于没有：
    // 地址一旦带了密钥，那一行就已经把它写进终端回滚缓冲了，中止也已经晚了。
    // `--dry-run` 不取密钥（它压根不连），所以这道检查只在真跑时才有。
    let api_key = if args.dry_run {
        None
    } else {
        match resolve_api_key(&args) {
            Ok(key) => Some(key),
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(exit::USAGE);
            }
        }
    };
    // 防"地址里塞的是密钥原文、没带 key=／token= 字样"——`carries_a_credential`
    // 按键名认不出这种，只能拿密钥原文对一遍。
    if let Some(key) = &api_key {
        if url.contains(key.as_str()) {
            eprintln!("连接地址里带上了密钥，已中止——密钥不许出现在地址里。");
            return ExitCode::from(exit::USAGE);
        }
    }

    // ── 到这儿才把要发的东西摊开给人看（连不连都先看） ────────────────────
    println!("最终连接地址：{url}");
    if let Some(env_base) = std::env::var(ENDPOINT_ENV).ok().filter(|v| !v.is_empty()) {
        if !url.starts_with(env_base.trim_end_matches('/')) {
            println!(
                "注意：环境变量 {ENDPOINT_ENV}={env_base} 在产品里会覆盖地区地址；\
                 探针以 `--endpoint` / `--region` 为准。"
            );
        }
    }
    let product_model = catalog::normalize_model_for(ModelProvider::Aliyun, &args.model);
    if product_model != args.model {
        println!(
            "注意：模型 {:?} 匹配不到方言。产品（`catalog::normalize_model_for`）会把它换成 \
             {product_model:?}，上面这条地址只有探针会发——要的就是这个（gummy / qwen3 之类\
             不在维护表里）。",
            args.model
        );
    }
    for note in &draft.notes {
        println!("注意：{note}");
    }
    let session_update = session_update_frame(&draft.object, "event_0_1");
    println!("发出的 session.update（原文）：{session_update}");

    // 音频计划也要先说清楚：读文件这一步跟连不连无关，坏文件当场就报错。
    let audio = match load_audio(&args) {
        Ok(audio) => audio,
        Err(message) => {
            eprintln!("`--send-audio` 有问题：{message}");
            return ExitCode::from(exit::USAGE);
        }
    };
    match (&audio, &args.send_audio) {
        (Some(samples), Some(source)) => println!(
            "音频计划：{}，共 {} 个样本（{:.2} 秒），按 {BLOCK_MS} ms 一片分 {} 片 append",
            source.describe(),
            samples.len(),
            samples.len() as f32 / INPUT_SAMPLE_RATE as f32,
            samples.len().div_ceil(BLOCK_SAMPLES),
        ),
        _ => println!("音频计划：不发音频（不给 `--send-audio`）"),
    }

    if args.dry_run {
        println!("--dry-run：不连、不取密钥。");
        return ExitCode::from(exit::OK);
    }

    let api_key = api_key.expect("不是 --dry-run 就一定解析出了密钥");
    run(&args, &url, &session_update, &api_key, audio)
}

/// 连上去、发会话、灌音频、听事件，最后给一个能区分的退出码。
fn run(
    args: &Args,
    url: &str,
    session_update: &str,
    api_key: &str,
    audio: Option<Vec<f32>>,
) -> ExitCode {
    if args.raw {
        return run_raw(args, url, session_update, api_key);
    }
    let mut transport = match WsTransport::standalone() {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("起不了网络运行时：{e}");
            return ExitCode::from(exit::NO_CONNECTION);
        }
    };
    let request = ConnectRequest {
        url: url.to_string(),
        auth_header: protocol::auth_header_value(api_key),
    };
    if let Err(e) = transport.connect(&request) {
        // 握手失败的原话（HTTP 状态码等）比"连不上"三个字有用，直接摊开。
        eprintln!("连不上 {url}：{e}");
        transport.close();
        return ExitCode::from(exit::NO_CONNECTION);
    }
    println!("握手成功，接下来听 {:.3} 秒", args.listen.as_secs_f32());

    if let Err(e) = transport.send(session_update) {
        eprintln!("session.update 发不出去：{e}");
        transport.close();
        return ExitCode::from(exit::NO_CONNECTION);
    }

    // 灌完音频再开始掐表：译文是**发完之后**才回来的，窗口算在前面会白白少听几秒。
    let mut seq: u64 = 1;
    let listen_from = Instant::now();
    if let Some(audio) = &audio {
        if let Err(e) = send_audio(&mut transport, audio, &mut seq) {
            eprintln!("音频发到一半断了：{e}");
            transport.close();
            return ExitCode::from(exit::NO_CONNECTION);
        }
        println!(
            "音频发完，接下来只听。Aliyun 现有协议没有客户端「输入到此为止」的事件\
             （`ClientEvent` 只有 {} 和 {} 两样），断句交给服务端的 VAD。",
            SESSION_UPDATE_TYPE,
            ClientEvent::AppendAudio(Vec::new()).type_name()
        );
    }

    let deadline = listen_from + args.listen;
    let mut decoder = Decoder::new();
    let mut saw_session_updated = false;
    let mut index = 0usize;
    let mut audio_deltas = 0usize;
    while Instant::now() < deadline {
        match transport.recv(RECV_POLL_MS) {
            Ok(None) => continue,
            Ok(Some(Incoming::Text(raw))) => {
                index += 1;
                println!("[{index:03}] {}", fold_audio_payload(&raw));
                match decoder.decode(&raw).map(|parsed| parsed.event) {
                    // 分类走产品自己的解码器，跟流水线看到的判据一致。
                    Some(ServerEvent::SessionUpdated) => saw_session_updated = true,
                    Some(ServerEvent::AudioDelta { .. }) => audio_deltas += 1,
                    Some(ServerEvent::Error { .. }) => {
                        println!(
                            "服务端报错，退出码 {}（原话在上面那条）。",
                            exit::SERVER_ERROR
                        );
                        transport.close();
                        return ExitCode::from(exit::SERVER_ERROR);
                    }
                    // 解码器读不出来的（垃圾帧 / 没接的事件）不打断，继续听。
                    _ => {}
                }
            }
            Ok(Some(Incoming::Closed(reason))) => {
                println!("连接被服务端关闭：{reason}");
                transport.close();
                return ExitCode::from(if saw_session_updated {
                    exit::OK
                } else {
                    exit::NO_CONNECTION
                });
            }
            Err(e) => {
                eprintln!("连接出错：{e}");
                transport.close();
                return ExitCode::from(exit::NO_CONNECTION);
            }
        }
    }
    transport.close();

    println!(
        "监听结束：{} 条事件（音频 delta {audio_deltas} 片），session.updated {}。",
        index,
        if saw_session_updated {
            "收到"
        } else {
            "没收到"
        }
    );
    ExitCode::from(if saw_session_updated {
        exit::OK
    } else {
        exit::TIMEOUT
    })
}

/// `--raw`：自己做 TLS，把服务端发来的**未解帧字节**逐段摊开，再交给 tungstenite 解析。
///
/// 为什么要有：tungstenite 遇到超过 125 字节的控制帧会直接报 `Control frame too big`，
/// 帧里到底是什么永远看不到。这条腿在 TLS 之上垫一层只读旁路（只看服务端→客户端方向，
/// 我们发出的字节——含鉴权头——不经过旁路），所以密钥不会出现在输出里；输出另外过一遍 `redact`。
fn run_raw(args: &Args, url: &str, session_update: &str, api_key: &str) -> ExitCode {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    struct Spy<S> {
        inner: S,
        secret: String,
    }
    impl<S: AsyncRead + Unpin> AsyncRead for Spy<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let before = buf.filled().len();
            let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
            if let Poll::Ready(Ok(())) = &polled {
                let fresh = &buf.filled()[before..];
                if !fresh.is_empty() {
                    let shown: String = fresh
                        .iter()
                        .map(|b| {
                            if b.is_ascii_graphic() || *b == b' ' {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    let head: Vec<String> =
                        fresh.iter().take(12).map(|b| format!("{b:02x}")).collect();
                    println!(
                        "RAW <- {} 字节，前 12 字节 {}：{}",
                        fresh.len(),
                        head.join(" "),
                        redact(&shown, &self.secret)
                    );
                }
            }
            polled
        }
    }
    impl<S: AsyncWrite + Unpin> AsyncWrite for Spy<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, data)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("起不了运行时：{e}");
            return ExitCode::from(exit::NO_CONNECTION);
        }
    };
    let listen = args.listen;
    let outcome: Result<(), String> = runtime.block_on(async {
        let mut request = url
            .into_client_request()
            .map_err(|e| format!("地址不合法：{e}"))?;
        let host = request.uri().host().ok_or("地址没有主机名")?.to_string();
        request.headers_mut().insert(
            "Authorization",
            protocol::auth_header_value(api_key)
                .parse()
                .map_err(|_| "鉴权头不合法".to_string())?,
        );
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tcp = tokio::net::TcpStream::connect((host.as_str(), 443))
            .await
            .map_err(|e| format!("TCP 连不上：{e}"))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                host.clone()
                    .try_into()
                    .map_err(|e| format!("主机名不合法：{e}"))?,
                tcp,
            )
            .await
            .map_err(|e| format!("TLS 失败：{e}"))?;
        let spy = Spy {
            inner: tls,
            secret: api_key.to_string(),
        };
        let (mut ws, _) = tokio_tungstenite::client_async(request, spy)
            .await
            .map_err(|e| format!("握手失败：{e}"))?;
        println!("握手成功（--raw），接下来听 {:.3} 秒", listen.as_secs_f32());
        ws.send(tokio_tungstenite::tungstenite::Message::text(
            session_update,
        ))
        .await
        .map_err(|e| format!("session.update 发不出去：{e}"))?;
        let deadline = Instant::now() + listen;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match tokio::time::timeout(left, ws.next()).await {
                Ok(Some(Ok(message))) => {
                    println!("解帧后：{}", redact(&format!("{message:?}"), api_key))
                }
                Ok(Some(Err(e))) => return Err(format!("解帧失败：{e}")),
                Ok(None) => return Err("服务端关闭了连接".to_string()),
                Err(_) => break,
            }
        }
        Ok(())
    });
    match outcome {
        Ok(()) => ExitCode::from(exit::OK),
        Err(message) => {
            println!("{message}");
            ExitCode::from(exit::NO_CONNECTION)
        }
    }
}

/// 把音频的 `delta` 折成一句长度提示。翻译文本原样留——那是这轮要看的正事。
fn fold_audio_payload(raw: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        // 解析不出 JSON 就没法按键名脱敏。地址之外，密钥唯一可能被回显的地方就是
        // 服务端把它原样吐回来；宁可看不见，也不让它进终端回滚缓冲。
        return format!("〈非 JSON 文本，{len} 字节，已折叠〉", len = raw.len());
    };
    let is_audio = value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.ends_with("audio.delta"));
    if !is_audio {
        return raw.to_string();
    }
    let Some(object) = value.as_object_mut() else {
        return raw.to_string();
    };
    if let Some(Value::String(payload)) = object.get("delta").cloned() {
        object.insert("delta".into(), Value::String(base64_hint(&payload)));
    }
    value.to_string()
}

/// base64 载荷折叠成一句可读提示。**只留长度与一小段头**——语音数据里对人有用的
/// 就是"有没有、多长"，几十 KB 的 base64 刷屏只会把真正要看的文字挤走。
/// 按字符切（不是按字节），服务端发来的东西再离谱也不会 panic。
fn base64_hint(payload: &str) -> String {
    const KEEP: usize = 24;
    let len = payload.chars().count();
    let head: String = payload.chars().take(KEEP).collect();
    format!("〈base64 已折叠：原长 {len} 字符，头 {KEEP} 字符是 {head}…〉")
}

/// 拼最终连接地址。
///
/// 跟 `protocol::endpoint_url_with` **故意有一处不同**：那边会把模型名过一遍
/// `catalog::normalize_model`（不认识的换成缺省模型），这边**原样照发**。
/// 探针的全部意义就是能问"产品还不会用的模型"（3.5 之类），悄悄归一等于把问题
/// 换成另一个问题回答。除此之外（去 query、去尾部斜杠、按地区挑缺省地址）行为一致。
///
/// 另外一处**故意更严**：地址不合法时产品会退回地区缺省，探针宁可报错——
/// 用户贴错一个地址然后连到别处，答出来的就不是他要问的那件事了。
fn build_endpoint(args: &Args) -> Result<String, String> {
    let base = match &args.endpoint {
        Some(raw) => sanitize_endpoint(raw)?,
        None => args.region.realtime_base()?.to_string(),
    };
    Ok(format!("{base}?model={}", args.model.trim()))
}

/// 把用户贴来的地址收拾成"干净的基址"：去空白、去自带的 query（模型名要重拼，
/// 不去就会拼出两个 `?`，握手 400）、去尾部斜杠。
fn sanitize_endpoint(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if !(trimmed.starts_with("wss://") || trimmed.starts_with("ws://")) {
        return Err(format!(
            "`--endpoint` 必须以 wss:// 或 ws:// 开头，收到 {trimmed:?}。\
             （产品这时会退回地区缺省地址，探针不这么做——那会答错问题。）"
        ));
    }
    // 先看**原样**的那一份再去 query：凭据检查放在剥离之后等于没检查（query 都被扔了），
    // 用户贴错一个带密钥的地址就会得到一次"成功但没带 key"的连接，他以为鉴权配好了。
    if carries_a_credential(trimmed) {
        return Err(
            "`--endpoint` 的地址里带着 key=…／token=…。密钥只走 Authorization 头，\
             地址里不许带。去掉再试。"
                .to_string(),
        );
    }
    // `user@` / `user:pass@` 那种 userinfo 形式：`carries_a_credential` 按键名认不出来，
    // 而探针第一件事就是把地址原样 `println!` 给人看——它会一路打到终端回滚缓冲里。
    // 所以在这里直接拒，而不是等 `url.contains(&api_key)` 那道更晚的检查（那道只认密钥原文）。
    if let Some(userinfo) = authority_userinfo(trimmed) {
        return Err(credential_in_url_error("--endpoint", userinfo));
    }
    let stripped = trimmed
        .split('?')
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    let scheme_len = trimmed.find("://").map(|i| i + 3).unwrap_or(trimmed.len());
    if stripped.len() <= scheme_len {
        return Err(format!(
            "`--endpoint {trimmed:?}` 去掉 query 之后没有主机名"
        ));
    }
    Ok(stripped.to_string())
}

/// 取 URL 的 authority 段里 `@` 之前的 userinfo，没有就返回 `None`。
///
/// authority = `://` 之后、第一个 `/` / `?` / `#` 之前那一段。**只看这一段**：
/// 路径里出现 `@`（`/api/@v1`）是合法的，全串找 `@` 会误伤。
fn authority_userinfo(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.find(['/', '?', '#']).map_or(rest, |i| &rest[..i]);
    // 最后一个 `@` 才是分隔符：`user:pa@ss@host` 这种畸形写法也照 `user:pa@ss` 拒。
    authority.rsplit_once('@').map(|(userinfo, _host)| userinfo)
}

/// "地址里带凭据"的那条错误消息。**只报长度，不报内容**——这里复述出来就等于把密钥
/// 又打了一遍，只不过换了个地方。这条错误自己就是泄漏路径，所以它比它要拦的东西还严格。
fn credential_in_url_error(flag: &str, userinfo: &str) -> String {
    format!(
        "`{flag}` 的地址里带着 userinfo（`用户@主机`／`用户:密码@主机` 那种形式，\
         {len} 个字符）。不复述它的内容——那正是凭据。\
         地址里不许带凭据，密钥只走 Authorization 头。去掉再试。",
        len = userinfo.chars().count()
    )
}

/// 地址里有没有疑似塞了凭据。只在真正可疑的键名上报，免得误伤 `{WorkspaceId}` 这类写法。
fn carries_a_credential(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    [
        "?key=",
        "&key=",
        "?token=",
        "&token=",
        "accesskey",
        "secret=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 把开关拼成 `session` 对象。`--session-json` 给了就**原样**用它，
/// 不做任何字段增删——那正是这个开关存在的意义（试产品现在还发不出来的形状）。
/// 不给就走产品那条路：`SessionParams::to_session_object`。
fn build_session_object(args: &Args) -> Result<SessionDraft, String> {
    if let Some(path) = &args.session_json {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("{} 不是合法 JSON：{e}", path.display()))?;
        if !value.is_object() {
            return Err(format!(
                "{} 的顶层是{}，不是对象——`--session-json` 要的是 session 对象本身",
                path.display(),
                json_type_name(&value)
            ));
        }
        return Ok(SessionDraft {
            object: value,
            notes: vec![
                "`--session-json` 的内容原样当 session 对象发，一个字段都没加也没删\
                 （要试的就是产品现在还发不出来的形状）。对象内部的键序按 serde_json \
                 规范化重排过——服务端按名字取字段，不看顺序。"
                    .to_string(),
            ],
        });
    }

    let mut notes = Vec::new();
    let requested = args
        .target_lang
        .as_deref()
        .unwrap_or(catalog::DEFAULT_TARGET_LANGUAGE)
        .trim();
    let target = catalog::normalize_language(requested);
    if target != requested {
        notes.push(format!(
            "目标语言 {requested:?} 已归一成 {target:?}（`catalog::normalize_language`）"
        ));
    }
    let voice = if args.audio {
        // `--audio on` 撞上一个只支持文本输出的目标语言时，
        // `SessionParams::audio_enabled()` 会**静默**把音色丢掉（`output_modalities`
        // 变成 `["text"]`，`audio.output.voice` 直接没有）。探针要是也照静默跑，
        // 问题 1「音色收在 `audio.output.voice` 对不对」会拿到一条**假阴性**——
        // 看着是"服务端不收"，其实是探针自己没发。所以这里当场报错。
        if !catalog::supports_audio_output(target) {
            return Err(format!(
                "目标语言 {target:?} 只支持文本输出（`catalog::supports_audio_output` 为否），\
                 而 `--audio on` 要求的音色会被 `SessionParams::to_session_object` 静默丢掉——\
                 这轮探测会拿不到关于音色的任何有效结论。\
                 要语音就换一个支持语音输出的目标语言（`--target-lang`，目录里 `audio_output: true` \
                 的那批）；确定只要文字就用 `--audio off`（那样就不该再给 `--voice`）。"
            ));
        }
        Some(
            args.voice
                .clone()
                .unwrap_or_else(|| catalog::default_voice_for_language(target)),
        )
    } else {
        notes.push("`--audio off`：只要文字，不带音色。".to_string());
        None
    };
    if let Some(source) = &args.source_lang {
        notes.push(format!(
            "给了源语言 {source:?}，报文里会多一段 `input_audio_transcription.language`\
             （不给就没有，靠服务端自动识别）。"
        ));
    }
    let params = SessionParams {
        model_name: args.model.clone(),
        target_language: target.to_string(),
        voice,
        clone_frequency: None,
        source_language: args
            .source_lang
            .as_ref()
            .map(|lang| lang.trim().to_string()),
    };
    Ok(SessionDraft {
        object: params.to_session_object(),
        notes,
    })
}

/// `session.update` 的外层信封。`--session-json` 与开关拼出来的对象走同一段封装。
fn session_update_frame(session: &Value, event_id: &str) -> String {
    json!({
        "event_id": event_id,
        "type": SESSION_UPDATE_TYPE,
        "session": session,
    })
    .to_string()
}

/// 按 20 ms 一片 append 上传。片大小是探针自己的节奏选择（见 `BLOCK_MS`）。
fn send_audio(transport: &mut WsTransport, audio: &[f32], seq: &mut u64) -> Result<(), String> {
    for (i, block) in audio.chunks(BLOCK_SAMPLES).enumerate() {
        let pcm = protocol::float_to_pcm16(block);
        let frame = ClientEvent::AppendAudio(pcm).to_json(&next_event_id(seq));
        if let Err(e) = transport.send(&frame) {
            return Err(format!(
                "第 {} 片（约 {:.2} 秒处）发不出去：{e}",
                i + 1,
                (i * BLOCK_SAMPLES).min(audio.len()) as f32 / INPUT_SAMPLE_RATE as f32
            ));
        }
    }
    Ok(())
}

/// 事件 id：跟产品一样是「毫秒时间戳 + 递增号」（`protocol::event_id`）。
fn next_event_id(seq: &mut u64) -> String {
    *seq += 1;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    protocol::event_id(now_ms, *seq)
}

/// 读上传音频。WAV 要严格是 16 kHz / PCM16 / 单声道——采样率或位深不对，
/// 服务端多半把它当成噪声，而报错原话通常不告诉你真正的原因。
fn load_audio(args: &Args) -> Result<Option<Vec<f32>>, String> {
    let Some(source) = &args.send_audio else {
        return Ok(None);
    };
    match source {
        SendAudio::Silence(seconds) => Ok(Some(vec![
            0.0f32;
            (seconds * INPUT_SAMPLE_RATE as f32).round()
                as usize
        ])),
        SendAudio::Wav(path) => {
            let bytes =
                std::fs::read(path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
            Ok(Some(
                read_wav_mono_16k(&bytes).map_err(|e| format!("{}：{e}", path.display()))?,
            ))
        }
    }
}

/// 读 16 kHz / PCM16 / 单声道 WAV 的采样，返回 `[-1, 1]` 的 f32。
///
/// 格式对不上就报错，不做重采样也不做混音——探针的职责是"原样发出去让服务端说"，
/// 悄悄改音频会让结论没法归因。
fn read_wav_mono_16k(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("不是 RIFF/WAVE 文件".to_string());
    }
    // (音频格式, 声道数, 采样率, 位深)
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        let Some(end) = start.checked_add(size).filter(|end| *end <= bytes.len()) else {
            return Err("块长度超出文件末尾（文件被截断了？）".to_string());
        };
        let body = &bytes[start..end];
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err("fmt 块短于 16 字节".to_string());
                }
                let u16at = |i: usize| u16::from_le_bytes(body[i..i + 2].try_into().unwrap());
                format = Some((u16at(0), u16at(2), u16at(4) as u32, u16at(14)));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        // RIFF 的块按偶数字节对齐，奇数长度后面跟一个补位字节。
        pos = end + (size & 1);
    }
    let (audio_format, channels, rate, bits) = format.ok_or("没有 fmt 块")?;
    let data = data.ok_or("没有 data 块")?;
    if audio_format != 1 {
        return Err(format!("音频格式 {audio_format} 不是 PCM（1）"));
    }
    if channels != 1 {
        return Err(format!("{channels} 声道，只接受单声道"));
    }
    if rate != INPUT_SAMPLE_RATE {
        return Err(format!("采样率 {rate} Hz，只接受 {INPUT_SAMPLE_RATE} Hz"));
    }
    if bits != 16 {
        return Err(format!("位深 {bits}，只接受 16"));
    }
    Ok(protocol::pcm16_to_float(data))
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

fn resolve_api_key(args: &Args) -> Result<String, String> {
    if let Ok(key) = std::env::var("DASHSCOPE_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
    }
    if args.keyring {
        return load_key_from_keyring();
    }
    Err(
        "没有密钥：先 `DASHSCOPE_API_KEY=…`，或者给 `--keyring` 从 Secret Service 读\
         （那一条腿要用 `--features probe-keyring` 重编）。"
            .to_string(),
    )
}

#[cfg(feature = "probe-keyring")]
fn load_key_from_keyring() -> Result<String, String> {
    // 复用 vox-host 的后端选择层：service `com.vox.app`、user `api-key.aliyun`
    // 是那边 `SecretBackend::SecretService` 定的，探针不另抄一份键名。
    // （`build()` 给的是 `Arc<dyn SecretStore>`，方法在 trait object 上直接调，
    // 不需要把 trait 引进作用域。）
    let store = vox_host::secrets::SecretBackend::SecretService.build();
    match store.load_api_key() {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err("Secret Service 里没有这条密钥（还没配过）。".to_string()),
        Err(e) => Err(format!("读 Secret Service 失败：{e}")),
    }
}

#[cfg(not(feature = "probe-keyring"))]
fn load_key_from_keyring() -> Result<String, String> {
    Err(
        "这个二进制没编 Secret Service 支持（vox-net 的 `probe-keyring` 没开）。\
         要用 `--keyring` 就带 feature 重编：\
         `cargo run -p vox-net --features probe-keyring --example aliyun_probe -- …`。"
            .to_string(),
    )
}

// --- `--list-models`：阿里云官方的"查询模型列表" --------------------------------
//
// 这一档不连 realtime，走的是另一个服务（`https://…/api/v1/models`，
// `Authorization: Bearer <key>`，分页）。它回答的是维护表另一个问题：
// **实时翻译这一族到底有哪些模型、哪些还有效（`offline_time`）、能力位对不对**。
//
// 响应字段名**没有一手文档可核**（本轮不联网核实），所以 `parse_models_page`
// 写得刻意宽容：常见的几种拼法都认，认不出的字段打 `—`，整页结构对不上时把
// （去密钥后的）原文摊开，让人照着改解析器——而不是打一行空列然后让人以为
// "这个模型没有 published_time"。

/// 每页条数。官方接口的常见上限。
const LIST_PAGE_SIZE: u32 = 100;
/// 能力位过滤值。**要核的就是它对不对**，所以配了 `--all-capabilities` 对照。
const LIST_CAPABILITY: &str = "Realtime-Audio-Translate";
/// 翻页硬上限。`output.total` 是服务端给的，它说谎时这条是最后一道闸。
const LIST_MAX_PAGES: u32 = 200;
/// `--all-capabilities` 模式下决定"这一条算不算实时翻译"的名字碎片。
const REALTIME_NAME_NEEDLES: [&str; 2] = ["livetranslate", "realtime"];
/// 表头里那一格缺失时打的东西。跟真实值区分得开，才不会把"没有"读成"忘了查"。
const MISSING: &str = "—";

/// 拼一次列表查询要用的那几格。
#[derive(Debug, Clone)]
struct ListQuery {
    service_site: Option<String>,
    /// 不带 `capabilities` 过滤。
    all_capabilities: bool,
}

impl ListQuery {
    fn from_args(args: &Args) -> Result<Self, String> {
        let service_site = match &args.service_site {
            None => None,
            Some(raw) => {
                let value = raw.trim();
                if value.is_empty() {
                    return Err(
                        "`--service-site` 给了空值——不给这个开关就是不带 `service_site`。"
                            .to_string(),
                    );
                }
                Some(value.to_string())
            }
        };
        Ok(Self {
            service_site,
            all_capabilities: args.all_capabilities,
        })
    }
}

/// 第 `page_no` 页的完整 URL。
///
/// 参数顺序按官方文档写（`capabilities` → `page_size` → `page_no` → `service_site`），
/// 这样 `page_no` 的差别在两页之间一眼看得出来——翻页出错最常见的原因就是它没变。
fn list_models_url(base: &str, page_no: u32, query: &ListQuery) -> String {
    let mut url = format!("{base}/api/v1/models?");
    if !query.all_capabilities {
        url.push_str(&format!("capabilities={LIST_CAPABILITY}&"));
    }
    url.push_str(&format!("page_size={LIST_PAGE_SIZE}&page_no={page_no}"));
    if let Some(site) = &query.service_site {
        url.push_str(&format!("&service_site={}", encode_query_component(site)));
    }
    url
}

/// query 值按 RFC 3986 的 unreserved 集编码。**只为拼 URL 服务**，`base` 走
/// `sanitize_list_base` 那套结构检查，不在这里编码。
fn encode_query_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `--list-base` 的结构检查。与 `sanitize_endpoint` 分开写：那边收 `wss://`、允许
/// 路径，这边只收 `https://` 且不该带路径，但**凭据那几道检查一字不差地重复一遍**
/// ——理由相同（探针会把地址打给人看），复用了 `authority_userinfo` /
/// `carries_a_credential`，所以新增的用户信息测试两边都覆盖得到。
fn sanitize_list_base(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("https://") {
        return Err(format!(
            "`--list-base` 必须以 https:// 开头，收到 {trimmed:?}。\
             （模型列表是普通的 HTTPS GET，不是 WebSocket。）"
        ));
    }
    if carries_a_credential(trimmed) {
        return Err(
            "`--list-base` 的地址里带着 key=…／token=…。密钥只走 Authorization 头，地址里不许带。"
                .to_string(),
        );
    }
    if let Some(userinfo) = authority_userinfo(trimmed) {
        return Err(credential_in_url_error("--list-base", userinfo));
    }
    let stripped = trimmed.trim_end_matches('/');
    // `https://` 之后总得剩下主机名。
    if authority_userinfo(stripped).is_some() || stripped.len() <= "https://".len() {
        return Err(format!("`--list-base {trimmed:?}` 没有主机名"));
    }
    if stripped
        .split_once("://")
        .map(|(_, rest)| !rest.contains('/'))
        != Some(true)
    {
        return Err(format!(
            "`--list-base` 只收基址（到主机名为止），路径由探针自己接 `/api/v1/models`——收到 {trimmed:?}"
        ));
    }
    Ok(stripped.to_string())
}

/// 一页解析出来的东西。
#[derive(Debug, Default, PartialEq)]
struct ModelPage {
    models: Vec<ModelRow>,
    /// `output.total`。缺了就让 `next_page` 退回"没满页就是最后一页"的推断。
    total: Option<u64>,
}

/// 一行表格。字段一律存成字符串，缺的就是空串——空串在打印时才变成 `—`。
#[derive(Debug, Default, PartialEq)]
struct ModelRow {
    model: String,
    name: String,
    published_time: String,
    equivalent_snapshot: String,
    offline_time: String,
    capabilities: String,
    request_modalities: String,
    response_modalities: String,
}

impl ModelRow {
    /// 表格里那一行的最后一格：`request→response`。
    fn modality_cell(&self) -> String {
        let left = or_missing(&self.request_modalities);
        let right = or_missing(&self.response_modalities);
        format!("{left}→{right}")
    }

    fn looks_realtime(&self) -> bool {
        let lower = self.model.to_ascii_lowercase();
        REALTIME_NAME_NEEDLES
            .iter()
            .any(|needle| lower.contains(needle))
    }
}

fn or_missing(value: &str) -> &str {
    if value.is_empty() {
        MISSING
    } else {
        value
    }
}

impl ListQuery {
    /// 这一行要不要在 `--all-capabilities` 模式下打出来。
    fn keep(&self, row: &ModelRow) -> bool {
        !self.all_capabilities || row.looks_realtime()
    }
}

/// 解析一页。**认不出的字段打空，不认不出的结构报错**——空的那一格会打成 `—`，
/// 人一看就知道是"服务端没给这个字段"而不是"解析器坏了"；结构对不上才会报错，
/// 报的时候把原文带上去，照着改就行。
fn parse_models_page(body: &str) -> Result<ModelPage, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| format!("响应不是合法 JSON（{e}）。原文：{body}"))?;
    // 官方形状是 `output.models`；下面几个是"万一它换了名字"的兜底。
    let models = find_array(&value, &["output", "models"])
        .or_else(|| find_array(&value, &["output", "model_list"]))
        .or_else(|| find_array(&value, &["models"]))
        .or_else(|| find_array(&value, &["output", "data"]))
        .or_else(|| find_array(&value, &["data"]))
        .ok_or_else(|| {
            format!(
                "响应里找不到模型数组（试过 output.models / output.model_list / models / \
                 output.data / data）。字段名跟预期对不上，**解析器要照着这份原文改**：\n{body}"
            )
        })?;
    let rows = models
        .iter()
        .filter_map(|entry| {
            // 非对象的条目（null、字符串）跳掉而不是 panic：服务端改了形状不该让探测崩掉。
            let entry = entry.as_object()?;
            let model = first_text(entry, &["model", "model_id", "model_name"]);
            if model.is_empty() {
                return None;
            }
            Some(ModelRow {
                model,
                name: first_text(entry, &["name", "display_name", "label"]),
                published_time: first_text(entry, &["published_time", "release_time"]),
                equivalent_snapshot: first_text(entry, &["equivalent_snapshot", "snapshot"]),
                // 下线时间在 `inference_offline_info.offline_time`（2026-10-09 真连核实）。
                offline_time: nested_text(entry, &["inference_offline_info", "offline_time"])
                    .or_else(|| first_nonempty(entry, &["offline_time", "deprecate_time"]))
                    .unwrap_or_default(),
                capabilities: first_text(entry, &["capabilities", "capability"]),
                // 模态在 `inference_metadata` 下面，而且是**单数**（同上）。
                request_modalities: nested_text(entry, &["inference_metadata", "request_modality"])
                    .or_else(|| first_nonempty(entry, &["request_modalities", "request_modality"]))
                    .unwrap_or_default(),
                response_modalities: nested_text(
                    entry,
                    &["inference_metadata", "response_modality"],
                )
                .or_else(|| first_nonempty(entry, &["response_modalities", "response_modality"]))
                .unwrap_or_default(),
            })
        })
        .collect();
    Ok(ModelPage {
        models: rows,
        total: find_u64(&value, &["output", "total"]).or_else(|| find_u64(&value, &["total"])),
    })
}

/// 顺着路径找数组。路径里任何一段不是对象就当没有（不 panic）。
fn find_array<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    let mut cursor = value;
    for (i, key) in path.iter().enumerate() {
        let next = cursor.as_object()?.get(*key)?;
        // 中间段必须是对象才能继续往下走。
        if i + 1 < path.len() && !next.is_object() {
            return None;
        }
        cursor = next;
    }
    cursor.as_array()
}

fn find_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.as_object()?.get(*key)?;
    }
    cursor.as_u64()
}

/// 取第一个非空的字符串字段。`capabilities` 那几格可能是数组，也可能是逗号串，
/// 两种都要收下——所以先按字符串取，取不到再按数组拼。
fn first_text(entry: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    text_of(keys.iter().filter_map(|key| entry.get(*key)))
}

/// 兜底用的老拼法：`first_nonempty(...)` 是"依次试几个键，都取不到才给空"，
/// `nested_text(...)` 是"顺着路径找"，两者取第一个有值的。
fn first_nonempty(entry: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    let found = first_text(entry, keys);
    (!found.is_empty()).then_some(found)
}

/// 顺着 `a.b.c` 找一格文本（`inference_metadata.request_modality` 这类）。
fn nested_text(entry: &serde_json::Map<String, Value>, path: &[&str]) -> Option<String> {
    let (first, rest) = path.split_first()?;
    let value = entry.get(*first)?;
    let mut cursor = value;
    for key in rest {
        cursor = cursor.as_object()?.get(*key)?;
    }
    let text = text_of(std::iter::once(cursor));
    (!text.is_empty()).then_some(text)
}

fn text_of<'a>(fields: impl Iterator<Item = &'a Value>) -> String {
    for field in fields {
        match field {
            Value::String(s) if !s.is_empty() => return s.clone(),
            Value::Array(items) => {
                let joined = items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(",");
                if !joined.is_empty() {
                    return joined;
                }
            }
            // 时间字段给 `null` 表示"没有下架时间"，那本来就该是空的。
            Value::Null => {}
            _ => {}
        }
    }
    String::new()
}

/// 翻页到哪儿为止。总页数**事前不知道**，只能边读边判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Page(u32),
    Stop,
}

/// 一页读完之后的翻页决定。
///
/// 三道闸，任何一道都独立成立：
/// 1. 这一页一条都没回来 → 再翻也是空的（也顺带挡住 `total` 说谎）；
/// 2. 页数到了硬上限；
/// 3. `total` 说了就照它算，没说就退回"没满页即末页"的常规推断。
fn next_page(page_no: u32, page_size: u32, got: usize, seen: usize, total: Option<u64>) -> Next {
    if got == 0 || page_no >= LIST_MAX_PAGES {
        return Next::Stop;
    }
    match total {
        Some(total) if (seen as u64) >= total => Next::Stop,
        Some(_) => Next::Page(page_no + 1),
        None if got < page_size as usize => Next::Stop,
        None => Next::Page(page_no + 1),
    }
}

/// 把密钥从一段文本里去掉。**打任何服务端回的东西之前都要过一遍。**
///
/// 两道：密钥原文（服务端把 `Authorization` 原样吐回来是已知的写法），以及
/// `Bearer <token>` 形式（万一它回的是截断/变形的键）。
fn redact(text: &str, api_key: &str) -> String {
    let mut out = if api_key.is_empty() {
        text.to_string()
    } else {
        text.replace(api_key, "〈密钥已去掉〉")
    };
    // `Bearer` 后面跟一段非空白，整体换成占位。
    let mut rebuilt = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(at) = rest.find("Bearer ") {
        let (head, tail) = rest.split_at(at);
        rebuilt.push_str(head);
        rebuilt.push_str("Bearer 〈已去掉〉");
        let token_len = tail[7..]
            .split_whitespace()
            .next()
            .map_or(0, |token| token.len());
        // 切的是**字节**下标；`split_whitespace` 的首段一定是合法 UTF-8 边界，
        // 切完剩下的是它的后缀，也从边界开始，所以不会 panic。
        rest = &tail[7 + token_len..];
    }
    rebuilt.push_str(rest);
    out.clear();
    rebuilt
}

/// 列表模式的一行输出。
fn model_line(row: &ModelRow) -> String {
    format!(
        "{} | {} | {} | {} | {} | {} | {}",
        or_missing(&row.model),
        or_missing(&row.name),
        or_missing(&row.published_time),
        or_missing(&row.equivalent_snapshot),
        or_missing(&row.offline_time),
        or_missing(&row.capabilities),
        row.modality_cell()
    )
}

fn run_list_models(args: &Args) -> ExitCode {
    if let Some(flag) = args.list_models_stray_flags() {
        eprintln!(
            "`--list-models` 是查模型列表，不连实时翻译，{flag} 在这一档没有意义。\
             这一档只认：--list-models / --region / --list-base / --service-site / \
             --all-capabilities / --keyring / --dry-run / --help。"
        );
        return ExitCode::from(exit::USAGE);
    }
    let base = match args.list_base.as_deref() {
        Some(raw) => match sanitize_list_base(raw) {
            Ok(base) => base,
            Err(message) => {
                eprintln!("列表地址有问题：{message}");
                return ExitCode::from(exit::USAGE);
            }
        },
        None => args.region.list_base().to_string(),
    };
    let query = match ListQuery::from_args(args) {
        Ok(query) => query,
        Err(message) => {
            eprintln!("参数有问题：{message}");
            return ExitCode::from(exit::USAGE);
        }
    };
    let page_one = list_models_url(&base, 1, &query);

    // 同样的规矩：凭据检查在第一行 `println!` 之前（`--list-base` 是用户贴进来的）。
    let api_key = if args.dry_run {
        None
    } else {
        match resolve_api_key(args) {
            Ok(key) => Some(key),
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(exit::USAGE);
            }
        }
    };
    if let Some(key) = &api_key {
        if page_one.contains(key.as_str()) {
            eprintln!("列表地址里带上了密钥，已中止——密钥不许出现在地址里。");
            return ExitCode::from(exit::USAGE);
        }
    }

    println!("模型列表地址（第 1 页）：{page_one}");
    if query.all_capabilities {
        println!(
            "注意：`--all-capabilities` **不带** capabilities={LIST_CAPABILITY} 过滤，\
             只打印 model 名含 {} 的条目——用来核对这个过滤值有没有漏掉模型。",
            REALTIME_NAME_NEEDLES.join(" / ")
        );
    }
    if args.dry_run {
        println!("--dry-run：不连、不取密钥。第 2 页起只有 `page_no` 变，其余原样。");
        return ExitCode::from(exit::OK);
    }
    let api_key = api_key.expect("不是 --dry-run 就一定解析出了密钥");

    // rustls 0.23 要显式选一个 provider，否则首次 TLS 握手 panic。
    // realtime 那条腿由 `WsTransport::standalone()` 顺手装上；列表这条腿不经过它，
    // 所以自己调同一个函数（幂等）。
    let _ = rustls::crypto::ring::default_provider().install_default();

    println!("表头：model | name | published_time | equivalent_snapshot | offline_time | capabilities | request→response modality");
    match fetch_all_models(&base, &query, &api_key) {
        Ok((rows, printed)) => {
            for row in &rows {
                if !query.keep(row) {
                    continue;
                }
                println!("{}", model_line(row));
            }
            println!(
                "共 {printed} 个模型（取回 {rows_len} 条{extra}）。",
                rows_len = rows.len(),
                extra = if query.all_capabilities {
                    "，其余被 `--all-capabilities` 的名字过滤挡掉"
                } else {
                    ""
                }
            );
            ExitCode::from(exit::OK)
        }
        Err(message) => {
            eprintln!("模型列表没取到：{message}");
            ExitCode::from(exit::NO_CONNECTION)
        }
    }
}

/// 翻页取完，打印了几条。
type ListedModels = (Vec<ModelRow>, usize);

fn fetch_all_models(base: &str, query: &ListQuery, api_key: &str) -> Result<ListedModels, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("起不了网络运行时：{e}"))?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("起不了 HTTPS 客户端：{e}"))?;
        let mut rows: Vec<ModelRow> = Vec::new();
        let mut seen = 0usize;
        let mut total: Option<u64> = None;
        let mut page_no = 1u32;
        loop {
            let url = list_models_url(base, page_no, query);
            let body = get_page(&client, &url, api_key).await?;
            let page = parse_models_page(&body).map_err(|e| redact(&e, api_key))?;
            if page.total.is_some() {
                total = page.total;
            }
            let got = page.models.len();
            rows.extend(page.models);
            seen += got;
            match next_page(page_no, LIST_PAGE_SIZE, got, seen, total) {
                Next::Stop => break,
                Next::Page(next) => page_no = next,
            }
        }
        let printed = rows.iter().filter(|row| query.keep(row)).count();
        Ok((rows, printed))
    })
}

/// 发一次 GET。**非 200 就把状态码与响应体带回去**，但体先过一遍 `redact`——
/// 服务端把 `Authorization` 原样吐回来是唯一的一条泄漏路径。
async fn get_page(client: &reqwest::Client, url: &str, api_key: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header(
            reqwest::header::AUTHORIZATION,
            protocol::auth_header_value(api_key),
        )
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败：{e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读 {url} 的响应体失败：{e}"))?;
    if !status.is_success() {
        return Err(format!(
            "HTTP {}：{}",
            status.as_u16(),
            redact(&body, api_key)
        ));
    }
    Ok(body)
}

fn usage() {
    println!(
        "百炼实时翻译现场探针（维护表重构第 0 步）\n\
         \n\
         用法：\n  \
         cargo run -p vox-net --example aliyun_probe -- [开关]\n\
         \n\
         开关：\n  \
         --model <名>          进地址的模型名，原样照发（缺省：catalog 当前缺省模型）\n  \
         --region cn|intl|hk   地区缺省地址（缺省 cn）。**hk 只对 --list-models 有意义**，\
         连 realtime 时给 hk 当场报错（产品只有国内站 / 国际站两档）\n  \
         --endpoint <wss://…>  覆盖地区地址；带 key=／token=／`用户@主机` 一律拒绝\n  \
         --session-json <文件> 把文件内容**原样**当 session 对象发（字段一个不多一个不少，\
         键序按 serde_json 规范化；不能与 --target-lang / --source-lang / --voice 同用）\n  \
         --target-lang <码>     目标语言（缺省 catalog 缺省）\n  \
         --source-lang <码>     源语言；不给 = 服务端自动识别，报文里不发 \
         `input_audio_transcription`\n  \
         --voice <音色>         `--audio on` 时用；不给取该语言的缺省音色\n  \
         --audio on|off        要不要语音输出（缺省 on）。目标语言只支持文本输出时，\
         `--audio on` **直接报错退出**——不许静默丢掉音色跑出一轮假结论\n  \
         --send-audio <WAV|silence:秒>  连上后按 {BLOCK_MS} ms 一片 append；不给就不发\n  \
         --listen <秒>         音频发完后听多久（缺省 8）\n  \
         --keyring             没有 DASHSCOPE_API_KEY 时从 Secret Service 读（需 probe-keyring feature）\n  \
         --dry-run             只打印地址与 session.update，不连、不取密钥\n  \
         --raw                 自己做 TLS，摊开服务端发来的未解帧字节（诊断 Control frame too big 这类解帧层先炸的问题）\n  \
         --help                这一屏\n\
         \n\
         查阿里云官方的模型列表（不连实时翻译；这一档只认下面这几个开关，\
         给了 realtime 的开关当场报错）：\n  \
         --list-models         GET <基址>/api/v1/models，带 capabilities={LIST_CAPABILITY}，\
         按 page_no 翻到 output.total 取完；每个模型一行，最后报总数\n  \
         --region hk           列表用的香港站（https://cn-hongkong.dashscope.aliyuncs.com）\n  \
         --list-base <https://…>  覆盖列表基址（只收主机名，路径探针自己接 /api/v1/models）\n  \
         --service-site <值>   原样透传成 query 的 service_site\n  \
         --all-capabilities    不带 capabilities 过滤，只打印 model 名含 \
         livetranslate / realtime 的条目——用来核对这个过滤值有没有漏掉模型\n  \
         \n\
         退出码：0 连上且收到 session.updated（或列表取完）/ 2 参数或密钥问题（没发请求）/ \
         3 服务端报错 / 4 连不上（或列表取不到）/ 5 超时没等到 session.updated\n\
         \n\
         三个问题怎么跑见 `cargo run -p vox-net --example aliyun_probe -- --help` 之后的文档注释\
         （本文件头部）。"
    );
}

// --- 单测：不联网就能钉住的那几件事 ---------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 产品维护表里的两条阿里云模型。探针的"产品那条路"报文按它们分方言，
    /// 测试里也得用真模型名（编一个假名会被当成"认不出方言"走最小报文）。
    const LIVE38: &str = "qwen3.8-livetranslate-flash-realtime";

    /// 造一段 WAV 头 + PCM16，给读文件的路径当输入，也给"格式对不上要报错"当反例。
    fn wav(rate: u32, channels: u16, bits: u16, samples: &[f32]) -> Vec<u8> {
        let pcm = protocol::float_to_pcm16(samples);
        let byte_rate = rate * channels as u32 * (bits as u32 / 8);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + pcm.len()) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&pcm);
        bytes
    }

    /// `wav()` 里 data 块长度字段的偏移（RIFF 头 12 + `fmt ` 头 8 + fmt 体 16 + `data` 头 8）。
    const WAV_DATA_SIZE_AT: usize = 40;

    fn args_with(endpoint: Option<&str>, region: ProbeRegion, model: &str) -> Args {
        Args {
            model: model.to_string(),
            region,
            endpoint: endpoint.map(str::to_string),
            session_json: None,
            target_lang: None,
            source_lang: None,
            voice: None,
            audio: true,
            send_audio: None,
            listen: Duration::from_secs(8),
            keyring: false,
            dry_run: false,
            raw: false,
            help: false,
            list_models: false,
            list_base: None,
            service_site: None,
            all_capabilities: false,
            given: Vec::new(),
        }
    }

    /// 地址要去掉用户贴进来的 `?model=…`、再去掉尾部斜杠，再按模型重拼 query——
    /// 少做一步就会拼出两个 `?`，握手 400，整轮结论全废。
    #[test]
    fn endpoint_override_is_stripped_before_the_model_is_attached() {
        let args = args_with(
            Some("  wss://ws-abc.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime/?model=stale  "),
            ProbeRegion::Intl,
            "qwen3.8-livetranslate-flash-realtime",
        );
        assert_eq!(
            build_endpoint(&args).unwrap(),
            "wss://ws-abc.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime\
             ?model=qwen3.8-livetranslate-flash-realtime"
        );
    }

    /// 地区缺省地址真的换主机：国际站的 Key 连北京会被拒，只换名字没用。
    #[test]
    fn region_switches_the_host_and_the_override_beats_it() {
        assert_eq!(
            build_endpoint(&args_with(None, ProbeRegion::Cn, "m")).unwrap(),
            format!("{}?model=m", catalog::API_BASE)
        );
        assert_eq!(
            build_endpoint(&args_with(None, ProbeRegion::Intl, "m")).unwrap(),
            format!("{}?model=m", catalog::API_BASE_INTL)
        );
        assert_ne!(catalog::API_BASE, catalog::API_BASE_INTL);

        // 同时给两个时 `--endpoint` 赢，并且不再看地区。
        assert_eq!(
            build_endpoint(&args_with(
                Some("wss://ws-x.example/realtime"),
                ProbeRegion::Intl,
                "m"
            ))
            .unwrap(),
            "wss://ws-x.example/realtime?model=m"
        );
    }

    /// 模型名原样照发，**不**过归一：探针的全部意义就是能问产品还不会用的模型。
    /// 这条要是哪天被"顺手改成走产品逻辑"，下线模型的结论会全部变成推荐模型的结论。
    #[test]
    fn model_name_is_not_normalized_away() {
        let unknown = "gummy-realtime-v1";
        assert_ne!(
            catalog::normalize_model_for(ModelProvider::Aliyun, unknown),
            unknown,
            "前提：gummy 不在维护表里（不然这条没测到东西）"
        );
        assert!(build_endpoint(&args_with(None, ProbeRegion::Cn, unknown))
            .unwrap()
            .ends_with(&format!("?model={unknown}")));
        // 进了表的模型（含带日期后缀的快照）产品会原样保留，探针与产品一致。
        let snapshot = "qwen3.5-livetranslate-flash-realtime-2026-05-19";
        assert_eq!(
            catalog::normalize_model_for(ModelProvider::Aliyun, snapshot),
            snapshot
        );
        assert!(build_endpoint(&args_with(None, ProbeRegion::Cn, snapshot))
            .unwrap()
            .ends_with(&format!("?model={snapshot}")));
    }

    /// 地址里带凭据一律拒绝。探针把地址原样打给人看，贴错一个带 key 的地址
    /// 就等于把密钥留在终端回滚缓冲里。
    #[test]
    fn endpoint_carrying_a_credential_is_refused() {
        for sneaky in [
            "wss://x.example/realtime?key=sk-abc",
            "wss://x.example/realtime?model=a&accessKeyId=zz",
            "wss://x.example/realtime?TOKEN=sk-abc",
        ] {
            assert!(
                sanitize_endpoint(sneaky).is_err(),
                "{sneaky:?} 带凭据却放行了"
            );
        }
        // 正常的工作空间地址（主机名里带 WorkspaceId）不该被误伤。
        assert!(sanitize_endpoint(
            "wss://ws-abc123.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime"
        )
        .is_ok());
    }

    /// 地址不合法时**报错**，不退回地区缺省。产品会退回，但那会让"我贴的是工作空间
    /// 专属地址"这句话变成一句没生效的话，探针答出来的就是另一件事。
    #[test]
    fn a_junk_endpoint_fails_loudly_instead_of_falling_back() {
        for junk in [
            "https://x.example/realtime",
            "x.example/realtime",
            "wss://",
            "   ",
        ] {
            assert!(
                sanitize_endpoint(junk).is_err(),
                "{junk:?} 应当报错而不是退回缺省"
            );
        }
    }

    /// `--session-json` 原样当 session 对象：多一个字段少一个字段都会让"这个形状
    /// 服务端收不收"的结论偏掉，所以这里不许有任何加工。
    #[test]
    fn the_session_object_is_wrapped_verbatim() {
        let raw = json!({
            "modalities": ["text", "audio"],
            "voice": "Tina",
            "weird_extra_field": {"nested": [1, 2, 3]},
        });
        let frame: Value = serde_json::from_str(&session_update_frame(&raw, "event_0_1")).unwrap();
        assert_eq!(frame["type"], "session.update");
        assert_eq!(frame["event_id"], "event_0_1");
        assert_eq!(frame["session"], raw, "session 对象必须一字不改");
    }

    /// 事件名不许跟协议枚举悄悄跑偏（`ClientEvent::type_name` 是那份枚举自己的答案）。
    #[test]
    fn session_update_name_matches_the_protocol_enum() {
        assert_eq!(
            SESSION_UPDATE_TYPE,
            ClientEvent::SessionUpdate(Box::new(SessionParams::text_only("", ""))).type_name()
        );
        assert_eq!(
            ClientEvent::AppendAudio(Vec::new()).type_name(),
            "input_audio_buffer.append"
        );
    }

    /// 开关拼出来的那条路必须跟产品一致（`SessionParams::to_session_object`）：
    /// 音色落在 `audio.output.voice`，源语言落在 `input_audio_transcription.language`。
    /// 这两条正是待核实的猜测，探针自己的默认形状不能先跑偏。
    #[test]
    fn flags_build_the_same_session_shape_as_the_product() {
        let mut args = args_with(None, ProbeRegion::Cn, catalog::DEFAULT_MODEL_NAME);
        args.target_lang = Some("ja".into());
        args.voice = Some("Tina".into());
        args.source_lang = Some("en".into());
        let draft = build_session_object(&args).unwrap();
        assert_eq!(draft.object["audio"]["output"]["voice"], "Tina");
        assert_eq!(draft.object["input_audio_transcription"]["language"], "en");
        assert_eq!(draft.object["translation"]["language"], "ja");
        assert_eq!(draft.object["output_modalities"], json!(["text", "audio"]));

        // `--audio off` 落到纯文字：不要音色，也别报 audio 模态。
        args.audio = false;
        args.voice = None;
        let text_only = build_session_object(&args).unwrap().object;
        assert_eq!(text_only["output_modalities"], json!(["text"]));
        assert!(text_only["audio"]["output"].get("voice").is_none());
    }

    /// `--session-json` 与语言/音色开关互斥。这两样同时给的时候到底听谁的说不清，
    /// 默默忽略一半会让人以为那一半生效了。
    #[test]
    fn session_json_collides_with_the_shape_flags() {
        let mut args = args_with(None, ProbeRegion::Cn, "m");
        args.session_json = Some(PathBuf::from("/tmp/x.json"));
        assert_eq!(args.shape_flags(), None);
        for set in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            args.target_lang = set.0.then(|| "ja".to_string());
            args.source_lang = set.1.then(|| "en".to_string());
            args.voice = set.2.then(|| "Tina".to_string());
            assert!(args.shape_flags().is_some(), "给了开关却没报冲突：{set:?}");
        }
    }

    /// 音频 delta 的 base64 折成长度提示——刷屏会把要看的译文挤走，而且几十 KB 的
    /// base64 在终端里没法读。
    #[test]
    fn audio_delta_is_folded_but_its_length_is_reported() {
        let payload = "A".repeat(4096);
        let raw = format!(r#"{{"type":"response.audio.delta","delta":"{payload}"}}"#);
        let folded = fold_audio_payload(&raw);
        assert!(folded.contains("原长 4096 字符"), "{folded}");
        assert!(!folded.contains(&"A".repeat(64)), "base64 正文不该还在");

        // 文字事件一个字都不改——那是这轮真正要看的东西。
        let text = r#"{"type":"response.text.delta","delta":"你好"}"#;
        assert_eq!(fold_audio_payload(text), text);
    }

    /// 服务端发来的东西再离谱也不能 panic：网络上的垃圾不该让探针崩掉，
    /// 崩了就等于这次探测白跑。
    #[test]
    fn hostile_payloads_do_not_panic() {
        for junk in ["", "不是 JSON", "[1,2,3]", "null", "\"字符串\""] {
            let _ = fold_audio_payload(junk);
        }
        // `delta` 不是字符串（对象 / null / 缺字段）时也要安全过去。
        for odd in [
            r#"{"type":"response.audio.delta","delta":{"a":1}}"#,
            r#"{"type":"response.audio.delta","delta":null}"#,
            r#"{"type":"response.audio.delta"}"#,
        ] {
            let _ = fold_audio_payload(odd);
        }
        // 多字节字符的载荷：按字节切会 panic，这里按字符切。
        assert!(base64_hint("中中中中").contains('中'));
    }

    /// WAV 读回来要跟写进去的一样。读错了上传上去就是噪声，而服务端的反应通常
    /// 不告诉你"你的音频格式不对"。
    #[test]
    fn wav_round_trips_through_the_probe_reader() {
        let original: Vec<f32> = (0..1600).map(|i| (i as f32 / 1600.0) * 2.0 - 1.0).collect();
        let read = read_wav_mono_16k(&wav(16_000, 1, 16, &original)).unwrap();
        assert_eq!(read.len(), original.len());
        for (a, b) in original.iter().zip(read.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    /// 格式对不上必须报错，不能"读出来再说"——探针悄悄重采样会把结论带偏。
    #[test]
    fn wrong_wav_formats_are_refused_with_the_reason() {
        for (bytes, needle) in [
            (wav(48_000, 1, 16, &[0.0]), "48000"),
            (wav(16_000, 2, 16, &[0.0]), "2 声道"),
            (wav(16_000, 1, 24, &[0.0]), "位深"),
            (b"NOTAWAVEFILE____".to_vec(), "RIFF"),
        ] {
            let err = read_wav_mono_16k(&bytes).unwrap_err();
            assert!(err.contains(needle), "错误里要说得清哪里不对，实际：{err}");
        }
        // data 块长度说谎（截断的文件）不能 panic，也不能当成正常音频。
        let mut truncated = wav(16_000, 1, 16, &[0.0; 100]);
        truncated[WAV_DATA_SIZE_AT..WAV_DATA_SIZE_AT + 4].copy_from_slice(&9999u32.to_le_bytes());
        assert!(read_wav_mono_16k(&truncated).is_err());
    }

    /// `silence:<秒>` 出来的样本数要对得上秒数——发 0.5 秒却变成 5 秒，
    /// 监听窗口会短得什么都听不见。
    #[test]
    fn silence_produces_the_requested_number_of_samples() {
        let mut args = args_with(None, ProbeRegion::Cn, "m");
        args.send_audio = Some(SendAudio::Silence(0.5));
        let audio = load_audio(&args).unwrap().unwrap();
        assert_eq!(audio.len(), INPUT_SAMPLE_RATE as usize / 2);
        assert!(audio.iter().all(|s| *s == 0.0));
    }

    /// 参数解析的边界：这些拼错的方式都会让人以为探针跑过了，其实一帧都没发。
    #[test]
    fn send_audio_source_parses_both_shapes() {
        assert!(matches!(
            parse_send_audio("silence:2.5"),
            Ok(SendAudio::Silence(s)) if (s - 2.5).abs() < f32::EPSILON
        ));
        assert!(matches!(
            parse_send_audio("/tmp/a.wav"),
            Ok(SendAudio::Wav(p)) if p.to_str() == Some("/tmp/a.wav")
        ));
        // 非法的一律报错，别默默当成"不发音频"。
        for junk in ["silence:", "silence:abc", "silence:-1", ""] {
            assert!(parse_send_audio(junk).is_err(), "{junk:?} 应当报错");
        }
    }

    // --- 缺陷 1：`--audio on` 撞上不支持语音输出的语言不许静默降级 ------------

    /// `SessionParams::audio_enabled()` 在目标语言不支持语音输出时会**静默**把音色丢掉
    /// （`output_modalities` 变 `["text"]`，`audio.output.voice` 直接没有）。
    /// 探针要是也跟着静默跑，问题 1「音色收在 `audio.output.voice` 对不对」会拿到一条
    /// **假阴性**：看着像"服务端不收"，其实是探针压根没发。所以必须当场报错。
    #[test]
    fn audio_on_refuses_a_target_language_that_cannot_speak() {
        let mut args = args_with(None, ProbeRegion::Cn, LIVE38);
        args.target_lang = Some("yue".into());
        args.voice = Some("Tina".into());
        let err = build_session_object(&args).unwrap_err();
        assert!(
            err.contains("只支持文本输出"),
            "错误要说清是语言不支持语音，实际：{err}"
        );

        // 前提钉死：产品那边确实是静默丢的（不是探针自己瞎报）。
        // 这条要是哪天产品改成报错，`supports_audio_output` 的语义就变了，本条的立论要重写。
        let params = SessionParams {
            model_name: LIVE38.into(),
            target_language: "yue".into(),
            voice: Some("Tina".into()),
            clone_frequency: None,
            source_language: None,
        };
        assert!(!params.audio_enabled(), "前提：yue 不支持语音输出");
        let silent = params.to_session_object();
        assert_eq!(silent["output_modalities"], json!(["text"]));
        assert!(silent["audio"]["output"].get("voice").is_none());

        // 同一个语言配 `--audio off` 就合法——要文字本来就不该带音色。
        args.audio = false;
        args.voice = None;
        assert_eq!(
            build_session_object(&args).unwrap().object["output_modalities"],
            json!(["text"])
        );

        // 支持语音输出的语言不受影响（`ja` 是 catalog 里 audio_output: true 的）。
        args.audio = true;
        args.target_lang = Some("ja".into());
        let ok = build_session_object(&args).unwrap().object;
        assert_eq!(ok["output_modalities"], json!(["text", "audio"]));
    }

    // --- 缺陷 2：userinfo 形式的凭据必须在任何打印之前被拒 --------------------

    /// `wss://sk-xxx@host/...` 这种把密钥塞进 userinfo 的写法，`carries_a_credential`
    /// 按键名认不出来，而探针第一件事就是把地址 `println!` 给人看——那等于把密钥写进
    /// 终端回滚缓冲。这里走 `build_endpoint`（**在第一行打印之前**）而不是直接调
    /// `sanitize_endpoint`：要钉的是"检查发生在打印之前"这件事本身。
    #[test]
    fn endpoint_with_userinfo_is_refused_before_anything_is_printed() {
        let mut args = args_with(None, ProbeRegion::Cn, "m");
        for sneaky in [
            "wss://sk-realkey999@x.example/realtime",
            "wss://user:sk-realkey999@x.example/realtime",
            "wss://user:pa@ss@x.example/realtime",
            "  wss://sk-realkey999@x.example/realtime  ",
        ] {
            args.endpoint = Some(sneaky.to_string());
            let err = build_endpoint(&args).unwrap_err();
            assert!(
                err.contains("userinfo"),
                "{sneaky:?} 带 userinfo 却放行了（错误：{err}）"
            );
            // 错误消息本身也不能把 userinfo 原样抄出来——那正是密钥。
            assert!(
                !err.contains("sk-realkey999"),
                "错误消息里不该出现密钥原文：{err}"
            );
        }

        // 路径里带 `@` 是合法的（`/api/@v1` 之类），全串找 `@` 会误伤。
        assert!(sanitize_endpoint("wss://x.example/api/@v1/realtime").is_ok());
        // 列表基址走的是同一道检查。
        assert!(sanitize_list_base("https://sk-realkey999@dashscope.aliyuncs.com").is_err());
    }

    // --- `--list-models` ---------------------------------------------------------

    /// 三个地区各自的列表基址，外加 `--list-base` 覆盖。**hk 只在这一档存在**，
    /// 连 realtime 时给 hk 必须当场报错——产品只有国内站 / 国际站两档。
    #[test]
    fn list_base_follows_the_region_and_hk_is_list_only() {
        assert_eq!(
            list_models_url(ProbeRegion::Cn.list_base(), 1, &plain_query()),
            "https://dashscope.aliyuncs.com/api/v1/models\
             ?capabilities=Realtime-Audio-Translate&page_size=100&page_no=1"
        );
        assert!(
            list_models_url(ProbeRegion::Intl.list_base(), 1, &plain_query())
                .starts_with("https://dashscope-intl.aliyuncs.com/api/v1/models?")
        );
        assert!(
            list_models_url(ProbeRegion::Hk.list_base(), 1, &plain_query())
                .starts_with("https://cn-hongkong.dashscope.aliyuncs.com/api/v1/models?")
        );

        // hk 没有实时翻译这条路：报错，不悄悄退回国内站。
        assert!(build_endpoint(&args_with(None, ProbeRegion::Hk, "m")).is_err());
    }

    /// URL 拼装：能力位过滤、`page_no`、`--all-capabilities`、`--service-site` 透传。
    /// 少一个 `&` 或者把过滤值拼漏了，这一档答出来的就是另一个问题。
    #[test]
    fn list_models_url_is_assembled_per_page_and_option() {
        // 第 2 页：只有 page_no 变。
        assert!(list_models_url("https://d.example", 2, &plain_query())
            .ends_with("&page_size=100&page_no=2"));

        // `--all-capabilities`：不带 capabilities 过滤（用来核对过滤值对不对）。
        let all = ListQuery {
            service_site: None,
            all_capabilities: true,
        };
        let url = list_models_url("https://d.example", 1, &all);
        assert!(!url.contains("capabilities"), "{url}");
        assert!(url.contains("page_no=1"), "{url}");

        // `--service-site` 透传，且要按 query 规则编码（值里带 & 会把 URL 拆散）。
        let site = ListQuery {
            service_site: Some("中国 & 日本".to_string()),
            all_capabilities: false,
        };
        let url = list_models_url("https://d.example", 3, &site);
        assert!(
            url.ends_with("&service_site=%E4%B8%AD%E5%9B%BD%20%26%20%E6%97%A5%E6%9C%AC"),
            "{url}"
        );
        // 普通的标识符原样通过，不被多此一举地编码。
        assert_eq!(
            encode_query_component("Realtime-Audio-Translate"),
            "Realtime-Audio-Translate"
        );
    }

    /// 翻页终止。`total` 说了就照它算，没说就退回"没满页即末页"，两条都要能停。
    /// 停不下来就是拿一个域名轮着翻页——那不是探测，那是往人家服务上砸请求。
    #[test]
    fn list_models_paging_stops_on_total() {
        // total=150：取两页（100 + 50）就停。
        assert_eq!(
            next_page(1, LIST_PAGE_SIZE, 100, 100, Some(150)),
            Next::Page(2)
        );
        assert_eq!(next_page(2, LIST_PAGE_SIZE, 50, 150, Some(150)), Next::Stop);

        // total=100：一页就够，不多翻一页去换一个空数组。
        assert_eq!(
            next_page(1, LIST_PAGE_SIZE, 100, 100, Some(100)),
            Next::Stop
        );

        // total=0：一页都不该翻。
        assert_eq!(next_page(1, LIST_PAGE_SIZE, 0, 0, Some(0)), Next::Stop);

        // 没有 total：满页就继续，不满页就停。
        assert_eq!(next_page(1, LIST_PAGE_SIZE, 100, 100, None), Next::Page(2));
        assert_eq!(next_page(2, LIST_PAGE_SIZE, 30, 130, None), Next::Stop);

        // 服务端说谎（total 远大于实际）时，空页那道闸独立成立。
        assert_eq!(next_page(2, LIST_PAGE_SIZE, 0, 100, Some(9999)), Next::Stop);

        // 硬上限独立成立。
        assert_eq!(
            next_page(
                LIST_MAX_PAGES,
                LIST_PAGE_SIZE,
                100,
                100 * LIST_MAX_PAGES as usize,
                Some(u64::MAX)
            ),
            Next::Stop
        );
    }

    /// 一段**真连拿到的**响应（2026-10-09 国内站，字段名照抄官方返回）。
    /// 这条既是"解析没写坏"的钉子，也是"列名对不对"的对照表——
    /// `inference_metadata.request_modality`（单数）与
    /// `inference_offline_info.offline_time` 都是 2026-10-09 核出来的，猜错过一次。
    #[test]
    fn a_hand_written_page_of_models_parses_into_table_rows() {
        let body = r#"{
          "request_id": "8A1B0000-0000-0000-0000-000000000000",
          "output": {
            "total": 2,
            "page_no": 1,
            "page_size": 100,
            "models": [
              {
                "model": "qwen3.8-livetranslate-flash-realtime",
                "name": "Qwen3.8 实时语音翻译",
                "provider": "qwen",
                "capabilities": ["Realtime-Audio-Translate"],
                "inference_metadata": {
                  "request_modality": ["audio"],
                  "response_modality": ["audio", "text"]
                },
                "published_time": "2026-09-11 18:02:53",
                "equivalent_snapshot": null,
                "inference_offline_info": null,
                "model_info": { "context_window": 32768 }
              },
              {
                "model": "qwen3-livetranslate-flash-realtime",
                "name": "Qwen3 实时语音翻译",
                "capabilities": "Realtime-Audio-Translate",
                "inference_metadata": {
                  "request_modality": ["audio"],
                  "response_modality": ["audio", "text"]
                },
                "published_time": "2025-09-01 00:00:00",
                "equivalent_snapshot": null,
                "inference_offline_info": { "offline_time": "2026-10-10 00:00:00" }
              }
            ]
          }
        }"#;
        let page = parse_models_page(body).unwrap();
        assert_eq!(page.total, Some(2));
        assert_eq!(page.models.len(), 2);

        let first = &page.models[0];
        assert_eq!(first.model, "qwen3.8-livetranslate-flash-realtime");
        assert_eq!(first.capabilities, "Realtime-Audio-Translate");
        assert_eq!(first.modality_cell(), "audio→audio,text");
        assert_eq!(
            first.offline_time, "",
            "`inference_offline_info: null` = 没有下线时间"
        );

        // 整行的列一个不多一个不少，缺的打成 `—` 而不是空串——空串会被读成"忘了查"。
        let line = model_line(first);
        assert_eq!(
            line,
            "qwen3.8-livetranslate-flash-realtime | Qwen3.8 实时语音翻译 | 2026-09-11 18:02:53 \
             | — | — | Realtime-Audio-Translate | audio→audio,text"
        );

        // 下线时间在 `inference_offline_info.offline_time`（不是顶层 `offline_time`）。
        let second = &page.models[1];
        assert_eq!(second.offline_time, "2026-10-10 00:00:00");
        assert_eq!(second.equivalent_snapshot, "", "null 按没有算");
        assert!(model_line(second)
            .contains(" | — | 2026-10-10 00:00:00 | Realtime-Audio-Translate | audio→audio,text"));

        // 结构对不上时报错，并且把原文带上去让人照着改解析器。
        let broken = parse_models_page(r#"{"output":{"unexpected":true}}"#).unwrap_err();
        assert!(broken.contains("找不到模型数组"), "{broken}");
        assert!(broken.contains("unexpected"), "报错要带原文：{broken}");
    }

    /// `--all-capabilities` 那一道名字过滤。它存在的意义是核对
    /// `capabilities=Realtime-Audio-Translate` 有没有漏掉模型，所以"漏"出来的那些
    /// 必须**打出来**，被过滤掉的必须不打——两个方向都要钉。
    #[test]
    fn all_capabilities_keeps_only_the_realtime_named_models() {
        let rows: Vec<ModelRow> = [
            "qwen3.8-livetranslate-flash-realtime",
            "qwen-realtime-vision",
            "qwen3-max",
            "qwen3.5-livetranslate-flash-realtime",
        ]
        .iter()
        .map(|model| ModelRow {
            model: (*model).to_string(),
            ..Default::default()
        })
        .collect();

        let all = ListQuery {
            service_site: None,
            all_capabilities: true,
        };
        let kept: Vec<&str> = rows
            .iter()
            .filter(|row| all.keep(row))
            .map(|row| row.model.as_str())
            .collect();
        assert_eq!(kept.len(), 3, "qwen3-max 不该留下：{kept:?}");
        assert!(
            kept.contains(&"qwen-realtime-vision"),
            "名字含 realtime 的要留下"
        );
        assert!(kept.contains(&"qwen3.5-livetranslate-flash-realtime"));

        // 不过滤时（服务端已经按能力位筛过了）全部留下。
        assert!(rows.iter().filter(|row| plain_query().keep(row)).count() == 4);
    }

    /// `--list-models` 里混进了 realtime 专用开关 → 当场报错。默默忽略一半的话，
    /// 人会以为 `--audio off` 那半边也生效了。
    ///
    /// **走真的 `parse_from`**，不走手工填的 `Args`：`-h` 归一成 `--help` 就发生在
    /// 解析里，直接填 `given` 会绕开它。
    #[test]
    fn list_models_mode_rejects_the_realtime_flags() {
        let parse = |extra: &[&str]| {
            let argv = ["--list-models", "--dry-run"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(str::to_string);
            Args::parse_from(argv).unwrap()
        };
        for flag in [
            "--audio",
            "--voice",
            "--target-lang",
            "--send-audio",
            "--session-json",
        ] {
            let value = match flag {
                "--send-audio" => "silence:1",
                "--session-json" => "/tmp/x.json",
                "--audio" => "off",
                _ => "Tina",
            };
            let args = parse(&[flag, value]);
            assert_eq!(
                args.list_models_stray_flags().as_deref(),
                Some(flag),
                "{flag} 在 --list-models 下应当被拒"
            );
        }
        // 这一档认的开关一个都不该被拒（包括归一后的 `-h`）。
        assert_eq!(
            parse(&["--all-capabilities", "-h"]).list_models_stray_flags(),
            None
        );
        assert_eq!(
            parse(&["--service-site", "cn-hangzhou"]).list_models_stray_flags(),
            None
        );
        assert_eq!(
            parse(&["--list-base", "https://d.example"]).list_models_stray_flags(),
            None
        );
    }

    /// 非 200 时要把响应体打出来给人看，但服务端把 `Authorization` 原样吐回来是
    /// 已知的一条泄漏路径——打之前必须去过密钥。
    #[test]
    fn response_bodies_are_redacted_before_they_are_printed() {
        let key = "sk-realkey999";
        // 服务端把头原样回显。
        let echoed = format!("{{\"code\":\"InvalidApiKey\",\"message\":\"bad key {key}\"}}");
        let safe = redact(&echoed, key);
        assert!(!safe.contains(key), "密钥还在：{safe}");
        assert!(safe.contains("InvalidApiKey"), "错误本身要留着：{safe}");

        // 回显成 `Bearer <token>` 形式。
        let bearer = format!("Authorization: Bearer {key} rejected");
        assert!(!redact(&bearer, key).contains(key));
        // 连键都不是的那个 token 也要去掉（服务端换成了一枚别的/截断的键）。
        assert!(!redact("Bearer sk-somethingelse", key).contains("sk-somethingelse"));

        // 键里没有的东西原样保留，别把诊断信息也抹了。
        assert_eq!(
            redact("page_no=2 out of range", key),
            "page_no=2 out of range"
        );
    }

    /// `--list-base` 只收"到主机名为止的基址"。路径探针自己接，收进去就会拼出
    /// `/api/v1/models/api/v1/models`；带凭据的一律拒（理由同 realtime 那条）。
    #[test]
    fn list_base_is_sanitized() {
        assert_eq!(
            sanitize_list_base("  https://dashscope.aliyuncs.com/  ").unwrap(),
            "https://dashscope.aliyuncs.com"
        );
        for junk in [
            "wss://dashscope.aliyuncs.com",
            "https://",
            "https://d.example/api/v1",
            "https://d.example?key=sk-abc",
            "  ",
        ] {
            assert!(sanitize_list_base(junk).is_err(), "{junk:?} 应当报错");
        }
    }

    /// `--service-site` 给了空串等于"给了个没用的开关"，不该悄悄当成"没给"。
    #[test]
    fn an_empty_service_site_is_refused() {
        let mut args = args_with(None, ProbeRegion::Cn, "m");
        args.service_site = Some("   ".to_string());
        assert!(ListQuery::from_args(&args).is_err());
        args.service_site = None;
        assert!(ListQuery::from_args(&args).unwrap().service_site.is_none());
    }

    fn plain_query() -> ListQuery {
        ListQuery {
            service_site: None,
            all_capabilities: false,
        }
    }
}
