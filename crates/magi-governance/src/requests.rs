use magi_core::{ApprovalRequirement, RiskLevel};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Builtin,
    Mcp,
    SkillBound,
    HostBound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionRequest {
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
}
