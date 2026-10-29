//! AI 身份领域模型：可演化的自我属性字段、校验规则与当前身份快照。
//!
//! 本模块只定义**纯领域契约**，不读写数据库，也不生成提示词。持久化、版本审计和
//! 提案审批由后续身份服务接入；这里先把“哪些字段可以改、值必须满足什么条件”固定下来，
//! 避免自动演化直接作用在无约束的 KV 文本上。
//!
//! 当前 MVP 的六个字段全部是 [`Mutability::ApprovalRequired`]：模型可以提出变更，
//! 但任何变更都必须先经过人工确认，不能自动写入当前身份。

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde_json::Value;
use thiserror::Error;

/// 身份字段的 canonical key。
///
/// 枚举顺序就是提示词渲染顺序，后续版本化渲染直接复用，避免另维护一份字段顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdentityFieldKey {
    SelfConcept,
    Personality,
    Values,
    VoiceStyle,
    Interests,
    RelationshipStance,
}

impl IdentityFieldKey {
    /// 全部字段，顺序固定。
    pub const ALL: [Self; 6] = [
        Self::SelfConcept,
        Self::Personality,
        Self::Values,
        Self::VoiceStyle,
        Self::Interests,
        Self::RelationshipStance,
    ];

    /// canonical key（用于数据库、提案和审计记录）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfConcept => "self_concept",
            Self::Personality => "personality",
            Self::Values => "values",
            Self::VoiceStyle => "voice_style",
            Self::Interests => "interests",
            Self::RelationshipStance => "relationship_stance",
        }
    }

    /// 中文展示标签。仅用于提示词渲染和管理入口展示，不用于持久化。
    pub fn label(self) -> &'static str {
        match self {
            Self::SelfConcept => "自我概念",
            Self::Personality => "性格",
            Self::Values => "价值观",
            Self::VoiceStyle => "表达风格",
            Self::Interests => "兴趣",
            Self::RelationshipStance => "关系姿态",
        }
    }

    /// 兼容旧 `self_profile` 中文 key 的别名。
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::SelfConcept => &["身份定位", "自我概念"],
            Self::Personality => &["性格", "人格"],
            Self::Values => &["三观", "价值观"],
            Self::VoiceStyle => &["语气", "说话风格", "表达风格"],
            Self::Interests => &["爱好", "兴趣"],
            Self::RelationshipStance => &["相处姿态", "关系姿态"],
        }
    }

    /// 解析 canonical key 或旧中文别名。
    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(IdentityError::UnknownField(raw.to_string()));
        }

        let canonical = trimmed.to_ascii_lowercase();
        if let Some(key) = Self::ALL.into_iter().find(|key| key.as_str() == canonical) {
            return Ok(key);
        }
        Self::ALL
            .into_iter()
            .find(|key| key.aliases().contains(&trimmed))
            .ok_or_else(|| IdentityError::UnknownField(trimmed.to_string()))
    }

    /// 字段的固定规则。
    pub fn spec(self) -> IdentityFieldSpec {
        let (max_chars, description) = match self {
            Self::SelfConcept => (400, "我是谁、如何看待自己"),
            Self::Personality => (400, "稳定的人格与性格倾向"),
            Self::Values => (500, "价值立场、原则与底线"),
            Self::VoiceStyle => (300, "表达长短、语气与称呼习惯"),
            Self::Interests => (400, "会自然带出的兴趣与偏好"),
            Self::RelationshipStance => (300, "亲近程度、边界感与相处姿态"),
        };
        IdentityFieldSpec {
            key: self,
            mutability: Mutability::ApprovalRequired,
            max_chars,
            requires_explicit_consent: matches!(self, Self::RelationshipStance),
            description,
        }
    }
}

impl fmt::Display for IdentityFieldKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 字段可变性。MVP 六个字段全部需要人工批准。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    Immutable,
    ApprovalRequired,
    LowRisk,
}

/// 身份变更提案状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Applied,
    RolledBack,
}

impl ProposalStatus {
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::Approved,
        Self::Rejected,
        Self::Applied,
        Self::RolledBack,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Applied => "applied",
            Self::RolledBack => "rolled_back",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        let raw = raw.trim();
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == raw)
            .ok_or_else(|| IdentityError::UnknownProposalStatus(raw.to_string()))
    }
}

/// 提案风险等级。MVP 由代码规则评估，不采信模型自报。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        let raw = raw.trim();
        Self::ALL
            .into_iter()
            .find(|risk| risk.as_str() == raw)
            .ok_or_else(|| IdentityError::UnknownRiskLevel(raw.to_string()))
    }
}

/// 身份版本来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSource {
    Manual,
    EvolutionAgent,
    Rollback,
}

impl ChangeSource {
    pub const ALL: [Self; 3] = [Self::Manual, Self::EvolutionAgent, Self::Rollback];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::EvolutionAgent => "evolution_agent",
            Self::Rollback => "rollback",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        let raw = raw.trim();
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == raw)
            .ok_or_else(|| IdentityError::UnknownChangeSource(raw.to_string()))
    }
}

/// 字段的固定约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityFieldSpec {
    pub key: IdentityFieldKey,
    pub mutability: Mutability,
    pub max_chars: usize,
    /// 关系姿态等敏感字段还要求用户在审批时明确同意，而不只是点击通过。
    pub requires_explicit_consent: bool,
    pub description: &'static str,
}

/// 通过校验的身份字段值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityFieldValue(String);

impl IdentityFieldValue {
    /// 校验并构造字段值。
    ///
    /// 只做结构与内容安全校验，不判断语义是否合理；语义风险由后续提案审批负责。
    pub fn new(key: IdentityFieldKey, raw: &str) -> Result<Self, IdentityError> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(IdentityError::EmptyValue { field: key });
        }

        let spec = key.spec();
        let chars = value.chars().count();
        if chars > spec.max_chars {
            return Err(IdentityError::ValueTooLong {
                field: key,
                max_chars: spec.max_chars,
                actual_chars: chars,
            });
        }
        if let Some(ch) = value.chars().find(|ch| forbidden_char(*ch)) {
            return Err(IdentityError::ForbiddenCharacter {
                field: key,
                code: ch as u32,
            });
        }

        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn forbidden_char(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')
}

/// 当前身份快照。
///
/// `version` 必须单调递增；具体持久化和事务提交在后续任务实现。本类型只保证内存中的
/// 字段值已经通过校验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    version: u64,
    fields: BTreeMap<IdentityFieldKey, IdentityFieldValue>,
    updated_at: DateTime<Utc>,
}

impl AgentIdentity {
    /// MVP 初始版本号。
    pub const INITIAL_VERSION: u64 = 1;

    /// 尚未写入版本审计表的旧 `self_profile` 数据。
    pub const UNVERSIONED: u64 = 0;

    /// 构造一个空身份。
    pub fn empty(version: u64, updated_at: DateTime<Utc>) -> Self {
        Self {
            version,
            fields: BTreeMap::new(),
            updated_at,
        }
    }

    /// 用已校验字段构造身份。重复字段由 `BTreeMap` 自然收敛。
    pub fn new(
        version: u64,
        updated_at: DateTime<Utc>,
        fields: BTreeMap<IdentityFieldKey, IdentityFieldValue>,
    ) -> Self {
        Self {
            version,
            fields,
            updated_at,
        }
    }

    /// 从旧 `self_profile` 行构造身份，兼容中文 key。
    ///
    /// 同一 canonical 字段被多个别名命中时按输入顺序用「；」合并；完全相同的值只保留
    /// 一次。合并后再做长度与内容校验，避免旧库中两条短值拼成一条超长值而未被发现。
    pub fn from_legacy_rows<I, K, V>(
        version: u64,
        updated_at: DateTime<Utc>,
        rows: I,
    ) -> Result<Self, IdentityError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut merged: BTreeMap<IdentityFieldKey, String> = BTreeMap::new();
        for (raw_key, raw_value) in rows {
            let key = IdentityFieldKey::parse(raw_key.as_ref())?;
            let value = raw_value.as_ref().trim();
            match merged.get_mut(&key) {
                Some(existing) if existing == value => {}
                Some(existing) => {
                    existing.push('；');
                    existing.push_str(value);
                }
                None => {
                    merged.insert(key, value.to_string());
                }
            }
        }

        let fields = merged
            .into_iter()
            .map(|(key, value)| IdentityFieldValue::new(key, &value).map(|v| (key, v)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(Self::new(version, updated_at, fields))
    }

    /// 从 `self_profile_versions.snapshot` 解析完整身份快照。
    ///
    /// 快照必须是 JSON object，字段值必须是字符串；未知字段、非字符串值和字段约束
    /// 校验失败都会返回错误。调用方在读取当前身份时可以选择回退到旧 KV 渲染，但
    /// 不应把损坏的版本快照当成有效身份继续使用。
    pub fn from_snapshot_json(
        version: u64,
        updated_at: DateTime<Utc>,
        snapshot: &str,
    ) -> Result<Self, IdentityError> {
        let raw: BTreeMap<String, Value> = serde_json::from_str(snapshot)
            .map_err(|e| IdentityError::InvalidSnapshot(e.to_string()))?;
        let mut fields = BTreeMap::new();

        for (raw_key, raw_value) in raw {
            let key = IdentityFieldKey::parse(&raw_key)?;
            let value = raw_value.as_str().ok_or_else(|| {
                IdentityError::InvalidSnapshot(format!("field `{raw_key}` must be a string"))
            })?;
            let value = IdentityFieldValue::new(key, value)?;
            if fields.insert(key, value).is_some() {
                return Err(IdentityError::DuplicateField { field: key });
            }
        }

        Ok(Self::new(version, updated_at, fields))
    }

    /// 序列化为可写入 `self_profile_versions.snapshot` 的 canonical JSON。
    ///
    /// 只写当前存在的字段；空身份序列化为 `{}`，读取时仍可得到空身份。
    pub fn to_snapshot_json(&self) -> Result<String, serde_json::Error> {
        let raw = self
            .fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<BTreeMap<_, _>>();
        serde_json::to_string(&raw)
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn get(&self, key: IdentityFieldKey) -> Option<&IdentityFieldValue> {
        self.fields.get(&key)
    }

    pub fn fields(&self) -> &BTreeMap<IdentityFieldKey, IdentityFieldValue> {
        &self.fields
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// 按固定字段顺序渲染为「标签：值；…」文本。
    ///
    /// 这是领域层的 canonical 渲染；`Profile::snapshot()` 在存在版本记录时使用它，
    /// 无版本记录或快照损坏时仍回退到旧 KV 的插入序渲染。
    pub fn render_block(&self) -> String {
        IdentityFieldKey::ALL
            .into_iter()
            .filter_map(|key| {
                self.get(key)
                    .map(|value| format!("{}：{}", key.label(), value.as_str()))
            })
            .collect::<Vec<_>>()
            .join("；")
    }
}

/// 身份契约校验错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentityError {
    #[error("unknown identity field `{0}`")]
    UnknownField(String),
    #[error("identity field `{field}` must not be empty")]
    EmptyValue { field: IdentityFieldKey },
    #[error("identity field `{field}` exceeds {max_chars} chars (got {actual_chars})")]
    ValueTooLong {
        field: IdentityFieldKey,
        max_chars: usize,
        actual_chars: usize,
    },
    #[error("identity field `{field}` contains forbidden character U+{code:04X}")]
    ForbiddenCharacter { field: IdentityFieldKey, code: u32 },
    #[error("identity snapshot is invalid: {0}")]
    InvalidSnapshot(String),
    #[error("identity snapshot contains duplicate field `{field}`")]
    DuplicateField { field: IdentityFieldKey },
    #[error("unknown proposal status `{0}`")]
    UnknownProposalStatus(String),
    #[error("unknown risk level `{0}`")]
    UnknownRiskLevel(String),
    #[error("unknown identity change source `{0}`")]
    UnknownChangeSource(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn canonical_keys_are_stable_and_unique() {
        let keys = IdentityFieldKey::ALL;
        assert_eq!(keys.len(), 6);
        for (i, key) in keys.into_iter().enumerate() {
            assert_eq!(key.as_str(), key.as_str().trim());
            assert!(!key.label().is_empty());
            assert!(!key.spec().description.is_empty());
            assert_eq!(key.spec().key, key);
            assert_eq!(
                IdentityFieldKey::parse(key.as_str()).unwrap(),
                key,
                "canonical key 必须可解析"
            );
            for other in &keys[i + 1..] {
                assert_ne!(key.as_str(), other.as_str(), "canonical key 不得重复");
            }
        }
    }

    #[test]
    fn legacy_aliases_map_to_canonical_fields() {
        let cases = [
            ("身份定位", IdentityFieldKey::SelfConcept),
            ("自我概念", IdentityFieldKey::SelfConcept),
            ("性格", IdentityFieldKey::Personality),
            ("人格", IdentityFieldKey::Personality),
            ("三观", IdentityFieldKey::Values),
            ("价值观", IdentityFieldKey::Values),
            ("语气", IdentityFieldKey::VoiceStyle),
            ("说话风格", IdentityFieldKey::VoiceStyle),
            ("爱好", IdentityFieldKey::Interests),
            ("兴趣", IdentityFieldKey::Interests),
            ("相处姿态", IdentityFieldKey::RelationshipStance),
            ("关系姿态", IdentityFieldKey::RelationshipStance),
        ];
        for (alias, expected) in cases {
            assert_eq!(IdentityFieldKey::parse(alias).unwrap(), expected);
        }
        assert_eq!(
            IdentityFieldKey::parse("  PERSONALITY ").unwrap(),
            IdentityFieldKey::Personality
        );
    }

    #[test]
    fn unknown_or_empty_field_is_rejected() {
        for raw in ["", "   ", "情绪", "system_prompt", "tool_policy"] {
            assert!(matches!(
                IdentityFieldKey::parse(raw),
                Err(IdentityError::UnknownField(_))
            ));
        }
    }

    #[test]
    fn empty_value_is_rejected() {
        for raw in ["", "   ", "\t\n"] {
            assert_eq!(
                IdentityFieldValue::new(IdentityFieldKey::VoiceStyle, raw),
                Err(IdentityError::EmptyValue {
                    field: IdentityFieldKey::VoiceStyle
                })
            );
        }
    }

    #[test]
    fn max_length_is_counted_in_chars_and_is_inclusive() {
        let key = IdentityFieldKey::Interests;
        let max = key.spec().max_chars;
        let exact = "字".repeat(max);
        assert_eq!(
            IdentityFieldValue::new(key, &exact).unwrap().as_str(),
            exact
        );

        let too_long = "字".repeat(max + 1);
        assert_eq!(
            IdentityFieldValue::new(key, &too_long),
            Err(IdentityError::ValueTooLong {
                field: key,
                max_chars: max,
                actual_chars: max + 1,
            })
        );
    }

    #[test]
    fn control_and_line_separator_chars_are_rejected() {
        for raw in ["坏\u{0007}人", "第一行\n第二行", "上\u{2028}下"] {
            assert!(matches!(
                IdentityFieldValue::new(IdentityFieldKey::Personality, raw),
                Err(IdentityError::ForbiddenCharacter {
                    field: IdentityFieldKey::Personality,
                    ..
                })
            ));
        }
    }

    #[test]
    fn legacy_rows_merge_aliases_in_input_order() {
        let identity = AgentIdentity::from_legacy_rows(
            AgentIdentity::INITIAL_VERSION,
            at(),
            [
                ("性格", "慢热"),
                ("人格", "INFP"),
                ("爱好", "旧书店"),
                ("兴趣", "旧书店"),
            ],
        )
        .unwrap();

        assert_eq!(
            identity
                .get(IdentityFieldKey::Personality)
                .unwrap()
                .as_str(),
            "慢热；INFP"
        );
        assert_eq!(
            identity.get(IdentityFieldKey::Interests).unwrap().as_str(),
            "旧书店",
            "完全相同的别名值只保留一次"
        );
        assert_eq!(identity.render_block(), "性格：慢热；INFP；兴趣：旧书店");
    }

    #[test]
    fn legacy_rows_reject_unknown_fields_and_empty_values() {
        assert_eq!(
            AgentIdentity::from_legacy_rows(
                AgentIdentity::INITIAL_VERSION,
                at(),
                [("情绪", "低落")]
            ),
            Err(IdentityError::UnknownField("情绪".to_string()))
        );
        assert_eq!(
            AgentIdentity::from_legacy_rows(
                AgentIdentity::INITIAL_VERSION,
                at(),
                [("性格", "   ")]
            ),
            Err(IdentityError::EmptyValue {
                field: IdentityFieldKey::Personality
            })
        );
    }

    #[test]
    fn snapshot_json_roundtrips_with_canonical_field_order() {
        let identity = AgentIdentity::from_legacy_rows(
            7,
            at(),
            [("兴趣", "旧书店"), ("性格", "慢热"), ("三观", "真诚")],
        )
        .unwrap();

        let snapshot = identity.to_snapshot_json().unwrap();
        assert_eq!(
            snapshot,
            r#"{"interests":"旧书店","personality":"慢热","values":"真诚"}"#
        );
        assert_eq!(
            AgentIdentity::from_snapshot_json(7, at(), &snapshot).unwrap(),
            identity
        );
        assert_eq!(
            AgentIdentity::from_snapshot_json(7, at(), &snapshot)
                .unwrap()
                .render_block(),
            "性格：慢热；价值观：真诚；兴趣：旧书店"
        );
    }

    #[test]
    fn snapshot_json_rejects_malformed_unknown_and_non_string_values() {
        for snapshot in [
            "not-json",
            r#"{"mood":"低落"}"#,
            r#"{"personality":42}"#,
            r#"{"personality":null}"#,
        ] {
            assert!(matches!(
                AgentIdentity::from_snapshot_json(1, at(), snapshot),
                Err(IdentityError::InvalidSnapshot(_)) | Err(IdentityError::UnknownField(_))
            ));
        }
    }

    #[test]
    fn snapshot_json_rejects_empty_or_oversized_values() {
        assert!(matches!(
            AgentIdentity::from_snapshot_json(1, at(), r#"{"personality":"  "}"#),
            Err(IdentityError::EmptyValue {
                field: IdentityFieldKey::Personality
            })
        ));
        let too_long = "字".repeat(IdentityFieldKey::Interests.spec().max_chars + 1);
        let snapshot = serde_json::json!({ "interests": too_long }).to_string();
        assert!(matches!(
            AgentIdentity::from_snapshot_json(1, at(), &snapshot),
            Err(IdentityError::ValueTooLong {
                field: IdentityFieldKey::Interests,
                ..
            })
        ));
    }

    #[test]
    fn snapshot_json_rejects_duplicate_canonical_aliases() {
        assert_eq!(
            AgentIdentity::from_snapshot_json(1, at(), r#"{"personality":"慢热","性格":"INFP"}"#),
            Err(IdentityError::DuplicateField {
                field: IdentityFieldKey::Personality
            })
        );
    }

    #[test]
    fn merged_alias_value_is_validated_after_merge() {
        let key = IdentityFieldKey::Personality;
        let half = "字".repeat(key.spec().max_chars / 2 + 1);
        let other = "另".repeat(key.spec().max_chars / 2 + 1);
        let err = AgentIdentity::from_legacy_rows(
            AgentIdentity::INITIAL_VERSION,
            at(),
            [("性格", half.as_str()), ("人格", other.as_str())],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            IdentityError::ValueTooLong {
                field: IdentityFieldKey::Personality,
                ..
            }
        ));
    }

    #[test]
    fn mvp_fields_require_approval_and_relationship_requires_consent() {
        for key in IdentityFieldKey::ALL {
            let spec = key.spec();
            assert_eq!(spec.mutability, Mutability::ApprovalRequired);
            assert_eq!(
                spec.requires_explicit_consent,
                matches!(key, IdentityFieldKey::RelationshipStance)
            );
        }
    }

    #[test]
    fn proposal_statuses_roundtrip_and_reject_unknown() {
        for status in ProposalStatus::ALL {
            assert_eq!(ProposalStatus::parse(status.as_str()).unwrap(), status);
        }
        assert!(matches!(
            ProposalStatus::parse("done"),
            Err(IdentityError::UnknownProposalStatus(_))
        ));
    }

    #[test]
    fn risk_levels_roundtrip_and_are_ordered() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
        for risk in RiskLevel::ALL {
            assert_eq!(RiskLevel::parse(risk.as_str()).unwrap(), risk);
        }
        assert!(matches!(
            RiskLevel::parse("critical"),
            Err(IdentityError::UnknownRiskLevel(_))
        ));
    }

    #[test]
    fn change_sources_roundtrip_and_reject_unknown() {
        for source in ChangeSource::ALL {
            assert_eq!(ChangeSource::parse(source.as_str()).unwrap(), source);
        }
        assert!(matches!(
            ChangeSource::parse("model"),
            Err(IdentityError::UnknownChangeSource(_))
        ));
    }
}
