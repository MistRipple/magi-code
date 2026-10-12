//! 工作流任务（`TaskKind::LocalWorkflow`）的宿主执行。
//!
//! 工作流核心只返回类型化动作；这里负责逐步校验、在 TaskStore 中持久化动作意图和真实
//! 结果、经原生执行服务执行工具与子任务，并把“等待用户”“等待子任务”表达为
//! `TaskOutcome::Yielded`。权限、预算、取消和终态仍归原生服务：本模块不提交终态，只
//! 返回完成尝试，由统一完成门裁决。

use super::{LlmTaskDispatcher, TaskLlmInvocationInput};
use crate::{
    task_runner_bridge::TaskOutcome, tool_batch::execute_task_tool_call_batch,
    tool_execution_ledger::ToolExecutionLedger,
};
use magi_bridge_client::{ChatToolCall, ChatToolFunction, ModelInvocationRequest};
use magi_core::{ExecutionResultStatus, LeaseId, SessionId, Task, TaskId, WorkerId, WorkspaceId};
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
use std::{path::PathBuf, sync::Arc};

/// 一次工作流执行的步数上限，防止核心无限循环占用租约。
const MAX_WORKFLOW_STEPS: usize = 32;

/// 工作流执行用到的调用上下文，由通用任务调用输入裁剪而来。
pub(super) struct WorkflowInvocation<'a> {
    task: &'a Task,
    lease_id: &'a LeaseId,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    execution_root: Option<&'a PathBuf>,
    worker_id: Option<&'a WorkerId>,
    is_sidechain: bool,
    execution_role_id: Option<&'a str>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
}

impl<'a> From<&TaskLlmInvocationInput<'a>> for WorkflowInvocation<'a> {
    fn from(input: &TaskLlmInvocationInput<'a>) -> Self {
        Self {
            task: input.task,
            lease_id: input.lease_id,
            session_id: input.session_id,
            workspace_id: input.workspace_id,
            execution_root: input.execution_root,
            worker_id: input.worker_id,
            is_sidechain: input.is_sidechain,
            execution_role_id: input.execution_role_id,
            execution_settings_snapshot: input.execution_settings_snapshot.clone(),
        }
    }
}

/// 工作流工具动作的执行环境：只借用调用方已解析的会话、工作区与执行根。
#[derive(Clone, Copy)]
struct WorkflowToolScope<'a> {
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    execution_root: Option<&'a PathBuf>,
    worker_id: Option<&'a WorkerId>,
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
            lease_id,
            session_id,
            workspace_id,
            execution_root,
            worker_id,
            is_sidechain,
            execution_role_id,
            execution_settings_snapshot,
        } = invocation;
        let magi_core::TaskRuntimePayload::Workflow {
            workflow_id,
            checkpoint_version,
            stage,
            checkpoint,
            pending_action,
            last_result,
        } = &task.runtime_payload
        else {
            return Err("LocalWorkflow 缺少持久化工作流核心身份".into());
        };
        let core = self
            .build_workflow_core(WorkflowCoreInvocationSpec {
                session_id: session_id.to_string(),
                project_id: workspace_id
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                run_id: task.task_id.to_string(),
                workflow_id: workflow_id.clone(),
                checkpoint_version: *checkpoint_version,
                config: checkpoint.clone(),
            })
            .map_err(|error| format!("工作流核心不可用：{error}"))?;
        let store = self.pipeline.execution_runtime.task_store();
        let execution_cancelled = || {
            store
                .get_active_lease(&task.task_id)
                .is_none_or(|lease| lease.lease_id != **lease_id)
        };
        let workspace_identity_root = workspace_id
            .as_ref()
            .and_then(|_| self.resolve_workspace_root_path(session_id, workspace_id));
        let resolved_root = self.resolve_task_execution_root(
            task,
            lease_id,
            session_id,
            workspace_id,
            *execution_root,
            *is_sidechain,
            *worker_id,
        )?;
        let execution_task = Self::task_for_execution_root(
            task,
            workspace_identity_root.as_ref(),
            resolved_root.as_ref(),
        );
        let scope = WorkflowToolScope {
            session_id,
            workspace_id,
            execution_root: resolved_root.as_ref(),
            worker_id: *worker_id,
        };
        let checkpoints = WorkflowCheckpoint {
            store,
            task,
            lease_id,
            workflow_id,
        };
        let mut state = WorkflowRunState::restore(task, stage, checkpoint, last_result.as_ref());
        if let Some(outcome) = self.settle_pending_action(
            &checkpoints,
            &mut state,
            *checkpoint_version,
            pending_action.as_ref(),
        )? {
            return Ok(outcome);
        }

        let mut tool_ledger = ToolExecutionLedger::for_task_goal(&task.goal);
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
                        output_refs: vec![format!("workflow-stage:{}", state.stage)],
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
                    // 模型选择、上下文引用与工具子集需要由 Magi 装配（设计 §7.1）。该装配
                    // 尚未接入，宁可明确拒绝，也不静默忽略核心提交的意图。
                    if model.is_some() || !context_refs.is_empty() || !tools.is_empty() {
                        return Err("工作流模型请求暂不支持 model、context_refs、tools 选择".into());
                    }
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        Some(json!({"kind":"model","actionId":action_id,"system":system})),
                        None,
                        "工作流模型动作持久化失败",
                    )?;
                    let client = self.resolve_model_client_for_task(
                        self.execution_settings_or_live(execution_settings_snapshot.as_ref()),
                        Some(task),
                        *execution_role_id,
                        Some(session_id),
                    )?;
                    let response = client
                        .invoke_with_cancellation(
                            ModelInvocationRequest {
                                provider: "workflow".into(),
                                prompt: format!("{system}\n\n{}", task.goal),
                                messages: None,
                                tools: None,
                                tool_choice: None,
                            },
                            &execution_cancelled,
                        )
                        .map_err(|error| format!("工作流模型请求失败：{error}"))?;
                    let result = Value::String(response.content.unwrap_or_default());
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
                        &execution_task,
                        scope,
                        &action_id,
                        &tool,
                        input,
                        &mut tool_ledger,
                    );
                    state.context_summary =
                        json!({"last_tool": tool, "status": status.wire_label()});
                    let result = json!({"status": status.wire_label(), "output": output});
                    if !matches!(status, ExecutionResultStatus::Succeeded) {
                        return Err(format!("工作流工具调用未成功：{output}"));
                    }
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        None,
                        Some(json!({"kind":"tool","value":result})),
                        "工作流工具结果持久化失败",
                    )?;
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
                } => {
                    checkpoints.record(
                        &state,
                        checkpoint_version,
                        Some(json!({
                            "kind": "dispatch_task",
                            "actionId": action_id,
                            "role": role,
                            "instruction": instruction,
                        })),
                        None,
                        "工作流派发动作持久化失败",
                    )?;
                    let child_task_id = self.spawn_workflow_child(
                        &execution_task,
                        scope,
                        &action_id,
                        &role,
                        &instruction,
                        &mut tool_ledger,
                    )?;
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
        let child_task_id = action
            .get("childTaskId")
            .and_then(Value::as_str)
            .map(TaskId::new)
            .ok_or("工作流派发检查点缺少 childTaskId")?;
        let task = checkpoints.task;
        let child = checkpoints
            .store
            .get_task(&child_task_id)
            .ok_or("工作流派发的子任务不存在")?;
        if child.parent_task_id.as_ref() != Some(&task.task_id)
            || child.root_task_id != task.root_task_id
            || child.mission_id != task.mission_id
        {
            return Err("工作流检查点中的子任务归属无效".into());
        }
        if !matches!(
            child.status,
            magi_core::TaskStatus::Completed
                | magi_core::TaskStatus::Failed
                | magi_core::TaskStatus::Killed
        ) {
            return Ok(Some(TaskOutcome::Yielded));
        }
        let receipt = crate::agent_receipt::AgentReceipt::from_terminal_task(&child)
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
    fn spawn_workflow_child(
        &self,
        task: &Task,
        scope: WorkflowToolScope<'_>,
        action_id: &str,
        role: &str,
        instruction: &str,
        ledger: &mut ToolExecutionLedger,
    ) -> Result<TaskId, String> {
        use sha2::{Digest, Sha256};
        let action_digest = format!("{:x}", Sha256::digest(action_id.as_bytes()));
        let (receipt, status) = self.execute_workflow_tool(
            task,
            scope,
            action_id,
            "agent_spawn",
            json!({
                "task_name": format!("workflow_{}", &action_digest[..32]),
                "role": role,
                "display_name": "工作流子任务",
                "goal": instruction,
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
            return Err("工作流派发子任务未成功".into());
        }
        serde_json::from_str::<Value>(&receipt)
            .ok()
            .and_then(|value| {
                value
                    .get("child_task_id")
                    .and_then(Value::as_str)
                    .map(TaskId::new)
            })
            .ok_or_else(|| "工作流派发回执缺少 child_task_id".to_string())
    }

    fn execute_workflow_tool(
        &self,
        task: &Task,
        scope: WorkflowToolScope<'_>,
        action_id: &str,
        tool: &str,
        input: Value,
        ledger: &mut ToolExecutionLedger,
    ) -> (String, ExecutionResultStatus) {
        let WorkflowToolScope {
            session_id,
            workspace_id,
            execution_root,
            worker_id,
        } = scope;
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
        let snapshot_session = self.snapshot_manager.as_ref().and_then(|manager| {
            self.resolve_workspace_root_path(session_id, workspace_id)
                .as_ref()
                .and_then(|root| manager.get_session_for_workspace(session_id.as_str(), root))
        });
        let project_memory = execution_root.and_then(|root| {
            self.project_memory_registry
                .get_or_open(&magi_core::WorkspaceRootPath::new(root.to_string_lossy()))
                .ok()
        });
        execute_task_tool_call_batch(
            self.event_bus.as_ref(),
            self.tool_registry.as_ref(),
            self.agent_role_registry.as_ref(),
            self.skill_runtime.as_deref(),
            self.skill_dispatch_runtime.as_deref(),
            task.executor_binding_active_skill_id(),
            self.pipeline.execution_runtime.task_store(),
            self.session_store.as_ref(),
            &self.execution_registry,
            self.conversation_registry.as_ref(),
            safety_gate.as_ref(),
            &plan_store,
            project_memory.as_deref(),
            task,
            session_id,
            workspace_id,
            execution_root,
            worker_id,
            self.tool_registry
                .as_ref()
                .and_then(ToolRegistry::browser_capability_snapshot),
            &[call],
            ledger,
            None,
            snapshot_session,
            Some(task.mission_id.to_string()),
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
        task_runner_bridge::EventBasedResultReceiver,
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
    }

    impl Harness {
        fn new(script: Vec<Script>, pending_action: Option<Value>) -> Self {
            let core = Arc::new(ScriptedCore {
                script: Mutex::new(script.into()),
                calls: Mutex::new(0),
            });
            let state_root = tempfile::tempdir().unwrap().keep();
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
                    result_receiver: Arc::new(EventBasedResultReceiver::new()),
                    conversation_registry: Arc::new(ConversationRegistry::new()),
                    agent_role_registry: Arc::new(
                        magi_agent_role::AgentRoleRegistry::load_default(),
                    ),
                },
                state_root,
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
                executor_binding: None,
                completion_contract: TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: TaskRuntimePayload::Workflow {
                    workflow_id: "plugin/acme.flow/main".into(),
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
            }
        }

        fn run_with_lease(&self, lease_id: &LeaseId) -> TaskOutcome {
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
                    lease_id,
                    session_id: &self.session_id,
                    workspace_id: &None,
                    execution_root: None,
                    worker_id: None,
                    is_sidechain: false,
                    execution_role_id: None,
                    execution_settings_snapshot: None,
                })
                .0
        }

        fn run(&self) -> TaskOutcome {
            self.run_with_lease(&self.lease_id)
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
        assert_eq!(attempt.output_refs, vec!["workflow-stage:working"]);
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
    fn a_core_that_never_finishes_hits_the_step_budget() {
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
        let error = failure(harness.run_with_lease(&LeaseId::new("superseded-lease")));
        assert!(error.contains("租约已失效"), "{error}");
        assert_eq!(harness.calls(), 0);
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
            assert!(error.contains("暂不支持"), "{error}");
            let TaskRuntimePayload::Workflow { pending_action, .. } = harness.payload() else {
                panic!("任务必须保留工作流负载");
            };
            assert!(pending_action.is_none(), "被拒绝的请求不能留下待决动作");
        }
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
