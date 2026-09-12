mod identity;
mod models;
mod registry;

pub use identity::{resolve_or_create_workspace_identity, verify_or_create_workspace_identity};
pub use models::{
    RecoveryHandle, RecoveryStatus, SnapshotRecord, WorkspaceDurableState,
    WorkspaceProjectionInput, WorkspaceRecord, WorkspaceRecoveryFlushMetadata,
    WorkspaceRecoveryFlushReason, WorkspaceRecoverySidecarExport,
    WorkspaceRecoverySidecarStoreState, WorkspaceStoreState, WorktreeAllocation,
};
pub use registry::WorkspaceStore;
