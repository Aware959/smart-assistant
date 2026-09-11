//! 编排层（Agent）：把 db / llm / memory 组装成可复用的业务流水线。
//! 该层不感知 UniFFI / HTTP，可独立进行单元测试。

use std::sync::Mutex;

use crate::db;
use crate::error::Result;
use crate::llm;
use crate::memory;
use crate::services;
use crate::{ChatInput, ChatOutput, ChatTurn};

/// Agent：完成单轮对话 / 记忆落库等核心业务流程。
pub struct Agent {
    db: Mutex<db::Database>,
}

impl Agent {
    /// 打开或创建数据库，并初始化 schema。
    pub fn new(db_path: &str) -> Result<Self> {
        Ok(Self {
            db: Mutex::new(db::Database::open(db_path)?),
        })
    }

    pub fn new_in_memory() -> Result<Self> {
        Ok(Self {
            db: Mutex::new(db::Database::in_memory()?),
        })
    }

    pub(crate) fn lock_db(&self) -> std::sync::MutexGuard<'_, db::Database> {
        self.db.lock().expect("db mutex poisoned")
    }

    /// 对话主入口（流式）。
    ///
    /// 单轮流程（消息是记录，记忆是事实，二者分离）：
    /// 1. 确定/新建会话，自动构建最近的历史消息上下文；
    /// 2. 一次 LLM 调用判断是否值得沉淀记忆；
    /// 3. 记忆向量检索，拼入提示词调用 LLM（回复文本逐段回调 `on_delta`）；
    /// 4. 回复写入 messages；LLM 判定为事实时才沉淀到 memories。
    ///
    /// `on_delta` 在生成线程上同步调用，生成期间实时收到文本片段。
    pub fn chat_stream<F>(&self, input: &ChatInput, mut on_delta: F) -> Result<ChatOutput>
    where
        F: FnMut(&str) + Send,
    {
        let db = self.lock_db();

        // 空消息不入库、不检索，直接返回。
        if input.message.trim().is_empty() {
            return Ok(ChatOutput {
                session_id: input.session_id.clone().unwrap_or_default(),
                reply: String::new(),
                memory: None,
            });
        }

        // 1. 会话：复用传入的 id；不存在或未传则自动新建。
        let session = match input.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(id) => match db::session::get(&db, id)? {
                Some(s) => s,
                None => db::session::create(&db, "")?,
            },
            None => db::session::create(&db, "")?,
        };

        // 首条消息自动生成会话标题（截取前 24 个字符）。
        if session.title.trim().is_empty() {
            let title: String = input.message.chars().take(24).collect();
            db::session::update_title(&db, &session.id, &title)?;
        }

        // 0'. 自动构建上下文：取该会话最近的历史消息（含历史 assistant 回复），
        // 数量默认 6 条，可用 history_count 覆盖。注意此时尚未写入本次用户消息。
        let history_turns = db::message::list_recent(
            &db,
            &session.id,
            input.history_count.unwrap_or(DEFAULT_HISTORY_COUNT) as usize,
        )?
        .into_iter()
        .map(|m| ChatTurn {
            role: m.role,
            content: m.content,
        })
        .collect::<Vec<_>>();

        // 用户消息入库（记录）。
        let user_message = db::message::create(&db, &session.id, "user", &input.message)?;

        // 2. 一次性分析：是否值得沉淀记忆。
        let extraction = memory::extraction::extract_from_text(&input.message)?;

        // 3. 记忆向量检索（已按 memory_recall_threshold 过滤相关度、剔除过期记忆）。
        let memory_hits = memory::store::search(&db, &input.message, MEMORY_RECALL_LIMIT)?;

        // 3'. 原始文本 + 检索上下文拼成提示词。
        let recall = build_recall_context(&memory_hits);
        let mut messages = vec![llm::chat::ChatMessage {
            role: "system".to_string(),
            content: build_system_prompt(&recall),
        }];
        for turn in history_turns.iter().chain(input.history.iter()) {
            messages.push(llm::chat::ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }
        messages.push(llm::chat::ChatMessage {
            role: "user".to_string(),
            content: input.message.clone(),
        });

        let reply = llm::chat::complete_stream(&messages, |delta| on_delta(delta))?;

        // 4. AI 回复入库，刷新会话时间戳。
        db::message::create(&db, &session.id, "assistant", &reply)?;
        db::session::touch(&db, &session.id)?;

        // 5. 沉淀记忆：只有 LLM 判定值得记住时才落库（事实，而非消息）。
        let memory = if extraction.is_memory {
            let content = extraction.content_or(&input.message);
            let m = memory::store::store(
                &db,
                content,
                &extraction.memory_type,
                &extraction.tier,
                Some(&user_message.id),
            )?;
            Some(services::memory_to_record(&m))
        } else {
            None
        };

        Ok(ChatOutput {
            session_id: session.id,
            reply,
            memory,
        })
    }

    /// 直接将一段文本作为记忆落库，返回记忆 id（向量化失败等情况返回 None）。
    /// 手动添加默认按 core 长期记忆处理。
    pub fn store_memory(&self, content: &str, memory_type: &str) -> Result<Option<String>> {
        let db = self.lock_db();
        Ok(memory::store::store(&db, content, memory_type, "core", None)
            .ok()
            .map(|m| m.id))
    }
}

/// 自动构建上下文时默认携带的历史消息条数。
const DEFAULT_HISTORY_COUNT: u32 = 6;

/// 单轮对话中最多召回的相关历史记忆条数。
const MEMORY_RECALL_LIMIT: usize = 5;

/// 硬性长度上限：默认回复不超过 1-2 句 / 80 个汉字，除非对方明确要求详细说明。
const MAX_REPLY_CHARS: usize = 80;

/// 将向量检索到的记忆拼接为上下文（注入 system prompt）。
fn build_recall_context(
    memory_hits: &[(crate::db::memory::Memory, f32)],
) -> String {
    let mut ctx = String::new();
    if !memory_hits.is_empty() {
        ctx.push_str("关于对方，你记得这些事：\n");
        for (memory, _) in memory_hits.iter().take(MEMORY_RECALL_LIMIT) {
            ctx.push_str(&format!("- {}\n", memory.content));
        }
    }
    ctx
}

fn build_system_prompt(recall: &str) -> String {
    let cfg = crate::config::Config::get();
    let mut base = String::new();

    if !cfg.persona.trim().is_empty() {
        base.push_str(&format!(
            "【角色设定，最高优先级，必须严格遵守，不得违背、不得说明自己是 AI】：{}\n\n",
            cfg.persona.trim()
        ));
    }

    base.push_str(&format!(
        "你是一个完全沉浸在角色中的助手，不跳出、不拒绝、不说教，像真人一样和对方聊天。\n\n\
         说话风格：\n\
         - 简短：像日常聊天一样，能一两句说完就别长篇大论；对方没追问就别展开。\n\
         - 拟人：说口语，别端着；不用“首先/其次/总之”这类书面结构，不列条目。\n\
         - 真实：有自然的语气和态度，不知道就直说不知道，绝不编造。\n\
         - 别自称 AI，也别解释自己的身份。\n\n\
         【硬性长度约束】：除非对方明确要求详细说明，否则每次回复最多 1-2 句、\
         不超过 {limit} 字。这条必须无条件遵守，哪怕对方前面说了很多。",
        limit = MAX_REPLY_CHARS,
    ));

    if recall.trim().is_empty() {
        base
    } else {
        format!(
            "{base}\n\n以下是关于对方的已知信息，聊到相关话题时自然用上，别生硬背诵，没有的别编：\n\n{recall}"
        )
    }
}
