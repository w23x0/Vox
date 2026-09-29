//! stdio ⇄ 本机 HTTP 的**桥**（`voxctl serve-stdio`）。
//!
//! 桌面宿主（Claude Desktop 那类 `command`/`args` 型配置）只会"起子进程 + 读写 stdio"，
//! 而控制面只有一条入口：本机 Streamable HTTP（§2.2.1 的传输矩阵）。这个文件就是两者之间
//! 那层**足够笨**的转发器：
//!
//! ```text
//!   宿主 ──stdin（换行分隔的 JSON-RPC）──▶ 桥 ──POST 127.0.0.1:<port>/mcp──▶ 控制面（app / 无屏档）
//!   宿主 ◀─stdout（一行一条 JSON-RPC）── 桥 ◀──JSON / text/event-stream──── 控制面
//! ```
//!
//! **它不拥有账本、不开设备**：第二个 Vox 实例会抢声卡，单实例插件也不让起。所以桥里
//! 没有 `Runtime`、没有 `LedgerBackend`、没有 `serve()`——只有 [`ControlPlane`] 那条瘦客户端
//! 路径。协议语义一行都不在这里：它转发的是宿主写下的**原字节**，回来的也原样写出去。
//!
//! 四件事是**必须**由桥做（HTTP 侧没有对应物）：
//!
//! 1. **分帧**：stdin 上一行一条消息（MCP 的 stdio 传输就是换行分隔；消息里不许有裸换行）；
//! 2. **补头**：HTTP 面要求的三个 `Mcp-*` 头从消息本身推（[`crate::client`] 的 `mcp_headers`）；
//! 3. **长流**：`subscriptions/listen` 在 HTTP 上是 `text/event-stream`——桥把每条 `data:`
//!    负载写一行到 stdout（宿主看到的就是"服务端推来的一条消息"）；
//! 4. **取消**：stdio 上客户端用 `notifications/cancelled` 取消请求，而 HTTP 上"关掉那条 POST
//!    的响应流"才是取消（规范：HTTP 没有这条通知）——桥把前者映射成后者。
//!
//! **stdout 只放 MCP 消息**：日志一律 stderr（宿主按行解析 stdout，多一个字都是协议污染）。
//! 上游的传输失败（连不上、401、看不懂的响应）翻成一条 `-32603` 错误响应（带原请求的 id）——
//! 宿主在等一条响应，而"等一个永远不会来的响应"比一条说得清的错误坏得多；通知（没有 id）
//! 只写 stderr。

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::client::{ControlPlane, Reply};
use crate::jsonrpc::{self, code};

/// 客户端取消一条请求的通知（规范 `basic/index` 的通知之一）。
const METHOD_CANCELLED: &str = "notifications/cancelled";

/// 跑桥：读 stdin 直到 EOF，退出码 `0`（干净收工）/ `2`（stdin 读坏了）。
///
/// **一条消息一个线程**：`subscriptions/listen` 的那条流会一直占着它那条上游连接，而宿主会在
/// 流开着的时候接着发别的请求（`tools/call`、`resources/read`）——单线程会把它们全堵在长流后面。
/// 线程之间只共享三样东西：stdout（一把锁，一行一条）、在册的上游流（按 JSON-RPC id，给取消用）、
/// 以及"还有几条在飞"的计数（stdin 关了之后要等它们把响应写完）。
pub fn serve(plane: ControlPlane) -> u8 {
    let out = Arc::new(Mutex::new(io::stdout()));
    let streams: Arc<Mutex<HashMap<String, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
    let inflight = Arc::new(InFlight::default());

    eprintln!(
        "stdio 桥已接上 http://{}{}（账本与设备在那边，本进程不碰）",
        plane.addr(),
        crate::transport::http::PATH
    );

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("stdio 桥：读 stdin 失败：{error}");
                return 2;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue; // 空行不是消息
        }

        // 解析只为了两件事：推请求头、认取消。**原文照转**（见 `ControlPlane::post_raw`）。
        let message = serde_json::from_str::<Value>(line).ok();
        if let Some(request_id) = cancelled_request(message.as_ref()) {
            cancel(&streams, &request_id);
            continue;
        }

        let plane = plane.clone();
        let out = Arc::clone(&out);
        let streams = Arc::clone(&streams);
        let inflight = Arc::clone(&inflight);
        let worker = Arc::clone(&inflight);
        let body = line.to_string();
        inflight.enter();
        let spawned = thread::Builder::new()
            .name("voxctl-stdio".to_string())
            .spawn(move || {
                forward(&plane, &body, message.as_ref(), &out, &streams);
                worker.leave();
            });
        if let Err(error) = spawned {
            inflight.leave();
            eprintln!("stdio 桥：转发线程起不来（这条消息没有回应）：{error}");
        }
    }

    // stdin 关了 = 宿主收摊：先把还开着的上游长流关掉（HTTP 上关流就是取消），再等**已经在飞**
    // 的那几条把响应写完。
    //
    // 这一步不能省：`printf '…\n' | voxctl serve-stdio` 这种一次性用法里，stdin 紧接着就是 EOF，
    // 不等的话进程会在响应写出去之前退出（stdout 上一个字都没有）。等待有上限（[`DRAIN`]）：
    // 宿主已经走了，我们不该陪它等一个卡住的上游。
    for (_, stream) in lock(&streams).drain() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    inflight.drain(DRAIN);
    0
}

/// stdin 关了之后最多等多久把在飞的响应写完。
const DRAIN: Duration = Duration::from_secs(5);

/// "还有几条消息在飞"：计数 + 条件变量（stdin 关了就等它归零）。
#[derive(Default)]
struct InFlight {
    count: Mutex<usize>,
    done: Condvar,
}

impl InFlight {
    fn enter(&self) {
        *lock(&self.count) += 1;
    }

    fn leave(&self) {
        let mut count = lock(&self.count);
        *count = count.saturating_sub(1);
        self.done.notify_all();
    }

    /// 等到归零，或者到 `deadline` 为止。
    fn drain(&self, deadline: Duration) {
        let until = Instant::now() + deadline;
        let mut count = lock(&self.count);
        while *count > 0 {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                eprintln!("stdio 桥：还有 {count} 条消息没写完，不等了");
                return;
            }
            count = self
                .done
                .wait_timeout(count, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

/// 转发一条消息，并把上游的回答写到 stdout。
fn forward(
    plane: &ControlPlane,
    body: &str,
    message: Option<&Value>,
    out: &Mutex<io::Stdout>,
    streams: &Mutex<HashMap<String, TcpStream>>,
) {
    let reply = match message {
        Some(message) => plane.post_raw(body, message),
        // 连 JSON 都不是：照样转（服务端会按 `-32700` 如实回），头只给版本那一个。
        None => plane.post_raw(body, &Value::Null),
    };

    match reply {
        // 一条响应：原样写出去（成功、JSON-RPC 错误、工具结果的 `isError` 都在这一格里）。
        Ok(Reply::Message(response)) => write_message(out, &response),
        // 通知：规范要求 202 且无 body，什么都不回。
        Ok(Reply::Accepted) => {}
        // 长流：逐条把 `data:` 负载写成一行，直到流结束（服务端收流 / 对端关了 / 被取消）。
        Ok(Reply::Stream(mut sse)) => {
            let request_id = request_id(message);
            if let (Some(id), Ok(handle)) = (&request_id, sse.handle()) {
                lock(streams).insert(id.to_string(), handle);
            }
            loop {
                match sse.next_message() {
                    Ok(Some(event)) => write_message(out, &event),
                    Ok(None) => break,
                    Err(error) => {
                        eprintln!("stdio 桥：读上游长流失败：{error}");
                        break;
                    }
                }
            }
            if let Some(id) = request_id {
                lock(streams).remove(&id.to_string());
            }
        }
        Ok(Reply::Unexpected { status, body }) => {
            let reason = if body.trim().is_empty() {
                format!("本机控制面回了 HTTP {status}（无 body）")
            } else {
                format!("本机控制面回了 HTTP {status}：{}", body.trim())
            };
            transport_failure(out, message, &reason);
        }
        Err(error) => transport_failure(out, message, &format!("连不上本机控制面：{error}")),
    }
}

/// 上游的传输失败 → 一条 `-32603`（带原请求的 id）；通知（没有 id）只写 stderr。
fn transport_failure(out: &Mutex<io::Stdout>, message: Option<&Value>, reason: &str) {
    match request_id(message) {
        Some(id) => write_message(
            out,
            &jsonrpc::error_result(
                Some(&id),
                &jsonrpc::ErrorObject::new(code::INTERNAL_ERROR, reason),
            ),
        ),
        None => eprintln!("stdio 桥：{reason}"),
    }
}

/// 取消：把在册的那条上游流关掉（HTTP 上这就是取消）。没有在册的流就只写一行 stderr——
/// **不**把这条通知转上去：HTTP 面收到它只会回 202（那边的取消语义是关流，不是通知）。
fn cancel(streams: &Mutex<HashMap<String, TcpStream>>, request_id: &str) {
    match lock(streams).remove(request_id) {
        Some(stream) => {
            let _ = stream.shutdown(Shutdown::Both);
            eprintln!("stdio 桥：收到 notifications/cancelled，已收掉上游流 #{request_id}");
        }
        None => eprintln!("stdio 桥：notifications/cancelled 指向的请求不在册：#{request_id}"),
    }
}

/// 一行一条消息，写完就 flush（宿主在等它）。**stdout 上只有这一个写入口**。
///
/// 写失败不在这里处理：stdout 没了意味着宿主已经走了，而"宿主走了"由 stdin 的 EOF 表达
/// （主循环那条路会把在飞的响应收干净再退出）。为了一个已经没人读的管道去拆桥，只会让
/// 退出路径多一条分支。
fn write_message(out: &Mutex<io::Stdout>, message: &Value) {
    let mut stdout = lock(out);
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

/// 这条消息的 JSON-RPC id（通知没有 id）。**原样**返回那个 `Value`：取消时要拿它回一条
/// 一模一样的响应 id（数字还是数字、字符串还是字符串），在册表的键用它的文本形态。
fn request_id(message: Option<&Value>) -> Option<Value> {
    let id = message?.get("id")?;
    if id.is_null() {
        return None;
    }
    Some(id.clone())
}

/// 这条消息是不是"取消某条请求"：是就给出被取消的 id。
fn cancelled_request(message: Option<&Value>) -> Option<String> {
    let message = message?;
    if message.get("method").and_then(Value::as_str) != Some(METHOD_CANCELLED) {
        return None;
    }
    let request_id = message.pointer("/params/requestId")?;
    if request_id.is_null() {
        return None;
    }
    Some(request_id.to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
