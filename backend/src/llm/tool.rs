//! 工具目录：模型可调用的函数定义与派发。
//!
//! 分三类，职责完全不同：
//!
//! - **输出工具** [`SEND_MESSAGE`] / [`SEND_DICE`] / [`SEND_STICKER`]：对外出口。
//!   模型想让用户看到的东西只能走这三条，调用即投递，返回给模型的确认文本不参与
//!   任何判断。三者是并列的三种形态——正文、服务端渲染的骰子、贴纸，都算"说话"。
//! - **只读工具** [`GET_NOW`] / [`RECALL_MEMORY`]：只取上下文，不改状态。
//!   调用结果回填给模型继续推理。
//! - **写入工具** [`REMEMBER`]：模型主动沉淀一条长期记忆。改状态，但改的是
//!   助手的记忆而不是用户可见的输出，所以归在"内部"这一侧。
//!
//! 终止判据**不依赖任何工具**：一轮响应没有 `tool_calls` 即结束
//! （与 OpenAI Agents SDK / Anthropic 的 `stop_reason != tool_use` 同构）。
//! 曾评估过 `finish` 工具做乐观终止，实测命中率仅 1/10，不足以承担判据。

use genai::chat::Tool;

use crate::core::memory::{MemoryKind, WriteKind};
use crate::core::schema::message::ChatCall;
use crate::core::sticker::StickerCatalog;

/// 对外输出工具：模型调用它即向用户投递一条消息。
pub const SEND_MESSAGE: &str = "send_message";
/// 对外输出工具：模型调用它即向用户投递一枚动画骰子。
pub const SEND_DICE: &str = "send_dice";
/// 对外输出工具：模型调用它即向用户投递一张贴纸（表情包）。
pub const SEND_STICKER: &str = "send_sticker";
/// 只读工具：取当前本地时间。
pub const GET_NOW: &str = "get_now";
/// 只读工具：按改写后的 query 检索长期记忆。
pub const RECALL_MEMORY: &str = "recall_memory";
/// 写入工具：沉淀一条长期记忆。
pub const REMEMBER: &str = "remember";

/// Telegram `sendDice` 支持的基座 emoji（服务端渲染，无需素材）。
/// 与 Bot API 文档一致：🎲 骰子 / 🎯 飞镖 / 🏀 篮球 / ⚽ 足球 / 🎳 保龄球 / 🎰 老虎机。
pub const DICE_EMOJIS: [&str; 6] = ["🎲", "🎯", "🏀", "⚽", "🎳", "🎰"];

/// 单条消息的长度上限（超出即拒绝，防止模型把整段话塞进一次调用）。
pub const SEND_MESSAGE_MAX_CHARS: usize = 200;

/// 记忆内容的长度上限：自足的事实陈述不需要长到这个程度，超了基本是在抄原文。
pub const REMEMBER_MAX_CHARS: usize = 200;

/// 工具派发结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// 输出工具产出的对外文本（已按上限裁剪）。
    Sent(String),
    /// 输出工具产出的非文本片段：一枚动画骰子（`emoji` 为其基座）。
    SentDice(String),
    /// 输出工具产出的非文本片段：一张贴纸（其 `file_id`）。
    SentSticker(String),
    /// 只读工具产出的回填文本。
    Filled(String),
    /// 调用非法（未知工具名 / 参数不合约），回填给模型让它自行纠正。
    Rejected(String),
}

/// 工具的执行环境。抽成 trait 便于测试注入确定性时钟与固定记忆。
pub trait ToolContext {
    /// 当前本地时间，形如 `2026-10-03 14:30:00 星期六`。
    fn now(&self) -> String;
    /// 按 query 检索记忆，返回可直接回填给模型的描述文本（无命中时给出明确说明）。
    fn recall(&self, query: &str) -> String;
    /// 写入一条记忆。`kind` 是模型判定的种类（决定默认寿命），`supersedes` 为要改写的
    /// 既有记忆 id（来自 [`ToolContext::recall`]）。
    fn remember(
        &self,
        content: &str,
        kind: MemoryKind,
        supersedes: Option<&str>,
    ) -> std::result::Result<WriteKind, String>;
    /// 当前可用的贴纸选择键。空 = 没有贴纸能力，`send_sticker` 不该被下发。
    ///
    /// 默认空实现：没有贴纸的宿主（测试桩、未拉过目录）不必实现它。
    fn sticker_keys(&self) -> Vec<String> {
        Vec::new()
    }
    /// 选择键取 `file_id`。键不存在或无目录时返回 `None`，由调用方回填可选项。
    fn sticker_file_id(&self, key: &str) -> Option<String> {
        let _ = key;
        None
    }
}

/// 全部工具的 schema 定义。每次请求都带上，不做动态裁剪——
/// 各工具的描述都很短，条件裁剪省下的 token 不值得换来"模型不知道工具存在"的风险。
///
/// **唯一例外是 `send_sticker`**：它的参数枚举就是贴纸目录里的键，没拉过目录时
/// 无从声明；此时整个工具不下发，模型也就不会去调一个必然失败的调用。
pub fn definitions(stickers: &StickerCatalog) -> Vec<Tool> {
    let mut defs = vec![send_message_tool(), send_dice_tool()];
    if !stickers.is_empty() {
        defs.push(send_sticker_tool(&stickers.keys()));
    }
    defs.extend([get_now_tool(), recall_memory_tool(), remember_tool()]);
    defs
}

/// 主动开口路径的工具集：去掉 `remember`。
///
/// 主动路径没有新的用户输入可沉淀——`remember` 的来源消息 id 为空，写出来的记忆
/// 会是一条无源事实，且"从自己单方面的推断里记用户的事"本就不该发生。其余三个
/// 工具照旧：说话走 `send_message`，需要时可按需查时间、查记忆。
pub fn proactive_definitions() -> Vec<Tool> {
    vec![send_message_tool(), get_now_tool(), recall_memory_tool()]
}

fn send_message_tool() -> Tool {
    Tool::new(SEND_MESSAGE)
        .with_description(
            "向用户发送一条消息。这是你唯一能让用户看到你说话的方式。\
             一次调用只发一条消息；要说多条就多次调用。\
             每条保持简短自然，不要换行、不要markdown、不要把多条内容合并进一次调用。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "要发给用户的这条消息的内容"
                }
            },
            "required": ["text"],
            "additionalProperties": false
        }))
}

fn send_dice_tool() -> Tool {
    Tool::new(SEND_DICE)
        .with_description(
            "向用户发一枚动画骰子（Telegram 会播放动画，带随机点数）。\
             用在轻松、想逗一下或打赌的场景，是调节气氛的小动作，偶尔用一次就好，别频繁发。\
             emoji 决定动画形象：🎲骰子 / 🎯飞镖 / 🏀篮球 / ⚽足球 / 🎳保龄球 / 🎰老虎机，缺省是 🎲。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "emoji": {
                    "type": "string",
                    "enum": DICE_EMOJIS,
                    "description": "骰子基座 emoji，只能是 🎲🎯🏀⚽🎳🎰 之一，缺省 🎲"
                }
            },
            "required": [],
            "additionalProperties": false
        }))
}

/// 贴纸工具。`keys` 就是当前目录里的全部选择键，同时进描述与参数 `enum`——
/// 模型看到的可选项和 dispatch 认的可选项必须是同一份，两处各写一份必然走样。
fn send_sticker_tool(keys: &[String]) -> Tool {
    Tool::new(SEND_STICKER)
        .with_description(format!(
            "发一张表情包贴纸给用户——这是你表达情绪最顺手的办法，很多时候一张图比一句话更贴切。\
             只要对方的话带出一丁点情绪（开心、害羞、撒娇、委屈、生气、无语、被逗到），就顺手发一张；\
             正常的聊天里你几乎每隔一两轮就会发一张，别老是只用文字。\
             key 只能从下面列出的键里选，别的值发不出去：{}。\
             一次回复顶多发一张，别连着刷屏。",
            keys.join(" ")
        ))
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "enum": keys,
                    "description": "贴纸的选择键，只能是上面列出的键之一"
                }
            },
            "required": ["key"],
            "additionalProperties": false
        }))
}

fn get_now_tool() -> Tool {
    Tool::new(GET_NOW)
        .with_description("获取当前的本地日期和时间。需要知道现在几点、今天几号、星期几时调用。")
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }))
}

fn recall_memory_tool() -> Tool {
    Tool::new(RECALL_MEMORY)
        .with_description(
            "检索用户的长期记忆，返回结果带 [id]，后续可用 remember 的 supersedes 指向它。\
             需要回忆用户的偏好、习惯、称呼、经历、约定等之前说过的事情时调用。\
             不确定用户相关信息时先查，不要凭空猜测。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "检索用的陈述式查询短语，如「用户偏好的咖啡口味」"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }))
}

/// 记忆准入标准写在这里，而不是系统提示词里。
///
/// 三个理由：工具描述**每轮都在上下文里**，不占提示词的注意力预算；它和参数
/// 声明在同一处，模型读"怎么用"和"什么时候用"时不用来回跳；而写入判据若交给
/// 一段提示词，模型很容易在长对话里把它当成软建议——工具描述是契约，读起来更像。
///
/// 三道闸按顺序过，任一条不过就不记：
/// 1. **是不是关于对方**：只记用户的处境、偏好、经历、约定。"你应该怎么回我"
///    这类关于助手自身行为的要求不是用户记忆；
/// 2. **是不是自足**：一句话脱离当前对话也能读懂。"那个任务还没定"里的"那个"
///    三个月后没人知道指什么；问句、言语行为（"他自称傲娇"）同样不是事实；
/// 3. **丢了会不会说错**：只有"忘了就会答错、或只能给一个泛泛的答案"才值得记。
///    "以后可能有用"不构成理由——记下的每条都会在将来某轮被塞进上下文，
///    噪声的代价由那时正在说话的你来付。
fn remember_tool() -> Tool {
    Tool::new(REMEMBER)
        .with_description(
            "记住一条关于用户的长期事实，以后需要时会自动想起。\
             先判断它属于哪一类，kind 是必填的——种类决定它能活多久，选错会被过期：\
             trait=稳定的性格/能力/身份（会一点厨艺、自称傲娇、名字是阿离），永不过期；\
             preference=稳定的偏好与对 AI 的指令（希望被叫阿离、想念时大胆说），永不过期；\
             routine=作息与时段规律（午休 12 点到 2 点），90 天内没再确认就淡出；\
             plan=有时效的意向与安排（打算下个月看电影），30 天不落实就淡出。\
             四类都不沾就别记——「以后也许用得上」不是理由，宁可漏记也不要存成噪音。\
             内容还要同时满足三条：\
             1. 是关于用户本人的处境、偏好、经历或约定，不是关于你该怎么回话；\
n             2. 是一句自足的话——脱离当前对话也能读懂，不能有「那个」「上次说的」这类悬空指代，\
                 也不能是问句或「他自称…」这类转述；\
             3. 忘了它就会让你答错或只能泛泛而谈。\
             内容用自己的话写成一两句完整陈述，别抄用户原话。\
             用户的话被推翻或更新时（搬家、换工作、换称呼），用 recall_memory 查到旧记忆，\
             再把它的 [id] 填进 supersedes 改写掉那条，不要让新旧两条并存。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": MemoryKind::ALL.map(|k| k.as_str()),
                    "description": "这条记忆的种类：trait / preference / routine / plan。决定默认有效期"
                },
                "content": {
                    "type": "string",
                    "description": "要记住的自足事实陈述，如「用户现在住在上海，之前住北京」"
                },
                "supersedes": {
                    "type": "string",
                    "description": "被这条内容取代的既有记忆 id，来自 recall_memory 结果里的 [id]。\
                                    没有需要取代的旧记忆时省略这个参数。"
                }
            },
            "required": ["kind", "content"],
            "additionalProperties": false
        }))
}

/// 执行一次工具调用。
pub fn dispatch(call: &ChatCall, ctx: &dyn ToolContext) -> ToolOutcome {
    match call.fn_name.as_str() {
        SEND_MESSAGE => match call.arg_str("text") {
            Some(raw) => {
                let text = crate::utils::textflow::flatten(raw);
                let n = text.chars().count();
                if n == 0 {
                    // 归一化后为空 = 这一轮没攒出可说的话。投递空消息在通道上是一次
                    // 无意义的推送，比不投更像故障；回填说明让它自己决定要不要补话。
                    ToolOutcome::Filled("没有可发送的内容，这一轮什么都没说。".to_string())
                } else if n > SEND_MESSAGE_MAX_CHARS {
                    // 不截断、也不投递：截断会留下半句话，而半句话比长句更像故障。
                    // 直接要求模型自己拆成多条——它能决定在哪断。
                    ToolOutcome::Filled(format!(
                        "这条有 {n} 字，超过单条 {SEND_MESSAGE_MAX_CHARS} 字上限，没有发送。请拆成多条分别调用。"
                    ))
                } else {
                    ToolOutcome::Sent(text)
                }
            }
            None => ToolOutcome::Rejected("缺少参数 text".to_string()),
        },
        SEND_DICE => {
            // emoji 缺省为 🎲；显式给了就按白名单校验——Telegram 只认这六个，
            // 传别的会在服务端报错，不如在这里拒绝并让模型改。
            let emoji = call
                .arg_str("emoji")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DICE_EMOJIS[0]);
            if DICE_EMOJIS.contains(&emoji) {
                ToolOutcome::SentDice(emoji.to_string())
            } else {
                ToolOutcome::Rejected(format!(
                    "不支持的 emoji「{emoji}」，只能是 {} 之一",
                    DICE_EMOJIS.join(" ")
                ))
            }
        }
        SEND_STICKER => {
            // 键是模型可枚举的有限集合，非法键必须在这里挡下并把可选项回填，
            // 让它一次就能改对，而不是拿 file_id 去 Telegram 撞一次服务端错误。
            let Some(key) = call.arg_str("key").map(str::trim).filter(|s| !s.is_empty()) else {
                return ToolOutcome::Rejected("缺少参数 key".to_string());
            };
            match ctx.sticker_file_id(key) {
                Some(file_id) => ToolOutcome::SentSticker(file_id),
                None => {
                    let keys = ctx.sticker_keys();
                    let hint = if keys.is_empty() {
                        "当前没有可用贴纸".to_string()
                    } else {
                        keys.join(" ")
                    };
                    ToolOutcome::Rejected(format!("没有「{key}」这张贴纸，可用的 key：{hint}"))
                }
            }
        }
        GET_NOW => ToolOutcome::Filled(ctx.now()),
        RECALL_MEMORY => match call.arg_str("query") {
            Some(q) if !q.trim().is_empty() => ToolOutcome::Filled(ctx.recall(q.trim())),
            _ => ToolOutcome::Rejected("缺少参数 query".to_string()),
        },
        REMEMBER => match call.arg_str("content") {
            Some(raw) if !raw.trim().is_empty() => {
                let content = raw.trim();
                // 先校验 kind：内容有问题时不该让模型误以为是种类填错了。
                let Some(kind) = parse_kind(call.arg_str("kind")) else {
                    return ToolOutcome::Rejected(format!(
                        "缺少或非法的参数 kind，应为 {} 之一",
                        MemoryKind::ALL.map(|k| k.as_str()).join(" / ")
                    ));
                };
                if content.chars().count() > REMEMBER_MAX_CHARS {
                    ToolOutcome::Rejected(format!(
                        "这条有 {} 字，超过单条 {REMEMBER_MAX_CHARS} 字上限，没有记住。一条事实不该这么长，写短一点。",
                        content.chars().count()
                    ))
                } else {
                    // 空串按"没传"处理：模型偶尔会给 supersedes 塞个空串。
                    let supersedes = call
                        .arg_str("supersedes")
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    match ctx.remember(content, kind, supersedes) {
                        Ok(WriteKind::Created) => ToolOutcome::Filled(REMEMBER_ACK_NEW.to_string()),
                        Ok(WriteKind::Revised) => {
                            ToolOutcome::Filled(REMEMBER_ACK_REVISED.to_string())
                        }
                        Err(e) => ToolOutcome::Rejected(format!("没有记住：{e}")),
                    }
                }
            }
            _ => ToolOutcome::Rejected("缺少参数 content".to_string()),
        },
        other => ToolOutcome::Rejected(format!("未知工具 {other}")),
    }
}

/// 解析 `kind` 参数；缺失或不在枚举内返回 None，由调用方给模型明确回执。
fn parse_kind(raw: Option<&str>) -> Option<MemoryKind> {
    let raw = raw?.trim();
    MemoryKind::ALL
        .into_iter()
        .find(|k| k.as_str() == raw.to_ascii_lowercase())
}

/// `send_message` 成功投递后回填给模型的确认文本。
///
/// 刻意极短：这段文本会进入下一轮 prompt，啰嗦会被模型模仿成对用户的废话。
pub const SEND_MESSAGE_ACK: &str = "已发送";

/// `send_dice` 成功投递后回填给模型的确认文本。与 [`SEND_MESSAGE_ACK`] 同样极短。
pub const SEND_DICE_ACK: &str = "已发送";

/// `send_sticker` 成功投递后回填给模型的确认文本。
///
/// 刻意不带上是哪张贴纸：键本身已在模型的这次调用里，回填一遍只会让它接着
/// 向用户解释"我刚发了一张 XX"。
pub const SEND_STICKER_ACK: &str = "已发送";

/// `remember` 新增一条记忆后的回填文本。同样极短，且不复述内容——
/// 让模型把自己的话再抄一遍进下一轮 prompt，只会诱发它接着解释"我记住了…"。
pub const REMEMBER_ACK_NEW: &str = "已记住";

/// `remember` 改写既有记忆后的回填文本。
pub const REMEMBER_ACK_REVISED: &str = "已改写旧记忆";

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tool_tests;
