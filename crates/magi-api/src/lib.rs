#![recursion_limit = "256"]

mod change_projection;
mod dto;
mod errors;
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
pub mod tunnel;

pub use dto::DirectHttpModelProbeConfig;
pub use errors::{ApiError, ErrorResponseDto};
pub use routes::build_router;
pub use state::{
    ApiState, RunnerManager, RunnerStartError, RunnerStopError, RuntimeStatePersistence,
    build_runtime_capability_dependency_provider,
};
