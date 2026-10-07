use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("{action}失败 ({path}): {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("隔离副本目录已存在: {0}")]
    DestinationExists(PathBuf),
    #[error("源目录不存在或不是目录: {0}")]
    InvalidSource(PathBuf),
    #[error("文件路径不安全: {0}")]
    UnsafePath(String),
    #[error("会话快照账本错误: {0}")]
    Snapshot(#[from] magi_snapshot::SnapshotError),
}

impl IsolationError {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

pub type IsolationResult<T> = Result<T, IsolationError>;
