use futures::StreamExt;
use genai::chat::{
    ChatMessage as GenaiChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ChatStreamEvent,
    JsonSpec,
};

use crate::config::Config;
use crate::error::{Result, SqlError};
use crate::llm::genai_client;

/// 一条发给 LLM 的对话消息（作用域本体默认 system / user / assistant）。
/// 定义在 [`crate::core::schema::message`]，此处 re-export 保持 `llm::chat::ChatMessage` 兼容。
pub use crate::core::schema::message::ChatMessage;
fn build_chat_request(messages: &[ChatMessage]) -> ChatRequest {
    let mut chat_req = ChatRequest::default();
    for msg in messages {
        let gmsg = match msg.role.as_str() {
            "system" => GenaiChatMessage::system(&msg.content),
            "assistant" => GenaiChatMessage::assistant(&msg.content),
            _ => GenaiChatMessage::user(&msg.content),
        };
        chat_req = chat_req.append_message(gmsg);
    }
    chat_req
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
    let chat_req = build_chat_request(messages);
    let started = std::time::Instant::now();

    let chat_res = genai_client::block_on(async move {
        let client = genai_client::chat_client();
        client.exec_chat(model, chat_req, options).await
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
/// 正常对话输出远小于这个量；一旦触发说明这次生成已经跑偏（长篇大论 / 复读），
/// 砍掉重来的代价远小于把一段失控文本发给用户。注意部分推理模型的思考 token
/// 也计入此额度，故取值留足余量。输出形状（长度 / 换行）由提示词示例负责，
/// 这里只兜底极端情况，不参与日常塑形。
const STREAM_MAX_TOKENS: u32 = 1024;

/// 流式调用远程 LLM 对话：每个文本增量在生成时立即回调 `on_delta`，返回完整文本。
///
/// 回调在阻塞生成线程上同步执行，适合把增量转发到 channel / socket / FFI。
///
/// **增量是归一化过的**：模型偶发的 `\n\n` 在下发前就被收敛（见
/// [`crate::utils::textflow`]）。必须在这里做而不是事后改写完整文本——`on_delta`
/// 是逐段下发的，等拿到全文再改，用户早就看到带空行的原文了。提示词负责降低
/// 出现概率，这里负责把漏网的变成确定结果，两者叠加。
pub fn complete_stream<F>(messages: &[ChatMessage], mut on_delta: F) -> Result<String>
where
    F: FnMut(&str) + Send,
{
    let chat_req = build_chat_request(messages);
    let model = crate::config::Config::get().llm_model.clone();
    let options = ChatOptions {
        capture_content: Some(true),
        max_tokens: Some(STREAM_MAX_TOKENS),
        ..Default::default()
    };

    genai_client::block_on(async move {
        let chat_res = genai_client::chat_client()
            .exec_chat_stream(&model, chat_req, Some(&options))
            .await?;
        let mut stream = chat_res.stream;

        // 首字节之前的等待集中了几个最典型的卡点：LLM 服务端重新加载模型、
        // TCP 半开连接、上游排队。记到 info，任何反常都能在日志时间轴上看出。
        let started = std::time::Instant::now();
        let mut flat = crate::utils::textflow::ParagraphFlattener::new();
        let mut collapsed = 0usize;
        while let Some(event) = stream.next().await {
            match event? {
                ChatStreamEvent::Chunk(chunk) => {
                    if flat.is_empty() {
                        let first_ms = started.elapsed().as_millis() as u64;
                        tracing::info!(model = %model, first_ms, "LLM 流式：首字节到达");
                    }
                    // 原文可能带空白而产出片段不带，差值即被归一化掉的字符数。
                    let before = chunk.content.len();
                    let piece = flat.push(&chunk.content);
                    collapsed += before - piece.len();
                    if !piece.is_empty() {
                        on_delta(&piece);
                    }
                }
                ChatStreamEvent::End(end) => {
                    // capture_content=true 时这里给出权威的完整拼接文本，兜底对齐。
                    // 只纠正最终入库文本，不重复下发增量（用户看到的流不回滚）。
                    if let Some(texts) = end.captured_texts() {
                        flat.overwrite(&texts.concat());
                    }
                    break;
                }
                _ => {}
            }
        }
        let total_ms = started.elapsed().as_millis() as u64;
        if collapsed > 0 {
            tracing::info!(
                model = %model,
                total_ms,
                collapsed,
                "LLM 流式：结束（已归一化空白）"
            );
        } else {
            tracing::info!(model = %model, total_ms, "LLM 流式：结束");
        }

        let full = flat.finish();
        if full.is_empty() {
            return Err(SqlError::Config("LLM 流式返回内容为空".to_string()));
        }
        Ok(full.to_string())
    })
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

    /// 流式对话：每个文本增量在生成时同步回调 `on_delta`，返回完整文本。
    pub fn complete_stream(
        &self,
        messages: &[ChatMessage],
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String> {
        crate::llm::chat::complete_stream(messages, on_delta)
    }
}
