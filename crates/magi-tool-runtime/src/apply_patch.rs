use crate::{
    BuiltinToolAccessMode, ToolExecutionContext,
    builtin::{field_string, resolve_path_with_context},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

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

pub(crate) fn execute_apply_patch(input: &str, context: &ToolExecutionContext) -> String {
    let patch_text = match extract_patch_text(input) {
        Ok(text) => text,
        Err(error) => return apply_patch_error(error),
    };
    let plan = match parse_apply_patch(&patch_text) {
        Ok(plan) => plan,
        Err(error) => return apply_patch_error(error),
    };

    let operations = plan.operations.len();
    match apply_plan(&plan, context) {
        Ok(changed_paths) => {
            let changed_paths = changed_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            serde_json::json!({
                "tool": "apply_patch",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
                "operations": operations,
                "changed_paths": changed_paths,
                "summary": format!("已应用 apply_patch，影响 {} 个路径", changed_paths.len()),
            })
            .to_string()
        }
        Err(error) => apply_patch_error(error),
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

fn extract_patch_text(input: &str) -> Result<String, String> {
    let text = match serde_json::from_str::<Value>(input) {
        Ok(Value::String(text)) => text,
        Ok(Value::Object(object)) => field_string(&object, &["patch", "input", "text"])
            .ok_or_else(|| {
                "apply_patch 输入 JSON 必须包含 patch 字段；freeform 调用可直接传入 patch 文本"
                    .to_string()
            })?,
        Ok(_) => {
            return Err(
                "apply_patch 输入必须是 patch 字符串，或包含 patch 字段的 JSON 对象".to_string(),
            );
        }
        Err(_) => input.to_string(),
    };

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
        let body = line[1..].to_string();
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
    let mut staged: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
    let mut changed_paths = BTreeSet::new();

    for operation in &plan.operations {
        match operation {
            PatchOperation::Add { path, content } => {
                let path = resolve_patch_path(path, context)?;
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
                let original = read_staged_or_disk(&path, &staged)?;
                let mut updated = original;
                for (index, hunk) in hunks.iter().enumerate() {
                    updated = apply_text_hunk(&updated, hunk).map_err(|error| {
                        format!(
                            "Update File {} hunk[{}] 失败: {error}",
                            path.display(),
                            index
                        )
                    })?;
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
    resolve_path_with_context(path, context).map_err(|error| format!("{path}: {error}"))
}

fn read_staged_or_disk(
    path: &PathBuf,
    staged: &BTreeMap<PathBuf, Option<String>>,
) -> Result<String, String> {
    if let Some(content) = staged.get(path) {
        return content
            .clone()
            .ok_or_else(|| format!("文件已在本 patch 中删除，不能继续更新: {}", path.display()));
    }
    fs::read_to_string(path).map_err(|error| format!("读取文件失败 {}: {error}", path.display()))
}

fn validate_file_can_be_deleted(
    path: &PathBuf,
    staged: &BTreeMap<PathBuf, Option<String>>,
) -> Result<(), String> {
    if staged.get(path).and_then(Option::as_ref).is_some() {
        return Ok(());
    }
    if !path.exists() {
        return Err(format!("删除失败，文件不存在: {}", path.display()));
    }
    if path.is_dir() {
        return Err(format!(
            "Delete File 只能删除文件，不能删除目录: {}",
            path.display()
        ));
    }
    Ok(())
}

fn apply_text_hunk(content: &str, hunk: &TextHunk) -> Result<String, String> {
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
        Err(format!("上下文匹配了 {ambiguous_count} 处，需要更多上下文"))
    } else {
        Err("未找到匹配上下文".to_string())
    }
}

fn join_patch_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn commit_staged_changes(staged: BTreeMap<PathBuf, Option<String>>) -> Result<(), String> {
    for (path, content) in staged
        .iter()
        .filter_map(|(path, content)| content.as_ref().map(|content| (path, content)))
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建父目录失败 {}: {error}", parent.display()))?;
        }
        fs::write(path, content)
            .map_err(|error| format!("写入文件失败 {}: {error}", path.display()))?;
    }

    for path in staged
        .iter()
        .filter_map(|(path, content)| content.is_none().then_some(path))
    {
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| format!("删除文件失败 {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn apply_patch_error(message: impl Into<String>) -> String {
    serde_json::json!({
        "tool": "apply_patch",
        "status": "failed",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "error": message.into(),
    })
    .to_string()
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
        let output = execute_apply_patch(patch, &context(&dir));
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
    fn apply_patch_accepts_json_patch_payload() {
        let dir = unique_temp_dir("magi-apply-patch-json-payload");
        let input = serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: json.txt\n+from json\n*** End Patch\n"
        })
        .to_string();

        let output = execute_apply_patch(&input, &context(&dir));
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

        let output = execute_apply_patch(patch, &context(&dir));
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(
            fs::read_to_string(dir.join("dup.txt")).unwrap(),
            "same\nsame\n"
        );
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
