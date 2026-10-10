//! 连一次 PipeWire、收一轮"图快照"、断开；外加全 crate 共用的那点 PipeWire 胶水
//! （连接、错误翻译、请求格式、后台线程的命令通道）。
//!
//! 快照这里刻意做成**一次性**的：设备枚举是低频动作（装配层 2 秒轮询一次），
//! 每次连一次、roundtrip 一轮、拿完就散，不维护常驻连接、不引后台线程。
//! 跟 Windows 侧 `WinDeviceRegistry` 每次新建一个 COM guard 是同一个路子。
//!
//! 采集和播放（要长期持有流）不走这里，各起自己的主循环线程，见 `capture.rs` /
//! `playback.rs`。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pipewire as pw;
use pw::proxy::{Listener, ProxyT};
use pw::spa;
use pw::spa::pod::Pod;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;
use vox_core::ports::{PortError, PortResult};

/// 输入设备（麦克风、虚拟源）。
pub(crate) const CLASS_SOURCE: &str = "Audio/Source";
/// 虚拟源（`Audio/Source/Virtual`）——PipeWire 给"监听某 sink"这类源的分档。
pub(crate) const CLASS_SOURCE_VIRTUAL: &str = "Audio/Source/Virtual";
/// 输出设备。
pub(crate) const CLASS_SINK: &str = "Audio/Sink";
/// 某个程序正在放的那条流。按程序抓音就是找这些节点。
pub(crate) const CLASS_OUTPUT_STREAM: &str = "Stream/Output/Audio";

/// 一个节点（设备、虚拟设备或某程序的一条播放流）。
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeRecord {
    pub media_class: String,
    /// `node.name`：稳定标识，用来做默认设备比对和 target 匹配。
    pub name: String,
    /// `node.description`：给人看的名字。
    pub description: String,
    /// 节点自带的程序名（`application.name`）。
    pub app_name: Option<String>,
    /// 节点自带的 `application.process.binary`。
    pub binary: Option<String>,
    /// `client.id`：指向下面的 `ClientRecord`。
    pub client_id: Option<u32>,
    /// 节点状态是不是 `running`（正在出声 / 正在采集）。
    pub running: bool,
}

/// 一个端口。混音（把同一程序的其它流接进来）要按 node/port id 显式建链，
/// 靠它找到两边的端口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortRecord {
    pub id: u32,
    /// `port.name`：`output_FL` / `input_FL` 这种。
    pub name: String,
    /// `port.direction`：`in` / `out`。
    pub direction: String,
}

/// 一个客户端进程（一条流属于谁）。
#[derive(Debug, Clone, Default)]
pub(crate) struct ClientRecord {
    pub app_name: Option<String>,
    pub binary: Option<String>,
    pub pid: Option<u32>,
}

/// 一轮图快照。
#[derive(Debug, Default, Clone)]
pub(crate) struct GraphSnapshot {
    pub nodes: HashMap<u32, NodeRecord>,
    pub clients: HashMap<u32, ClientRecord>,
    /// 按节点分组的端口：`node id -> 端口`。
    pub ports: HashMap<u32, Vec<PortRecord>>,
    /// 默认输入设备的 `node.name`（wireplumber 的 `default.audio.source`）。
    pub default_source: Option<String>,
    /// 默认输出设备的 `node.name`。
    pub default_sink: Option<String>,
}

impl GraphSnapshot {
    /// 按 `media.class` 取节点，按 `node.name` 排序保证顺序稳定（UI 里列表不跳）。
    pub fn nodes_of_class(&self, class: &str) -> Vec<&NodeRecord> {
        let mut v: Vec<&NodeRecord> = self
            .nodes
            .values()
            .filter(|n| n.media_class == class)
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// 某个程序名对应的所有播放流节点（大小写不敏感）。
    ///
    /// 同一程序多条流（Chromium 每个标签页一条）会全部返回，调用方负责全连上混音。
    pub fn output_streams_of(&self, executable: &str) -> Vec<&NodeRecord> {
        let want = executable.to_ascii_lowercase();
        let mut streams: Vec<&NodeRecord> = self
            .nodes
            .values()
            .filter(|node| node.media_class == CLASS_OUTPUT_STREAM)
            .filter(|node| {
                self.binary_of(node)
                    .map(|binary| binary.to_ascii_lowercase() == want)
                    .unwrap_or(false)
            })
            .collect();
        streams.sort_by_key(|node| node.name.clone());
        streams
    }

    /// 某个节点的端口，按名字排序（`input_FL` 在 `input_FR` 前面，建链顺序稳定）。
    pub fn ports_of(&self, node_id: u32) -> Vec<&PortRecord> {
        let mut ports: Vec<&PortRecord> = self
            .ports
            .get(&node_id)
            .map(|ports| ports.iter().collect())
            .unwrap_or_default();
        ports.sort_by(|a, b| a.name.cmp(&b.name));
        ports
    }

    /// 某个节点某个方向的端口。
    pub fn ports_of_dir(&self, node_id: u32, direction: &str) -> Vec<&PortRecord> {
        self.ports_of(node_id)
            .into_iter()
            .filter(|port| port.direction == direction)
            .collect()
    }

    /// 某个节点的所属客户端（可能没有：系统创建的设备节点没有 client）。
    pub fn client_of<'a>(&'a self, node: &'a NodeRecord) -> Option<&'a ClientRecord> {
        node.client_id.and_then(|id| self.clients.get(&id))
    }

    /// 程序的 `executable`：优先客户端报的 `application.process.binary`，退到节点自己报的。
    pub fn binary_of<'a>(&'a self, node: &'a NodeRecord) -> Option<&'a str> {
        self.client_of(node)
            .and_then(|c| c.binary.as_deref())
            .or(node.binary.as_deref())
    }

    /// 程序的 pid：只有客户端报。拿不到就返回 `None`（不编一个假的）。
    pub fn pid_of(&self, node: &NodeRecord) -> Option<u32> {
        self.client_of(node).and_then(|c| c.pid)
    }

    pub fn app_name_of<'a>(&'a self, node: &'a NodeRecord) -> Option<&'a str> {
        self.client_of(node)
            .and_then(|c| c.app_name.as_deref())
            .or(node.app_name.as_deref())
    }
}

/// 绑定出来的代理和它们的监听器必须活到事件收完为止。
///
/// PipeWire 的 `bind` 返回的代理一旦 drop，监听器就失效——所以这里把
/// `(proxy, listener)` 成对存着，等快照收完再一起丢。
type KeptProxies = Rc<RefCell<Vec<(Box<dyn ProxyT>, Box<dyn Listener>)>>>;

/// 库里唯一的全局初始化。PipeWire 的 `pw_init` 幂等，重复调没事。
pub(crate) fn init() {
    pw::init();
}

/// PipeWire 在不在（能连上就算在）。给"能力门槛"用：连不上就没有按进程抓音。
pub(crate) fn available() -> bool {
    snapshot().is_ok()
}

/// 收一轮图快照。任何一步失败都翻成中文 `PortError`，不 panic。
pub(crate) fn snapshot() -> PortResult<GraphSnapshot> {
    init();
    let (main_loop, core) = connect()?;
    let registry = core.get_registry_rc().map_err(map_err)?;

    let graph = Rc::new(RefCell::new(GraphSnapshot::default()));
    let keep: KeptProxies = Rc::new(RefCell::new(Vec::new()));

    let registry_listener = registry
        .add_listener_local()
        .global({
            let graph = Rc::clone(&graph);
            let keep = Rc::clone(&keep);
            let registry_weak = registry.downgrade();
            move |obj| {
                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                match obj.type_ {
                    ObjectType::Node => {
                        let Ok(node) = registry.bind::<pw::node::Node, _>(obj) else {
                            return;
                        };
                        let id = obj.id;
                        let listener = node
                            .add_listener_local()
                            .info({
                                let graph = Rc::clone(&graph);
                                move |info| {
                                    let record = node_record(info);
                                    graph.borrow_mut().nodes.insert(id, record);
                                }
                            })
                            .register();
                        keep.borrow_mut().push((Box::new(node), Box::new(listener)));
                    }
                    ObjectType::Client => {
                        let Ok(client) = registry.bind::<pw::client::Client, _>(obj) else {
                            return;
                        };
                        let id = obj.id;
                        let listener = client
                            .add_listener_local()
                            .info({
                                let graph = Rc::clone(&graph);
                                move |info| {
                                    let record = client_record(info);
                                    graph.borrow_mut().clients.insert(id, record);
                                }
                            })
                            .register();
                        keep.borrow_mut()
                            .push((Box::new(client), Box::new(listener)));
                    }
                    ObjectType::Port => {
                        let Ok(port) = registry.bind::<pw::port::Port, _>(obj) else {
                            return;
                        };
                        let listener = port
                            .add_listener_local()
                            .info({
                                let graph = Rc::clone(&graph);
                                move |info| {
                                    let props = DictLookup(info.props());
                                    let Some(node_id) =
                                        props.get("node.id").and_then(|v| v.parse().ok())
                                    else {
                                        return;
                                    };
                                    let record = PortRecord {
                                        id: info.id(),
                                        name: props.get("port.name").unwrap_or_default(),
                                        direction: props.get("port.direction").unwrap_or_default(),
                                    };
                                    graph
                                        .borrow_mut()
                                        .ports
                                        .entry(node_id)
                                        .or_default()
                                        .push(record);
                                }
                            })
                            .register();
                        keep.borrow_mut().push((Box::new(port), Box::new(listener)));
                    }
                    ObjectType::Metadata => {
                        let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(obj) else {
                            return;
                        };
                        let listener = metadata
                            .add_listener_local()
                            .property({
                                let graph = Rc::clone(&graph);
                                move |subject, key, _type, value| {
                                    // subject 0 = 全局元数据；wireplumber 把默认设备写在这里。
                                    if subject != 0 {
                                        return 0;
                                    }
                                    let Some(key) = key else { return 0 };
                                    let Some(name) = value.and_then(json_name) else {
                                        return 0;
                                    };
                                    let mut g = graph.borrow_mut();
                                    match key {
                                        "default.audio.source" => g.default_source = Some(name),
                                        "default.audio.sink" => g.default_sink = Some(name),
                                        _ => {}
                                    }
                                    0
                                }
                            })
                            .register();
                        keep.borrow_mut()
                            .push((Box::new(metadata), Box::new(listener)));
                    }
                    _ => {}
                }
            }
        })
        .register();

    // **两轮** roundtrip，一轮不够：第一轮收到的是全局对象列表，我们在回调里
    // 才去 bind 节点/客户端代理；这些 bind 请求排在 sync 之后才到服务端，
    // 它们的信息事件要第二轮才收得到。少跑一轮的表现是"图快照永远是空的"。
    roundtrip(&main_loop, &core)?;
    roundtrip(&main_loop, &core)?;

    let out = graph.borrow().clone();
    drop(registry_listener);
    Ok(out)
}

/// 建一次连接，返回（主循环，核心）。调用方负责让主循环活着。
pub(crate) fn connect() -> PortResult<(pw::main_loop::MainLoopRc, pw::core::CoreRc)> {
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(map_err)?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(map_err)?;
    let core = context.connect_rc(None).map_err(map_err)?;
    Ok((main_loop, core))
}

/// 跑一轮 roundtrip，把服务端已经发出的事件收干净。
///
/// 收发事件必须在同一个线程上跑主循环，所以这里是**阻塞**的：拿到 `done`
/// 事件就叫停主循环。这也是为什么枚举不能在 Tauri 主线程上做——装配层那侧
/// 已经把它放在设备轮询线程里了。
pub(crate) fn roundtrip(
    main_loop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
) -> PortResult<()> {
    let done = Rc::new(Cell::new(false));
    let seq = core.sync(0).map_err(map_err)?;
    let listener = {
        let done = Rc::clone(&done);
        let main_loop = main_loop.clone();
        core.add_listener_local()
            .done(move |id, pending| {
                if id == pw::core::PW_ID_CORE && pending == seq {
                    done.set(true);
                    main_loop.quit();
                }
            })
            .register()
    };
    while !done.get() {
        main_loop.run();
    }
    drop(listener);
    Ok(())
}

/// PipeWire 的错误翻成中文，带上原始描述（用户看到的是这个字符串）。
pub(crate) fn map_err(err: pw::Error) -> PortError {
    PortError::new(format!("PipeWire error: {err}"))
}

/// 我们向图请求的格式：48 kHz 立体声 f32。
///
/// 48 kHz 是 RNNoise 的原生率；采集与播放请求的是同一份，由 PipeWire 在图里转成
/// 设备/程序真正要的（Linux 上不用像 Windows 那样自己探设备率）。
pub(crate) const REQUEST_RATE: u32 = 48_000;
pub(crate) const REQUEST_CHANNELS: u16 = 2;

/// 把请求格式序列化成 `EnumFormat` 参数并连到流上。采集与播放唯一不同的是方向与 flag。
pub(crate) fn connect_request_format(
    stream: &pw::stream::StreamBox,
    direction: spa::utils::Direction,
    flags: pw::stream::StreamFlags,
) -> PortResult<()> {
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(REQUEST_RATE);
    audio_info.set_channels(REQUEST_CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);

    let values = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pw::spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| PortError::new(format!("failed to build the audio format: {e}")))?
    .0
    .into_inner();
    let pod = Pod::from_bytes(&values)
        .ok_or_else(|| PortError::new("the serialized audio format is invalid"))?;
    let mut params = [pod];

    stream
        .connect(direction, None, flags, &mut params)
        .map_err(map_err)?;
    Ok(())
}

/// 后台线程（采集、播放、链路守护）的收工命令：三者都只有"停"这一件事。
pub(crate) enum Cmd {
    Quit,
}

/// 把命令通道挂到主循环上：收到 `Quit` 就叫停主循环，线程随之收工。
///
/// 返回值必须活到线程结束为止——drop 掉就等于把监听摘了。
pub(crate) fn attach_quit(
    main_loop: &pw::main_loop::MainLoopRc,
    cmd_rx: pw::channel::Receiver<Cmd>,
) -> pw::channel::AttachedReceiver<'_, Cmd> {
    let loop_weak = main_loop.downgrade();
    cmd_rx.attach(main_loop.loop_(), move |cmd| match cmd {
        Cmd::Quit => {
            if let Some(main_loop) = loop_weak.upgrade() {
                main_loop.quit();
            }
        }
    })
}

/// PipeWire 给的是字节切片，我们按 f32 读。长度一定 4 的整数倍（stride 算过）。
pub(crate) fn as_f32_slice(slice: &[u8]) -> &[f32] {
    // SAFETY：f32 对齐要求 4，PipeWire 的缓冲按 4 字节对齐；长度取整到 4 的倍数。
    let len = slice.len() / std::mem::size_of::<f32>();
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<f32>(), len) }
}

/// 同上，可变版：播放侧要往缓冲里写。
pub(crate) fn as_f32_slice_mut(slice: &mut [u8]) -> &mut [f32] {
    // SAFETY：同 `as_f32_slice`。这条转换在音频路径上很常见，
    // 用 unsafe 换掉每帧一次拷贝。
    let len = slice.len() / std::mem::size_of::<f32>();
    unsafe { std::slice::from_raw_parts_mut(slice.as_mut_ptr().cast::<f32>(), len) }
}

fn node_record(info: &pw::node::NodeInfoRef) -> NodeRecord {
    let props = DictLookup(info.props());
    NodeRecord {
        media_class: props.get("media.class").unwrap_or_default(),
        name: props.get("node.name").unwrap_or_default(),
        description: props
            .get("node.description")
            .or_else(|| props.get("node.nick"))
            .unwrap_or_default(),
        app_name: props.get("application.name"),
        binary: props.get("application.process.binary"),
        client_id: props.get("client.id").and_then(|s| s.parse().ok()),
        running: matches!(info.state(), pw::node::NodeState::Running),
    }
}

fn client_record(info: &pw::client::ClientInfoRef) -> ClientRecord {
    let props = DictLookup(info.props());
    ClientRecord {
        app_name: props.get("application.name"),
        binary: props.get("application.process.binary"),
        pid: props
            .get("application.process.id")
            .and_then(|s| s.parse().ok()),
    }
}

/// 字典取值的小包装：不存在或空串 → `None`。
struct DictLookup<'a>(Option<&'a DictRef>);

impl DictLookup<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .and_then(|d| d.get(key))
            .map(|s| s.to_owned())
            .filter(|s| !s.is_empty())
    }
}

/// wireplumber 的默认设备是 `{"name":"<node.name>"}` 这种 Spa JSON，只取 `name`。
fn json_name(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()?
        .get("name")?
        .as_str()
        .map(|s| s.to_owned())
}
