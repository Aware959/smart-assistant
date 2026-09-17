//! 提示词组装：把历史消息、记忆召回、世界状态拼成注入模型的上下文。
//!
//! 纯内核逻辑，只消费可移植值类型（[`crate::core::types`]），不感知任何能力实现；
//! 历史消息与时区偏移等由调用方（内核流水线，经端口）准备好后传入。

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::core::types::{ChatTurn, MemoryHit, MessageTurn};
use crate::error::Result;

/// 一天的起点钟点（用户当地时钟）：这个钟点之前算昨天、之后算今天。
const DAY_START_HOUR: u32 = 6;
/// 今日消息少于该条数时，把昨天也并入上下文（人记得昨晚的事）。
pub(crate) const DAY_POOL_THRESHOLD: usize = 6;
/// 按天上下文的最大消息条数；超出时保留开头与结尾、中间整段省略。
const DAY_CONTEXT_MAX: usize = 60;
/// 压缩时保留的头部条数（保住今天早晨的开场）。
const DAY_KEEP_HEAD: usize = 8;

/// 单轮对话中最多召回的相关历史记忆条数。
pub(crate) const MEMORY_RECALL_LIMIT: usize = 5;

/// 硬性长度上限：默认回复不超过 1-2 句 / 80 个汉字，除非对方明确要求详细说明。
pub(crate) const MAX_REPLY_CHARS: usize = 80;

/// 按用户当地时钟计算"今天"的起点（日界 = 当地 06:00，世界时 UTC 时刻）。
pub(crate) fn day_window_start(offset_minutes: i64) -> DateTime<chrono::Utc> {
    (Utc::now() + Duration::minutes(offset_minutes))
        .date_naive()
        .and_hms_opt(DAY_START_HOUR, 0, 0)
        .expect("valid clock time")
        .and_utc()
        - Duration::minutes(offset_minutes)
}

/// 按"今天"把历史消息构建成对话上下文（参考人的作息与生物节律）：
/// - 今日消息过少（< [`DAY_POOL_THRESHOLD`]）时并入昨天——由调用方先取昨日序列再传入。
/// - 入场条数超上限时做骨架压缩（保留开头与结尾、省略中间），控制单轮上下文长度。
/// - 说话人靠消息本身的 role 一并传给模型，内容保持原文、不做任何装饰——时间感知交给
///   「此刻的世界」快照承担，避免上下文里出现可被模型模仿的时间/角色前缀。
pub(crate) fn build_day_history(turns: &[MessageTurn]) -> Result<Vec<ChatTurn>> {
    let total = turns.len();
    let compact = total > DAY_CONTEXT_MAX;
    let tail_start = total.saturating_sub(DAY_CONTEXT_MAX - DAY_KEEP_HEAD);

    let mut out = Vec::with_capacity(total.min(DAY_CONTEXT_MAX));
    for (idx, m) in turns.iter().enumerate() {
        if compact && idx >= DAY_KEEP_HEAD && idx < tail_start {
            continue;
        }
        out.push(ChatTurn {
            role: m.role.clone(),
            content: m.content.clone(),
        });
    }
    Ok(out)
}

/// 将向量检索到的记忆拼接为上下文（注入 system prompt）。
pub(crate) fn build_recall_context(memory_hits: &[MemoryHit]) -> String {
    let mut ctx = String::new();
    if !memory_hits.is_empty() {
        ctx.push_str("你记得这些事：\n");
        for hit in memory_hits.iter().take(MEMORY_RECALL_LIMIT) {
            ctx.push_str(&format!("- {}\n", hit.content));
        }
    }
    ctx
}

/// 组装 system prompt：角色设定（最高优先级）+ 对话主提示词 + 世界状态 + 召回记忆。
pub(crate) fn build_system_prompt(recall: &str, world: &str) -> String {
    let cfg = Config::get();
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
    // [格式禁令] 开头禁止时间戳/角色前缀（如 [09:31] AI:），直接说人话
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
         - Format ban: never open a reply with a timestamp, a time bracket like \"[09:31]\", or a role \
         prefix like \"AI:\" or \"user:\". Just talk naturally.\n\
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
    use chrono::{DateTime, Timelike};
    use crate::core::types::MessageTurn;

    const EAST8_OFFSET: i64 = 8 * 60;

    fn turn(role: &str, content: &str, created_at: &str) -> MessageTurn {
        MessageTurn {
            role: role.to_string(),
            content: content.to_string(),
            created_at: created_at.to_string(),
        }
    }

    fn today_6am_utc() -> DateTime<Utc> {
        let now = Utc::now();
        (now + Duration::minutes(EAST8_OFFSET))
            .date_naive()
            .and_hms_opt(DAY_START_HOUR, 0, 0)
            .unwrap()
            .and_utc()
            - Duration::minutes(EAST8_OFFSET)
    }

    fn rfc(dt: DateTime<Utc>) -> String {
        dt.to_rfc3339()
    }

    #[test]
    fn day_history_preserves_raw_content_and_native_roles() {
        let base = today_6am_utc();
        let turns = vec![
            turn("user", "早上好", &rfc(base + Duration::minutes(30))),
            turn("assistant", "早呀", &rfc(base + Duration::minutes(31))),
            turn("user", "午安", &rfc(base + Duration::minutes(211))),
        ];

        let out = build_day_history(&turns).unwrap();
        assert_eq!(out.len(), 3);
        // 说话人只靠原生 role 表达，内容保持原文，不带任何时间戳/角色前缀装饰。
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content, "早上好");
        assert_eq!(out[1].role, "assistant");
        assert_eq!(out[1].content, "早呀");
        assert_eq!(out[2].role, "user");
        assert_eq!(out[2].content, "午安");
    }

    #[test]
    fn day_history_compacts_middle_when_over_capacity() {
        let base = today_6am_utc();
        let turns = (0..(DAY_CONTEXT_MAX + 10))
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                turn(role, &format!("消息{i}"), &rfc(base + Duration::minutes(i as i64)))
            })
            .collect::<Vec<_>>();
        let tail_start = turns.len() - (DAY_CONTEXT_MAX - DAY_KEEP_HEAD);

        let out = build_day_history(&turns).unwrap();
        assert_eq!(out.len(), DAY_CONTEXT_MAX);
        // 保留开头与结尾，中间整段省略。
        assert_eq!(out[0].content, "消息0");
        assert_eq!(out[1].content, "消息1");
        assert_eq!(out[DAY_KEEP_HEAD].content, format!("消息{tail_start}"));
        assert_eq!(out.last().unwrap().content, format!("消息{}", turns.len() - 1));
    }

    #[test]
    fn day_window_start_is_local_6am_utc_converted() {
        let start = day_window_start(EAST8_OFFSET);
        // 世界时应等于"当地 06:00 的时刻减 8 小时"。
        let local = start + Duration::minutes(EAST8_OFFSET);
        assert_eq!(local.hour(), DAY_START_HOUR);
        assert_eq!(local.minute(), 0);
    }
}