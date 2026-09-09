//! genai 适配层：为 LLM chat 与 embedding 各自构建一个共享 Client，
//! 通过全局 tokio runtime 以同步接口暴露（端侧无需感知 async）。

use std::future::Future;
use std::sync::OnceLock;

use genai::adapter::AdapterKind;
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};
use tokio::runtime::Runtime;

use crate::config::Config;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static CHAT_CLIENT: OnceLock<Client> = OnceLock::new();
static EMBED_CLIENT: OnceLock<Client> = OnceLock::new();

/// 在专用线程上以同步方式执行 future，返回其输出。
///
/// 不能在调用方线程直接 `Runtime::block_on`：axum 的 worker 线程已处于
/// tokio runtime 上下文，嵌套启动会 panic（"Cannot start a runtime from within
/// a runtime"）；JNI / FFI 线程则没有 runtime。统一挪到一次性的 scoped 线程
/// 上执行，两种情况都安全，且复用同一个全局 runtime（不重复创建线程池）。
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| runtime().block_on(future))
            .join()
            .expect("genai I/O thread panicked")
    })
}

/// 全局 tokio runtime，仅由 `block_on` 后的专用线程使用。
fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Runtime::new().expect("failed to create tokio runtime for genai adapter")
    })
}

/// LLM chat 专用的 genai Client。
pub(crate) fn chat_client() -> &'static Client {
    CHAT_CLIENT.get_or_init(|| build_client(&Config::get().llm_api_url, &Config::get().llm_api_key))
}

/// embedding 专用的 genai Client。
pub(crate) fn embed_client() -> &'static Client {
    EMBED_CLIENT
        .get_or_init(|| build_client(&Config::get().embedding_api_url, &Config::get().embedding_api_key))
}

/// 构建绑定到 OpenAI 兼容端点的 Client。
///
/// - `api_url`  非空时作为 Endpoint；为空回退到 genai 默认（OpenAI 官方）。
/// - `api_key`  非空时直接使用；为空回退到环境变量 `OPENAI_API_KEY`。
fn build_client(api_url: &str, api_key: &str) -> Client {
    let url = resolve_url(api_url).map(ToOwned::to_owned);
    let auth = if api_key.is_empty() {
        AuthData::from_env("OPENAI_API_KEY")
    } else {
        AuthData::from_single(api_key)
    };

    let service_target_resolver = ServiceTargetResolver::from_resolver_fn(
        move |st: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
            let ServiceTarget { model, endpoint, .. } = st;
            let endpoint = match &url {
                Some(url) => Endpoint::from_owned(url.clone()),
                None => endpoint,
            };
            let model = ModelIden::new(AdapterKind::OpenAI, model.model_name);
            Ok(ServiceTarget {
                endpoint,
                auth: auth.clone(),
                model,
            })
        },
    );

    Client::builder()
        .with_service_target_resolver(service_target_resolver)
        .build()
}

/// URL 为空字符串或等于已知默认值时，返回 None（让 genai 用其内置默认端点）。
fn resolve_url(api_url: &str) -> Option<&str> {
    const DEFAULTS: &[&str] = &[
        "https://api.openai.com/v1/chat/completions",
        "https://api.openai.com/v1/embeddings",
    ];
    let trimmed = api_url.trim();
    if trimmed.is_empty() || DEFAULTS.contains(&trimmed) {
        None
    } else {
        Some(trimmed)
    }
}