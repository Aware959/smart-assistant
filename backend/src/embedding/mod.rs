use crate::config::Config;
use crate::error::{Result, SqlError};
use crate::genai_client;

/// 对一段文本进行向量化。
pub fn embed_text(text: &str) -> Result<Vec<f32>> {
    let model = Config::get().embedding_model.clone();

    let res = genai_client::block_on(async move {
        let client = genai_client::embed_client();
        client.embed(&model, text, None).await
    })?;

    res.first_embedding()
        .map(|e| e.vector().clone())
        .ok_or_else(|| SqlError::Config("embedding 返回为空".to_string()))
}

/// 对多段文本进行向量化。
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let model = Config::get().embedding_model.clone();

    let res = genai_client::block_on(async move {
        let client = genai_client::embed_client();
        client.embed_batch(&model, texts.to_vec(), None).await
    })?;

    Ok(res.embeddings.into_iter().map(|e| e.into_vector()).collect())
}