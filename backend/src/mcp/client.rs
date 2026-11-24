//! MCP 传输：把 JSON-RPC 2.0 跑在 **stdio 子进程**或 **Streamable HTTP** 上。
//!
//! 两种传输的共同形状是「一问一答」：`request` 发一条 JSON-RPC、等一条回应。
//! 差别只在底下怎么送：
//!
//! - **stdio**：起子进程，读线程逐行读它的 stdout，按 `id` 派发给等待者。
//!   适合本机自建服务器（`comfy-mcp` 之类）。
//! - **HTTP**（Streamable HTTP）：POST 到一个端点，服务器用 `Mcp-Session-Id`
//!   头维持会话；响应可能是普通 JSON，也可能是 SSE 流。适合远程、或已有网关
//!   上挂着的服务器（本机 magpie 的 `fetch` 就是这种）。
//!
//! 实现刻意保持在"够用"这一档：没有 OAuth、没有重连风暴、没有多路复用。
//! 自己实现比引一整套 SDK 更可控——依赖树不变，行为可测，出问题时的现场
//! 就是这几百行代码。

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::config::{ServerConfig, TransportKind};
use super::protocol::{self, CallResult, ToolInfo, PROTOCOL_VERSION};

/// 一种传输：发请求 / 发通知，外加存活判断与收尾。
trait Transport: Send + Sync {
    /// 发一个请求并等回应。
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String>;
    /// 发一个通知（无回应）。
    fn notify(&self, method: &str, params: Value, timeout: Duration) -> Result<(), String>;
    /// 连接是否还可用。
    fn is_alive(&self) -> bool;
    /// 服务器 stderr 的尾部若干行（供错误信息附上；没有则为空）。
    fn stderr_tail(&self) -> Vec<String> {
        Vec::new()
    }
    /// 收尾（杀进程 / 关会话）。
    fn shutdown(&self);
}

/// stderr 尾部保留的行数：够看出 Python 的 traceback 头几行，又不至于把
/// 错误信息撑成一整屏日志。
const STDERR_TAIL_LINES: usize = 20;

/// 把服务器 stderr 的尾部附到错误信息后面。
///
/// 服务器起不来时，它的报错**只**写在这条管道里（Python traceback、uv 的
/// 安装失败…）。不把它带出来，调用方只看到"进程已退出"四个字，等于什么也没说。
fn with_stderr_tail(message: String, tail: &[String]) -> String {
    let tail: Vec<&str> = tail
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if tail.is_empty() {
        return message;
    }
    format!("{message}（服务器 stderr：\n{}\n）", tail.join("\n"))
}

/// 一个 MCP 客户端：传输 + MCP 协议流程（握手 / 列工具 / 调工具）。
pub struct McpClient {
    transport: Arc<dyn Transport>,
    /// 握手与工具列举的超时。
    ///
    /// 用服务器配置的 `timeout_secs`，不用更短的固定值：冷启动慢的服务器
    /// （`uvx`/`npx` 首次拉依赖、Python 首次导入模块）在这段时间里还没开始
    /// 应答，用 30s 的固定值会把"启动慢"误报成"连不上"。
    timeout: Duration,
}

impl McpClient {
    /// 按配置连上服务器并完成握手。
    pub fn connect(config: &ServerConfig) -> Result<Self, String> {
        let transport: Arc<dyn Transport> = match config.transport() {
            TransportKind::Stdio => Arc::new(StdioTransport::connect(config)?),
            #[cfg(feature = "mcp-http")]
            TransportKind::Http => Arc::new(HttpTransport::new(config)?),
            #[cfg(not(feature = "mcp-http"))]
            TransportKind::Http => {
                return Err(
                    "本二进制没有编译 MCP 的 HTTP 传输（需要 feature `mcp-http`）".to_string(),
                )
            }
        };
        let client = Self {
            transport,
            timeout: config.timeout(),
        };
        // 握手失败时把服务器 stderr 带上：起不来的原因只写在它自己的管道里。
        client
            .initialize()
            .map_err(|error| with_stderr_tail(error, &client.transport.stderr_tail()))?;
        Ok(client)
    }

    /// 握手：`initialize` → `notifications/initialized`。
    fn initialize(&self) -> Result<(), String> {
        let result = self.transport.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "zhiwei", "version": env!("CARGO_PKG_VERSION")},
            }),
            self.timeout,
        )?;
        if let Some(version) = result.get("protocolVersion").and_then(|v| v.as_str()) {
            tracing::info!(server_version = version, "MCP 握手完成");
        }
        self.transport
            .notify("notifications/initialized", json!({}), self.timeout)?;
        Ok(())
    }

    /// 取服务器的工具列表（跟随 `nextCursor` 翻页）。
    pub fn list_tools(&self) -> Result<Vec<ToolInfo>, String> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = json!({});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let result = self.transport.request("tools/list", params, self.timeout)?;
            all.extend(protocol::parse_tools(&result));
            cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|c| !c.is_empty())
                .map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// 调用一个工具。
    pub fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<CallResult, String> {
        let result = self.transport.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            timeout,
        )?;
        Ok(protocol::parse_call_result(&result))
    }

    /// 连接是否还可用。
    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    /// 收尾。
    pub fn shutdown(&self) {
        self.transport.shutdown();
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// —————————————————————————————————————— stdio 传输 ——————————————————————————————————————

/// 一条 stdio 连接的共享状态。
struct Conn {
    /// 写侧（子进程 stdin）。抽象成 trait 是为了让测试能注入内存缓冲。
    writer: Mutex<Box<dyn Write + Send>>,
    /// 在途请求：`id` → 回应通道。
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    /// 进程是否还活着。退出后所有等待者立刻失败，而不是等到超时。
    alive: AtomicBool,
}

impl Conn {
    fn new(writer: Box<dyn Write + Send>) -> Arc<Self> {
        Arc::new(Self {
            writer: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
        })
    }

    /// 写一条 JSON-RPC 消息（一行，不带换行以外的空白）。
    fn write_message(&self, value: &Value) -> Result<(), String> {
        let line = serde_json::to_string(value).map_err(|e| format!("序列化 MCP 消息失败：{e}"))?;
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush())
            .map_err(|e| format!("写 MCP 服务器失败：{e}"))
    }

    /// 处理收到的一条消息：响应按 `id` 派发，请求（ping）就地回一句。
    fn handle_message(&self, value: &Value) {
        if let Some(method) = value.get("method").and_then(|v| v.as_str()) {
            if method == "ping" {
                if let Some(id) = value.get("id") {
                    let reply = json!({"jsonrpc": "2.0", "id": id, "result": {}});
                    let _ = self.write_message(&reply);
                }
            } else {
                tracing::debug!(method, "忽略 MCP 服务器发来的请求");
            }
            return;
        }
        let Some(id) = value.get("id").and_then(|v| v.as_u64()) else {
            return;
        };
        let outcome = match value.get("error") {
            Some(err) => Err(error_message(err)),
            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        };
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
    }

    /// 连接已断时，让所有等待者立刻拿到失败（而不是各自等到超时）。
    fn fail_all(&self, reason: &str) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(reason.to_string()));
        }
    }
}

/// 把 JSON-RPC 的 `error` 字段转成人读的消息。
fn error_message(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    match error.get("code").and_then(|v| v.as_i64()) {
        Some(code) => format!("{message}（code {code}）"),
        None => message.to_string(),
    }
}

/// 读线程：逐行读服务器输出，交给连接派发。
///
/// 读法刻意不用 `BufRead::lines()`：它把"非 UTF-8"与"行尾"混为一谈——服务器
/// 写出一行非 UTF-8 字节（Windows 中文环境下按 GBK 写日志很常见）时，它直接
/// 返回错误，而 `lines()` 的迭代就此终止，读线程会把**整个连接**当成已断，
/// 之后所有调用都报"MCP 服务器进程已退出"。服务器还活着，只是一行坏了。
///
/// 故这里按字节读行（`read_until`），坏行只丢这一行。
fn reader_loop(conn: Arc<Conn>, stdout: std::process::ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            // 读到 0 字节 = 真正的 EOF：服务器确实结束了。
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(error = %error, "MCP 读取服务器输出失败");
                break;
            }
        }
        handle_raw_line(&conn, &buf);
    }
    conn.alive.store(false, Ordering::SeqCst);
    conn.fail_all("MCP 服务器进程已退出");
}

/// 处理服务器送来的一行原始字节。
///
/// 丢掉的是**这一行**，不是整条连接：
///
/// - 非 UTF-8 → 记一条警告后跳过（不猜、不替换：猜出来的内容会带着坏字符
///   回填给模型，比丢掉更糟）；
/// - 非 JSON → 记一条警告后跳过（服务器调试输出混进了 stdout 的常见情形）。
///
/// 返回 `false` 表示这一行被丢弃。
fn handle_raw_line(conn: &Conn, raw: &[u8]) -> bool {
    let line = match std::str::from_utf8(raw) {
        // 行首的 BOM 剥掉：Windows 上用 UTF-8 写 stdout 的程序常带 BOM
        // （PowerShell 的 `[Console]::OutputEncoding = UTF8` 就会），留着它
        // JSON 解析会直接失败。
        Ok(text) => text.trim().trim_start_matches('\u{feff}').trim(),
        Err(error) => {
            tracing::warn!(error = %error, "MCP 服务器返回了非 UTF-8 行，已忽略");
            return false;
        }
    };
    if line.is_empty() {
        return false;
    }
    match serde_json::from_str::<Value>(line) {
        Ok(value) => {
            conn.handle_message(&value);
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "MCP 服务器返回了非 JSON 行，已忽略");
            false
        }
    }
}

/// stdio 传输：把子进程的标准输入输出当作 JSON-RPC 管道。
struct StdioTransport {
    conn: Arc<Conn>,
    /// 子进程句柄。留在本仓是为了**能主动杀掉它**：服务器进程若不收尾，
    /// 会在宿主退出后继续占着端口与显存。
    child: Mutex<Option<Child>>,
    /// 服务器 stderr 的尾部（起不来时它就是唯一的现场）。
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl StdioTransport {
    fn connect(config: &ServerConfig) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动 MCP 服务器 `{}` 失败：{e}", config.command))?;
        let stdout = child.stdout.take().ok_or("MCP 服务器没有 stdout")?;
        let stdin = child.stdin.take().ok_or("MCP 服务器没有 stdin")?;
        // stderr 必须单独抽干：服务器若往 stderr 写日志而没人读，管道写满后
        // 服务器会卡住——表现为"工具调用莫名超时"。
        // 抽干同样按字节读行：服务器写出一行非 UTF-8 的日志不该让抽干就此
        // 停止——那等于把管道重新堵回去，正是这段代码要防的事。
        //
        // 同时留一份尾部：服务器起不来时，它的报错只写在这条管道里（Python
        // 的 traceback 之类），不进日志就等于没有——错误信息里必须带上它，
        // 否则"进程已退出"这句话什么也说明不了。
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    tracing::debug!(line = %line, "MCP 服务器 stderr");
                    let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                    if tail.len() >= STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }
        let conn = Conn::new(Box::new(stdin));
        let reader_conn = conn.clone();
        std::thread::spawn(move || reader_loop(reader_conn, stdout));
        Ok(Self {
            conn,
            child: Mutex::new(Some(child)),
            stderr_tail,
        })
    }
}

impl Transport for StdioTransport {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.conn.alive.load(Ordering::SeqCst) {
            return Err(with_stderr_tail(
                "MCP 服务器进程已退出".to_string(),
                &self.stderr_tail(),
            ));
        }
        let id = self.conn.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.conn
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let payload = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.conn.write_message(&payload) {
            self.conn
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(with_stderr_tail(e, &self.stderr_tail())),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 超时后要把这条登记摘掉，否则等待者永远不会回收这条槽位。
                self.conn
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(format!("MCP 调用 {method} 超时（{}s）", timeout.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(with_stderr_tail(
                "MCP 服务器已关闭连接".to_string(),
                &self.stderr_tail(),
            )),
        }
    }

    fn notify(&self, method: &str, params: Value, _timeout: Duration) -> Result<(), String> {
        let payload = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.conn.write_message(&payload)
    }

    fn is_alive(&self) -> bool {
        self.conn.alive.load(Ordering::SeqCst)
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn shutdown(&self) {
        self.conn.alive.store(false, Ordering::SeqCst);
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// —————————————————————————————————————— HTTP 传输 ——————————————————————————————————————

/// Streamable HTTP 传输。
///
/// 协议要点（<https://modelcontextprotocol.io>）：
///
/// - 客户端 POST 一条 JSON-RPC 到端点；服务器用 `Mcp-Session-Id` 头把这次
///   连接认作一个"会话"，**之后的每个请求都要带上它**（少了就是 400）。
/// - 通知（`notifications/*`）服务器回 202 且无正文。
/// - 请求的正文可能是普通 JSON，也可能是 SSE 流（`text/event-stream`）——
///   两种都要能读，本仓两种都处理。
#[cfg(feature = "mcp-http")]
struct HttpTransport {
    url: String,
    /// 配置里附带的请求头（如 `Authorization`）。
    headers: Vec<(String, String)>,
    /// 会话 id：握手时从响应头取，之后每次请求都带上。
    session: Mutex<Option<String>>,
    client: reqwest::Client,
    next_id: AtomicU64,
}

#[cfg(feature = "mcp-http")]
impl HttpTransport {
    fn new(config: &ServerConfig) -> Result<Self, String> {
        let url = config.url.clone().ok_or("HTTP 传输需要配置 url")?;
        // 强制不走系统代理：本机 Clash 之类会把 127.0.0.1 也路由进代理，
        // 那样就连不上本机的 MCP 服务器（HTTP 传输多半指向本机网关）。
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| format!("创建 MCP HTTP 客户端失败：{e}"))?;
        let headers = config
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Self {
            url,
            headers,
            session: Mutex::new(None),
            client,
            next_id: AtomicU64::new(1),
        })
    }

    /// 发一条消息，返回 `(状态码, Content-Type, 正文)`。
    async fn post(
        &self,
        payload: &Value,
        timeout: Duration,
    ) -> Result<(u16, String, String), String> {
        let session = {
            self.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        };
        let mut request = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            // 两种响应形态都要接受，故两个都写进 Accept。
            .header("Accept", "application/json, text/event-stream")
            .json(payload);
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("调用 MCP HTTP 端点失败：{e}"))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // 会话 id 只在握手那次出现；之后的请求沿用它。
        if let Some(session) = response.headers().get("Mcp-Session-Id") {
            if let Ok(session) = session.to_str() {
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.to_string());
            }
        }
        let body = response
            .text()
            .await
            .map_err(|e| format!("读取 MCP 响应失败：{e}"))?;
        Ok((status, content_type, body))
    }
}

#[cfg(feature = "mcp-http")]
impl Transport for HttpTransport {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let (status, content_type, body) = block_on(self.post(&payload, timeout))?;
        if !(200..300).contains(&status) {
            // 会话过期：清掉会话 id，下一次调用重新握手。
            if status == 404 {
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            let detail: String = body.chars().take(300).collect();
            return Err(format!("MCP HTTP 端点返回 {status}：{detail}"));
        }
        // SSE 流：从事件里挑出属于本次请求的那条回应。
        if content_type.contains("text/event-stream") {
            let events = parse_sse(&body);
            return events
                .into_iter()
                .find(|event| event.get("id").and_then(|v| v.as_u64()) == Some(id))
                .map(|event| match event.get("error") {
                    Some(error) => Err(error_message(error)),
                    None => Ok(event.get("result").cloned().unwrap_or(Value::Null)),
                })
                .unwrap_or_else(|| Err(format!("MCP SSE 回应里没有 id={id} 的消息")));
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|e| format!("MCP 响应不是合法 JSON：{e}"))?;
        match value.get("error") {
            Some(error) => Err(error_message(error)),
            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    fn notify(&self, method: &str, params: Value, timeout: Duration) -> Result<(), String> {
        let payload = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let (status, _, body) = block_on(self.post(&payload, timeout))?;
        if !(200..300).contains(&status) {
            let detail: String = body.chars().take(300).collect();
            return Err(format!("MCP HTTP 端点返回 {status}：{detail}"));
        }
        Ok(())
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn shutdown(&self) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// 解析 SSE 正文，取出其中每条 `data:` 的 JSON。
#[cfg(feature = "mcp-http")]
fn parse_sse(body: &str) -> Vec<Value> {
    fn flush(data: &mut String, events: &mut Vec<Value>) {
        if data.is_empty() {
            return;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            events.push(value);
        }
        data.clear();
    }
    let mut events = Vec::new();
    let mut data = String::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            flush(&mut data, &mut events);
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    flush(&mut data, &mut events);
    events
}

/// 在全局 tokio runtime 上跑一个异步 future 并同步等结果。
///
/// 与 `llm::genai_client` 的做法一致（那里是同义实现）：调用方大多在同步的
/// 工具循环里，不能直接 await。
#[cfg(feature = "mcp-http")]
fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    crate::llm::genai_client::block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个不接任何服务器的连接：写侧落进内存缓冲，读侧没有回应者。
    fn test_conn() -> (Arc<Conn>, Arc<Mutex<Vec<u8>>>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let writer = SinkWriter(sink.clone());
        (Conn::new(Box::new(writer)), sink)
    }

    /// 写侧落到共享内存缓冲的假写入器。
    struct SinkWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SinkWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 造一个只连内存缓冲、不接真实进程的 stdio 传输。
    fn test_transport() -> (StdioTransport, Arc<Mutex<Vec<u8>>>) {
        let (conn, sink) = test_conn();
        (
            StdioTransport {
                conn,
                child: Mutex::new(None),
                stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            },
            sink,
        )
    }

    #[test]
    fn request_times_out_without_a_responder() {
        let (transport, sink) = test_transport();
        let err = transport
            .request("tools/list", json!({}), Duration::from_millis(80))
            .unwrap_err();
        assert!(err.contains("超时"), "实际错误：{err}");
        // 请求本身应按 JSON-RPC 写出去了一行：带 id、带方法名。
        let written = String::from_utf8_lossy(&sink.lock().unwrap()).to_string();
        assert!(
            written.contains("\"method\":\"tools/list\""),
            "实际写出：{written}"
        );
        assert!(written.contains("\"id\":1"), "实际写出：{written}");
    }

    #[test]
    fn timeout_releases_the_pending_slot() {
        let (transport, _sink) = test_transport();
        let _ = transport.request("tools/list", json!({}), Duration::from_millis(50));
        assert!(transport.conn.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn response_is_routed_to_the_waiting_call() {
        let (conn, _sink) = test_conn();
        let id = conn.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        conn.handle_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{"name": "echo"}]}
        }));
        let value = rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        // 派发给等待者的是 `result` 本身（协议错误另行处理）。
        assert_eq!(value["tools"][0]["name"], json!("echo"));
    }

    #[test]
    fn error_response_becomes_an_err() {
        let (conn, _sink) = test_conn();
        let id = conn.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        conn.handle_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "没有这个工具"}
        }));
        let err = rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert!(err.contains("没有这个工具"), "实际错误：{err}");
        assert!(err.contains("-32601"), "实际错误：{err}");
    }

    #[test]
    fn ping_is_answered() {
        let (conn, sink) = test_conn();
        conn.handle_message(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
        let written = String::from_utf8_lossy(&sink.lock().unwrap()).to_string();
        assert!(written.contains("\"id\":7"), "实际写出：{written}");
        assert!(written.contains("\"result\":{}"), "实际写出：{written}");
    }

    #[test]
    fn notifications_are_ignored() {
        let (conn, sink) = test_conn();
        conn.handle_message(&json!({"jsonrpc": "2.0", "method": "notifications/message",
            "params": {"level": "info"}}));
        assert!(sink.lock().unwrap().is_empty());
    }

    #[test]
    fn non_utf8_line_is_dropped_without_killing_the_connection() {
        let (conn, _sink) = test_conn();
        // 服务器写出一行非 UTF-8 字节（Windows 下按 GBK 写日志的常见情形）：
        // 丢的是这一行，连接还得能用。
        assert!(!handle_raw_line(&conn, b"\xff\xfe not utf-8\n"));
        // 坏行之后，一条正常回应照样派发到等待者。
        let id = conn.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        assert!(handle_raw_line(
            &conn,
            format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"ok\":true}}}}\n").as_bytes()
        ));
        let value = rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(value["ok"], json!(true));
    }

    #[test]
    fn non_json_line_is_dropped_without_killing_the_connection() {
        let (conn, _sink) = test_conn();
        // 服务器把调试输出混进了 stdout：这一行不算数，连接照旧。
        assert!(!handle_raw_line(&conn, b"[debug] thinking...\n"));
        assert!(conn.alive.load(Ordering::SeqCst));
    }

    #[test]
    fn blank_line_is_ignored() {
        let (conn, sink) = test_conn();
        assert!(!handle_raw_line(&conn, b"   \r\n"));
        assert!(sink.lock().unwrap().is_empty());
    }

    #[test]
    fn 错误信息带上服务器_stderr() {
        // 服务器起不来时，traceback 只在它自己的 stderr 里；不带出来就等于没信息。
        let message = with_stderr_tail(
            "MCP 服务器进程已退出".to_string(),
            &[
                "Traceback (most recent call last):".to_string(),
                "ModuleNotFoundError: No module named 'mcp.server.fastmcp'".to_string(),
            ],
        );
        assert!(message.contains("进程已退出"));
        assert!(message.contains("ModuleNotFoundError"));
    }

    #[test]
    fn 没有_stderr_时错误信息保持原样() {
        assert_eq!(
            with_stderr_tail("连不上".to_string(), &[]),
            "连不上".to_string()
        );
        assert_eq!(
            with_stderr_tail("连不上".to_string(), &["   ".to_string()]),
            "连不上".to_string()
        );
    }

    #[test]
    fn bom_prefixed_line_is_still_parsed() {
        let (conn, _sink) = test_conn();
        let id = conn.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        // Windows 上带 BOM 写 stdout 的程序，第一行常带 BOM。
        assert!(handle_raw_line(
            &conn,
            format!("\u{feff}{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"ok\":true}}}}\n")
                .as_bytes()
        ));
        let value = rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(value["ok"], json!(true));
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn sse_body_is_parsed_into_messages() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n\
                    event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["id"], json!(1));
        assert_eq!(events[0]["result"]["ok"], json!(true));
    }

    #[cfg(windows)]
    #[test]
    fn end_to_end_against_a_fake_server() {
        // 端到端：拉起一个只懂 MCP 最小子集的假服务器，走完
        // 握手 → tools/list → tools/call 全链路。
        let dir = std::env::temp_dir().join(format!("zhiwei-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-mcp.ps1");
        // 脚本文件带 UTF-8 BOM：PowerShell 5.1 读 .ps1 时，无 BOM 就按系统 ANSI
        // 代码页（中文机器上是 GBK）解码，脚本里的中文字面量会变成乱码。
        // 有 BOM 才按 UTF-8 读。
        std::fs::write(
            &script,
            format!(
                "\u{feff}{}",
                r#"
$ErrorActionPreference = 'Stop'
# MCP 的 stdio 传输规定 UTF-8。PowerShell 5.1 的 Console.In / Console.Out 在
# 管道下并不按 UTF-8 读写（实测中文会被写坏），所以这里直接操作原始字节流、
# 自己指定编码；输出用无 BOM 的 UTF8Encoding。
$utf8 = New-Object System.Text.UTF8Encoding($false)
$reader = New-Object System.IO.StreamReader -ArgumentList @([Console]::OpenStandardInput(), $utf8)
$writer = New-Object System.IO.StreamWriter -ArgumentList @([Console]::OpenStandardOutput(), $utf8)
while ($true) {
  $line = $reader.ReadLine()
  if ($null -eq $line) { break }
  $req = $line | ConvertFrom-Json
  if ($null -eq $req.id) { continue }
  if ($req.method -eq 'initialize') {
    $resp = @{ jsonrpc = '2.0'; id = $req.id; result = @{ protocolVersion = '2025-06-18'; capabilities = @{}; serverInfo = @{ name = 'fake'; version = '1' } } }
  } elseif ($req.method -eq 'tools/list') {
    $resp = @{ jsonrpc = '2.0'; id = $req.id; result = @{ tools = @(@{ name = 'echo'; description = '回显'; inputSchema = @{ type = 'object'; properties = @{ text = @{ type = 'string' } } } }) } }
  } elseif ($req.method -eq 'tools/call') {
    $resp = @{ jsonrpc = '2.0'; id = $req.id; result = @{ content = @(@{ type = 'text'; text = ('echo: ' + $req.params.arguments.text) }) } }
  } else {
    $resp = @{ jsonrpc = '2.0'; id = $req.id; result = @{} }
  }
  $writer.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
  $writer.Flush()
}
"#
            ),
        )
        .unwrap();
        let config = ServerConfig {
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script.to_string_lossy().to_string(),
            ],
            env: Default::default(),
            tools: vec![],
            disabled: false,
            timeout_secs: Some(30),
            url: None,
            headers: Default::default(),
            confirm: crate::mcp::config::ConfirmationPolicy::default(),
        };
        let client = McpClient::connect(&config).expect("假服务器应能连上");
        let tools = client.list_tools().expect("应能取到工具列表");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let result = client
            .call_tool("echo", json!({"text": "你好"}), Duration::from_secs(10))
            .expect("应能调用工具");
        assert_eq!(
            result.content,
            vec![protocol::Content::Text("echo: 你好".into())]
        );
        client.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
