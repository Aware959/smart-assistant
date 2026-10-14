use futures::StreamExt;
use genai::chat::{
    ChatMessage as GenaiChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ChatStreamEvent,
    JsonSpec, StopReason, Tool, ToolCall as GenaiToolCall, ToolChoice, ToolResponse,
};

use crate::config::Config;
use crate::error::{Result, SqlError};
use crate::llm::genai_client;

use crate::core::schema::message::ChatCall;
/// 一条发给 LLM 的对话消息（作用域本体默认 system / user / assistant）。
/// 定义在 [`crate::core::schema::message`]，此处 re-export 保持 `llm::chat::ChatMessage` 兼容。
pub use crate::core::schema::message::ChatMessage;

/// 把内核消息序列翻译成 provider 请求。
///
/// 工具轮的往返是这里新增的核心职责：
/// - assistant 带 `tool_calls` → 发成 assistant 轮的 ToolCall 部件；
/// - `role == "tool"` → 发成 ToolResponse 部件（协议要求每个调用都有配对结果，
///   漏一个就会被 provider 拒）。
fn build_chat_request(messages: &[ChatMessage], tools: &[Tool]) -> ChatRequest {
    let mut chat_req = ChatRequest::default();
    for msg in messages {
        let gmsg = match msg.role.as_str() {
            "system" => GenaiChatMessage::system(&msg.content),
            "assistant" if !msg.tool_calls.is_empty() => {
                let calls: Vec<GenaiToolCall> = msg
                    .tool_calls
                    .iter()
                    .map(|c| GenaiToolCall {
                        call_id: c.call_id.clone(),
                        fn_name: c.fn_name.clone(),
                        fn_arguments: c.fn_arguments.clone(),
                        thought_signatures: None,
                    })
                    .collect();
                GenaiChatMessage::from(calls)
            }
            "assistant" => GenaiChatMessage::assistant(&msg.content),
            "tool" => {
                let mut resp = ToolResponse::new(
                    msg.tool_call_id.clone().unwrap_or_default(),
                    msg.content.clone(),
                );
                if let Some(name) = &msg.tool_name {
                    resp = resp.with_fn_name(name.clone());
                }
                GenaiChatMessage::from(resp)
            }
            _ => GenaiChatMessage::user(&msg.content),
        };
        chat_req = chat_req.append_message(gmsg);
    }
    if !tools.is_empty() {
        chat_req = chat_req.with_tools(tools.to_vec());
    }
    chat_req
}

/// 无工具的请求（记忆提取、判定等辅助任务），保持原行为。
fn build_plain_request(messages: &[ChatMessage]) -> ChatRequest {
    build_chat_request(messages, &[])
}

/// 瞬时错误最多重试次数：初始请求 + 3 次重试。
const LLM_MAX_RETRIES: u32 = 3;
/// 重试退避基准间隔（毫秒），指数递增 `base · 2^(attempt-1)`：500 / 1000 / 2000，
/// 合计最多 ~3.5s，远在 `AI_DEADLINE_SECS`（240s）的单轮预算内。
const LLM_RETRY_BASE_DELAY_MS: u64 = 500;

/// 第 `attempt` 次重试前的退避时长（毫秒），`attempt` 从 1 起。指数退避：500/1000/2000。
fn retry_backoff_ms(attempt: u32) -> u64 {
    LLM_RETRY_BASE_DELAY_MS * 2u64.pow(attempt - 1)
}

/// 判断一个 genai 错误是否属于可重试的瞬时故障。
///
/// genai 的错误是结构化的，按 variant 走标准判定，不依赖字符串特征：
/// - `HttpError { status }` / `webc::Error::ResponseFailedStatus { status }`：直接带 HTTP
///   状态码，429 限流与 5xx 网关瞬时都可重试；
/// - `webc::Error::Reqwest(_)`：请求根本没完成（连接超时 / 重置 / DNS / 代理抖动）。
///   genai 把"传输层失败"与"成功但状态码异常"拆成了两个 variant，所以走到这里的必然
///   是传输层失败，天然可重试。
///
/// 其余（鉴权 401、参数 400、响应体解析、流解析）视为致命，立刻上抛交给上层兜底。
fn is_genai_retryable(e: &genai::Error) -> bool {
    use genai::Error as G;
    match e {
        G::HttpError { status, .. } => is_status_retryable(status.as_u16()),
        G::WebAdapterCall { webc_error, .. } | G::WebModelCall { webc_error, .. } => {
            is_webc_retryable(webc_error)
        }
        _ => false,
    }
}

/// 429 限流 + 5xx 网关瞬时可重试。
fn is_status_retryable(code: u16) -> bool {
    code == 429 || (500..=599).contains(&code)
}

/// 判定 webc 层（genai 对 reqwest 的封装）错误是否可重试。
///
/// 用 `as_u16()` 取状态码，避免直接引用 `reqwest::StatusCode` 类型——本仓的 reqwest
/// 是 0.12，而 genai 依赖的是 0.13，两个 major 的类型不互通，但状态码数值语义一致。
fn is_webc_retryable(e: &genai::webc::Error) -> bool {
    match e {
        genai::webc::Error::ResponseFailedStatus { status, .. } => {
            is_status_retryable(status.as_u16())
        }
        // 走到 Reqwest variant 的必然是传输层失败（无响应），不是业务状态码错误，
        // 故一律可重试。
        genai::webc::Error::Reqwest(_) => true,
        _ => false,
    }
}

/// 对 genai 请求做带指数退避的重试。
///
/// 仅当错误可重试（`[is_genai_retryable]`）且未达次数上限时才重试；否则原样返回错误。
/// 退避在 genai 专用 runtime 上 `sleep`，不阻塞通道轮询线程。只覆盖"首字节之前"的请求
/// 阶段失败（429 限流、`error sending request` 传输错误都发生在这一步）；中道流式错误与
/// 致命错误不在此重试。
pub(crate) async fn with_llm_retry<F, Fut, T>(mut op: F) -> std::result::Result<T, genai::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, genai::Error>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= LLM_MAX_RETRIES || !is_genai_retryable(&e) {
                    return Err(e);
                }
                attempt += 1;
                let delay_ms = retry_backoff_ms(attempt);
                tracing::warn!(
                    attempt,
                    max = LLM_MAX_RETRIES,
                    delay_ms,
                    error = %e,
                    "LLM 请求瞬时失败，准备重试"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// 调用远程 LLM 完成非流式对话，返回回复文本。
pub fn complete(messages: &[ChatMessage]) -> Result<String> {
    complete_with_model(messages, &crate::config::Config::get().llm_model)
}

/// 用指定模型完成非流式对话（记忆提取、判定等任务可与对话模型分离）。
pub fn complete_with_model(messages: &[ChatMessage], model: &str) -> Result<String> {
    complete_impl(messages, model, None)
}

/// 按 JSON 约束强制结构化输出的非流式对话（记忆提取/判定等任务）。
///
/// 输出模式由 `LLM_STRUCTURED_OUTPUT` 控制：
/// - `json_schema`（默认）：`response_format: {type: json_schema, json_schema: {name, strict, schema}}`。
///   支持端：OpenAI / Gemini / 本地 llama.cpp 系 / Groq 的 Qwen，能同时压掉思考段。
/// - `json_object`：`response_format: {type: json_object}`，仅提示工程约束（提示词里
///   必须要求"只输出 JSON"）。DeepSeek 等端点不支持 json_schema 但接受 json_object。
pub fn complete_structured(
    messages: &[ChatMessage],
    model: &str,
    name: impl Into<String>,
    schema: serde_json::Value,
) -> Result<String> {
    let structured = Config::get().llm_structured_output.to_ascii_lowercase();
    let options = if structured == "json_object" {
        ChatOptions {
            response_format: Some(ChatResponseFormat::JsonMode),
            ..Default::default()
        }
    } else {
        ChatOptions {
            response_format: Some(ChatResponseFormat::JsonSpec(JsonSpec::new(name, schema))),
            ..Default::default()
        }
    };
    complete_impl(messages, model, Some(&options))
}

fn complete_impl(
    messages: &[ChatMessage],
    model: &str,
    options: Option<&ChatOptions>,
) -> Result<String> {
    let started = std::time::Instant::now();

    let chat_res = genai_client::block_on(async move {
        let client = genai_client::chat_client();
        // 限流 429 / 网络抖动等瞬时错误退避重试，避免直接失败触发通道兜底话术。
        with_llm_retry(|| {
            let req = build_plain_request(messages);
            client.exec_chat(model, req, options)
        })
        .await
    })?;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(model, elapsed_ms, "LLM 非流式：完成");

    chat_res
        .first_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| SqlError::Config("LLM 返回内容为空".to_string()))
}

/// 流式生成的最大 token 数：**异常熔断，不是长度塑形**。
///
/// 正常一轮远小于这个量；一旦触发说明这次生成已经跑偏（长篇大论 / 复读），
/// 砍掉重来的代价远小于把一段失控文本发给用户。注意部分推理模型的思考 token
/// 也计入此额度，故取值留足余量。
///
/// 实测 `DeepSeek-V4.1-Flash`：日常陪伴回复 82~192 tokens，但模型一旦对
/// 「讲个长故事」这类指令陷入长思考，实测撞到 901/1024——思考挤掉了正文的
/// 额度。抬到 2048 留出思考余量，真正的熔断交给 `max_iter` 而不是这里的 token 墙。
const STREAM_MAX_TOKENS: u32 = 2048;

/// 一轮流式生成的结果。
#[derive(Debug, Default, Clone)]
pub struct StreamOutcome {
    /// 归一化后的正文。输出工具路线下通常为空——正文走 `send_message` 的参数。
    pub text: String,
    /// 本轮模型发出的工具调用（已由 genai 累积并解析成完整 JSON 参数）。
    pub tool_calls: Vec<ChatCall>,
    /// provider 归一化后的停止原因；本地框架常不报，缺失时以 `tool_calls` 为准。
    pub stop_reason: Option<StopReason>,
    /// 思考内容长度，仅用于日志与预算观测，从不外发。
    pub reasoning_chars: usize,
}

impl StreamOutcome {
    /// 本轮是否有工具调用。这是循环的唯一判据。
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// 带工具的一轮流式对话。
///
/// 与 [`complete_stream`] 的差别是返回值和工具支持：正文走 `send_message`
/// 的工具参数，`on_delta` 只在模型**绕过工具直接写正文**时才会被调用；而调用方
/// （见 [`crate::core::agent`] 的 `run_tool_loop`）按约定丢弃这类正文，故不会外发。
///
/// `tool_choice` 是 provider 原生工具选择（`None` = 默认 auto）。带工具的对话里它是
/// 治本手段：实测本仓的 `DeepSeek-V4.1-Flash` 在"必须回一条"的语境下**经常不调工具、
/// 直接把回话写进 content**（同一上下文 auto 下 0/3 命中，`Required` 下 3/3 命中）。
/// 提示词是软约束，挡不住这种行为；靠 `tool_choice = Required` 在协议层兜住——调用方
/// 只在"还没产出任何消息"时这样传，一旦说上话就放开，避免模型无法收尾。
///
/// 工具调用必须依赖 `End` 事件里的 `captured_tool_calls()`：流式过程中
/// `ToolCallChunk.fn_arguments` 是**逐片到达的半截 JSON 字符串**，直接解析必然失败；
/// genai 在 `End` 才按 index 累积并反序列化（见 openai streamer）。
pub fn complete_stream_with_tools<F>(
    messages: &[ChatMessage],
    tools: &[Tool],
    tool_choice: Option<ToolChoice>,
    mut on_delta: F,
) -> Result<StreamOutcome>
where
    F: FnMut(&str) + Send,
{
    let model = crate::config::Config::get().llm_model.clone();
    let options = ChatOptions {
        capture_content: Some(true),
        capture_tool_calls: Some(true),
        capture_reasoning_content: Some(true),
        max_tokens: Some(STREAM_MAX_TOKENS),
        tool_choice,
        ..Default::default()
    };

    Ok(genai_client::block_on(async move {
        let client = genai_client::chat_client();
        // 请求阶段与消费阶段共用同一重试预算（最多 LLM_MAX_RETRIES 次）：
        // - 首字节之前：429 限流 / 网络抖动 / 5xx 网关瞬时；
        // - 首字节之后：连接中途断开、流解析失败等。
        // on_delta 只在整轮成功（End 事件）后才可能下发用户内容，故中途失败重试
        // 不会重复下发；工具循环路径下 on_delta 本就是空实现，同样安全。
        let mut attempt: u32 = 0;
        loop {
            // —— 请求阶段：拿不到首字节即失败，可重试则退避后整轮重来。
            let chat_res = match client
                .exec_chat_stream(&model, build_chat_request(messages, tools), Some(&options))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= LLM_MAX_RETRIES || !is_genai_retryable(&e) {
                        return Err(e);
                    }
                    attempt += 1;
                    let delay_ms = retry_backoff_ms(attempt);
                    tracing::warn!(
                        attempt,
                        max = LLM_MAX_RETRIES,
                        delay_ms,
                        error = %e,
                        "LLM 请求瞬时失败，准备重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
            };

            // —— 消费阶段：首字节之后的处理。
            let started = std::time::Instant::now();
            let mut flat = crate::utils::textflow::ParagraphFlattener::new();
            let mut collapsed = 0usize;
            let mut reasoning_chars = 0usize;
            let mut tool_chunks = 0usize;
            let mut stop_reason = None;
            let mut tool_calls: Vec<ChatCall> = Vec::new();
            let mut stream = chat_res.stream;
            let mut stream_err: Option<genai::Error> = None;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ChatStreamEvent::Chunk(chunk)) => {
                        if flat.is_empty() {
                            let first_ms = started.elapsed().as_millis() as u64;
                            tracing::info!(model = %model, first_ms, "LLM 流式：首字节到达");
                        }
                        // 原文可能带空白而产出片段不带，差值即被归一化掉的字符数。
                        let before = chunk.content.len();
                        let piece = flat.push(&chunk.content);
                        collapsed += before - piece.len();
                        // 这里**不**回调：直接正文只在"整轮无工具调用"时才算数，
                        // 而这件事要等 End 才确定。先下发给用户再发现模型同时调了
                        // send_message，就会多发一段凭空正文（实测 content 与 tool_calls
                        // 同轮出现过）。所以一律先攒着，由下方收尾按条件统一下发。
                    }
                    Ok(ChatStreamEvent::ReasoningChunk(chunk)) => {
                        reasoning_chars += chunk.content.chars().count();
                    }
                    Ok(ChatStreamEvent::ToolCallChunk(_)) => tool_chunks += 1,
                    Ok(ChatStreamEvent::End(end)) => {
                        // capture_content=true 时这里给出权威的完整拼接文本，兜底对齐。
                        // 只纠正最终入库文本，不重复下发增量（用户看到的流不回滚）。
                        if let Some(texts) = end.captured_texts() {
                            flat.overwrite(&texts.concat());
                        }
                        stop_reason = end.captured_stop_reason.clone();
                        tool_calls = end
                            .captured_tool_calls()
                            .map(|calls| {
                                calls
                                    .into_iter()
                                    .map(|c| {
                                        ChatCall::new(
                                            c.call_id.clone(),
                                            c.fn_name.clone(),
                                            c.fn_arguments.clone(),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }

            match stream_err {
                Some(e) => {
                    // 中道流式错误：传输层断开（WebStream）可重试；流解析等致命错误不重试。
                    if attempt >= LLM_MAX_RETRIES || !is_genai_retryable(&e) {
                        return Err(e);
                    }
                    attempt += 1;
                    let delay_ms = retry_backoff_ms(attempt);
                    tracing::warn!(
                        attempt,
                        max = LLM_MAX_RETRIES,
                        delay_ms,
                        error = %e,
                        "LLM 流式中途失败，准备整轮重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                None => {
                    let total_ms = started.elapsed().as_millis() as u64;
                    tracing::info!(
                        model = %model,
                        total_ms,
                        collapsed,
                        tool_chunks,
                        tool_calls = tool_calls.len(),
                        reasoning_chars,
                        ?stop_reason,
                        "LLM 流式：结束"
                    );

                    let text = flat.finish().to_string();
                    // 收尾裁定直接正文：只在整轮无工具调用时才交给 `on_delta`。
                    //
                    // 有工具调用时正文是模型的"自言自语"，不是给用户看的话（探针里
                    // content 与 send_message 同轮出现时，content 是前言铺垫、正文在参数里），
                    // 交给调用方只会让用户收到一段与工具消息重复的突兀开场。调用方
                    // （`run_tool_loop`）另有约定：即便无工具调用，也丢弃正文。
                    let dropped_text = !tool_calls.is_empty() && !text.trim().is_empty();
                    if dropped_text {
                        tracing::warn!(
                            chars = text.chars().count(),
                            tool_calls = tool_calls.len(),
                            "模型同轮既有直接正文又有工具调用，按约定丢弃直接正文"
                        );
                    } else if !text.is_empty() {
                        on_delta(&text);
                    }

                    return Ok(StreamOutcome {
                        text,
                        tool_calls,
                        stop_reason,
                        reasoning_chars,
                    });
                }
            }
        }
    })?)
}

/// 无状态的 LLM 适配器：把对话 / 结构化输出 / 流式回复接到本模块的实现函数上。
#[derive(Default)]
pub struct Llm;

impl Llm {
    /// 非流式对话，返回回复文本。
    pub fn complete(&self, messages: &[ChatMessage]) -> Result<String> {
        crate::llm::chat::complete(messages)
    }

    /// 强制结构化输出（`json_schema` 或 `json_object`，由配置决定）。
    pub fn complete_structured(
        &self,
        messages: &[ChatMessage],
        model: &str,
        name: &str,
        schema: serde_json::Value,
    ) -> Result<String> {
        crate::llm::chat::complete_structured(messages, model, name, schema)
    }

    /// 流式对话（无工具），返回归一化后的完整正文。
    pub fn complete_stream(
        &self,
        messages: &[ChatMessage],
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String> {
        let outcome = crate::llm::chat::complete_stream_with_tools(messages, &[], None, on_delta)?;
        if outcome.text.is_empty() {
            return Err(SqlError::Config("LLM 流式返回内容为空".to_string()));
        }
        Ok(outcome.text)
    }

    /// 带工具的流式对话，返回正文与本轮工具调用。`tool_choice` 见
    /// [`crate::llm::chat::complete_stream_with_tools`]。
    pub fn complete_stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[Tool],
        tool_choice: Option<ToolChoice>,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamOutcome> {
        crate::llm::chat::complete_stream_with_tools(messages, tools, tool_choice, on_delta)
    }
}
