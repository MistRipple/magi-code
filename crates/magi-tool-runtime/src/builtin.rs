use self::failure::{PathResolveError, invalid_input};
use crate::{
    BuiltinTool, BuiltinToolName, BuiltinToolSpec, ToolExecutionContext, ToolExecutionProgress,
    ToolRuntimeResources, apply_patch::execute_apply_patch, image_generate::execute_image_generate,
    tool_catalog::execute_tool_catalog, view_image::execute_view_image,
};
use magi_core::ToolFailure;
use magi_core::{ApprovalRequirement, ExecutionResultStatus, RiskLevel, ToolCallId};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod diagram;
pub(crate) mod failure;
mod file_transfer;
mod files;
pub(crate) mod fs_support;
mod knowledge;
mod process;
mod search;
mod shell;
mod web;

pub(crate) use process::{cancel_active_processes, cancel_all_active_processes};
#[cfg(test)]
pub(crate) use shell::read_child_pipe;

#[derive(Clone, Debug)]
pub(crate) struct NormalizedBuiltinTool {
    name: BuiltinToolName,
    risk_level: RiskLevel,
    approval_requirement: ApprovalRequirement,
}

impl NormalizedBuiltinTool {
    pub(crate) fn new(
        name: BuiltinToolName,
        risk_level: RiskLevel,
        approval_requirement: ApprovalRequirement,
    ) -> Self {
        Self {
            name,
            risk_level,
            approval_requirement,
        }
    }
}

impl BuiltinTool for NormalizedBuiltinTool {
    fn name(&self) -> &'static str {
        self.name.as_str()
    }

    fn execute(
        &self,
        tool_call_id: &ToolCallId,
        input: &str,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> String {
        match self.name {
            BuiltinToolName::FileRead => files::execute_file_read(input, context),
            BuiltinToolName::ViewImage => execute_view_image(input, context),
            BuiltinToolName::ImageGenerate => {
                execute_image_generate(tool_call_id, input, context, resources)
            }
            BuiltinToolName::FileWrite => files::execute_file_write(input, context),
            BuiltinToolName::FilePatch => files::execute_file_patch(input, context),
            BuiltinToolName::ApplyPatch => execute_apply_patch(input, context),
            BuiltinToolName::FileRemove => file_transfer::execute_file_remove(input, context),
            BuiltinToolName::FileMkdir => files::execute_file_mkdir(input, context),
            BuiltinToolName::FileCopy => file_transfer::execute_file_copy(input, context),
            BuiltinToolName::FileMove => file_transfer::execute_file_move(input, context),
            BuiltinToolName::SearchText => search::execute_search_text(input, context),
            BuiltinToolName::SearchSemantic => {
                knowledge::execute_search_semantic(input, context, resources)
            }
            BuiltinToolName::ShellExec => shell::execute_shell_exec(input, context, None),
            BuiltinToolName::ProcessLaunch => process::execute_process_launch(input, context),
            BuiltinToolName::ProcessRead => process::execute_process_read(input, context),
            BuiltinToolName::ProcessWrite => process::execute_process_write(input, context),
            BuiltinToolName::ProcessKill => process::execute_process_kill(input, context),
            BuiltinToolName::ProcessList => process::execute_process_list(context),
            BuiltinToolName::ProcessInspect => process::execute_process_inspect(input),
            BuiltinToolName::DiffPreview => files::execute_diff_preview(input, context),
            BuiltinToolName::WebSearch => web::execute_web_search(input),
            BuiltinToolName::WebFetch => web::execute_web_fetch(input),
            BuiltinToolName::BrowserNavigate
            | BuiltinToolName::BrowserSnapshot
            | BuiltinToolName::BrowserClick
            | BuiltinToolName::BrowserType
            | BuiltinToolName::BrowserPress
            | BuiltinToolName::BrowserScroll
            | BuiltinToolName::BrowserScreenshot
            | BuiltinToolName::BrowserTabs
            | BuiltinToolName::BrowserViewport
            | BuiltinToolName::BrowserWaitFor
            | BuiltinToolName::BrowserHover
            | BuiltinToolName::BrowserDrag
            | BuiltinToolName::BrowserFillForm
            | BuiltinToolName::BrowserDialog
            | BuiltinToolName::BrowserUploadFile
            | BuiltinToolName::BrowserClickAt
            | BuiltinToolName::BrowserEvaluate
            | BuiltinToolName::BrowserConsole
            | BuiltinToolName::BrowserNetwork
            | BuiltinToolName::BrowserEmulate
            | BuiltinToolName::BrowserPerformance
            | BuiltinToolName::BrowserLighthouse
            | BuiltinToolName::BrowserHeap
            | BuiltinToolName::BrowserThirdParty
            | BuiltinToolName::BrowserWebMcp
            | BuiltinToolName::BrowserPwa
            | BuiltinToolName::BrowserRead
            | BuiltinToolName::BrowserStorage
            | BuiltinToolName::BrowserDownload => {
                execute_browser_tool(tool_call_id, self.name, input, context, resources)
            }
            BuiltinToolName::DiagramRender => diagram::execute_diagram_render(input),
            BuiltinToolName::KnowledgeQuery => {
                knowledge::execute_knowledge_query(input, context, resources)
            }
            BuiltinToolName::KnowledgeGraphQuery => {
                knowledge::execute_knowledge_graph_query(input, context, resources)
            }
            BuiltinToolName::CodeSymbols => {
                knowledge::execute_code_symbols(input, context, resources)
            }
            BuiltinToolName::ToolCatalog => execute_tool_catalog(input, context, resources),
            BuiltinToolName::GitStatus
            | BuiltinToolName::GitBranchList
            | BuiltinToolName::GitBranchCreate
            | BuiltinToolName::GitBranchSwitch
            | BuiltinToolName::GitPull
            | BuiltinToolName::GitPush
            | BuiltinToolName::GitMergePreview
            | BuiltinToolName::GitMerge
            | BuiltinToolName::GitBranchDelete
            | BuiltinToolName::GitWorktreeList
            | BuiltinToolName::GitWorktreeCreate
            | BuiltinToolName::GitWorktreeRemove
            | BuiltinToolName::AgentApply => execute_git_tool(self.name, input, context, resources),
            BuiltinToolName::GetGoal
            | BuiltinToolName::CreateGoal
            | BuiltinToolName::UpdateGoal => execute_orchestration_only(self.name),
            BuiltinToolName::AgentSpawn
            | BuiltinToolName::AgentSend
            | BuiltinToolName::AgentCancel
            | BuiltinToolName::AgentWait
            | BuiltinToolName::ContextSearch
            | BuiltinToolName::ContextRead
            | BuiltinToolName::ContextRequest
            | BuiltinToolName::UpdatePlan
            | BuiltinToolName::MemoryWrite
            | BuiltinToolName::AskUserQuestion => execute_orchestration_only(self.name),
        }
    }

    fn execute_with_progress(
        &self,
        tool_call_id: &ToolCallId,
        input: &str,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
        on_progress: &(dyn Fn(ToolExecutionProgress) + Sync),
    ) -> String {
        if self.name == BuiltinToolName::ShellExec {
            shell::execute_shell_exec(input, context, Some((tool_call_id, on_progress)))
        } else {
            self.execute(tool_call_id, input, context, resources)
        }
    }

    fn spec(&self) -> BuiltinToolSpec {
        BuiltinToolSpec {
            name: self.name.as_str().to_string(),
            risk_level: self.risk_level,
            approval_requirement: self.approval_requirement,
        }
    }
}

fn execute_browser_tool(
    tool_call_id: &ToolCallId,
    tool: BuiltinToolName,
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let Some(executor) = resources.browser_tool_executor.as_ref() else {
        return serde_json::json!({
            "tool": tool.as_str(),
            "status": "failed",
            "error_code": "browser_host_unavailable",
            "recoverable": false,
            "requires_user_action": true,
            "error": "内置浏览器运行时不可用",
        })
        .to_string();
    };
    executor(tool_call_id, tool.as_str(), input, context).0
}

fn execute_git_tool(
    tool: BuiltinToolName,
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let Some(executor) = resources.git_tool_executor.as_ref() else {
        return ToolFailure::coded(
            tool.as_str(),
            "git_runtime_unavailable",
            "结构化 Git 运行时不可用",
        )
        .instruction("不要重试；改用 shell_exec 执行 git 命令，或告知用户 Git 运行时不可用。")
        .into_payload();
    };
    executor(tool.as_str(), input, context).0
}

/// 内置工具结果的执行状态：只认 payload `status` 字段里的规范标签。
/// 内置工具必须自己写出规范状态；缺失或不认识是工具的缺陷，按失败处理并记录，
/// 不能默认成成功。
pub(crate) fn execution_status_of(tool: &str, payload: &str) -> ExecutionResultStatus {
    let status = serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("status")?
                .as_str()
                .and_then(ExecutionResultStatus::from_wire_label)
        });
    status.unwrap_or_else(|| {
        tracing::error!(
            tool,
            "builtin tool result has no canonical status; treating as failed"
        );
        ExecutionResultStatus::Failed
    })
}

pub(crate) fn parse_json_object(input: &str) -> Option<serde_json::Map<String, Value>> {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
}

pub(crate) fn field_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

/// 必填的非空字符串参数；缺失或全是空白时返回可直接作为工具结果的 `invalid_input` 失败。
fn required_string_field(
    request: Option<&serde_json::Map<String, Value>>,
    key: &str,
    tool: &str,
    missing_message: &str,
) -> Result<String, String> {
    let value = request
        .and_then(|object| field_string(object, key))
        .unwrap_or_default()
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(invalid_input(tool, missing_message));
    }
    Ok(value)
}

fn field_usize(object: &serde_json::Map<String, Value>, key: &str) -> Option<usize> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn field_bool(object: &serde_json::Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

fn field_string_array(object: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn context_working_directory(context: &ToolExecutionContext) -> Result<PathBuf, PathResolveError> {
    context
        .working_directory
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or(PathResolveError::WorkingDirectoryUnavailable)
}

pub(crate) fn resolve_path_with_context(
    input: &str,
    context: &ToolExecutionContext,
) -> Result<PathBuf, PathResolveError> {
    let cwd = context_working_directory(context)?;
    let trimmed = input.trim();
    let text_path = Path::new(trimmed);
    if text_path
        .components()
        .all(|component| matches!(component, std::path::Component::CurDir))
    {
        return Ok(cwd);
    }
    magi_core::HostPath::resolve_native_input(trimmed, Some(&cwd), dirs::home_dir().as_deref())
        .map(magi_core::HostPath::into_path_buf)
        .map_err(PathResolveError::Invalid)
}

/// 协调器 / 长任务工具（agent_spawn 等）落到 BuiltinTool::execute 时
/// 必然是误调用——它们的语义需要 orchestration 层访问 task_store + spawn_graph +
/// conversation registry，远超 BuiltinTool trait 暴露的 ToolExecutionContext。
/// 真正的拦截点在 `crates/magi-conversation-runtime/src/tool_batch.rs::execute_task_tool_call`
/// （conversation runtime）。这里返回稳定错误码，避免把内部调用链或原始输入暴露到产品表面。
fn execute_orchestration_only(name: BuiltinToolName) -> String {
    ToolFailure::new(
        name.as_str(),
        "orchestration_required",
        "该协调工具需要由任务运行时处理，当前执行入口不可用",
    )
    .instruction("不要重试该调用；改用本轮可用的其他工具，或直接向用户说明。")
    .into_payload()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_tool_path_resolver_expands_home_directory() {
        let home = dirs::home_dir().expect("home directory should exist for test");
        let context = ToolExecutionContext {
            working_directory: Some(std::env::temp_dir()),
            ..ToolExecutionContext::default()
        };

        let resolved = resolve_path_with_context("~", &context).expect("home path should resolve");

        assert_eq!(resolved, home);
    }

    #[test]
    fn path_resolution_failures_are_structured_and_do_not_echo_the_requested_path() {
        let output = failure::path_resolution_failure(
            "file_read",
            "/private/workspace/secret.txt",
            &PathResolveError::WorkingDirectoryUnavailable,
        );
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "file_read_workspace_unavailable");
        assert!(payload["instruction"].as_str().is_some());
        assert!(!output.contains("/private/workspace/secret.txt"));
    }

    #[test]
    fn field_helpers_accept_only_the_schema_types() {
        let object = serde_json::json!({"count": "10", "flag": "true", "n": 3, "ok": true})
            .as_object()
            .cloned()
            .expect("object");

        assert_eq!(field_usize(&object, "count"), None);
        assert_eq!(field_bool(&object, "flag"), None);
        assert_eq!(field_usize(&object, "n"), Some(3));
        assert_eq!(field_bool(&object, "ok"), Some(true));
    }
}
