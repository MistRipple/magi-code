use std::{path::PathBuf, process::Command};

#[cfg(target_os = "windows")]
use std::ffi::OsString;

use magi_core::HostPath;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RevealWorkspaceFileRequest {
    target_path_ref: String,
    workspace_root_path_ref: String,
}

fn resolve_reveal_paths(
    request: &RevealWorkspaceFileRequest,
) -> Result<(PathBuf, PathBuf), String> {
    let workspace_root = HostPath::from_path_ref(&request.workspace_root_path_ref)
        .map_err(|_| "工作区路径引用无效".to_string())?;
    let target_path = HostPath::from_path_ref(&request.target_path_ref)
        .map_err(|_| "文件路径引用无效".to_string())?;
    let canonical_workspace_root = HostPath::canonicalize(workspace_root.as_path())
        .map(HostPath::into_path_buf)
        .map_err(|error| format!("工作区目录不可读取或不存在: {error}"))?;
    if !canonical_workspace_root.is_dir() {
        return Err("工作区路径不是目录".to_string());
    }
    let canonical_target = HostPath::canonicalize(target_path.as_path())
        .map(HostPath::into_path_buf)
        .map_err(|error| format!("文件不可读取或不存在: {error}"))?;
    if !canonical_target.is_file() {
        return Err("目标路径不是文件".to_string());
    }
    if !canonical_target.starts_with(&canonical_workspace_root) {
        return Err("文件路径越出工作区边界".to_string());
    }
    Ok((canonical_workspace_root, canonical_target))
}

fn reveal_file_command(target_path: &std::path::Path) -> Result<Command, String> {
    #[cfg(target_os = "macos")]
    {
        let mut command = magi_process::std_command("open");
        command.arg("-R").arg(target_path);
        Ok(command)
    }

    #[cfg(target_os = "windows")]
    {
        let mut select_argument = OsString::from("/select,");
        select_argument.push(target_path.as_os_str());
        let mut command = magi_process::std_command("explorer.exe");
        command.arg(select_argument);
        Ok(command)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = target_path
            .parent()
            .ok_or_else(|| "文件缺少可打开的父目录".to_string())?;
        let mut command = magi_process::std_command("xdg-open");
        command.arg(parent);
        Ok(command)
    }
}

#[tauri::command]
pub(crate) fn reveal_workspace_file(request: RevealWorkspaceFileRequest) -> Result<(), String> {
    let (_, target_path) = resolve_reveal_paths(&request)?;
    reveal_file_command(&target_path)?
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("打开文件所在目录失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "magi-desktop-{label}-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).expect("test directory should create");
        directory
    }

    fn path_ref(path: PathBuf) -> String {
        HostPath::from_path(path).to_path_ref().as_str().to_string()
    }

    #[test]
    fn reveal_paths_accept_existing_workspace_file() {
        let workspace = test_directory("reveal-valid");
        let file = workspace.join("src/main.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();
        let request = RevealWorkspaceFileRequest {
            target_path_ref: path_ref(file.clone()),
            workspace_root_path_ref: path_ref(workspace.clone()),
        };

        let (resolved_workspace, resolved_file) = resolve_reveal_paths(&request).unwrap();
        assert_eq!(resolved_workspace, workspace.canonicalize().unwrap());
        assert_eq!(resolved_file, file.canonicalize().unwrap());
    }

    #[test]
    fn reveal_paths_reject_directory_and_outside_file() {
        let workspace = test_directory("reveal-boundary");
        let outside = test_directory("reveal-outside").join("outside.txt");
        fs::write(&outside, "outside\n").unwrap();

        let directory_request = RevealWorkspaceFileRequest {
            target_path_ref: path_ref(workspace.clone()),
            workspace_root_path_ref: path_ref(workspace.clone()),
        };
        assert_eq!(
            resolve_reveal_paths(&directory_request).unwrap_err(),
            "目标路径不是文件"
        );

        let outside_request = RevealWorkspaceFileRequest {
            target_path_ref: path_ref(outside),
            workspace_root_path_ref: path_ref(workspace),
        };
        assert_eq!(
            resolve_reveal_paths(&outside_request).unwrap_err(),
            "文件路径越出工作区边界"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reveal_uses_finder_selection() {
        let command = reveal_file_command(std::path::Path::new("/tmp/example.txt")).unwrap();
        assert_eq!(command.get_program(), "open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("-R"),
                std::ffi::OsStr::new("/tmp/example.txt")
            ]
        );
    }
}
