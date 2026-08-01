//! 任务系统 — L12 SafetyGate：高危操作语义判定层。
//!
//! 与 L7 `PermissionEngine` 的边界：
//! - PermissionEngine 是**工具白名单**：按工具名 + read/write 性质给出 allow/deny。
//! - SafetyGate 是**语义判定**：同样是 `shell_exec`，但参数里出现
//!   `git push --force-with-lease` / `rm -rf /` 这种高危子串，需要单独拦下来。
//!
//! SafetyGate 是运行期拦截器：在工具调用真正落到
//! `ToolRegistry::execute_with_policy` 之前，先执行高危命令语义判定。
//!
//! 设计要点：
//! - 引擎本身无可变状态；规则以快照形式注入。
//! - 规则可以来自：内置默认集（`builtin_rules`）+ 用户在 settings 里自定义的
//!   `safeguardConfig.rules`。两者最终都汇成同一份 `Vec<SafetyRule>`。
//! - `evaluate` 返回 Safety Policy 动作：Allow / HardBlock /
//!   RequireApprovalInRestricted / AuditOnly。访问模式如何解释这些动作由
//!   上层 ToolPreflight 统一决定，SafetyGate 本身不持有用户授权状态。

use serde::{Deserialize, Serialize};
// --- SafetyCategory / SafetyRule

/// 规则分类。分类用于审计和默认动作推导，真正裁决由 `SafetyAction` 表达。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyCategory {
    /// 改写 git 历史：`git push --force` / `git rebase` / `git reset --hard` / `git commit --amend` 等。
    GitHistory,
    /// 丢弃本地修改：`git checkout --` / `git restore` / `git clean` / `git stash drop` 等。
    GitDiscard,
    /// 发布制品：`npm publish` / `cargo publish` / `pip upload` 等。
    PackagePublish,
    /// 批量删除：`rm -rf` / `rimraf` 等。
    BulkDelete,
    /// 用户自定义；默认 RequireApproval。
    Custom,
}

impl SafetyCategory {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "git_history" => Self::GitHistory,
            "git_discard" => Self::GitDiscard,
            "package_publish" => Self::PackagePublish,
            "bulk_delete" => Self::BulkDelete,
            _ => Self::Custom,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHistory => "git_history",
            Self::GitDiscard => "git_discard",
            Self::PackagePublish => "package_publish",
            Self::BulkDelete => "bulk_delete",
            Self::Custom => "custom",
        }
    }

    /// 内置高危类别默认在受限执行下拦截，custom 也默认走同一策略。
    /// 真正不可接受的模式必须在规则上显式声明 HardBlock。
    pub fn default_action(self) -> SafetyAction {
        SafetyAction::RequireApprovalInRestricted
    }
}

/// 单条规则：一个待匹配的模式 + 分类。`enabled=false` 的规则不参与判定。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyRule {
    pub pattern: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub category: SafetyCategory,
    #[serde(default)]
    pub action: SafetyAction,
}

fn default_true() -> bool {
    true
}

impl SafetyRule {
    pub fn new(pattern: impl Into<String>, category: SafetyCategory) -> Self {
        Self {
            pattern: pattern.into(),
            enabled: true,
            category,
            action: category.default_action(),
        }
    }

    pub fn with_action(
        pattern: impl Into<String>,
        category: SafetyCategory,
        action: SafetyAction,
    ) -> Self {
        Self {
            pattern: pattern.into(),
            enabled: true,
            category,
            action,
        }
    }

    fn matches(&self, command: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let pattern = self.pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        contains_token_sequence(&normalize_for_match(command), &normalize_for_match(pattern))
    }
}
// --- Decision

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyAction {
    /// 任何访问模式下都拒绝。
    HardBlock,
    /// 受限执行下拦截；完全授权下允许但应审计。
    #[default]
    RequireApprovalInRestricted,
    /// 允许执行，仅记录风险。
    AuditOnly,
}

impl SafetyAction {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hard_block" | "block" | "deny" => Self::HardBlock,
            "require_approval_in_restricted"
            | "require_approval"
            | "approval"
            | "needs_approval" => Self::RequireApprovalInRestricted,
            "audit_only" | "audit" => Self::AuditOnly,
            _ => Self::RequireApprovalInRestricted,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::HardBlock => "hard_block",
            Self::RequireApprovalInRestricted => "require_approval_in_restricted",
            Self::AuditOnly => "audit_only",
        }
    }

    pub fn precedence(self) -> u8 {
        match self {
            Self::HardBlock => 3,
            Self::RequireApprovalInRestricted => 2,
            Self::AuditOnly => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyMatch {
    pub category: SafetyCategory,
    pub pattern: String,
    pub action: SafetyAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyEvaluation {
    pub matches: Vec<SafetyMatch>,
    pub decision: SafetyDecision,
}

impl SafetyEvaluation {
    pub fn is_allow(&self) -> bool {
        self.decision.is_allow()
    }

    pub fn selected_match(&self) -> Option<&SafetyMatch> {
        self.matches.iter().find(|matched| match &self.decision {
            SafetyDecision::Allow => false,
            SafetyDecision::HardBlock { pattern, .. }
            | SafetyDecision::RequireApprovalInRestricted { pattern, .. }
            | SafetyDecision::AuditOnly { pattern, .. } => &matched.pattern == pattern,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafetyDecision {
    Allow,
    HardBlock {
        category: SafetyCategory,
        pattern: String,
        reason: String,
    },
    RequireApprovalInRestricted {
        category: SafetyCategory,
        pattern: String,
        reason: String,
    },
    AuditOnly {
        category: SafetyCategory,
        pattern: String,
        reason: String,
    },
}

impl SafetyDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
    pub fn is_block(&self) -> bool {
        matches!(self, Self::HardBlock { .. })
    }
    pub fn is_require_approval(&self) -> bool {
        matches!(self, Self::RequireApprovalInRestricted { .. })
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::HardBlock { reason, .. }
            | Self::RequireApprovalInRestricted { reason, .. }
            | Self::AuditOnly { reason, .. } => Some(reason),
        }
    }
}
// --- SafetyGate

/// 进程内单例：依据当前 settings 中的 safeguardConfig.rules 构造一次，
/// 每次工具调用前 evaluate。无可变状态，可 clone 任意分发。
#[derive(Clone, Debug, Default)]
pub struct SafetyGate {
    rules: Vec<SafetyRule>,
}

impl SafetyGate {
    pub fn new(rules: Vec<SafetyRule>) -> Self {
        Self { rules }
    }

    /// 内置默认规则集。任何调用方都应基于此再合并用户自定义规则。
    pub fn with_builtin_defaults() -> Self {
        Self::new(builtin_rules())
    }

    pub fn rules(&self) -> &[SafetyRule] {
        &self.rules
    }

    pub fn evaluate(&self, tool_name: &str, arguments_json: &str) -> SafetyDecision {
        self.evaluate_with_evidence(tool_name, arguments_json)
            .decision
    }

    pub fn evaluate_text(&self, command: &str) -> SafetyDecision {
        self.evaluate_text_with_evidence(command).decision
    }

    pub fn evaluate_with_evidence(
        &self,
        tool_name: &str,
        arguments_json: &str,
    ) -> SafetyEvaluation {
        let Some(operation) = operation_text_for_tool(tool_name, arguments_json) else {
            return SafetyEvaluation {
                matches: Vec::new(),
                decision: SafetyDecision::Allow,
            };
        };
        self.evaluate_text_with_evidence(&operation)
    }

    pub fn evaluate_text_with_evidence(&self, command: &str) -> SafetyEvaluation {
        let command = expand_command_aliases(command);
        let mut matches = self
            .rules
            .iter()
            .filter(|rule| rule.matches(&command))
            .map(|rule| SafetyMatch {
                category: rule.category,
                pattern: rule.pattern.trim().to_string(),
                action: rule.action,
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .action
                .precedence()
                .cmp(&left.action.precedence())
                .then_with(|| right.pattern.len().cmp(&left.pattern.len()))
                .then_with(|| left.pattern.cmp(&right.pattern))
        });

        let decision = matches
            .first()
            .map(decision_for_match)
            .unwrap_or(SafetyDecision::Allow);
        SafetyEvaluation { matches, decision }
    }
}

/// 从 settings 里序列化得到的"规则数组"（`safeguardConfig.rules`）反序列化成
/// `Vec<SafetyRule>`。容错：忽略缺字段或类型不匹配的条目，不让坏数据阻塞引擎构造。
pub fn rules_from_settings_value(value: &serde_json::Value) -> Vec<SafetyRule> {
    value
        .as_array()
        .map(|array| array.iter().filter_map(rule_from_json).collect::<Vec<_>>())
        .unwrap_or_default()
}

/// 合并内置默认规则与 settings 规则。
///
/// 同一 `(category, pattern)` 的 settings 规则覆盖内置规则，用于让用户在设置页
/// 调整内置规则的 enabled/action 后，运行期 SafetyGate 与 UI 保持一致；缺失的
/// 内置规则会自动补齐，保证版本升级新增默认规则时仍能生效。
pub fn merge_rules_with_builtin_defaults(settings_rules: Vec<SafetyRule>) -> Vec<SafetyRule> {
    let mut merged = builtin_rules();
    for rule in settings_rules {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| same_rule_identity(existing, &rule))
        {
            *existing = rule;
        } else {
            merged.push(rule);
        }
    }
    merged
}

fn same_rule_identity(left: &SafetyRule, right: &SafetyRule) -> bool {
    left.category == right.category
        && left
            .pattern
            .trim()
            .eq_ignore_ascii_case(right.pattern.trim())
}

fn rule_from_json(value: &serde_json::Value) -> Option<SafetyRule> {
    let object = value.as_object()?;
    let pattern = object.get("pattern")?.as_str()?.trim().to_string();
    if pattern.is_empty() {
        return None;
    }
    let enabled = object
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let category = object
        .get("category")
        .and_then(serde_json::Value::as_str)
        .map(SafetyCategory::parse)
        .unwrap_or(SafetyCategory::Custom);
    let action = object
        .get("action")
        .and_then(serde_json::Value::as_str)
        .map(SafetyAction::parse)
        .unwrap_or_else(|| category.default_action());
    Some(SafetyRule {
        pattern,
        enabled,
        category,
        action,
    })
}

fn extract_shell_command(arguments_json: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(arguments_json).ok()?;
    let object = value.as_object()?;
    object
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn operation_text_for_tool(tool_name: &str, arguments_json: &str) -> Option<String> {
    let tool_name = tool_name.trim();
    if tool_name == "shell_exec" {
        let command =
            extract_shell_command(arguments_json).unwrap_or_else(|| arguments_json.to_string());
        return Some(expand_command_aliases(&command));
    }

    let tool_is_structured_write = matches!(
        tool_name,
        "git_push"
            | "git_merge"
            | "git_branch_delete"
            | "git_worktree_remove"
            | "file_write"
            | "file_patch"
            | "file_remove"
            | "file_copy"
            | "file_move"
            | "file_mkdir"
    );
    if tool_is_structured_write {
        return Some({
            let mut operation = if matches!(tool_name, "file_write" | "file_patch") {
                serde_json::from_str::<serde_json::Value>(arguments_json)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .map(|path| format!("{tool_name} {path}"))
                    })
                    .unwrap_or_else(|| tool_name.to_string())
            } else {
                format!("{tool_name} {arguments_json}")
            };
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments_json) {
                if tool_name == "git_push"
                    && value
                        .get("force")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                {
                    operation.push_str(" git push --force");
                }
                if tool_name == "git_branch_delete"
                    && value
                        .get("force")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                {
                    operation.push_str(" git branch delete --force");
                }
            }
            operation
        });
    }

    let unscoped_tools = [
        "file_read",
        "view_image",
        "search_text",
        "search_semantic",
        "diff_preview",
        "web_search",
        "web_fetch",
        "knowledge_query",
        "code_symbols",
        "tool_catalog",
        "git_status",
        "git_branch_list",
        "git_worktree_list",
        "get_goal",
        "context_search",
        "context_read",
        "context_request",
    ];
    (!unscoped_tools.contains(&tool_name)).then(|| format!("{tool_name} {arguments_json}"))
}

fn decision_for_match(matched: &SafetyMatch) -> SafetyDecision {
    let reason = format!(
        "命中安全规则（{}）：{}",
        matched.category.as_str(),
        matched.pattern
    );
    match matched.action {
        SafetyAction::HardBlock => SafetyDecision::HardBlock {
            category: matched.category,
            pattern: matched.pattern.clone(),
            reason,
        },
        SafetyAction::RequireApprovalInRestricted => SafetyDecision::RequireApprovalInRestricted {
            category: matched.category,
            pattern: matched.pattern.clone(),
            reason,
        },
        SafetyAction::AuditOnly => SafetyDecision::AuditOnly {
            category: matched.category,
            pattern: matched.pattern.clone(),
            reason,
        },
    }
}

fn normalize_for_match(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| character.to_lowercase())
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn expand_command_aliases(command: &str) -> String {
    let mut operation = command.to_string();
    let normalized = normalize_for_match(command);
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        let following = &tokens[index.saturating_add(1)..];
        if *token == "rm" && following.contains(&"r") && following.contains(&"f") {
            operation.push_str(" rm -rf");
        }
        if *token == "remove"
            && following.first() == Some(&"item")
            && following.contains(&"recurse")
            && following.contains(&"force")
        {
            operation.push_str(" Remove-Item -Recurse -Force");
        }
        if *token == "rm" && following.contains(&"recurse") && following.contains(&"force") {
            operation.push_str(" Remove-Item -Recurse -Force");
        }
        if *token == "del" && following.contains(&"s") && following.contains(&"q") {
            operation.push_str(" del /s /q");
        }
        if *token == "rmdir" && following.contains(&"s") && following.contains(&"q") {
            operation.push_str(" rmdir /s /q");
        }
        if *token == "git"
            && following.first() == Some(&"push")
            && following
                .iter()
                .any(|value| matches!(*value, "f" | "force" | "forcewithlease"))
        {
            operation.push_str(" git push --force");
        }
    }
    operation
}

fn contains_token_sequence(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// 内置危险模式集合，后续只在本 crate 维护。
/// state.rs 的 seed_default_safeguard_rules 从此读取。
pub fn builtin_rules() -> Vec<SafetyRule> {
    use SafetyCategory::*;
    let raw: &[(&str, SafetyCategory)] = &[
        ("git push --force", GitHistory),
        ("git push -f", GitHistory),
        ("git rebase", GitHistory),
        ("git reset --hard", GitHistory),
        ("git commit --amend", GitHistory),
        ("git checkout --", GitDiscard),
        ("git restore", GitDiscard),
        ("git clean", GitDiscard),
        ("git stash drop", GitDiscard),
        ("npm publish", PackagePublish),
        ("cargo publish", PackagePublish),
        ("yarn publish", PackagePublish),
        ("pip upload", PackagePublish),
        ("rm -rf", BulkDelete),
        ("rimraf", BulkDelete),
        ("remove-item -recurse -force", BulkDelete),
        ("del /s /q", BulkDelete),
        ("rmdir /s /q", BulkDelete),
    ];
    raw.iter()
        .map(|(pattern, category)| SafetyRule::new(*pattern, *category))
        .collect()
}
// --- Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_exec_force_push_requires_restricted_approval() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "git push --force origin main" }).to_string();
        let decision = gate.evaluate("shell_exec", &args);
        assert!(matches!(
            decision,
            SafetyDecision::RequireApprovalInRestricted {
                category: SafetyCategory::GitHistory,
                ..
            }
        ));
        assert!(
            !decision.reason().unwrap_or_default().contains("SafetyGate"),
            "公开拦截理由不应暴露内部组件名"
        );
    }

    #[test]
    fn shell_exec_rm_rf_requires_restricted_approval() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "rm -rf /tmp/foo" }).to_string();
        let decision = gate.evaluate("shell_exec", &args);
        assert!(matches!(
            decision,
            SafetyDecision::RequireApprovalInRestricted {
                category: SafetyCategory::BulkDelete,
                ..
            }
        ));
    }

    #[test]
    fn shell_exec_safe_command_is_allowed() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "ls -la /tmp" }).to_string();
        assert_eq!(gate.evaluate("shell_exec", &args), SafetyDecision::Allow);
    }

    #[test]
    fn non_shell_tools_are_passthrough() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "rm -rf /" }).to_string();
        assert_eq!(gate.evaluate("file_read", &args), SafetyDecision::Allow);
    }

    #[test]
    fn disabled_rule_is_ignored() {
        let rule = SafetyRule {
            pattern: "rm -rf".to_string(),
            enabled: false,
            category: SafetyCategory::BulkDelete,
            action: SafetyAction::RequireApprovalInRestricted,
        };
        let gate = SafetyGate::new(vec![rule]);
        let args = serde_json::json!({ "command": "rm -rf /tmp" }).to_string();
        assert_eq!(gate.evaluate("shell_exec", &args), SafetyDecision::Allow);
    }

    #[test]
    fn custom_category_requires_restricted_approval() {
        let gate = SafetyGate::new(vec![SafetyRule::new("aws s3 rm", SafetyCategory::Custom)]);
        let args =
            serde_json::json!({ "command": "aws s3 rm s3://bucket --recursive" }).to_string();
        match gate.evaluate("shell_exec", &args) {
            SafetyDecision::RequireApprovalInRestricted { category, .. } => {
                assert_eq!(category, SafetyCategory::Custom);
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }

    #[test]
    fn case_insensitive_matching() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "GIT PUSH --FORCE origin main" }).to_string();
        assert!(gate.evaluate("shell_exec", &args).is_require_approval());
    }

    #[test]
    fn normalized_matching_covers_split_posix_flags() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "command": "rm -r -f ./build" }).to_string();
        assert!(gate.evaluate("shell_exec", &args).is_require_approval());
    }

    #[test]
    fn normalized_matching_covers_power_shell_bulk_delete() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({
            "command": "Remove-Item -Force -Recurse ./build"
        })
        .to_string();
        assert!(gate.evaluate("shell_exec", &args).is_require_approval());
    }

    #[test]
    fn strongest_matching_action_wins_regardless_of_rule_order() {
        let gate = SafetyGate::new(vec![
            SafetyRule::with_action("deploy", SafetyCategory::Custom, SafetyAction::AuditOnly),
            SafetyRule::with_action(
                "deploy production",
                SafetyCategory::Custom,
                SafetyAction::HardBlock,
            ),
        ]);
        let decision = gate.evaluate(
            "shell_exec",
            &serde_json::json!({ "command": "deploy production" }).to_string(),
        );
        assert!(decision.is_block());
    }

    #[test]
    fn structured_git_push_uses_the_same_safety_policy() {
        let gate = SafetyGate::with_builtin_defaults();
        let args = serde_json::json!({ "force": true, "remote": "origin" }).to_string();
        assert!(gate.evaluate("git_push", &args).is_require_approval());
    }

    #[test]
    fn audit_evaluation_keeps_all_matches_for_observability() {
        let gate = SafetyGate::new(vec![
            SafetyRule::with_action("deploy", SafetyCategory::Custom, SafetyAction::AuditOnly),
            SafetyRule::with_action(
                "deploy production",
                SafetyCategory::Custom,
                SafetyAction::AuditOnly,
            ),
        ]);
        let evaluation = gate.evaluate_with_evidence(
            "shell_exec",
            &serde_json::json!({ "command": "deploy production" }).to_string(),
        );
        assert_eq!(evaluation.matches.len(), 2);
        assert!(matches!(
            evaluation.decision,
            SafetyDecision::AuditOnly { .. }
        ));
    }

    #[test]
    fn rules_from_settings_value_round_trip() {
        let json = serde_json::json!([
            { "pattern": "git push --force", "enabled": true, "category": "git_history" },
            { "pattern": "  ", "category": "custom" }, // pattern 为空：丢弃
            { "pattern": "aws s3 rm", "category": "custom", "action": "hard_block" },
            "not-an-object", // 非对象：丢弃
        ]);
        let rules = rules_from_settings_value(&json);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "git push --force");
        assert_eq!(rules[0].category, SafetyCategory::GitHistory);
        assert_eq!(rules[1].category, SafetyCategory::Custom);
        assert_eq!(rules[1].action, SafetyAction::HardBlock);
    }

    #[test]
    fn settings_rules_override_builtin_defaults_and_keep_missing_builtins() {
        let rules = merge_rules_with_builtin_defaults(vec![
            SafetyRule {
                pattern: "git push --force".to_string(),
                enabled: false,
                category: SafetyCategory::GitHistory,
                action: SafetyAction::AuditOnly,
            },
            SafetyRule::with_action("aws s3 rm", SafetyCategory::Custom, SafetyAction::HardBlock),
        ]);

        let force_push = rules
            .iter()
            .find(|rule| {
                rule.category == SafetyCategory::GitHistory && rule.pattern == "git push --force"
            })
            .expect("builtin rule should stay present");
        assert!(!force_push.enabled);
        assert_eq!(force_push.action, SafetyAction::AuditOnly);
        assert!(
            rules
                .iter()
                .any(|rule| rule.category == SafetyCategory::BulkDelete && rule.pattern == "rm -rf"),
            "未在 settings 中出现的内置规则应继续补齐"
        );
        assert!(
            rules
                .iter()
                .any(|rule| rule.category == SafetyCategory::Custom && rule.pattern == "aws s3 rm"),
            "自定义规则应保留"
        );
    }

    #[test]
    fn merged_gate_honors_disabled_builtin_rule() {
        let rules = merge_rules_with_builtin_defaults(vec![SafetyRule {
            pattern: "rm -rf".to_string(),
            enabled: false,
            category: SafetyCategory::BulkDelete,
            action: SafetyAction::RequireApprovalInRestricted,
        }]);
        let gate = SafetyGate::new(rules);
        let args = serde_json::json!({ "command": "rm -rf /tmp/foo" }).to_string();

        assert_eq!(gate.evaluate("shell_exec", &args), SafetyDecision::Allow);
    }

    #[test]
    fn explicit_hard_block_rule_blocks_in_every_mode() {
        let gate = SafetyGate::new(vec![SafetyRule::with_action(
            "dangerous-op",
            SafetyCategory::Custom,
            SafetyAction::HardBlock,
        )]);
        let args = serde_json::json!({ "command": "dangerous-op --now" }).to_string();
        assert!(matches!(
            gate.evaluate("shell_exec", &args),
            SafetyDecision::HardBlock { .. }
        ));
    }

    #[test]
    fn builtin_rules_cover_default_patterns() {
        let rules = builtin_rules();
        // 默认规则集保持 18 条内置规则。
        assert_eq!(rules.len(), 18);
        assert!(rules.iter().all(|r| r.enabled));
        assert!(rules.iter().any(|r| r.pattern == "rm -rf"));
        assert!(rules.iter().any(|r| r.pattern == "cargo publish"));
    }
}
