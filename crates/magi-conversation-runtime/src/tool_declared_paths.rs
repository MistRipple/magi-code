use magi_bridge_client::ChatToolCall;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// 从 tool_call 参数推断可能被改写的路径，供 SnapshotSession 的 after_tool 强制拍后态。
/// 覆盖 canonical 文件工具（file_write / file_patch / apply_patch / file_remove / file_mkdir）
/// 和 shell 工具（changed_paths）。
/// 无法可靠推断时返回空 Vec，由 ChangeLog 的全树对账补齐。
pub(crate) fn derive_declared_paths(tool_call: &ChatToolCall) -> Vec<PathBuf> {
    let tool_name = tool_call.function.name.as_str();
    if tool_name == "apply_patch" {
        return magi_tool_runtime::apply_patch_declared_paths_from_input(
            &tool_call.function.arguments,
        );
    }

    let Ok(arguments) = serde_json::from_str::<Value>(&tool_call.function.arguments) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    match tool_name {
        "file_write" | "file_patch" | "file_remove" | "file_mkdir" => {
            if let Some(path) = arguments.get("path").and_then(Value::as_str) {
                paths.push(PathBuf::from(path));
            }
        }
        "shell_exec" => {
            if let Some(list) = arguments.get("changed_paths").and_then(Value::as_array) {
                for item in list {
                    if let Some(path) = item.as_str() {
                        paths.push(PathBuf::from(path));
                    }
                }
            }
        }
        _ => {}
    }
    paths
}

/// 工具执行完成后，部分工具才会知道真实改写范围。
///
/// 典型场景是 `shell_exec`：运行前只能知道命令，运行后 runtime 会把实际文件差异
/// 写入 `changed_paths`。Snapshot hook 必须使用这些结果路径，否则 watcher 漏事件时
/// 只能把变更归成 external。
pub(crate) fn append_result_declared_paths(paths: &mut Vec<PathBuf>, tool_result: &str) {
    let Ok(value) = serde_json::from_str::<Value>(tool_result) else {
        return;
    };

    for key in ["path", "file_path", "filePath"] {
        if let Some(path) = value.get(key).and_then(Value::as_str) {
            paths.push(PathBuf::from(path));
        }
    }
    for key in ["changed_paths", "changedPaths"] {
        if let Some(list) = value.get(key).and_then(Value::as_array) {
            for item in list {
                if let Some(path) = item.as_str() {
                    paths.push(PathBuf::from(path));
                }
            }
        }
    }

    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.to_string_lossy().replace('\\', "/")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;

    fn tool_call(name: &str, arguments: impl Into<String>) -> ChatToolCall {
        ChatToolCall {
            id: "tool-call-test".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: arguments.into(),
            },
        }
    }

    #[test]
    fn append_result_declared_paths_reads_changed_paths() {
        let mut paths = vec![PathBuf::from("before.txt")];
        append_result_declared_paths(
            &mut paths,
            r#"{"status":"succeeded","changed_paths":["after.txt","nested/out.txt"]}"#,
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("before.txt"),
                PathBuf::from("after.txt"),
                PathBuf::from("nested/out.txt"),
            ]
        );
    }

    #[test]
    fn derive_declared_paths_reads_apply_patch_json_payload() {
        let call = tool_call(
            "apply_patch",
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+x\n*** Update File: b.txt\n*** Move to: c.txt\n@@\n-old\n+new\n*** End Patch\n"
            })
            .to_string(),
        );

        assert_eq!(
            derive_declared_paths(&call),
            vec![
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("c.txt")
            ]
        );
    }

    #[test]
    fn derive_declared_paths_reads_apply_patch_raw_payload() {
        let call = tool_call(
            "apply_patch",
            "*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch\n",
        );

        assert_eq!(
            derive_declared_paths(&call),
            vec![PathBuf::from("gone.txt")]
        );
    }
}
