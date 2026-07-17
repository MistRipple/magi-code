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
        if kind == PathAccessKind::Write
            && (access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode())
        {
            return Decision::Deny {
                reason: format!("只读任务不允许写入路径：{}", absolute_path.display()),
            };
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
        let is_read_only = Self::shell_arguments_request_read_only(arguments_json);
        if !is_read_only
            && (access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode())
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

        if object
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return false;
        }

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
        let dialect = shell_dialect(
            json_string(&object, &["shell"])
                .as_deref()
                .unwrap_or_default(),
        );
        !shell_command_has_write_indicator(&command, dialect)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellDialect {
    Posix,
    Cmd,
    PowerShell,
}

impl ShellDialect {
    fn escape_char(self) -> char {
        match self {
            Self::Posix => '\\',
            Self::Cmd => '^',
            Self::PowerShell => '`',
        }
    }
}

fn shell_dialect(shell: &str) -> ShellDialect {
    let shell = shell.trim().to_ascii_lowercase();
    if shell.contains("powershell") || shell.ends_with("pwsh") || shell.ends_with("pwsh.exe") {
        ShellDialect::PowerShell
    } else if shell.contains("cmd") || (shell.is_empty() && cfg!(windows)) {
        ShellDialect::Cmd
    } else {
        ShellDialect::Posix
    }
}

fn shell_command_has_write_indicator(command: &str, dialect: ShellDialect) -> bool {
    shell_command_has_write_indicator_with_depth(command, dialect, 0)
}

fn shell_command_has_write_indicator_with_depth(
    command: &str,
    dialect: ShellDialect,
    depth: usize,
) -> bool {
    shell_command_has_unsafe_unquoted_output_redirection(command, dialect)
        || shell_command_includes_write_command(command, dialect, depth)
        || shell_command_includes_download_output_write(command, dialect)
        || shell_command_includes_mutating_git(command, dialect)
}

fn shell_command_has_unsafe_unquoted_output_redirection(
    command: &str,
    dialect: ShellDialect,
) -> bool {
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
                if output_redirection_targets_safe_sink(&chars, index, dialect) {
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

fn output_redirection_targets_safe_sink(
    chars: &[char],
    redirect_index: usize,
    dialect: ShellDialect,
) -> bool {
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
    redirection_target_is_null_device(&target, dialect)
}

fn redirection_word_boundary(ch: Option<&char>) -> bool {
    ch.is_none_or(|ch| ch.is_whitespace() || matches!(*ch, ';' | '|' | '&' | '(' | ')'))
}

fn redirection_target_is_null_device(target: &str, dialect: ShellDialect) -> bool {
    let trimmed = target.trim_matches(|ch| matches!(ch, '"' | '\''));
    match dialect {
        ShellDialect::Posix => trimmed == "/dev/null",
        ShellDialect::Cmd => trimmed.eq_ignore_ascii_case("NUL"),
        ShellDialect::PowerShell => {
            trimmed.eq_ignore_ascii_case("NUL") || trimmed.eq_ignore_ascii_case("$null")
        }
    }
}

fn shell_command_tokens(command: &str, dialect: ShellDialect) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == dialect.escape_char() && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"')
            || (character == '`' && dialect != ShellDialect::PowerShell)
        {
            quote = Some(character);
            continue;
        }
        if character.is_whitespace() || matches!(character, ';' | '|' | '&' | '(' | ')') {
            push_shell_command_token(&mut tokens, &mut token);
            continue;
        }
        token.push(character);
    }
    if escaped {
        token.push(dialect.escape_char());
    }
    push_shell_command_token(&mut tokens, &mut token);
    tokens
}

fn push_shell_command_token(tokens: &mut Vec<String>, token: &mut String) {
    let normalized = token
        .trim_matches(|character: char| matches!(character, '[' | ']' | '{' | '}' | ','))
        .trim()
        .to_ascii_lowercase();
    if !normalized.is_empty() {
        tokens.push(normalized);
    }
    token.clear();
}

fn shell_token_is_write_indicator(token: &str, dialect: ShellDialect) -> bool {
    let common = matches!(
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
    );
    common
        || match dialect {
            ShellDialect::Posix => false,
            ShellDialect::Cmd => matches!(
                token,
                "del" | "erase" | "copy" | "xcopy" | "robocopy" | "move" | "ren" | "rename"
            ),
            ShellDialect::PowerShell => matches!(
                token,
                "remove-item"
                    | "set-content"
                    | "add-content"
                    | "clear-content"
                    | "copy-item"
                    | "move-item"
                    | "rename-item"
                    | "new-item"
                    | "out-file"
            ),
        }
}

fn shell_command_includes_write_command(
    command: &str,
    dialect: ShellDialect,
    depth: usize,
) -> bool {
    shell_command_segments(command).into_iter().any(|segment| {
        let tokens = shell_command_tokens(&segment, dialect);
        let Some(command_index) = shell_command_token_index(&tokens) else {
            return false;
        };
        shell_invocation_has_write_indicator(&tokens[command_index..], dialect, depth)
    })
}

fn shell_invocation_has_write_indicator(
    tokens: &[String],
    dialect: ShellDialect,
    depth: usize,
) -> bool {
    let Some(command) = tokens.first().map(|token| shell_command_basename(token)) else {
        return false;
    };
    let arguments = &tokens[1..];
    if shell_token_is_write_indicator(command, dialect) {
        return true;
    }
    match command {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            shell_inline_script(arguments, &["-c", "--command"]).is_some_and(|script| {
                depth >= 4
                    || shell_command_has_write_indicator_with_depth(
                        script,
                        ShellDialect::Posix,
                        depth + 1,
                    )
            })
        }
        "cmd" | "cmd.exe" => shell_inline_script(arguments, &["/c", "/k"]).is_some_and(|script| {
            depth >= 4
                || shell_command_has_write_indicator_with_depth(
                    script,
                    ShellDialect::Cmd,
                    depth + 1,
                )
        }),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => {
            if arguments.iter().any(|argument| {
                matches!(
                    argument.as_str(),
                    "-encodedcommand" | "-enc" | "-file" | "-f"
                )
            }) {
                return true;
            }
            shell_inline_script(arguments, &["-command", "-c"]).is_some_and(|script| {
                depth >= 4
                    || shell_command_has_write_indicator_with_depth(
                        script,
                        ShellDialect::PowerShell,
                        depth + 1,
                    )
            })
        }
        "xargs" => xargs_invocation_has_write_indicator(arguments, dialect, depth),
        "find" => arguments.iter().any(|argument| {
            matches!(
                argument.as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
            )
        }),
        "sed" => arguments.iter().any(|argument| {
            argument == "-i" || argument.starts_with("-i") || argument.starts_with("--in-place")
        }),
        "perl" => arguments.iter().any(|argument| {
            argument == "-i"
                || argument.starts_with("-i")
                || matches!(argument.as_str(), "-e" | "-E")
        }),
        "node" | "node.exe" | "python" | "python3" | "python.exe" | "py" | "ruby" | "ruby.exe" => {
            !command_invocation_is_information_only(arguments)
        }
        "cargo" | "cargo.exe" => cargo_invocation_has_write_indicator(arguments),
        "make" | "gmake" | "ninja" | "cmake" | "rustc" | "gcc" | "clang" | "cl" | "cl.exe" => {
            !command_invocation_is_information_only(arguments)
        }
        _ => false,
    }
}

fn shell_inline_script<'a>(arguments: &'a [String], flags: &[&str]) -> Option<&'a str> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        flags
            .iter()
            .any(|flag| argument.eq_ignore_ascii_case(flag))
            .then(|| arguments.get(index + 1).map(String::as_str))
            .flatten()
    })
}

fn xargs_invocation_has_write_indicator(
    arguments: &[String],
    dialect: ShellDialect,
    depth: usize,
) -> bool {
    let mut index = 0usize;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if matches!(
            argument,
            "-a" | "--arg-file"
                | "-e"
                | "-E"
                | "-i"
                | "-I"
                | "-l"
                | "-L"
                | "-n"
                | "--max-args"
                | "-p"
                | "-P"
                | "--max-procs"
                | "-s"
                | "--max-chars"
        ) {
            index += 2;
            continue;
        }
        if argument.starts_with('-') {
            index += 1;
            continue;
        }
        return shell_invocation_has_write_indicator(&arguments[index..], dialect, depth + 1);
    }
    false
}

fn command_invocation_is_information_only(arguments: &[String]) -> bool {
    arguments.is_empty()
        || arguments.iter().any(|argument| {
            matches!(
                argument.as_str(),
                "--version" | "-v" | "-V" | "--help" | "-h" | "/?"
            )
        })
}

fn cargo_invocation_has_write_indicator(arguments: &[String]) -> bool {
    let Some(subcommand) = arguments
        .iter()
        .map(String::as_str)
        .find(|argument| !argument.starts_with('-'))
    else {
        return false;
    };
    !matches!(
        subcommand,
        "help" | "locate-project" | "metadata" | "pkgid" | "search" | "tree" | "version"
    )
}

fn shell_command_includes_download_output_write(command: &str, dialect: ShellDialect) -> bool {
    shell_command_segments(command).into_iter().any(|segment| {
        let tokens = shell_command_tokens(&segment, dialect);
        let Some(command_index) = shell_command_token_index(&tokens) else {
            return false;
        };
        match shell_command_basename(&tokens[command_index]) {
            "curl" => curl_tokens_write_output(&tokens[command_index + 1..]),
            "wget" => wget_tokens_write_output(&tokens[command_index + 1..]),
            _ => false,
        }
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

fn shell_command_includes_mutating_git(command: &str, dialect: ShellDialect) -> bool {
    shell_command_segments(command).into_iter().any(|segment| {
        let tokens = shell_command_tokens(&segment, dialect);
        let Some(command_index) = shell_command_token_index(&tokens) else {
            return false;
        };
        if shell_command_basename(&tokens[command_index]) != "git" {
            return false;
        }
        !git_invocation_is_read_only(&tokens[command_index + 1..])
    })
}

fn shell_command_basename(command: &str) -> &str {
    command.rsplit(['/', '\\']).next().unwrap_or(command)
}

fn shell_command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut segment = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            segment.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            segment.push(ch);
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            segment.push(ch);
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            segment.push(ch);
            continue;
        }
        if matches!(ch, ';' | '|' | '&' | '\n') {
            if !segment.trim().is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
            continue;
        }
        segment.push(ch);
    }
    if !segment.trim().is_empty() {
        segments.push(segment);
    }
    segments
}

fn shell_command_token_index(tokens: &[String]) -> Option<usize> {
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "if" | "then" | "do" | "else" | "elif" | "time" | "command" | "builtin" | "exec"
            | "sudo" | "env" => index += 1,
            token if token.contains('=') && !token.starts_with('/') && !token.starts_with("./") => {
                index += 1
            }
            _ => return Some(index),
        }
    }
    None
}

fn git_subcommand_and_arguments(tokens: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "-c" | "-C" | "--git-dir" | "--work-tree" => index += 2,
            token if token.starts_with("--git-dir=") || token.starts_with("--work-tree=") => {
                index += 1
            }
            token if token.starts_with('-') => index += 1,
            token => return Some((token, &tokens[index + 1..])),
        }
    }
    None
}

fn git_invocation_is_read_only(tokens: &[String]) -> bool {
    let Some((subcommand, arguments)) = git_subcommand_and_arguments(tokens) else {
        return true;
    };
    if matches!(
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
            | "rev-list"
            | "shortlog"
            | "reflog"
            | "ls-tree"
            | "cat-file"
            | "for-each-ref"
            | "check-ignore"
            | "count-objects"
    ) {
        return true;
    }
    match subcommand {
        "remote" => git_remote_invocation_is_read_only(arguments),
        "worktree" => arguments.first().is_some_and(|argument| argument == "list"),
        "stash" => arguments
            .first()
            .is_some_and(|argument| matches!(argument.as_str(), "list" | "show")),
        "submodule" => arguments
            .first()
            .is_some_and(|argument| matches!(argument.as_str(), "status" | "summary")),
        "config" => git_config_invocation_is_read_only(arguments),
        _ => false,
    }
}

fn git_remote_invocation_is_read_only(arguments: &[String]) -> bool {
    let first_meaningful = arguments.iter().map(String::as_str).find(|argument| {
        !matches!(*argument, "-v" | "--verbose") && !shell_token_is_redirection(argument)
    });
    match first_meaningful {
        None => true,
        Some("get-url" | "show") => true,
        Some(_) => false,
    }
}

fn shell_token_is_redirection(token: &str) -> bool {
    token
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .starts_with(['>', '<'])
}

fn git_config_invocation_is_read_only(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--get"
                | "--get-all"
                | "--get-regexp"
                | "--get-urlmatch"
                | "--list"
                | "-l"
                | "--show-origin"
                | "--show-scope"
        )
    }) && !arguments.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--add"
                | "--replace-all"
                | "--unset"
                | "--unset-all"
                | "--rename-section"
                | "--remove-section"
        )
    })
}

fn path_is_within(target: &Path, root: &Path) -> bool {
    target.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with_test_tools() -> PermissionEngine {
        let mut engine = PermissionEngine::default();
        engine.register_read_only_tool("file_read");
        engine.register_restricted_auto_write_tool("file_patch");
        engine.register_restricted_auto_write_tool("file_write");
        engine.register_restricted_auto_write_tool("apply_patch");
        engine
    }

    fn policy_empty() -> PermissionPolicy {
        PermissionPolicy::default()
    }

    fn policy_read_only() -> PermissionPolicy {
        PermissionPolicy {
            command_mode: "read_only".to_string(),
            ..PermissionPolicy::default()
        }
    }

    #[test]
    fn full_access_does_not_override_read_only_command_policy() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_patch",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::FullAccess);
        assert!(decision.is_deny());
    }

    #[test]
    fn denied_tool_takes_precedence_over_allow_list() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.denied_tools.insert("file_patch".to_string());
        policy.allowed_tools.insert("file_patch".to_string());
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_patch",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn allow_list_excludes_unlisted_tool() {
        let engine = engine_with_test_tools();
        let mut policy = policy_empty();
        policy.allowed_tools.insert("file_read".to_string());
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_patch",
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
            tool_name: "file_patch",
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
            tool_name: "file_patch",
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
            tool_name: "file_patch",
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
    fn shell_read_only_declaration_is_reclassified_by_product_access_profile() {
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
        assert!(matches!(
            engine.decide(&req, &policy, AccessProfile::Restricted),
            Decision::NeedsApproval { .. }
        ));
        assert_eq!(
            engine.decide(&req, &policy, AccessProfile::FullAccess),
            Decision::Allow
        );
    }

    #[test]
    fn read_only_command_policy_still_rejects_misdeclared_write_shell_in_full_access() {
        let engine = engine_with_test_tools();
        let policy = policy_read_only();
        let args = r#"{"command":"printf hi > out.txt","access_mode":"read_only"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };

        assert!(
            engine
                .decide(&req, &policy, AccessProfile::FullAccess)
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
    fn shell_read_only_declaration_allows_windows_null_device() {
        let args =
            r#"{"shell":"cmd.exe","command":"git status >NUL 2>&1","access_mode":"read_only"}"#;
        assert!(PermissionEngine::shell_arguments_request_read_only(args));
    }

    #[test]
    fn shell_read_only_declaration_rejects_cmd_write_commands() {
        for command in [
            "del notes.txt",
            "copy source.txt target.txt",
            "move source.txt target.txt",
            "mkdir output",
        ] {
            let args = serde_json::json!({
                "shell": "cmd.exe",
                "command": command,
                "access_mode": "read_only"
            })
            .to_string();
            assert!(
                !PermissionEngine::shell_arguments_request_read_only(&args),
                "cmd write command must be rejected: {command}"
            );
        }
    }

    #[test]
    fn shell_read_only_declaration_rejects_powershell_write_commands() {
        for command in [
            "Remove-Item notes.txt",
            "Set-Content notes.txt value",
            "Copy-Item source.txt target.txt",
            "Move-Item source.txt target.txt",
            "New-Item output -ItemType Directory",
        ] {
            let args = serde_json::json!({
                "shell": "powershell.exe",
                "command": command,
                "access_mode": "read_only"
            })
            .to_string();
            assert!(
                !PermissionEngine::shell_arguments_request_read_only(&args),
                "PowerShell write command must be rejected: {command}"
            );
        }
    }

    #[test]
    fn shell_read_only_rejects_indirect_and_ambiguous_side_effect_commands() {
        for (shell, command) in [
            ("/bin/zsh", "bash -c 'touch nested.txt'"),
            ("/bin/zsh", "printf nested | xargs touch"),
            ("/bin/zsh", "find . -name '*.tmp' -delete"),
            ("/bin/zsh", "sed -i.bak 's/a/b/' file.txt"),
            (
                "/bin/zsh",
                "node -e 'require(\"fs\").writeFileSync(\"out\", \"x\")'",
            ),
            ("/bin/zsh", "cargo test"),
            ("cmd.exe", "cmd /C del nested.txt"),
            (
                "powershell.exe",
                "powershell -Command Set-Content nested.txt value",
            ),
        ] {
            let args = serde_json::json!({
                "shell": shell,
                "command": command,
                "access_mode": "read_only"
            })
            .to_string();
            assert!(
                !PermissionEngine::shell_arguments_request_read_only(&args),
                "indirect or ambiguous side effect must not stay read-only: {shell}: {command}"
            );
        }
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
    fn shell_background_execution_is_never_classified_as_read_only() {
        let args = r#"{"command":"printf background","access_mode":"read_only","background":true}"#;

        assert!(!PermissionEngine::shell_arguments_request_read_only(args));
        assert!(matches!(
            engine_with_test_tools().decide(
                &PermissionRequest::ShellCommand {
                    arguments_json: args,
                },
                &PermissionPolicy::default(),
                AccessProfile::Restricted,
            ),
            Decision::NeedsApproval { .. }
        ));
    }

    #[test]
    fn shell_permission_matrix_keeps_product_modes_stable() {
        let engine = engine_with_test_tools();
        let unrestricted_policy = PermissionPolicy::default();
        let read_only_policy = policy_read_only();
        let cases = [
            (
                "declared read",
                r#"{"command":"printf hello","access_mode":"read_only"}"#,
                true,
            ),
            (
                "misdeclared write command",
                r#"{"command":"touch output.txt","access_mode":"read_only"}"#,
                false,
            ),
            (
                "misdeclared redirection",
                r#"{"command":"printf hello > output.txt","access_mode":"read_only"}"#,
                false,
            ),
            (
                "background process",
                r#"{"command":"printf hello","access_mode":"read_only","background":true}"#,
                false,
            ),
            (
                "declared write",
                r#"{"command":"printf hello","access_mode":"maybe_write"}"#,
                false,
            ),
        ];

        for (label, arguments, classified_read_only) in cases {
            assert_eq!(
                PermissionEngine::shell_arguments_request_read_only(arguments),
                classified_read_only,
                "shell classification mismatch: {label}"
            );
            let request = PermissionRequest::ShellCommand {
                arguments_json: arguments,
            };
            assert_eq!(
                engine.decide(&request, &unrestricted_policy, AccessProfile::FullAccess),
                Decision::Allow,
                "full access must not be downgraded by shell declaration: {label}"
            );
            let restricted =
                engine.decide(&request, &unrestricted_policy, AccessProfile::Restricted);
            assert_eq!(
                matches!(restricted, Decision::Allow),
                classified_read_only,
                "restricted classification mismatch: {label}"
            );
            let read_only = engine.decide(&request, &unrestricted_policy, AccessProfile::ReadOnly);
            assert_eq!(
                matches!(read_only, Decision::Allow),
                classified_read_only,
                "read-only profile mismatch: {label}"
            );
            let constrained = engine.decide(&request, &read_only_policy, AccessProfile::FullAccess);
            assert_eq!(
                matches!(constrained, Decision::Allow),
                classified_read_only,
                "task read-only constraint mismatch: {label}"
            );
        }
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
    fn shell_read_only_allows_compound_repository_inspection() {
        let args = serde_json::json!({
            "access_mode": "read_only",
            "command": "cd /Users/xie/code/magi && {\n  echo \"=== ROOT LISTING ===\";\n  ls -la;\n  echo;\n  echo \"=== GIT STATE ===\";\n  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then\n    git rev-parse --is-inside-work-tree;\n    git remote -v 2>/dev/null | head -5;\n    git status --short | head -50;\n    echo \"--- recent commits ---\";\n    git log --oneline -15 2>/dev/null || echo \"(no commits)\";\n  else\n    echo \"NOT_GIT_WORKTREE\";\n  fi\n  echo;\n  echo \"=== FILE COUNT BY TYPE (top-level dirs only) ===\";\n  for d in */; do printf \"%s \" \"$d\"; find \"$d\" -type f 2>/dev/null | wc -l; done | head -40;\n} 2>&1 | head -200"
        })
        .to_string();

        assert!(PermissionEngine::shell_arguments_request_read_only(&args));
    }

    #[test]
    fn shell_read_only_ignores_write_command_names_inside_search_patterns() {
        let args = serde_json::json!({
            "access_mode": "read_only",
            "command": "grep -rn \"Router::\\|layer(\\|middleware\\|DefaultBodyLimit\\|Cors\\|serve\\|bind\\|0.0.0.0\\|127.0.0.1\\|tunnel_token\\|sessions.json\\|write_all\\|atomic\" crates/magi-api crates/magi-daemon apps --include='*.rs' 2>/dev/null | head -80; echo '==='; grep -rn \"max_tokens\\|temperature\\|context_window\\|token_limit\\|compress\\|truncate\" crates/magi-context-runtime crates/magi-conversation-runtime --include='*.rs' 2>/dev/null | head -40; echo '==='; wc -c .magi/sessions.json 2>/dev/null; ls -la .magi/snapshots 2>/dev/null | head"
        })
        .to_string();

        assert!(PermissionEngine::shell_arguments_request_read_only(&args));
        assert_eq!(
            engine_with_test_tools().decide(
                &PermissionRequest::ShellCommand {
                    arguments_json: &args,
                },
                &PermissionPolicy::default(),
                AccessProfile::FullAccess,
            ),
            Decision::Allow
        );
    }

    #[test]
    fn shell_read_only_distinguishes_remote_queries_from_remote_mutations() {
        for command in [
            "git remote",
            "git remote -v",
            "git remote get-url origin",
            "git remote show origin",
        ] {
            let args = serde_json::json!({
                "access_mode": "read_only",
                "command": command,
            })
            .to_string();
            assert!(
                PermissionEngine::shell_arguments_request_read_only(&args),
                "remote query should remain read-only: {command}"
            );
        }

        for command in [
            "git remote add origin https://example.test/repo.git",
            "git remote -v add origin https://example.test/repo.git",
            "git remote remove origin",
            "git remote rename origin upstream",
            "git remote set-url origin https://example.test/repo.git",
            "git remote prune origin",
            "git remote update",
        ] {
            let args = serde_json::json!({
                "access_mode": "read_only",
                "command": command,
            })
            .to_string();
            assert!(
                !PermissionEngine::shell_arguments_request_read_only(&args),
                "remote mutation must not be classified as read-only: {command}"
            );
        }
    }

    #[test]
    fn registered_read_only_tools_recognised() {
        let engine = engine_with_test_tools();
        assert!(engine.is_read_only_tool("file_read"));
        assert!(!engine.is_read_only_tool("file_patch"));
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
