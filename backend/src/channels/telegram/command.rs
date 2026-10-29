//! Telegram command 子模块。

use super::*;

/// Telegram 管理命令的纯解析结果。
///
/// 通道层只负责把文本解析成动作，再调用 `IdentityService`；这里不读数据库，
/// 便于在单元测试中覆盖命令边界而不发网络请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Command {
    Start,
    New,
    Identity,
    Proposals,
    Approve { id: String, consent: bool },
    Reject { id: String },
    Rollback { version: Option<i64> },
    Help,
}

/// 解析一条命令。普通消息返回 `None`；未知斜杠命令返回 [`Command::Help`]。
pub(super) fn parse_command(text: &str) -> Result<Option<Command>, String> {
    let mut parts = text.split_whitespace();
    let Some(raw) = parts.next() else {
        return Ok(None);
    };
    if !raw.starts_with('/') {
        return Ok(None);
    }
    let command = raw.trim_start_matches('/').split('@').next().unwrap_or("");
    let command = command.to_ascii_lowercase();
    let args: Vec<&str> = parts.collect();
    let command = match command.as_str() {
        "start" => Command::Start,
        "new" => Command::New,
        "identity" => Command::Identity,
        "proposals" => Command::Proposals,
        "approve" => {
            let id = args
                .first()
                .ok_or_else(|| "用法：/approve <提案 id> [consent|--consent]".to_string())?
                .to_string();
            let consent = args
                .iter()
                .skip(1)
                .any(|arg| matches!(*arg, "consent" | "--consent"));
            Command::Approve { id, consent }
        }
        "reject" => {
            let id = args
                .first()
                .ok_or_else(|| "用法：/reject <提案 id>".to_string())?
                .to_string();
            Command::Reject { id }
        }
        "rollback" => {
            let version = match args.first() {
                None => None,
                Some(raw) => Some(
                    raw.parse::<i64>()
                        .map_err(|_| "用法：/rollback [正整数版本]".to_string())?,
                ),
            };
            Command::Rollback { version }
        }
        "help" => Command::Help,
        _ => Command::Help,
    };
    Ok(Some(command))
}

pub(super) fn format_identity(view: &IdentityView) -> String {
    let version = match view.version {
        Some(0) => "未版本化（旧 self_profile）".to_string(),
        Some(v) => format!("v{v}"),
        None => "未知".to_string(),
    };
    let body = if view.text.trim().is_empty() {
        "（当前身份为空）"
    } else {
        view.text.as_str()
    };
    format!("当前身份（{version}）\n{body}")
}

pub(super) fn format_proposal(view: &ProposalView) -> String {
    let before = view.before_value.as_deref().unwrap_or("（未设置）");
    let evidence = if view.evidence_message_ids.is_empty() {
        "（无）".to_string()
    } else {
        view.evidence_message_ids.join(", ")
    };
    format!(
        "{}\n字段：{}（{}）\n风险：{}；置信度：{}；基线：v{}\n变更：{} → {}\n理由：{}\n证据：{}",
        view.id,
        IdentityFieldKey::parse(&view.field_key)
            .map(|key| key.label())
            .unwrap_or("未知字段"),
        view.field_key,
        view.risk_level,
        view.confidence,
        view.base_version,
        before,
        view.after_value,
        view.reason,
        evidence,
    )
}

pub(super) fn format_proposals(views: &[ProposalView]) -> String {
    if views.is_empty() {
        return "当前没有待审批的身份提案。".to_string();
    }
    let mut out = format!("待审批身份提案（{} 条）", views.len());
    for view in views {
        out.push_str("\n\n");
        out.push_str(&format_proposal(view));
    }
    out
}

pub(super) fn format_approved(proposal: &ProposalView, version: &IdentityVersionView) -> String {
    format!(
        "已批准并应用提案 {}，身份版本 v{}。\n变更字段：{}\n新值：{}",
        proposal.id, version.version, proposal.field_key, proposal.after_value
    )
}

pub(super) fn format_rejected(proposal: &ProposalView) -> String {
    format!("已拒绝提案 {}，当前身份未改变。", proposal.id)
}

pub(super) fn format_rollback(version: &IdentityVersionView) -> String {
    format!(
        "已回滚到身份版本 v{}。\n回滚来源：{}\n原因：{}",
        version.version, version.source, version.reason
    )
}

pub(super) fn format_help() -> String {
    "可用命令：\n/identity 查看当前身份（仅管理员）\n/proposals 查看待审批提案（仅管理员）\n/approve <id> [consent|--consent] 批准提案（仅管理员；关系姿态必须带 consent）\n/reject <id> 拒绝提案（仅管理员）\n/rollback 撤销最新生效提案（仅管理员）\n/rollback <version> 恢复到指定历史版本（仅管理员）\n/new 开始新对话".to_string()
}

pub(super) fn execute_command(
    session: &Sessions,
    identity: &IdentityService,
    is_admin: bool,
    chat_id: i64,
    command: Command,
) -> Result<String, String> {
    let now = Utc::now();
    let operator = format!("telegram:{chat_id}");
    match command {
        command if is_admin_command(&command) && !is_admin => Ok(admin_denied()),
        Command::Start => Ok(format!(
            "你好，我是你的记忆助手。直接发消息聊天，我会记住值得记住的事；发送 /new 开始一段新的对话。\n\n{}",
            format_help()
        )),
        Command::New => {
            session
                .reset(CHANNEL, &chat_id.to_string())
                .map_err(|e| e.to_string())?;
            Ok("已开始新对话，之前的记录保留在记忆库里。".to_string())
        }
        Command::Identity => Ok(format_identity(&identity.current())),
        Command::Proposals => {
            let proposals = identity.pending(10).map_err(|e| e.to_string())?;
            Ok(format_proposals(&proposals))
        }
        Command::Approve { id, consent } => {
            let proposal = identity.get(&id).map_err(|e| e.to_string())?;
            let version = identity
                .approve_and_apply(&id, consent, &operator, now)
                .map_err(|e| e.to_string())?;
            Ok(format_approved(&proposal, &version))
        }
        Command::Reject { id } => {
            let proposal = identity
                .reject(&id, &operator, now)
                .map_err(|e| e.to_string())?;
            Ok(format_rejected(&proposal))
        }
        Command::Rollback { version } => {
            let result = identity
                .rollback_version(version, &operator, now)
                .map_err(|e| e.to_string())?;
            Ok(format_rollback(&result))
        }
        Command::Help => Ok(format_help()),
    }
}

/// 哪些命令属于身份管理，必须走管理员门禁。
pub(super) fn is_admin_command(command: &Command) -> bool {
    matches!(
        command,
        Command::Identity
            | Command::Proposals
            | Command::Approve { .. }
            | Command::Reject { .. }
            | Command::Rollback { .. }
    )
}

/// 身份管理命令的统一拒绝文案。
pub(super) fn admin_denied() -> String {
    "身份管理命令只允许白名单管理员使用。".to_string()
}

/// 命令类消息的即时回执：管理命令不进 AI 流水线。
///
/// 返回 `Some(文本)` 表示这是命令、已处理完；`None` 表示普通消息，交给 AI 流水线。
pub(super) fn command_reply(
    session: &Sessions,
    identity: &IdentityService,
    is_admin: bool,
    chat_id: i64,
    text: &str,
) -> Result<Option<String>, String> {
    match parse_command(text)? {
        Some(command) => execute_command(session, identity, is_admin, chat_id, command).map(Some),
        None => Ok(None),
    }
}
