use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSourceRole {
    Worker,
    Orchestrator,
    Auxiliary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageScope {
    pub workspace_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub dispatch_wave_id: Option<String>,
    pub assignment_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBindingIdentity {
    pub template_id: String,
    pub engine_id: String,
    pub binding_revision: u32,
    pub role: UsageSourceRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlMode {
    Full,
    Proxy,
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelResolutionIdentity {
    pub provider: String,
    pub declared_model_spec: String,
    pub resolved_model: String,
    pub canonical_base_url: String,
    pub base_url_fingerprint: String,
    pub url_mode: UrlMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_fingerprint: Option<String>,
    pub binding_revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_context_modifiers: Option<Vec<String>>,
    pub model_identity_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePhase {
    Planning,
    Execution,
    Verification,
    Integration,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCallIdentity {
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub source: UsageSourceRole,
    pub phase: UsagePhase,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEventUsageDelta {
    pub raw_input_tokens: u64,
    pub raw_output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_included_in_input: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageEventType {
    LlmCallCompleted,
    LlmCallFailed,
    SessionReset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCallStatus {
    Success,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub event_id: String,
    pub ledger_seq: u64,
    pub workspace_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_wave_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    pub timestamp: u64,
    pub event_type: UsageEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_binding: Option<ExecutionBindingIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_identity: Option<ModelResolutionIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_identity: Option<UsageCallIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_delta: Option<UsageEventUsageDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<UsageCallStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub llm_call_count: u64,
    pub assignment_count: u64,
    pub turn_count: u64,
    pub raw_input_tokens: u64,
    pub raw_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub net_input_tokens: u64,
    pub net_output_tokens: u64,
    pub total_tokens: u64,
    pub success_count: u64,
    pub failure_count: u64,
}

impl UsageTotals {
    pub fn add(&self, other: &UsageTotals) -> UsageTotals {
        UsageTotals {
            llm_call_count: self.llm_call_count + other.llm_call_count,
            assignment_count: self.assignment_count + other.assignment_count,
            turn_count: self.turn_count + other.turn_count,
            raw_input_tokens: self.raw_input_tokens + other.raw_input_tokens,
            raw_output_tokens: self.raw_output_tokens + other.raw_output_tokens,
            cache_read_tokens: self.cache_read_tokens + other.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + other.cache_write_tokens,
            net_input_tokens: self.net_input_tokens + other.net_input_tokens,
            net_output_tokens: self.net_output_tokens + other.net_output_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
            success_count: self.success_count + other.success_count,
            failure_count: self.failure_count + other.failure_count,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBindingSnapshot {
    pub template_id: String,
    pub engine_id: String,
    pub binding_revision: u32,
    pub role: UsageSourceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_model_spec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_identity_key: Option<String>,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelSnapshot {
    pub model_identity_key: String,
    pub provider: String,
    pub declared_model_spec: String,
    pub resolved_model: String,
    pub base_url_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageSnapshot {
    pub workspace_id: String,
    pub session_id: String,
    pub version: u64,
    pub last_applied_ledger_seq: u64,
    pub updated_at: u64,
    pub totals: UsageTotals,
    pub by_execution_binding: Vec<UsageBindingSnapshot>,
    pub by_model_identity: Vec<UsageModelSnapshot>,
}

impl SessionUsageSnapshot {
    pub fn empty(workspace_id: &str, session_id: &str) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            session_id: session_id.to_string(),
            version: 0,
            last_applied_ledger_seq: 0,
            updated_at: 0,
            totals: UsageTotals::default(),
            by_execution_binding: Vec::new(),
            by_model_identity: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub version: u64,
    pub updated_at: u64,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUsageSnapshot {
    pub workspace_id: String,
    pub version: u64,
    pub last_applied_session_snapshot_versions: std::collections::HashMap<String, u64>,
    pub updated_at: u64,
    pub totals: UsageTotals,
    pub by_session: Vec<SessionSummary>,
    pub by_execution_binding: Vec<UsageBindingSnapshot>,
    pub by_model_identity: Vec<UsageModelSnapshot>,
}

impl WorkspaceUsageSnapshot {
    pub fn empty(workspace_id: &str) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            version: 0,
            last_applied_session_snapshot_versions: std::collections::HashMap::new(),
            updated_at: 0,
            totals: UsageTotals::default(),
            by_session: Vec::new(),
            by_execution_binding: Vec::new(),
            by_model_identity: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokenInput {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_included_in_input: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub url_mode: UrlMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCallRecordInput {
    pub workspace_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_wave_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    pub execution_binding: ExecutionBindingIdentity,
    pub model_config: LlmConfig,
    pub call_identity: UsageCallIdentity,
    pub usage: UsageTokenInput,
    pub status: UsageCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}
