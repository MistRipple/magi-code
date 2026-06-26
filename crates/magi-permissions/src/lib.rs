//! 任务系统 — L7 Permissions：三维（工具 / 目录 / 命令）× 产品级访问模式
//! （read_only / restricted / full_access）权限引擎。
//!
//! 目标：把工具类别判定、工具白名单、shell 命令写入识别、Task.policy
//! 中的 allow/deny 列表统一收敛到一个 `PermissionEngine`，
//! 让 Conversation/Task 在调用工具或访问路径前都经过同一份判定。
//! 具体产品的工具名分类由调用方注入；本 crate 不硬编码 Magi 内置工具清单。
//!
//! 设计要点：
//! - 三个 axis 分别对应一种 `PermissionRequest`：
//!   * `ToolInvocation` — 按工具名 allow/deny
//!   * `PathAccess` — 按目录读/写 scope
//!   * `ShellCommand` — 按 shell 参数推断读/写性质
//! - 访问模式来自 `TaskPolicy.access_profile`。引擎本身无状态，便于跨线程复用。
//! - 引擎不直接处理 UI 呈现；它只输出 `Decision`（Allow / Deny / NeedsApproval），
//!   交给上层（SafetyGate / governance 服务 / UI）统一解释为受限拦截或放行。
//!
//! 与既有 magi-governance 的关系：governance 关注"风险打分 / 拦截记录"，
//! 即"已经判定 NeedsApproval 之后如何记录与呈现"；permissions 关注
//! "在调用前根据规则给出 Allow/Deny/NeedsApproval"。两者职责互不重叠。

use magi_core::AccessProfile;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
// --- PermissionRequest / Decision

/// 三个维度的调用请求。caller 在工具调用前构造，引擎据此判定。
#[derive(Clone, Debug)]
pub enum PermissionRequest<'a> {
    /// 工具调用：按工具名做 allow/deny 比对。`arguments` 用于在 shell_exec
    /// 这类工具上联动 ShellCommand 推断（commandRequest 由 caller 自行构造，
    /// 或调用 `PermissionEngine::shell_command_writes` 复用判定）。
    ToolInvocation {
        tool_name: &'a str,
        is_write_tool: bool,
    },
    /// 目录访问：caller 给出 absolute path，引擎据 read/write 意图比对 scope。
    PathAccess {
        absolute_path: &'a Path,
        kind: PathAccessKind,
    },
    /// shell 命令分类：caller 把 shell_exec 的 `arguments` 原文传进来，由引擎
    /// 结合模型声明与命令文本推断这是不是只读命令。
    ShellCommand { arguments_json: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathAccessKind {
    Read,
    Write,
}

/// 引擎判定结果。三态：
/// - Allow：放行
/// - Deny：拒绝，附带原因（用于上抛模型 / 写入 turn item）
/// - NeedsApproval：受限模式下需要由上层按风险策略拦截
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
    NeedsApproval { reason: String },
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::Deny { reason } | Self::NeedsApproval { reason } => Some(reason),
        }
    }
}
// --- PermissionPolicy（Task / Mission 维度的具体规则集）

/// 单条权限策略快照：直接由 `magi_core::TaskPolicy` 映射而来，引擎不持久化
/// 自己的规则，只在 decide 时按需读。
#[derive(Clone, Debug, Default)]
pub struct PermissionPolicy {
    pub allowed_tools: HashSet<String>,
    pub denied_tools: HashSet<String>,
    pub allowed_paths: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    /// "read_only" / "read_write" / 空字符串 ⇒ 默认（read_write）。
    pub command_mode: String,
}

impl PermissionPolicy {
    pub fn from_core_policy(policy: &magi_core::TaskPolicy) -> Self {
        Self {
            allowed_tools: policy.allowed_tools.iter().cloned().collect(),
            denied_tools: policy.denied_tools.iter().cloned().collect(),
            allowed_paths: policy.allowed_paths.iter().map(PathBuf::from).collect(),
            denied_paths: policy.denied_paths.iter().map(PathBuf::from).collect(),
            command_mode: policy.command_mode.clone(),
        }
    }

    fn is_read_only_command_mode(&self) -> bool {
        self.command_mode.eq_ignore_ascii_case("read_only")
    }
}
// --- PermissionEngine

/// 进程内单例：注入到 dispatch 入口。引擎本身无可变状态，可任意 clone。
#[derive(Clone, Debug, Default)]
pub struct PermissionEngine {
    read_only_tools: HashSet<&'static str>,
    restricted_auto_write_tools: HashSet<&'static str>,
}

impl PermissionEngine {
    pub fn register_read_only_tool(&mut self, name: &'static str) {
        self.read_only_tools.insert(name);
    }

    pub fn register_restricted_auto_write_tool(&mut self, name: &'static str) {
        self.restricted_auto_write_tools.insert(name);
    }

    pub fn is_read_only_tool(&self, name: &str) -> bool {
        self.read_only_tools.contains(name)
    }

    pub fn is_restricted_auto_write_tool(&self, name: &str) -> bool {
        self.restricted_auto_write_tools.contains(name)
    }

    /// 主判定入口。`policy` 一般来自当前 Task 的 policy snapshot。
    pub fn decide(
        &self,
        request: &PermissionRequest<'_>,
        policy: &PermissionPolicy,
        access_profile: AccessProfile,
    ) -> Decision {
        match request {
            PermissionRequest::ToolInvocation {
                tool_name,
                is_write_tool,
            } => self.decide_tool(tool_name, *is_write_tool, policy, access_profile),
            PermissionRequest::PathAccess {
                absolute_path,
                kind,
            } => self.decide_path(absolute_path, *kind, policy, access_profile),
            PermissionRequest::ShellCommand { arguments_json } => {
                self.decide_shell_command(arguments_json, policy, access_profile)
            }
        }
    }

    fn decide_tool(
        &self,
        tool_name: &str,
        is_write_tool: bool,
        policy: &PermissionPolicy,
        access_profile: AccessProfile,
    ) -> Decision {
        if policy.denied_tools.contains(tool_name) {
            return Decision::Deny {
                reason: format!("任务策略拒绝工具：{tool_name}"),
            };
        }
        if !policy.allowed_tools.is_empty() && !policy.allowed_tools.contains(tool_name) {
            return Decision::Deny {
                reason: format!("任务策略未授权工具：{tool_name}"),
            };
        }
        if (access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode())
            && is_write_tool
        {
            return Decision::Deny {
                reason: format!("只读任务不允许执行写入工具：{tool_name}"),
            };
        }
        match access_profile {
            AccessProfile::Restricted
                if is_write_tool && !self.is_restricted_auto_write_tool(tool_name) =>
            {
                Decision::NeedsApproval {
                    reason: format!("受限执行未自动授权写入工具：{tool_name}"),
                }
            }
            _ => Decision::Allow,
        }
    }

    fn decide_path(
        &self,
        absolute_path: &Path,
        kind: PathAccessKind,
        policy: &PermissionPolicy,
        access_profile: AccessProfile,
    ) -> Decision {
        if policy
            .denied_paths
            .iter()
            .any(|denied| path_is_within(absolute_path, denied))
        {
            return Decision::Deny {
                reason: format!("策略拒绝访问路径：{}", absolute_path.display()),
            };
        }
        if !policy.allowed_paths.is_empty()
            && !policy
                .allowed_paths
                .iter()
                .any(|allowed| path_is_within(absolute_path, allowed))
        {
            return Decision::Deny {
                reason: format!("策略未授权访问路径：{}", absolute_path.display()),
            };
        }
        if kind == PathAccessKind::Write {
            if access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode() {
                return Decision::Deny {
                    reason: format!("只读任务不允许写入路径：{}", absolute_path.display()),
                };
            }
        }
        Decision::Allow
    }

    fn decide_shell_command(
        &self,
        arguments_json: &str,
        policy: &PermissionPolicy,
        access_profile: AccessProfile,
    ) -> Decision {
        if !Self::shell_arguments_have_permission_relevant_operation(arguments_json) {
            return Decision::Allow;
        }
        let declares_read_only = Self::shell_arguments_declare_read_only(arguments_json);
        let is_read_only = Self::shell_arguments_request_read_only(arguments_json);
        if declares_read_only && !is_read_only {
            return Decision::Deny {
                reason: "shell_exec 声明为只读时，命令不能包含写入迹象".to_string(),
            };
        }
        if (access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode())
            && !is_read_only
        {
            return Decision::Deny {
                reason:
                    "只读任务中的 shell_exec 必须声明 access_mode=read_only，且命令不能包含写入迹象"
                        .to_string(),
            };
        }
        match access_profile {
            AccessProfile::Restricted if !is_read_only => Decision::NeedsApproval {
                reason: "受限执行不自动放行写类 shell 命令".to_string(),
            },
            _ => Decision::Allow,
        }
    }

    /// 检查 arguments JSON 是否表达了只读 shell。
    ///
    /// 只读判定不能只相信模型声明：`access_mode=read_only` 只是必要条件，命令文本中
    /// 出现重定向或常见写类命令时仍然视为写类 shell。
    pub fn shell_arguments_request_read_only(arguments_json: &str) -> bool {
        let Some(object) = serde_json::from_str::<serde_json::Value>(arguments_json)
            .ok()
            .and_then(|value| value.as_object().cloned())
        else {
            return false;
        };
        let action = json_string(&object, &["action", "operation", "op"])
            .map(|value| value.trim().to_ascii_lowercase());
        let has_terminal_id = json_has_any(&object, &["terminal_id", "terminalId", "id"]);
        let has_command = json_string(&object, &["command", "script", "line"])
            .is_some_and(|value| !value.trim().is_empty());

        match action.as_deref() {
            None if has_terminal_id && !has_command => return true,
            Some("read" | "poll" | "status" | "list" | "ls") => return true,
            Some("write" | "stdin" | "send" | "kill" | "stop" | "terminate" | "cancel") => {
                return false;
            }
            Some("run" | "exec" | "command") | None => {}
            Some(_) => return false,
        }

        if !json_string(&object, &["access_mode", "write_mode", "intent"]).is_some_and(|mode| {
            matches!(
                mode.trim().to_ascii_lowercase().as_str(),
                "read" | "read_only" | "readonly"
            )
        }) {
            return false;
        }
        let Some(command) = json_string(&object, &["command", "script", "line"]) else {
            return false;
        };
        !shell_command_has_write_indicator(&command)
    }

    fn shell_arguments_have_permission_relevant_operation(arguments_json: &str) -> bool {
        if arguments_json.trim().is_empty() {
            return false;
        }
        let Some(object) = serde_json::from_str::<serde_json::Value>(arguments_json)
            .ok()
            .and_then(|value| value.as_object().cloned())
        else {
            return true;
        };
        let action = json_string(&object, &["action", "operation", "op"])
            .map(|value| value.trim().to_ascii_lowercase());
        let has_terminal_id = json_has_any(&object, &["terminal_id", "terminalId", "id"]);
        let has_command = json_string(&object, &["command", "script", "line"])
            .is_some_and(|value| !value.trim().is_empty());

        match action.as_deref() {
            None => has_command || has_terminal_id,
            Some("run" | "exec" | "command") => has_command,
            Some(
                "read" | "poll" | "status" | "list" | "ls" | "write" | "stdin" | "send" | "kill"
                | "stop" | "terminate" | "cancel",
            ) => true,
            Some(_) => false,
        }
    }

    fn shell_arguments_declare_read_only(arguments_json: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(arguments_json)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .and_then(|object| json_string(&object, &["access_mode", "write_mode", "intent"]))
            .is_some_and(|mode| {
                matches!(
                    mode.trim().to_ascii_lowercase().as_str(),
                    "read" | "read_only" | "readonly"
                )
            })
    }

    /// caller 直接拿 list 用于 dedup 逻辑。
    pub fn read_only_tool_names(&self) -> Vec<&'static str> {
        self.read_only_tools.iter().copied().collect()
    }
}

fn json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

fn json_has_any(object: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| object.contains_key(*key))
}

fn shell_command_has_write_indicator(command: &str) -> bool {
    let tokens = shell_command_tokens(command);
    shell_command_has_unsafe_unquoted_output_redirection(command)
        || tokens
            .iter()
            .any(|token| shell_token_is_write_indicator(token))
        || shell_tokens_include_download_output_write(&tokens)
        || shell_tokens_include_mutating_git(&tokens)
}

fn shell_command_has_unsafe_unquoted_output_redirection(command: &str) -> bool {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            index += 1;
            continue;
        }
        match ch {
            '\'' if !double_quoted => single_quoted = !single_quoted,
            '"' if !single_quoted => double_quoted = !double_quoted,
            '>' if !single_quoted && !double_quoted => {
                if output_redirection_targets_safe_sink(&chars, index) {
                    index += 1;
                    continue;
                }
                return true;
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn output_redirection_targets_safe_sink(chars: &[char], redirect_index: usize) -> bool {
    let mut index = redirect_index + 1;
    if chars.get(index) == Some(&'>') || chars.get(index) == Some(&'|') {
        index += 1;
    }
    while chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
        index += 1;
    }
    if chars.get(index) == Some(&'&') {
        index += 1;
        let start = index;
        while chars
            .get(index)
            .is_some_and(|ch| ch.is_ascii_digit() || *ch == '-')
        {
            index += 1;
        }
        return index > start && redirection_word_boundary(chars.get(index));
    }

    let start = index;
    while chars
        .get(index)
        .is_some_and(|ch| !ch.is_whitespace() && !matches!(*ch, ';' | '|' | '&' | '(' | ')'))
    {
        index += 1;
    }
    if index == start {
        return false;
    }
    let target: String = chars[start..index].iter().collect();
    redirection_target_is_dev_null(&target)
}

fn redirection_word_boundary(ch: Option<&char>) -> bool {
    ch.is_none_or(|ch| ch.is_whitespace() || matches!(*ch, ';' | '|' | '&' | '(' | ')'))
}

fn redirection_target_is_dev_null(target: &str) -> bool {
    let trimmed = target.trim_matches(|ch| matches!(ch, '"' | '\''));
    trimmed == "/dev/null"
}

fn shell_command_tokens(command: &str) -> Vec<String> {
    command
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '(' | ')'))
        .filter_map(|part| {
            let token = part
                .trim_matches(|ch: char| {
                    matches!(ch, '"' | '\'' | '`' | '[' | ']' | '{' | '}' | ',')
                })
                .trim()
                .to_ascii_lowercase();
            (!token.is_empty()).then_some(token)
        })
        .collect()
}

fn shell_token_is_write_indicator(token: &str) -> bool {
    matches!(
        token,
        "rm" | "rmdir"
            | "mv"
            | "cp"
            | "mkdir"
            | "touch"
            | "ln"
            | "chmod"
            | "chown"
            | "chgrp"
            | "truncate"
            | "dd"
            | "tee"
            | "install"
            | "rsync"
            | "scp"
    )
}

fn shell_tokens_include_download_output_write(tokens: &[String]) -> bool {
    tokens
        .iter()
        .enumerate()
        .any(|(index, token)| match token.as_str() {
            "curl" => curl_tokens_write_output(&tokens[index + 1..]),
            "wget" => wget_tokens_write_output(&tokens[index + 1..]),
            _ => false,
        })
}

fn curl_tokens_write_output(tokens: &[String]) -> bool {
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if matches!(token, "-o" | "--output") {
            if tokens
                .get(index + 1)
                .is_some_and(|value| value.as_str() == "-")
            {
                index += 2;
                continue;
            }
            return true;
        }
        if token.starts_with("--output=") {
            return token != "--output=-";
        }
        if token.starts_with('-')
            && !token.starts_with("--")
            && token.len() > 2
            && token.contains('o')
            && token != "-o-"
        {
            return true;
        }
        if token.starts_with("-o") && token != "-o-" {
            return true;
        }
        if matches!(token, "--remote-name" | "--remote-name-all") {
            return true;
        }
        index += 1;
    }
    false
}

fn wget_tokens_write_output(tokens: &[String]) -> bool {
    let mut has_stdout_output = false;
    let mut has_spider = false;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        match token {
            "--spider" => {
                has_spider = true;
            }
            "-o" | "--output-document" => {
                if tokens
                    .get(index + 1)
                    .is_some_and(|value| value.as_str() == "-")
                {
                    has_stdout_output = true;
                    index += 2;
                    continue;
                }
                return true;
            }
            "-o-" | "--output-document=-" => {
                has_stdout_output = true;
            }
            value if value.starts_with("--output-document=") => {
                if value != "--output-document=-" {
                    return true;
                }
                has_stdout_output = true;
            }
            value if value.contains("o-") => {
                has_stdout_output = true;
            }
            _ => {}
        }
        index += 1;
    }
    !has_stdout_output && !has_spider
}

fn shell_tokens_include_mutating_git(tokens: &[String]) -> bool {
    for (index, token) in tokens.iter().enumerate() {
        if token != "git" {
            continue;
        }
        let Some(subcommand) = git_subcommand_after(&tokens[index + 1..]) else {
            continue;
        };
        if !git_subcommand_is_read_only(subcommand) {
            return true;
        }
    }
    false
}

fn git_subcommand_after(tokens: &[String]) -> Option<&str> {
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "-c" | "-C" | "--git-dir" | "--work-tree" => index += 2,
            token if token.starts_with("--git-dir=") || token.starts_with("--work-tree=") => {
                index += 1
            }
            token if token.starts_with('-') => index += 1,
            token => return Some(token),
        }
    }
    None
}

fn git_subcommand_is_read_only(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "status"
            | "diff"
            | "log"
            | "show"
            | "rev-parse"
            | "ls-files"
            | "grep"
            | "describe"
            | "merge-base"
            | "name-rev"
    )
}

fn path_is_within(target: &Path, root: &Path) -> bool {
    target.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with_test_tools() -> PermissionEngine {
        let mut engine = PermissionEngine::default();
        engine.register_read_only_tool("file_view");
        engine.register_restricted_auto_write_tool("file_edit");
        engine.register_restricted_auto_write_tool("file_write");
        engine.register_restricted_auto_write_tool("apply_patch");
        engine
    }

    fn policy_empty() -> PermissionPolicy {
        PermissionPolicy::default()
    }

    fn policy_read_only() -> PermissionPolicy {
        let mut p = PermissionPolicy::default();
        p.command_mode = "read_only".to_string();
        p
    }

    #[test]
    fn full_access_does_not_override_read_only_command_policy() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::FullAccess);
        assert!(decision.is_deny());
    }

    #[test]
    fn denied_tool_takes_precedence_over_allow_list() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.denied_tools.insert("file_edit".to_string());
        policy.allowed_tools.insert("file_edit".to_string());
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn allow_list_excludes_unlisted_tool() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.allowed_tools.insert("file_view".to_string());
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn read_only_policy_blocks_write_tool() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn read_only_profile_denies_write_tool() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::ReadOnly);
        assert!(decision.is_deny());
    }

    #[test]
    fn restricted_profile_passes_registered_write_tool_blocks_shell() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let edit_req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        assert_eq!(
            engine.decide(&edit_req, &policy, AccessProfile::Restricted),
            Decision::Allow
        );
        let canonical_edit_req = PermissionRequest::ToolInvocation {
            tool_name: "file_write",
            is_write_tool: true,
        };
        assert_eq!(
            engine.decide(&canonical_edit_req, &policy, AccessProfile::Restricted),
            Decision::Allow
        );
        let apply_patch_req = PermissionRequest::ToolInvocation {
            tool_name: "apply_patch",
            is_write_tool: true,
        };
        assert_eq!(
            engine.decide(&apply_patch_req, &policy, AccessProfile::Restricted),
            Decision::Allow
        );
        let shell_req = PermissionRequest::ToolInvocation {
            tool_name: "shell_exec",
            is_write_tool: true,
        };
        let decision = engine.decide(&shell_req, &policy, AccessProfile::Restricted);
        assert!(matches!(decision, Decision::NeedsApproval { .. }));
    }

    #[test]
    fn path_within_denied_root_is_rejected() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.denied_paths.push(PathBuf::from("/etc"));
        let path = PathBuf::from("/etc/passwd");
        let req = PermissionRequest::PathAccess {
            absolute_path: &path,
            kind: PathAccessKind::Read,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn path_outside_allow_list_is_rejected() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.allowed_paths.push(PathBuf::from("/work"));
        let path = PathBuf::from("/secret/file");
        let req = PermissionRequest::PathAccess {
            absolute_path: &path,
            kind: PathAccessKind::Read,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn shell_read_only_passes_in_read_only_policy() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let args = r#"{"command":"ls","access_mode":"read_only"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn shell_writes_blocked_in_read_only_policy() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let args = r#"{"command":"rm -rf /tmp/foo"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn shell_read_only_declaration_does_not_hide_redirection_write() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let args = r#"{"command":"printf hi > out.txt","access_mode":"read_only"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };

        assert!(!PermissionEngine::shell_arguments_request_read_only(args));
        assert!(
            engine
                .decide(&req, &policy, AccessProfile::ReadOnly)
                .is_deny()
        );
        assert!(
            engine
                .decide(&req, &policy, AccessProfile::Restricted)
                .is_deny()
        );
    }

    #[test]
    fn shell_read_only_declaration_allows_dev_null_redirection() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let args = r#"{"command":"if command -v rg >/dev/null 2>&1; then rg --files; fi","access_mode":"read_only"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };

        assert!(PermissionEngine::shell_arguments_request_read_only(args));
        assert_eq!(
            engine.decide(&req, &policy, AccessProfile::FullAccess),
            Decision::Allow
        );
        assert_eq!(
            engine.decide(&req, &policy, AccessProfile::Restricted),
            Decision::Allow
        );
    }

    #[test]
    fn shell_missing_command_is_left_to_tool_validation() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let req = PermissionRequest::ShellCommand {
            arguments_json: r#"{"command":"   "}"#,
        };

        assert_eq!(
            engine.decide(&req, &policy, AccessProfile::Restricted),
            Decision::Allow
        );
    }

    #[test]
    fn shell_read_only_declaration_rejects_common_write_commands() {
        let args = r#"{"command":"touch out.txt","access_mode":"read_only"}"#;

        assert!(!PermissionEngine::shell_arguments_request_read_only(args));
    }

    #[test]
    fn shell_read_only_declaration_rejects_downloads_that_write_files() {
        let curl_output = r#"{"command":"curl -L https://example.test/file -o out.bin","access_mode":"read_only"}"#;
        let curl_remote_name =
            r#"{"command":"curl -LO https://example.test/file","access_mode":"read_only"}"#;
        let wget_default =
            r#"{"command":"wget https://example.test/file","access_mode":"read_only"}"#;
        let wget_output =
            r#"{"command":"wget https://example.test/file -O out.bin","access_mode":"read_only"}"#;

        assert!(!PermissionEngine::shell_arguments_request_read_only(
            curl_output
        ));
        assert!(!PermissionEngine::shell_arguments_request_read_only(
            curl_remote_name
        ));
        assert!(!PermissionEngine::shell_arguments_request_read_only(
            wget_default
        ));
        assert!(!PermissionEngine::shell_arguments_request_read_only(
            wget_output
        ));
    }

    #[test]
    fn shell_read_only_declaration_allows_downloads_to_stdout() {
        let curl_stdout =
            r#"{"command":"curl -L https://example.test/file -o -","access_mode":"read_only"}"#;
        let wget_stdout =
            r#"{"command":"wget -qO- https://example.test/file","access_mode":"read_only"}"#;

        assert!(PermissionEngine::shell_arguments_request_read_only(
            curl_stdout
        ));
        assert!(PermissionEngine::shell_arguments_request_read_only(
            wget_stdout
        ));
    }

    #[test]
    fn shell_read_only_declaration_rejects_mutating_git_subcommands() {
        let checkout = r#"{"command":"git checkout main","access_mode":"read_only"}"#;
        let status =
            r#"{"command":"git -C /tmp/project status --short","access_mode":"read_only"}"#;

        assert!(!PermissionEngine::shell_arguments_request_read_only(
            checkout
        ));
        assert!(PermissionEngine::shell_arguments_request_read_only(status));
    }

    #[test]
    fn shell_read_only_allows_quoted_redirection_text() {
        let args = r#"{"command":"printf 'a > b'","access_mode":"read_only"}"#;

        assert!(PermissionEngine::shell_arguments_request_read_only(args));
    }

    #[test]
    fn registered_read_only_tools_recognised() {
        let engine = engine_with_test_tools();
        assert!(engine.is_read_only_tool("file_view"));
        assert!(!engine.is_read_only_tool("file_edit"));
    }

    #[test]
    fn full_access_allows_write_shell_without_permission_approval() {
        let engine = engine_with_test_tools();
        let policy = policy_empty();
        let args = r#"{"command":"printf hi > out.txt"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::FullAccess);
        assert_eq!(decision, Decision::Allow);
    }
}
