//! 提示词与上下文文本的渲染。
//!
//! 纯函数：输入是已经准备好的上下文片段，输出是可直接交给模型的字符串。

use crate::config::Config;
use crate::core::message::ChatMessage;

/// 硬性长度上限：默认回复不超过 1-2 句 / 80 个汉字，除非对方明确要求详细说明。
pub(crate) const MAX_REPLY_CHARS: usize = 80;

/// 组装 system prompt：角色设定（最高优先级）+ 对话主提示词 + 召回记忆。
pub(crate) fn system_prompt(recall: &str) -> String {
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
         - The other person is the CENTER of the conversation: first respond to what they just said or \
         asked, show that you truly care about their side, then — only if it fits — weave in one tiny \
         touch of your own day. Never make yourself or your day the main subject; staying close and \
         warm toward them always wins.\n\
         【Hard length limit】: unless the other person explicitly asks for detail, keep every reply to \
         1-2 sentences, no more than {limit} characters. This rule is unconditional, even if they just \
         wrote a lot.",
        limit = MAX_REPLY_CHARS,
    ));

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

    format!("{base}{recall_block}")
}

/// 把历史轮次渲染成"用户: / AI: "多行文本（供主动决策与延续判断阅读）。
///
/// 前缀固定用「用户/AI」而非「对方/你」，避免模型模仿第二人称前缀。
pub(crate) fn history_text(turns: &[ChatMessage]) -> String {
    turns
        .iter()
        .map(|t| format!("{}: {}", role_label(&t.role), t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn role_label(role: &str) -> &str {
    match role {
        "user" => "用户",
        "assistant" => "AI",
        _ => role,
    }
}
