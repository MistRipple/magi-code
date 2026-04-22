use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSpecType {
    FileExists,
    FileContent,
    TestPass,
    TaskCompleted,
    Custom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentMatchMode {
    Contains,
    Exact,
    Regex,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationSpec {
    #[serde(rename = "type")]
    pub spec_type: VerificationSpecType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_match_mode: Option<ContentMatchMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_validator: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    pub id: String,
    pub description: String,
    pub verifiable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_spec: Option<VerificationSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionExecutionReport {
    pub criterion_id: String,
    #[serde(rename = "type")]
    pub spec_type: VerificationSpecType,
    pub status: String,
    pub detail: String,
    pub executor_id: String,
}

pub struct VerificationContext {
    pub workspace_root: String,
    pub modified_files: Vec<String>,
}

fn build_failure(
    criterion: &AcceptanceCriterion,
    spec_type: VerificationSpecType,
    detail: &str,
    executor_id: &str,
) -> CriterionExecutionReport {
    CriterionExecutionReport {
        criterion_id: criterion.id.clone(),
        spec_type,
        status: "failed".to_string(),
        detail: detail.to_string(),
        executor_id: executor_id.to_string(),
    }
}

fn execute_file_exists(
    criterion: &AcceptanceCriterion,
    ctx: &VerificationContext,
) -> CriterionExecutionReport {
    let spec = criterion.verification_spec.as_ref().unwrap();
    let Some(target) = &spec.target_path else {
        return build_failure(
            criterion,
            VerificationSpecType::FileExists,
            "file_exists: targetPath 未指定",
            "builtin:file_exists",
        );
    };
    let full_path = Path::new(&ctx.workspace_root).join(target);
    let exists = full_path.exists();
    CriterionExecutionReport {
        criterion_id: criterion.id.clone(),
        spec_type: VerificationSpecType::FileExists,
        status: if exists { "passed" } else { "failed" }.to_string(),
        detail: if exists {
            format!("文件存在: {target}")
        } else {
            format!("文件不存在: {target}")
        },
        executor_id: "builtin:file_exists".to_string(),
    }
}

fn execute_file_content(
    criterion: &AcceptanceCriterion,
    ctx: &VerificationContext,
) -> CriterionExecutionReport {
    let spec = criterion.verification_spec.as_ref().unwrap();
    let (Some(target), Some(expected)) = (&spec.target_path, &spec.expected_content) else {
        return build_failure(
            criterion,
            VerificationSpecType::FileContent,
            "file_content: targetPath 或 expectedContent 未指定",
            "builtin:file_content",
        );
    };
    let full_path = Path::new(&ctx.workspace_root).join(target);
    let Ok(content) = std::fs::read_to_string(&full_path) else {
        return build_failure(
            criterion,
            VerificationSpecType::FileContent,
            &format!("文件不存在: {target}"),
            "builtin:file_content",
        );
    };
    let mode = spec
        .content_match_mode
        .as_ref()
        .unwrap_or(&ContentMatchMode::Contains);
    let matched = match mode {
        ContentMatchMode::Exact => content == *expected,
        ContentMatchMode::Contains => content.contains(expected.as_str()),
        ContentMatchMode::Regex => regex::Regex::new(expected)
            .map(|re| re.is_match(&content))
            .unwrap_or(false),
    };
    let mode_label = match mode {
        ContentMatchMode::Exact => "exact",
        ContentMatchMode::Contains => "contains",
        ContentMatchMode::Regex => "regex",
    };
    CriterionExecutionReport {
        criterion_id: criterion.id.clone(),
        spec_type: VerificationSpecType::FileContent,
        status: if matched { "passed" } else { "failed" }.to_string(),
        detail: if matched {
            format!("文件内容匹配({mode_label}): {target}")
        } else {
            format!("文件内容不匹配({mode_label}): {target}")
        },
        executor_id: "builtin:file_content".to_string(),
    }
}

pub struct ValidatorRegistry {
    executors: HashMap<VerificationSpecType, String>,
}

impl ValidatorRegistry {
    pub fn new() -> Self {
        let mut executors = HashMap::new();
        executors.insert(
            VerificationSpecType::FileExists,
            "builtin:file_exists".to_string(),
        );
        executors.insert(
            VerificationSpecType::FileContent,
            "builtin:file_content".to_string(),
        );
        executors.insert(
            VerificationSpecType::TaskCompleted,
            "builtin:task_completed".to_string(),
        );
        executors.insert(
            VerificationSpecType::TestPass,
            "builtin:test_pass".to_string(),
        );
        executors.insert(
            VerificationSpecType::Custom,
            "builtin:custom".to_string(),
        );
        Self { executors }
    }

    pub fn register(&mut self, spec_type: VerificationSpecType, executor_id: &str) {
        self.executors.insert(spec_type, executor_id.to_string());
    }

    pub fn has_executor(&self, spec_type: &VerificationSpecType) -> bool {
        self.executors.contains_key(spec_type)
    }

    pub fn execute_criterion(
        &self,
        criterion: &AcceptanceCriterion,
        ctx: &VerificationContext,
    ) -> Option<CriterionExecutionReport> {
        if !criterion.verifiable {
            return None;
        }
        let spec = criterion.verification_spec.as_ref()?;

        if !self.executors.contains_key(&spec.spec_type) {
            return Some(build_failure(
                criterion,
                spec.spec_type,
                &format!("未知验证类型: {:?}", spec.spec_type),
                "registry:missing-executor",
            ));
        }

        Some(match spec.spec_type {
            VerificationSpecType::FileExists => execute_file_exists(criterion, ctx),
            VerificationSpecType::FileContent => execute_file_content(criterion, ctx),
            VerificationSpecType::TestPass => build_failure(
                criterion,
                VerificationSpecType::TestPass,
                "test_pass: 需要外部命令执行器",
                "builtin:test_pass",
            ),
            VerificationSpecType::TaskCompleted => build_failure(
                criterion,
                VerificationSpecType::TaskCompleted,
                "task_completed: 需要 batch 上下文",
                "builtin:task_completed",
            ),
            VerificationSpecType::Custom => build_failure(
                criterion,
                VerificationSpecType::Custom,
                "custom: 需要外部校验器",
                "builtin:custom",
            ),
        })
    }

    pub fn execute_criteria(
        &self,
        criteria: &[AcceptanceCriterion],
        ctx: &VerificationContext,
    ) -> Vec<CriterionExecutionReport> {
        criteria
            .iter()
            .filter_map(|c| self.execute_criterion(c, ctx))
            .collect()
    }
}

impl Default for ValidatorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn make_criterion(
        id: &str,
        spec_type: VerificationSpecType,
        target_path: Option<&str>,
        expected_content: Option<&str>,
    ) -> AcceptanceCriterion {
        AcceptanceCriterion {
            id: id.to_string(),
            description: "test".to_string(),
            verifiable: true,
            verification_spec: Some(VerificationSpec {
                spec_type,
                target_path: target_path.map(|s| s.to_string()),
                expected_content: expected_content.map(|s| s.to_string()),
                content_match_mode: None,
                test_command: None,
                task_pattern: None,
                custom_validator: None,
            }),
        }
    }

    #[test]
    fn file_exists_pass() {
        let dir = temp_workspace();
        let file_path = dir.path().join("test.txt");
        std::fs::File::create(&file_path).unwrap();

        let registry = ValidatorRegistry::new();
        let criterion = make_criterion("c1", VerificationSpecType::FileExists, Some("test.txt"), None);
        let ctx = VerificationContext {
            workspace_root: dir.path().to_string_lossy().to_string(),
            modified_files: vec![],
        };
        let result = registry.execute_criterion(&criterion, &ctx).unwrap();
        assert_eq!(result.status, "passed");
    }

    #[test]
    fn file_exists_fail() {
        let dir = temp_workspace();
        let registry = ValidatorRegistry::new();
        let criterion =
            make_criterion("c1", VerificationSpecType::FileExists, Some("missing.txt"), None);
        let ctx = VerificationContext {
            workspace_root: dir.path().to_string_lossy().to_string(),
            modified_files: vec![],
        };
        let result = registry.execute_criterion(&criterion, &ctx).unwrap();
        assert_eq!(result.status, "failed");
    }

    #[test]
    fn file_content_contains_pass() {
        let dir = temp_workspace();
        let file_path = dir.path().join("hello.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"hello world foo bar").unwrap();

        let registry = ValidatorRegistry::new();
        let criterion = make_criterion(
            "c2",
            VerificationSpecType::FileContent,
            Some("hello.txt"),
            Some("world foo"),
        );
        let ctx = VerificationContext {
            workspace_root: dir.path().to_string_lossy().to_string(),
            modified_files: vec![],
        };
        let result = registry.execute_criterion(&criterion, &ctx).unwrap();
        assert_eq!(result.status, "passed");
    }

    #[test]
    fn file_content_not_found() {
        let dir = temp_workspace();
        let registry = ValidatorRegistry::new();
        let criterion = make_criterion(
            "c3",
            VerificationSpecType::FileContent,
            Some("nope.txt"),
            Some("content"),
        );
        let ctx = VerificationContext {
            workspace_root: dir.path().to_string_lossy().to_string(),
            modified_files: vec![],
        };
        let result = registry.execute_criterion(&criterion, &ctx).unwrap();
        assert_eq!(result.status, "failed");
        assert!(result.detail.contains("不存在"));
    }

    #[test]
    fn non_verifiable_returns_none() {
        let registry = ValidatorRegistry::new();
        let criterion = AcceptanceCriterion {
            id: "c4".to_string(),
            description: "not verifiable".to_string(),
            verifiable: false,
            verification_spec: None,
        };
        let ctx = VerificationContext {
            workspace_root: "/tmp".to_string(),
            modified_files: vec![],
        };
        assert!(registry.execute_criterion(&criterion, &ctx).is_none());
    }

    #[test]
    fn execute_criteria_batch() {
        let dir = temp_workspace();
        let file_path = dir.path().join("a.txt");
        std::fs::File::create(&file_path).unwrap();

        let registry = ValidatorRegistry::new();
        let criteria = vec![
            make_criterion("c1", VerificationSpecType::FileExists, Some("a.txt"), None),
            make_criterion("c2", VerificationSpecType::FileExists, Some("b.txt"), None),
        ];
        let ctx = VerificationContext {
            workspace_root: dir.path().to_string_lossy().to_string(),
            modified_files: vec![],
        };
        let results = registry.execute_criteria(&criteria, &ctx);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].status, "passed");
        assert_eq!(results[1].status, "failed");
    }

    #[test]
    fn has_executor_registered() {
        let registry = ValidatorRegistry::new();
        assert!(registry.has_executor(&VerificationSpecType::FileExists));
        assert!(registry.has_executor(&VerificationSpecType::TestPass));
    }
}
