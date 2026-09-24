//! 相对时间锚点的确定性到期推导。
//!
//! 用户消息里的"今天 / 明天 / 本周末 / 下周 / N 天后…"都是**相对当下时刻**的，
//! 过了指称时刻就成旧闻。而 LLM 判定常把它们记成 `core`（永不过期），或套错
//! TTL（`short` 7 天 / `intent` 90 天，都远长于"明天就过期"）。
//!
//! 本模块在落库前做一道确定性守护：命中强时间锚词，就强制降级层级并给出精确
//! 的 `expires_at`（覆盖层级 TTL），过期即不再被召回——既保留窗口期内的可用性，
//! 又不让它沉淀成核心事实。
//!
//! 只处理**强锚**（今天/明天/后天/本周末/下周/N天(后)等），**宁可漏不可错**：
//! "每天 / 经常 / 每周一到周五"这类反复性表达不命中（它们是长期事实）；
//! "改天 / 有空 / 以后再说"这类没有明确日期的未定之约也不命中（归原层级 TTL）。

use chrono::{DateTime, Duration, Utc};

const IMMEDIATE: &[&str] = &["马上", "待会儿", "待会", "等会儿", "等会", "一会儿", "稍后", "过会儿", "几分钟"];
const TODAY: &[&str] = &["今天", "今日", "今晚", "今早", "今晨"];
const TOMORROW: &[&str] = &["明天", "明日", "明早", "明晚"];
const DAY_AFTER: &[&str] = &["后天", "明后天", "大后天"];
const WEEKEND: &[&str] = &["周末", "本周末", "这周末", "这个周末"];
const THIS_WEEK: &[&str] = &["这周", "本周"];
const NEXT_WEEK: &[&str] = &["下周", "下礼拜", "下周末"];
const NEXT_MONTH: &[&str] = &["下个月", "下月"];

/// 相对时间锚点的到期推导。
///
/// 命中返回 `(强制层级, 到期时刻)`；未命中返回 `None`（保持原层级走 TTL 兜底）。
///
/// 层级语义：`short` = 今日 / 即刻之类临时态；`intent` = 计划 / 意向（即将到来但会过去）。
/// 到期时刻统一取「指称日结束」的下一个零点，保证当天仍可召回、次日即失效。
pub(crate) fn temporal_anchor(content: &str) -> Option<(&'static str, DateTime<Utc>)> {
    let now = Utc::now();
    let base = now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();

    // 即刻类：几小时内作废。
    if IMMEDIATE.iter().any(|w| content.contains(w)) {
        return Some(("short", now + Duration::hours(2)));
    }
    // 今天类：当日结束（次日零点）即失效。
    if TODAY.iter().any(|w| content.contains(w)) {
        return Some(("short", base + Duration::days(1)));
    }
    // 明天类：指称日结束（明天结束于再下一天零点）。
    if TOMORROW.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(2)));
    }
    // 后天类。
    if DAY_AFTER.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(3)));
    }
    // 周末类：涵盖本周末及其缓冲。
    if WEEKEND.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(4)));
    }
    // 本周类：覆盖到本周结束。
    if THIS_WEEK.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(8)));
    }
    // 下周类：覆盖"下周"全部。
    if NEXT_WEEK.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(10)));
    }
    // 下月类。
    if NEXT_MONTH.iter().any(|w| content.contains(w)) {
        return Some(("intent", base + Duration::days(40)));
    }
    // 数字类：`N 天 / N 小时 / N 周 / N 个月 / N 年 +（后/内/之后/之内/以内/以后）`。
    numeric_anchor(content, now, base)
}

/// 解析「N + 时间单位 + 后缀」短语；命中返回 `(是否小时级, 到期时刻)`。
fn numeric_anchor(
    content: &str,
    now: DateTime<Utc>,
    base: DateTime<Utc>,
) -> Option<(&'static str, DateTime<Utc>)> {
    let chars: Vec<char> = content.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let num = if chars[i].is_ascii_digit() {
            let num_start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            chars[num_start..i].iter().collect::<String>().parse::<i64>().ok()?
        } else if is_cn_digit(chars[i]) {
            // 中文数字：贪婪取整段（"三五"这类非法串解析失败则跳过）。
            let num_start = i;
            while i < n && is_cn_digit(chars[i]) {
                i += 1;
            }
            let Some(num) = cn_number(&chars[num_start..i]) else { continue };
            num
        } else {
            i += 1;
            continue;
        };
        // 数字与单位之间允许空格 / 全角空格。
        let mut j = i;
        while j < n && (chars[j] == ' ' || chars[j] == '\u{3000}') {
            j += 1;
        }
        let rest: String = chars[j..].iter().collect();
        let (unit_len, now_based, minutes_per_unit) = if rest.starts_with("个小时") {
            (3, true, 60)
        } else if rest.starts_with("小时") {
            (2, true, 60)
        } else if rest.starts_with("分钟") {
            (2, true, 1)
        } else if rest.starts_with("个星期") {
            (3, false, 7 * 24 * 60)
        } else if rest.starts_with("星期") {
            (2, false, 7 * 24 * 60)
        } else if rest.starts_with("个月") {
            (2, false, 30 * 24 * 60)
        } else if rest.starts_with("周") {
            (1, false, 7 * 24 * 60)
        } else if rest.starts_with("天") {
            (1, false, 24 * 60)
        } else if rest.starts_with("年") {
            (1, false, 365 * 24 * 60)
        } else {
            continue;
        };
        // 后缀限定为"将来"，排除"前"（那是已成事实："三个月前辞职"）。
        let suffix: String = chars.iter().skip(j + unit_len).collect();
        let suffix = suffix.trim_start();
        let suffix_ok = ["后", "内", "以后", "之内", "以内", "之后"]
            .iter()
            .any(|s| suffix.starts_with(s));
        if !suffix_ok {
            continue;
        }
        // 天数类过期时刻 = 指称日结束 = `base + (N 个单位 + 1) 天`（+1 天到次日零点）；
        // 即时类 = `now + N 分钟`。
        let deadline = if now_based {
            now + Duration::minutes(num * minutes_per_unit)
        } else {
            base + Duration::minutes(num * minutes_per_unit + 24 * 60)
        };
        return Some((if now_based { "short" } else { "intent" }, deadline));
    }
    None
}

/// 单个中文数字字符（含"两"）。
fn is_cn_digit(c: char) -> bool {
    matches!(c, '零' | '〇' | '一' | '二' | '两' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十')
}

fn cn_digit_value(c: char) -> Option<i64> {
    match c {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        '十' => Some(10),
        _ => None,
    }
}

/// 把一段中文数字翻译成数值（支持 0-9、10-99 的"几十几"写法；非法串返回 None）。
fn cn_number(cs: &[char]) -> Option<i64> {
    if cs.is_empty() || cs.iter().any(|c| !is_cn_digit(*c)) {
        return None;
    }
    match cs.iter().position(|c| *c == '十') {
        None => cn_digit_value(cs[0]),
        Some(t) => {
            // "X十Y"：t=0 时是"十"或"十几"，否则首位是十位数。
            let tens = if t == 0 { 10 } else { cn_digit_value(cs[0])? * 10 };
            let ones = cs.get(t + 1).map_or(Some(0), |c| cn_digit_value(*c))?;
            Some(tens + ones)
        }
    }
}

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod temporal_tests;