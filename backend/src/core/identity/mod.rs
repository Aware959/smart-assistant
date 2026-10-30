//! AI 身份领域：可演化自我属性的唯一入口。
//!
//! - [`contract`]：字段白名单、值校验、风险与状态契约；
//! - [`service`]：提案校验、限流、审批、事务应用与回滚；
//! - [`evolution`]：后台演化 Agent，从证据生成待审提案；
//! - [`profile`]：当前身份快照到提示词人设区块的渲染缓存。

pub mod contract;
pub mod evolution;
pub mod profile;
pub mod service;

pub use contract::{
    AgentIdentity, ChangeSource, IdentityError, IdentityFieldKey, IdentityFieldSpec,
    IdentityFieldValue, Mutability, ProposalStatus, RiskLevel,
};
pub use evolution::{
    EvidenceMemory, EvidenceMessage, EvidenceSnapshot, EvolutionConfig, IdentityEvolutionAgent,
    StructuredLlm,
};
pub use profile::Profile;
pub use service::{
    CreatedProposal, EvolutionGate, IdentityService, IdentityVersionView, IdentityView,
    ProposalDraft, ProposalLimits, ProposalView,
};
