//! 人工确认闸门：MCP 工具执行前先问过用户。
//!
//! # 为什么要有这一道
//!
//! MCP 工具意味着「以本机身份做外部动作」——跑工作流、写文件、调接口。
//! 模型直接发起就执行，等于把"要不要做这件事"的裁量权交给了模型，而用户
//! 连插话的机会都没有。故在执行前先把「要调哪个服务器的哪个工具、参数是什么」
//! 投给用户，等一个明确的"同意"才真正执行。
//!
//! # 等待怎么实现
//!
//! 通道侧收到用户消息的**当下**就把它落库（见 `channels::telegram::poll`），
//! 且"本会话已有轮次在跑"时不会再起第二轮——那条消息就躺在时间线里。
//! 故本模块的等待是**轮询会话时间线**：从游标之后找用户消息，找到即判定。
//! 这条路不依赖任何通道专有的回调，Web / iLink 也能走（是否可用由各自实现决定）。
//!
//! # 判定口径
//!
//! 只有**明确同意**才执行。其余情形（明确拒绝 / 回的别的内容 / 超时）一律
//! 不执行，并把原因如实回填给模型，让它自己改口径（换参数、问用户、或者放弃）。
//! 宁可不做，不可错做。

use std::time::{Duration, Instant};

use crate::core::session::Sessions;

/// 轮询间隔：等用户回应时的检查频率。
///
/// 取 250ms：Telegram 的长轮询在消息到达时立刻返回，落库也就这一瞬，
/// 250ms 足够跟手，又不至于把数据库问得太勤。
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 确认请求里展示的参数正文上限（字符）。超长截断——用户要看得懂，不是读日志。
const ARG_PREVIEW_LIMIT: usize = 600;

/// 用户对一次确认请求的回应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 明确同意：执行。
    Approved,
    /// 明确拒绝：不执行。
    Rejected,
    /// 回了别的内容：既没说同意也没说拒绝。按不执行处理，但把原文带给模型。
    Unclear(String),
    /// 等超时：没等到回应。不执行。
    TimedOut,
}

impl Decision {
    /// 是否放行执行。只有 [`Decision::Approved`] 放行。
    pub fn approved(&self) -> bool {
        matches!(self, Decision::Approved)
    }
}

/// 明确同意的整句（归一化后精确匹配）。
const APPROVE_EXACT: &[&str] = &[
    "确认",
    "确认执行",
    "同意",
    "同意执行",
    "批准",
    "允许",
    "可以",
    "好",
    "好的",
    "好吧",
    "行",
    "行吧",
    "嗯",
    "对",
    "是",
    "是的",
    "没问题",
    "可以执行",
    "执行",
    "执行吧",
    "继续",
    "继续吧",
    "做吧",
    "去做吧",
    "去吧",
    "yes",
    "y",
    "ok",
    "okay",
    "sure",
    "go",
    "go ahead",
    "confirm",
    "confirmed",
    "approve",
    "approved",
    "accept",
    "do it",
    "proceed",
    "run it",
];

/// 明确拒绝的整句（归一化后精确匹配）。
const REJECT_EXACT: &[&str] = &[
    "不",
    "不行",
    "不用",
    "不用了",
    "不要",
    "不要了",
    "不需要",
    "不必",
    "不能",
    "不可以",
    "别",
    "别了",
    "别做",
    "算了",
    "取消",
    "撤回",
    "停",
    "停下",
    "停止",
    "拒绝",
    "不确认",
    "不批准",
    "否",
    "no",
    "n",
    "nope",
    "cancel",
    "stop",
    "reject",
    "rejected",
    "deny",
    "abort",
];

/// 长句里的同意关键词（中文按子串匹配）。
const APPROVE_CJK: &[&str] = &[
    "确认",
    "同意",
    "批准",
    "允许",
    "可以",
    "没问题",
    "执行",
    "继续",
    "放行",
    "准许",
    "好的",
    "好吧",
];

/// 长句里的拒绝关键词（中文按子串匹配）。
const REJECT_CJK: &[&str] = &[
    "取消",
    "拒绝",
    "不要",
    "不用",
    "不必",
    "不行",
    "不能",
    "不可以",
    "别做",
    "别了",
    "算了",
    "撤回",
    "停止",
    "停下",
    "不干",
];

/// 长句里的同意关键词（英文按整词匹配，避免 `ok` 命中 `book`）。
const APPROVE_ASCII: &[&str] = &[
    "yes",
    "y",
    "ok",
    "okay",
    "sure",
    "confirm",
    "confirmed",
    "approve",
    "approved",
    "accept",
    "proceed",
    "go",
];

/// 长句里的同意短语（英文按整段匹配）。
///
/// 短语与单词分开：`do` / `it` 单独作词太容易误判（"do you want me to run it?" 里
/// 的 `do` 不是同意），只有连成 `do it` 才是。
const APPROVE_ASCII_PHRASES: &[&str] = &["go ahead", "do it", "run it", "sounds good", "please do"];

/// 长句里的拒绝关键词（英文按整词匹配）。
const REJECT_ASCII: &[&str] = &[
    "no",
    "n",
    "nope",
    "cancel",
    "stop",
    "reject",
    "rejected",
    "deny",
    "abort",
    "nevermind",
];

/// 「别 + 动作」= 拒绝（"别执行"、"别做"、"别发"…）。
///
/// 单看"别"字不够：它也出现在"别人"、"区别"、"特别"里，那些不是拒绝。
/// 故要看它后面跟的是不是动作词。
const NEGATED_ACTIONS: &[&str] = &[
    "执行", "做", "跑", "调", "发", "动", "开", "试", "弄", "干", "用", "传", "写", "删", "改",
    "点", "启动", "触发", "调用", "生成", "上传", "下载", "发送", "碰",
];

/// 归一化：标点与空白一律换成空格，ASCII 字母转小写，保留中日韩文字。
///
/// 保留文字是必须的——"好"、"行"这类同意词全靠它；换成空格是为了让英文词
/// 能被整词切分（`ok` 不该命中 `book`）。
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            out.extend(ch.to_lowercase());
        } else {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// 去掉所有空白后的形式（中文短句常被空格拆开：`"好 的"`）。
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 整词命中（英文）。
fn contains_word(text: &str, needle: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| word == needle)
}

/// 「别 + 动作词」命中（"别执行"是拒绝，"别人"不是）。
fn negated_action(compact: &str) -> bool {
    let mut offset = 0usize;
    while let Some(pos) = compact[offset..].find('别') {
        // '别' 占 3 字节，`find` 给的是字符边界，加 3 之后仍是边界。
        let after = &compact[offset + pos + '别'.len_utf8()..];
        if NEGATED_ACTIONS.iter().any(|verb| after.starts_with(verb)) {
            return true;
        }
        offset += pos + '别'.len_utf8();
    }
    false
}

/// 判定用户的回应。
///
/// **拒绝优先于同意**：一句里同时出现两边时（"不要执行"/"确认后别执行"），
/// 按拒绝处理。错做的代价远大于漏做。
///
/// **疑问不算同意**：带问号、或以"吗""呢"收尾的句子是在问，不是在点头。
/// 这条比关键词表更要紧——"do you want me to run it?" 里也有 `run it`，
/// 只看关键词会把问句当成批准。
pub fn classify(text: &str) -> Decision {
    let normalized = normalize(text);
    let compact = compact(&normalized);
    if REJECT_EXACT.contains(&normalized.as_str()) || REJECT_EXACT.contains(&compact.as_str()) {
        return Decision::Rejected;
    }
    if APPROVE_EXACT.contains(&normalized.as_str()) || APPROVE_EXACT.contains(&compact.as_str()) {
        return Decision::Approved;
    }
    // 长句：关键词扫描，仍以拒绝优先。
    if REJECT_CJK.iter().any(|k| compact.contains(k))
        || REJECT_ASCII.iter().any(|k| contains_word(&normalized, k))
        || negated_action(&compact)
    {
        return Decision::Rejected;
    }
    // 疑问句不点头。
    if is_questioning(text, &compact) {
        return Decision::Unclear(text.trim().to_string());
    }
    if APPROVE_CJK.iter().any(|k| compact.contains(k))
        || APPROVE_ASCII.iter().any(|k| contains_word(&normalized, k))
        || APPROVE_ASCII_PHRASES.iter().any(|p| normalized.contains(p))
    {
        return Decision::Approved;
    }
    Decision::Unclear(text.trim().to_string())
}

/// 这句话是不是在提问。
///
/// 问号要看**原文**：归一化会把标点换成空格，问号到那儿已经没了。
fn is_questioning(original: &str, compact: &str) -> bool {
    original.contains('?')
        || original.contains('？')
        || compact.ends_with("吗")
        || compact.ends_with("呢")
}

/// 确认请求的正文：投给用户看的那条消息。
///
/// 说清楚三件事：**哪个服务器、哪个工具、传了什么参数**——用户要能凭这条消息
/// 判断"这事该不该做"，光说"要调用一个工具"是不够的。
pub fn request_text(server: &str, tool: &str, arguments: &serde_json::Value) -> String {
    let args = serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
    let args = if args.chars().count() > ARG_PREVIEW_LIMIT {
        let kept: String = args.chars().take(ARG_PREVIEW_LIMIT).collect();
        format!("{kept}…（参数过长，已截断）")
    } else {
        args
    };
    format!(
        "⚠️ 需要你确认一次外部操作\n\
         服务器：{server}\n\
         工具：{tool}\n\
         参数：\n{args}\n\
         回复「确认」执行，回复「取消」放弃。"
    )
}

/// 回填给模型的拒绝说明。
///
/// 拒绝 / 超时 / 回的别的内容，各说各的——模型据此决定下一步（改参数再问、
/// 告诉用户为什么没做、或者干脆放弃）。回的别的内容时把原文带上：用户可能
/// 只是在回答模型刚才问的别的问题，模型得看得见这句话。
pub fn refusal_text(decision: &Decision) -> String {
    match decision {
        Decision::Approved => {
            "调用失败：用户已同意执行，但本次未执行（内部状态不一致）。".to_string()
        }
        Decision::Rejected => "调用失败：用户拒绝了这次调用，未执行。".to_string(),
        Decision::Unclear(reply) => {
            if reply.trim().is_empty() {
                "调用失败：用户没有明确确认，未执行。".to_string()
            } else {
                format!(
                    "调用失败：用户没有明确确认（回复内容：{reply}），未执行。请据此判断下一步。"
                )
            }
        }
        Decision::TimedOut => "调用失败：等待用户确认超时，未执行。".to_string(),
    }
}

/// 等用户回应：轮询会话时间线里游标之后的用户消息。
///
/// 找到**第一条**就据此判定，并把游标推进到它——推进的意义是"这条已经被这次
/// 确认消费掉了"：插话并入（[`crate::core::agent`] 的 `ingest_steers`）与确认
/// 闸门共用同一个游标，故确认回复不会被当成新插话再并进上下文一遍。
///
/// 游标为 `None`（没有可用的水位线）时永远等不到——返回 [`Decision::TimedOut`]，
/// 即"不执行"。宁可不做，不可错做。
pub fn wait(
    sessions: &Sessions,
    session_id: &str,
    cursor: &mut Option<String>,
    timeout: Duration,
) -> Decision {
    let deadline = Instant::now() + timeout;
    loop {
        let fresh = match cursor.as_deref() {
            Some(after) => sessions.user_messages_after(session_id, after),
            None => Ok(Vec::new()),
        };
        match fresh {
            Ok(messages) => {
                if let Some(first) = messages.first() {
                    *cursor = Some(first.id.clone());
                    return classify(&first.content);
                }
            }
            Err(error) => {
                tracing::warn!(
                    session = %session_id,
                    error = %error,
                    "确认闸门读取会话消息失败，继续等待"
                );
            }
        }
        if Instant::now() >= deadline {
            return Decision::TimedOut;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 明确的同意词放行() {
        for text in [
            "确认",
            "确认。",
            "好",
            "好的",
            "行吧",
            "可以",
            "ok",
            "OK!",
            "yes",
            "Yes",
        ] {
            assert_eq!(classify(text), Decision::Approved, "应放行：{text}");
        }
    }

    #[test]
    fn 明确的拒绝词不放行() {
        for text in [
            "取消",
            "不要",
            "不用了",
            "别执行",
            "算了",
            "no",
            "Nope",
            "拒绝",
        ] {
            assert_eq!(classify(text), Decision::Rejected, "应拒绝：{text}");
        }
    }

    #[test]
    fn 别人之类不算拒绝() {
        // "别" 只在后面跟动作词时才算拒绝。
        assert!(matches!(classify("这和别人不一样"), Decision::Unclear(_)));
        assert!(matches!(classify("特别谢谢"), Decision::Unclear(_)));
    }

    #[test]
    fn 拒绝优先于同意() {
        // 一句里同时出现两边时按拒绝处理：错做的代价远大于漏做。
        assert_eq!(classify("不要执行"), Decision::Rejected);
        assert_eq!(classify("确认后别执行"), Decision::Rejected);
        assert_eq!(classify("不可以执行"), Decision::Rejected);
    }

    #[test]
    fn 长句里的同意词按整词匹配不误伤() {
        // `ok` 不该命中 `book` / `look`；中文短词也不该从长句里乱认。
        assert!(matches!(classify("那本书放在哪了"), Decision::Unclear(_)));
        assert!(matches!(classify("look at this"), Decision::Unclear(_)));
        assert_eq!(classify("那你去确认一下吧"), Decision::Approved);
        assert_eq!(classify("OK, go ahead"), Decision::Approved);
    }

    #[test]
    fn 疑问句不算同意() {
        // "do" / "run" 单独成词不算同意——只有连成短语才是。
        assert!(matches!(
            classify("do you want me to run it?"),
            Decision::Unclear(_)
        ));
        assert!(matches!(classify("你是要我执行吗"), Decision::Unclear(_)));
        assert_eq!(classify("do it"), Decision::Approved);
        assert_eq!(classify("please do it"), Decision::Approved);
    }

    #[test]
    fn 认不出来的回复保持原样() {
        let decision = classify("今天天气怎么样");
        assert_eq!(decision, Decision::Unclear("今天天气怎么样".to_string()));
    }

    #[test]
    fn 拒绝说明各说各的() {
        assert!(refusal_text(&Decision::Rejected).contains("拒绝"));
        assert!(refusal_text(&Decision::TimedOut).contains("超时"));
        let unclear = refusal_text(&Decision::Unclear("今天天气怎么样".to_string()));
        assert!(unclear.contains("今天天气怎么样"));
        assert!(unclear.contains("未执行"));
    }

    #[test]
    fn 确认请求说清服务器工具与参数() {
        let text = request_text(
            "comfy",
            "run_workflow",
            &serde_json::json!({"prompt": "一只猫"}),
        );
        assert!(text.contains("comfy"));
        assert!(text.contains("run_workflow"));
        assert!(text.contains("一只猫"));
        assert!(text.contains("确认"));
    }

    #[test]
    fn 超长参数被截断() {
        let long = "x".repeat(ARG_PREVIEW_LIMIT + 100);
        let text = request_text("srv", "tool", &serde_json::json!({"k": long}));
        assert!(text.contains("参数过长，已截断"));
    }

    /// 造一个内存会话（不碰磁盘库）。
    fn sessions() -> crate::core::session::Sessions {
        crate::core::session::Sessions::new(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::Database::in_memory().unwrap(),
        )))
        .unwrap()
    }

    #[test]
    fn 等待取到游标之后的用户消息并推进游标() {
        let conv = sessions();
        let sid = conv.resolve("telegram", "100").unwrap();
        let trigger = conv.add_message(&sid, "user", "帮我跑个工作流").unwrap();
        let mut cursor = Some(trigger.id.clone());

        // 还没有新消息：等不到就是超时（不执行）。
        assert_eq!(
            wait(&conv, &sid, &mut cursor, Duration::from_millis(10)),
            Decision::TimedOut
        );

        // 用户回了"确认"。
        conv.add_message(&sid, "user", "确认").unwrap();
        assert_eq!(
            wait(&conv, &sid, &mut cursor, Duration::from_secs(1)),
            Decision::Approved
        );

        // 游标已推进到那条消息：同一条不会被消费第二遍（防重复确认）。
        assert_eq!(
            wait(&conv, &sid, &mut cursor, Duration::from_millis(10)),
            Decision::TimedOut
        );
    }

    #[test]
    fn 没有水位线时永远等不到() {
        let conv = sessions();
        let sid = conv.resolve("telegram", "101").unwrap();
        let mut cursor: Option<String> = None;
        assert_eq!(
            wait(&conv, &sid, &mut cursor, Duration::from_millis(10)),
            Decision::TimedOut
        );
    }
}
