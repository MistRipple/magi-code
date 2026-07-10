mod apply_patch;
mod builtin;
mod builtin_catalog;
mod policy;
mod registry;
mod tool_catalog;
mod types;
mod view_image;
mod workspace_changes;
pub use apply_patch::apply_patch_declared_paths_from_input;
#[cfg(test)]
pub(crate) use builtin_catalog::RestrictedWriteProfilePolicy;
pub use builtin_catalog::{
    BuiltinToolAccessMode, BuiltinToolInvocationPolicy, BuiltinToolName, BuiltinToolSpec,
    builtin_permission_engine, canonical_builtin_tool_name, is_internal_builtin_tool_surface,
    is_public_builtin_tool_surface,
};
pub(crate) use builtin_catalog::{low_risk_policy, tool_policy_decision_payload};
pub use policy::{
    ToolPathAccessRequest, canonicalize_tool_permission_path, effective_tool_policy_allowed_paths,
    normalize_tool_policy_paths, tool_path_access_requests,
};
pub use registry::ToolRegistry;
pub use types::external_mcp_model_tool_name;
pub use types::{
    AgentRoleCatalogEntry, AgentRoleCatalogProvider, BuiltinTool, ExternalMcpServerCatalogEntry,
    ExternalMcpToolCatalogEntry, ExternalMcpToolExecutor, ExternalToolCatalogEntry,
    ExternalToolCatalogProvider, ExternalToolCatalogSnapshot, RuntimeCapabilityDependencyEntry,
    RuntimeCapabilityDependencyProvider, ToolExecutionContext, ToolExecutionContextQuery,
    ToolExecutionInput, ToolExecutionOutput, ToolExecutionPolicy, ToolExecutionSummary,
    ToolInvocationRecord, ToolRuntimeResources, WriteProtectionScope,
};

#[cfg(test)]
mod tests;
