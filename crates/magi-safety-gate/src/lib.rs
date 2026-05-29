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

// ---------------------------------------------------------------------------
// SafetyCategory / SafetyRule
// ---------------------------------------------------------------------------

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

    /// 内置高危类别默认在受限执行下审批，custom 也默认审批。
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

    /// 大小写不敏感的子串匹配。模式形如 `git push --force` 或 `rm -rf`。
    fn matches(&self, command: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let pattern = self.pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        command
            .to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
    }
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyAction {
    /// 任何访问模式下都拒绝。
    HardBlock,
    /// 受限执行下需要批准；完全授权下允许但应审计。
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

// ---------------------------------------------------------------------------
// SafetyGate
// ---------------------------------------------------------------------------

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

    /// 主判定入口：从工具名 + arguments JSON 中抽出待审命令文本，过一遍规则。
    ///
    /// - `shell_exec`：提取 arguments.command（若没有则把整个 JSON 当作待审字符串）。
    /// - 其他工具：当前版本只对 `shell_exec` 生效；扩展点见 `evaluate_text`。
    pub fn evaluate(&self, tool_name: &str, arguments_json: &str) -> SafetyDecision {
        if tool_name.trim() != "shell_exec" {
            return SafetyDecision::Allow;
        }
        let command =
            extract_shell_command(arguments_json).unwrap_or_else(|| arguments_json.to_string());
        self.evaluate_text(&command)
    }

    /// 直接对一段文本（命令行 / 提交信息 / 任意载荷）做规则匹配。
    pub fn evaluate_text(&self, command: &str) -> SafetyDecision {
        for rule in &self.rules {
            if rule.matches(command) {
                let pattern = rule.pattern.clone();
                let reason = format!(
                    "命中 SafetyGate {} 规则：{}",
                    rule.category.as_str(),
                    pattern
                );
                return match rule.action {
                    SafetyAction::HardBlock => SafetyDecision::HardBlock {
                        category: rule.category,
                        pattern,
                        reason,
                    },
                    SafetyAction::RequireApprovalInRestricted => {
                        SafetyDecision::RequireApprovalInRestricted {
                            category: rule.category,
                            pattern,
                            reason,
                        }
                    }
                    SafetyAction::AuditOnly => SafetyDecision::AuditOnly {
                        category: rule.category,
                        pattern,
                        reason,
                    },
                };
            }
        }
        SafetyDecision::Allow
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
    ];
    raw.iter()
        .map(|(pattern, category)| SafetyRule::new(*pattern, *category))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        assert_eq!(gate.evaluate("file_view", &args), SafetyDecision::Allow);
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
        // 默认规则集保持 15 条内置规则。
        assert_eq!(rules.len(), 15);
        assert!(rules.iter().all(|r| r.enabled));
        assert!(rules.iter().any(|r| r.pattern == "rm -rf"));
        assert!(rules.iter().any(|r| r.pattern == "cargo publish"));
    }
}
