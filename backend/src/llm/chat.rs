use futures::StreamExt;
use genai::chat::{
    ChatMessage as GenaiChatMessage, ChatOptions, ChatRequest, ChatStreamEvent,
};
use serde::{Deserialize, Serialize};

use crate::genai_client;
use crate::error::{Result, SqlError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

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
    let chat_req = build_chat_request(messages);
    let model = crate::config::Config::get().llm_model.clone();

    let chat_res = genai_client::block_on(async move {
        let client = genai_client::chat_client();
        client.exec_chat(&model, chat_req, None).await
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