mod directories;
mod identity;
mod models;
mod projects;
mod registry;

pub use identity::{
    existing_workspace_identity, resolve_or_create_workspace_identity,
    verify_or_create_workspace_identity,
};
pub use models::{
    RecoveryHandle, RecoveryStatus, SnapshotRecord, WorkspaceDurableState,
    WorkspaceProjectionInput, WorkspaceRecord, WorkspaceRecoveryFlushMetadata,
    WorkspaceRecoveryFlushReason, WorkspaceRecoverySidecarExport,
    WorkspaceRecoverySidecarStoreState, WorkspaceSourceFolder, WorkspaceStoreState,
    WorktreeAllocation,
};
pub use projects::WorkspaceProjectEdit;
pub use registry::WorkspaceStore;
