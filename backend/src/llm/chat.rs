use futures::StreamExt;
use genai::chat::{
    ChatMessage as GenaiChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ChatStreamEvent,
    JsonSpec,
};

use crate::config::Config;
use crate::core::ports::ChatLlm;
use crate::error::{Result, SqlError};
use crate::llm::genai_client;

/// 一条发给 LLM 的对话消息（作用域本体默认 system / user / assistant）。
/// 定义在 [`crate::core::types`]，此处 re-export 保持 `llm::chat::ChatMessage` 兼容。
pub use crate::core::types::ChatMessage;

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

    let chat_res = genai_client::block_on(async move {
        let client = genai_client::chat_client();
        client.exec_chat(model, chat_req, options).await
    })?;

    chat_res
        .first_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| SqlError::Config("LLM 返回内容为空".to_string()))
}

/// 流式调用远程 LLM 对话：每个文本增量在生成时立即回调 `on_delta`，返回完整文本。
///
/// 回调在阻塞生成线程上同步执行，适合把增量转发到 channel / socket / FFI。
pub fn complete_stream<F>(messages: &[ChatMessage], mut on_delta: F) -> Result<String>
where
    F: FnMut(&str) + Send,
{
    let chat_req = build_chat_request(messages);
    let model = crate::config::Config::get().llm_model.clone();
    let options = ChatOptions {
        capture_content: Some(true),
        ..Default::default()
    };

    genai_client::block_on(async move {
        let chat_res = genai_client::chat_client()
            .exec_chat_stream(&model, chat_req, Some(&options))
            .await?;

        let mut stream = chat_res.stream;
        let mut full = String::new();
        while let Some(event) = stream.next().await {
            match event? {
                ChatStreamEvent::Chunk(chunk) => {
                    full.push_str(&chunk.content);
                    on_delta(&chunk.content);
                }
                ChatStreamEvent::End(end) => {
                    // capture_content=true 时这里给出权威的完整拼接文本，兜底对齐。
                    if let Some(texts) = end.captured_texts() {
                        full = texts.concat();
                    }
                    break;
                }
                _ => {}
            }
        }

        if full.is_empty() {
            return Err(SqlError::Config("LLM 流式返回内容为空".to_string()));
        }
        Ok(full)
    })
}

/// 无状态的 LLM 适配器：把 [`ChatLlm`] 端口接到本模块的实现函数上。
pub struct Llm;

impl ChatLlm for Llm {
    fn complete(&self, messages: &[ChatMessage]) -> Result<String> {
        crate::llm::chat::complete(messages)
    }

    fn complete_structured(
        &self,
        messages: &[ChatMessage],
        model: &str,
        name: &str,
        schema: serde_json::Value,
    ) -> Result<String> {
        crate::llm::chat::complete_structured(messages, model, name, schema)
    }

    fn complete_stream(
        &self,
        messages: &[ChatMessage],
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String> {
        crate::llm::chat::complete_stream(messages, on_delta)
    }
}