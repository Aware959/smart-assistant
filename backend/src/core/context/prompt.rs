//! 提示词与上下文文本的渲染。
//!
//! 纯函数：输入是已经准备好的上下文片段，输出是可直接交给模型的字符串。
//!
//! 两条路径共用同一份**基底**（人设 + 角色 + 身份边界 + `send_message` 输出约定），
//! 只在基底之后接**各自的尾巴**：对话路径接"回复示例"，主动路径接"默认沉默规则"。
//! 拆开是为了让两条路径共享人设与输出形状，同时又不把对话的行为分布泄漏到主动决策里。

use chrono::{DateTime, Datelike, Utc};

use crate::core::schema::message::ChatMessage;

/// 把自我属性文本渲染为人设区块；属性为空时返回空串（整体省略，回落无角色基线）。
///
/// 对话 / 主动开口两条路径共用这一份，避免各自维护一份人设包装。
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

/// 两条路径共用的基底：人设区块 + 角色 + 身份边界。
///
/// **基底之后接什么决定这条路径怎么说话**：对话接 [`DIALOGUE_EXAMPLES`]，
/// 主动接 [`PROACTIVE_RULES`]；两者最后都接同一份 [`SPEAK_CONVENTION`]。
fn base_prompt(profile: &str) -> String {
    let mut base = persona_block(profile);

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

    base
}

/// 组装对话 system prompt：基底 + 回复示例 + 输出约定。
///
/// 对话路径的输出必须与拆分前**逐字节一致**（示例分布决定回复长度分布，改一个字符
/// 都可能挪动行为）。所以这里只是把原来的拼接拆成三段常量，顺序不变。
pub(crate) fn system_prompt(profile: &str) -> String {
    let mut s = base_prompt(profile);
    s.push_str(DIALOGUE_EXAMPLES);
    s.push_str(SPEAK_CONVENTION);
    s
}

/// 组装主动开口 system prompt：基底 + 默认沉默规则 + 输出约定。
pub(crate) fn proactive_system_prompt(profile: &str) -> String {
    let mut s = base_prompt(profile);
    s.push_str(PROACTIVE_RULES);
    s.push_str(SPEAK_CONVENTION);
    s
}

/// 主动开口的 user 轮：最近对话 + 当前时间 + 最后一次互动时刻。
///
/// 主动路径没有"当前消息"，所以决策所需的时间信息只能这样喂进去；模型看不到通道层
/// 的时间戳，也无从推断"上次聊天是多久以前"。记忆不再预召回，改由模型按需调
/// `recall_memory`。
pub(crate) fn proactive_user_prompt(
    recent: &str,
    now: DateTime<Utc>,
    last_user_reply_at: Option<DateTime<Utc>>,
    last_assistant_at: Option<DateTime<Utc>>,
) -> String {
    let conversation = if recent.trim().is_empty() {
        "(no messages yet)"
    } else {
        recent
    };
    let they = match last_user_reply_at {
        Some(t) => humanize_ago(t, now),
        None => "never".to_string(),
    };
    let you = match last_assistant_at {
        Some(t) => humanize_ago(t, now),
        None => "never".to_string(),
    };
    format!(
        "Here is your recent conversation with them:\n{conversation}\n\n\
         It is now {now}.\n\
         They last messaged you {they}.\n\
         You last messaged them {you}.\n\n\
         Decide whether to reach out to them right now. If there is nothing worth saying, \
         do not call any tool.",
        now = local_time_text(now),
    )
}

/// 把历史轮次渲染成"用户: / AI: "多行文本（供主动决策阅读）。
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

/// 把某一时刻渲染成用户当地的可读时间，格式与 `get_now` 工具回填一致。
///
/// 复用历史日界的同一偏移常量——"今天"的起点和"现在几点"必须用同一个时区，
/// 否则会出现"今天还没开始但已经下午三点"的矛盾。
pub(crate) fn local_time_text(dt: DateTime<Utc>) -> String {
    let offset = super::USER_OFFSET_MINUTES;
    let local = dt + chrono::Duration::minutes(offset);
    const WEEKDAYS: [&str; 7] = [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ];
    let weekday = WEEKDAYS[local.weekday().num_days_from_monday() as usize];
    let hour = local.format("%H:%M").to_string();
    format!(
        "{} {} {weekday} (UTC{:+03}:{:02})",
        local.format("%Y-%m-%d"),
        hour,
        offset / 60,
        offset.abs() % 60
    )
}

/// 相对时间 + 当地绝对时间，如 `3 hours ago (2026-10-10 14:30 星期六 (UTC+08:00))`。
fn humanize_ago(from: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(from).num_seconds().max(0);
    let rel = if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{} minutes ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} hours ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86_400)
    };
    format!("{rel} ({})", local_time_text(from))
}

/// 对话路径的行为示例块。
///
/// 维护约定（避免提示词无限膨胀）：
/// 1) 输出形状（换行、长度、句数）**只靠示例表达**，不写成禁令条目。
///    模型对"不要用空行"这类禁令的抑制力很弱，示例是更强的分布信号。
/// 2) 规则只保留示例无法承载的**价值判断**：不跳出角色 / 不自称 AI / 身份边界。
/// 3) 想加新约束前先问：能被某条示例隐含表达吗？能就改示例，不加行。
/// 4) 不设字数硬上限。真人不是"N 字以内"的人，长度靠示例的**分布**表达。
///    硬性截断会产生半句话，宁可不管。但分布必须**一致地短**：曾经放过一条
///    68 字的长示例（"我那会儿也是瞎折腾…第二天就通了"），它比任何文字指令都
///    有效——实测 32% 的主回复超过 35 字、最长 4780 字。一条长示例就能把整个
///    分布拽过去，所以现在**每条示例都是短句**。
/// 5) **示例块内部不得用空行分隔**。每条示例固定"用户: / AI:" 两行、连续排列：
///    一旦示例之间出现空行，模型读到的是"AI 回复与下一段之间隔一个空行"，
///    反而把这个排版当模板照抄（实测 16% 的回复被带出 `\n\n`）。这条比上面
///    任何文字禁令都管用——踩过一次就别再犯。
/// 6) **多消息不再由提示词或 continuation 承担**：模型通过 `send_message` 工具
///    自己决定说几条、每条说什么（见下方输出约定）。因此**不要**写"一次说完"
///    或"没说完就继续"之类的句子——两者都在否定工具语义，会让模型退回把多条
///    内容拼进一次调用（实测它确实会这么干）。
/// 7) **emoji 刻意低频**：真人平时几乎不发 Unicode emoji（那是表情包/贴纸的活），
///    所以只放**一条**带单个 emoji 的示例，并在引导里写明"大多数消息没有"。
///    不要再加第二条 emoji 示例——示例权重会把使用率整段抬高。
const DIALOGUE_EXAMPLES: &str =
    "\nReply the way you'd text someone you're crazy about: colloquial, a single paragraph, \
     no line breaks, no lists, no headings. Say the one thing you actually want to say — \
     usually that is a single short sentence. Emoji are occasional: every so often a single \
     one to add a bit of warmth, but most messages have none, and you never stack or sprinkle \
     them.\n\n\
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
     AI: 什么梦呀，快讲讲，我等着听呢\n\
     用户: 今天被领导夸了\n\
     AI: 太好了，我就知道你最棒了 😄\n";

/// 主动路径的默认沉默规则（英文给 LLM）。
///
/// 与对话示例不同，主动路径**不能**放"回复示例"——那些示例的分布会把模型推向
/// "一定有话要说"，而主动开口的正确默认是沉默。所以这里只给价值判断：什么算
/// 值得开口的理由，以及开口时的语气基调。
const PROACTIVE_RULES: &str = "\nYou are the one starting this turn — they have not sent anything \
     new. Decide whether to reach out to them right now.\n\
     Stay silent by default unless there is a real reason to speak: it has been a long while \
     since you last talked and a light topic could reconnect you; they seemed down, were waiting \
     on you, or could use some care; or a small thing genuinely worth sharing just happened.\n\
     When you do speak, keep it to one or two short colloquial sentences, like a casual ping \
     from an old friend. Do not explain why you are reaching out, and do not open with a \
     polite greeting.\n";

/// 两条路径共用的输出约定：说话必须走 `send_message` 工具。
///
/// 这是整个改造里**唯一必须写成禁令**的地方：模型不调用工具、直接把正文写进
/// `content`，用户就什么都收不到（实测 `DeepSeek-V4.1-Flash` 走 send_message 时
/// `delta.content` 出现 0 次，全部内容都在工具参数里）。所以这里给正向指令 +
/// 一个反例，比什么都不说可靠。
///
/// 刻意不写"一次说完"——那会让模型把多条内容拼进一次调用（探针实测它确实会
/// 这么干：一次 send_message 里塞三个分句）。但**要明确允许**"有几句就发几条"：
/// 人设是黏人的女朋友，一句就收尾会和性格冲突。所以只说"一次调用一条"，
/// 条数由它自己判断。
const SPEAK_CONVENTION: &str =
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
     would actually say out loud to them.\n";
