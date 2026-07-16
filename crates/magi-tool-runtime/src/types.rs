use crate::{BuiltinToolSpec, canonical_builtin_tool_name};
use magi_core::{
    ApprovalRequirement, ExecutionResultStatus, RiskLevel, SessionId, TaskId, ToolCallId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_governance::{GovernanceDecision, ToolKind};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionInput {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub input: String,
    pub approval_requirement: ApprovalRequirement,
    pub risk_level: RiskLevel,
}

impl ToolExecutionInput {
    pub fn for_builtin_invocation(
        tool_call_id: ToolCallId,
        requested_tool_name: impl AsRef<str>,
        input: impl Into<String>,
    ) -> Self {
        let input = input.into();
        let requested_tool_name = requested_tool_name.as_ref().trim();
        let (tool_name, invocation_policy) =
            if let Some(tool) = crate::BuiltinToolName::from_name(requested_tool_name) {
                (
                    tool.as_str().to_string(),
                    tool.invocation_policy_for_input(&input),
                )
            } else {
                (requested_tool_name.to_string(), crate::low_risk_policy())
            };

        Self {
            tool_call_id,
            tool_name,
            tool_kind: ToolKind::Builtin,
            input,
            approval_requirement: invocation_policy.approval_requirement,
            risk_level: invocation_policy.risk_level,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContext {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub access_profile: magi_core::AccessProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContextQuery {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProtectionScope {
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub task_id: Option<TaskId>,
    pub working_directory: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionOutput {
    pub tool_call_id: ToolCallId,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub governance: GovernanceDecision,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionPolicy {
    pub access_profile: magi_core::AccessProfile,
    pub source_skill_ids: Vec<String>,
    pub allowed_tool_names: Vec<String>,
    pub denied_tool_names: Vec<String>,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub denied_paths: Vec<String>,
    #[serde(default)]
    pub read_only_paths: Vec<String>,
    #[serde(default)]
    pub command_mode: String,
}

impl ToolExecutionPolicy {
    pub fn from_task_policy(policy: &magi_core::TaskPolicy) -> Self {
        let mut tool_policy = Self::default();
        tool_policy.apply_task_policy(policy);
        tool_policy
    }

    pub fn apply_task_policy(&mut self, policy: &magi_core::TaskPolicy) {
        self.access_profile = policy.access_profile;
        self.allowed_paths = policy.allowed_paths.clone();
        self.denied_paths = policy.denied_paths.clone();
        self.read_only_paths = policy.read_only_paths.clone();
        self.command_mode = policy.command_mode.clone();
        extend_unique(&mut self.denied_tool_names, &policy.denied_tools);
        merge_allowed_tools(&mut self.allowed_tool_names, &policy.allowed_tools);
    }

    pub fn effective_access_profile(&self) -> magi_core::AccessProfile {
        if self.command_mode.eq_ignore_ascii_case("read_only") {
            magi_core::AccessProfile::ReadOnly
        } else {
            self.access_profile
        }
    }
}

fn extend_unique(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        let canonical_value = canonical_tool_policy_name(value);
        if !target
            .iter()
            .any(|existing| canonical_tool_policy_name(existing) == canonical_value)
        {
            target.push(value.clone());
        }
    }
}

fn merge_allowed_tools(target: &mut Vec<String>, values: &[String]) {
    if values.is_empty() {
        return;
    }
    if target.is_empty() {
        target.extend(values.iter().cloned());
        return;
    }
    target.retain(|tool_name| {
        let canonical_tool_name = canonical_tool_policy_name(tool_name);
        values
            .iter()
            .any(|value| canonical_tool_policy_name(value) == canonical_tool_name)
    });
}

fn canonical_tool_policy_name(value: &str) -> String {
    canonical_builtin_tool_name(value).unwrap_or_else(|| value.trim().to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocationRecord {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub context: ToolExecutionContext,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionSummary {
    pub total_invocations: usize,
    pub successful_invocations: usize,
    pub blocked_invocations: usize,
    pub failed_invocations: usize,
}

pub type ExternalToolCatalogProvider =
    Arc<dyn Fn() -> ExternalToolCatalogSnapshot + Send + Sync + 'static>;
pub type ExternalMcpToolExecutor =
    Arc<dyn Fn(&str, &str, &str) -> (String, ExecutionResultStatus) + Send + Sync + 'static>;
pub type AgentRoleCatalogProvider =
    Arc<dyn Fn() -> Vec<AgentRoleCatalogEntry> + Send + Sync + 'static>;
pub type RuntimeCapabilityDependencyProvider =
    Arc<dyn Fn() -> Vec<RuntimeCapabilityDependencyEntry> + Send + Sync + 'static>;
pub type ImageGenerationExecutor = Arc<
    dyn Fn(ImageGenerationRequest) -> Result<GeneratedImageData, String> + Send + Sync + 'static,
>;
pub type ImageGenerationReadinessProvider = Arc<dyn Fn() -> bool + Send + Sync + 'static>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub size: String,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImageData {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub revised_prompt: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalToolCatalogSnapshot {
    #[serde(default)]
    pub instruction_skill_count: usize,
    pub skill_tools: Vec<ExternalToolCatalogEntry>,
    pub mcp_servers: Vec<ExternalMcpServerCatalogEntry>,
    pub mcp_tools: Vec<ExternalMcpToolCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalMcpToolCatalogEntry {
    pub server_id: String,
    pub server_name: String,
    pub model_tool_name: String,
    pub tool_name: String,
    pub description: String,
    #[serde(default)]
    pub read_only: bool,
    pub input_schema: serde_json::Value,
}

pub fn external_mcp_model_tool_name(server_id: &str, tool_name: &str) -> String {
    fn segment(value: &str) -> String {
        let normalized = value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let normalized = normalized.trim_matches('_');
        if normalized.is_empty() {
            "unnamed".to_string()
        } else {
            normalized.to_string()
        }
    }

    let server = segment(server_id);
    let tool = segment(tool_name);
    let full = format!("mcp__{server}__{tool}");
    let identifiers_changed = server != server_id || tool != tool_name;
    if full.len() <= 64 && !identifiers_changed {
        return full;
    }

    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("{server_id}\0{tool_name}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let suffix = format!("__{hash:016x}");
    let max_prefix_len = 64usize.saturating_sub(suffix.len());
    let prefix = full.chars().take(max_prefix_len).collect::<String>();
    format!("{prefix}{suffix}")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalToolCatalogEntry {
    pub source: String,
    pub skill_id: Option<String>,
    pub binding_id: Option<String>,
    pub name: String,
    pub description: String,
    pub bridge_kind: String,
    pub dispatch_action: String,
    pub bridge_target: String,
    pub access_profile_behavior: String,
    pub risk_level: String,
    pub approval_requirement: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalMcpServerCatalogEntry {
    pub server_id: String,
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub health: String,
    pub tool_count: Option<usize>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRoleCatalogEntry {
    pub role_id: String,
    pub spawnable: bool,
    pub coordinator_mode: bool,
    pub supported_kinds: Vec<String>,
    pub parallelism_limit: Option<u32>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCapabilityDependencyEntry {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawnable_role_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<usize>,
}

/// 工具执行时可用的进程内运行时资源（非序列化句柄）。
///
/// 与 ToolExecutionContext（可序列化、随调用流转的标识信息）区分：这里承载的是
/// daemon 进程内的共享服务引用，由 ToolRegistry 持有并在 dispatch 时传入。
#[derive(Clone, Default)]
pub struct ToolRuntimeResources {
    pub knowledge_store: Option<Arc<magi_knowledge_store::KnowledgeStore>>,
    pub external_tool_catalog_provider: Option<ExternalToolCatalogProvider>,
    pub external_mcp_tool_executor: Option<ExternalMcpToolExecutor>,
    pub agent_role_catalog_provider: Option<AgentRoleCatalogProvider>,
    pub runtime_capability_dependency_provider: Option<RuntimeCapabilityDependencyProvider>,
    pub image_generation_executor: Option<ImageGenerationExecutor>,
    pub image_generation_readiness_provider: Option<ImageGenerationReadinessProvider>,
}

pub trait BuiltinTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        input: &str,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> String;
    fn spec(&self) -> BuiltinToolSpec;
}
