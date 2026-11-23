//! Agent 角色与工具权限边界。
//!
//! 角色白名单是**代码契约**，不是提示词建议：工具定义按角色裁剪，派发前还会再校验
//! 一次。即使模型幻觉调用未下发的工具，也只能得到拒绝回执，不能投递消息或写入状态。
//!
//! MVP 只固定三个角色的能力边界，不引入通用多 Agent 编排框架。身份演化角色目前
//! 只有只读证据工具；提交身份提案的工具在提案持久化接入后加入白名单。

use crate::llm::tool;

/// 一个 Agent 角色。顺序固定，便于遍历和测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentRole {
    Conversation,
    Proactive,
    IdentityEvolution,
}

impl AgentRole {
    pub const ALL: [Self; 3] = [Self::Conversation, Self::Proactive, Self::IdentityEvolution];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Proactive => "proactive",
            Self::IdentityEvolution => "identity_evolution",
        }
    }

    /// 该角色允许调用的全部工具。
    ///
    /// 对话角色拥有当前完整工具集；主动角色保持现有能力，不写用户记忆；身份演化
    /// 角色只有只读证据工具，绝不能向用户发送消息。
    pub fn allowed_tools(self) -> &'static [&'static str] {
        match self {
            Self::Conversation => &[
                tool::SEND_MESSAGE,
                tool::SEND_DICE,
                tool::SEND_STICKER,
                tool::GENERATE_IMAGE,
                tool::GENERATE_VOICE,
                tool::GET_NOW,
                tool::RECALL_MEMORY,
                tool::REMEMBER,
            ],
            Self::Proactive => &[tool::SEND_MESSAGE, tool::GET_NOW, tool::RECALL_MEMORY],
            Self::IdentityEvolution => &[tool::GET_NOW, tool::RECALL_MEMORY],
        }
    }

    pub fn allows_tool(self, name: &str) -> bool {
        self.allowed_tools().contains(&name)
    }

    /// 该角色是否可以使用 MCP 工具。
    ///
    /// MCP 工具意味着「以本机身份执行外部动作」（跑工作流、读文件、调接口），
    /// 副作用落在本机别处，比内置工具更难收口——内置工具的副作用都还在本仓的
    /// 边界内（发消息、写记忆）。故只对**对话**角色开放：主动开口轮次没有用户
    /// 的新输入作依据，让它自发调用外部动作等于让它在没人要求时动用户的机器。
    pub fn allows_mcp(self) -> bool {
        matches!(self, Self::Conversation)
    }

    /// 是否可以产生用户可见的投递。身份演化角色永远为 `false`。
    pub fn can_message_user(self) -> bool {
        matches!(self, Self::Conversation | Self::Proactive)
    }

    /// 是否可以写用户记忆。主动与身份演化角色都不能写。
    pub fn can_write_user_memory(self) -> bool {
        matches!(self, Self::Conversation)
    }

    /// 是否允许修改当前身份。任何角色都不能直接改；身份变更必须走提案与审批。
    pub fn can_apply_identity_change(self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_are_stable_and_unique() {
        for (i, role) in AgentRole::ALL.into_iter().enumerate() {
            assert_eq!(role.as_str(), role.as_str().trim());
            for other in &AgentRole::ALL[i + 1..] {
                assert_ne!(role.as_str(), other.as_str());
            }
        }
    }

    #[test]
    fn identity_evolution_cannot_message_or_write() {
        let role = AgentRole::IdentityEvolution;
        assert!(!role.can_message_user());
        assert!(!role.can_write_user_memory());
        assert!(!role.can_apply_identity_change());
        for name in [
            tool::SEND_MESSAGE,
            tool::SEND_DICE,
            tool::SEND_STICKER,
            tool::GENERATE_IMAGE,
            tool::GENERATE_VOICE,
            tool::REMEMBER,
        ] {
            assert!(!role.allows_tool(name), "身份角色不得拥有工具 {name}");
        }
        assert!(role.allows_tool(tool::GET_NOW));
        assert!(role.allows_tool(tool::RECALL_MEMORY));
    }

    #[test]
    fn proactive_can_message_but_cannot_write_memory() {
        let role = AgentRole::Proactive;
        assert!(role.can_message_user());
        assert!(!role.can_write_user_memory());
        assert!(role.allows_tool(tool::SEND_MESSAGE));
        assert!(!role.allows_tool(tool::REMEMBER));
        assert!(!role.allows_tool(tool::SEND_DICE));
    }

    #[test]
    fn only_conversation_role_may_use_mcp_tools() {
        assert!(AgentRole::Conversation.allows_mcp());
        assert!(
            !AgentRole::Proactive.allows_mcp(),
            "主动开口不得自发调用外部工具"
        );
        assert!(
            !AgentRole::IdentityEvolution.allows_mcp(),
            "身份演化不得调用外部工具"
        );
    }

    #[test]
    fn conversation_has_the_full_current_tool_set() {
        let role = AgentRole::Conversation;
        assert!(role.can_message_user());
        assert!(role.can_write_user_memory());
        for name in [
            tool::SEND_MESSAGE,
            tool::SEND_DICE,
            tool::SEND_STICKER,
            tool::GENERATE_IMAGE,
            tool::GENERATE_VOICE,
            tool::GET_NOW,
            tool::RECALL_MEMORY,
            tool::REMEMBER,
        ] {
            assert!(role.allows_tool(name), "对话角色应拥有工具 {name}");
        }
    }

    #[test]
    fn no_role_can_apply_identity_changes_directly() {
        for role in AgentRole::ALL {
            assert!(!role.can_apply_identity_change());
        }
    }
}
