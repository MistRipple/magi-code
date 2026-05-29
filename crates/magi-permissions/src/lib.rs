//! 任务系统 — L7 Permissions：三维（工具 / 目录 / 命令）× 产品级访问模式
//! （read_only / restricted / full_access）权限引擎。
//!
//! 目标：把 read-only 判定、工具白名单、shell 命令写入识别、Task.policy
//! 中的 allow/deny 列表统一收敛到一个 `PermissionEngine`，
//! 让 Conversation/Task 在调用工具或访问路径前都经过同一份判定。
//!
//! 设计要点：
//! - 三个 axis 分别对应一种 `PermissionRequest`：
//!   * `ToolInvocation` — 按工具名 allow/deny
//!   * `PathAccess` — 按目录读/写 scope
//!   * `ShellCommand` — 按 shell 参数推断读/写性质
//! - 访问模式来自 `TaskPolicy.access_profile`。引擎本身无状态，便于跨线程复用。
//! - 引擎不直接处理用户审批弹窗；它只输出 `Decision`（Allow / Deny / NeedsApproval），
//!   交给上层（SafetyGate / governance 服务 / UI）决定怎么呈现。
//!
//! 与既有 magi-governance 的关系：governance 关注"审批流转 / 风险打分"，
//! 即"已经判定 NeedsApproval 之后由谁审批、怎么记录"；permissions 关注
//! "在调用前根据规则给出 Allow/Deny/NeedsApproval"。两者职责互不重叠。

use magi_core::AccessProfile;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// PermissionRequest / Decision
// ---------------------------------------------------------------------------

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
    /// 推断这是不是只读命令。
    ShellCommand { arguments_json: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathAccessKind {
    Read,
    Write,
}

/// 引擎判定结果。三态：
/// - Allow：放行，不需要审批
/// - Deny：拒绝，附带原因（用于上抛模型 / 写入 turn item）
/// - NeedsApproval：未被规则裁定，需要 governance / 用户介入
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

// ---------------------------------------------------------------------------
// PermissionPolicy（Task / Mission 维度的具体规则集）
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// PermissionEngine
// ---------------------------------------------------------------------------

/// 进程内单例：注入到 dispatch 入口。引擎本身无可变状态，可任意 clone。
#[derive(Clone, Debug, Default)]
pub struct PermissionEngine {
    read_only_tools: HashSet<&'static str>,
    edit_tools: HashSet<&'static str>,
}

impl PermissionEngine {
    /// 默认构造：注入内置的"已知只读"工具名 + "已知编辑"工具名。
    /// 这两份名单是 magi 内置工具语义的客观分类，可被 caller 通过 builder 扩展。
    pub fn with_builtin_defaults() -> Self {
        Self {
            read_only_tools: BUILTIN_READ_ONLY_TOOLS.iter().copied().collect(),
            edit_tools: BUILTIN_EDIT_TOOLS.iter().copied().collect(),
        }
    }

    pub fn register_read_only_tool(&mut self, name: &'static str) {
        self.read_only_tools.insert(name);
    }

    pub fn register_edit_tool(&mut self, name: &'static str) {
        self.edit_tools.insert(name);
    }

    pub fn is_read_only_tool(&self, name: &str) -> bool {
        self.read_only_tools.contains(name)
    }

    pub fn is_edit_tool(&self, name: &str) -> bool {
        self.edit_tools.contains(name)
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
            AccessProfile::Restricted if is_write_tool && !self.is_edit_tool(tool_name) => {
                Decision::NeedsApproval {
                    reason: format!("受限执行仅自动放行编辑类写入，{tool_name} 需审批"),
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
        let is_read_only = Self::shell_arguments_request_read_only(arguments_json);
        if (access_profile == AccessProfile::ReadOnly || policy.is_read_only_command_mode())
            && !is_read_only
        {
            return Decision::Deny {
                reason: "只读任务中的 shell_exec 必须显式声明 access_mode=read_only".to_string(),
            };
        }
        match access_profile {
            AccessProfile::Restricted if !is_read_only => Decision::NeedsApproval {
                reason: "受限执行不自动放行写类 shell 命令".to_string(),
            },
            _ => Decision::Allow,
        }
    }

    /// 检查 arguments JSON 顶层是否有 `access_mode` / `write_mode` 字段且取值在只读集合里。
    pub fn shell_arguments_request_read_only(arguments_json: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(arguments_json)
            .ok()
            .and_then(|value| {
                value
                    .as_object()
                    .and_then(|object| {
                        object
                            .get("access_mode")
                            .or_else(|| object.get("write_mode"))
                    })
                    .and_then(serde_json::Value::as_str)
                    .map(|m| {
                        matches!(
                            m.trim().to_ascii_lowercase().as_str(),
                            "read" | "read_only" | "readonly"
                        )
                    })
            })
            .unwrap_or(false)
    }

    /// caller 直接拿 list 用于 dedup 逻辑。
    pub fn read_only_tool_names(&self) -> Vec<&'static str> {
        self.read_only_tools.iter().copied().collect()
    }
}

fn path_is_within(target: &Path, root: &Path) -> bool {
    target.starts_with(root)
}

/// 内置只读工具名，供权限判定与 dedup 逻辑共享。
const BUILTIN_READ_ONLY_TOOLS: &[&str] = &[
    "file_read",
    "file_view",
    "view_image",
    "image_view",
    "search_text",
    "code_search_regex",
    "search_semantic",
    "code_search_semantic",
    "diff_preview",
    "web_search",
    "web_fetch",
    "diagram_render",
    "knowledge_query",
    "project_knowledge_query",
    "code_symbols",
    "tool_catalog",
    "tool_diagnostics",
    "process_inspect",
];

/// 编辑类写入工具：受限执行模式下自动放行的子集，shell 等其他写入工具
/// 不在此列。
const BUILTIN_EDIT_TOOLS: &[&str] = &[
    "file_write",
    "file_create",
    "file_patch",
    "file_edit",
    "file_insert",
    "apply_patch",
    "file_remove",
    "file_mkdir",
    "file_copy",
    "file_move",
];

#[cfg(test)]
mod tests {
    use super::*;

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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
        let policy = policy_empty();
        let req = PermissionRequest::ToolInvocation {
            tool_name: "file_edit",
            is_write_tool: true,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::ReadOnly);
        assert!(decision.is_deny());
    }

    #[test]
    fn restricted_profile_passes_edit_tool_blocks_shell() {
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
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
        let engine = PermissionEngine::with_builtin_defaults();
        let policy = policy_read_only();
        let args = r#"{"command":"rm -rf /tmp/foo"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::Restricted);
        assert!(decision.is_deny());
    }

    #[test]
    fn builtin_read_only_tools_recognised() {
        let engine = PermissionEngine::with_builtin_defaults();
        for tool in BUILTIN_READ_ONLY_TOOLS {
            assert!(engine.is_read_only_tool(tool), "{tool} should be read-only");
        }
    }

    #[test]
    fn full_access_allows_write_shell_without_permission_approval() {
        let engine = PermissionEngine::with_builtin_defaults();
        let policy = policy_empty();
        let args = r#"{"command":"printf hi > out.txt"}"#;
        let req = PermissionRequest::ShellCommand {
            arguments_json: args,
        };
        let decision = engine.decide(&req, &policy, AccessProfile::FullAccess);
        assert_eq!(decision, Decision::Allow);
    }
}
