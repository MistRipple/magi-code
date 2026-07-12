use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionContextReferenceKind {
    File,
    Directory,
}

impl SessionContextReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextReference {
    pub kind: SessionContextReferenceKind,
    pub path: PathBuf,
    pub name: String,
}

pub fn session_context_references_metadata(
    references: &[SessionContextReference],
) -> HashMap<String, Value> {
    if references.is_empty() {
        return HashMap::new();
    }
    HashMap::from([(
        "contextReferences".to_string(),
        serde_json::to_value(references).unwrap_or(Value::Array(Vec::new())),
    )])
}

pub fn session_context_references_prompt(references: &[SessionContextReference]) -> Option<String> {
    if references.is_empty() {
        return None;
    }
    let mut lines = vec![
        "本轮用户显式添加了以下只读上下文引用。仅在任务需要时按路径读取，不要假设目录内容已经进入上下文："
            .to_string(),
    ];
    lines.extend(references.iter().map(|reference| {
        format!(
            "- {}: {} ({})",
            reference.kind.as_str(),
            reference.path.display(),
            reference.name
        )
    }));
    Some(lines.join("\n"))
}

pub fn session_context_reference_paths(references: &[SessionContextReference]) -> Vec<String> {
    references
        .iter()
        .map(|reference| reference.path.display().to_string())
        .collect()
}

pub fn session_context_reference_input_refs(references: &[SessionContextReference]) -> Vec<String> {
    references
        .iter()
        .map(|reference| {
            format!(
                "只读上下文引用：kind={} path={} name={}",
                reference.kind.as_str(),
                reference.path.display(),
                reference.name
            )
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionContextReferencePolicy {
    pub allowed_paths: Vec<String>,
    pub read_only_paths: Vec<String>,
}

pub fn session_context_reference_policy(
    references: &[SessionContextReference],
    workspace_root_path: Option<&str>,
    access_profile: magi_core::AccessProfile,
) -> SessionContextReferencePolicy {
    let read_only_paths = session_context_reference_paths(references);
    let mut allowed_paths = Vec::new();
    if access_profile != magi_core::AccessProfile::FullAccess && !read_only_paths.is_empty() {
        if let Some(workspace_root) = workspace_root_path
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            allowed_paths.push(workspace_root.to_string());
        }
        for path in &read_only_paths {
            if !allowed_paths.contains(path) {
                allowed_paths.push(path.clone());
            }
        }
    }
    SessionContextReferencePolicy {
        allowed_paths,
        read_only_paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::AccessProfile;

    #[test]
    fn restricted_reference_policy_preserves_workspace_and_read_only_external_paths() {
        let references = vec![SessionContextReference {
            kind: SessionContextReferenceKind::Directory,
            path: PathBuf::from("/tmp/external-reference"),
            name: "external-reference".to_string(),
        }];
        let policy = session_context_reference_policy(
            &references,
            Some("/tmp/workspace"),
            AccessProfile::Restricted,
        );

        assert_eq!(
            policy.allowed_paths,
            vec![
                "/tmp/workspace".to_string(),
                "/tmp/external-reference".to_string()
            ]
        );
        assert_eq!(
            policy.read_only_paths,
            vec!["/tmp/external-reference".to_string()]
        );

        let full_access = session_context_reference_policy(
            &references,
            Some("/tmp/workspace"),
            AccessProfile::FullAccess,
        );
        assert!(full_access.allowed_paths.is_empty());
        assert_eq!(full_access.read_only_paths, policy.read_only_paths);
    }
}
