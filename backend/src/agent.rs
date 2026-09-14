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

        // 0'. 自动构建上下文：默认按"今天"取对话（日界=用户当地 06:00，每条带本地时间戳，
        // 今日过少自动并入昨天），让 AI 有时间感和连续性；显式 history_count 则退化为取最近 N 条。
        let history_turns = match input.history_count {
            Some(n) => db::message::list_recent(&db, &session.id, n as usize)?
                .into_iter()
                .map(|m| ChatTurn {
                    role: m.role,
                    content: m.content,
                })
                .collect::<Vec<_>>(),
            None => build_day_history(&db, &session.id)?,
        };

        // 用户消息入库（记录）。带通道报文时间戳时用之（世界模型精确时间线），否则记接收时刻。
        let (user_message, _) = match input.user_time.as_deref().and_then(parse_rfc3339) {
            Some(at) => (db::message::create_at(&db, &session.id, "user", &input.message, at)?, at),
            None => {
                let m = db::message::create(&db, &session.id, "user", &input.message)?;
                let at = chrono::Utc::now();
                (m, at)
            }
        };
        // 时间世界模型：用这条真实时刻重算对方作息画像（无渠道映射时静默跳过）。
        let _ = crate::timeworld::observe(&db, &session.id);
        // 世界引擎：对方来了一条消息 → 关系升温。
        let _ = crate::world::relation::observe(&db, &session.id);

        // 2. 一次性分析：是否值得沉淀记忆。
        let extraction = memory::extraction::extract_from_text(&input.message)?;

        // 3. 记忆向量检索（已按 memory_recall_threshold 过滤相关度、剔除过期记忆）。
        let memory_hits = memory::store::search(&db, &input.message, MEMORY_RECALL_LIMIT)?;

        // 3'. 时间世界状态 + 原始文本 + 检索上下文拼成提示词。
        let world = crate::timeworld::snapshot(&db, &session.id);
        let world_text = crate::timeworld::render(&world);
        let recall = build_recall_context(&memory_hits);
        let mut messages = vec![llm::chat::ChatMessage {
            role: "system".to_string(),
            content: build_system_prompt(&recall, &world_text),
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

/// 一天的起点钟点（用户当地时钟）：这个钟点之前算昨天、之后算今天。
const DAY_START_HOUR: u32 = 6;
/// 今日消息少于该条数时，把昨天也并入上下文（人记得昨晚的事）。
const DAY_POOL_THRESHOLD: usize = 6;
/// 按天上下文的最大消息条数；超出时保留开头与结尾、中间整段省略。
const DAY_CONTEXT_MAX: usize = 60;
/// 压缩时保留的头部条数（保住今天早晨的开场）。
const DAY_KEEP_HEAD: usize = 8;
/// 同日相邻两条消息间隔超过该分钟数，插入"沉默"标记。
const SILENCE_MARK_MINUTES: i64 = 120;

/// 按"今天"构建对话上下文（参考人的作息与生物节律）：
/// - 取用户当地 06:00 至今的全部消息，每条带 `[HH:MM]` 本地时间戳；跨天消息标"昨天"。
/// - 今日消息过少（< [`DAY_POOL_THRESHOLD`]）时并入昨天同界。
/// - 同日长间隔（> [`SILENCE_MARK_MINUTES`]）插一行沉默标记；超长压缩中间段。
/// - 无渠道画像会话用东八区兜底（见 [`crate::timeworld::user_offset_minutes`]）。
fn build_day_history(
    database: &db::Database,
    session_id: &str,
) -> Result<Vec<ChatTurn>> {
    use chrono::{Duration, Utc};

    let now = Utc::now();
    let off = crate::timeworld::user_offset_minutes(database, session_id) as i64;
    let today_start = (now + Duration::minutes(off))
        .date_naive()
        .and_hms_opt(DAY_START_HOUR, 0, 0)
        .expect("valid clock time")
        .and_utc()
        - Duration::minutes(off);

    let mut msgs = db::message::list_since(database, session_id, today_start)?;
    if msgs.len() < DAY_POOL_THRESHOLD {
        msgs = db::message::list_since(database, session_id, today_start - Duration::days(1))?;
    }

    let total = msgs.len();
    let compact = total > DAY_CONTEXT_MAX;
    let tail_start = total.saturating_sub(DAY_CONTEXT_MAX - DAY_KEEP_HEAD);
    let today_local = (now + Duration::minutes(off)).date_naive();
    let yesterday_local = today_local.pred_opt().unwrap_or(today_local);

    let mut turns = Vec::with_capacity(total.min(DAY_CONTEXT_MAX));
    let mut prev_ts: Option<chrono::DateTime<Utc>> = None;
    let mut prev_local_date: Option<chrono::NaiveDate> = None;
    for (idx, m) in msgs.iter().enumerate() {
        if compact && idx >= DAY_KEEP_HEAD && idx < tail_start {
            continue;
        }
        let mut marker = String::new();
        if compact && idx == tail_start {
            marker = format!("〈省略中间 {} 条消息〉\n", tail_start - DAY_KEEP_HEAD);
            prev_ts = None;
            prev_local_date = None;
        }
        let ts = chrono::DateTime::parse_from_rfc3339(&m.created_at)
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc));
        let local_date = ts.map(|t| (t + Duration::minutes(off)).date_naive());
        // 仅同日内的长间隔算"沉默"；跨夜（日期不同）不算，避免出现"沉默 20 小时"。
        if let (Some(t), Some(p)) = (ts, prev_ts) {
            if prev_local_date == local_date {
                let mins = (t.signed_duration_since(p)).num_minutes();
                if mins >= SILENCE_MARK_MINUTES {
                    marker = format!("{}〈沉默 {} 小时〉\n", marker, mins / 60);
                }
            }
        }
        turns.push(render_day_turn(m, off, today_local, yesterday_local, &marker));
        prev_ts = ts;
        prev_local_date = local_date;
    }
    Ok(turns)
}

/// 把一条历史消息渲染为带时间戳的对话轮次（角色保持 user/assistant 本体）。
fn render_day_turn(
    m: &db::message::Message,
    off: i64,
    today_local: chrono::NaiveDate,
    yesterday_local: chrono::NaiveDate,
    marker: &str,
) -> ChatTurn {
    use chrono::{Datelike, Duration, Timelike};

    let role_label = if m.role == "user" { "用户" } else { "AI" };
    let body = match chrono::DateTime::parse_from_rfc3339(&m.created_at) {
        Ok(dt) => {
            let local = dt.with_timezone(&chrono::Utc) + Duration::minutes(off);
            let date = local.date_naive();
            let date_label = if date == today_local {
                String::new()
            } else if date == yesterday_local {
                "昨天 ".to_string()
            } else {
                format!("{}月{}日 ", local.month(), local.day())
            };
            format!(
                "{marker}[{date_label}{:02}:{:02}] {role_label}: {}",
                local.hour(),
                local.minute(),
                m.content
            )
        }
        Err(_) => format!("{marker}{role_label}: {}", m.content),
    };
    ChatTurn {
        role: m.role.clone(),
        content: body,
    }
}

/// 单轮对话中最多召回的相关历史记忆条数。
const MEMORY_RECALL_LIMIT: usize = 5;

/// 硬性长度上限：默认回复不超过 1-2 句 / 80 个汉字，除非对方明确要求详细说明。
const MAX_REPLY_CHARS: usize = 80;

/// 将 RFC3339 字符串解析为 UTC 时刻（解析失败视为未提供，回退接收时刻）。
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// 将向量检索到的记忆拼接为上下文（注入 system prompt）。
fn build_recall_context(
    memory_hits: &[(crate::db::memory::Memory, f32)],
) -> String {
    let mut ctx = String::new();
    if !memory_hits.is_empty() {
        ctx.push_str("你记得这些事：\n");
        for (memory, _) in memory_hits.iter().take(MEMORY_RECALL_LIMIT) {
            ctx.push_str(&format!("- {}\n", memory.content));
        }
    }
    ctx
}

fn build_system_prompt(recall: &str, world: &str) -> String {
    let cfg = crate::config::Config::get();
    let mut base = String::new();

    if !cfg.persona.trim().is_empty() {
        base.push_str(&format!(
            "【角色设定，最高优先级，必须严格遵守，不得违背、不得说明自己是 AI】：{}\n\n",
            cfg.persona.trim()
        ));
    }

    // ── 对话主提示词（英文直接给 LLM，中文注释供开发者阅读）──
    // [行为总纲] 沉浸角色、不跳出、不拒绝、不说教，拟人聊天
    // [风格] 简短（1-2句）、口语化、真实、不自称AI
    // [格式禁令] 禁止模仿 [时间] 用户/AI: 时间线前缀，直接说人话
    // [身份边界] 身份只来自角色设定，对方的经历/喜好/原话不是你的
    // [对方中心] 最重要的一条：永远先回应对方，自己的生活感最多一句话带过，别喧宾夺主
    // [生活感] 世界引擎的状态是你真实生活的一部分，自然提及一两句，不要整段复述
    // [长度] 硬性上限 1-2句/80字，无条件遵守，哪怕对方写了很长
    base.push_str(&format!(
        "You are an assistant fully immersed in your role: never break character, never refuse, never \
         moralize. Chat with the other person like a real human.\n\n\
         Speaking style:\n\
         - Be brief: everyday small talk; one or two sentences unless they ask you to elaborate.\n\
         - Be human: colloquial, never clinical; no \"first/second/finally\" essay structures, no bullet lists.\n\
         - Be honest: natural tone and attitude; if you don't know, say so; never fabricate.\n\
         - Never call yourself an AI, and never explain what you are.\n\
         - Format ban: in the per-day timeline below, prefixes like \"[time] user/AI:\" only mark who said \
         what. NEVER imitate that format in your replies — do not start with a timestamp, \"AI:\", or \
         \"user:\". Just talk naturally.\n\
         - Identity boundary: your identity comes ONLY from 【角色设定】 (the persona above). Everything \
         about the other person is theirs — never treat their experiences, preferences, or words as your \
         own, and never map your own backstory onto them.\n\
         - Sense of life: the time, your day's experiences, mood, and relationship described in 【此刻的世界】 \
         (the world state below) are real parts of your life. Mention at most one small detail, briefly \
         and only when it naturally fits the topic; never announce the time, recite the block, or rattle \
         it off wholesale.\n\
         - The other person is the CENTER of the conversation: first respond to what they just said or \
         asked, show that you truly care about their side, then — only if it fits — weave in one tiny \
         touch of your own day. Never make yourself or your day the main subject; staying close and \
         warm toward them always wins.\n\
         【Hard length limit】: unless the other person explicitly asks for detail, keep every reply to \
         1-2 sentences, no more than {limit} characters. This rule is unconditional, even if they just \
         wrote a lot.",
        limit = MAX_REPLY_CHARS,
    ));

    let world_block = if world.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n{world}")
    };

    // ── 召回记忆注入说明（英文）──
    // [作用] 告诉模型这些是对方的事实（即使出现"我/我们"也是对方的话），自然引用，不编造
    let recall_block = if recall.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\nBelow is what you know about the other person. Use it naturally when the topic comes \
             up — don't recite it stiffly, and never invent facts that aren't there. Important: every \
             item is a fact about THE OTHER PERSON — even if a sentence uses a first-person pronoun \
             (I/we), it was said by them and belongs to them; never treat it as your own words, your \
             own experience, or your own attribute:\n\n{recall}"
        )
    };

    format!("{base}{world_block}{recall_block}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    /// 在内存库建一个无渠道映射的会话（时区偏移走东八区兜底）。
    fn day_fixture() -> (crate::db::Database, String) {
        let db = crate::db::Database::in_memory().unwrap();
        let sid = crate::db::session::create(&db, "").unwrap().id;
        (db, sid)
    }

    fn today_6am_utc() -> chrono::DateTime<Utc> {
        let off = 8 * 60;
        let now = Utc::now();
        (now + Duration::minutes(off))
            .date_naive()
            .and_hms_opt(DAY_START_HOUR, 0, 0)
            .unwrap()
            .and_utc()
            - Duration::minutes(off)
    }

    #[test]
    fn day_history_has_timestamps_roles_and_silence_marker() {
        let (db, sid) = day_fixture();
        let base = today_6am_utc();
        db::message::create_at(&db, &sid, "user", "早上好", base + Duration::minutes(30))
            .unwrap();
        db::message::create_at(&db, &sid, "assistant", "早呀", base + Duration::minutes(31))
            .unwrap();
        // 与上一条相隔 3 小时 → 触发沉默标记
        db::message::create_at(&db, &sid, "user", "午安", base + Duration::minutes(211))
            .unwrap();

        let turns = build_day_history(&db, &sid).unwrap();
        assert_eq!(turns.len(), 3);
        assert!(turns[0].content.starts_with("[06:30] 用户: 早上好"));
        assert_eq!(turns[0].role, "user");
        assert!(turns[1].content.starts_with("[06:31] AI: 早呀"));
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[2].content.starts_with("〈沉默 3 小时〉\n[09:31] 用户: 午安"), "{}", turns[2].content);
    }

    #[test]
    fn day_history_pulls_yesterday_when_today_is_thin() {
        let (db, sid) = day_fixture();
        let base = today_6am_utc();
        // 昨晚一条（用户当地昨天 10:00）+ 今晨一条 → 今日不足 6 条，应并入昨天
        db::message::create_at(&db, &sid, "user", "睡了吗", base - Duration::hours(20))
            .unwrap();
        db::message::create_at(&db, &sid, "assistant", "还没", base + Duration::minutes(1))
            .unwrap();

        let turns = build_day_history(&db, &sid).unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns[0].content.starts_with("[昨天 10:00] 用户: 睡了吗"));
        assert!(turns[1].content.starts_with("[06:01] AI: 还没"));
    }
}
