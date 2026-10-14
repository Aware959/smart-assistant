//! 文本向量化：经 genai embedding Client 调用外部能力，供 `core::memory` 使用。

use crate::config::Config;
use crate::error::{Result, SqlError};

use super::chat::with_llm_retry;
use super::genai_client;

/// 对一段文本进行向量化。
pub fn embed_text(text: &str) -> Result<Vec<f32>> {
    let model = Config::get().embedding_model.clone();
    let closure_model = model.clone();
    let started = std::time::Instant::now();

    let res = genai_client::block_on(async move {
        let client = genai_client::embed_client();
        // 限流 429 / 网络抖动 / 5xx 等瞬时错误退避重试，避免记忆写入/检索因瞬时错误失败。
        with_llm_retry(|| client.embed(&closure_model, text, None)).await
    })?;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(model = %model, elapsed_ms, "embedding 完成");

    res.first_embedding()
        .map(|e| e.vector().clone())
        .ok_or_else(|| SqlError::Config("embedding 返回为空".to_string()))
}

/// 对多段文本进行向量化。
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let model = Config::get().embedding_model.clone();

    let res = genai_client::block_on(async move {
        let client = genai_client::embed_client();
        // 限流 429 / 网络抖动 / 5xx 等瞬时错误退避重试，避免记忆写入/检索因瞬时错误失败。
        with_llm_retry(|| client.embed_batch(&model, texts.to_vec(), None)).await
    })?;

    Ok(res
        .embeddings
        .into_iter()
        .map(|e| e.into_vector())
        .collect())
}
