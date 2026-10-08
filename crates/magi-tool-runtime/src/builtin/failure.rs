//! 内置工具失败载荷的唯一出口。
//!
//! 约定见 `docs/builtin-tool-failure-contract.md`：每个失败都有按类别区分的稳定 `error_code`
//! （`{tool}_{类别}`）、面向模型的 `error`（只描述类别，不带底层错误文本）和 `instruction`
//! （下一步怎么做）。底层错误的完整内容只写日志。

use magi_core::{HostPathError, ToolFailure};
use std::{fmt, io, path::Path};

/// 参数缺失或形状不对。模型应按工具 schema 修正参数后重新调用。
pub(crate) fn invalid_input(tool: &str, error: impl Into<String>) -> String {
    ToolFailure::new(tool, "invalid_input", error)
        .instruction("按该工具的参数 schema 修正参数后再调用。")
        .into_payload()
}

/// 文件系统操作失败的类别。由 `io::Error` 归类，决定错误码、说明与下一步指引。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FsFailureKind {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    StorageFull,
    ReadOnly,
    InvalidText,
    Other,
}

impl FsFailureKind {
    pub(crate) fn classify(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            io::ErrorKind::NotADirectory => Self::NotADirectory,
            io::ErrorKind::IsADirectory => Self::IsADirectory,
            io::ErrorKind::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            io::ErrorKind::StorageFull => Self::StorageFull,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnly,
            io::ErrorKind::InvalidData => Self::InvalidText,
            _ => Self::Other,
        }
    }

    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::AlreadyExists => "already_exists",
            Self::NotADirectory => "not_a_directory",
            Self::IsADirectory => "is_a_directory",
            Self::DirectoryNotEmpty => "directory_not_empty",
            Self::StorageFull => "storage_full",
            Self::ReadOnly => "read_only_filesystem",
            Self::InvalidText => "not_utf8_text",
            Self::Other => "io_failed",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::NotFound => "路径不存在",
            Self::PermissionDenied => "没有权限访问该路径",
            Self::AlreadyExists => "目标已存在",
            Self::NotADirectory => "路径中的某一级不是目录",
            Self::IsADirectory => "目标是目录，不是文件",
            Self::DirectoryNotEmpty => "目录不是空的",
            Self::StorageFull => "磁盘空间不足",
            Self::ReadOnly => "文件系统是只读的",
            Self::InvalidText => "文件内容不是有效的 UTF-8 文本",
            Self::Other => "文件系统操作失败",
        }
    }

    fn instruction(self) -> &'static str {
        match self {
            Self::NotFound => {
                "用 file_read 读取其父目录确认实际路径后，再用正确的路径调用；不要重复同一个路径。"
            }
            Self::PermissionDenied => {
                "不要用相同参数重试；换一个有权限的位置，或告知用户需要调整权限。"
            }
            Self::AlreadyExists => "换一个目标路径；确认要覆盖时，使用该工具的覆盖参数显式允许。",
            Self::NotADirectory => "用 file_read 查看父目录，确认每一级路径的类型后再调用。",
            Self::IsADirectory => "目标是目录；改用目录对应的操作，或指向目录内的具体文件。",
            Self::DirectoryNotEmpty => "目录非空；需要连同内容一起处理时，使用该工具的递归参数。",
            Self::StorageFull => "磁盘空间不足；不要重复写入，告知用户先释放空间。",
            Self::ReadOnly => "目标所在文件系统只读；换一个可写位置，或告知用户。",
            Self::InvalidText => "该文件不是文本；不要再按文本读取或修改它。",
            Self::Other => "原因已记录到日志；不要用相同参数重复调用，换一种方式或告知用户。",
        }
    }
}

/// 文件系统操作失败。`operation` 是动作短语（如“读取文件”），完整 `io::Error` 只写日志。
pub(crate) fn filesystem_failure(
    tool: &str,
    operation: &'static str,
    path: &Path,
    error: &io::Error,
) -> ToolFailure {
    tracing::warn!(
        tool,
        operation,
        path = %path.display(),
        error = %error,
        "builtin filesystem operation failed"
    );
    let kind = FsFailureKind::classify(error);
    ToolFailure::new(
        tool,
        kind.slug(),
        format!("{operation}失败：{}", kind.description()),
    )
    .instruction(kind.instruction())
}

/// 路径无法解析为工作区内的具体位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathResolveError {
    WorkingDirectoryUnavailable,
    Invalid(HostPathError),
}

impl fmt::Display for PathResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkingDirectoryUnavailable => formatter.write_str("无法解析当前工作目录"),
            Self::Invalid(error) => write!(formatter, "{error}"),
        }
    }
}

pub(crate) fn path_resolution_failure(
    tool: &str,
    requested_path: &str,
    error: &PathResolveError,
) -> String {
    tracing::warn!(
        tool,
        requested_path,
        error = %error,
        "builtin path resolution failed"
    );
    match error {
        PathResolveError::WorkingDirectoryUnavailable => {
            ToolFailure::new(tool, "workspace_unavailable", "当前工作区目录不可用")
                .instruction("告知用户重新选择工作区；不要重试。")
        }
        PathResolveError::Invalid(error) => {
            ToolFailure::new(tool, "invalid_path", format!("路径无效：{error}"))
                .instruction("使用工作区内的相对路径，或以 / 或 ~ 开头的绝对路径。")
        }
    }
    .into_payload()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn payload(raw: String) -> Value {
        serde_json::from_str(&raw).expect("failure payload is json")
    }

    #[test]
    fn io_errors_are_classified_into_distinct_codes_without_leaking_os_text() {
        let cases = [
            (io::ErrorKind::NotFound, "file_write_not_found"),
            (
                io::ErrorKind::PermissionDenied,
                "file_write_permission_denied",
            ),
            (io::ErrorKind::AlreadyExists, "file_write_already_exists"),
            (io::ErrorKind::NotADirectory, "file_write_not_a_directory"),
            (io::ErrorKind::IsADirectory, "file_write_is_a_directory"),
            (
                io::ErrorKind::DirectoryNotEmpty,
                "file_write_directory_not_empty",
            ),
            (io::ErrorKind::StorageFull, "file_write_storage_full"),
            (
                io::ErrorKind::ReadOnlyFilesystem,
                "file_write_read_only_filesystem",
            ),
            (io::ErrorKind::InvalidData, "file_write_not_utf8_text"),
            (io::ErrorKind::TimedOut, "file_write_io_failed"),
        ];
        for (kind, expected) in cases {
            let error = io::Error::new(kind, "os error 13: secret detail");
            let raw = filesystem_failure("file_write", "写入文件", Path::new("/w/a.txt"), &error)
                .into_payload();
            let value = payload(raw.clone());

            assert_eq!(value["error_code"], expected, "{kind:?}");
            assert!(
                value["instruction"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            assert!(!raw.contains("secret detail"), "{kind:?}");
            assert!(!raw.contains("os error"), "{kind:?}");
        }
    }

    #[test]
    fn path_resolution_failures_are_distinguished_and_do_not_echo_the_requested_path() {
        let raw = path_resolution_failure(
            "file_read",
            "/private/workspace/secret.txt",
            &PathResolveError::WorkingDirectoryUnavailable,
        );
        let value = payload(raw.clone());
        assert_eq!(value["error_code"], "file_read_workspace_unavailable");
        assert!(!raw.contains("secret.txt"));

        let invalid = payload(path_resolution_failure(
            "file_read",
            "~x",
            &PathResolveError::Invalid(HostPathError::InvalidPathRef),
        ));
        assert_eq!(invalid["error_code"], "file_read_invalid_path");
    }

    #[test]
    fn invalid_input_names_the_tool_and_points_to_the_schema() {
        let value = payload(invalid_input("file_patch", "缺少 path 字段"));

        assert_eq!(value["error_code"], "file_patch_invalid_input");
        assert_eq!(value["error"], "缺少 path 字段");
        assert!(value["instruction"].as_str().is_some());
    }
}
