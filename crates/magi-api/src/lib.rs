#![recursion_limit = "256"]

mod agent_service;
mod app_server;
pub mod browser_downloads;
mod browser_image;
pub mod browser_network_policy;
mod browser_tool_runtime;
pub mod builtin_skills;
mod change_projection;
mod continuation;
mod dto;
mod errors;
mod external_turns;
pub mod git_tool_runtime;
mod goal_service;
mod host_paths;
mod image_response;
mod input_service;
mod interaction_service;
mod isolation;
mod locales;
pub mod mcp_config;
mod mcp_direct;
mod mcp_runtime;
mod mcp_service;
mod mcp_sessions;
mod mcp_tunnel;
mod model_config;
pub mod native_plugins;
mod performance;
mod plugin_messages;
pub mod plugin_session_control;
pub mod plugin_session_facts;
pub use plugin_messages::plugin_session_message_admitter;
mod public_canonical;
mod request_guard;
mod routes;
mod safeguard_audit;
mod scope_binding;
mod session_activity;
pub(crate) mod session_continue;
mod session_lifecycle;
pub mod session_title;
pub mod skill_loader;
mod snapshot_lifecycle;
pub use snapshot_lifecycle::managed_snapshot_manager;
mod sse;
mod state;
mod task_dispatch;
pub mod task_turn_finalize;
mod terminal_runtime;
pub mod tunnel;
#[cfg(test)]
mod turn_harness;
mod turn_service;

pub use browser_tool_runtime::BrowserToolRuntimeDependencies;
pub use dto::DaemonIdentity;
pub use errors::{ApiError, ErrorResponseDto};
pub use native_plugins::{NativePlatformFuture, NativePluginPlatform, PluginClientSlotsReport};
pub use routes::build_router;
pub use routes::{NpmArchive, fetch_npm_archive, fetch_npm_packument, fetch_npm_release_archive};
pub use state::{
    ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot,
    ExecutionResourceCancellationReport, ExecutionResourceCoordinator, RunnerManager,
    RunnerStartError, RunnerStopError, RuntimeStatePersistence, SessionProjectionPersistMode,
    TaskCheckpointPersist, build_runtime_capability_dependency_provider,
    recover_role_delete_transaction,
};
pub use task_dispatch::{schedule_restored_session_task_dispatches, workspace_execution_preparer};
