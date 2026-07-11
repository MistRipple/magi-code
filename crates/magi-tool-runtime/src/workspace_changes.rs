use crate::{BuiltinToolName, ToolExecutionContext, ToolExecutionInput};
use magi_governance::ToolKind;
use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceChangeSnapshot {
    root: PathBuf,
    files: BTreeMap<String, WorkspaceFileFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceFileFingerprint {
    status_code: String,
    content_hash: Option<u64>,
    exists: bool,
    is_dir: bool,
}

const FILESYSTEM_SNAPSHOT_MAX_FILES: usize = 5000;
const FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) fn capture_tool_workspace_snapshot(
    input: &ToolExecutionInput,
    context: &ToolExecutionContext,
) -> Option<WorkspaceChangeSnapshot> {
    if input.tool_kind != ToolKind::Builtin {
        return None;
    }
    let tool_name = BuiltinToolName::from_name(input.tool_name.trim())?;
    if !tool_name.captures_workspace_changes() {
        return None;
    }
    capture_workspace_change_snapshot(context.working_directory.as_deref()?)
}

fn capture_workspace_change_snapshot(workdir: &Path) -> Option<WorkspaceChangeSnapshot> {
    if let Some(repo_root) = run_git_capture(workdir, &["rev-parse", "--show-toplevel"])
        .map(|root| PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
    {
        let status = run_git_capture(
            &repo_root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let mut files = BTreeMap::new();
        for line in status.lines() {
            let Some((status_code, file_path)) = parse_git_status_path(line) else {
                continue;
            };
            files.insert(
                file_path.clone(),
                fingerprint_workspace_file(&repo_root, &file_path, &status_code),
            );
        }
        return Some(WorkspaceChangeSnapshot {
            root: repo_root,
            files,
        });
    }

    capture_filesystem_change_snapshot(workdir)
}

fn capture_filesystem_change_snapshot(root: &Path) -> Option<WorkspaceChangeSnapshot> {
    let root = root
        .canonicalize()
        .ok()
        .or_else(|| Some(root.to_path_buf()))?;
    let mut files = BTreeMap::new();
    collect_filesystem_fingerprints(&root, &root, &mut files);
    Some(WorkspaceChangeSnapshot { root, files })
}

pub(crate) fn append_workspace_changed_paths(
    payload: String,
    before: Option<&WorkspaceChangeSnapshot>,
    context: &ToolExecutionContext,
) -> String {
    let Some(before) = before else {
        return payload;
    };
    let Some(after) = capture_workspace_change_snapshot(
        context
            .working_directory
            .as_deref()
            .unwrap_or(before.root.as_path()),
    ) else {
        return payload;
    };
    if after.root != before.root {
        return payload;
    }
    let changed_paths = workspace_changed_paths(before, &after);
    if changed_paths.is_empty() {
        return payload;
    }
    append_changed_paths_to_json_payload(payload, &changed_paths)
}

fn workspace_changed_paths(
    before: &WorkspaceChangeSnapshot,
    after: &WorkspaceChangeSnapshot,
) -> Vec<String> {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(path) != after.files.get(path))
        .collect()
}

fn append_changed_paths_to_json_payload(payload: String, changed_paths: &[String]) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return payload;
    };
    let Some(object) = value.as_object_mut() else {
        return payload;
    };

    let mut merged = object
        .get("changed_paths")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    merged.extend(changed_paths.iter().cloned());
    object.insert(
        "changed_paths".to_string(),
        serde_json::Value::Array(merged.into_iter().map(serde_json::Value::String).collect()),
    );
    value.to_string()
}

fn run_git_capture(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_git_status_path(line: &str) -> Option<(String, String)> {
    if line.len() < 4 {
        return None;
    }
    let status_code = line.get(..2)?.to_string();
    let path_segment = line.get(3..)?.trim();
    if path_segment.is_empty() {
        return None;
    }
    let file_path = path_segment
        .rsplit(" -> ")
        .next()
        .map(str::trim)
        .filter(|path| !path.is_empty())?
        .to_string();
    Some((status_code, file_path))
}

fn fingerprint_workspace_file(
    repo_root: &Path,
    file_path: &str,
    status_code: &str,
) -> WorkspaceFileFingerprint {
    let absolute_path = repo_root.join(file_path);
    match fs::metadata(&absolute_path) {
        Ok(metadata) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: if metadata.is_file() {
                hash_file_contents(&absolute_path)
            } else {
                None
            },
            exists: true,
            is_dir: metadata.is_dir(),
        },
        Err(_) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: None,
            exists: false,
            is_dir: false,
        },
    }
}

fn collect_filesystem_fingerprints(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, WorkspaceFileFingerprint>,
) {
    if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if filesystem_snapshot_should_skip_dir(&path) {
                continue;
            }
            collect_filesystem_fingerprints(root, &path, files);
            continue;
        }
        if !metadata.is_file() || metadata.len() > FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES {
            continue;
        }
        let Some(relative_path) = path
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        files.insert(
            relative_path,
            WorkspaceFileFingerprint {
                status_code: "FS".to_string(),
                content_hash: hash_file_contents(&path),
                exists: true,
                is_dir: false,
            },
        );
    }
}

fn filesystem_snapshot_should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "coverage" | ".next" | ".svelte-kit"
    )
}

fn hash_file_contents(path: &Path) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}
