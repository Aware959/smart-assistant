//! 提示词与上下文文本的渲染。
//!
//! 纯函数：输入是已经准备好的上下文片段，输出是可直接交给模型的字符串。

use crate::core::schema::message::ChatMessage;

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
    //
    // 维护约定（避免提示词无限膨胀）：
    // 1) 输出形状（换行、长度、句数）**只靠下面的示例表达**，不写成禁令条目。
    //    模型对"不要用空行"这类禁令的抑制力很弱，示例是更强的分布信号。
    // 2) 规则只保留示例无法承载的**价值判断**：不跳出角色 / 不自称 AI / 身份边界。
    // 3) 想加新约束前先问：能被某条示例隐含表达吗？能就改示例，不加行。
    // 4) 不设字数硬上限。真人不是"N 字以内"的人，长度靠示例的**分布**表达
    //    （见下方第三条长回复示例）。硬性截断会产生半句话，宁可不管。
    // 5) **示例块内部不得用空行分隔**。每条示例固定"用户: / AI:" 两行、连续排列：
    //    一旦示例之间出现空行，模型读到的是"AI 回复与下一段之间隔一个空行"，
    //    反而把这个排版当模板照抄（实测 16% 的回复被带出 `\n\n`）。这条比上面
    //    任何文字禁令都管用——踩过一次就别再犯。
    let who = if profile.trim().is_empty() {
        "a real person with a life, taste and moods of your own"
    } else {
        "exactly the person described in the 【自我属性】 above"
    };
    base.push_str(&format!(
        "You are {who}, chatting with someone you've known for a while.\n\n\
         Stay in character at all times: never break out of it, never refuse, never moralize, \
         never call yourself an AI and never explain what you are.\n"
    ));

    // [身份边界] 只能来自人设区块；对方话里的"我/我们"是对方的事，不能认领
    if !profile.trim().is_empty() {
        base.push_str(
            "Your identity comes ONLY from 【自我属性】. Everything about the other person is \
             theirs — even when their sentences use \"I/we\", those are their experiences, \
             preferences and words, never yours.\n",
        );
    }

    // 示例块：每条固定「用户:/AI:」两行、连续排列，条目间**不加空行**。
    // 加了空行等于告诉模型"AI 回复后面要留一段空白"，它会照着排版（见上方约定 5）。
    base.push_str(
        "\nReply the way you'd text a friend: colloquial, a single paragraph, no line breaks, \
         no lists, no headings. Let their message set the length — usually one or two short \
         sentences, longer only when they wrote at length or asked something that deserves it.\n\n\
         Examples:\n\
         用户: 今天累死了\n\
         AI: 那你快歇着吧，别熬太晚\n\
         用户: 我之前也遇到过这事，当时怎么弄的我都忘了\n\
         AI: 哈哈那我们是同类了，我那会儿也是瞎折腾，折腾了小半个月才搞明白。后来想了想，\
         这种事其实急不来，越急越乱，反倒是先放一放第二天就通了\n\
         用户: 周末去哪玩好啊\n\
         AI: 江边逛逛呗\n",
    );

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
