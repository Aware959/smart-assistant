//! 编排内核（Agent）：把会话 + 能力组件组装成可复用的业务流水线。
//!
//! 重构后会话是 core 内具体类型（[`Sessions`]），能力直接以具体类型接线：
//! LLM = [`crate::llm::chat::Llm`]，记忆 = [`crate::core::memory::Store`]。

use std::sync::Arc;

use crate::core::context::{self, AgentContext};
use crate::core::memory::{MemoryHit, MemoryRecord, Store};
use crate::core::message::ChatMessage;
use crate::core::session::Sessions;
use crate::error::Result;
use crate::llm::chat::Llm;

/// 对话输入：处理层拿到的最低输入契约（会话 / 消息 / 可选的显式历史）。
#[derive(Debug, Clone)]
pub struct ChatInput {
    pub message: String,
    /// 会话 id。留空时后端自动新建会话并返回新 id。
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选，补充上下文）；留空时后端自动从会话消息构建。
    pub history: Vec<ChatMessage>,
    /// 自动构建上下文的方式：缺省按"今天"取消息（日界=用户当地 06:00、带时间戳）；
    /// 显式指定 N 时改为取最近 N 条纯文本。
    pub history_count: Option<u32>,
    /// 用户端消息的真实发出时刻（RFC3339 世界时，来自通道报文时间戳）。
    /// 缺省时为后端接收时刻；用于消息记录的真实时间线。
    pub user_time: Option<String>,
}

/// 对话输出：一轮对话的结果（会话 / 回复 / 来源消息 / 沉淀记忆）。
#[derive(Debug, Clone)]
pub struct ChatOutput {
    pub session_id: String,
    pub reply: String,
    /// 本条用户消息的 id：宿主在回复交付后调用 `settle_memory` 时用它关联记忆来源。
    pub user_message_id: String,
    /// 本轮沉淀出的记忆。
    ///
    /// `chat_stream` 返回时**恒为 None**——记忆沉淀已后置到回复交付之后，由宿主调用
    /// `settle_memory` 拿到结果后自行填充（响应结构因此保持不变）。
    pub memory: Option<MemoryRecord>,
}

/// Agent：完成单轮对话 / 记忆落库等核心业务流程。
pub struct Agent {
    conversation: Arc<Sessions>,
    llm: Arc<Llm>,
    memory: Arc<Store>,
}

impl Agent {
    /// 用具体会话适配器与两个能力组件构造内核。
    pub fn new(conversation: Arc<Sessions>, llm: Arc<Llm>, memory: Arc<Store>) -> Self {
        Self {
            conversation,
            llm,
            memory,
        }
    }

    /// 暴露 LLM 能力（主动陪伴等周边编排复用同一具体实现）。
    pub(crate) fn chat_llm(&self) -> Arc<Llm> {
        self.llm.clone()
    }

    /// 最近 `limit` 条对话的渲染文本（延续判断等只需历史的场景，避免白跑一次召回）。
    pub(crate) fn recent_history_text(&self, session_id: &str, limit: usize) -> String {
        context::recent_history_text(self.conversation.as_ref(), session_id, limit)
    }

    /// 记忆召回的纯文本渲染（主动决策阅读用，失败静默为空）。
    pub(crate) fn recall_text(&self, query: &str, limit: usize) -> String {
        match context::fetch_recall(self.memory.as_ref(), query, limit) {
            Ok(hits) => context::plain_lines(&hits),
            Err(_) => String::new(),
        }
    }

    /// 对话主入口（流式）。
    ///
    /// 单轮流程（消息是记录，记忆是事实，二者分离）：
    /// 1. 确定/新建会话；取历史消息（经 [`crate::core::context`]，须在消息入库前取）；
    /// 2. 本条消息入库；
    /// 3. 记忆召回（失败降级为空召回），拼入提示词调用 LLM
    ///    （回复文本逐段回调 `on_delta`）；
    /// 4. 回复写入 messages，连同本条用户消息 id 一起返回。
    ///
    /// **记忆沉淀不在本函数里**：判定/事实化要额外调 LLM、沉淀要调 embedding，
    /// 挂在对话路径上既拖首字延迟、又可能让用户拿不到回复。宿主应在回复交付之后
    /// 调用 [`Self::settle_memory`] 补齐（可放后台线程）。
    ///
    /// `on_delta` 在生成线程上同步调用，生成期间实时收到文本片段。
    pub fn chat_stream<F>(&self, input: &ChatInput, mut on_delta: F) -> Result<ChatOutput>
    where
        F: FnMut(&str) + Send,
    {
        // 追踪：一次调用 = 一个 trace_id span，本函数内所有 target="trace" 的事件
        // 都会带上 trace_id / session_id，便于把"一次请求的各环节"归成一组。
        let span = crate::utils::trace::turn_span();
        let _enter = span.enter();
        let turn_started = std::time::Instant::now();

        // 空消息不入库、不检索，直接返回。
        if input.message.trim().is_empty() {
            return Ok(ChatOutput {
                session_id: input.session_id.clone().unwrap_or_default(),
                reply: String::new(),
                user_message_id: String::new(),
                memory: None,
            });
        }

        // 1. 会话：复用传入的 id；不存在或未传则自动新建。
        let session = self.conversation.get_or_create_session(input.session_id.as_deref())?;

        // 首条消息自动生成会话标题（截取前 24 个字符）。
        if session.title.trim().is_empty() {
            let title: String = input.message.chars().take(24).collect();
            self.conversation.set_session_title(&session.id, &title)?;
        }
        span.record("session_id", session.id.as_str());
        tracing::info!(
            target: "trace",
            stage = "start",
            history_count = input.history_count,
            user_time = input.user_time.as_deref().unwrap_or(""),
            message = %crate::utils::trace::Text(&input.message),
            "对话开始"
        );

        // 2. 上下文（一）：历史消息。**必须在"本条用户消息入库"之前取**，否则本轮消息
        // 会被算进历史重复出现。默认按"今天"取对话（日界=用户当地 06:00，今日过少自动
        // 并入昨天，让 AI 有连续性）；显式 history_count 则退化为取最近 N 条。
        // 取数与整理统一由 core::context 负责（与主动决策共用同一套实现）。
        let history_turns = context::fetch_history(
            self.conversation.as_ref(),
            &session.id,
            input.history_count,
        )?;
        tracing::info!(
            target: "trace",
            stage = "history",
            turns = history_turns.len(),
            history = %crate::utils::trace::Lazy(|| render_messages(&history_turns)),
            "历史消息已取"
        );

        // 用户消息入库（记录）。带通道报文时间戳时用之（消息记录精确时间线），否则记接收时刻。
        let user_message = match input.user_time.as_deref().and_then(parse_rfc3339) {
            Some(at) => self
                .conversation
                .add_message_at(&session.id, "user", &input.message, at)?,
            None => self.conversation.add_message(&session.id, "user", &input.message)?,
        };

        // 3. 上下文（二）：记忆召回（已按 memory_recall_threshold 过滤相关度、剔除过期记忆）。
        // 检索依赖 embedding 端点，不可用时降级为空召回（等价于"这轮没有相关记忆"）。
        //
        // 注意"判定 + 沉淀 + 关系演化"都不在这条路径上：它们被后置到回复交付之后
        // （见 [`Self::settle_memory`]），既不占首字延迟，失败也不会连累这一轮回复。
        let memory_phase = std::time::Instant::now();
        let memory_hits = match context::fetch_recall(
            self.memory.as_ref(),
            &input.message,
            context::RECALL_LIMIT,
        ) {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!(error = %e, "记忆召回失败，本轮按无记忆上下文继续");
                Vec::new()
            }
        };
        let memory_ms = memory_phase.elapsed().as_millis() as u64;
        tracing::info!(
            hits = memory_hits.len(),
            memory_ms,
            "对话阶段：记忆向量召回完成"
        );
        tracing::info!(
            target: "trace",
            stage = "recall",
            hits = memory_hits.len(),
            recalled = %crate::utils::trace::Lazy(|| render_hits(&memory_hits)),
            "记忆召回完成"
        );

        // 4. 上下文（二）：召回记忆拼入提示词。本条消息连同历史一起入 messages。
        let ctx = AgentContext {
            history: history_turns,
            memories: memory_hits,
        };
        tracing::info!(session = %session.id, "对话阶段：提示词组装完成");
        let recall = context::build_recall_context(&ctx.memories);
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: context::system_prompt(&recall),
        }];
        for turn in ctx.history.iter().chain(input.history.iter()) {
            messages.push(ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: input.message.clone(),
        });
        tracing::info!(
            target: "trace",
            stage = "prompt",
            n = messages.len(),
            system = %crate::utils::trace::Text(&messages[0].content),
            messages = %crate::utils::trace::Lazy(|| render_messages(&messages)),
            "提示词与消息已组装"
        );

        let llm_phase = std::time::Instant::now();
        let reply = self.llm.complete_stream(&messages, &mut on_delta)?;
        let llm_ms = llm_phase.elapsed().as_millis() as u64;
        tracing::info!(session = %session.id, llm_ms, "对话阶段：LLM 回复完成");
        tracing::info!(
            target: "trace",
            stage = "reply",
            llm_ms,
            reply = %crate::utils::trace::Text(&reply),
            "模型回复完成"
        );

        // 3. AI 回复入库，刷新会话时间戳。
        self.conversation.add_message(&session.id, "assistant", &reply)?;
        self.conversation.touch_session(&session.id)?;

        span.record("user_message_id", user_message.id.as_str());
        tracing::info!(
            target: "trace",
            stage = "done",
            user_message_id = %user_message.id,
            total_ms = turn_started.elapsed().as_millis() as u64,
            "对话完成"
        );

        Ok(ChatOutput {
            session_id: session.id,
            reply,
            user_message_id: user_message.id,
            // 记忆沉淀被后置（见 settle_memory），由宿主在回复交付后填充。
            memory: None,
        })
    }

    /// 记忆沉淀（回复交付后的独立一步）：判定 → 落库。
    ///
    /// **为什么单独一步**：判定要调 LLM、沉淀要调 embedding，两者都可能慢、也可能失败。
    /// 挂在 [`Self::chat_stream`] 上会拉长首字延迟，失败还会让用户拿不到回复。后置之后
    /// 宿主的调用时序变成"回复已送达 → 后台沉淀"，记忆链路彻底不参与对话体验。
    ///
    /// 任何环节失败都只记 warn 并返回 `None`——记忆是辅助能力。
    /// `user_message_id` 为空表示来源不可关联（记忆照常落库，只是 `message_id` 为空）。
    pub fn settle_memory(
        &self,
        session_id: &str,
        user_message_id: &str,
        text: &str,
    ) -> Option<MemoryRecord> {
        let span = crate::utils::trace::settle_span(session_id, user_message_id);
        let _enter = span.enter();
        tracing::info!(session_id, "记忆沉淀：判定阶段开始");
        tracing::info!(
            target: "trace",
            stage = "extract_start",
            text = %crate::utils::trace::Text(text),
            "记忆沉淀：判定开始"
        );
        let extraction = match self.memory.extract(text) {
            Ok(extraction) => extraction,
            Err(e) => {
                tracing::warn!(error = %e, "记忆判定失败，跳过本轮记忆沉淀");
                return None;
            }
        };
        tracing::info!(
            session_id,
            is_memory = extraction.is_memory,
            memory_type = %extraction.memory_type,
            tier = %extraction.tier,
            "记忆沉淀：判定完成"
        );
        tracing::info!(
            target: "trace",
            stage = "extract_done",
            is_memory = extraction.is_memory,
            memory_type = %extraction.memory_type,
            tier = %extraction.tier,
            relation = %extraction.relation,
            content = %crate::utils::trace::Text(extraction.content.as_deref().unwrap_or("")),
            "记忆判定完成"
        );

        if !extraction.is_memory {
            return None;
        }

        // 事实化失败时回退原文（content_or 的兜底）。
        let content = extraction.content_or(text);
        let message_id = (!user_message_id.is_empty()).then_some(user_message_id);
        let store_started = std::time::Instant::now();
        match self
            .memory
            .store(content, &extraction.memory_type, &extraction.tier, message_id)
        {
            Ok(record) => {
                tracing::info!(
                    session_id,
                    store_ms = store_started.elapsed().as_millis() as u64,
                    "记忆沉淀：落库完成"
                );
                tracing::info!(
                    target: "trace",
                    stage = "store_done",
                    memory_id = %record.id,
                    content = %crate::utils::trace::Text(&record.content),
                    "记忆落库完成"
                );
                Some(record)
            }
            Err(e) => {
                tracing::warn!(error = %e, "记忆沉淀失败，本轮回复不受影响");
                None
            }
        }
    }
}

/// 将 RFC3339 字符串解析为 UTC 时刻（解析失败视为未提供，回退接收时刻）。
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// 把消息列表渲染成 `role: content` 多行文本（追踪日志用，仅在开启全文时构建）。
fn render_messages(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 把召回记忆渲染成 `[score] id | content` 多行文本（追踪日志用）。
fn render_hits(hits: &[MemoryHit]) -> String {
    hits.iter()
        .map(|h| format!("[{:.3}] {} | {}", h.score, h.id, h.content))
        .collect::<Vec<_>>()
        .join("\n")
}