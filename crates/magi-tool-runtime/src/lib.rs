mod agent_delegation;
mod apply_patch;
mod background_job;
mod builtin;
mod builtin_catalog;
mod image_generate;
mod policy;
mod registry;
mod tool_catalog;
mod types;
mod view_image;
mod workspace_changes;
pub use agent_delegation::{
    AGENT_DELEGATION_GUIDANCE, AGENT_DELEGATION_RECOVERY_GUIDANCE, AgentDelegationDecision,
    AgentDelegationPurpose,
};
pub use apply_patch::apply_patch_declared_paths_from_input;
pub use background_job::{
    BACKGROUND_JOB_METADATA_KEY, BackgroundJob, BackgroundJobJournal, BackgroundJobStatus,
};
pub use builtin::file_read_result_matches_request;
#[cfg(test)]
pub(crate) use builtin_catalog::RestrictedWriteProfilePolicy;
pub use builtin_catalog::{
    BuiltinToolAccessMode, BuiltinToolInvocationPolicy, BuiltinToolModelScope, BuiltinToolName,
    BuiltinToolSpec, builtin_permission_engine, canonical_builtin_tool_name,
    is_internal_builtin_tool_surface, is_public_builtin_tool_surface,
};
pub(crate) use builtin_catalog::{low_risk_policy, tool_policy_decision_payload};
pub use policy::{
    ToolPathAccessRequest, browser_action_class, canonicalize_tool_permission_path,
    decide_tool_path_access, effective_tool_policy_allowed_paths, external_tool_access_decision,
    normalize_tool_policy_paths, path_access_policy, tool_path_access_requests,
};
pub use registry::ToolRegistry;
pub use types::external_mcp_model_tool_name;
pub use types::{
    AgentRoleCatalogEntry, AgentRoleCatalogProvider, BrowserCapabilityProvider,
    BrowserToolExecutor, BuiltinTool, ExternalMcpServerCatalogEntry, ExternalMcpToolCatalogEntry,
    ExternalMcpToolExecutor, ExternalToolCatalogEntry, ExternalToolCatalogProvider,
    ExternalToolCatalogSnapshot, GeneratedImageData, GitToolExecutor,
    ImageGenerationExecutionContext, ImageGenerationExecutor, ImageGenerationReadinessProvider,
    ImageGenerationRequest, PluginToolCatalogEntry, PluginToolCatalogProvider, PluginToolExecutor,
    RuntimeCapabilityDependencyEntry, RuntimeCapabilityDependencyProvider, ToolCallerScope,
    ToolExecutionContext, ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolExecutionProgress, ToolExecutionSummary, ToolInvocationRecord,
    ToolNameRejection, ToolRuntimeResources, WriteProtectionScope,
};

#[cfg(test)]
mod tests;

/// 工具授权、恢复与去重共用的 JSON 参数规范形式。
pub fn normalized_tool_arguments(arguments: &str) -> String {
    fn canonicalize(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => serde_json::Value::Object(
                object
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonicalize).collect())
            }
            value => value,
        }
    }
    serde_json::from_str::<serde_json::Value>(arguments)
        .map(|value| canonicalize(value).to_string())
        .unwrap_or_else(|_| arguments.trim().into())
}
