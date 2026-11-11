use serde::{Deserialize, Serialize};

/// 模型调用的业务归属。插件操作不会创建或冒充用户会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginModelOwner {
    pub plugin_id: String,
    pub digest: String,
    pub instance_id: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl PluginModelOwner {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.plugin_id, self.digest, self.operation_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum UsageOwner {
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Plugin {
        plugin: PluginModelOwner,
    },
}

impl<'de> Deserialize<'de> for UsageOwner {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fields {
            session_id: Option<String>,
            plugin: Option<PluginModelOwner>,
        }
        let fields = Fields::deserialize(deserializer)?;
        match (fields.session_id, fields.plugin) {
            (Some(session_id), None) if !session_id.trim().is_empty() => {
                Ok(Self::Session { session_id })
            }
            (None, Some(plugin)) if !plugin.plugin_id.trim().is_empty() => {
                Ok(Self::Plugin { plugin })
            }
            _ => Err(serde::de::Error::custom("模型用量必须且只能归属会话或插件")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UsageSubject {
    Session(String),
    Plugin(String),
}

impl UsageOwner {
    pub fn session(session_id: impl Into<String>) -> Self {
        Self::Session {
            session_id: session_id.into(),
        }
    }
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Session { session_id } => Some(session_id),
            Self::Plugin { .. } => None,
        }
    }
    pub fn ledger_key(&self) -> String {
        match self {
            Self::Session { session_id } => session_id.clone(),
            Self::Plugin { plugin } => format!("plugin:{}", plugin.key()),
        }
    }
    pub(crate) fn subject(&self) -> UsageSubject {
        match self {
            Self::Session { session_id } => UsageSubject::Session(session_id.clone()),
            Self::Plugin { plugin } => UsageSubject::Plugin(plugin.plugin_id.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSourceRole {
    Worker,
    Orchestrator,
    Auxiliary,
    ImageGeneration,
    Vision,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBindingIdentity {
    pub template_id: String,
    pub engine_id: String,
    pub binding_revision: u32,
    pub role: UsageSourceRole,
}

pub use magi_core::ReasoningEffort;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelResolutionIdentity {
    pub provider: String,
    pub declared_model_spec: String,
    pub resolved_model: String,
    pub canonical_base_url: String,
    pub base_url_fingerprint: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<UsageTransportIdentity>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTransportIdentity {
    /// Explicit provider usage presence; old receipts keep None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_reported: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_timing: Option<magi_app_server_protocol::ModelResponseTiming>,
    pub request_fingerprint: String,
    pub http_status: Option<u16>,
}

impl UsageTransportIdentity {
    pub fn from_receipt(record: &magi_app_server_protocol::ModelTransportRecord) -> Self {
        // Pre-provenance receipts could contain fabricated all-zero adapter defaults.
        let usage_reported = record.usage_observed.or_else(|| {
            record
                .usage
                .as_ref()
                .filter(|usage| {
                    usage.input_tokens > 0
                        || usage.output_tokens > 0
                        || usage.cache_read_tokens.is_some()
                        || usage.cache_write_tokens.is_some()
                })
                .map(|_| true)
        });
        Self {
            usage_reported,
            response_timing: record.response_timing.clone(),
            request_fingerprint: record.request_fingerprint.clone(),
            http_status: record.http_status,
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(flatten)]
    pub owner: UsageOwner,
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

/// Only successful, observed body streams contribute. Missing timing stays unknown.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePerformanceTotals {
    pub ttft_sample_count: u64,
    pub ttft_total_ms: u64,
    pub tps_sample_count: u64,
    pub text_output_tokens: u64,
    pub content_duration_ms: u64,
    pub last_observed_at: u64,
    pub last_ttft_ms: Option<u64>,
    pub last_tps: Option<f64>,
    /// End-to-end response throughput; includes all reported output and waiting time.
    #[serde(default)]
    pub response_sample_count: u64,
    #[serde(default)]
    pub response_output_tokens: u64,
    #[serde(default)]
    pub response_duration_ms: u64,
    #[serde(default)]
    pub last_response_tps: Option<f64>,
}

impl UsagePerformanceTotals {
    pub fn add(&self, other: &Self) -> Self {
        let last = if other.last_observed_at >= self.last_observed_at {
            other
        } else {
            self
        };
        Self {
            ttft_sample_count: self.ttft_sample_count + other.ttft_sample_count,
            ttft_total_ms: self.ttft_total_ms + other.ttft_total_ms,
            tps_sample_count: self.tps_sample_count + other.tps_sample_count,
            text_output_tokens: self.text_output_tokens + other.text_output_tokens,
            content_duration_ms: self.content_duration_ms + other.content_duration_ms,
            last_observed_at: last.last_observed_at,
            last_ttft_ms: last.last_ttft_ms,
            last_tps: last.last_tps,
            response_sample_count: self.response_sample_count + other.response_sample_count,
            response_output_tokens: self.response_output_tokens + other.response_output_tokens,
            response_duration_ms: self.response_duration_ms + other.response_duration_ms,
            last_response_tps: last.last_response_tps,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    #[serde(default)]
    pub token_observed_call_count: u64,
    #[serde(default)]
    pub performance: UsagePerformanceTotals,
    pub llm_call_count: u64,
    pub assignment_count: u64,
    pub turn_count: u64,
    pub raw_input_tokens: u64,
    pub raw_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cache_write_observed_call_count: u64,
    /// 只有 Provider 明确返回 cache read（包括零）的调用才计入观测样本。
    #[serde(default)]
    pub cache_observed_call_count: u64,
    #[serde(default)]
    pub cache_hit_call_count: u64,
    /// 已观测调用的完整输入量，统一包含缓存读取/写入；可作 token 命中率分母。
    #[serde(default)]
    pub cache_observed_input_tokens: u64,
    pub net_input_tokens: u64,
    pub net_output_tokens: u64,
    pub total_tokens: u64,
    pub success_count: u64,
    pub failure_count: u64,
}

impl UsageTotals {
    pub fn add(&self, other: &UsageTotals) -> UsageTotals {
        UsageTotals {
            performance: self.performance.add(&other.performance),
            token_observed_call_count: self.token_observed_call_count
                + other.token_observed_call_count,
            llm_call_count: self.llm_call_count + other.llm_call_count,
            assignment_count: self.assignment_count + other.assignment_count,
            turn_count: self.turn_count + other.turn_count,
            raw_input_tokens: self.raw_input_tokens + other.raw_input_tokens,
            raw_output_tokens: self.raw_output_tokens + other.raw_output_tokens,
            cache_read_tokens: self.cache_read_tokens + other.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + other.cache_write_tokens,
            cache_write_observed_call_count: self.cache_write_observed_call_count
                + other.cache_write_observed_call_count,
            cache_observed_call_count: self.cache_observed_call_count
                + other.cache_observed_call_count,
            cache_hit_call_count: self.cache_hit_call_count + other.cache_hit_call_count,
            cache_observed_input_tokens: self.cache_observed_input_tokens
                + other.cache_observed_input_tokens,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub session_id: String,
    pub version: u64,
    pub last_applied_ledger_seq: u64,
    pub updated_at: u64,
    pub totals: UsageTotals,
    pub by_execution_binding: Vec<UsageBindingSnapshot>,
    pub by_model_identity: Vec<UsageModelSnapshot>,
}

/// 统计投影的公共聚合部分，既不带会话身份也不制造插件会话。
#[derive(Clone, Debug, Default)]
pub(crate) struct UsageAggregateSnapshot {
    pub version: u64,
    pub last_applied_ledger_seq: u64,
    pub updated_at: u64,
    pub totals: UsageTotals,
    pub by_execution_binding: Vec<UsageBindingSnapshot>,
    pub by_model_identity: Vec<UsageModelSnapshot>,
}

impl UsageAggregateSnapshot {
    pub(crate) fn into_session(
        self,
        workspace_id: Option<&str>,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        SessionUsageSnapshot {
            workspace_id: workspace_id.map(str::to_owned),
            session_id: session_id.into(),
            version: self.version,
            last_applied_ledger_seq: self.last_applied_ledger_seq,
            updated_at: self.updated_at,
            totals: self.totals,
            by_execution_binding: self.by_execution_binding,
            by_model_identity: self.by_model_identity,
        }
    }
}

impl From<&SessionUsageSnapshot> for UsageAggregateSnapshot {
    fn from(s: &SessionUsageSnapshot) -> Self {
        Self {
            version: s.version,
            last_applied_ledger_seq: s.last_applied_ledger_seq,
            updated_at: s.updated_at,
            totals: s.totals.clone(),
            by_execution_binding: s.by_execution_binding.clone(),
            by_model_identity: s.by_model_identity.clone(),
        }
    }
}

impl SessionUsageSnapshot {
    pub fn empty(workspace_id: Option<&str>, session_id: &str) -> Self {
        Self {
            workspace_id: workspace_id.map(ToOwned::to_owned),
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
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCallRecordInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(flatten)]
    pub owner: UsageOwner,
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
