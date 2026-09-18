//! genai 适配层：为 LLM chat 与 embedding 各自构建一个共享 Client，
//! 通过全局 tokio runtime 以同步接口暴露（端侧无需感知 async）。

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use genai::adapter::AdapterKind;
use genai::chat::ChatOptions;
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget, WebConfig};
use tokio::runtime::Runtime;

use crate::config::{Config, LlmProvider};

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

/// 建立连接的硬上限：连不上就尽快失败，让上层降级，
/// 避免 Telegram 长轮询被半开的 TCP 连接永久挂起。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次 chat 调用的总超时（流式长回复需要留足余量）。
const CHAT_TIMEOUT: Duration = Duration::from_secs(300);
/// 单次 embedding 调用的总超时。
const EMBED_TIMEOUT: Duration = Duration::from_secs(60);

/// 带超时的 WebConfig：`total` 到期即报错返回，而不是无限阻塞。
fn web_config(total: Duration) -> WebConfig {
    WebConfig::default()
        .with_connect_timeout(CONNECT_TIMEOUT)
        .with_timeout(total)
}

/// LLM chat 专用的 genai Client。
///
/// 客户端默认开启 `normalize_reasoning_content`：本地模型常把推理过程写进
/// content 的 ` thinking...response` 标签块，genai 会剥掉并归入
/// `reasoning_content`，正文只保留真正的回答。
/// 注意：该归一化作用于非流式 `exec_chat`；流式 `exec_chat_stream` 只分离
/// provider 返回在 `delta.reasoning_content` 里的思考（LLM 内联在 content 里
/// 的标签块不会在流式路径被剥离，需保留给上层或依赖 provider 行为）。
pub(crate) fn chat_client() -> &'static Client {
    let chat_options = ChatOptions {
        normalize_reasoning_content: Some(true),
        ..Default::default()
    };
    CHAT_CLIENT.get_or_init(|| {
        let cfg = Config::get();
        match cfg.llm_provider {
            LlmProvider::OpenAI => build_client(
                &cfg.llm_api_url,
                &cfg.llm_api_key,
                Some(&chat_options),
                CHAT_TIMEOUT,
            ),
            LlmProvider::Gemini => build_gemini_client(
                &cfg.llm_api_url,
                &cfg.llm_api_key,
                Some(&chat_options),
                CHAT_TIMEOUT,
            ),
        }
    })
}

/// embedding 专用的 genai Client（genez 的嵌入与 LLM 通道独立，仍按 OpenAI 兼容端点）。
pub(crate) fn embed_client() -> &'static Client {
    EMBED_CLIENT.get_or_init(|| {
        build_client(
            &Config::get().embedding_api_url,
            &Config::get().embedding_api_key,
            None,
            EMBED_TIMEOUT,
        )
    })
}

/// 构建绑定到 Gemini（Google AI Studio）原生协议的 Client。
///
/// - `api_url`  非空时覆盖端点（如走代理网关），为空用 genai 默认
///   `https://generativelanguage.googleapis.com/v1beta/`。
/// - `api_key`  非空时直接使用；为空回退环境变量 `GEMINI_API_KEY`。
/// - 鉴权头为 `x-goog-api-key`（GeminiAdapter 约定），不走 `Authorization: Bearer`。
fn build_gemini_client(
    api_url: &str,
    api_key: &str,
    chat_options: Option<&ChatOptions>,
    total: Duration,
) -> Client {
    let url = resolve_url(api_url).map(ToOwned::to_owned);
    let auth = if api_key.is_empty() {
        AuthData::from_env("GEMINI_API_KEY")
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
            let model = ModelIden::new(AdapterKind::Gemini, model.model_name);
            Ok(ServiceTarget {
                endpoint,
                auth: auth.clone(),
                model,
            })
        },
    );

    let mut builder = Client::builder()
        .with_service_target_resolver(service_target_resolver)
        .with_web_config(web_config(total));
    if let Some(options) = chat_options {
        builder = builder.with_chat_options(options.clone());
    }
    builder.build()
}

/// 构建绑定到 OpenAI 兼容端点的 Client。
///
/// - `api_url`  非空时作为 Endpoint；为空回退到 genai 默认（OpenAI 官方）。
/// - `api_key`  非空时直接使用；为空时：自定义端点 → `AuthData::None`（本地无鉴权），
///   官方端点 → 回退读取环境变量 `OPENAI_API_KEY`。
/// - `chat_options` 非空时设为客户端默认行为（作用于该 client 的全部 chat 请求）。
fn build_client(
    api_url: &str,
    api_key: &str,
    chat_options: Option<&ChatOptions>,
    total: Duration,
) -> Client {
    let url = resolve_url(api_url).map(ToOwned::to_owned);
    let auth = if api_key.is_empty() {
        if url.is_some() {
            // 自建/本地 OpenAI 兼容端点（LM Studio、llama.cpp 等）不校验 token。
            // 用占位 key 绕开 genai 对 OPENAI_API_KEY 的强制要求；请求会带上
            // `Authorization: Bearer none`，本地服务会忽略它。
            AuthData::from_single("none")
        } else {
            AuthData::from_env("OPENAI_API_KEY")
        }
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

    let mut builder = Client::builder()
        .with_service_target_resolver(service_target_resolver)
        .with_web_config(web_config(total));
    if let Some(options) = chat_options {
        builder = builder.with_chat_options(options.clone());
    }
    builder.build()
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