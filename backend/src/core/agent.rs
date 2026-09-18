//! 编排内核（Agent）：把能力端口组装成可复用的业务流水线。
//!
//! 内核只依赖 [`crate::core::ports`] 定义的能力契约（含对话持久化 `ConversationStore`），
//! 不感知 UniFFI / HTTP / 存储实现，可独立进行单元测试，
//! 也能被其他宿主换用不同的能力实现复用。

use std::sync::Arc;

use crate::core::ports::{ChatLlm, ConversationStore, MemoryStore, WorldState};
use crate::core::prompt::{self, build_day_history, build_recall_context, build_system_prompt};
use crate::core::types::{ChatInput, ChatOutput, ChatTurn, MemoryRecord};
use crate::error::Result;

/// Agent：完成单轮对话 / 记忆落库等核心业务流程。
pub struct Agent {
    conversation: Arc<dyn ConversationStore>,
    llm: Arc<dyn ChatLlm>,
    memory: Arc<dyn MemoryStore>,
    world: Arc<dyn WorldState>,
}

impl Agent {
    /// 用给定的能力端口构造内核（宿主负责准备存储句柄并注入各适配器）。
    pub fn with_ports(
        conversation: Arc<dyn ConversationStore>,
        llm: Arc<dyn ChatLlm>,
        memory: Arc<dyn MemoryStore>,
        world: Arc<dyn WorldState>,
    ) -> Self {
        Self {
            conversation,
            llm,
            memory,
            world,
        }
    }

    /// 暴露 LLM 端口（主动陪伴等周边编排复用同一能力契约）。
    pub(crate) fn chat_llm(&self) -> Arc<dyn ChatLlm> {
        self.llm.clone()
    }

    /// 世界状态快照文本（主动陪伴上下文组装用）。
    pub(crate) fn world_snapshot_text(&self, session_id: &str) -> String {
        self.world.snapshot_text(session_id)
    }

    /// 对话主入口（流式）。
    ///
    /// 单轮流程（消息是记录，记忆是事实，二者分离）：
    /// 1. 确定/新建会话，自动构建最近的历史消息上下文；
    /// 2. 世界能力：用真实时刻重算作息画像；记忆向量检索（失败降级为空召回）；
    /// 3. 世界状态 + 检索上下文拼入提示词调用 LLM（回复文本逐段回调 `on_delta`）；
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

        // 0'. 自动构建上下文：默认按"今天"取对话（日界=用户当地 06:00，今日过少自动并入昨天，
        // 让 AI 有连续性；时间感知由世界快照提供），内容保持原文不进装饰；
        // 显式 history_count 则退化为取最近 N 条。
        let history_turns = match input.history_count {
            Some(n) => self
                .conversation
                .recent_messages(&session.id, n as usize)?
                .into_iter()
                .map(|m| ChatTurn {
                    role: m.role,
                    content: m.content,
                })
                .collect::<Vec<_>>(),
            None => {
                let off = self.world.user_offset_minutes(&session.id);
                let start = prompt::day_window_start(off);
                let mut turns = self.conversation.messages_since(&session.id, start)?;
                if turns.len() < prompt::DAY_POOL_THRESHOLD {
                    turns = self
                        .conversation
                        .messages_since(&session.id, start - chrono::Duration::days(1))?;
                }
                build_day_history(&turns)?
            }
        };

        // 用户消息入库（记录）。带通道报文时间戳时用之（世界模型精确时间线），否则记接收时刻。
        let user_message = match input.user_time.as_deref().and_then(parse_rfc3339) {
            Some(at) => self
                .conversation
                .add_message_at(&session.id, "user", &input.message, at)?,
            None => self.conversation.add_message(&session.id, "user", &input.message)?,
        };
        // 世界能力：用这条真实时刻重算对方作息画像（无渠道映射时静默跳过）。
        let _ = self.world.observe(&session.id);

        // 2. 记忆能力：向量检索（已按 memory_recall_threshold 过滤相关度、剔除过期记忆）。
        // 检索依赖 embedding 端点，不可用时降级为空召回（等价于"这轮没有相关记忆"）。
        //
        // 注意"判定 + 沉淀 + 关系演化"都不在这条路径上：它们被后置到回复交付之后
        // （见 [`Self::settle_memory`]），既不占首字延迟，失败也不会连累这一轮回复。
        let memory_phase = std::time::Instant::now();
        let memory_hits = match self
            .memory
            .search(&input.message, prompt::MEMORY_RECALL_LIMIT)
        {
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

        // 2'. 世界状态 + 原始文本 + 检索上下文拼成提示词。
        let world_text = self.world.snapshot_text(&session.id);
        tracing::info!(session = %session.id, "对话阶段：提示词组装完成");
        let recall = build_recall_context(&memory_hits);
        let mut messages = vec![crate::core::types::ChatMessage {
            role: "system".to_string(),
            content: build_system_prompt(&recall, &world_text),
        }];
        for turn in history_turns.iter().chain(input.history.iter()) {
            messages.push(crate::core::types::ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }
        messages.push(crate::core::types::ChatMessage {
            role: "user".to_string(),
            content: input.message.clone(),
        });

        let llm_phase = std::time::Instant::now();
        let reply = self.llm.complete_stream(&messages, &mut on_delta)?;
        let llm_ms = llm_phase.elapsed().as_millis() as u64;
        tracing::info!(session = %session.id, llm_ms, "对话阶段：LLM 回复完成");

        // 3. AI 回复入库，刷新会话时间戳。
        self.conversation.add_message(&session.id, "assistant", &reply)?;
        self.conversation.touch_session(&session.id)?;

        Ok(ChatOutput {
            session_id: session.id,
            reply,
            user_message_id: user_message.id,
            // 记忆沉淀被后置（见 settle_memory），由宿主在回复交付后填充。
            memory: None,
        })
    }

    /// 记忆沉淀（回复交付后的独立一步）：判定 → 关系演化 → 落库。
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
        tracing::info!(session_id, "记忆沉淀：判定阶段开始");
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

        // 世界能力：按关系事件调整关系——日常闲聊影响很小，关心/暧昧大幅升温，
        // 矛盾/吵架乘法折损，和解专门修复信任。
        let _ = self.world.apply_event(session_id, &extraction.relation);

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
                Some(record)
            }
            Err(e) => {
                tracing::warn!(error = %e, "记忆沉淀失败，本轮回复不受影响");
                None
            }
        }
    }

    /// 直接将一段文本作为记忆落库，返回记忆 id（向量化失败等情况返回 None）。
    /// 手动添加默认按 core 长期记忆处理。
    pub fn store_memory(&self, content: &str, memory_type: &str) -> Result<Option<String>> {
        Ok(self
            .memory
            .store(content, memory_type, "core", None)
            .map(|m| m.id)
            .ok())
    }
}

/// 将 RFC3339 字符串解析为 UTC 时刻（解析失败视为未提供，回退接收时刻）。
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use crate::core::types::{
        ChatMessage, ConversationSession, Extraction, MemoryHit, MemoryRecord, MessageTurn,
        StoredMessage,
    };
    use crate::error::SqlError;

    // ---------- 假能力实现：证明内核可在零 db / 零真实能力下独立运行 ----------

    /// 记录一切写入的假会话存储。
    struct FakeConversation {
        log: Mutex<Vec<String>>,
    }

    impl ConversationStore for FakeConversation {
        fn get_or_create_session(&self, id: Option<&str>) -> Result<ConversationSession> {
            let id = id.unwrap_or("fake-session").to_string();
            Ok(ConversationSession {
                id,
                title: String::new(),
            })
        }
        fn set_session_title(&self, session_id: &str, _title: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("title {session_id}"));
            Ok(())
        }
        fn touch_session(&self, session_id: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("touch {session_id}"));
            Ok(())
        }
        fn add_message(&self, session_id: &str, role: &str, content: &str) -> Result<StoredMessage> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{role}:{content}"));
            let _ = session_id;
            Ok(StoredMessage { id: "m".to_string() })
        }
        fn add_message_at(
            &self,
            session_id: &str,
            role: &str,
            content: &str,
            _at: chrono::DateTime<chrono::Utc>,
        ) -> Result<StoredMessage> {
            self.add_message(session_id, role, content)
        }
        fn messages_since(
            &self,
            _session_id: &str,
            _since: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<MessageTurn>> {
            Ok(Vec::new())
        }
        fn recent_messages(&self, _session_id: &str, _limit: usize) -> Result<Vec<MessageTurn>> {
            Ok(Vec::new())
        }
    }

    /// 固定回复的假 LLM（回显输入，验证消息确实经过端口）。
    struct FakeLlm;

    impl ChatLlm for FakeLlm {
        fn complete(&self, _msgs: &[ChatMessage]) -> Result<String> {
            Ok("(okk)".to_string())
        }
        fn complete_structured(
            &self,
            _msgs: &[ChatMessage],
            _model: &str,
            _name: &str,
            _schema: serde_json::Value,
        ) -> Result<String> {
            Ok(String::new())
        }
        fn complete_stream(
            &self,
            _msgs: &[ChatMessage],
            on_delta: &mut (dyn FnMut(&str) + Send),
        ) -> Result<String> {
            on_delta("hi");
            Ok("hi".to_string())
        }
    }

    /// 假记忆能力：默认不入记忆、检索为空。
    struct FakeMemory;

    impl MemoryStore for FakeMemory {
        fn extract(&self, _text: &str) -> Result<Extraction> {
            Ok(Extraction {
                is_memory: false,
                content: None,
                memory_type: "fact".to_string(),
                tier: "core".to_string(),
                relation: "neutral".to_string(),
            })
        }
        fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryHit>> {
            Ok(Vec::new())
        }
        fn store(
            &self,
            _content: &str,
            _memory_type: &str,
            _tier: &str,
            _message_id: Option<&str>,
        ) -> Result<MemoryRecord> {
            unreachable!("本测试不沉淀记忆")
        }
    }

    /// 假记忆能力：可分别让判定 / 召回 / 沉淀失败，验证记忆故障的降级行为。
    struct FaultyMemory {
        extract_fails: bool,
        search_fails: bool,
        store_fails: bool,
        /// 判定的结论（是否值得沉淀）。
        is_memory: bool,
    }

    impl FaultyMemory {
        /// 全链路正常、判定为"值得记住"的基线配置。
        fn healthy() -> Self {
            Self {
                extract_fails: false,
                search_fails: false,
                store_fails: false,
                is_memory: true,
            }
        }
    }

    impl MemoryStore for FaultyMemory {
        fn extract(&self, _text: &str) -> Result<Extraction> {
            if self.extract_fails {
                return Err(SqlError::Config("模拟：解析提取结果失败".to_string()));
            }
            Ok(Extraction {
                is_memory: self.is_memory,
                content: Some("对方在准备考试".to_string()),
                memory_type: "fact".to_string(),
                tier: "intent".to_string(),
                relation: "neutral".to_string(),
            })
        }
        fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryHit>> {
            if self.search_fails {
                return Err(SqlError::Config("模拟：embedding 端点不可用".to_string()));
            }
            Ok(Vec::new())
        }
        fn store(
            &self,
            content: &str,
            memory_type: &str,
            tier: &str,
            message_id: Option<&str>,
        ) -> Result<MemoryRecord> {
            if self.store_fails {
                return Err(SqlError::Config("模拟：沉淀失败".to_string()));
            }
            Ok(MemoryRecord {
                id: "mem-1".to_string(),
                content: content.to_string(),
                memory_type: memory_type.to_string(),
                tier: tier.to_string(),
                expires_at: None,
                message_id: message_id.map(String::from),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            })
        }
    }

    /// 假世界状态：什么都不做。
    struct FakeWorld;

    impl WorldState for FakeWorld {
        fn observe(&self, _session_id: &str) -> Result<()> {
            Ok(())
        }
        fn apply_event(&self, _session_id: &str, _relation: &str) -> Result<()> {
            Ok(())
        }
        fn snapshot_text(&self, _session_id: &str) -> String {
            String::new()
        }
        fn user_offset_minutes(&self, _session_id: &str) -> i64 {
            8 * 60
        }
    }

    #[test]
    fn port_driven_pipeline_flows_messages_and_returns_reply() {
        let conversation = Arc::new(FakeConversation {
            log: Mutex::new(Vec::new()),
        });
        let agent = Agent::with_ports(
            conversation.clone(),
            Arc::new(FakeLlm),
            Arc::new(FakeMemory),
            Arc::new(FakeWorld),
        );
        let mut deltas = Vec::new();
        let out = agent
            .chat_stream(
                &ChatInput {
                    message: "在吗".to_string(),
                    session_id: None,
                    history: Vec::<ChatTurn>::new(),
                    history_count: None,
                    user_time: None,
                },
                |d| deltas.push(d.to_string()),
            )
            .unwrap();

        assert_eq!(out.reply, "hi");
        assert_eq!(deltas, vec!["hi"]);
        assert!(out.memory.is_none(), "记忆沉淀已后置，chat_stream 不返回记忆");
        assert_eq!(out.user_message_id, "m", "需回传用户消息 id 供后置沉淀关联");

        // 流水线应把用户消息与 AI 回复都写入会话，并按序触发标题/活跃刷新。
        let log = conversation.log.lock().unwrap();
        assert!(log.contains(&"user:在吗".to_string()), "{log:?}");
        assert!(log.contains(&"assistant:hi".to_string()), "{log:?}");
        assert!(log.iter().any(|e| e.starts_with("title ")), "{log:?}");
        assert!(log.iter().any(|e| e.starts_with("touch ")), "{log:?}");
        // 用户消息必须先于 AI 回复写入。
        let u = log.iter().position(|e| e == "user:在吗").unwrap();
        let a = log.iter().position(|e| e == "assistant:hi").unwrap();
        assert!(u < a, "{log:?}");
    }

    fn chat_input(text: &str) -> ChatInput {
        ChatInput {
            message: text.to_string(),
            session_id: None,
            history: Vec::<ChatTurn>::new(),
            history_count: None,
            user_time: None,
        }
    }

    /// 组装一个只换了记忆能力的 Agent，用于记忆链路的降级测试。
    fn agent_with_memory(memory: FaultyMemory) -> Agent {
        Agent::with_ports(
            Arc::new(FakeConversation {
                log: Mutex::new(Vec::new()),
            }),
            Arc::new(FakeLlm),
            Arc::new(memory),
            Arc::new(FakeWorld),
        )
    }

    #[test]
    fn memory_recall_failure_still_replies() {
        // 召回故障（embedding 端点不可用）：记忆是辅助能力，对话必须照常返回。
        let agent = agent_with_memory(FaultyMemory {
            search_fails: true,
            ..FaultyMemory::healthy()
        });

        let out = agent.chat_stream(&chat_input("在吗"), |_| {}).unwrap();

        assert_eq!(out.reply, "hi");
        assert!(out.memory.is_none(), "沉淀已后置，chat_stream 不返回记忆");
        assert_eq!(out.user_message_id, "m", "仍需回传用户消息 id");
    }

    #[test]
    fn settle_memory_stores_recorded_fact() {
        // 回复交付后的沉淀：判定值得记住 → 落库，并用用户消息 id 关联来源。
        let agent = agent_with_memory(FaultyMemory::healthy());

        let record = agent.settle_memory("s1", "m1", "我在准备考试").unwrap();

        assert_eq!(record.message_id.as_deref(), Some("m1"));
        assert_eq!(record.content, "对方在准备考试", "优先用事实化内容");
        assert_eq!(record.tier, "intent");
    }

    #[test]
    fn settle_memory_skips_when_not_worth_remembering() {
        let agent = agent_with_memory(FaultyMemory {
            is_memory: false,
            ..FaultyMemory::healthy()
        });

        assert!(agent.settle_memory("s1", "m1", "在吗").is_none());
    }

    #[test]
    fn settle_memory_degrades_on_failure() {
        // 判定失败、沉淀失败都只返回 None：不 panic、不影响已经交付的回复。
        for memory in [
            FaultyMemory {
                extract_fails: true,
                ..FaultyMemory::healthy()
            },
            FaultyMemory {
                store_fails: true,
                ..FaultyMemory::healthy()
            },
        ] {
            let agent = agent_with_memory(memory);
            assert!(agent.settle_memory("s1", "m1", "我在准备考试").is_none());
        }
    }
}