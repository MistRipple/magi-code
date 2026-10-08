//! `apply_patch`：按 `*** Begin Patch` 信封一次修改多个文件。
//!
//! 整份 patch 先在内存里解析并应用到暂存内容，确认每个文件都能匹配之后才开始写盘；
//! 写盘中途失败会把已经写入的文件恢复成原样，并在结果里说明恢复情况。

use crate::{
    BuiltinToolAccessMode, ToolExecutionContext,
    builtin::{
        failure::{ToolFailure, filesystem_failure, path_resolution_failure},
        field_string,
        fs_support::write_file_atomically,
        resolve_path_with_context,
    },
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

const TOOL_NAME: &str = "apply_patch";

#[derive(Clone, Debug)]
struct ApplyPatchPlan {
    operations: Vec<PatchOperation>,
}

#[derive(Clone, Debug)]
enum PatchOperation {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<TextHunk>,
    },
}

#[derive(Clone, Debug, Default)]
struct TextHunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

/// patch 的格式错误。携带可直接返回给模型的说明，不涉及文件系统。
fn invalid_patch(error: impl Into<String>) -> String {
    ToolFailure::new(TOOL_NAME, "invalid_patch", error)
        .instruction(
            "按 apply_patch 格式修正后重新提交完整的 patch：以 *** Begin Patch 开始、*** End Patch 结束，文件头为 *** Add File: / *** Update File: / *** Delete File:。",
        )
        .into_payload()
}

pub(crate) fn execute_apply_patch(input: &str, context: &ToolExecutionContext) -> String {
    let patch_text = match extract_patch_text(input) {
        Ok(text) => text,
        Err(error) => return invalid_patch(error),
    };
    let plan = match parse_apply_patch(&patch_text) {
        Ok(plan) => plan,
        Err(error) => return invalid_patch(error),
    };

    let operations = plan.operations.len();
    match apply_plan(&plan, context) {
        Ok(changed_paths) => {
            let changed_paths = changed_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            serde_json::json!({
                "tool": TOOL_NAME,
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
                "operations": operations,
                "changed_paths": changed_paths,
                "summary": format!("已应用 apply_patch，影响 {} 个路径", changed_paths.len()),
            })
            .to_string()
        }
        Err(failure) => failure,
    }
}

pub fn apply_patch_declared_paths_from_input(input: &str) -> Vec<PathBuf> {
    let Ok(patch_text) = extract_patch_text(input) else {
        return Vec::new();
    };
    let Ok(plan) = parse_apply_patch(&patch_text) else {
        return Vec::new();
    };

    let mut paths = BTreeSet::new();
    for operation in plan.operations {
        match operation {
            PatchOperation::Add { path, .. } | PatchOperation::Delete { path } => {
                paths.insert(PathBuf::from(path));
            }
            PatchOperation::Update { path, move_to, .. } => {
                paths.insert(PathBuf::from(path));
                if let Some(move_to) = move_to {
                    paths.insert(PathBuf::from(move_to));
                }
            }
        }
    }
    paths.into_iter().collect()
}

/// 输入必须是带 `patch` 字符串字段的 JSON 对象（与工具 schema 一致）。
fn extract_patch_text(input: &str) -> Result<String, String> {
    let object = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "apply_patch 输入必须是包含 patch 字段的 JSON 对象".to_string())?;
    let text = field_string(&object, "patch")
        .ok_or_else(|| "apply_patch 输入 JSON 必须包含 patch 字符串字段".to_string())?;
    if text.trim().is_empty() {
        return Err("apply_patch patch 不能为空".to_string());
    }
    Ok(text)
}

fn parse_apply_patch(patch: &str) -> Result<ApplyPatchPlan, String> {
    let normalized = patch.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.split('\n').collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err("apply_patch 必须以 *** Begin Patch 开始".to_string());
    }

    let mut index = 1usize;
    let mut operations = Vec::new();
    loop {
        let Some(line) = lines.get(index).copied() else {
            return Err("apply_patch 缺少 *** End Patch".to_string());
        };
        if line == "*** End Patch" {
            index += 1;
            break;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let (operation, next_index) = parse_add_file(path, &lines, index + 1)?;
            operations.push(operation);
            index = next_index;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOperation::Delete {
                path: normalize_patch_path(path)?,
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let (operation, next_index) = parse_update_file(path, &lines, index + 1)?;
            operations.push(operation);
            index = next_index;
            continue;
        }
        return Err(format!("apply_patch 第 {} 行不是合法文件操作头", index + 1));
    }

    if operations.is_empty() {
        return Err("apply_patch 至少需要一个文件操作".to_string());
    }
    if lines[index..].iter().any(|line| !line.is_empty()) {
        return Err("apply_patch 在 *** End Patch 之后包含额外内容".to_string());
    }

    Ok(ApplyPatchPlan { operations })
}

fn parse_add_file(
    path: &str,
    lines: &[&str],
    mut index: usize,
) -> Result<(PatchOperation, usize), String> {
    let path = normalize_patch_path(path)?;
    let mut content_lines = Vec::new();
    while let Some(line) = lines.get(index).copied() {
        if is_patch_boundary(line) {
            break;
        }
        let Some(content) = line.strip_prefix('+') else {
            return Err(format!(
                "Add File {} 的第 {} 行必须以 + 开头",
                path,
                index + 1
            ));
        };
        content_lines.push(content.to_string());
        index += 1;
    }
    if content_lines.is_empty() {
        return Err(format!("Add File {path} 必须包含至少一行 + 内容"));
    }

    Ok((
        PatchOperation::Add {
            path,
            content: join_patch_lines(&content_lines),
        },
        index,
    ))
}

fn parse_update_file(
    path: &str,
    lines: &[&str],
    mut index: usize,
) -> Result<(PatchOperation, usize), String> {
    let path = normalize_patch_path(path)?;
    let mut move_to = None;
    if let Some(line) = lines.get(index).copied()
        && let Some(target) = line.strip_prefix("*** Move to: ")
    {
        move_to = Some(normalize_patch_path(target)?);
        index += 1;
    }

    let mut hunks = Vec::new();
    let mut current = TextHunk::default();
    while let Some(line) = lines.get(index).copied() {
        if is_patch_boundary(line) {
            break;
        }
        if line.starts_with("@@") {
            push_hunk_if_present(&mut hunks, &mut current);
            index += 1;
            continue;
        }
        if line == "*** End of File" {
            index += 1;
            continue;
        }
        let Some(prefix) = line.chars().next() else {
            return Err(format!("Update File {path} 的第 {} 行为空", index + 1));
        };
        // 前缀都是单字节 ASCII；首字符是别的（包括多字节字符）时落到下面的错误分支，不能按字节切片。
        let body = line.get(1..).unwrap_or_default().to_string();
        match prefix {
            ' ' => {
                current.old_lines.push(body.clone());
                current.new_lines.push(body);
            }
            '-' => current.old_lines.push(body),
            '+' => current.new_lines.push(body),
            _ => {
                return Err(format!(
                    "Update File {} 的第 {} 行必须以空格、+、-、@@ 或 *** End of File 开头",
                    path,
                    index + 1
                ));
            }
        }
        index += 1;
    }
    push_hunk_if_present(&mut hunks, &mut current);

    if move_to.is_none() && hunks.is_empty() {
        return Err(format!("Update File {path} 缺少变更行"));
    }

    Ok((
        PatchOperation::Update {
            path,
            move_to,
            hunks,
        },
        index,
    ))
}

fn push_hunk_if_present(hunks: &mut Vec<TextHunk>, current: &mut TextHunk) {
    if !current.old_lines.is_empty() || !current.new_lines.is_empty() {
        hunks.push(std::mem::take(current));
    }
}

fn normalize_patch_path(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("apply_patch 文件路径不能为空".to_string());
    }
    Ok(path.to_string())
}

fn is_patch_boundary(line: &str) -> bool {
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

fn apply_plan(
    plan: &ApplyPatchPlan,
    context: &ToolExecutionContext,
) -> Result<BTreeSet<PathBuf>, String> {
    // `None` 表示该路径在 patch 应用后不存在（已删除或被移走）。
    let mut staged: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
    let mut changed_paths = BTreeSet::new();

    for operation in &plan.operations {
        match operation {
            PatchOperation::Add { path, content } => {
                let path = resolve_patch_path(path, context)?;
                let occupied = match staged.get(&path) {
                    Some(staged_content) => staged_content.is_some(),
                    None => fs::symlink_metadata(&path).is_ok(),
                };
                if occupied {
                    return Err(ToolFailure::new(TOOL_NAME, "file_exists", "Add File 的目标已存在")
                        .instruction(
                            "要修改已有文件，改用 Update File；要整体替换，先 Delete File 再 Add File。",
                        )
                        .with("path", path.display().to_string())
                        .into_payload());
                }
                staged.insert(path.clone(), Some(content.clone()));
                changed_paths.insert(path);
            }
            PatchOperation::Delete { path } => {
                let path = resolve_patch_path(path, context)?;
                validate_file_can_be_deleted(&path, &staged)?;
                staged.insert(path.clone(), None);
                changed_paths.insert(path);
            }
            PatchOperation::Update {
                path,
                move_to,
                hunks,
            } => {
                let path = resolve_patch_path(path, context)?;
                let mut updated = read_staged_or_disk(&path, &staged)?;
                for (index, hunk) in hunks.iter().enumerate() {
                    updated = apply_text_hunk(&updated, hunk)
                        .map_err(|error| hunk_failure(&path, index, error))?;
                }

                if let Some(move_to) = move_to {
                    let target = resolve_patch_path(move_to, context)?;
                    staged.insert(target.clone(), Some(updated));
                    if target != path {
                        staged.insert(path.clone(), None);
                    }
                    changed_paths.insert(path);
                    changed_paths.insert(target);
                } else {
                    staged.insert(path.clone(), Some(updated));
                    changed_paths.insert(path);
                }
            }
        }
    }

    commit_staged_changes(staged)?;
    Ok(changed_paths)
}

fn resolve_patch_path(path: &str, context: &ToolExecutionContext) -> Result<PathBuf, String> {
    resolve_path_with_context(path, context)
        .map_err(|error| path_resolution_failure(TOOL_NAME, path, &error))
}

fn read_staged_or_disk(
    path: &PathBuf,
    staged: &BTreeMap<PathBuf, Option<String>>,
) -> Result<String, String> {
    if let Some(content) = staged.get(path) {
        return content.clone().ok_or_else(|| {
            ToolFailure::new(
                TOOL_NAME,
                "conflicting_operations",
                "文件已在本 patch 中删除，不能继续更新",
            )
            .instruction(
                "同一份 patch 里不要先删除再更新同一个文件；调整操作顺序或拆成两次 patch。",
            )
            .with("path", path.display().to_string())
            .into_payload()
        });
    }
    fs::read_to_string(path).map_err(|error| {
        filesystem_failure(TOOL_NAME, "读取待修改文件", path, &error).into_payload()
    })
}

fn validate_file_can_be_deleted(
    path: &PathBuf,
    staged: &BTreeMap<PathBuf, Option<String>>,
) -> Result<(), String> {
    if staged.get(path).and_then(Option::as_ref).is_some() {
        return Ok(());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(ToolFailure::new(
            TOOL_NAME,
            "not_a_file",
            "Delete File 只能删除文件，不能删除目录",
        )
        .instruction("删除目录请使用 file_remove。")
        .with("path", path.display().to_string())
        .into_payload()),
        Ok(_) => Ok(()),
        Err(error) => {
            Err(filesystem_failure(TOOL_NAME, "读取待删除文件信息", path, &error).into_payload())
        }
    }
}

/// hunk 与文件内容对不上。先带上具体是哪个文件的第几个 hunk，再说明重新读取后重做。
fn hunk_failure(path: &Path, index: usize, error: HunkError) -> String {
    let (kind, message) = match error {
        HunkError::NotFound => ("context_not_found", "未找到匹配上下文".to_string()),
        HunkError::Ambiguous(count) => (
            "context_ambiguous",
            format!("上下文匹配了 {count} 处，需要更多上下文"),
        ),
    };
    let instruction = match kind {
        "context_ambiguous" => {
            "给这个 hunk 增加更多前后文行，使它在文件中唯一，然后重新提交整份 patch。"
        }
        _ => {
            "先用 file_read 重新读取该文件（内容可能已变化），再以文件里的原文作为上下文行和删除行重新生成 patch。"
        }
    };
    ToolFailure::new(
        TOOL_NAME,
        kind,
        format!(
            "Update File {} hunk[{index}] 失败：{message}",
            path.display()
        ),
    )
    .instruction(instruction)
    .with("path", path.display().to_string())
    .with("hunk_index", index)
    .into_payload()
}

enum HunkError {
    NotFound,
    Ambiguous(usize),
}

fn apply_text_hunk(content: &str, hunk: &TextHunk) -> Result<String, HunkError> {
    if hunk.old_lines.is_empty() {
        let mut output = content.to_string();
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&join_patch_lines(&hunk.new_lines));
        return Ok(output);
    }

    let old_with_newline = join_patch_lines(&hunk.old_lines);
    let old_without_newline = hunk.old_lines.join("\n");
    let candidates = if old_with_newline == old_without_newline {
        vec![old_with_newline]
    } else {
        vec![old_with_newline, old_without_newline]
    };

    let mut ambiguous_count = 0usize;
    for old_text in candidates {
        let count = content.matches(old_text.as_str()).count();
        if count == 1 {
            let new_text = if old_text.ends_with('\n') {
                join_patch_lines(&hunk.new_lines)
            } else {
                hunk.new_lines.join("\n")
            };
            return Ok(content.replacen(old_text.as_str(), &new_text, 1));
        }
        ambiguous_count = ambiguous_count.max(count);
    }

    if ambiguous_count > 1 {
        Err(HunkError::Ambiguous(ambiguous_count))
    } else {
        Err(HunkError::NotFound)
    }
}

fn join_patch_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// 一个已经执行过的写盘动作及其原始内容（`None` 表示原本不存在），用于失败回滚。
struct AppliedChange {
    path: PathBuf,
    original: Option<Vec<u8>>,
}

fn restore_original(change: &AppliedChange) -> io::Result<()> {
    match &change.original {
        Some(bytes) => write_file_atomically(&change.path, bytes),
        None => match fs::remove_file(&change.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

/// 写盘。每个动作前先记下原始内容；任何一步失败，已执行的动作按相反顺序恢复，
/// 返回的失败里说明失败的路径与恢复情况，模型不必猜哪些文件已经被改。
fn commit_staged_changes(staged: BTreeMap<PathBuf, Option<String>>) -> Result<(), String> {
    // 先写新内容，再删除，保证“移动”在任何时刻都不会同时丢掉两份。
    let writes = staged
        .iter()
        .filter_map(|(path, content)| content.as_ref().map(|content| (path, Some(content))));
    let deletes = staged
        .iter()
        .filter(|(_, content)| content.is_none())
        .map(|(path, _)| (path, None));

    let mut applied: Vec<AppliedChange> = Vec::new();
    for (path, content) in writes.chain(deletes) {
        let original = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(rollback_and_fail(
                    &applied,
                    "读取写入前的原始内容",
                    path,
                    &error,
                ));
            }
        };
        let result = match content {
            Some(content) => path
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| write_file_atomically(path, content.as_bytes())),
            None => match fs::remove_file(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        match result {
            Ok(()) => applied.push(AppliedChange {
                path: path.clone(),
                original,
            }),
            Err(error) => {
                return Err(rollback_and_fail(
                    &applied,
                    if content.is_some() {
                        "写入文件"
                    } else {
                        "删除文件"
                    },
                    path,
                    &error,
                ));
            }
        }
    }
    Ok(())
}

fn rollback_and_fail(
    applied: &[AppliedChange],
    operation: &'static str,
    failed_path: &Path,
    error: &io::Error,
) -> String {
    let mut unrestored = Vec::new();
    for change in applied.iter().rev() {
        if let Err(restore_error) = restore_original(change) {
            tracing::error!(
                path = %change.path.display(),
                error = %restore_error,
                "apply_patch rollback failed"
            );
            unrestored.push(change.path.display().to_string());
        }
    }
    let rolled_back = unrestored.is_empty();
    let instruction = if rolled_back {
        "已把本次 patch 已写入的文件恢复原样，没有任何文件被改动；排除失败原因后可重新提交整份 patch。"
    } else {
        "回滚没有完全成功：unrestored_paths 列出的文件仍是 patch 写入后的内容，请逐个用 file_read 检查并手动修复。"
    };
    filesystem_failure(TOOL_NAME, operation, failed_path, error)
        .instruction(instruction)
        .with("rolled_back", rolled_back)
        .with("unrestored_paths", unrestored)
        .into_payload()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}-{}", name, std::process::id(), suffix));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn run_patch(patch: &str, root: &std::path::Path) -> String {
        execute_apply_patch(
            &serde_json::json!({ "patch": patch }).to_string(),
            &context(root),
        )
    }

    fn context(root: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            working_directory: Some(root.to_path_buf()),
            ..ToolExecutionContext::default()
        }
    }

    #[test]
    fn apply_patch_add_update_delete_and_move() {
        let dir = unique_temp_dir("magi-apply-patch-add-update-delete-move");
        fs::write(dir.join("old.txt"), "alpha\nbeta\n").expect("old file");
        fs::write(dir.join("remove.txt"), "gone\n").expect("remove file");

        let patch = r#"*** Begin Patch
*** Add File: created.txt
+hello
*** Update File: old.txt
*** Move to: nested/new.txt
@@
-alpha
+ALPHA
 beta
*** Delete File: remove.txt
*** End Patch
"#;
        let output = run_patch(patch, &dir);
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(
            fs::read_to_string(dir.join("created.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("nested/new.txt")).unwrap(),
            "ALPHA\nbeta\n"
        );
        assert!(!dir.join("old.txt").exists());
        assert!(!dir.join("remove.txt").exists());
    }

    #[test]
    fn apply_patch_reads_the_patch_field() {
        let dir = unique_temp_dir("magi-apply-patch-json-payload");
        let output = run_patch(
            "*** Begin Patch\n*** Add File: json.txt\n+from json\n*** End Patch\n",
            &dir,
        );
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(
            fs::read_to_string(dir.join("json.txt")).unwrap(),
            "from json\n"
        );
    }

    #[test]
    fn apply_patch_rejects_ambiguous_update_context() {
        let dir = unique_temp_dir("magi-apply-patch-ambiguous");
        fs::write(dir.join("dup.txt"), "same\nsame\n").expect("dup file");
        let patch = r#"*** Begin Patch
*** Update File: dup.txt
@@
-same
+changed
*** End Patch
"#;

        let output = run_patch(patch, &dir);
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(
            fs::read_to_string(dir.join("dup.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn missing_update_target_reports_not_found_without_internal_details() {
        let dir = unique_temp_dir("magi-apply-patch-missing-target");
        let patch =
            "*** Begin Patch\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch\n";

        let output = run_patch(patch, &dir);
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "apply_patch_not_found");
        assert!(payload["instruction"].as_str().is_some());
        assert!(!output.contains("No such file"));
        assert!(!output.contains("os error"));
    }

    #[test]
    fn hunk_mismatch_names_the_file_and_hunk_and_tells_the_model_to_reread() {
        let dir = unique_temp_dir("magi-apply-patch-mismatch");
        fs::write(dir.join("a.txt"), "one\ntwo\n").expect("seed");
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-three\n+3\n*** End Patch\n";

        let payload: Value = serde_json::from_str(&run_patch(patch, &dir)).expect("json");

        assert_eq!(payload["error_code"], "apply_patch_context_not_found");
        assert_eq!(payload["hunk_index"], 0);
        assert!(
            payload["instruction"]
                .as_str()
                .is_some_and(|text| text.contains("file_read"))
        );
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn add_file_refuses_to_overwrite_an_existing_file() {
        let dir = unique_temp_dir("magi-apply-patch-add-existing");
        fs::write(dir.join("keep.txt"), "precious\n").expect("seed");
        let patch = "*** Begin Patch\n*** Add File: keep.txt\n+replacement\n*** End Patch\n";

        let payload: Value = serde_json::from_str(&run_patch(patch, &dir)).expect("json");

        assert_eq!(payload["error_code"], "apply_patch_file_exists");
        assert_eq!(
            fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "precious\n"
        );
    }

    #[test]
    fn delete_then_add_replaces_a_file_within_one_patch() {
        let dir = unique_temp_dir("magi-apply-patch-replace");
        fs::write(dir.join("f.txt"), "old\n").expect("seed");
        let patch =
            "*** Begin Patch\n*** Delete File: f.txt\n*** Add File: f.txt\n+new\n*** End Patch\n";

        let payload: Value = serde_json::from_str(&run_patch(patch, &dir)).expect("json");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "new\n");
    }

    #[test]
    fn updating_a_file_deleted_earlier_in_the_patch_is_a_conflict() {
        let dir = unique_temp_dir("magi-apply-patch-conflict");
        fs::write(dir.join("f.txt"), "x\n").expect("seed");
        let patch = "*** Begin Patch\n*** Delete File: f.txt\n*** Update File: f.txt\n@@\n-x\n+y\n*** End Patch\n";

        let payload: Value = serde_json::from_str(&run_patch(patch, &dir)).expect("json");

        assert_eq!(payload["error_code"], "apply_patch_conflicting_operations");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "x\n");
    }

    #[test]
    fn non_ascii_line_without_a_prefix_is_a_patch_error_not_a_panic() {
        let dir = unique_temp_dir("magi-apply-patch-non-ascii");
        fs::write(dir.join("a.txt"), "x\n").expect("seed");
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n中文行没有前缀\n*** End Patch\n";

        let payload: Value = serde_json::from_str(&run_patch(patch, &dir)).expect("json");

        assert_eq!(payload["error_code"], "apply_patch_invalid_patch");
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_halfway_through_restores_files_that_were_already_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_temp_dir("magi-apply-patch-rollback");
        fs::write(dir.join("a.txt"), "A-old\n").expect("a");
        fs::create_dir_all(dir.join("locked")).expect("locked dir");
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o500)).expect("chmod");
        // a.txt 先写成功；locked 目录里的新文件写不进去。路径按字典序 a.txt < locked/new.txt。
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-A-old\n+A-new\n*** Add File: locked/new.txt\n+x\n*** End Patch\n";

        let output = run_patch(patch, &dir);
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o700)).expect("unlock");
        let payload: Value = serde_json::from_str(&output).expect("json");

        if payload["status"] == "succeeded" {
            // 以 root 运行时目录权限不生效，没有可测的失败。
            return;
        }
        assert_eq!(payload["rolled_back"], true);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "A-old\n");
        assert!(!dir.join("locked/new.txt").exists());
    }

    #[test]
    fn apply_patch_declared_paths_reads_patch_envelope() {
        let input = serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: a.txt\n+x\n*** Update File: b.txt\n*** Move to: c.txt\n@@\n-old\n+new\n*** End Patch\n"
        })
        .to_string();

        assert_eq!(
            apply_patch_declared_paths_from_input(&input),
            vec![
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("c.txt")
            ]
        );
    }
}
