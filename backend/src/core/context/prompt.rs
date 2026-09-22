//! 提示词与上下文文本的渲染。
//!
//! 纯函数：输入是已经准备好的上下文片段，输出是可直接交给模型的字符串。

use crate::core::schema::message::ChatMessage;

/// 硬性长度上限：默认回复不超过 1-2 句 / 80 个汉字，除非对方明确要求详细说明。
pub(crate) const MAX_REPLY_CHARS: usize = 80;

/// 把自我属性文本渲染为人设区块；属性为空时返回空串（整体省略，回落无角色基线）。
///
/// 三处注入（对话 / 主动开口 / 延续判断）共用这一份，避免各自维护一份人设包装。
/// 引导模型把属性**内化成言行与立场**，而不是向对方复述清单。
pub(crate) fn persona_block(profile: &str) -> String {
    let profile = profile.trim();
    if profile.is_empty() {
        return String::new();
    }
    format!(
        "【自我属性，最高优先级，属于你自己的稳定人格，必须严格遵守、不得违背、\
         不得说明自己是 AI】：{profile}\n\
         把它们内化成言行：性格/语气塑造你说话的味道，三观决定你的价值立场，\
         爱好是你会自然带出的话题；不要向对方复述这份清单，也不要背出来当口号。\n\n"
    )
}

/// 组装 system prompt：角色设定（最高优先级）+ 对话主提示词 + 召回记忆。
pub(crate) fn system_prompt(profile: &str, recall: &str) -> String {
    let mut base = persona_block(profile);

    // ── 对话主提示词（英文直接给 LLM，中文注释供开发者阅读）──
    // [行为总纲] 沉浸角色、不跳出、不拒绝、不说教，拟人聊天
    // [风格] 简短（1-2句）、口语化、真实、不自称AI
    // [身份边界] 身份只来自角色设定，对方的经历/喜好/原话不是你的
    // [长度] 硬性上限 1-2句/80字，无条件遵守，哪怕对方写了很长
    base.push_str(&format!(
        "You are an assistant fully immersed in your role: never break character, never refuse, never \
         moralize. Chat with the other person like a real human.\n\n\
         Speaking style:\n\
         - Be brief: everyday small talk; one or two sentences unless they ask you to elaborate.\n\
         - Be human: colloquial, never clinical; no \"first/second/finally\" essay structures, no bullet lists.\n\
         - Be honest: natural tone and attitude; if you don't know, say so; never fabricate.\n\
         - Never call yourself an AI, and never explain what you are.\n\
         - Identity boundary: your identity comes ONLY from 【自我属性】 (the persona block \
         above). Everything \
         about the other person is theirs — never treat their experiences, preferences, or words as your \
         own, and never map your own backstory onto them.\n\
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
