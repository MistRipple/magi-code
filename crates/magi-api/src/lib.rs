#![recursion_limit = "256"]

mod agent_service;
mod app_server;
pub mod browser_downloads;
mod browser_image;
pub mod browser_network_policy;
mod browser_tool_runtime;
pub mod builtin_skills;
mod bundled_plugins;
mod change_projection;
mod dto;
mod errors;
mod external_turns;
pub mod git_tool_runtime;
mod goal_service;
mod host_paths;
mod input_service;
mod interaction_service;
mod isolation;
mod locales;
pub mod mcp_config;
mod mcp_direct;
mod mcp_runtime;
mod mcp_service;
mod mcp_tunnel;
mod model_config;
mod performance;
mod public_canonical;
mod request_guard;
mod routes;
mod safeguard_audit;
mod scope_binding;
mod session_activity;
pub(crate) mod session_continue;
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
mod tunnel_client_install;
#[cfg(test)]
mod turn_harness;
mod turn_service;
mod web_model_channel;
mod web_model_driver;
pub mod web_model_ops;
pub mod web_model_setup;
mod web_question;
pub mod web_slot_mcp;

pub use browser_tool_runtime::BrowserToolRuntimeDependencies;
pub use dto::{DaemonIdentity, DirectHttpModelProbeConfig};
pub use errors::{ApiError, ErrorResponseDto};
pub use routes::build_router;
pub use routes::{write_plugin_resource_with_event, write_plugin_settings_with_event};
pub use state::{
    ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot,
    ExecutionResourceCancellationReport, ExecutionResourceCoordinator, RunnerManager,
    RunnerStartError, RunnerStopError, RuntimeStatePersistence, SessionProjectionPersistMode,
    TaskCheckpointPersist, build_runtime_capability_dependency_provider,
    recover_role_delete_transaction,
};
pub use task_dispatch::{schedule_restored_session_task_dispatches, workspace_execution_preparer};
pub use web_model_channel::{
    WEB_MODEL_TUNNEL_SECTION, WebModelChannelRuntime, WebModelTunnelConfig, WebModelTunnelStatus,
    restore_configured_channel,
};
pub use web_model_driver::{HostWebModelPageDriver, WebModelHostFactory};
