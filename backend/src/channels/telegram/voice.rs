//! Telegram voice 子模块。

use super::*;

/// 语音转写结果落库时加的前缀：让"这条源自语音"在时间线上可辨（FR-009）。
///
/// **只进 `messages.content`**（供模型与记忆使用），**绝不回帖给用户**——用户不该看到
/// 转写文字（FR-010）。
pub(super) const VOICE_PREFIX: &str = "[语音] ";

/// 过期语音的存档占位。只入库、不会被回复，所以它不需要是"给模型看的情境描述"。
pub(super) const EXPIRED_VOICE_PLACEHOLDER: &str = "[对方发来一条语音（已过期）]";

/// 没听出内容时的落库标记。
///
/// ⚠️ 它是**给模型看的情境描述**，不是"回给用户的话术"——用户看到的那句由模型现场生成
/// （FR-005）。仓库里 MUST NOT 出现任何固定回复文案。
pub(super) const VOICE_UNINTELLIGIBLE_MARKER: &str = "[对方发来一条语音，但没能听清内容]";

/// 音频超出处理上限（时长 / 体积）时的落库标记。同样是给模型看的情境描述。
///
/// 与"没听清"分开，是因为这是**我们能确定的原因**：不是识别不了，是它太大了——模型据此
/// 可以选择合适的回应方式（FR-003/005）。
pub(super) const VOICE_TOO_LARGE_MARKER: &str = "[对方发来一条语音，但内容太大没能听清]";

/// Telegram Bot API `getFile` 的下载硬上限（20 MB）：超过它连下载都做不到，
/// 因此必须在**发起下载之前**拦下（见 [`voice_plan`]）。
pub(super) const MAX_VOICE_BYTES: i64 = 20 * 1024 * 1024;
#[derive(Debug, Deserialize)]
pub(super) struct TgFile {
    #[serde(default)]
    file_path: Option<String>,
}

/// 取一条语音/音频的原始字节 → 送给本机网关转写，返回 `(状态码, 响应体)`。
///
/// 状态码语义由 [`voice_outcome`] 解释。`Err` 只表示**下载或连接这一步就没走通**
/// （网络、超时、Telegram 拒绝、网关不在）——即系统故障路径，调用方据此直接放弃本轮。
///
/// 两步都是必需的：Telegram 的文件下载与 API 调用不在同一个前缀下
/// （`/bot<token>/…` 对 `/file/bot<token>/…`）。
///
/// ⚠️ 网关 client 必须显式 `.no_proxy()`：本机 Clash 系统代理会把 `127.0.0.1` 也路由进
/// 代理，否则这里会连不上网关（理由同 [`media_generate`]）。
pub(super) async fn transcribe_voice(
    tg_client: &reqwest::Client,
    token: &str,
    gateway_url: &str,
    timeout: u64,
    file_id: &str,
    mime_type: &str,
) -> Result<(u16, String), String> {
    // 1) getFile：拿到服务端的文件路径。file_id 是 Bot API 给的 URL-safe 串，直接拼即可。
    let url = format!("{}/getFile?file_id={file_id}", api_base(token));
    let file: ApiResponse<TgFile> = api_get(tg_client, &url).await?;
    if !file.ok {
        return Err(format!(
            "getFile 失败：{}",
            file.description.unwrap_or_default()
        ));
    }
    let path = file
        .result
        .and_then(|f| f.file_path)
        .ok_or_else(|| "getFile 未返回 file_path".to_string())?;

    // 2) 下载原始字节（注意是 `/file/bot<token>/` 前缀，不是 `/bot<token>/`）。
    let audio = tg_client
        .get(format!("https://api.telegram.org/file/bot{token}/{path}"))
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .map_err(|e| format!("下载语音失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("下载语音失败：{e}"))?
        .bytes()
        .await
        .map_err(|e| format!("读取语音字节失败：{e}"))?
        .to_vec();

    // 3) 送网关：请求体是**原始音频字节**，不是 JSON、不是 multipart、不是 base64。
    //    网关内部用 PyAV 直吃 OGG/OPUS，不必在这里做任何转码。
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| format!("创建转写客户端失败: {e}"))?;
    let resp = client
        .post(format!("{gateway_url}/transcribe"))
        .header(reqwest::header::CONTENT_TYPE, mime_type)
        .body(audio)
        .send()
        .await
        .map_err(|e| format!("调用转写端点失败：{e}"))?;
    let status = resp.status().as_u16();
    // 非 2xx 也要把响应体读出来——错误详情在里面，是排错与分流的依据。
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}
/// 转写结果的分流。**判据是 HTTP 状态码**（契约见
/// `specs/001-telegram-voice-transcription/contracts/transcribe-api.md`）。
///
/// 分界线一句话：4xx 是"这条音频的问题"（交给模型用人的方式回应），
/// 5xx / 连接失败 / 超时是"我们自己的问题"（是缺陷，不许伪装成"没听清"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VoiceOutcome {
    /// 转写成功。`text` 已去首尾空白且非空。
    Transcribed { text: String },
    /// 音频里没听出内容（含整条无人声）。交模型自然回应（FR-004/005）。
    Unintelligible,
    /// 这条音频本身的问题：解不开、太长、不是音频。同样交模型自然回应（FR-003/005）。
    Rejected { reason: String },
    /// 我们自己的问题：能力未启用 / 连不上 / 超时 / 网关 5xx。
    /// **不落库、不起轮次**，只记错误（FR-006/013）。
    Skipped { reason: String },
}

/// 从 `/transcribe` 的 200 响应体里取 `text`。形状不对按系统故障处理（契约被违反了）。
pub(super) fn transcribe_text(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    value
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| "响应缺少 text 字段".to_string())
}

/// 从 4xx/5xx 响应体里取 `error` 文本（取不到就退化成原文），只用于日志。
pub(super) fn api_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string())
}

/// "什么都没听出来"的判据：空白、或只剩标点/符号。
///
/// ⚠️ **不**把简短应答（"嗯"、"好"）算进来——那是正常转写，该交给模型理解，而不是
/// 被当成"没听清"（见 spec 边界情形）。故判据是"有没有字母/数字/表意文字"，而不是
/// "有多长"。
pub(super) fn heard_nothing(text: &str) -> bool {
    !text.chars().any(char::is_alphanumeric)
}

/// 网关 HTTP 状态码 + 响应体 → 分流结果。纯函数，便于单测。
pub(super) fn voice_outcome(status: u16, body: &str) -> VoiceOutcome {
    match status {
        200 => match transcribe_text(body) {
            Ok(text) if !heard_nothing(&text) => VoiceOutcome::Transcribed { text },
            Ok(_) => VoiceOutcome::Unintelligible,
            Err(e) => VoiceOutcome::Skipped {
                reason: format!("转写响应无法解析（网关契约被破坏）：{e}"),
            },
        },
        422 => VoiceOutcome::Unintelligible,
        400 | 413 | 415 => VoiceOutcome::Rejected {
            reason: api_error(body),
        },
        // 500 / 503 以及任何没见过的状态码：宁可当成"我们自己的问题"（不落库、不编话术），
        // 也不要猜。状态码集合是封闭的，出现意外值本身就该被看见。
        _ => VoiceOutcome::Skipped {
            reason: format!("网关返回 {status}: {}", api_error(body)),
        },
    }
}

/// caption（用户自己写的说明，若有）在前，正文在后。
///
/// caption MUST NOT 被覆盖或丢弃（FR-012）；只有空白的 caption 等于没有。
pub(super) fn with_caption(caption: Option<&str>, body: &str) -> String {
    match caption.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => format!("{c}\n{body}"),
        None => body.to_string(),
    }
}

/// 组装转写成功的落库内容：`[语音] <文本>`（caption 若有则在前面）。
pub(super) fn transcribed_content(caption: Option<&str>, text: &str) -> String {
    with_caption(caption, &format!("{VOICE_PREFIX}{}", text.trim()))
}

/// caption（若有）在前、标记在后的落库内容。与 [`transcribed_content`] 同形，
/// 区别只是"正文"换成了情境描述标记。
pub(super) fn marker_content(caption: Option<&str>, marker: &str) -> String {
    with_caption(caption, marker)
}

/// 入站语音转写的运行参数：从 [`Config`] 取一次快照。
///
/// 快照而不是每次现读，是为了让 `inbound_audio_content` 的分流规则可以被单测**注入**，
/// 不必去改进程环境变量（那是全局状态，测试间会互相污染）。
#[derive(Debug, Clone, Copy)]
pub(super) struct VoiceCfg {
    pub(super) enabled: bool,
    /// 单次转写硬超时（秒）。
    pub(super) timeout: u64,
    /// 语音时长前置上限（秒）。
    pub(super) max_secs: u64,
}

impl VoiceCfg {
    pub(super) fn from_env() -> Self {
        let c = Config::get();
        Self {
            enabled: c.voice_transcribe_enabled,
            timeout: c.voice_transcribe_timeout,
            max_secs: c.voice_transcribe_max_secs,
        }
    }
}

/// 音频的处理决策：**在发起下载之前**就能定下来的部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VoicePlan {
    /// 前置拦截：不下载、不转写，直接按内容层面失败落标记。
    Reject(&'static str),
    /// 值得下载并转写。
    Transcribe,
}

/// 前置拦截（FR-003）：这条音频值不值得付下载 + 转写的代价。
///
/// 两道闸：`duration` 超过 `max_secs`（默认 120s）、`file_size` 超过 20 MB。
/// 两者都归 `Rejected`，用同一个标记——对用户与模型来说"太大了"是同一件事。
///
/// ⚠️ 必须排在下载之前。缺少这道的后果很具体：一条几分钟的语音会一路跑到转写**超时**，
/// 用户长时间什么都收不到、最后还静默失败——正是 FR-003 禁止的"长时间无响应"
/// （research.md D10、H1）。字段缺失（`None`）时**不拦**：不知道就别猜，交给下载与
/// 网关去判。
pub(super) fn voice_plan(audio: &AudioRef<'_>, max_secs: u64) -> VoicePlan {
    let too_long = audio
        .duration
        .is_some_and(|d| d > i64::try_from(max_secs).unwrap_or(i64::MAX));
    let too_big = audio.file_size.is_some_and(|s| s > MAX_VOICE_BYTES);
    if too_long || too_big {
        VoicePlan::Reject(VOICE_TOO_LARGE_MARKER)
    } else {
        VoicePlan::Transcribe
    }
}

/// "把一条音频送去转写"的抽象：返回 `(网关状态码, 响应体)`。
///
/// 存在的理由是**可测**。转写是本特性唯一的副作用入口（下载 + 调网关），把它收成一个
/// 窄接口后：① `inbound_audio_content` 的全部分流规则（成功 / 没听清 / 太大 / 系统故障）
/// 都能被单测覆盖；② "超限语音不会发起下载"（FR-003）能靠**调用次数**直接断言，
/// 而不是靠人读代码相信。
pub(super) trait Transcriber {
    fn transcribe(
        &self,
        file_id: &str,
        mime: &str,
        timeout: u64,
    ) -> impl std::future::Future<Output = Result<(u16, String), String>> + Send;
}

/// 生产实现：真的去 Telegram 取文件、再送本机网关。
pub(super) struct GatewayTranscriber<'a> {
    pub(super) client: &'a reqwest::Client,
    pub(super) token: &'a str,
    pub(super) gateway_url: &'a str,
}

impl Transcriber for GatewayTranscriber<'_> {
    async fn transcribe(
        &self,
        file_id: &str,
        mime: &str,
        timeout: u64,
    ) -> Result<(u16, String), String> {
        transcribe_voice(
            self.client,
            self.token,
            self.gateway_url,
            timeout,
            file_id,
            mime,
        )
        .await
    }
}

/// 处理一条**新鲜**的入站音频：前置拦截 → 下载 → 转写 → 组装落库内容。
///
/// 返回 `None` 表示**不落库、不起轮次**（只可能是系统故障路径，FR-006/013）；
/// 返回 `Some(文本)` 表示照常落库并起轮次——与用户直接打字走**完全相同**的那条路。
///
/// 三个分支对应 spec 的三类结果，别混：
/// - 成功 → `[语音] <转写文本>`；
/// - 内容层面失败（没听出内容 / 这条音频解不开 / 太大）→ 情境描述标记，**照常起轮次**，
///   让模型自己用自然语言回应（FR-004/005）；
/// - 系统故障（能力没开 / 连不上 / 超时 / 网关 5xx）→ 什么都不落，只记 error。
pub(super) async fn inbound_audio_content<T: Transcriber>(
    cfg: &VoiceCfg,
    chat_id: i64,
    audio: &AudioRef<'_>,
    caption: Option<&str>,
    transcriber: &T,
) -> Option<String> {
    if !cfg.enabled {
        tracing::error!(
            chat_id,
            "收到语音，但语音转写未启用（VOICE_TRANSCRIBE_ENABLED=0）：不落库不回复（FR-013）"
        );
        return None;
    }

    tracing::info!(
        chat_id,
        mime = audio.mime(),
        duration = ?audio.duration,
        file_size = ?audio.file_size,
        file_name = ?audio.file_name,
        title = ?audio.title,
        "开始转写入站语音"
    );

    // 【前置拦截】排在下载之前：这条音频如果注定处理不了，就**一点都不做**，直接按内容
    // 层面失败交给模型（FR-003）。这是"长语音不会长时间无响应"的落点。
    if let VoicePlan::Reject(marker) = voice_plan(audio, cfg.max_secs) {
        tracing::info!(
            chat_id,
            duration = ?audio.duration,
            file_size = ?audio.file_size,
            max_secs = cfg.max_secs,
            "语音超出处理上限，前置拦截：不下载、不转写（FR-003）"
        );
        return Some(marker_content(caption, marker));
    }

    let (status, body) = match transcriber
        .transcribe(audio.file_id, audio.mime(), cfg.timeout)
        .await
    {
        Ok(pair) => pair,
        // 下载失败 / 连不上网关 / 读超时：都是我们这边的问题（FR-006）。
        Err(e) => {
            tracing::error!(
                error = %e,
                chat_id,
                "语音转写调用失败（系统故障）：不落库不回复（FR-006/013）"
            );
            return None;
        }
    };

    match voice_outcome(status, &body) {
        VoiceOutcome::Transcribed { text } => {
            tracing::info!(chat_id, chars = text.chars().count(), "语音转写成功");
            Some(transcribed_content(caption, &text))
        }
        VoiceOutcome::Unintelligible => {
            tracing::info!(chat_id, "语音里没听出内容，交模型自然回应（FR-004/005）");
            Some(marker_content(caption, VOICE_UNINTELLIGIBLE_MARKER))
        }
        VoiceOutcome::Rejected { reason } => {
            tracing::info!(
                chat_id,
                %reason,
                "语音内容层面失败，交模型自然回应（FR-004/005）"
            );
            Some(marker_content(caption, VOICE_UNINTELLIGIBLE_MARKER))
        }
        VoiceOutcome::Skipped { reason } => {
            tracing::error!(
                chat_id,
                %reason,
                "语音转写不可用（系统故障）：不落库、不起轮次、不产生任何面向用户的文本（FR-006/013）"
            );
            None
        }
    }
}

/// `/health` 里我们关心的 ASR 部分。
#[derive(Debug)]
pub(super) struct AsrHealth {
    pub(super) engine: String,
    pub(super) importable: bool,
    pub(super) model_loaded: bool,
}

/// 从 `/health` 响应体里取 ASR 摘要。字段缺失一律按"不知道 / 不可用"处理，**绝不 panic**——
/// 探测是锦上添花，不该有自己的失败模式。
pub(super) fn asr_health(value: &serde_json::Value) -> AsrHealth {
    let asr = value.get("asr");
    let pick = |k: &str| asr.and_then(|a| a.get(k));
    AsrHealth {
        engine: pick("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("（网关未报告）")
            .to_string(),
        importable: pick("importable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        model_loaded: pick("model_loaded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

/// 启动自检（FR-013）：把转写能力的**真实**状态打进日志。
///
/// 目的是省掉"发了语音没反应，还得去翻代码猜是哪一层"这种排错。两种情况都要有明确日志：
/// 未启用时直说未启用；启用时报告网关的引擎 / 依赖 / 模型状态。
///
/// **任何失败都只记 warn，不上抛、不阻止长轮询**——网关是独立进程，没起是常态。
pub(super) async fn probe_voice_capability(cfg: &VoiceCfg, gateway_url: &str) {
    if !cfg.enabled {
        tracing::info!(
            "入站语音转写未启用（VOICE_TRANSCRIBE_ENABLED=0）：收到语音只记 error，不落库不回复"
        );
        return;
    }
    let client = match reqwest::Client::builder()
        .no_proxy() // 理由同 media_generate：本机代理会把 127.0.0.1 也路由进去
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "语音能力自检跳过：创建探测客户端失败");
            return;
        }
    };
    match client.get(format!("{gateway_url}/health")).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(value) => {
                let h = asr_health(&value);
                if h.importable {
                    tracing::info!(
                        engine = %h.engine,
                        importable = h.importable,
                        model_loaded = h.model_loaded,
                        "语音能力自检通过（转写可用）"
                    );
                } else {
                    tracing::warn!(
                        engine = %h.engine,
                        "语音能力自检：网关报告 ASR 不可用，转写会返回 503——检查 media-gateway 的 \
                         ASR_ENGINE 与 faster-whisper 安装（见 gateway/asr/）"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "语音能力自检：/health 响应不是合法 JSON"),
        },
        Ok(resp) => tracing::warn!(
            status = resp.status().as_u16(),
            gateway_url,
            "语音能力自检：网关 /health 返回非成功状态"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            gateway_url,
            "语音能力自检：连不上媒体网关（未启动？）；长轮询照常开始"
        ),
    }
}
