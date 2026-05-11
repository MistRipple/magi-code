use std::path::PathBuf;
use thiserror::Error;

pub type SnapshotResult<T> = Result<T, SnapshotError>;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("path escapes workspace root: {0}")]
    PathEscape(String),

    #[error("path is not a child of workspace root: {0}")]
    PathOutsideRoot(String),

    #[error("session {0} not found")]
    SessionNotFound(String),

    #[error("watcher error: {0}")]
    Watcher(String),

    #[error("workspace root invalid: {0}")]
    InvalidRoot(String),

    #[error("blob {0} missing")]
    BlobMissing(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl SnapshotError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SnapshotError::Io {
            path: path.into(),
            source,
        }
    }
}
