//! 工作流任务（`TaskKind::LocalWorkflow`）的宿主执行。
//!
//! 工作流核心只返回类型化动作；这里负责逐步校验、在 TaskStore 中持久化动作意图和真实
//! 结果、经原生执行服务执行工具与子任务，并把“等待用户”“等待子任务”表达为
//! `TaskOutcome::Yielded`。权限、运行保护、取消和终态仍归原生服务：本模块不提交终态，只
//! 返回完成尝试，由统一完成门裁决。

use super::{LlmTaskDispatcher, TaskLlmInvocationInput};
use crate::{
    task_runner_bridge::TaskOutcome, tool_batch::execute_task_tool_call_batch,
    tool_execution_ledger::ToolExecutionLedger,
};
use magi_bridge_client::{
    ChatToolCall, ChatToolDefinition, ChatToolFunction, ModelBridgeClient, ModelInvocationRequest,
};
use magi_core::{
    ExecutionResultStatus, LeaseId, SessionId, Task, TaskId, ThreadId, WorkerId, WorkspaceId,
};
use magi_orchestrator::{
    ExecutionContextSummary,
    task_store::{TaskStore, WorkflowCheckpointRef},
};
use magi_plugin_system::{
    RunCancellation, WorkflowCoreInvocationSpec,
    workflow::{WorkflowAction, WorkflowCore, WorkflowDecision, WorkflowInput},
};
use magi_settings_store::SettingsStore;
use magi_tool_runtime::ToolRegistry;
use serde_json::{Value, json};
use std::{cell::RefCell, path::PathBuf, sync::Arc};

/// 一次工作流执行的步数上限，防止核心无限循环占用租约。
const MAX_WORKFLOW_STEPS: usize = 32;

fn workflow_child_task_name(action_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = magi_core::hex_lower(Sha256::digest(action_id.as_bytes()));
    format!("workflow_{}", &digest[..32])
}

struct WorkflowSpawnFailure {
    message: String,
    no_child_created: bool,
}

/// 工作流执行用到的调用上下文，由通用任务调用输入裁剪而来。
pub(super) struct WorkflowInvocation<'a> {
    task: &'a Task,
    use_tools: bool,
    lease_id: &'a LeaseId,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    execution_root: Option<&'a PathBuf>,
    worker_id: Option<&'a WorkerId>,
    is_sidechain: bool,
    execution_role_id: Option<&'a str>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
    thread_id: &'a ThreadId,
    usage_binding: &'a crate::usage_recording::ModelUsageBinding,
}

impl<'a> From<&TaskLlmInvocationInput<'a>> for WorkflowInvocation<'a> {
    fn from(input: &TaskLlmInvocationInput<'a>) -> Self {
        Self {
            task: input.task,
            use_tools: input.use_tools,
            lease_id: input.lease_id,
            session_id: input.session_id,
            workspace_id: input.workspace_id,
            execution_root: input.execution_root,
            worker_id: input.worker_id,
            is_sidechain: input.is_sidechain,
            execution_role_id: input.execution_role_id,
            execution_settings_snapshot: input.execution_settings_snapshot.clone(),
            thread_id: input.thread_id,
            usage_binding: input.usage_binding,
        }
    }
}

/// 工作流工具动作的执行环境：只借用调用方已解析的会话、工作区与执行根。
struct WorkflowToolScope<'a> {
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    execution_root: Option<&'a PathBuf>,
    worker_id: Option<&'a WorkerId>,
    lease_id: &'a LeaseId,
    is_sidechain: bool,
    prepared: RefCell<Option<crate::conversation_loop::PreparedWorkspaceExecution>>,
}

/// 工作流检查点的唯一写入口：固定任务、租约与工作流身份，调用点只传变化的部分。
/// 所有写入都经过 TaskStore 的租约校验，租约失效时写入失败。
struct WorkflowCheckpoint<'a> {
    store: &'a TaskStore,
    task: &'a Task,
    lease_id: &'a LeaseId,
    workflow_id: &'a str,
}

impl WorkflowCheckpoint<'_> {
    fn reference<'r>(
        &'r self,
        stage: &'r str,
        checkpoint_version: u32,
        checkpoint: &'r Value,
    ) -> WorkflowCheckpointRef<'r> {
        WorkflowCheckpointRef {
            task_id: &self.task.task_id,
            root_task_id: &self.task.root_task_id,
            lease_id: self.lease_id,
            workflow_id: self.workflow_id,
            stage,
            checkpoint_version,
            checkpoint,
        }
    }

    /// 记录动作意图（`pending_action`）或真实结果（`last_result`）。`label` 描述失败位置。
    fn record(
        &self,
        state: &WorkflowRunState,
        checkpoint_version: u32,
        pending_action: Option<Value>,
        last_result: Option<Value>,
        label: &str,
    ) -> Result<(), String> {
        self.store
            .update_workflow_state(
                self.reference(&state.stage, checkpoint_version, &state.checkpoint),
                pending_action,
                last_result,
            )
            .map_err(|error| format!("{label}：{error}"))
    }

    fn wait_for_user(
        &self,
        state: &WorkflowRunState,
        checkpoint_version: u32,
        action_id: String,
        question: String,
    ) -> Result<TaskOutcome, String> {
        match self.store.requeue_workflow_for_user(
            self.reference(&state.stage, checkpoint_version, &state.checkpoint),
            action_id,
            question,
        ) {
            Ok(true) => Ok(TaskOutcome::Yielded),
            Ok(false) => Err("工作流租约已失效，无法等待用户".into()),
            Err(error) => Err(format!("工作流等待状态持久化失败：{error}")),
        }
    }

    fn wait_for_child(
        &self,
        state: &WorkflowRunState,
        checkpoint_version: u32,
        action_id: String,
        child_task_id: TaskId,
    ) -> Result<TaskOutcome, String> {
        match self.store.requeue_workflow_for_child(
            self.reference(&state.stage, checkpoint_version, &state.checkpoint),
            action_id,
            child_task_id,
        ) {
            Ok(true) => Ok(TaskOutcome::Yielded),
            Ok(false) => Err("工作流租约已失效，无法等待子任务".into()),
            Err(error) => Err(format!("工作流等待子任务失败：{error}")),
        }
    }
}

/// 一次执行内随动作推进的运行态。启动时从持久化的阶段、检查点和上次结果恢复。
struct WorkflowRunState {
    stage: String,
    checkpoint: Value,
    user_input: String,
    model_result: Option<Value>,
    tool_result: Option<Value>,
    context_summary: Value,
}

impl WorkflowRunState {
    fn restore(task: &Task, stage: &str, checkpoint: &Value, last_result: Option<&Value>) -> Self {
        // 上次结果按类型回灌给核心：模型结果、用户回答、工具或子任务回执。
        let value_of = |kinds: &[&str]| -> Option<Value> {
            let result = last_result?;
            let kind = result.get("kind").and_then(Value::as_str)?;
            kinds
                .contains(&kind)
                .then(|| result.get("value").cloned().unwrap_or(Value::Null))
        };
        Self {
            stage: stage.to_string(),
            checkpoint: checkpoint.clone(),
            user_input: value_of(&["user_input"])
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| task.goal.clone()),
            model_result: value_of(&["model"]),
            tool_result: value_of(&["tool", "child_task"]),
            context_summary: json!({}),
        }
    }
}

impl LlmTaskDispatcher {
    /// 执行一段工作流，返回本次调用的结果。工作流只会以 `Completed`（完成尝试）、
    /// `Yielded`（等待用户或子任务，任务已回到 Pending）或 `Failed` 结束。
    pub(super) fn invoke_workflow_once(
        &self,
        invocation: WorkflowInvocation<'_>,
    ) -> (TaskOutcome, Option<ExecutionContextSummary>) {
        let outcome = self
            .run_workflow(&invocation)
            .unwrap_or_else(|error| TaskOutcome::Failed { error });
        (outcome, None)
    }

    fn run_workflow(&self, invocation: &WorkflowInvocation<'_>) -> Result<TaskOutcome, String> {
        let WorkflowInvocation {
            task,
            use_tools,
            lease_id,
            session_id,
            workspace_id,
            execution_root,
            worker_id,
            is_sidechain,
            execution_role_id,
            execution_settings_snapshot,
            thread_id,
            usage_binding,
        } = invocation;
        let magi_core::TaskRuntimePayload::Workflow {
            workflow_id,
            package_digest,
            checkpoint_version,
            stage,
            checkpoint,
            pending_action,
            last_result,
        } = &task.runtime_payload
        else {
            return Err("LocalWorkflow 缺少持久化工作流核心身份".into());
        };
        let package_digest = package_digest
            .as_ref()
            .filter(|digest| !digest.trim().is_empty())
            .ok_or("原工作流缺少固定包摘要，不能自动恢复或使用当前插件版本")?;
        let core = self
            .build_workflow_core(WorkflowCoreInvocationSpec {
                session_id: session_id.to_string(),
                project_id: workspace_id
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                run_id: task.task_id.to_string(),
                workflow_id: workflow_id.clone(),
                package_digest: package_digest.clone(),
                checkpoint_version: *checkpoint_version,
                config: checkpoint.clone(),
            })
            .map_err(|error| format!("工作流核心不可用：{error}"))?;
        let store = self.pipeline.execution_runtime.task_store();
        let execution_guard = crate::task_execution_guard::TaskExecutionGuard::new(
            store,
            &self.session_store,
            &self.execution_registry,
            session_id,
            &task.task_id,
            lease_id,
        );
        let execution_cancelled = || !execution_guard.is_active();
        let initial_root = execution_root
            .cloned()
            .or_else(|| self.resolve_workspace_root_path(session_id, workspace_id));
        let scope = WorkflowToolScope {
            session_id,
            workspace_id,
            execution_root: initial_root.as_ref(),
            worker_id: *worker_id,
            lease_id,
            is_sidechain: *is_sidechain,
            prepared: RefCell::new(None),
        };
        let checkpoints = WorkflowCheckpoint {
            store,
            task,
            lease_id,
            workflow_id,
        };
        let mut state = WorkflowRunState::restore(task, stage, checkpoint, last_result.as_ref());
        if execution_cancelled() {
            return Err("工作流执行租约已失效".into());
        }
        if let Some(outcome) = self.settle_pending_action(
            &checkpoints,
            session_id,
            &mut state,
            *checkpoint_version,
            pending_action.as_ref(),
        )? {
            return Ok(outcome);
        }

        let mut tool_ledger = ToolExecutionLedger::for_task_goal(&task.goal);
        let native_tools_enabled = *use_tools
            && task
                .policy_snapshot
                .as_ref()
                .is_none_or(|policy| !policy.command_mode.eq_ignore_ascii_case("no_tools"));
        let attempt_id = lease_id.to_string();
        for step in 0..MAX_WORKFLOW_STEPS {
            if execution_cancelled() {
                return Err("工作流执行租约已失效".into());
            }
            // 宿主签发本次决策的唯一动作身份，核心只能原样回传。
            let input = WorkflowInput {
                run_id: task.task_id.to_string(),
                attempt_id: attempt_id.clone(),
                action_id: format!("{attempt_id}:{step}"),
                stage: state.stage.clone(),
                user_input: state.user_input.clone(),
                model_result: state.model_result.take(),
                tool_result: state.tool_result.take(),
                context_summary: state.context_summary.clone(),
                config: state.checkpoint.clone(),
            };
            let decision =
                WorkflowCoreDecisionRunner::run(core.clone(), input.clone(), &execution_cancelled)
                    .map_err(|error| format!("工作流决策失败：{error}"))?;
            decision
                .validate(&task.task_id.to_string(), &input)
                .map_err(|error| format!("工作流决策不符合合同：{error}"))?;
            // 核心决策可能在等待期间才返回；关闭 Turn/撤销租约后，不能把迟到
            // 的 complete/save_stage/wait_user 决策写入检查点或交付为新事实。
            if execution_cancelled() {
                return Err("工作流执行租约已失效".into());
            }
            let requires_tools = match &decision.action {
                WorkflowAction::ToolCall { .. } | WorkflowAction::DispatchTask { .. } => true,
                WorkflowAction::ModelRequest { tools, .. } => !tools.is_empty(),
                _ => false,
            };
            if !native_tools_enabled && requires_tools {
                return Err("工作流当前已关闭原生工具，不能请求工具或派发子任务".into());
            }
            let WorkflowDecision {
                checkpoint_version,
                stage,
                action,
            } = decision;
            state.stage = stage;
            match action {
                WorkflowAction::Complete { summary, .. } => {
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        None,
                        None,
                        "工作流终态持久化失败",
                    )?;
                    let attempt = magi_core::TaskCompletionAttempt {
                        output_refs: vec![summary.clone()],
                        final_response: Some(summary),
                        evidence: Vec::new(),
                    };
                    task.completion_contract
                        .validate(&attempt)
                        .map_err(|error| format!("工作流完成合同校验失败：{error}"))?;
                    return Ok(TaskOutcome::Completed { attempt });
                }
                WorkflowAction::SaveStage { checkpoint, .. } => {
                    state.checkpoint = checkpoint;
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        None,
                        None,
                        "工作流检查点持久化失败",
                    )?;
                }
                WorkflowAction::ModelRequest {
                    action_id,
                    model,
                    system,
                    context_refs,
                    tools,
                } => {
                    // 工作流只能在宿主已解析的身份和授权内收窄模型请求：模型字符串不
                    // 作为配置覆盖入口；工具子集从同一目录解析，不能凭名称扩权。
                    if model.is_some() {
                        return Err("工作流模型请求不能覆盖当前角色模型配置".into());
                    }
                    let authorized_references = task
                        .input_refs
                        .iter()
                        .collect::<std::collections::HashSet<_>>();
                    if context_refs
                        .iter()
                        .any(|reference| !authorized_references.contains(reference))
                    {
                        return Err("工作流上下文引用不属于当前任务已授权引用".into());
                    }
                    if tools.iter().any(|tool| tool.trim().is_empty()) {
                        return Err("工作流工具子集包含空名称".into());
                    }
                    let mut requested_tools =
                        tools.iter().map(|tool| tool.trim()).collect::<Vec<_>>();
                    requested_tools.sort_unstable();
                    requested_tools.dedup();
                    let model_tools = if requested_tools.is_empty() {
                        None
                    } else {
                        let access_profile = task
                            .policy_snapshot
                            .as_ref()
                            .map(magi_core::TaskPolicy::effective_access_profile)
                            .unwrap_or_default();
                        let active_skill = self.workflow_active_skill(task);
                        let definitions = self.build_tool_definitions(
                            task,
                            active_skill.as_deref(),
                            access_profile,
                            workspace_id.as_ref(),
                        );
                        let mut selected = Vec::<ChatToolDefinition>::new();
                        for requested in &requested_tools {
                            let Some(definition) = definitions
                                .iter()
                                .find(|definition| definition.function.name == *requested)
                                .cloned()
                            else {
                                return Err(format!(
                                    "工作流请求的工具未在当前授权目录中：{requested}"
                                ));
                            };
                            selected.push(definition);
                        }
                        Some(selected)
                    };
                    let call_id = format!("workflow-model-{}-{action_id}", task.task_id);
                    let turn_id = self
                        .execution_registry
                        .turn_id(&task.task_id)
                        .ok_or("工作流模型请求缺少持久 Turn 归属，拒绝调用 Provider")?;
                    let input_request_id = if *is_sidechain {
                        None
                    } else {
                        crate::model_step::root_input_request_id(
                            &self.session_store,
                            session_id,
                            &turn_id,
                        )
                    };
                    let settings =
                        self.execution_settings_or_live(execution_settings_snapshot.as_ref());
                    let context_prompt = context_refs
                        .iter()
                        .map(|reference| format!("- {reference}"))
                        .collect::<Vec<_>>();
                    let prompt = if context_prompt.is_empty() {
                        format!("{system}\n\n{}", task.goal)
                    } else {
                        format!(
                            "{system}\n\n{}\n\n已授权上下文引用：\n{}",
                            task.goal,
                            context_prompt.join("\n")
                        )
                    };
                    let request = ModelInvocationRequest {
                        provider: "workflow".into(),
                        prompt: prompt.clone(),
                        messages: Some(vec![magi_bridge_client::ChatMessage {
                            input_request_id,
                            context_origin: Default::default(),
                            role: "user".into(),
                            content: Some(prompt),
                            images: Vec::new(),
                            tool_calls: Vec::new(),
                            tool_call_id: None,
                            provider_context: Vec::new(),
                        }]),
                        tools: model_tools,
                        tool_choice: None,
                    };
                    let request_value = serde_json::to_value(&request)
                        .map_err(|error| format!("工作流模型请求意图序列化失败：{error}"))?;
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        Some(json!({
                            "kind":"model",
                            "actionId":action_id,
                            "stepId":call_id,
                            "model":model,
                            "system":system,
                            "contextRefs":context_refs,
                            "tools":requested_tools,
                            "request":request_value,
                        })),
                        None,
                        "工作流模型动作持久化失败",
                    )?;
                    let client = self.resolve_model_client_for_task(
                        self.execution_settings_or_live(execution_settings_snapshot.as_ref()),
                        Some(task),
                        *execution_role_id,
                        Some(session_id),
                    )?;
                    let mut client = crate::model_step::StepModelClient::new(
                        client.as_ref(),
                        &self.session_store,
                        session_id,
                        Some(&turn_id),
                        Some(&task.task_id),
                        thread_id,
                        &call_id,
                    )
                    .with_model_binding(settings, usage_binding);
                    if let Some(policy) = task.policy_snapshot.as_ref() {
                        // model_request 只负责生成模型结果；工具动作由工作流下一步提交，
                        // 不能让 ExecutorOwned 在模型动作内部执行第二套工具循环。
                        client =
                            client.with_execution_scope(magi_bridge_client::ModelExecutionScope {
                                working_dir: scope
                                    .prepared
                                    .borrow()
                                    .as_ref()
                                    .and_then(|prepared| prepared.root.clone())
                                    .or_else(|| initial_root.clone()),
                                policy: policy.clone(),
                                requires_native_tools: true,
                                closed_skill_scope: false,
                            });
                    }
                    let response = client.invoke_with_cancellation(request, &execution_cancelled);
                    crate::usage_recording::publish_model_usage_record_for_turn(
                        &self.event_bus,
                        &self.session_store,
                        settings,
                        Some(&turn_id),
                        crate::usage_recording::ModelUsageRecordInput {
                            context_breakdown: None,
                            session_id,
                            workspace_id,
                            binding: usage_binding,
                            call_id,
                            usage: response
                                .as_ref()
                                .ok()
                                .and_then(|response| response.usage.as_ref()),
                            status: if response.is_ok() {
                                magi_usage_authority::UsageCallStatus::Success
                            } else {
                                magi_usage_authority::UsageCallStatus::Failed
                            },
                            assignment_id: Some(task.task_id.to_string()),
                            error_code: response
                                .as_ref()
                                .err()
                                .map(|_| "workflow_model_failed".to_string()),
                        },
                    );
                    let response =
                        response.map_err(|error| format!("工作流模型请求失败：{error}"))?;
                    let result = json!({
                        "content": response.content,
                        "tool_calls": response.tool_calls,
                        "status": response.status,
                    });
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        None,
                        Some(json!({"kind":"model","value":result})),
                        "工作流模型结果持久化失败",
                    )?;
                    state.model_result = Some(result);
                }
                WorkflowAction::ToolCall {
                    action_id,
                    tool,
                    input,
                } => {
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        Some(json!({"kind":"tool","actionId":action_id,"tool":tool,"input":input})),
                        None,
                        "工作流工具动作持久化失败",
                    )?;
                    let (output, status) = self.execute_workflow_tool(
                        task,
                        &scope,
                        &action_id,
                        &tool,
                        input,
                        &mut tool_ledger,
                    );
                    if let Some(skill_id) =
                        crate::tool_surface_state::activated_skill_id_from_tool_result(
                            &tool, &output, status,
                        )
                        && self.workflow_active_skill(task).as_deref() != Some(skill_id.as_str())
                    {
                        crate::tool_surface_state::persist_task_skill_activation(
                            &self.execution_registry,
                            &self.session_store,
                            &task.task_id,
                            session_id,
                            self.execution_registry.turn_id(&task.task_id).as_deref(),
                            skill_id,
                            self.session_state_persist_callback.as_deref(),
                        )
                        .map_err(|error| {
                            format!("Skill 激活状态未持久化，已停止后续执行：{error}")
                        })?;
                    }
                    state.context_summary =
                        json!({"last_tool": tool, "status": status.wire_label()});
                    let result = json!({"status": status.wire_label(), "output": output});
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        None,
                        Some(json!({"kind":"tool","value":result})),
                        "工作流工具结果持久化失败",
                    )?;
                    if !matches!(status, ExecutionResultStatus::Succeeded) {
                        return Err(format!("工作流工具调用未成功：{output}"));
                    }
                    state.tool_result = Some(result);
                }
                WorkflowAction::WaitUser {
                    action_id,
                    question,
                } => {
                    return checkpoints.wait_for_user(
                        &state,
                        checkpoint_version,
                        action_id,
                        question,
                    );
                }
                WorkflowAction::DispatchTask {
                    action_id,
                    role,
                    instruction,
                    delegation,
                } => {
                    let delegation = magi_tool_runtime::AgentDelegationDecision::from_arguments(
                        &json!({"delegation": delegation}),
                    )
                    .map_err(|error| format!("工作流委派决策无效：{error}"))?;
                    if delegation.purpose == magi_tool_runtime::AgentDelegationPurpose::ParallelWork
                    {
                        return Err("工作流委派决策无效：dispatch_task 会立即等待子任务，不能提供父子并行收益；请重新判断真实独立价值或直接执行当前步骤".into());
                    }
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        Some(json!({
                            "kind": "dispatch_task",
                            "actionId": action_id,
                            "role": role,
                            "instruction": instruction,
                            "delegation": delegation,
                        })),
                        None,
                        "工作流派发动作持久化失败",
                    )?;
                    let child_task_id = match self.spawn_workflow_child(
                        task,
                        &scope,
                        &action_id,
                        &role,
                        &instruction,
                        &delegation,
                        &mut tool_ledger,
                    ) {
                        Ok(child_task_id) => child_task_id,
                        Err(error) => {
                            // 只有明确未创建子任务的回执才清除待决意图；未知
                            // 回执或已有任务引用保留，恢复时核对原 Task。
                            if error.no_child_created {
                                checkpoints.record(
                                    &state,
                                    checkpoint_version,
                                    None,
                                    Some(json!({
                                        "kind": "tool",
                                        "value": {
                                            "tool": "agent_spawn",
                                            "status": "failed",
                                            "output": error.message,
                                            "delegation": delegation,
                                        },
                                    })),
                                    "工作流派发失败结果持久化失败",
                                )?;
                            }
                            return Err(error.message);
                        }
                    };
                    return checkpoints.wait_for_child(
                        &state,
                        checkpoint_version,
                        action_id,
                        child_task_id,
                    );
                }
            }
        }
        Err(format!("工作流超过最大步骤数 {MAX_WORKFLOW_STEPS}"))
    }

    /// 结算上次执行遗留的待决动作。
    ///
    /// 只有“派发子任务”可以在子任务终态后继续：结果经原生任务事实回灌给核心。
    /// 其余未确认的动作可能已产生外部副作用，禁止自动重放，必须核对后恢复。
    fn settle_pending_action(
        &self,
        checkpoints: &WorkflowCheckpoint<'_>,
        session_id: &SessionId,
        state: &mut WorkflowRunState,
        checkpoint_version: u32,
        pending_action: Option<&Value>,
    ) -> Result<Option<TaskOutcome>, String> {
        let Some(action) = pending_action else {
            return Ok(None);
        };
        match action.get("kind").and_then(Value::as_str) {
            Some("dispatch_task") => {}
            Some("wait_user") => {
                return Err("工作流仍在等待用户输入，必须通过恢复入口提交回答".into());
            }
            _ => {
                return Err(
                    "工作流存在结果未确认的动作，必须核对原生执行记录后恢复，禁止自动重放".into(),
                );
            }
        }
        if action
            .get("childTaskId")
            .is_some_and(|value| value.as_str().is_none_or(|id| id.trim().is_empty()))
        {
            return Err("工作流派发检查点 childTaskId 无效，拒绝推断其他子任务".into());
        }
        let child_task_id = action
            .get("childTaskId")
            .and_then(Value::as_str)
            .map(TaskId::new)
            .or_else(|| {
                let action_id = action
                    .get("actionId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())?;
                let task_name = workflow_child_task_name(action_id);
                let canonical = crate::agent_spawn_preflight::child_canonical_task_name(
                    checkpoints.task.canonical_task_name().unwrap_or("/root"),
                    &task_name,
                );
                let mut matches = checkpoints
                    .store
                    .get_children(&checkpoints.task.task_id)
                    .into_iter()
                    .filter(|candidate| {
                        candidate.kind == magi_core::TaskKind::LocalAgent
                            && candidate.canonical_task_name() == Some(canonical.as_str())
                    });
                let child = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                if action.get("role").and_then(Value::as_str).map(str::trim)
                    != child.executor_binding_target_role()
                    || action
                        .get("instruction")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        != Some(child.goal.as_str())
                {
                    return None;
                }
                Some(child.task_id)
            })
            .ok_or("工作流派发检查点缺少 childTaskId，且无法从原动作唯一核对已创建子任务")?;
        let task = checkpoints.task;
        let child = checkpoints
            .store
            .get_task(&child_task_id)
            .ok_or("工作流派发的子任务不存在")?;
        if child.parent_task_id.as_ref() != Some(&task.task_id)
            || child.root_task_id != task.root_task_id
            || child.mission_id != task.mission_id
            || child.kind != magi_core::TaskKind::LocalAgent
            || child.workspace_scope != task.workspace_scope
            || child.write_scope != task.write_scope
        {
            return Err("工作流检查点中的子任务归属无效".into());
        }
        if !matches!(
            child.status,
            magi_core::TaskStatus::Completed
                | magi_core::TaskStatus::Failed
                | magi_core::TaskStatus::Killed
        ) {
            let action_id = action
                .get("actionId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or("工作流派发检查点缺少 actionId")?;
            return checkpoints
                .wait_for_child(state, checkpoint_version, action_id.into(), child_task_id)
                .map(Some);
        }
        let receipt = crate::agent_receipt::AgentReceipt::from_terminal_task_in_session(
            &child,
            &self.session_store,
            session_id,
        )
        .ok_or("工作流子任务终态无法生成回执")?;
        let child_value = crate::agent_receipt::render_receipts(
            vec![receipt],
            crate::agent_receipt::AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES,
        )
        .remove(0);
        checkpoints.record(
            state,
            checkpoint_version,
            None,
            Some(json!({"kind":"child_task","value":child_value})),
            "工作流子任务结果持久化失败",
        )?;
        state.tool_result = Some(child_value);
        Ok(None)
    }

    /// 经 `agent_spawn` 的唯一接纳、权限、注册和回执链路派发子任务，返回子任务身份。
    #[allow(clippy::too_many_arguments)]
    fn spawn_workflow_child(
        &self,
        task: &Task,
        scope: &WorkflowToolScope<'_>,
        action_id: &str,
        role: &str,
        instruction: &str,
        delegation: &magi_tool_runtime::AgentDelegationDecision,
        ledger: &mut ToolExecutionLedger,
    ) -> Result<TaskId, WorkflowSpawnFailure> {
        let (receipt, status) = self.execute_workflow_tool(
            task,
            scope,
            action_id,
            "agent_spawn",
            json!({
                "task_name": workflow_child_task_name(action_id),
                "role": role,
                "display_name": "工作流子任务",
                "goal": instruction,
                "delegation": delegation,
                "context_package": {
                    "summary": instruction,
                    "constraints": [],
                    "expected_output": "返回可供工作流继续决策的最终结果",
                    "references": [],
                },
            }),
            ledger,
        );
        if !matches!(status, ExecutionResultStatus::Succeeded) {
            let payload = serde_json::from_str::<Value>(&receipt).ok();
            let no_child_created = payload.as_ref().is_some_and(|value| {
                value.get("existing_task_id").is_none()
                    && value.get("child_task_id").is_none_or(Value::is_null)
                    && matches!(
                        status,
                        ExecutionResultStatus::Failed
                            | ExecutionResultStatus::Rejected
                            | ExecutionResultStatus::Cancelled
                    )
                    && (matches!(status, ExecutionResultStatus::Rejected)
                        || crate::tool_result_utils::tool_result_is_interrupted_not_started(
                            &receipt,
                        )
                        || (value.get("tool").and_then(Value::as_str) == Some("agent_spawn")
                            && value.get("child_task_id").is_some_and(Value::is_null)))
            });
            return Err(WorkflowSpawnFailure {
                message: format!("工作流派发子任务未成功：{receipt}"),
                no_child_created,
            });
        }
        serde_json::from_str::<Value>(&receipt)
            .ok()
            .and_then(|value| {
                value
                    .get("child_task_id")
                    .and_then(Value::as_str)
                    .map(TaskId::new)
            })
            .ok_or_else(|| WorkflowSpawnFailure {
                message: "工作流派发回执缺少 child_task_id".into(),
                no_child_created: false,
            })
    }

    /// 当前执行计划投影包含运行中已持久化的 Skill 激活；没有后续激活时沿用
    /// Task 接纳时的绑定。模型目录和实际工具调用必须读取同一个 Skill 来源。
    fn workflow_active_skill(&self, task: &Task) -> Option<String> {
        self.execution_registry
            .get(&task.task_id)
            .and_then(|plan| match plan {
                crate::task_execution_registry::TaskExecutionPlan::Dispatch {
                    skill_name, ..
                } => skill_name,
            })
            .or_else(|| task.executor_binding_active_skill_id().map(str::to_string))
    }

    fn execute_workflow_tool(
        &self,
        task: &Task,
        scope: &WorkflowToolScope<'_>,
        action_id: &str,
        tool: &str,
        input: Value,
        ledger: &mut ToolExecutionLedger,
    ) -> (String, ExecutionResultStatus) {
        let session_id = scope.session_id;
        let workspace_id = scope.workspace_id;
        let execution_root = scope.execution_root;
        let worker_id = scope.worker_id;
        let lease_id = scope.lease_id;
        let is_sidechain = scope.is_sidechain;
        let execution_guard = crate::task_execution_guard::TaskExecutionGuard::new(
            self.pipeline.execution_runtime.task_store(),
            &self.session_store,
            &self.execution_registry,
            session_id,
            &task.task_id,
            lease_id,
        );
        if !execution_guard.is_active() {
            return crate::tool_result_utils::tool_interrupted_before_execution_result(tool);
        }
        if scope.prepared.borrow().is_none()
            && super::tool_requires_execution_resources(tool, &input.to_string(), is_sidechain)
        {
            match self.prepare_task_workspace_execution(task, lease_id, session_id, workspace_id,
                execution_root, is_sidechain, worker_id) {
                Ok(prepared) => {
                    if prepared.root.as_ref() != execution_root {
                        ledger.rebind_workspace_root(prepared.root.as_deref());
                    }
                    *scope.prepared.borrow_mut() = Some(prepared);
                }
                Err(error) => return (
                    json!({"tool":tool,"status":"failed","error_code":"workspace_preparation_failed",
                        "execution_started":false,"error":error}).to_string(),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
        let prepared = scope.prepared.borrow();
        let resolved_root = prepared
            .as_ref()
            .and_then(|prepared| prepared.root.as_ref())
            .or(execution_root);
        if resolved_root != execution_root
            && super::tool_requires_execution_resources(tool, &input.to_string(), is_sidechain)
            && execution_root.is_some_and(|root| {
                super::arguments_reference_retired_root(
                    tool,
                    &input.to_string(),
                    root,
                    resolved_root.map(PathBuf::as_path),
                )
            })
        {
            return (json!({"tool":tool,"status":"failed","error_code":"workspace_execution_root_changed",
                "execution_started":false,"execution_root":resolved_root,
                "error":"执行目录已按需切换，请使用当前执行目录重新提交动作"}).to_string(), ExecutionResultStatus::Failed);
        }
        let execution_task = prepared
            .as_ref()
            .map(|prepared| &prepared.task)
            .unwrap_or(task);
        let snapshot_session = prepared
            .as_ref()
            .and_then(|prepared| prepared.snapshot.clone());
        let call = ChatToolCall {
            id: format!("workflow-tool-{}-{action_id}", task.task_id),
            kind: "function".into(),
            function: ChatToolFunction {
                name: tool.into(),
                arguments: input.to_string(),
            },
        };
        let plan_store = magi_plan::PlanStore::new(self.session_store.clone(), session_id.clone());
        let safety_gate = self.build_safety_gate(self.settings_store.as_ref());
        let project_memory = execution_root.and_then(|root| {
            self.project_memory_registry
                .get_or_open(&magi_core::WorkspaceRootPath::new(root.to_string_lossy()))
                .ok()
        });
        let active_skill = self.workflow_active_skill(task);
        execute_task_tool_call_batch(
            self.event_bus.as_ref(),
            self.tool_registry.as_ref(),
            self.agent_role_registry.as_ref(),
            self.skill_runtime.as_deref(),
            self.skill_dispatch_runtime.as_deref(),
            active_skill.as_deref(),
            self.pipeline.execution_runtime.task_store(),
            self.session_store.as_ref(),
            &self.execution_registry,
            self.conversation_registry.as_ref(),
            safety_gate.as_ref(),
            &plan_store,
            project_memory.as_deref(),
            execution_task,
            session_id,
            workspace_id,
            resolved_root,
            worker_id,
            self.tool_registry
                .as_ref()
                .and_then(ToolRegistry::browser_capability_snapshot),
            &[call],
            ledger,
            None,
            snapshot_session,
            Some(task.mission_id.to_string()),
            &|| execution_guard.is_active(),
        )
        .remove(0)
    }
}

/// 在专用线程与单线程运行时中驱动一次异步决策，并按租约状态协作取消。
struct WorkflowCoreDecisionRunner;

impl WorkflowCoreDecisionRunner {
    fn run(
        core: Arc<dyn WorkflowCore>,
        input: WorkflowInput,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkflowDecision, String> {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("magi-workflow-core".into())
                .spawn_scoped(scope, move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("工作流运行时不可用：{error}"))?
                        .block_on(async move {
                            let cancellation = RunCancellation::default();
                            if cancelled() {
                                cancellation.cancel();
                                return Err("工作流执行租约已失效".into());
                            }
                            let decision = core.decide(input, &cancellation);
                            tokio::pin!(decision);
                            let mut poll = tokio::time::interval(std::time::Duration::from_millis(50));
                            loop {
                                tokio::select! {
                                    result = &mut decision => return result.map_err(|error| error.to_string()),
                                    _ = poll.tick() => {
                                        if cancelled() {
                                            cancellation.cancel();
                                            return match tokio::time::timeout(
                                                std::time::Duration::from_millis(100),
                                                &mut decision,
                                            )
                                            .await
                                            {
                                                Ok(result) => result.map_err(|error| error.to_string()),
                                                Err(_) => Err("工作流执行租约已失效".into()),
                                            };
                                        }
                                    }
                                }
                            }
                        })
                })
                .map_err(|error| format!("启动工作流核心失败：{error}"))?
                .join()
                .map_err(|_| "工作流核心线程异常退出".to_string())?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConversationRegistry,
        task_execution_dispatcher::{ExecutionPipeline, LlmTaskDispatcherDependencies},
        task_execution_registry::TaskExecutionRegistry,
    };
    use magi_core::{
        MissionId, TaskCompletionContract, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::OrchestratedExecutionRuntime;
    use magi_plugin_system::{WorkflowCoreFactory, workflow::WorkflowError};
    use magi_session_store::SessionStore;
    use std::{collections::VecDeque, future::Future, pin::Pin, sync::Mutex};

    type Script = Box<dyn Fn(&WorkflowInput) -> WorkflowDecision + Send + Sync>;

    /// 按顺序返回预置决策的核心，并记录被调用次数。
    struct ScriptedCore {
        script: Mutex<VecDeque<Script>>,
        calls: Mutex<usize>,
    }

    impl WorkflowCore for ScriptedCore {
        fn decide<'a>(
            &'a self,
            input: WorkflowInput,
            _cancellation: &'a RunCancellation,
        ) -> Pin<Box<dyn Future<Output = Result<WorkflowDecision, WorkflowError>> + Send + 'a>>
        {
            Box::pin(async move {
                *self.calls.lock().unwrap() += 1;
                let next = self.script.lock().unwrap().pop_front();
                match next {
                    Some(decide) => Ok(decide(&input)),
                    None => Err(WorkflowError {
                        message: "脚本已耗尽".into(),
                    }),
                }
            })
        }
    }

    struct ScriptedFactory(Arc<ScriptedCore>);

    impl WorkflowCoreFactory for ScriptedFactory {
        fn supports(&self, _workflow_id: &str) -> bool {
            true
        }

        fn build_workflow_core(
            &self,
            _spec: WorkflowCoreInvocationSpec,
        ) -> Result<Arc<dyn WorkflowCore>, String> {
            Ok(self.0.clone())
        }
    }

    fn decision(
        action: impl Fn(&WorkflowInput) -> WorkflowAction + Send + Sync + 'static,
    ) -> Script {
        Box::new(move |input| WorkflowDecision {
            checkpoint_version: 1,
            stage: "working".into(),
            action: action(input),
        })
    }

    fn save_stage(value: Value) -> Script {
        decision(move |input| WorkflowAction::SaveStage {
            action_id: input.action_id.clone(),
            stage: "working".into(),
            checkpoint: value.clone(),
        })
    }

    fn complete(summary: &'static str) -> Script {
        decision(move |input| WorkflowAction::Complete {
            action_id: input.action_id.clone(),
            summary: summary.into(),
        })
    }

    struct Harness {
        dispatcher: LlmTaskDispatcher,
        core: Arc<ScriptedCore>,
        task_id: TaskId,
        lease_id: LeaseId,
        session_id: SessionId,
        _state_root: tempfile::TempDir,
    }

    impl Harness {
        fn new(script: Vec<Script>, pending_action: Option<Value>) -> Self {
            Self::new_with_role(script, pending_action, None)
        }

        fn new_with_role(
            script: Vec<Script>,
            pending_action: Option<Value>,
            role: Option<&str>,
        ) -> Self {
            Self::new_with_role_and_inputs(script, pending_action, role, Vec::new(), None)
        }

        fn new_with_role_and_inputs(
            script: Vec<Script>,
            pending_action: Option<Value>,
            role: Option<&str>,
            input_refs: Vec<String>,
            active_skill: Option<&str>,
        ) -> Self {
            let core = Arc::new(ScriptedCore {
                script: Mutex::new(script.into()),
                calls: Mutex::new(0),
            });
            let state_root = tempfile::tempdir().unwrap();
            let dispatcher = LlmTaskDispatcher::new(
                Arc::new(InMemoryEventBus::new(64)),
                ExecutionPipeline {
                    execution_runtime: OrchestratedExecutionRuntime::new(
                        magi_worker_runtime::WorkerRuntime::new(),
                    ),
                    memory_store: MemoryStore::new(),
                },
                LlmTaskDispatcherDependencies {
                    session_store: Arc::new(SessionStore::new()),
                    execution_registry: TaskExecutionRegistry::default(),
                    completion_sink: Arc::new(|_| panic!("此夹具不应派发 Task 终态")),
                    conversation_registry: Arc::new(ConversationRegistry::new()),
                    agent_role_registry: Arc::new(
                        magi_agent_role::AgentRoleRegistry::load_default(),
                    ),
                },
                state_root.path().to_path_buf(),
            )
            .with_workflow_core_factory(Arc::new(ScriptedFactory(core.clone())));
            let task_id = TaskId::new("workflow-task");
            let now = UtcMillis(1_000);
            let task = Task {
                task_id: task_id.clone(),
                mission_id: MissionId::new("workflow-mission"),
                root_task_id: task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::LocalWorkflow,
                title: "workflow".into(),
                goal: "做一件事".into(),
                status: TaskStatus::Pending,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: role.map(|role| {
                    magi_core::TaskExecutorBinding::for_role(role)
                        .with_active_skill_id(active_skill.map(str::to_string))
                }),
                completion_contract: TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs,
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: TaskRuntimePayload::Workflow {
                    workflow_id: "plugin/acme.flow/main".into(),
                    package_digest: Some("scripted-package".into()),
                    checkpoint_version: 1,
                    stage: "start".into(),
                    checkpoint: json!({"n": 0}),
                    pending_action,
                    last_result: None,
                },
                created_at: now,
                updated_at: now,
            };
            let store = dispatcher.pipeline.execution_runtime.task_store();
            store.insert_task(task).unwrap();
            let lease = store
                .grant_lease_and_start_task(
                    &task_id,
                    &task_id,
                    &WorkerId::new("workflow-worker"),
                    "executor",
                    10_000,
                )
                .unwrap()
                .unwrap();
            Self {
                dispatcher,
                core,
                task_id,
                lease_id: lease.lease_id,
                session_id: SessionId::new("workflow-session"),
                _state_root: state_root,
            }
        }

        fn run_with_lease(&self, lease_id: &LeaseId, use_tools: bool) -> TaskOutcome {
            let task = self
                .dispatcher
                .pipeline
                .execution_runtime
                .task_store()
                .get_task(&self.task_id)
                .unwrap();
            self.dispatcher
                .invoke_workflow_once(WorkflowInvocation {
                    task: &task,
                    use_tools,
                    lease_id,
                    session_id: &self.session_id,
                    workspace_id: &None,
                    execution_root: None,
                    worker_id: None,
                    is_sidechain: false,
                    execution_role_id: None,
                    execution_settings_snapshot: None,
                    thread_id: &ThreadId::new("workflow-test-thread"),
                    usage_binding: &crate::usage_recording::session_turn_model_usage_binding(true),
                })
                .0
        }

        fn run(&self) -> TaskOutcome {
            self.run_with_lease(&self.lease_id, true)
        }

        fn attach_turn(&self) {
            use crate::task_execution_registry::TaskExecutionPlan;
            use magi_session_store::{ActiveExecutionTurn, TimelineEntryInput, TimelineEntryKind};
            let store = &self.dispatcher.session_store;
            store
                .create_session(self.session_id.clone(), "workflow journal")
                .unwrap();
            store
                .accept_current_turn_with_timeline_entry(
                    self.session_id.clone(),
                    TimelineEntryInput::new(
                        "workflow-user",
                        TimelineEntryKind::UserMessage,
                        "work",
                        UtcMillis(1),
                    ),
                    ActiveExecutionTurn {
                        turn_id: "workflow-turn".into(),
                        turn_seq: 1,
                        accepted_at: UtcMillis(1),
                        status: "running".into(),
                        completed_at: None,
                        user_message: Some("work".into()),
                        items: vec![],
                    },
                )
                .unwrap();
            self.dispatcher
                .execution_registry
                .insert(
                    self.task_id.clone(),
                    TaskExecutionPlan::Dispatch {
                        target: magi_core::TaskExecutionTarget {
                            mission_id: MissionId::new("workflow-mission"),
                            root_task_id: self.task_id.clone(),
                            task_id: self.task_id.clone(),
                            requested_worker_id: None,
                            recovery_id: None,
                            execution_chain_ref: None,
                        },
                        worker_id: WorkerId::new("workflow-worker"),
                        thread_id: ThreadId::new("workflow-test-thread"),
                        is_primary: true,
                        session_id: self.session_id.clone(),
                        turn_id: "workflow-turn".into(),
                        workspace_id: None,
                        execution_root: None,
                        writebacks: Default::default(),
                        use_tools: true,
                        skill_name: None,
                        images: vec![],
                        execution_settings_snapshot: None,
                    },
                )
                .unwrap();
        }

        fn calls(&self) -> usize {
            *self.core.calls.lock().unwrap()
        }

        fn payload(&self) -> TaskRuntimePayload {
            self.dispatcher
                .pipeline
                .execution_runtime
                .task_store()
                .get_task(&self.task_id)
                .unwrap()
                .runtime_payload
        }
    }

    fn failure(outcome: TaskOutcome) -> String {
        match outcome {
            TaskOutcome::Failed { error } => error,
            other => panic!("期望失败，实际为 {other:?}"),
        }
    }

    #[test]
    fn saved_checkpoints_are_durable_and_completion_goes_through_the_completion_contract() {
        let harness = Harness::new(
            vec![save_stage(json!({"n": 1})), complete("全部完成")],
            None,
        );
        let TaskOutcome::Completed { attempt } = harness.run() else {
            panic!("工作流应提交完成尝试");
        };
        assert_eq!(attempt.final_response.as_deref(), Some("全部完成"));
        assert_eq!(attempt.output_refs, vec!["全部完成"]);
        let TaskRuntimePayload::Workflow {
            stage, checkpoint, ..
        } = harness.payload()
        else {
            panic!("任务必须保留工作流负载");
        };
        assert_eq!(stage, "working");
        assert_eq!(
            checkpoint,
            json!({"n": 1}),
            "保存的检查点必须落入 TaskStore"
        );
        assert_eq!(harness.calls(), 2);
        let tasks = harness.dispatcher.pipeline.execution_runtime.task_store();
        tasks
            .complete_lease_and_task(
                &harness.task_id,
                &harness.task_id,
                &harness.lease_id,
                attempt,
            )
            .unwrap();
        let task = tasks.get_task(&harness.task_id).unwrap();
        assert!(
            crate::session_turn_finalize::build_root_completion_summary(tasks, &task)
                .contains("全部完成")
        );
    }

    #[test]
    fn waiting_for_the_user_persists_the_question_and_yields_instead_of_failing() {
        let harness = Harness::new(
            vec![decision(|input| WorkflowAction::WaitUser {
                action_id: input.action_id.clone(),
                question: "选哪个？".into(),
            })],
            None,
        );
        assert!(matches!(harness.run(), TaskOutcome::Yielded));
        let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
            panic!("任务必须保留工作流负载");
        };
        let pending = pending_action.expect("等待用户必须留下待决动作");
        assert_eq!(pending["kind"], "wait_user");
        assert_eq!(pending["question"], "选哪个？");
        let store = harness.dispatcher.pipeline.execution_runtime.task_store();
        assert!(store.has_pending_user_input(&harness.task_id));
    }

    #[test]
    fn a_forged_action_id_is_rejected_before_any_effect() {
        let harness = Harness::new(
            vec![decision(|_| WorkflowAction::Complete {
                action_id: "forged".into(),
                summary: "x".into(),
            })],
            None,
        );
        let error = failure(harness.run());
        assert!(error.contains("不符合合同"), "{error}");
        let TaskRuntimePayload::Workflow { stage, .. } = harness.payload() else {
            panic!("任务必须保留工作流负载");
        };
        assert_eq!(stage, "start", "被拒绝的决策不得推进持久化阶段");
    }

    #[test]
    fn disabled_workflow_tools_reject_effects_but_allow_internal_completion() {
        for kind in ["tool", "model_tools", "delegate"] {
            let harness = Harness::new(
                vec![decision(move |input| match kind {
                    "tool" => WorkflowAction::ToolCall {
                        action_id: input.action_id.clone(),
                        tool: "get_goal".into(),
                        input: json!({}),
                    },
                    "model_tools" => WorkflowAction::ModelRequest {
                        action_id: input.action_id.clone(),
                        model: None,
                        system: "work".into(),
                        context_refs: vec![],
                        tools: vec!["web_fetch".into()],
                    },
                    _ => WorkflowAction::DispatchTask {
                        action_id: input.action_id.clone(),
                        role: "explorer".into(),
                        instruction: "investigate".into(),
                        delegation: json!({"purpose":"context_isolation","reason":"independent investigation"}),
                    },
                })],
                None,
            );
            let before = serde_json::to_value(harness.payload()).unwrap();
            let error = failure(harness.run_with_lease(&harness.lease_id, false));
            assert!(error.contains("已关闭原生工具"), "{kind}: {error}");
            assert_eq!(serde_json::to_value(harness.payload()).unwrap(), before);
        }
        let harness = Harness::new(
            vec![save_stage(json!({"saved":true})), complete("done")],
            None,
        );
        assert!(matches!(
            harness.run_with_lease(&harness.lease_id, false),
            TaskOutcome::Completed { .. }
        ));
    }

    #[test]
    fn workflow_delegation_rejects_invalid_decisions_before_recording_an_action() {
        for delegation in [
            Value::Null,
            json!({"purpose":"context_isolation", "reason":" "}),
            json!({"purpose":"parallel_work", "reason":"独立模块"}),
            json!({"purpose":"parallel_work", "reason":"独立模块", "parent_work":"另一个模块"}),
            json!({"purpose":"role_matches", "reason":"有可用角色"}),
            json!({"purpose":"independent_review", "reason":"独立复核", "approved":true}),
        ] {
            let harness = Harness::new(
                vec![decision(move |input| WorkflowAction::DispatchTask {
                    action_id: input.action_id.clone(),
                    role: "explorer".into(),
                    instruction: "读取大量日志并返回定位证据".into(),
                    delegation: delegation.clone(),
                })],
                None,
            );
            let error = failure(harness.run());
            assert!(error.contains("工作流委派决策无效"), "{error}");
            let TaskRuntimePayload::Workflow {
                pending_action,
                last_result,
                ..
            } = harness.payload()
            else {
                panic!("工作流负载");
            };
            assert!(pending_action.is_none());
            assert!(last_result.is_none());
            assert_eq!(harness.calls(), 1);
            assert_eq!(
                harness
                    .dispatcher
                    .pipeline
                    .execution_runtime
                    .task_store()
                    .all_tasks()
                    .len(),
                1
            );
            assert!(
                harness
                    .dispatcher
                    .execution_registry
                    .get(&harness.task_id)
                    .is_none()
            );
        }
    }

    #[test]
    fn workflow_delegation_preserves_its_reason_and_reaches_the_original_permission_gate() {
        let delegation = json!({
            "purpose":"context_isolation",
            "reason":"隔离大量日志中间输出，仅把错误位置和行号返回工作流"
        });
        let expected = delegation.clone();
        let harness = Harness::new(
            vec![
                decision(move |input| WorkflowAction::DispatchTask {
                    action_id: input.action_id.clone(),
                    role: "explorer".into(),
                    instruction: "读取大量日志并返回定位证据".into(),
                    delegation: delegation.clone(),
                }),
                decision(|input| {
                    let result = input.tool_result.as_ref().expect("恢复时应收到原拒绝结果");
                    assert_eq!(result["status"], "failed");
                    assert!(
                        result["output"]
                            .as_str()
                            .unwrap()
                            .contains("tool_policy_rejected")
                    );
                    WorkflowAction::Complete {
                        action_id: input.action_id.clone(),
                        summary: "核对拒绝后直接结束，不重新派发".into(),
                    }
                }),
            ],
            None,
        );
        let error = failure(harness.run());
        assert!(!error.contains("invalid_delegation_decision"), "{error}");
        assert!(error.contains("tool_policy_rejected"), "{error}");
        let TaskRuntimePayload::Workflow {
            pending_action,
            last_result,
            ..
        } = harness.payload()
        else {
            panic!("工作流负载");
        };
        assert!(pending_action.is_none(), "明确未创建的拒绝不能遗留待决派发");
        let last_result = last_result.unwrap();
        assert_eq!(last_result["value"]["delegation"], expected);
        assert!(
            last_result["value"]["output"]
                .as_str()
                .unwrap()
                .contains("tool_policy_rejected")
        );
        assert_eq!(
            harness
                .dispatcher
                .pipeline
                .execution_runtime
                .task_store()
                .all_tasks()
                .len(),
            1
        );
        assert!(matches!(harness.run(), TaskOutcome::Completed { .. }));
        assert_eq!(harness.calls(), 2);
    }

    #[test]
    fn workflow_delegation_registers_a_real_child_through_agent_spawn() {
        run_workflow_delegation_registration(false);
    }

    #[test]
    fn workflow_delegation_recovers_registered_child_after_wait_checkpoint_failure() {
        run_workflow_delegation_registration(true);
    }

    fn run_workflow_delegation_registration(fail_wait_checkpoint: bool) {
        use magi_session_store::{
            ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        };
        let delegation = json!({
            "purpose":"context_isolation",
            "reason":"隔离大量日志中间输出，仅返回错误位置和证据"
        });
        let expected = delegation.clone();
        let mut harness = Harness::new_with_role(
            vec![decision(move |input| WorkflowAction::DispatchTask {
                action_id: input.action_id.clone(),
                role: "explorer".into(),
                instruction: "读取大量日志并返回定位证据".into(),
                delegation: delegation.clone(),
            })],
            None,
            Some("coordinator"),
        );
        harness.attach_turn();
        let sessions = &harness.dispatcher.session_store;
        let tasks = harness.dispatcher.pipeline.execution_runtime.task_store();
        let parent = tasks.get_task(&harness.task_id).unwrap();
        sessions.ensure_session_mission(&harness.session_id, UtcMillis::now(), || {
            parent.mission_id.clone()
        });
        sessions
            .upsert_active_execution_chain(
                harness.session_id.clone(),
                ActiveExecutionChain {
                    session_id: harness.session_id.clone(),
                    mission_id: parent.mission_id.clone(),
                    root_task_id: parent.task_id.clone(),
                    execution_chain_ref: "workflow-chain".into(),
                    workspace_id: None,
                    active_branch_task_ids: vec![parent.task_id.clone()],
                    active_worker_bindings: vec![WorkerId::new("workflow-worker")],
                    branches: vec![ActiveExecutionBranch {
                        task_id: parent.task_id.clone(),
                        worker_id: WorkerId::new("workflow-worker"),
                        stage: "execute".into(),
                        lease_id: Some(harness.lease_id.clone()),
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: None,
                        next_step_index: None,
                        checkpoint_at: None,
                        resume_mode: None,
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("workflow-test-thread"),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: UtcMillis::now(),
                        entry_id: "workflow-user".into(),
                        trimmed_text: Some(parent.goal.clone()),
                        skill_name: None,
                    },
                    current_turn: sessions
                        .runtime_sidecar(&harness.session_id)
                        .unwrap()
                        .current_turn,
                },
            )
            .unwrap();
        if fail_wait_checkpoint {
            tasks.set_checkpoint_callback(Box::new(|snapshot| {
                if snapshot.tasks.iter().any(|task| {
                    matches!(
                        &task.runtime_payload,
                        TaskRuntimePayload::Workflow { pending_action: Some(action), .. }
                            if action.get("childTaskId").is_some()
                    )
                }) {
                    return Err(magi_core::DomainError::InvalidState {
                        message: "injected wait checkpoint failure".into(),
                    });
                }
                Ok(())
            }));
            let error = failure(harness.run());
            assert!(
                error.contains("injected wait checkpoint failure"),
                "{error}"
            );
            assert_eq!(tasks.get_children(&parent.task_id).len(), 1);
            let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
                panic!("工作流负载");
            };
            assert!(pending_action.unwrap().get("childTaskId").is_none());
            // 只重建持久 Task/lease，保留同一受控夹具的执行上下文；这不是
            // 绕过 daemon 重启后的中断/显式 Continue 准入。
            let directory = tempfile::tempdir().unwrap();
            tasks
                .checkpoint_to_projection_directory(directory.path())
                .unwrap();
            let restored = TaskStore::restore_from_projection_directory(directory.path())
                .unwrap()
                .unwrap();
            harness.dispatcher.pipeline.execution_runtime =
                OrchestratedExecutionRuntime::new(magi_worker_runtime::WorkerRuntime::new())
                    .with_task_store(Arc::new(restored));
        }
        assert!(matches!(harness.run(), TaskOutcome::Yielded));
        let tasks = harness.dispatcher.pipeline.execution_runtime.task_store();
        let children = tasks.get_children(&parent.task_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, TaskKind::LocalAgent);
        assert_eq!(children[0].executor_binding_target_role(), Some("explorer"));
        assert!(!children[0].delegation_allowed());
        assert!(
            harness
                .dispatcher
                .execution_registry
                .get(&children[0].task_id)
                .is_some()
        );
        let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
            panic!("工作流负载");
        };
        let pending = pending_action.unwrap();
        assert_eq!(pending["childTaskId"], children[0].task_id.as_str());
        let events = harness.dispatcher.event_bus.snapshot().recent_events;
        let spawns = events
            .iter()
            .filter(|event| event.event_type == "task.coordinator.agent_spawn")
            .collect::<Vec<_>>();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].payload["delegation"], expected);
        assert_eq!(harness.calls(), 1);
        assert_eq!(
            tasks.get_task(&parent.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert!(tasks.get_active_lease(&parent.task_id).is_none());
    }

    #[test]
    fn an_unconfirmed_pending_action_is_never_replayed() {
        let harness = Harness::new(
            vec![complete("不应被调用")],
            Some(json!({"kind": "tool", "actionId": "a:0", "tool": "file_write"})),
        );
        let error = failure(harness.run());
        assert!(error.contains("禁止自动重放"), "{error}");
        assert_eq!(harness.calls(), 0, "存在未确认动作时不能再询问核心");
    }

    #[test]
    fn a_workflow_still_waiting_for_the_user_cannot_run_without_the_resume_entry() {
        let harness = Harness::new(
            vec![complete("不应被调用")],
            Some(json!({"kind": "wait_user", "actionId": "a:0", "question": "?"})),
        );
        let error = failure(harness.run());
        assert!(error.contains("恢复入口"), "{error}");
        assert_eq!(harness.calls(), 0);
    }

    #[test]
    fn a_core_that_never_finishes_hits_the_step_safety_limit() {
        let script = (0..MAX_WORKFLOW_STEPS + 1)
            .map(|index| save_stage(json!({ "n": index })))
            .collect();
        let harness = Harness::new(script, None);
        let error = failure(harness.run());
        assert!(error.contains("最大步骤数"), "{error}");
        assert_eq!(harness.calls(), MAX_WORKFLOW_STEPS);
    }

    #[test]
    fn a_stale_lease_stops_the_run_before_the_core_is_consulted() {
        let harness = Harness::new(vec![complete("不应被调用")], None);
        let error = failure(harness.run_with_lease(&LeaseId::new("superseded-lease"), true));
        assert!(error.contains("租约已失效"), "{error}");
        assert_eq!(harness.calls(), 0);
    }

    #[test]
    fn an_unversioned_workflow_cannot_replay_a_checkpoint() {
        let harness = Harness::new(vec![complete("不得交付")], None);
        let task = harness
            .dispatcher
            .pipeline
            .execution_runtime
            .task_store()
            .get_task(&harness.task_id)
            .unwrap();
        let mut unversioned = task.clone();
        let TaskRuntimePayload::Workflow { package_digest, .. } = &mut unversioned.runtime_payload
        else {
            unreachable!()
        };
        *package_digest = None;
        let outcome = harness
            .dispatcher
            .invoke_workflow_once(WorkflowInvocation {
                task: &unversioned,
                use_tools: true,
                lease_id: &harness.lease_id,
                session_id: &harness.session_id,
                workspace_id: &None,
                execution_root: None,
                worker_id: None,
                is_sidechain: false,
                execution_role_id: None,
                execution_settings_snapshot: None,
                thread_id: &ThreadId::new("workflow-test-thread"),
                usage_binding: &crate::usage_recording::session_turn_model_usage_binding(true),
            })
            .0;
        assert!(failure(outcome).contains("缺少固定包摘要"));
        assert_eq!(harness.calls(), 0);
        assert_eq!(
            harness
                .dispatcher
                .pipeline
                .execution_runtime
                .task_store()
                .get_task(&harness.task_id)
                .unwrap()
                .runtime_payload,
            task.runtime_payload
        );
    }

    #[test]
    fn a_closed_turn_stops_the_workflow_while_its_task_lease_is_still_active() {
        for child_status in [None, Some(TaskStatus::Running), Some(TaskStatus::Completed)] {
            let harness = Harness::new(
                vec![complete("不得交付")],
                child_status.map(|_| json!({"kind":"dispatch_task", "actionId":"a:0", "childTaskId":"workflow-child"})),
            );
            harness.attach_turn();
            if let Some(status) = child_status {
                harness.insert_child(status);
            }
            harness
                .dispatcher
                .session_store
                .update_current_turn_status_for_turn_with_change(
                    &harness.session_id,
                    Some("workflow-turn"),
                    "cancelled",
                )
                .unwrap();
            assert!(
                harness
                    .dispatcher
                    .pipeline
                    .execution_runtime
                    .task_store()
                    .execution_lease_is_current(&harness.task_id, &harness.lease_id)
            );
            let before = harness.payload();
            let _ = failure(harness.run());
            assert_eq!(harness.calls(), 0, "关闭 Turn 后不再调用外部核心");
            assert_eq!(
                harness.payload(),
                before,
                "关闭 Turn 后不能领取待决子任务回执或重新排队"
            );
        }
    }

    #[test]
    fn a_turn_closed_during_core_decision_cannot_commit_a_new_action() {
        for action in [
            complete("不得完成已关闭的 Turn"),
            save_stage(json!({"must_not_commit": true})),
            decision(|input| WorkflowAction::WaitUser {
                action_id: input.action_id.clone(),
                question: "不得创建新的等待".into(),
            }),
        ] {
            let harness = Harness::new(Vec::new(), None);
            harness.attach_turn();
            let sessions = harness.dispatcher.session_store.clone();
            let session_id = harness.session_id.clone();
            harness
                .core
                .script
                .lock()
                .unwrap()
                .push_back(Box::new(move |input| {
                    sessions
                        .update_current_turn_status_for_turn_with_change(
                            &session_id,
                            Some("workflow-turn"),
                            "cancelled",
                        )
                        .unwrap();
                    action(input)
                }));
            let before = harness.payload();
            let error = failure(harness.run());
            assert!(error.contains("已失效"), "{error}");
            assert_eq!(harness.calls(), 1);
            assert_eq!(
                harness.payload(),
                before,
                "关闭期间返回的决策不能推进检查点或新建等待"
            );
            assert!(
                harness
                    .dispatcher
                    .pipeline
                    .execution_runtime
                    .task_store()
                    .execution_lease_is_current(&harness.task_id, &harness.lease_id)
            );
        }
    }

    #[test]
    fn workspace_preparation_rechecks_turn_after_waiting_for_resources() {
        let mut harness = Harness::new(Vec::new(), None);
        harness.attach_turn();
        let store = Arc::clone(&harness.dispatcher.session_store);
        harness.dispatcher = harness
            .dispatcher
            .with_workspace_execution_preparer(Arc::new(move |session, _, turn| {
                store
                    .update_current_turn_status_for_turn_with_change(
                        session,
                        Some(turn),
                        "cancelled",
                    )
                    .unwrap();
                Ok(None)
            }));
        let task = harness
            .dispatcher
            .pipeline
            .execution_runtime
            .task_store()
            .get_task(&harness.task_id)
            .unwrap();
        let result = harness.dispatcher.prepare_task_workspace_execution(
            &task,
            &harness.lease_id,
            &harness.session_id,
            &None,
            None,
            false,
            None,
        );
        assert!(
            result.is_err(),
            "资源等待期间关闭 Turn 后不能继续分配执行目录"
        );
    }

    #[test]
    fn model_requests_that_select_models_context_or_tools_fail_closed() {
        for request in [
            |input: &WorkflowInput| WorkflowAction::ModelRequest {
                action_id: input.action_id.clone(),
                model: Some("other-model".into()),
                system: "s".into(),
                context_refs: Vec::new(),
                tools: Vec::new(),
            },
            |input: &WorkflowInput| WorkflowAction::ModelRequest {
                action_id: input.action_id.clone(),
                model: None,
                system: "s".into(),
                context_refs: vec!["file:a.rs".into()],
                tools: Vec::new(),
            },
            |input: &WorkflowInput| WorkflowAction::ModelRequest {
                action_id: input.action_id.clone(),
                model: None,
                system: "s".into(),
                context_refs: Vec::new(),
                tools: vec!["file_read".into()],
            },
        ] {
            let harness = Harness::new(vec![decision(request)], None);
            let error = failure(harness.run());
            assert!(
                error.contains("不能覆盖当前角色模型配置")
                    || error.contains("上下文引用不属于当前任务已授权引用")
                    || error.contains("工具未在当前授权目录中"),
                "{error}"
            );
            let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
                panic!("任务必须保留工作流负载");
            };
            assert!(pending_action.is_none(), "被拒绝的请求不能留下待决动作");
        }
    }

    #[test]
    fn workflow_model_uses_the_shared_durable_step_boundary_before_provider_execution() {
        use magi_bridge_client::{ModelBridgeClient, ModelResponse};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Client(Arc<AtomicUsize>);
        impl ModelBridgeClient for Client {
            fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
                magi_bridge_client::native_executor_capabilities()
            }

            fn invoke(
                &self,
                _: ModelInvocationRequest,
            ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ModelResponse::completed("model evidence"))
            }
            fn invoke_streaming(
                &self,
                request: ModelInvocationRequest,
                _: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
            ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
                self.invoke(request)
            }
        }
        for closed in [false, true] {
            let mut harness = Harness::new(
                vec![
                    decision(|input| WorkflowAction::ModelRequest {
                        action_id: input.action_id.clone(),
                        model: None,
                        system: "work".into(),
                        context_refs: vec![],
                        tools: vec![],
                    }),
                    complete("done"),
                ],
                None,
            );
            harness.attach_turn();
            let calls = Arc::new(AtomicUsize::new(0));
            harness.dispatcher.model_bridge_client = Some(Arc::new(Client(calls.clone())));
            if closed {
                harness
                    .dispatcher
                    .session_store
                    .update_current_turn_status_for_turn_with_change(
                        &harness.session_id,
                        Some("workflow-turn"),
                        "cancelled",
                    )
                    .unwrap();
            }
            let outcome = harness.run();
            let turn = harness
                .dispatcher
                .session_store
                .canonical_turn_for_session_turn_id(&harness.session_id, "workflow-turn")
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!closed));
            if closed {
                assert!(matches!(outcome, TaskOutcome::Failed { .. }));
                assert!(turn.model_steps.is_empty());
            } else {
                assert!(matches!(outcome, TaskOutcome::Completed { .. }));
                assert_eq!(turn.model_steps.len(), 1);
                assert_eq!(turn.model_steps[0].task_id.as_ref(), Some(&harness.task_id));
                assert_eq!(
                    turn.model_steps[0].status,
                    magi_session_store::ModelStepStatus::ResponseReceived
                );
            }
        }
    }

    #[test]
    fn workflow_model_forwards_authorized_references_and_tool_subset_without_replaying_failures() {
        use magi_bridge_client::{ModelBridgeClient, ModelResponse};

        struct Client {
            requests: Arc<Mutex<Vec<ModelInvocationRequest>>>,
            runtime: OrchestratedExecutionRuntime,
            task_id: TaskId,
            fail: bool,
        }
        impl ModelBridgeClient for Client {
            fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
                magi_bridge_client::native_executor_capabilities()
            }

            fn invoke(
                &self,
                request: ModelInvocationRequest,
            ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
                // 模型请求已经发出时，原 Task 必须先持久保存完整动作意图。
                let task = self.runtime.task_store().get_task(&self.task_id).unwrap();
                let TaskRuntimePayload::Workflow { pending_action, .. } = task.runtime_payload
                else {
                    panic!("workflow payload");
                };
                let pending = pending_action.unwrap();
                assert_eq!(pending["system"], "work");
                assert_eq!(pending["model"], Value::Null);
                assert_eq!(pending["contextRefs"], json!(["file:a.rs"]));
                assert_eq!(pending["tools"], json!(["web_fetch"]));
                assert_eq!(
                    pending["stepId"],
                    format!(
                        "workflow-model-{}-{}",
                        self.task_id,
                        pending["actionId"].as_str().unwrap()
                    )
                );
                assert_eq!(pending["request"], serde_json::to_value(&request).unwrap());
                self.requests.lock().unwrap().push(request);
                if self.fail {
                    return Err(magi_bridge_client::BridgeClientError::CallFailed {
                        layer: magi_bridge_client::BridgeErrorLayer::Transport,
                        code: None,
                        message: "injected unknown model outcome".into(),
                    });
                }
                Ok(ModelResponse::completed("workflow model result"))
            }

            fn invoke_streaming(
                &self,
                request: ModelInvocationRequest,
                _: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
            ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
                self.invoke(request)
            }
        }

        for (fail, closed_skill) in [(false, false), (true, false), (false, true)] {
            let mut harness = Harness::new_with_role_and_inputs(
                vec![
                    decision(|input| WorkflowAction::ModelRequest {
                        action_id: input.action_id.clone(),
                        model: None,
                        system: "work".into(),
                        context_refs: vec!["file:a.rs".into()],
                        tools: vec![" web_fetch ".into(), "web_fetch".into()],
                    }),
                    decision(|input| {
                        assert_eq!(
                            input.model_result.as_ref().unwrap()["content"],
                            "workflow model result"
                        );
                        WorkflowAction::Complete {
                            action_id: input.action_id.clone(),
                            summary: "done".into(),
                        }
                    }),
                ],
                None,
                closed_skill.then_some("coordinator"),
                vec!["file:a.rs".into()],
                closed_skill.then_some("closed-workflow"),
            );
            harness.attach_turn();
            if closed_skill {
                let skills = magi_skill_runtime::SkillRegistry::new();
                skills.register(magi_skill_runtime::SkillDefinition {
                    skill_id: "closed-workflow".into(),
                    title: "Closed".into(),
                    instruction: "Only fetch".into(),
                    metadata: magi_skill_runtime::SkillMetadata {
                        category: "test".into(),
                        tags: vec![],
                    },
                    restrict_standard_tools: true,
                    allowed_tools: vec!["web_fetch".into()],
                    custom_tool_bindings: vec![],
                    prompt_priority: 50,
                });
                harness.dispatcher.skill_runtime =
                    Some(Arc::new(magi_skill_runtime::SkillRuntime::new(skills)));
            }
            let mut registry = ToolRegistry::new(
                Arc::new(magi_governance::GovernanceService::default()),
                harness.dispatcher.event_bus.clone(),
            );
            registry.register_default_builtins();
            harness.dispatcher.tool_registry = Some(registry);
            let requests = Arc::new(Mutex::new(Vec::new()));
            harness.dispatcher.model_bridge_client = Some(Arc::new(Client {
                requests: requests.clone(),
                runtime: harness.dispatcher.pipeline.execution_runtime.clone(),
                task_id: harness.task_id.clone(),
                fail,
            }));
            let outcome = harness.run();
            if fail {
                assert!(failure(outcome).contains("injected unknown model outcome"));
                let payload = harness.payload();
                let projection = tempfile::tempdir().unwrap();
                let tasks = harness.dispatcher.pipeline.execution_runtime.task_store();
                TaskStore::checkpoint_snapshot_to_projection_directory(
                    &tasks.snapshot(),
                    projection.path(),
                )
                .unwrap();
                let restored = TaskStore::restore_from_projection_directory(projection.path())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    restored.get_task(&harness.task_id).unwrap().runtime_payload,
                    payload
                );
                harness.dispatcher.pipeline.execution_runtime = harness
                    .dispatcher
                    .pipeline
                    .execution_runtime
                    .clone()
                    .with_task_store(Arc::new(restored));
                assert!(failure(harness.run()).contains("禁止自动重放"));
                assert_eq!(harness.calls(), 1);
                assert_eq!(harness.payload(), payload);
            } else {
                assert!(matches!(outcome, TaskOutcome::Completed { .. }));
                assert_eq!(harness.calls(), 2);
            }
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert!(request.prompt.contains("file:a.rs"));
            assert_eq!(
                request.messages.as_ref().unwrap()[0].content.as_ref(),
                Some(&request.prompt)
            );
            let tools = request.tools.as_ref().unwrap();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].function.name, "web_fetch");
            assert!(tools[0].function.parameters["properties"].is_object());
        }
    }

    #[test]
    fn workflow_model_and_tool_actions_share_initial_and_updated_skill_restrictions() {
        for updated in [false, true] {
            for model in [false, true] {
                let action = decision(move |input| {
                    if model {
                        WorkflowAction::ModelRequest {
                            action_id: input.action_id.clone(),
                            model: None,
                            system: "work".into(),
                            context_refs: vec![],
                            tools: vec!["get_goal".into()],
                        }
                    } else {
                        WorkflowAction::ToolCall {
                            action_id: input.action_id.clone(),
                            tool: "get_goal".into(),
                            input: json!({}),
                        }
                    }
                });
                let mut harness = Harness::new_with_role_and_inputs(
                    vec![action],
                    None,
                    Some("coordinator"),
                    vec![],
                    (!updated).then_some("closed-workflow"),
                );
                harness.attach_turn();
                if updated {
                    let registry = &harness.dispatcher.execution_registry;
                    let mut plan = registry.remove(&harness.task_id).unwrap();
                    let crate::task_execution_registry::TaskExecutionPlan::Dispatch {
                        skill_name,
                        ..
                    } = &mut plan;
                    *skill_name = Some("closed-workflow".into());
                    registry.insert(harness.task_id.clone(), plan).unwrap();
                }
                let skills = magi_skill_runtime::SkillRegistry::new();
                skills.register(magi_skill_runtime::SkillDefinition {
                    skill_id: "closed-workflow".into(),
                    title: "Closed".into(),
                    instruction: "Only fetch".into(),
                    metadata: magi_skill_runtime::SkillMetadata {
                        category: "test".into(),
                        tags: vec![],
                    },
                    restrict_standard_tools: true,
                    allowed_tools: vec!["web_fetch".into()],
                    custom_tool_bindings: vec![],
                    prompt_priority: 50,
                });
                harness.dispatcher.skill_runtime =
                    Some(Arc::new(magi_skill_runtime::SkillRuntime::new(skills)));
                let mut tools = ToolRegistry::new(
                    Arc::new(magi_governance::GovernanceService::default()),
                    harness.dispatcher.event_bus.clone(),
                );
                tools.register_default_builtins();
                harness.dispatcher.tool_registry = Some(tools);
                let error = failure(harness.run());
                assert!(
                    if model {
                        error.contains("工具未在当前授权目录中")
                    } else {
                        error.contains("Skill") && error.contains("不允许")
                    },
                    "updated={updated}, model={model}: {error}"
                );
                assert!(
                    harness
                        .dispatcher
                        .session_store
                        .canonical_turn_for_session_turn_id(&harness.session_id, "workflow-turn")
                        .unwrap()
                        .model_steps
                        .is_empty()
                );
                assert_eq!(harness.calls(), 1);
            }
        }
    }

    #[test]
    fn workspace_preparation_failure_settles_the_unexecuted_workflow_action() {
        let mut harness = Harness::new(
            vec![decision(|input| WorkflowAction::ToolCall {
                action_id: input.action_id.clone(),
                tool: "file_write".into(),
                input: json!({"path": "not-written.txt", "content": "must not execute"}),
            })],
            None,
        );
        harness.attach_turn();
        harness.dispatcher.workspace_execution_preparer =
            Some(Arc::new(|_, _, _| Err("injected resource failure".into())));
        let error = failure(harness.run());
        assert!(error.contains("workspace_preparation_failed"), "{error}");
        let TaskRuntimePayload::Workflow {
            pending_action,
            last_result,
            ..
        } = harness.payload()
        else {
            panic!("workflow payload");
        };
        assert!(
            pending_action.is_none(),
            "明确未执行的失败不能保留为结果未知动作"
        );
        assert!(
            last_result
                .unwrap()
                .to_string()
                .contains("execution_started")
        );
    }

    #[test]
    fn missing_core_factory_fails_without_a_default_core_fallback() {
        let mut harness = Harness::new(Vec::new(), None);
        harness.dispatcher.workflow_core_factory = None;
        let error = failure(harness.run());
        assert!(error.contains("工作流核心不可用"), "{error}");
    }
    impl Harness {
        /// 在同一任务树下登记一个指定状态的子任务，返回其身份。
        fn insert_child(&self, status: TaskStatus) -> TaskId {
            let store = self.dispatcher.pipeline.execution_runtime.task_store();
            let mut child = store.get_task(&self.task_id).unwrap();
            let child_id = TaskId::new("workflow-child");
            child.task_id = child_id.clone();
            child.parent_task_id = Some(self.task_id.clone());
            child.kind = TaskKind::LocalAgent;
            child.status = status;
            child.runtime_payload = TaskRuntimePayload::default();
            store.insert_task(child).unwrap();
            child_id
        }

        fn insert_dispatch_child(&self, status: TaskStatus) -> TaskId {
            let store = self.dispatcher.pipeline.execution_runtime.task_store();
            let mut child = store.get_task(&self.task_id).unwrap();
            let canonical = crate::agent_spawn_preflight::child_canonical_task_name(
                child.canonical_task_name().unwrap_or("/root"),
                &workflow_child_task_name("a:0"),
            );
            child.task_id = TaskId::new("workflow-recovered-child");
            child.parent_task_id = Some(self.task_id.clone());
            child.kind = TaskKind::LocalAgent;
            child.status = status;
            child.goal = "读取大量日志并返回定位证据".into();
            child.runtime_payload = TaskRuntimePayload::default();
            child.executor_binding = Some(
                magi_core::TaskExecutorBinding::for_role("explorer")
                    .with_canonical_task_name(canonical),
            );
            store.insert_task(child.clone()).unwrap();
            child.task_id
        }
    }

    #[test]
    fn a_running_child_keeps_the_workflow_waiting_without_consulting_the_core() {
        let harness = Harness::new(vec![complete("不应被调用")], None);
        let child = harness.insert_child(TaskStatus::Running);
        let harness_pending = json!({
            "kind": "dispatch_task",
            "actionId": "a:0",
            "childTaskId": child.as_str(),
        });
        let store = harness.dispatcher.pipeline.execution_runtime.task_store();
        store
            .update_workflow_state(
                WorkflowCheckpointRef {
                    task_id: &harness.task_id,
                    root_task_id: &harness.task_id,
                    lease_id: &harness.lease_id,
                    workflow_id: "plugin/acme.flow/main",
                    stage: "start",
                    checkpoint_version: 1,
                    checkpoint: &json!({"n": 0}),
                },
                Some(harness_pending),
                None,
            )
            .unwrap();
        assert!(matches!(harness.run(), TaskOutcome::Yielded));
        assert_eq!(harness.calls(), 0);
    }

    #[test]
    fn a_dispatch_checkpoint_without_child_id_recovers_only_the_matching_child() {
        let harness = Harness::new(vec![complete("不得重复询问核心")], None);
        let store = harness.dispatcher.pipeline.execution_runtime.task_store();
        let mut child = store.get_task(&harness.task_id).unwrap();
        child.task_id = TaskId::new("workflow-recovered-child");
        child.parent_task_id = Some(harness.task_id.clone());
        child.kind = TaskKind::LocalAgent;
        child.status = TaskStatus::Running;
        child.goal = "读取大量日志并返回定位证据".into();
        child.executor_binding = Some(
            magi_core::TaskExecutorBinding::for_role("explorer").with_canonical_task_name(
                crate::agent_spawn_preflight::child_canonical_task_name(
                    child.canonical_task_name().unwrap_or("/root"),
                    &workflow_child_task_name("a:0"),
                ),
            ),
        );
        store.insert_task(child.clone()).unwrap();
        store
            .update_workflow_state(
                WorkflowCheckpointRef {
                    task_id: &harness.task_id,
                    root_task_id: &harness.task_id,
                    lease_id: &harness.lease_id,
                    workflow_id: "plugin/acme.flow/main",
                    stage: "start",
                    checkpoint_version: 1,
                    checkpoint: &json!({"n": 0}),
                },
                Some(json!({
                    "kind": "dispatch_task",
                    "actionId": "a:0",
                    "role": "explorer",
                    "instruction": "读取大量日志并返回定位证据",
                    "delegation": {"purpose":"context_isolation", "reason":"隔离日志中间输出"}
                })),
                None,
            )
            .unwrap();
        assert!(matches!(harness.run(), TaskOutcome::Yielded));
        let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
            panic!("工作流负载");
        };
        assert_eq!(
            pending_action.unwrap()["childTaskId"],
            child.task_id.as_str()
        );
        assert_eq!(harness.calls(), 0);
    }

    #[test]
    fn dispatch_recovery_reads_original_terminal_receipts_without_respawning() {
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
        ] {
            let harness = Harness::new(
                vec![decision(move |input| {
                    let result = input.tool_result.as_ref().expect("原子任务回执");
                    assert_eq!(result["child_task_id"], "workflow-recovered-child");
                    WorkflowAction::Complete {
                        action_id: input.action_id.clone(),
                        summary: "已核对原结果".into(),
                    }
                })],
                Some(
                    json!({"kind":"dispatch_task", "actionId":"a:0", "role":"explorer",
                    "instruction":"读取大量日志并返回定位证据"}),
                ),
            );
            harness.insert_dispatch_child(status);
            assert!(matches!(harness.run(), TaskOutcome::Completed { .. }));
            let tasks = harness.dispatcher.pipeline.execution_runtime.task_store();
            let children = tasks.get_children(&harness.task_id);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].status, status);
            assert_eq!(harness.calls(), 1);
        }
    }

    #[test]
    fn dispatch_recovery_preserves_uncertain_or_mismatched_intents() {
        for change in [
            "absent",
            "role",
            "goal",
            "action",
            "null",
            "empty",
            "missing_id",
        ] {
            let mut pending = json!({"kind":"dispatch_task", "actionId":"a:0", "role":"explorer",
                "instruction":"读取大量日志并返回定位证据"});
            match change {
                "role" => pending["role"] = json!("reviewer"),
                "goal" => pending["instruction"] = json!("另外的工作"),
                "action" => pending["actionId"] = json!("different:0"),
                "null" => pending["childTaskId"] = Value::Null,
                "empty" => pending["childTaskId"] = json!(""),
                "missing_id" => pending["childTaskId"] = json!("unknown-child"),
                _ => {}
            }
            let expected = pending.clone();
            let harness = Harness::new(vec![complete("不得重派")], Some(pending));
            if change != "absent" {
                harness.insert_dispatch_child(TaskStatus::Running);
            }
            let error = failure(harness.run());
            assert!(
                error.contains("检查点") || error.contains("子任务不存在"),
                "{change}: {error}"
            );
            assert_eq!(harness.calls(), 0);
            let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
                panic!("工作流负载");
            };
            assert_eq!(pending_action.unwrap(), expected);
        }
    }

    #[test]
    fn a_finished_child_is_fed_back_to_the_core_as_a_persisted_receipt() {
        let harness = Harness::new(
            vec![decision(|input| {
                assert!(
                    input.tool_result.is_some(),
                    "子任务回执必须作为 tool_result 回灌给核心"
                );
                WorkflowAction::Complete {
                    action_id: input.action_id.clone(),
                    summary: "收到回执".into(),
                }
            })],
            None,
        );
        let child = harness.insert_child(TaskStatus::Completed);
        let store = harness.dispatcher.pipeline.execution_runtime.task_store();
        store
            .update_workflow_state(
                WorkflowCheckpointRef {
                    task_id: &harness.task_id,
                    root_task_id: &harness.task_id,
                    lease_id: &harness.lease_id,
                    workflow_id: "plugin/acme.flow/main",
                    stage: "start",
                    checkpoint_version: 1,
                    checkpoint: &json!({"n": 0}),
                },
                Some(json!({
                    "kind": "dispatch_task",
                    "actionId": "a:0",
                    "childTaskId": child.as_str(),
                })),
                None,
            )
            .unwrap();
        assert!(matches!(harness.run(), TaskOutcome::Completed { .. }));
        assert_eq!(harness.calls(), 1);
    }

    #[test]
    fn a_child_from_another_task_tree_is_rejected() {
        let harness = Harness::new(vec![complete("不应被调用")], None);
        let child = harness.insert_child(TaskStatus::Completed);
        let store = harness.dispatcher.pipeline.execution_runtime.task_store();
        // 子任务必须归属本工作流：伪造的 parent 不能借检查点读取其他任务的结果。
        let mut foreign = store.get_task(&child).unwrap();
        foreign.task_id = TaskId::new("foreign-child");
        foreign.parent_task_id = Some(TaskId::new("someone-else"));
        store.insert_task(foreign).unwrap();
        store
            .update_workflow_state(
                WorkflowCheckpointRef {
                    task_id: &harness.task_id,
                    root_task_id: &harness.task_id,
                    lease_id: &harness.lease_id,
                    workflow_id: "plugin/acme.flow/main",
                    stage: "start",
                    checkpoint_version: 1,
                    checkpoint: &json!({"n": 0}),
                },
                Some(json!({
                    "kind": "dispatch_task",
                    "actionId": "a:0",
                    "childTaskId": "foreign-child",
                })),
                None,
            )
            .unwrap();
        let error = failure(harness.run());
        assert!(error.contains("归属无效"), "{error}");
        assert_eq!(harness.calls(), 0);
    }
}
