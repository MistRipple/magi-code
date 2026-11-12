//! 单个任务轮次内的工具执行账本。
//!
//! 模型可以重复提出相同的 function call，但模型请求不是工具执行授权。账本在
//! 调度边界统一约束实际执行次数：只读幂等调用复用同轮成功结果；用户明确要求
//! 某工具只调用一次时，后续调用返回结构化用户调用约束结果而不再触发外部副作用。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use magi_bridge_client::ChatToolCall;
use magi_core::{ExecutionResultStatus, SessionId, Task, ThreadId};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    SessionStore, ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction,
};
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};
use serde_json::Value;

use crate::{
    canonical_tool_call_name,
    context_authority::CurrentFileFact,
    tool_result_utils::{
        DeterministicToolFailure, DeterministicToolFailureTracker, infer_tool_call_status,
        normalized_tool_arguments, tool_result_is_interrupted_not_started,
    },
};

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolExecutionLedger {
    successful_idempotent_calls: BTreeMap<ToolCallFingerprint, String>,
    current_file_facts: BTreeMap<PathBuf, CurrentFileFact>,
    workspace_root_path: Option<PathBuf>,
    project_directory_map: Option<magi_core::ProjectDirectoryExecutionMap>,
    successful_recovered_side_effect_calls: BTreeMap<ToolCallFingerprint, String>,
    interrupted_non_idempotent_calls: BTreeMap<ToolCallFingerprint, String>,
    executed_call_counts: BTreeMap<String, usize>,
    explicit_call_limits: BTreeMap<String, usize>,
    /// 本轮重复失败检测；任务轮次与对话轮次共用同一账本，因此共用同一刹车。
    failure_tracker: DeterministicToolFailureTracker,
}

/// 恢复裁决与完成证据共用完整工具事实；模型摘要不参与安全判断。
#[derive(Debug)]
pub(crate) struct RecoveredTaskTools {
    pub(crate) ledger: ToolExecutionLedger,
    pub(crate) history: Vec<ThreadChatMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ToolCallFingerprint {
    tool_name: String,
    canonical_arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallExecutionDecision {
    Execute {
        fingerprint: Option<ToolCallFingerprint>,
    },
    Reuse {
        result: String,
    },
    ReuseAfterExecution {
        source_index: usize,
        fingerprint: ToolCallFingerprint,
    },
    UserCallLimitReached {
        result: String,
    },
    RecoveryBlocked {
        result: String,
    },
}

impl ToolCallExecutionDecision {
    pub(crate) fn immediate_result(&self) -> Option<(String, ExecutionResultStatus)> {
        match self {
            Self::Reuse { result } | Self::UserCallLimitReached { result } => {
                Some((result.clone(), ExecutionResultStatus::Succeeded))
            }
            Self::RecoveryBlocked { result } => {
                Some((result.clone(), ExecutionResultStatus::Failed))
            }
            Self::Execute { .. } | Self::ReuseAfterExecution { .. } => None,
        }
    }
}

fn validate_native_recovery_contract(
    store: &SessionStore,
    session_id: &SessionId,
    task_id: &magi_core::TaskId,
    thread_id: &ThreadId,
) -> Result<(), String> {
    if store
        .canonical_model_steps_for_task(session_id, task_id, thread_id)
        .iter()
        .any(|step| {
            step.purpose != Some(magi_session_store::ModelStepPurpose::ContextCompaction)
                && step.executor_capabilities.as_ref().is_some_and(|caps| {
                    caps.resume != magi_bridge_client::ExecutorResumeMode::Checkpoint
                        || caps.context != magi_bridge_client::ExecutorContextOwnership::MagiOwned
                })
        })
    {
        return Err("executor_resume_unsupported: 原执行器不支持 Magi 检查点恢复；切换模型也不能把外部历史变成原生可重放任务，请核对远端进展后发送新的明确指令".into());
    }
    Ok(())
}

impl ToolExecutionLedger {
    pub(crate) fn for_task_goal(goal: &str) -> Self {
        Self {
            explicit_call_limits: explicit_single_call_limits(goal),
            ..Self::default()
        }
    }

    pub(crate) fn from_canonical_task(
        store: &SessionStore,
        task_store: &TaskStore,
        session_id: &SessionId,
        task: &Task,
        thread_id: &ThreadId,
        tool_registry: Option<&ToolRegistry>,
    ) -> Result<RecoveredTaskTools, String> {
        store
            .validate_approval_execution_links(session_id)
            .map_err(|error| error.to_string())?;
        validate_native_recovery_contract(store, session_id, &task.task_id, thread_id)?;
        let mut items = store.canonical_tool_calls_for_task(session_id, &task.task_id, thread_id);
        let mut checkpoint = task.recovery_checkpoint().cloned();
        let mut visited = std::collections::HashSet::new();
        visited.insert(task.task_id.clone());
        while let Some(source) = checkpoint {
            if source.source_session_id != *session_id
                || !visited.insert(source.source_task_id.clone())
            {
                return Err("工具恢复来源跨会话或存在循环，拒绝执行".into());
            }
            let source_task = task_store.get_task(&source.source_task_id).ok_or_else(|| {
                format!(
                    "工具恢复来源任务 {} 缺失，无法确认完整副作用历史",
                    source.source_task_id
                )
            })?;
            if source_task.mission_id != task.mission_id {
                return Err("工具恢复来源任务不属于当前 mission，拒绝执行".into());
            }
            validate_native_recovery_contract(
                store,
                &source.source_session_id,
                &source.source_task_id,
                &source.source_thread_id,
            )?;
            // 同一任务可能经历多轮 Continue；其全部工具事实均属于恢复链。
            items.extend(store.canonical_tool_calls_for_task(
                &source.source_session_id,
                &source.source_task_id,
                &source.source_thread_id,
            ));
            checkpoint = source_task.recovery_checkpoint().cloned();
        }
        for item in &items {
            item.background_job().map_err(|error| error.to_string())?;
            item.user_question().map_err(|error| error.to_string())?;
            item.tool_approval().map_err(|error| error.to_string())?;
        }
        let history = canonical_tool_history(items);
        let mut restored = Self::from_thread_history(&task.goal, &history, tool_registry);
        let sources =
            crate::agent_activation::previous_activation_tasks(task_store, task, session_id)?;
        if !sources.is_empty() {
            let threads = store.thread_registry_snapshot(session_id);
            let mut previous_items = Vec::new();
            for source in sources {
                let thread = threads
                    .iter()
                    .find(|thread| thread.handled_task_ids.contains(&source.task_id))
                    .ok_or("Agent 前序 activation 的 Thread 缺失，无法核对未知副作用")?;
                previous_items.extend(store.canonical_tool_calls_for_task(
                    session_id,
                    &source.task_id,
                    &thread.thread_id,
                ));
            }
            for item in &previous_items {
                item.background_job().map_err(|error| error.to_string())?;
                item.user_question().map_err(|error| error.to_string())?;
                item.tool_approval().map_err(|error| error.to_string())?;
            }
            let previous = Self::from_thread_history(
                "",
                &canonical_tool_history(previous_items),
                tool_registry,
            );
            // 新 activation 不是旧任务重放；只继承未知副作用护栏，不继承成功证据、读取缓存或调用次数。
            restored
                .interrupted_non_idempotent_calls
                .extend(previous.interrupted_non_idempotent_calls);
        }
        // 上一 activation 的观测不证明当前目录状态，恢复时重新读取；副作用防重放保留。
        restored.successful_idempotent_calls.clear();
        Ok(RecoveredTaskTools {
            ledger: restored,
            history,
        })
    }

    /// 从执行事实的消息投影构建账本；生产入口使用完整 canonical 工具记录。
    ///
    /// 工具调用先于实际执行写入 thread，成功结果随后写入。因此恢复时：已有成功
    /// 结果的调用直接复用；没有结果的非只读调用必须先检查实际状态，不能自动重放。
    pub(crate) fn from_thread_history(
        goal: &str,
        history: &[ThreadChatMessage],
        tool_registry: Option<&ToolRegistry>,
    ) -> Self {
        let mut ledger = Self::for_task_goal(goal);
        let current_turn_start = history
            .iter()
            .rposition(|message| message.role == "user")
            .map(|index| index.saturating_add(1))
            .unwrap_or_default();
        // 按历史顺序回放：写操作会使此前的只读结果失效，顺序不能丢。
        let mut calls = Vec::<(usize, ChatToolCall)>::new();
        let mut results = BTreeMap::<String, String>::new();

        for (message_index, message) in history.iter().enumerate() {
            if message.role == "assistant" {
                for call in &message.tool_calls {
                    calls.push((
                        message_index,
                        ChatToolCall {
                            id: call.id.clone(),
                            kind: call.kind.clone(),
                            function: magi_bridge_client::ChatToolFunction {
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                            },
                        },
                    ));
                }
            }
            if message.role == "tool"
                && let (Some(call_id), Some(result)) =
                    (message.tool_call_id.as_deref(), message.content.as_deref())
            {
                results.insert(call_id.to_string(), result.to_string());
            }
        }

        for (message_index, tool_call) in calls {
            let canonical_name = canonical_tool_call_name(&tool_call.function.name);
            let result = results.get(&tool_call.id);
            let belongs_to_current_turn = message_index >= current_turn_start;
            if result.is_some_and(|result| tool_result_is_interrupted_not_started(result)) {
                continue;
            }
            if belongs_to_current_turn {
                *ledger
                    .executed_call_counts
                    .entry(canonical_name.clone())
                    .or_default() += 1;
            }
            // 等待和定向取消须重新核对原 Task 的最新事实。取消只作用于原子树，
            // 不重置终态或创建 activation；缓存旧回执会让 resources_settled=false
            // 在恢复后永远不变。新调用仍经原权限与直接子任务范围检查。
            if can_recheck_after_recovery(&canonical_name, &tool_call.function.arguments) {
                if canonical_name == "agent_cancel" {
                    ledger.successful_idempotent_calls.clear();
                }
                continue;
            }
            let is_idempotent_read = is_idempotent_read_tool(&canonical_name, tool_registry);
            if !is_idempotent_read {
                // 写操作已经执行（或可能已执行）之后，之前的只读结果都不再代表当前状态。
                ledger.successful_idempotent_calls.clear();
            }

            let Some(result) = result else {
                if belongs_to_current_turn
                    && !is_idempotent_read
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .interrupted_non_idempotent_calls
                        .insert(fingerprint, interrupted_call_result(&canonical_name));
                }
                continue;
            };

            if tool_result_is_interrupted(result) {
                if belongs_to_current_turn
                    && tool_result_is_interrupted_unknown(result)
                    && !is_idempotent_read
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .interrupted_non_idempotent_calls
                        .insert(fingerprint, interrupted_call_result(&canonical_name));
                }
            } else if infer_tool_call_status(result) == "success" {
                if is_idempotent_read {
                    // 跨轮只允许 file_read 凭内容哈希证明结果仍然有效；git_status、
                    // search_text 等没有当前性证据的只读结果只在本轮内复用。
                    if (belongs_to_current_turn || canonical_name == "file_read")
                        && reusable_result_is_current(&canonical_name, result)
                        && let Some(fingerprint) =
                            tool_call_fingerprint(&tool_call, &canonical_name)
                    {
                        ledger
                            .successful_idempotent_calls
                            .insert(fingerprint, result.clone());
                    }
                } else if belongs_to_current_turn
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .successful_recovered_side_effect_calls
                        .insert(fingerprint, result.clone());
                }
            }
        }

        ledger
    }

    pub(crate) fn with_current_file_facts(
        mut self,
        facts: &[CurrentFileFact],
        workspace_root_path: Option<&Path>,
    ) -> Self {
        self.workspace_root_path = workspace_root_path.map(normalize_existing_path);
        for fact in facts {
            let path = normalize_existing_path(Path::new(&fact.absolute_path));
            self.current_file_facts.insert(path, fact.clone());
        }
        self
    }

    pub(crate) fn rebind_workspace_root(&mut self, root: Option<&Path>) {
        self.workspace_root_path = root.map(normalize_existing_path);
        self.current_file_facts.clear();
        self.successful_idempotent_calls.clear();
    }

    /// 默认 cwd 不变时，其他源目录也可能切换到执行副本。读缓存必须跟随完整
    /// 执行映射；已知/未知副作用记录保留，不能因为换目录重新执行旧写入。
    pub(crate) fn rebind_project_directories(
        &mut self,
        map: Option<&magi_core::ProjectDirectoryExecutionMap>,
    ) {
        if self.project_directory_map.as_ref() != map {
            self.project_directory_map = map.cloned();
            self.successful_idempotent_calls.clear();
        }
    }

    /// 为一个模型响应生成执行决策。相同只读调用在同一响应内只允许一个
    /// 真实执行代表；跨响应则直接复用已经成功的结果。
    pub(crate) fn plan(
        &self,
        tool_calls: &[ChatToolCall],
        tool_registry: Option<&ToolRegistry>,
    ) -> Vec<ToolCallExecutionDecision> {
        let mut first_pending_by_fingerprint = BTreeMap::<ToolCallFingerprint, usize>::new();
        let mut planned_call_counts = self.executed_call_counts.clone();

        tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| {
                let canonical_name = canonical_tool_call_name(&tool_call.function.name);
                let fingerprint = idempotent_fingerprint(tool_call, &canonical_name, tool_registry);

                if canonical_name == "file_read"
                    && let Some(result) = self.reuse_current_file_fact(tool_call)
                {
                    return ToolCallExecutionDecision::Reuse { result };
                }

                if let Some(fingerprint) = fingerprint.as_ref()
                    && let Some(result) = self.successful_idempotent_calls.get(fingerprint)
                {
                    return ToolCallExecutionDecision::Reuse {
                        result: reused_result(
                            &canonical_name,
                            result,
                            "duplicate_idempotent_call",
                            "本轮已复用相同只读工具的成功结果，未再次执行。",
                        ),
                    };
                }

                // 较早的一次成功不能掩盖后来同参数调用的未知结果。
                if let Some(fingerprint) = tool_call_fingerprint(tool_call, &canonical_name)
                    && let Some(result) = self.interrupted_non_idempotent_calls.get(&fingerprint)
                {
                    return ToolCallExecutionDecision::RecoveryBlocked {
                        result: result.clone(),
                    };
                }
                if let Some(fingerprint) = tool_call_fingerprint(tool_call, &canonical_name)
                    && let Some(result) = self
                        .successful_recovered_side_effect_calls
                        .get(&fingerprint)
                {
                    return ToolCallExecutionDecision::Reuse {
                        result: reused_result(
                            &canonical_name,
                            result,
                            "completed_before_recovery",
                            "相同外部操作已在中断前成功完成，本次恢复直接继承原结果，未重复执行。",
                        ),
                    };
                }

                if let Some(limit) = self.explicit_call_limits.get(&canonical_name)
                    && planned_call_counts
                        .get(&canonical_name)
                        .copied()
                        .unwrap_or_default()
                        >= *limit
                {
                    return ToolCallExecutionDecision::UserCallLimitReached {
                        result: user_call_limit_result(&canonical_name, *limit),
                    };
                }

                if let Some(fingerprint) = fingerprint {
                    if let Some(source_index) = first_pending_by_fingerprint.get(&fingerprint) {
                        return ToolCallExecutionDecision::ReuseAfterExecution {
                            source_index: *source_index,
                            fingerprint,
                        };
                    }
                    first_pending_by_fingerprint.insert(fingerprint.clone(), index);
                    *planned_call_counts.entry(canonical_name).or_default() += 1;
                    return ToolCallExecutionDecision::Execute {
                        fingerprint: Some(fingerprint),
                    };
                }

                *planned_call_counts.entry(canonical_name).or_default() += 1;
                ToolCallExecutionDecision::Execute { fingerprint: None }
            })
            .collect()
    }

    pub(crate) fn record_execution(
        &mut self,
        tool_call: &ChatToolCall,
        fingerprint: Option<&ToolCallFingerprint>,
        result: &(String, ExecutionResultStatus),
    ) {
        let canonical_name = canonical_tool_call_name(&tool_call.function.name);
        *self
            .executed_call_counts
            .entry(canonical_name.clone())
            .or_default() += 1;
        // 写操作只要实际执行过（失败的命令也可能已改动文件），此前所有只读结果都
        // 不再代表当前状态；被拒绝或等待审批的调用没有执行，不影响缓存。
        if !matches!(
            result.1,
            ExecutionResultStatus::Rejected | ExecutionResultStatus::NeedsApproval
        ) && !is_idempotent_read_tool(&canonical_name, None)
        {
            self.successful_idempotent_calls.clear();
            self.current_file_facts.clear();
        }
        if matches!(result.1, ExecutionResultStatus::Succeeded)
            && let Some(fingerprint) = fingerprint
        {
            self.successful_idempotent_calls
                .insert(fingerprint.clone(), result.0.clone());
        }
        if canonical_name == "file_read"
            && matches!(result.1, ExecutionResultStatus::Succeeded)
            && let Some(fact) = current_file_fact_from_result(&result.0)
        {
            self.current_file_facts.insert(
                normalize_existing_path(Path::new(&fact.absolute_path)),
                fact,
            );
        }
    }

    /// 记录一次工具结果；同一调用（规范化参数）以相同错误连续失败达到上限时返回终止原因。
    pub(crate) fn observe_tool_result(
        &mut self,
        tool_name: &str,
        arguments: &str,
        result: &str,
        status: ExecutionResultStatus,
        retry_limit: u32,
    ) -> Option<DeterministicToolFailure> {
        self.failure_tracker
            .observe(tool_name, arguments, result, status, retry_limit)
    }

    pub(crate) fn execute_batch_with(
        &mut self,
        tool_calls: &[ChatToolCall],
        tool_registry: Option<&ToolRegistry>,
        mut execute: impl FnMut(&[ChatToolCall]) -> Vec<(String, ExecutionResultStatus)>,
    ) -> Vec<(String, ExecutionResultStatus)> {
        let decisions = self.plan(tool_calls, tool_registry);
        let execution_indices = decisions
            .iter()
            .enumerate()
            .filter_map(|(index, decision)| {
                matches!(decision, ToolCallExecutionDecision::Execute { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let execution_calls = execution_indices
            .iter()
            .map(|index| tool_calls[*index].clone())
            .collect::<Vec<_>>();
        let executed_results = execute(&execution_calls);
        let mut results = vec![None; tool_calls.len()];
        for (execution_index, result) in execution_indices.iter().zip(executed_results) {
            let ToolCallExecutionDecision::Execute { fingerprint } = &decisions[*execution_index]
            else {
                unreachable!("only execute decisions are dispatched");
            };
            if !tool_result_is_interrupted_not_started(&result.0) {
                self.record_execution(&tool_calls[*execution_index], fingerprint.as_ref(), &result);
            }
            results[*execution_index] = Some(result);
        }

        let mut retry_indices = Vec::new();
        for (index, decision) in decisions.iter().enumerate() {
            if let Some(result) = decision.immediate_result() {
                results[index] = Some(result);
                continue;
            }
            match decision {
                ToolCallExecutionDecision::ReuseAfterExecution {
                    source_index,
                    fingerprint,
                } => {
                    let Some(source_result) = results[*source_index].as_ref() else {
                        unreachable!("duplicate source must execute before its reuse decision");
                    };
                    if let Some(reused) =
                        self.reuse_after_execution(&tool_calls[index], fingerprint, source_result)
                    {
                        results[index] = Some(reused);
                    } else {
                        retry_indices.push(index);
                    }
                }
                ToolCallExecutionDecision::Execute { .. } => {}
                ToolCallExecutionDecision::Reuse { .. }
                | ToolCallExecutionDecision::UserCallLimitReached { .. }
                | ToolCallExecutionDecision::RecoveryBlocked { .. } => {
                    unreachable!("immediate decisions are handled before deferred execution")
                }
            }
        }

        if !retry_indices.is_empty() {
            let retry_calls = retry_indices
                .iter()
                .map(|index| tool_calls[*index].clone())
                .collect::<Vec<_>>();
            let retry_results = execute(&retry_calls);
            for (index, result) in retry_indices.into_iter().zip(retry_results) {
                let ToolCallExecutionDecision::ReuseAfterExecution { fingerprint, .. } =
                    &decisions[index]
                else {
                    unreachable!("only failed duplicate calls are retried");
                };
                if !tool_result_is_interrupted_not_started(&result.0) {
                    self.record_execution(&tool_calls[index], Some(fingerprint), &result);
                }
                results[index] = Some(result);
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                result.unwrap_or_else(|| {
                    (
                        serde_json::json!({
                            "tool": tool_calls[index].function.name,
                            "status": "failed",
                            "error_code": "tool_execution_missing_result",
                            "error": "工具执行未返回结果"
                        })
                        .to_string(),
                        ExecutionResultStatus::Failed,
                    )
                })
            })
            .collect()
    }

    pub(crate) fn reuse_after_execution(
        &self,
        tool_call: &ChatToolCall,
        fingerprint: &ToolCallFingerprint,
        source_result: &(String, ExecutionResultStatus),
    ) -> Option<(String, ExecutionResultStatus)> {
        if !matches!(source_result.1, ExecutionResultStatus::Succeeded) {
            return None;
        }
        let source = self.successful_idempotent_calls.get(fingerprint)?;
        Some((
            reused_result(
                &canonical_tool_call_name(&tool_call.function.name),
                source,
                "duplicate_idempotent_call",
                "本轮已复用相同只读工具的成功结果，未再次执行。",
            ),
            ExecutionResultStatus::Succeeded,
        ))
    }

    fn reuse_current_file_fact(&self, tool_call: &ChatToolCall) -> Option<String> {
        let arguments = serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?;
        let requested_path = arguments.get("path").and_then(Value::as_str)?;
        let context = magi_tool_runtime::ToolExecutionContext {
            workspace_id: self
                .project_directory_map
                .as_ref()
                .map(|map| map.workspace_id.clone()),
            working_directory: self.workspace_root_path.clone(),
            project_directory_map: self.project_directory_map.clone(),
            ..Default::default()
        }
        .for_tool_arguments(&tool_call.function.arguments)
        .ok()?;
        let absolute_path = magi_core::HostPath::resolve_native_input(
            requested_path,
            context.working_directory.as_deref(),
            None,
        )
        .ok()?
        .into_path_buf();
        let fact = self
            .current_file_facts
            .get(&normalize_existing_path(&absolute_path))?;
        if !magi_snapshot::path_content_hash(Path::new(&fact.absolute_path))
            .is_ok_and(|actual_hash| actual_hash == fact.content_hash)
        {
            return None;
        }
        if !magi_tool_runtime::file_read_result_matches_request(&fact.result, &arguments) {
            return None;
        }
        Some(reused_result(
            "file_read",
            &fact.result,
            "current_session_file_fact",
            "文件内容未变化，已复用当前会话中的文件事实，未再次执行读取。",
        ))
    }
}

fn canonical_tool_history(
    mut items: Vec<magi_session_store::CanonicalTurnItem>,
) -> Vec<ThreadChatMessage> {
    items.sort_by(|left, right| {
        left.turn_seq
            .cmp(&right.turn_seq)
            .then(left.item_seq.cmp(&right.item_seq))
    });
    let item_ids = items
        .iter()
        .map(|item| {
            (
                item.session_id.clone(),
                item.turn_id.clone(),
                item.item_id.clone(),
            )
        })
        .collect::<std::collections::HashSet<_>>();
    let mut history = Vec::with_capacity(items.len() * 2);
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let background_job = item.background_job().ok().flatten();
        let question_result = item
            .user_question()
            .ok()
            .flatten()
            .and_then(|question| question.result_payload());
        let approval = item.tool_approval().ok().flatten();
        if let Some(execution) = approval
            .as_ref()
            .and_then(|approval| approval.execution.as_ref())
            && (execution.turn_id != item.turn_id || execution.item_id != item.item_id)
            && item_ids.contains(&(
                item.session_id.clone(),
                execution.turn_id.clone(),
                execution.item_id.clone(),
            ))
        {
            continue;
        }
        let identity = (item.session_id, item.turn_id, item.item_id);
        if !seen.insert(identity.clone()) {
            continue;
        }
        let call_id = serde_json::to_string(&identity).expect("tool identity is serializable");
        let Some(tool) = item.tool else {
            continue;
        };
        let mut tool = tool;
        if let Some(Value::String(text)) = tool.result.as_ref()
            && let Ok(parsed) = serde_json::from_str::<Value>(text)
        {
            tool.result = Some(parsed);
        }
        let awaiting_approval = tool.result.as_ref().is_some_and(|result| {
            matches!(
                result.get("status").and_then(Value::as_str),
                Some("awaiting_approval" | "awaiting_user_input")
            )
        });
        let unfinished_progress = item.status
            != magi_session_store::CanonicalTurnItemStatus::Completed
            && tool.result.as_ref().is_some_and(|result| {
                matches!(
                    result.get("status").and_then(Value::as_str),
                    Some("running" | "pending" | "outcome_unknown")
                )
            });
        let incomplete_job = background_job.as_ref().is_some_and(|job| {
            job.status != magi_tool_runtime::BackgroundJobStatus::Completed
                && !tool.result.as_ref().is_some_and(|result| {
                    tool_result_is_interrupted_not_started(&result.to_string())
                })
        });
        let unknown = incomplete_job
            || unfinished_progress
            || matches!(
                item.status,
                magi_session_store::CanonicalTurnItemStatus::Running
                    | magi_session_store::CanonicalTurnItemStatus::Pending
                    | magi_session_store::CanonicalTurnItemStatus::Blocked
                    | magi_session_store::CanonicalTurnItemStatus::Cancelled
                    | magi_session_store::CanonicalTurnItemStatus::Indeterminate
            );
        let confirmed_question_response = question_result.is_some();
        let result = if let Some(result) = question_result {
            result.to_string()
        } else if let Some(result) = approval
            .as_ref()
            .and_then(|approval| approval.denied_result())
        {
            result.to_string()
        } else if awaiting_approval
            && approval
                .as_ref()
                .is_none_or(|approval| approval.execution.is_none())
        {
            crate::tool_result_utils::tool_interrupted_before_execution_payload(&tool.name)
        } else if unknown
            && !tool
                .result
                .as_ref()
                .is_some_and(|result| tool_result_is_interrupted_not_started(&result.to_string()))
        {
            serde_json::json!({"tool":tool.name,"status":"interrupted","execution":"unknown",
                    "reason":"canonical_tool_outcome_unknown", "job_id":background_job.as_ref().map(|job| &job.job_id)})
            .to_string()
        } else if let Some(result) = tool.result.as_ref() {
            match result {
                Value::String(text) => text.clone(),
                value => value.to_string(),
            }
        } else if let Some(error) = tool.error {
            serde_json::json!({"tool":tool.name,"status":"failed","error":error}).to_string()
        } else {
            serde_json::json!({"tool":tool.name,"status":"interrupted","execution":"unknown",
                    "reason":"canonical_tool_outcome_unknown"})
            .to_string()
        };
        let result = if item.status == magi_session_store::CanonicalTurnItemStatus::Failed
            && !confirmed_question_response
            && infer_tool_call_status(&result) == "success"
        {
            serde_json::json!({"status":"failed", "tool":tool.name, "source_result":result})
                .to_string()
        } else {
            result
        };
        let arguments = match tool.arguments {
            Some(Value::String(text)) => text,
            Some(value) => value.to_string(),
            None => "{}".into(),
        };
        history.push(ThreadChatMessage {
            input_request_id: None,
            role: "assistant".into(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: call_id.clone(),
                kind: "function".into(),
                function: ThreadChatToolFunction {
                    name: tool.name,
                    arguments,
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        });
        history.push(ThreadChatMessage {
            input_request_id: None,
            role: "tool".into(),
            content: Some(result),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id),
            provider_context: Vec::new(),
        });
    }
    history
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn current_file_fact_from_result(result: &str) -> Option<CurrentFileFact> {
    let payload = serde_json::from_str::<Value>(result).ok()?;
    if payload.get("tool").and_then(Value::as_str) != Some("file_read")
        || infer_tool_call_status(result) != "success"
    {
        return None;
    }
    let absolute_path = payload.get("path").and_then(Value::as_str)?.to_string();
    let content_hash = payload
        .get("content_hash")
        .and_then(Value::as_str)?
        .to_string();
    let summary = payload
        .get("content")
        .and_then(Value::as_str)
        .or_else(|| payload.get("summary").and_then(Value::as_str))
        .unwrap_or("文件读取成功")
        .chars()
        .take(8_000)
        .collect();
    Some(CurrentFileFact {
        absolute_path,
        content_hash,
        result: result.to_string(),
        summary,
    })
}

fn reusable_result_is_current(tool_name: &str, result: &str) -> bool {
    if tool_name != "file_read" {
        return true;
    }
    let Ok(payload) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    let Some(path) = payload.get("path").and_then(Value::as_str) else {
        return false;
    };
    let Some(expected_hash) = payload.get("content_hash").and_then(Value::as_str) else {
        return false;
    };
    magi_snapshot::path_content_hash(Path::new(path))
        .is_ok_and(|actual_hash| actual_hash == expected_hash)
}

fn idempotent_fingerprint(
    tool_call: &ChatToolCall,
    canonical_name: &str,
    tool_registry: Option<&ToolRegistry>,
) -> Option<ToolCallFingerprint> {
    if !is_idempotent_read_tool(canonical_name, tool_registry) {
        return None;
    }
    tool_call_fingerprint(tool_call, canonical_name)
}

fn tool_call_fingerprint(
    tool_call: &ChatToolCall,
    canonical_name: &str,
) -> Option<ToolCallFingerprint> {
    // 只对可解析的 JSON 参数去重；规范形式与审批、重复失败检测共用。
    serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?;
    Some(ToolCallFingerprint {
        tool_name: canonical_name.to_string(),
        canonical_arguments: normalized_tool_arguments(&tool_call.function.arguments),
    })
}

fn is_idempotent_read_tool(tool_name: &str, tool_registry: Option<&ToolRegistry>) -> bool {
    BuiltinToolName::from_name(tool_name).is_some_and(|tool| tool.is_idempotent_read_operation())
        || tool_registry.is_some_and(|registry| registry.is_idempotent_read_tool(tool_name))
}

fn can_recheck_after_recovery(tool_name: &str, arguments: &str) -> bool {
    BuiltinToolName::from_name(tool_name).is_some_and(|tool| {
        matches!(
            tool,
            BuiltinToolName::AgentWait | BuiltinToolName::AgentCancel
        ) || tool.is_background_process_observation(arguments)
    })
}

fn explicit_single_call_limits(goal: &str) -> BTreeMap<String, usize> {
    let normalized = goal.trim().to_ascii_lowercase();
    let mut limits = BTreeMap::new();

    for tool in BuiltinToolName::ALL {
        let name = tool.as_str();
        let chinese = [
            format!("只调用一次 {name}"),
            format!("仅调用一次 {name}"),
            format!("只使用一次 {name}"),
            format!("仅使用一次 {name}"),
            format!("{name} 只调用一次"),
            format!("{name} 仅调用一次"),
        ];
        let english = [
            format!("only call {name} once"),
            format!("call {name} only once"),
            format!("call {name} exactly once"),
            format!("only use {name} once"),
            format!("use {name} only once"),
            format!("use {name} exactly once"),
        ];
        if chinese.iter().any(|pattern| goal.contains(pattern))
            || english.iter().any(|pattern| normalized.contains(pattern))
        {
            limits.insert(name.to_string(), 1);
        }
    }
    limits
}

fn reused_result(tool_name: &str, source_result: &str, reason: &str, message: &str) -> String {
    let source_result = serde_json::from_str::<Value>(source_result)
        .unwrap_or_else(|_| Value::String(source_result.to_string()));
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "reused",
        "reason": reason,
        "message": message,
        "source_result": source_result,
    })
    .to_string()
}

fn user_call_limit_result(tool_name: &str, limit: usize) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "skipped",
        "reason": "user_tool_call_limit_reached",
        "message": format!("用户已要求本轮 {tool_name} 最多调用 {limit} 次；已达到用户指定的次数，请基于已有结果继续回答。"),
    })
    .to_string()
}

fn interrupted_call_result(tool_name: &str) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "interrupted",
        "execution": "blocked",
        "reason": "interrupted_tool_outcome_unknown",
        "message": "上次执行在结果持久化前中断，无法确认该非只读操作是否已生效。请先核对真实状态并取得明确结论；不得改写参数规避核对，系统不会自动重放同一调用。",
    })
    .to_string()
}

fn tool_result_is_interrupted_unknown(result: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("interrupted")
        && matches!(
            value.get("execution").and_then(Value::as_str),
            Some("unknown" | "blocked")
        )
}

fn tool_result_is_interrupted(result: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("interrupted")
}

#[cfg(test)]
mod tests;
