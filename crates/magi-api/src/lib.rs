#![recursion_limit = "256"]

mod browser_tool_runtime;
mod change_projection;
mod dto;
mod errors;
pub mod git_tool_runtime;
mod host_paths;
pub mod mcp_config;
mod model_config;
mod public_canonical;
mod routes;
mod scope_binding;
pub(crate) mod session_continue;
pub mod session_title;
pub mod skill_loader;
mod snapshot_lifecycle;
mod sse;
mod state;
mod task_dispatch;
pub mod task_turn_finalize;
mod terminal_runtime;
pub mod tunnel;

pub use browser_tool_runtime::BrowserToolRuntimeDependencies;
pub use dto::DirectHttpModelProbeConfig;
pub use errors::{ApiError, ErrorResponseDto};
pub use routes::build_router;
pub use state::{
    ApiState, BrowserRuntimeStatusSnapshot, ExecutionResourceCancellationReport,
    ExecutionResourceCoordinator, RunnerManager, RunnerStartError, RunnerStopError,
    RuntimeStatePersistence, build_runtime_capability_dependency_provider,
};
