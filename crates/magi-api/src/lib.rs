#![recursion_limit = "256"]

mod app_server;
mod browser_image;
mod browser_tool_runtime;
mod change_projection;
mod dto;
mod errors;
pub mod git_tool_runtime;
mod host_paths;
pub mod mcp_config;
mod mcp_runtime;
mod mcp_service;
mod mcp_tunnel;
mod model_config;
mod performance;
mod public_canonical;
mod routes;
mod scope_binding;
mod session_activity;
pub(crate) mod session_continue;
pub mod session_title;
pub mod builtin_skills;
mod mcp_direct;
pub mod skill_loader;
mod snapshot_lifecycle;
mod sse;
mod state;
mod task_dispatch;
pub mod task_turn_finalize;
mod terminal_runtime;
pub mod tunnel;
mod tunnel_client_install;
#[cfg(test)]
mod turn_harness;
mod turn_service;
mod web_model_channel;
mod web_model_driver;
pub mod web_model_ops;
pub mod web_slot_mcp;

pub use browser_tool_runtime::BrowserToolRuntimeDependencies;
pub use dto::{DaemonIdentity, DirectHttpModelProbeConfig};
pub use errors::{ApiError, ErrorResponseDto};
pub use routes::build_router;
pub use web_model_channel::{
    WEB_MODEL_TUNNEL_SECTION, WebModelChannelRuntime, WebModelTunnelConfig, WebModelTunnelStatus,
};
pub use web_model_driver::{HostWebModelPageDriver, WebModelHostFactory};
pub fn schedule_restored_session_task_dispatches(state: ApiState) {
    routes::schedule_restored_session_task_dispatches(state);
}
pub use state::{
    ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot,
    ExecutionResourceCancellationReport, ExecutionResourceCoordinator, RunnerManager,
    RunnerStartError, RunnerStopError, RuntimeStatePersistence, SessionProjectionPersistMode,
    TaskCheckpointPersist, build_runtime_capability_dependency_provider,
    recover_role_delete_transaction,
};
