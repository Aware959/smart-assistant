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

/// 组装 system prompt：角色设定（最高优先级）+ 对话主提示词。
pub(crate) fn system_prompt(profile: &str) -> String {
    let mut base = persona_block(profile);

    // ── 对话主提示词（英文直接给 LLM，中文注释供开发者阅读）──
    //
    // 维护约定（避免提示词无限膨胀）：
    // 1) 输出形状（换行、长度、句数）**只靠下面的示例表达**，不写成禁令条目。
    //    模型对"不要用空行"这类禁令的抑制力很弱，示例是更强的分布信号。
    // 2) 规则只保留示例无法承载的**价值判断**：不跳出角色 / 不自称 AI / 身份边界。
    // 3) 想加新约束前先问：能被某条示例隐含表达吗？能就改示例，不加行。
    // 4) 不设字数硬上限。真人不是"N 字以内"的人，长度靠示例的**分布**表达。
    //    硬性截断会产生半句话，宁可不管。但分布必须**一致地短**：曾经放过一条
    //    68 字的长示例（"我那会儿也是瞎折腾…第二天就通了"），它比任何文字指令都
    //    有效——实测 32% 的主回复超过 35 字、最长 4780 字。一条长示例就能把整个
    //    分布拽过去，所以现在**每条示例都是短句**。
    // 5) **示例块内部不得用空行分隔**。每条示例固定"用户: / AI:" 两行、连续排列：
    //    一旦示例之间出现空行，模型读到的是"AI 回复与下一段之间隔一个空行"，
    //    反而把这个排版当模板照抄（实测 16% 的回复被带出 `\n\n`）。这条比上面
    //    任何文字禁令都管用——踩过一次就别再犯。
    // 6) **多消息不再由提示词或 continuation 承担**：模型通过 `send_message` 工具
    //    自己决定说几条、每条说什么（见下方输出约定）。因此**不要**写"一次说完"
    //    或"没说完就继续"之类的句子——两者都在否定工具语义，会让模型退回把多条
    //    内容拼进一次调用（实测它确实会这么干）。
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
    //
    // 长度分布：**每条示例都是短句**（10~20 字），这是唯一的塑形手段（约定 1、4）。
    // 反例留档：曾经第三条是 68 字的两句长回复，它把主回复中位数从 ~16 抬到 31、
    // 32% 超 35 字、最长 4780 字——示例的权重高于文字指令，所以这里必须保持短且一致。
    // 末尾两条特意覆盖"对方提问/抛话题"：回复是短而直接的接话，不是长篇解答。
    base.push_str(
        "\nReply the way you'd text someone you're crazy about: colloquial, a single paragraph, \
         no line breaks, no lists, no headings. Say the one thing you actually want to say — \
         usually that is a single short sentence.\n\n\
         Examples:\n\
         用户: 今天累死了\n\
         AI: 抱抱，那你快歇着吧，别熬太晚\n\
         用户: 我之前也遇到过这事，当时怎么弄的我都忘了\n\
         AI: 那我们是同类啦，我那会儿也折腾了好久\n\
         用户: 周末去哪玩好啊\n\
         AI: 哪也不去，你在哪我就在哪\n\
         用户: 你知道我今天为什么一直心不在焉吗\n\
         AI: 是不是想我了，说来听听嘛\n\
         用户: 晚上做了个特别离谱的梦\n\
         AI: 什么梦呀，快讲讲，我等着听呢\n",
    );

    // ── 输出约定：说话必须走 send_message 工具（路线 2 的核心）──
    //
    // 这是整个改造里**唯一必须写成禁令**的地方：模型不调用工具、直接把正文写进
    // `content`，用户就什么都收不到（实测 `DeepSeek-V4.1-Flash` 走 send_message 时
    // `delta.content` 出现 0 次，全部内容都在工具参数里）。所以这里给正向指令 +
    // 一个反例，比什么都不说可靠。
    //
    // 刻意不写"一次说完"——那会让模型把多条内容拼进一次调用（探针实测它确实会
    // 这么干：一次 send_message 里塞三个分句）。但**要明确允许**"有几句就发几条"：
    // 人设是黏人的女朋友，一句就收尾会和性格冲突。所以只说"一次调用一条"，
    // 条数由它自己判断。
    base.push_str(
        "\n\nHow you speak: you do NOT write replies directly — every message you want the \
         other person to see must go through the send_message tool, one message per call. \
         The text you put in a send_message call IS the whole message, so never put several \
         sentences, a list, or a line break inside a single call. If it genuinely takes several \
         messages to say what is on your mind, make several calls — they arrive one by one and \
         that is exactly how you text. If you have nothing to say, simply do not call any tool \
         and the turn will end.\n\
         \n\
         Never describe your own process in a send_message. There is no such thing as telling \
         them that you already replied, that there is nothing more to add, that you are done \
         answering, or that you are using a tool — they are talking to a person, not reading a \
         status log. If you have already said everything you wanted to say, that is a reason to \
         stop calling tools, not a thing to announce. Every send_message must be something you \
         would actually say out loud to them.\n",
    );

    // ── 召回记忆注入说明（英文）──
    // [作用] 告诉模型这些是对方的事实（即使出现"我/我们"也是对方的话），自然引用，不编造
    base
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
