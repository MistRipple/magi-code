//! 用户插件命令是原生 Task 的执行动作；不创建独立会话执行器或模型占位对象。
use super::*;
use crate::plugin_command::PluginCommandInvocation;
use crate::task_execution_guard::TaskExecutionGuard;
use magi_session_store::CanonicalTurnItemKind;
use std::sync::atomic::{AtomicBool, Ordering};

impl LlmTaskDispatcher {
    pub(super) fn execute_plugin_task_command(
        &self,
        input: &TaskInvocationInput<'_>,
    ) -> Option<TaskOutcome> {
        // 命令身份来自接纳时原子提交的用户 item，不能从提示词或子任务文本推断。
        if input.is_sidechain || input.task.parent_task_id.is_some() {
            return None;
        }
        let turn_id = self.execution_registry.turn_id(&input.task.task_id)?;
        let turn = self
            .session_store
            .canonical_turn_for_session_turn_id(input.session_id, &turn_id)?;
        let item = turn
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)?;
        let command = item.metadata.get("pluginCommandId")?;
        let run = || -> Result<TaskOutcome, String> {
            let command_id = command
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("插件命令身份无效")?
                .to_owned();
            let executor = self
                .plugin_command_executor
                .clone()
                .ok_or("插件命令执行服务不可用")?;
            let guard = TaskExecutionGuard::new(
                self.pipeline.task_store.as_ref(),
                &self.session_store,
                &self.execution_registry,
                input.session_id,
                &input.task.task_id,
                input.lease_id,
            );
            if !guard.is_active() {
                return Ok(TaskOutcome::Cancelled);
            }
            let prepared = self
                .prepare_task_workspace_execution(
                    input.task,
                    input.lease_id,
                    input.session_id,
                    input.workspace_id,
                    input.execution_root,
                    false,
                    input.worker_id,
                )
                .map_err(|error| format!("插件命令执行目录准备失败，调用尚未发送：{error}"))?;
            let context = magi_tool_runtime::ToolExecutionContext {
                task_id: Some(input.task.task_id.clone()),
                worker_id: input.worker_id.cloned(),
                session_id: Some(input.session_id.clone()),
                workspace_id: input.workspace_id.clone(),
                access_profile: input
                    .task
                    .policy_snapshot
                    .as_ref()
                    .map(|policy| policy.access_profile)
                    .unwrap_or_default(),
                working_directory: prepared.root.clone(),
                project_directory_map: prepared.resources.directories,
                ..Default::default()
            }
            .for_tool_arguments("{}")
            .map_err(|error| error.to_string())?;
            let invocation = PluginCommandInvocation {
                command_id,
                turn_id: turn_id.clone(),
                context,
                input: item.content.clone().unwrap_or_default(),
                job: item
                    .metadata
                    .get("pluginJob")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| format!("插件作业上下文无效：{error}"))?,
            };
            if !guard.is_active() {
                return Ok(TaskOutcome::Cancelled);
            }
            let cancellation = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancellation);
            let worker = std::thread::spawn(move || executor.invoke(invocation, &worker_cancel));
            while !worker.is_finished() {
                if !guard.is_active() {
                    cancellation.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let result = worker
                .join()
                .map_err(|_| "插件命令执行线程异常退出".to_string())?;
            // 停止或替换之后，迟到的成功和失败都不能写入后续 Turn。
            if !guard.is_active() {
                return Ok(TaskOutcome::Cancelled);
            }
            let result = result.map_err(|error| format!("插件命令执行失败：{error}"))?;
            let visibility = crate::task_turn_visibility(
                input.task,
                false,
                input.worker_id,
                input.thread_id,
                &self.agent_role_registry,
            );
            conversation_loop::complete_task_command(conversation_loop::TaskCommandCompletion {
                event_bus: &self.event_bus,
                session_store: &self.session_store,
                task_store: self.pipeline.task_store.as_ref(),
                task: input.task,
                session_id: input.session_id,
                workspace_id: input.workspace_id,
                visibility: &visibility,
                turn_id: &turn_id,
                persist: self.session_state_persist_callback.as_deref(),
                content: &result.content,
            })
        };
        Some(run().unwrap_or_else(|error| TaskOutcome::Failed { error }))
    }

    /// 当 `ctx.agentLoop` 显式选中非 `magi-native` 执行循环时接管主线 Turn。
    /// 选中外部循环不可用或执行失败时直接返回 `TaskOutcome::Failed`，绝不静默回退到原生循环。
    pub(super) fn execute_plugin_agent_loop(
        &self,
        input: &TaskInvocationInput<'_>,
    ) -> Option<TaskOutcome> {
        if input.is_sidechain || input.task.parent_task_id.is_some() {
            return None;
        }
        let hooks = self.extension_hooks.as_ref()?;
        if !hooks.has_selected_plugin_agent_loop() {
            return None;
        }
        let turn_id = self.execution_registry.turn_id(&input.task.task_id)?;
        let turn = self
            .session_store
            .canonical_turn_for_session_turn_id(input.session_id, &turn_id)?;
        let prompt = turn
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            .and_then(|item| item.content.clone())
            .unwrap_or_else(|| input.task.goal.clone());
        let guard = TaskExecutionGuard::new(
            self.pipeline.task_store.as_ref(),
            &self.session_store,
            &self.execution_registry,
            input.session_id,
            &input.task.task_id,
            input.lease_id,
        );
        if !guard.is_active() {
            return Some(TaskOutcome::Cancelled);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation);
        let hooks = Arc::clone(hooks);
        let session_id_str = input.session_id.as_str().to_string();
        let turn_id_str = turn_id.clone();
        let task_id_str = input.task.task_id.as_str().to_string();
        let worker = std::thread::spawn(move || {
            let req = crate::extension_hooks::PluginAgentLoopRequest {
                session_id: &session_id_str,
                turn_id: &turn_id_str,
                task_id: &task_id_str,
                prompt: &prompt,
            };
            hooks.dispatch_agent_loop(&req, &worker_cancel)
        });
        while !worker.is_finished() {
            if !guard.is_active() {
                cancellation.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let outcome = match worker.join() {
            Ok(outcome) => outcome,
            Err(_) => {
                return Some(TaskOutcome::Failed {
                    error: "插件执行循环线程异常退出".to_string(),
                });
            }
        };
        if !guard.is_active() {
            return Some(TaskOutcome::Cancelled);
        }
        match outcome {
            crate::extension_hooks::PluginAgentLoopOutcome::Native => None,
            crate::extension_hooks::PluginAgentLoopOutcome::Failed { error } => {
                Some(TaskOutcome::Failed { error })
            }
            crate::extension_hooks::PluginAgentLoopOutcome::Completed { output } => {
                let visibility = crate::task_turn_visibility(
                    input.task,
                    false,
                    input.worker_id,
                    input.thread_id,
                    &self.agent_role_registry,
                );
                Some(
                    conversation_loop::complete_task_command(
                        conversation_loop::TaskCommandCompletion {
                            event_bus: &self.event_bus,
                            session_store: &self.session_store,
                            task_store: self.pipeline.task_store.as_ref(),
                            task: input.task,
                            session_id: input.session_id,
                            workspace_id: input.workspace_id,
                            visibility: &visibility,
                            turn_id: &turn_id,
                            persist: self.session_state_persist_callback.as_deref(),
                            content: &output,
                        },
                    )
                    .unwrap_or_else(|error| TaskOutcome::Failed { error }),
                )
            }
        }
    }
}
