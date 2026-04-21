use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationConfig {
    pub compile_check: bool,
    pub compile_command: String,
    #[serde(default = "default_missing_command_policy")]
    pub compile_missing_command_policy: MissingCommandPolicy,
    pub ide_check: bool,
    pub lint_check: bool,
    pub lint_command: String,
    pub test_check: bool,
    pub test_command: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingCommandPolicy {
    Warn,
    Fail,
}

fn default_missing_command_policy() -> MissingCommandPolicy {
    MissingCommandPolicy::Warn
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            compile_check: true,
            compile_command: "npm run compile".to_string(),
            compile_missing_command_policy: MissingCommandPolicy::Warn,
            ide_check: true,
            lint_check: false,
            lint_command: "npm run lint".to_string(),
            test_check: false,
            test_command: "npm test".to_string(),
            timeout_ms: 60_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compile_result: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint_result: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_result: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ide_result: Option<IdeDiagnosticResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeDiagnosticResult {
    pub success: bool,
    pub errors: usize,
    pub warnings: usize,
    pub details: Vec<IdeDiagnosticDetail>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeDiagnosticDetail {
    pub file: String,
    pub line: u32,
    pub message: String,
    pub severity: DiagnosticSeverity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

const NON_BLOCKING_WARNING_PATTERNS: &[&str] = &[
    "自动跳过编译检查",
    "未找到可用编译命令",
];

pub fn is_non_blocking_verification_warning(warning: &str) -> bool {
    let trimmed = warning.trim();
    if trimmed.is_empty() {
        return false;
    }
    NON_BLOCKING_WARNING_PATTERNS
        .iter()
        .any(|pat| trimmed.contains(pat))
}

pub struct VerificationRunner {
    config: VerificationConfig,
    workspace_root: PathBuf,
}

impl VerificationRunner {
    pub fn new(workspace_root: impl Into<PathBuf>, config: Option<VerificationConfig>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            config: config.unwrap_or_default(),
        }
    }

    pub fn update_config(&mut self, config: VerificationConfig) {
        self.config = config;
    }

    pub fn clone_with_overrides(&self, overrides: VerificationConfig) -> Self {
        Self {
            workspace_root: self.workspace_root.clone(),
            config: overrides,
        }
    }

    pub fn run_verification(
        &self,
        task_id: &str,
        modified_files: Option<&[String]>,
    ) -> VerificationResult {
        info!(task_id, "验证开始");

        let verification_roots = self.resolve_verification_roots(modified_files);

        let mut result = VerificationResult {
            success: true,
            compile_result: None,
            lint_result: None,
            test_result: None,
            ide_result: None,
            warnings: Vec::new(),
            summary: String::new(),
        };
        let mut summary_parts = Vec::new();

        if self.config.compile_check {
            let compile_result = self.run_project_checks(
                &verification_roots,
                "编译",
                |root| self.resolve_compile_command(root),
                "自动跳过编译检查：未检测到编译命令",
                self.config.compile_missing_command_policy,
            );
            if !compile_result.success {
                result.success = false;
                summary_parts.push(format!(
                    "编译失败: {}",
                    compile_result.error.as_deref().unwrap_or("未知错误")
                ));
            } else if verification_roots.len() > 1 {
                summary_parts.push(format!("编译通过（{} 个项目）", verification_roots.len()));
            } else {
                summary_parts.push("编译通过".to_string());
            }
            if !compile_result.warnings.is_empty() {
                let user_facing: Vec<&str> = compile_result
                    .warnings
                    .iter()
                    .filter(|w| !is_non_blocking_verification_warning(w))
                    .map(|w| w.as_str())
                    .collect();
                if !user_facing.is_empty() {
                    summary_parts.push(format!("编译告警: {}", user_facing.join("；")));
                }
                result.warnings.extend(compile_result.warnings.clone());
            }
            result.compile_result = Some(compile_result);
        }

        if self.config.lint_check {
            let lint_result = self.run_project_checks(
                &verification_roots,
                "Lint",
                |root| self.resolve_lint_command(root),
                "未找到可用 Lint 命令",
                MissingCommandPolicy::Fail,
            );
            if !lint_result.success {
                result.success = false;
                summary_parts.push(format!(
                    "Lint 失败: {}",
                    lint_result.error.as_deref().unwrap_or("未知错误")
                ));
            } else if verification_roots.len() > 1 {
                summary_parts.push(format!("Lint 通过（{} 个项目）", verification_roots.len()));
            } else {
                summary_parts.push("Lint 通过".to_string());
            }
            result.lint_result = Some(lint_result);
        }

        if self.config.test_check {
            let test_result = self.run_project_checks(
                &verification_roots,
                "测试",
                |root| self.resolve_test_command(root),
                "未找到可用测试命令",
                MissingCommandPolicy::Fail,
            );
            if !test_result.success {
                result.success = false;
                summary_parts.push(format!(
                    "测试失败: {}",
                    test_result.error.as_deref().unwrap_or("未知错误")
                ));
            } else if verification_roots.len() > 1 {
                summary_parts.push(format!("测试通过（{} 个项目）", verification_roots.len()));
            } else {
                summary_parts.push("测试通过".to_string());
            }
            result.test_result = Some(test_result);
        }

        result.summary = summary_parts.join(" | ");

        info!(task_id, success = result.success, "验证完成");
        result
    }

    pub fn get_error_details(&self, result: &VerificationResult) -> String {
        let mut details = Vec::new();

        if let Some(ref cr) = result.compile_result {
            if !cr.success {
                details.push(format!(
                    "编译错误:\n{}",
                    cr.error.as_deref().unwrap_or(&cr.output)
                ));
            }
        }

        if let Some(ref ir) = result.ide_result {
            if !ir.success {
                let error_lines: Vec<String> = ir
                    .details
                    .iter()
                    .filter(|d| d.severity == DiagnosticSeverity::Error)
                    .map(|d| format!("  {}:{}: {}", d.file, d.line, d.message))
                    .collect();
                details.push(format!("IDE 错误:\n{}", error_lines.join("\n")));
            }
        }

        if let Some(ref lr) = result.lint_result {
            if !lr.success {
                details.push(format!(
                    "Lint 错误:\n{}",
                    lr.error.as_deref().unwrap_or(&lr.output)
                ));
            }
        }

        if let Some(ref tr) = result.test_result {
            if !tr.success {
                details.push(format!(
                    "测试错误:\n{}",
                    tr.error.as_deref().unwrap_or(&tr.output)
                ));
            }
        }

        details.join("\n\n")
    }

    pub fn quick_compile_check(&self) -> bool {
        if !self.config.compile_check {
            return true;
        }
        let Some(cmd) = self.resolve_compile_command(&self.workspace_root) else {
            return self.config.compile_missing_command_policy == MissingCommandPolicy::Warn;
        };
        let result = self.run_command(&cmd, "编译", &self.workspace_root);
        result.success
    }

    fn run_command(&self, command: &str, name: &str, cwd: &Path) -> CommandResult {
        let start = Instant::now();
        let timeout = Duration::from_millis(self.config.timeout_ms);

        let shell = if cfg!(target_os = "windows") { "cmd" } else { "sh" };
        let flag = if cfg!(target_os = "windows") { "/C" } else { "-c" };

        let output = Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(cwd)
            .output();

        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let success = output.status.success();

                CommandResult {
                    success,
                    output: stdout,
                    error: if !success {
                        Some(if stderr.is_empty() {
                            format!(
                                "{name}失败，退出码: {}",
                                output.status.code().unwrap_or(-1)
                            )
                        } else {
                            stderr
                        })
                    } else {
                        None
                    },
                    warnings: Vec::new(),
                    duration_ms: duration_ms.min(timeout.as_millis() as u64),
                }
            }
            Err(err) => CommandResult {
                success: false,
                output: String::new(),
                error: Some(format!("{name}执行错误: {err}")),
                warnings: Vec::new(),
                duration_ms,
            },
        }
    }

    fn run_project_checks(
        &self,
        project_roots: &[PathBuf],
        name: &str,
        resolve_command: impl Fn(&Path) -> Option<String>,
        missing_command_message: &str,
        missing_command_policy: MissingCommandPolicy,
    ) -> CommandResult {
        let roots = if project_roots.is_empty() {
            vec![self.workspace_root.clone()]
        } else {
            project_roots.to_vec()
        };

        let mut outputs = Vec::new();
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut duration_ms = 0u64;
        let mut success = true;

        for root in &roots {
            let Some(command) = resolve_command(root) else {
                let msg = format!("[{}] {}", root.display(), missing_command_message);
                match missing_command_policy {
                    MissingCommandPolicy::Warn => warnings.push(msg),
                    MissingCommandPolicy::Fail => {
                        success = false;
                        errors.push(msg);
                    }
                }
                continue;
            };

            let cmd_result = self.run_command(&command, name, root);
            duration_ms += cmd_result.duration_ms;

            let output_trimmed = cmd_result.output.trim();
            if !output_trimmed.is_empty() {
                outputs.push(format!("[{}]\n{}", root.display(), output_trimmed));
            }

            if !cmd_result.success {
                success = false;
                let error_text = cmd_result
                    .error
                    .as_deref()
                    .map(|e| e.trim())
                    .unwrap_or("未知错误");
                errors.push(format!("[{}] {}", root.display(), error_text));
            }
        }

        CommandResult {
            success,
            output: outputs.join("\n\n"),
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("\n\n"))
            },
            warnings,
            duration_ms,
        }
    }

    fn resolve_verification_roots(&self, modified_files: Option<&[String]>) -> Vec<PathBuf> {
        let Some(files) = modified_files else {
            return vec![self.workspace_root.clone()];
        };

        let mut hit_count = std::collections::HashMap::<PathBuf, usize>::new();
        for file in files {
            let normalized = self.normalize_modified_path(file);
            let Some(normalized) = normalized else { continue };
            let project_root = self
                .find_nearest_project_root(&normalized)
                .unwrap_or_else(|| self.workspace_root.clone());
            *hit_count.entry(project_root).or_insert(0) += 1;
        }

        if hit_count.is_empty() {
            return vec![self.workspace_root.clone()];
        }

        let mut entries: Vec<(PathBuf, usize)> = hit_count.into_iter().collect();
        entries.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.0.as_os_str().len().cmp(&a.0.as_os_str().len()))
        });
        entries.into_iter().map(|(root, _)| root).collect()
    }

    fn normalize_modified_path(&self, file: &str) -> Option<PathBuf> {
        let trimmed = file.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        Some(self.workspace_root.join(trimmed))
    }

    fn find_nearest_project_root(&self, file_path: &Path) -> Option<PathBuf> {
        let mut current = if file_path.is_file() {
            file_path.parent()?.to_path_buf()
        } else {
            file_path.to_path_buf()
        };

        let workspace_root = self.workspace_root.canonicalize().ok()?;

        loop {
            if current.join("package.json").exists()
                || current.join("tsconfig.json").exists()
                || current.join("Cargo.toml").exists()
                || current.join("pyproject.toml").exists()
                || current.join("go.mod").exists()
            {
                return Some(current);
            }

            if current.canonicalize().ok()? == workspace_root {
                break;
            }

            let parent = current.parent()?.to_path_buf();
            if parent == current {
                break;
            }
            current = parent;
        }

        None
    }

    fn resolve_compile_command(&self, cwd: &Path) -> Option<String> {
        let configured = self.config.compile_command.trim();
        if !configured.is_empty() && configured != "npm run compile" {
            return Some(configured.to_string());
        }

        let scripts = read_package_scripts(cwd);
        if scripts.contains_key("compile") {
            return Some("npm run compile".to_string());
        }
        if scripts.contains_key("typecheck") {
            return Some("npm run typecheck".to_string());
        }
        if scripts.contains_key("check") {
            return Some("npm run check".to_string());
        }

        let tsconfig = cwd.join("tsconfig.json");
        if tsconfig.exists() {
            return Some(format!("npx tsc --noEmit -p \"{}\"", tsconfig.display()));
        }

        if scripts.contains_key("build") {
            return Some("npm run build".to_string());
        }

        // Rust 项目检测
        let cargo_toml = cwd.join("Cargo.toml");
        if cargo_toml.exists() {
            return Some("cargo check".to_string());
        }

        None
    }

    fn resolve_lint_command(&self, cwd: &Path) -> Option<String> {
        let configured = self.config.lint_command.trim();
        if !configured.is_empty() && configured != "npm run lint" {
            return Some(configured.to_string());
        }

        let scripts = read_package_scripts(cwd);
        if scripts.contains_key("lint") {
            return Some("npm run lint".to_string());
        }

        None
    }

    fn resolve_test_command(&self, cwd: &Path) -> Option<String> {
        let configured = self.config.test_command.trim();
        if !configured.is_empty() && configured != "npm test" {
            return Some(configured.to_string());
        }

        let scripts = read_package_scripts(cwd);
        if scripts.contains_key("test") {
            return Some("npm test".to_string());
        }

        // Rust 项目检测
        let cargo_toml = cwd.join("Cargo.toml");
        if cargo_toml.exists() {
            return Some("cargo test".to_string());
        }

        None
    }
}

fn read_package_scripts(cwd: &Path) -> std::collections::HashMap<String, String> {
    let package_json = cwd.join("package.json");
    if !package_json.exists() {
        return std::collections::HashMap::new();
    }
    let Ok(content) = std::fs::read_to_string(&package_json) else {
        return std::collections::HashMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
        return std::collections::HashMap::new();
    };
    let Some(scripts) = parsed.get("scripts").and_then(|s| s.as_object()) else {
        return std::collections::HashMap::new();
    };
    scripts
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}
