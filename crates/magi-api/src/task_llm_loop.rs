use crate::{
    builtin_tool_schema::internal_builtin_tool_rejection_payload,
    prompt_utils::{
        normalize_model_stream_preview_content, normalize_model_visible_content,
        workspace_context_system_prompt,
    },
    session_turn_writeback::{
        append_session_turn_item_with_task_store, publish_current_session_turn_item_event,
        publish_session_turn_item_event, session_turn_item,
        upsert_session_turn_item_with_task_store,
    },
    settings_store::SettingsStore,
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        infer_tool_call_status, summarize_tool_result, tool_execution_status_label,
        turn_item_status_for_tool_result,
    },
    usage_recording::{ModelUsageBinding, publish_model_usage_record},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolChoice, ChatToolDefinition, ModelBridgeClient,
    ModelInvocationRequest, ModelStreamingDelta, SHADOW_MODEL_PROVIDER,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, LeaseId, RiskLevel, SessionId, TaskId,
    TaskKind, TaskStatus, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{
    ExecutionContextSummary, task_runner::TaskOutcome, task_store::TaskStore,
    task_worker_catalog::resolve_task_role,
};
use magi_session_store::{ActiveExecutionTurnItem, SessionStore, TimelineEntryKind};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::UsageCallStatus;
use std::{path::PathBuf, sync::Arc, thread};

const BASE_TOOL_CALL_ROUNDS: usize = 16;
const MAX_TOOL_CALL_ROUNDS: usize = 32;

pub(crate) struct TaskLlmLoopRequest<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub task_store: &'a TaskStore,
    pub task: &'a magi_core::Task,
    pub task_id: &'a TaskId,
    pub lease_id: &'a LeaseId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
    pub usage_binding: &'a ModelUsageBinding,
    pub streaming_entry_id: Option<&'a str>,
    pub worker_lane_id: Option<&'a str>,
    pub worker_lane_seq: Option<usize>,
    pub worker_id: Option<&'a magi_core::WorkerId>,
    pub context_summary: Option<ExecutionContextSummary>,
    pub system_prompt: Option<String>,
    pub workspace_root_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct TaskTurnVisibility {
    thread_visible: bool,
    worker_visible: bool,
    primary_worker_sidechain: bool,
    role_id: Option<String>,
    worker_id: Option<magi_core::WorkerId>,
    lane_id: Option<String>,
    lane_seq: Option<usize>,
}

fn task_turn_visibility(
    task: &magi_core::Task,
    thread_visible: bool,
    worker_lane_id: Option<&str>,
    worker_lane_seq: Option<usize>,
    worker_id: Option<&magi_core::WorkerId>,
    primary_worker_sidechain: bool,
) -> TaskTurnVisibility {
    let role_id = resolve_task_role(task)
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(ToOwned::to_owned);
    let lane_id = worker_lane_id
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .map(ToOwned::to_owned);
    TaskTurnVisibility {
        thread_visible,
        worker_visible: lane_id.is_some(),
        primary_worker_sidechain,
        role_id,
        worker_id: worker_id.cloned(),
        lane_id,
        lane_seq: worker_lane_seq,
    }
}

fn apply_task_turn_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    item.task_id = Some(task.task_id.clone());
    item.thread_visible = visibility.thread_visible;
    item.worker_visible = visibility.worker_visible;
    if visibility.worker_visible {
        item.lane_id = visibility.lane_id.clone();
        item.lane_seq = visibility.lane_seq;
        item.worker_id = visibility.worker_id.clone();
        if let Some(role_id) = visibility.role_id.as_ref() {
            item.role_id = Some(role_id.clone());
            item.source = role_id.clone();
        } else if let Some(worker_id) = visibility.worker_id.as_ref() {
            item.source = worker_id.to_string();
        }
    }
}

fn apply_task_worker_detail_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
    if !visibility.primary_worker_sidechain {
        return;
    }
    item.thread_visible = false;
    item.worker_visible = true;
    item.worker_id = visibility.worker_id.clone();
    if let Some(role_id) = visibility.role_id.as_ref() {
        item.role_id = Some(role_id.clone());
        item.source = role_id.clone();
    } else if let Some(worker_id) = visibility.worker_id.as_ref() {
        item.source = worker_id.to_string();
    }
}

fn apply_task_final_visibility(
    item: &mut ActiveExecutionTurnItem,
    task_store: &TaskStore,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
    let root_is_completed = task_store
        .get_task(&task.root_task_id)
        .is_some_and(|root| root.status == TaskStatus::Completed);
    if visibility.primary_worker_sidechain && !root_is_completed {
        apply_task_worker_detail_visibility(item, task, visibility);
    }
}

pub(crate) fn run_task_llm_loop(
    request: TaskLlmLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    let TaskLlmLoopRequest {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        task_store,
        task,
        task_id,
        lease_id,
        session_id,
        workspace_id,
        prompt,
        tools,
        usage_binding,
        streaming_entry_id,
        worker_lane_id,
        worker_lane_seq,
        worker_id,
        context_summary,
        system_prompt,
        workspace_root_path,
    } = request;

    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    if let Some(root_path) = workspace_root_path.as_ref() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(workspace_context_system_prompt(
                &root_path.display().to_string(),
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    let task_context = task_event_context(task, session_id, workspace_id);
    publish_task_llm_started(
        event_bus,
        task,
        session_id,
        workspace_id,
        &prompt,
        &task_context,
    );

    let mut final_content = String::new();
    let mut tool_call_records: Vec<serde_json::Value> = Vec::new();
    let mut failed_tool_summaries: Vec<String> = Vec::new();
    let required_tool_chain = task_required_tool_chain(task);
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let mut last_stream_item_id: Option<String> = None;
    let mut had_tool_calls = false;
    let primary_worker_sidechain = worker_lane_id.is_none()
        && worker_id.is_some()
        && current_turn_has_worker_lanes(session_store, session_id);
    let turn_visibility = task_turn_visibility(
        task,
        streaming_entry_id.is_some()
            || task_is_thread_visible_turn_owner(session_store, session_id, task_id),
        worker_lane_id,
        worker_lane_seq,
        worker_id,
        primary_worker_sidechain,
    );

    if let Some(final_content) = deterministic_task_final_content(task, task_store) {
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_final_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &final_content,
                None,
                streaming_entry_id,
                &turn_visibility,
            );
        }
        return (
            TaskOutcome::Completed {
                output_refs: vec![final_content],
            },
            context_summary,
        );
    }

    let tool_call_round_limit = tool_call_round_limit(&required_tool_chain);
    for round in 0..tool_call_round_limit {
        let should_record_turn_artifacts =
            turn_visibility.thread_visible || turn_visibility.worker_visible;
        let thinking_item_id = format!("turn-item-assistant-thinking-{task_id}-{round}");
        let stream_item_id = task_stream_item_id(task_id, round, streaming_entry_id);
        last_stream_item_id = Some(stream_item_id.clone());
        let streamed_thinking = std::cell::RefCell::new(String::new());
        let last_thinking_len = std::cell::Cell::new(0usize);
        let invocation_request = ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: prompt.clone(),
            messages: Some(messages.clone()),
            tools: tools.clone(),
            tool_choice: forced_task_tool_choice_for_round(
                &required_tool_chain,
                tools.as_ref(),
                &completed_required_tool_names,
            ),
        };

        let response = if streaming_entry_id.is_some() {
            let on_delta = |delta: &ModelStreamingDelta| {
                if should_record_turn_artifacts {
                    publish_task_thinking_delta(
                        event_bus,
                        session_store,
                        task_store,
                        task,
                        session_id,
                        workspace_id,
                        &thinking_item_id,
                        &last_thinking_len,
                        &streamed_thinking,
                        &turn_visibility,
                        &delta.thinking,
                    );
                }
                publish_stream_delta(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &stream_item_id,
                    (round == 0).then_some(stream_item_id.as_str()),
                    &turn_visibility,
                    &delta.content,
                );
            };

            match client.invoke_streaming(invocation_request, &on_delta) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                    if should_record_turn_artifacts
                        && task_lease_is_current(task_store, task_id, lease_id)
                    {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
                        },
                        context_summary,
                    );
                }
            }
        } else {
            match client.invoke(invocation_request) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM invocation failed");
                    if should_record_turn_artifacts
                        && task_lease_is_current(task_store, task_id, lease_id)
                    {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
                        },
                        context_summary,
                    );
                }
            }
        };

        let parsed = response.parse_chat_payload();
        if should_record_turn_artifacts {
            let final_thinking = parsed
                .thinking
                .as_deref()
                .map(str::trim)
                .filter(|thinking| !thinking.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    let thinking = streamed_thinking.borrow();
                    let trimmed = thinking.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                });
            if let Some(thinking) = final_thinking {
                upsert_task_thinking_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &thinking_item_id,
                    &turn_visibility,
                    "completed",
                    &thinking,
                );
            }
        }
        publish_model_usage_record(
            event_bus,
            session_store,
            settings_store,
            session_id,
            workspace_id,
            usage_binding,
            format!("task-{}-{}-{round}", task_id, lease_id),
            parsed.usage.as_ref(),
            UsageCallStatus::Success,
            Some(lease_id.to_string()),
            None,
        );

        if let Some(ref content) = parsed.content {
            final_content = content.clone();
        }
        if !parsed.tool_calls.is_empty() {
            had_tool_calls = true;
        }

        if parsed.tool_calls.is_empty() {
            if !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: parsed.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            break;
        }

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });

        for tool_call in &parsed.tool_calls {
            if should_record_turn_artifacts {
                append_task_tool_call_started_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &turn_visibility,
                    tool_call,
                );
            }
        }

        let tool_results = execute_task_tool_call_batch(
            event_bus,
            tool_registry,
            skill_runtime,
            task,
            session_id,
            workspace_id,
            workspace_root_path.as_ref(),
            turn_visibility.worker_id.as_ref(),
            &parsed.tool_calls,
        );

        for (tool_call, (result, tool_status)) in parsed.tool_calls.iter().zip(tool_results) {
            if should_record_turn_artifacts {
                upsert_task_tool_call_result_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &turn_visibility,
                    tool_call,
                    &result,
                    tool_status,
                );
            }
            if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
                failed_tool_summaries.push(format!(
                    "{}: {}",
                    tool_call.function.name,
                    summarize_tool_result(&result)
                ));
            }
            tool_call_records.push(tool_call_record(tool_call, &result));
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id.clone()),
            });
        }
        record_completed_required_tools(
            &mut completed_required_tool_names,
            &required_tool_chain,
            &parsed
                .tool_calls
                .iter()
                .map(|tool_call| canonical_tool_call_name(&tool_call.function.name))
                .collect::<Vec<_>>(),
        );
    }

    if final_content.trim().is_empty() {
        let failure_reason = if had_tool_calls {
            "模型在工具调用后未返回最终回复"
        } else {
            "模型未返回可显示回复"
        };
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
            },
            context_summary,
        );
    }
    final_content = normalize_model_visible_content(final_content);
    if final_content.trim().is_empty() {
        let failure_reason = "模型未返回可显示回复";
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
            },
            context_summary,
        );
    }
    if !task_lease_is_current(task_store, task_id, lease_id) {
        return (
            TaskOutcome::Failed {
                error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
            },
            context_summary,
        );
    }

    if let Some(failure_reason) = task_tool_failure_reason(task.kind, &failed_tool_summaries) {
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                &failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }

    if task.kind == TaskKind::Validation && validation_result_rejects_delivery(&final_content) {
        let failure_reason = compact_validation_failure(&final_content);
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                &failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }

    if turn_visibility.thread_visible || turn_visibility.worker_visible {
        append_task_final_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &final_content,
            last_stream_item_id.as_deref().or(streaming_entry_id),
            streaming_entry_id,
            &turn_visibility,
        );
    }

    (
        TaskOutcome::Completed {
            output_refs: vec![build_output_content(tool_call_records, final_content)],
        },
        context_summary,
    )
}

fn task_tool_failure_reason(
    task_kind: TaskKind,
    failed_tool_summaries: &[String],
) -> Option<String> {
    if task_kind == TaskKind::Validation || failed_tool_summaries.is_empty() {
        return None;
    }
    let compact = failed_tool_summaries
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let suffix = if failed_tool_summaries.len() > 3 {
        format!("；另有 {} 个工具失败", failed_tool_summaries.len() - 3)
    } else {
        String::new()
    };
    Some(format!("工具执行失败，任务不能标记完成：{compact}{suffix}"))
}

fn validation_result_rejects_delivery(content: &str) -> bool {
    let leading = content.trim_start().chars().take(240).collect::<String>();
    let lower = leading.to_ascii_lowercase();
    let normalized = leading
        .chars()
        .filter(|ch| !matches!(ch, '*' | '_' | '`' | '#' | '>' | ' ' | '\t' | '\r' | '\n'))
        .collect::<String>();
    let negative_markers = [
        "不通过",
        "未通过",
        "部分通过",
        "验收未通过",
        "验证未通过",
        "无法确认",
        "未能确认",
        "不能判定",
        "不满足",
    ];
    negative_markers
        .iter()
        .any(|marker| normalized.contains(marker))
        || lower.starts_with("failed")
        || lower.starts_with("failure")
        || lower.starts_with("not passed")
        || lower.contains("not passed")
        || lower.contains("does not pass")
}

fn compact_validation_failure(content: &str) -> String {
    let trimmed = content.trim();
    let compact = trimmed.chars().take(240).collect::<String>();
    if trimmed.chars().count() > 240 {
        format!("验证未通过: {compact}…")
    } else {
        format!("验证未通过: {compact}")
    }
}

fn deterministic_task_final_content(
    task: &magi_core::Task,
    task_store: &TaskStore,
) -> Option<String> {
    if is_planning_no_tool_action(task) {
        return Some(deterministic_planning_content(task));
    }
    if is_planning_text_validation(task) {
        return deterministic_planning_validation_content(task, task_store);
    }
    if is_execution_tool_validation(task) {
        return deterministic_execution_tool_validation_content(task, task_store);
    }
    None
}

fn is_planning_no_tool_action(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Action
        && task.title.contains("梳理目标")
        && task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        && task.dependency_ids.is_empty()
}

fn deterministic_planning_content(task: &magi_core::Task) -> String {
    let goal = extract_deep_task_goal(&task.goal).unwrap_or_else(|| task.goal.trim().to_string());
    format!(
        "目标：{goal}\n\n边界：规划步骤只整理目标、边界、执行计划和验收标准，不调用工具，不执行文件、shell 或网络操作。\n\n执行计划：执行步骤负责按用户目标调用工具并产生可验证结果；交付步骤只基于执行产出总结，不重复调用工具。\n\n验收标准：规划文本必须包含目标、边界、执行计划、验收标准四部分；执行结果必须以真实工具结果为准，失败或阻塞不得伪装成功。"
    )
}

fn is_planning_text_validation(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("只验证规划文本完整性")
}

fn deterministic_planning_validation_content(
    task: &magi_core::Task,
    task_store: &TaskStore,
) -> Option<String> {
    let dependency_text = task
        .dependency_ids
        .iter()
        .filter_map(|dependency_id| task_store.get_task(dependency_id))
        .flat_map(|dependency| dependency.output_refs)
        .collect::<Vec<_>>()
        .join("\n\n");
    let has_required_sections = ["目标：", "边界：", "执行计划：", "验收标准："]
        .iter()
        .all(|section| dependency_text.contains(section));
    has_required_sections.then(|| {
        "通过。规划文本已包含目标、边界、执行计划和验收标准；本步骤未验证后续执行结果、文件内容或工作区变更。".to_string()
    })
}

fn is_execution_tool_validation(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("实际执行和工具结果")
}

fn deterministic_execution_tool_validation_content(
    task: &magi_core::Task,
    task_store: &TaskStore,
) -> Option<String> {
    let dependencies = task
        .dependency_ids
        .iter()
        .filter_map(|dependency_id| task_store.get_task(dependency_id))
        .collect::<Vec<_>>();
    if dependencies.is_empty() {
        return None;
    }

    let mut required_tools = Vec::new();
    let mut observed_tools = Vec::new();
    let mut failed_tools = Vec::new();
    let mut has_final_text = false;

    for dependency in dependencies {
        for tool_name in task_required_tool_chain(&dependency) {
            if !required_tools.iter().any(|existing| existing == &tool_name) {
                required_tools.push(tool_name);
            }
        }
        for output in dependency.output_refs {
            collect_dependency_output_validation_facts(
                &output,
                &mut observed_tools,
                &mut failed_tools,
                &mut has_final_text,
            );
        }
    }

    let missing_tools = required_tools
        .iter()
        .filter(|tool_name| !observed_tools.iter().any(|observed| observed == *tool_name))
        .cloned()
        .collect::<Vec<_>>();

    if !failed_tools.is_empty() || !missing_tools.is_empty() || !has_final_text {
        return None;
    }

    let tools = if observed_tools.is_empty() {
        "无工具调用".to_string()
    } else {
        observed_tools.join(", ")
    };
    Some(format!(
        "通过。已基于依赖任务的结构化输出核验当前执行产物，工具调用均成功且最终回复已生成；已验证工具：{tools}。"
    ))
}

fn collect_dependency_output_validation_facts(
    output: &str,
    observed_tools: &mut Vec<String>,
    failed_tools: &mut Vec<String>,
    has_final_text: &mut bool,
) {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        *has_final_text = true;
        return;
    };
    let Some(blocks) = value.get("blocks").and_then(serde_json::Value::as_array) else {
        if !trimmed.is_empty() {
            *has_final_text = true;
        }
        return;
    };
    for block in blocks {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("tool_call") => {
                let Some(tool_call) = block.get("toolCall") else {
                    continue;
                };
                let Some(tool_name) = tool_call
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(canonical_tool_call_name)
                else {
                    continue;
                };
                if !observed_tools.iter().any(|observed| observed == &tool_name) {
                    observed_tools.push(tool_name.clone());
                }
                let status = tool_call
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if status != "success" {
                    failed_tools.push(tool_name.clone());
                    continue;
                }
                let result_status = tool_call
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|result| serde_json::from_str::<serde_json::Value>(result).ok())
                    .and_then(|result| {
                        result
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    });
                if result_status
                    .as_deref()
                    .is_some_and(|status| status != "succeeded")
                {
                    failed_tools.push(tool_name);
                }
            }
            Some("text") => {
                if block
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| !content.trim().is_empty())
                {
                    *has_final_text = true;
                }
            }
            _ => {}
        }
    }
}

fn extract_deep_task_goal(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("<<<MAGI_DEEP_TASK_GOAL>>>")?;
    let (goal, _) = rest.split_once("<<<END_MAGI_DEEP_TASK_GOAL>>>")?;
    Some(
        goal.trim()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn task_required_tool_chain(task: &magi_core::Task) -> Vec<String> {
    if task.kind != TaskKind::Action {
        return Vec::new();
    }
    if task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| !policy.command_mode.eq_ignore_ascii_case("full"))
    {
        return Vec::new();
    }
    let normalized = task.goal.to_ascii_lowercase();
    let mut matches: Vec<(&'static str, usize)> = Vec::new();
    for (alias, canonical_name) in public_builtin_tool_reference_aliases() {
        let Some(position) = tool_reference_position(&normalized, alias) else {
            continue;
        };
        if let Some((_, existing_position)) =
            matches.iter_mut().find(|(name, _)| *name == canonical_name)
        {
            *existing_position = (*existing_position).min(position);
        } else {
            matches.push((canonical_name, position));
        }
    }
    matches.sort_by_key(|(_, position)| *position);
    matches
        .into_iter()
        .map(|(tool_name, _)| tool_name.to_string())
        .collect()
}

fn public_builtin_tool_reference_aliases() -> Vec<(&'static str, &'static str)> {
    let mut aliases = Vec::new();
    for tool in BuiltinToolName::ALL {
        if tool.is_public_tool_surface() {
            let name = tool.as_str();
            aliases.push((name, name));
        }
    }
    aliases.extend([
        ("file_view", "file_read"),
        ("file_create", "file_write"),
        ("file_edit", "file_patch"),
        ("file_insert", "file_patch"),
        ("code_search_regex", "search_text"),
        ("code_search_semantic", "search_semantic"),
        ("shell", "shell_exec"),
        ("project_knowledge_query", "knowledge_query"),
    ]);
    aliases
}

fn tool_reference_position(text: &str, tool_name: &str) -> Option<usize> {
    text.match_indices(tool_name).find_map(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + tool_name.len()..].chars().next();
        (is_tool_reference_boundary(before) && is_tool_reference_boundary(after)).then_some(start)
    })
}

fn is_tool_reference_boundary(value: Option<char>) -> bool {
    value
        .map(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .unwrap_or(true)
}

fn forced_task_tool_choice_for_round(
    required_tool_chain: &[String],
    tools: Option<&Vec<ChatToolDefinition>>,
    completed_required_tool_names: &[String],
) -> Option<ChatToolChoice> {
    let forced_tool_name = required_tool_chain
        .iter()
        .find(|tool_name| {
            !completed_required_tool_names
                .iter()
                .any(|completed| completed == *tool_name)
        })?
        .trim();
    if forced_tool_name.is_empty() {
        return None;
    }
    let tool_is_available = tools
        .map(|definitions| {
            definitions
                .iter()
                .any(|definition| definition.function.name == forced_tool_name)
        })
        .unwrap_or(false);
    tool_is_available.then(|| ChatToolChoice::force_function(forced_tool_name))
}

fn record_completed_required_tools(
    completed: &mut Vec<String>,
    required_tool_chain: &[String],
    tool_call_names: &[String],
) {
    for tool_name in tool_call_names {
        if !required_tool_chain
            .iter()
            .any(|required| required == tool_name)
        {
            continue;
        }
        if !completed
            .iter()
            .any(|completed_name| completed_name == tool_name)
        {
            completed.push(tool_name.clone());
        }
    }
}

fn required_tool_chain_is_complete(required_tool_chain: &[String], completed: &[String]) -> bool {
    required_tool_chain.iter().all(|required| {
        completed
            .iter()
            .any(|completed_name| completed_name == required)
    })
}

fn required_tool_chain_recovery_prompt(
    required_tool_chain: &[String],
    completed: &[String],
) -> String {
    let missing = required_tool_chain
        .iter()
        .filter(|required| {
            !completed
                .iter()
                .any(|completed_name| completed_name == *required)
        })
        .cloned()
        .collect::<Vec<_>>();
    format!(
        "上一轮提前给出了文字回复，但当前 action 明确要求调用的内置工具链尚未完成。已完成：{}。仍需继续调用：{}。请继续调用下一个缺失工具，不要总结。",
        if completed.is_empty() {
            "无".to_string()
        } else {
            completed.join(", ")
        },
        missing.join(", ")
    )
}

fn tool_call_round_limit(required_tool_chain: &[String]) -> usize {
    BASE_TOOL_CALL_ROUNDS
        .max(required_tool_chain.len().saturating_add(2))
        .min(MAX_TOOL_CALL_ROUNDS)
}

fn canonical_tool_call_name(tool_name: &str) -> String {
    BuiltinToolName::from_str(tool_name.trim())
        .map(|tool| tool.as_str().to_string())
        .unwrap_or_else(|| tool_name.trim().to_string())
}

fn task_event_context(
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
) -> EventContext {
    EventContext {
        workspace_id: workspace_id.clone(),
        session_id: Some(session_id.clone()),
        mission_id: Some(task.mission_id.clone()),
        task_id: Some(task.task_id.clone()),
        ..EventContext::default()
    }
}

fn publish_task_llm_started(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    prompt: &str,
    task_context: &EventContext,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-llm-started-{}", UtcMillis::now().0)),
            "task.llm.started",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "prompt_length": prompt.len(),
            }),
        )
        .with_context(task_context.clone()),
    );
}

fn publish_stream_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
    accumulated_text: &str,
) {
    let visible_text = normalize_model_stream_preview_content(accumulated_text);
    if visible_text.trim().is_empty() {
        return;
    }
    if let Some(timeline_entry_id) = timeline_entry_id.filter(|_| turn_visibility.thread_visible) {
        session_store.upsert_timeline_entry(
            session_id.clone(),
            timeline_entry_id,
            TimelineEntryKind::AssistantMessage,
            &visible_text,
        );
    }
    let mut item = session_turn_item(
        "assistant_stream",
        "running",
        Some("生成回复".to_string()),
        Some(visible_text),
        Some(item_id.to_string()),
    );
    apply_task_turn_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn publish_task_thinking_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    last_sent_len: &std::cell::Cell<usize>,
    streamed_thinking: &std::cell::RefCell<String>,
    turn_visibility: &TaskTurnVisibility,
    accumulated_thinking: &str,
) {
    if accumulated_thinking.len() <= last_sent_len.get() {
        return;
    }
    last_sent_len.set(accumulated_thinking.len());
    {
        let mut thinking = streamed_thinking.borrow_mut();
        thinking.clear();
        thinking.push_str(accumulated_thinking);
    }
    upsert_task_thinking_turn_item(
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        item_id,
        turn_visibility,
        "running",
        accumulated_thinking,
    );
}

fn upsert_task_thinking_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    turn_visibility: &TaskTurnVisibility,
    status: &str,
    thinking: &str,
) {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut item = session_turn_item(
        "assistant_thinking",
        status,
        Some("模型思考".to_string()),
        Some(trimmed.to_string()),
        Some(item_id.to_string()),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn append_task_tool_call_started_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
) {
    let mut item = session_turn_item(
        "tool_call_started",
        "running",
        Some(tool_call.function.name.clone()),
        Some(format!("正在调用工具：{}", tool_call.function.name)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some("running".to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn upsert_task_tool_call_result_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some(status_label.to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    item.tool_result = Some(tool_result.to_string());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        item.tool_error = Some(tool_result.to_string());
    }
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn execute_task_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_calls: &[ChatToolCall],
) -> Vec<(String, ExecutionResultStatus)> {
    let parsed_arguments = tool_calls
        .iter()
        .map(|tool_call| {
            serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments).ok()
        })
        .collect::<Vec<_>>();
    let tool_inputs = tool_calls
        .iter()
        .zip(parsed_arguments.iter())
        .map(|(tool_call, arguments)| ToolConcurrencyInput {
            tool_name: tool_call.function.name.as_str(),
            arguments: arguments.as_ref(),
        })
        .collect::<Vec<_>>();
    let mut results = vec![None; tool_calls.len()];

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    results[tool_index] = Some(execute_task_tool_call(
                        event_bus,
                        tool_registry,
                        skill_runtime,
                        task,
                        session_id,
                        workspace_id,
                        workspace_root_path,
                        worker_id,
                        &tool_calls[tool_index],
                    ));
                }
            }
            ToolBatchKind::Concurrent => {
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            let tool_call = &tool_calls[tool_index];
                            (
                                tool_index,
                                scope.spawn(move || {
                                    execute_task_tool_call(
                                        event_bus,
                                        tool_registry,
                                        skill_runtime,
                                        task,
                                        session_id,
                                        workspace_id,
                                        workspace_root_path,
                                        worker_id,
                                        tool_call,
                                    )
                                }),
                            )
                        })
                        .collect::<Vec<_>>();

                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| {
                            (
                                serde_json::json!({
                                    "tool": tool_calls[tool_index].function.name,
                                    "status": "failed",
                                    "error": "任务工具执行线程异常"
                                })
                                .to_string(),
                                ExecutionResultStatus::Failed,
                            )
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(tool_index, result)| {
            result.unwrap_or_else(|| {
                (
                    serde_json::json!({
                        "tool": tool_calls[tool_index].function.name,
                        "status": "failed",
                        "error": "任务工具未产生执行结果"
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                )
            })
        })
        .collect()
}

fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(registry) = tool_registry else {
        return (
            serde_json::json!({ "error": "tool registry not available" }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-tool-invoked-{}", UtcMillis::now().0)),
            "task.tool.invoked",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "worker_id": worker_id.map(ToString::to_string),
                "tool_name": tool_call.function.name,
                "tool_call_id": tool_call.id,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );

    if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    if let Some(rejection) = task_policy_tool_rejection(
        task,
        &tool_call.function.name,
        &tool_call.function.arguments,
    ) {
        return (rejection, ExecutionResultStatus::Rejected);
    }

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            tool_kind: ToolKind::Builtin,
            input: tool_call.function.arguments.clone(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: workspace_root_path.cloned(),
        },
        &ToolExecutionPolicy::default(),
    );

    (output.payload, output.status)
}

fn task_policy_tool_rejection(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let policy = task.policy_snapshot.as_ref()?;
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    if policy.command_mode.eq_ignore_ascii_case("no_tools") {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
        ));
    }
    if policy.denied_tools.iter().any(|tool| {
        canonical_builtin_tool_name(tool).as_deref() == Some(canonical_tool_name.as_str())
    }) {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("任务策略已拒绝工具: {canonical_tool_name}"),
        ));
    }
    if !policy.allowed_tools.is_empty()
        && !policy.allowed_tools.iter().any(|tool| {
            canonical_builtin_tool_name(tool).as_deref() == Some(canonical_tool_name.as_str())
        })
    {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("任务策略未授权工具: {canonical_tool_name}"),
        ));
    }
    if policy.command_mode.eq_ignore_ascii_case("read_only") {
        if BuiltinToolName::from_str(canonical_tool_name.as_str())
            .is_some_and(|tool| tool.is_write_operation())
        {
            return Some(task_policy_rejection_payload(
                &canonical_tool_name,
                format!("只读任务不允许执行写入工具: {canonical_tool_name}"),
            ));
        }
        if canonical_tool_name == BuiltinToolName::ShellExec.as_str()
            && !shell_arguments_request_read_only(arguments)
        {
            return Some(task_policy_rejection_payload(
                &canonical_tool_name,
                "只读任务中的 shell_exec 必须显式声明 access_mode=read_only".to_string(),
            ));
        }
    }
    None
}

fn canonical_builtin_tool_name(tool_name: &str) -> Option<String> {
    BuiltinToolName::from_str(tool_name.trim()).map(|tool| tool.as_str().to_string())
}

fn shell_arguments_request_read_only(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
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
                .map(|mode| {
                    matches!(
                        mode.trim().to_ascii_lowercase().as_str(),
                        "read" | "read_only" | "readonly"
                    )
                })
        })
        .unwrap_or(false)
}

fn task_policy_rejection_payload(tool_name: &str, error: String) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "rejected",
        "error": error,
    })
    .to_string()
}

fn tool_call_record(tool_call: &ChatToolCall, result: &str) -> serde_json::Value {
    let status = infer_tool_call_status(result);
    serde_json::json!({
        "type": "tool_call",
        "content": format!("{}: {}", tool_call.function.name, summarize_tool_result(result)),
        "toolCall": {
            "id": tool_call.id,
            "name": tool_call.function.name,
            "arguments": serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .unwrap_or(serde_json::Value::String(tool_call.function.arguments.clone())),
            "status": status,
            "result": result,
        }
    })
}

fn append_task_final_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    final_content: &str,
    final_item_id: Option<&str>,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
) {
    let has_requested_final_item_id = final_item_id.is_some();
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        final_item_id.map(str::to_string),
    );
    apply_task_final_visibility(&mut final_item, task_store, task, turn_visibility);
    if let Some(timeline_entry_id) = timeline_entry_id {
        final_item.timeline_entry_id = Some(timeline_entry_id.to_string());
    }
    let final_item_id = final_item.item_id.clone();
    if has_requested_final_item_id {
        if let Some(published) = upsert_session_turn_item_with_task_store(
            session_store,
            session_id,
            final_item,
            Some(task_store),
        ) {
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
    } else if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        final_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    let root_task_completed = task_store
        .get_task(&task.root_task_id)
        .is_some_and(|root_task| root_task.status == TaskStatus::Completed);
    if turn_visibility.thread_visible && root_task_completed {
        let _ = session_store.update_current_turn_status(session_id, "completed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &final_item_id,
            Some(task_store),
        );
    }
}

fn append_task_error_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    error_text: &str,
    _streaming_entry_id: Option<&str>,
) {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
    );
    apply_task_turn_visibility(&mut error_item, task, turn_visibility);
    let error_item_id = error_item.item_id.clone();
    if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        error_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    if turn_visibility.thread_visible {
        let _ = session_store.update_current_turn_status(session_id, "failed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &error_item_id,
            Some(task_store),
        );
    }
}

fn build_output_content(
    mut tool_call_records: Vec<serde_json::Value>,
    final_content: String,
) -> String {
    if tool_call_records.is_empty() {
        return final_content;
    }
    tool_call_records.push(serde_json::json!({
        "type": "text",
        "content": final_content,
    }));
    serde_json::json!({ "blocks": tool_call_records }).to_string()
}

fn task_lease_is_current(task_store: &TaskStore, task_id: &TaskId, lease_id: &LeaseId) -> bool {
    task_store
        .get_active_lease(task_id)
        .is_some_and(|lease| lease.lease_id == *lease_id)
}

fn task_stream_item_id(task_id: &TaskId, round: usize, streaming_entry_id: Option<&str>) -> String {
    if round == 0 {
        return streaming_entry_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("turn-item-assistant-stream-{task_id}-{round}"));
    }
    format!("turn-item-assistant-stream-{task_id}-{round}")
}

fn task_is_thread_visible_turn_owner(
    session_store: &SessionStore,
    session_id: &SessionId,
    task_id: &TaskId,
) -> bool {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            turn.items.iter().any(|item| {
                item.task_id.as_ref() == Some(task_id)
                    && item.thread_visible
                    && item.kind == "assistant_phase"
            })
        })
}

fn current_turn_has_worker_lanes(session_store: &SessionStore, session_id: &SessionId) -> bool {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| !turn.worker_lanes.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, ChatToolFunction,
    };
    use magi_core::{MissionId, Task, TaskKind, TaskStatus, WorkerId};
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionTurn, ActiveExecutionTurnLane, CanonicalTurnItemKind,
        CanonicalTurnItemStatus, CanonicalTurnStatus, TimelineEntryKind,
    };
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    struct TaskToolBatchModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    struct FailingTaskModelBridgeClient;
    struct StaticTaskFinalModelBridgeClient {
        content: &'static str,
    }
    struct TaskToolFailureThenFinalModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    impl ModelBridgeClient for TaskToolBatchModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [
                        {
                            "id": "task-tool-shell-a",
                            "type": "function",
                            "function": {
                                "name": "shell_exec",
                                "arguments": serde_json::json!({
                                    "command": "printf a",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        },
                        {
                            "id": "task-tool-shell-b",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": serde_json::json!({
                                    "command": "printf b",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        }
                    ]
                })
            } else {
                let tool_message_ids = request
                    .messages
                    .as_ref()
                    .expect("工具响应轮次必须携带消息上下文")
                    .iter()
                    .filter(|message| message.role == "tool")
                    .map(|message| message.tool_call_id.as_deref())
                    .collect::<Vec<_>>();
                assert_eq!(
                    tool_message_ids,
                    vec![Some("task-tool-shell-a"), Some("task-tool-shell-b")]
                );
                serde_json::json!({
                    "content": "任务工具调用完成",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 0 {
                on_delta(&ModelStreamingDelta {
                    content: "任务工具调用完成".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for FailingTaskModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32099),
                message: "model bridge unavailable".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for StaticTaskFinalModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "content": self.content,
                    "finish_reason": "stop"
                })
                .to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: self.content.to_string(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for TaskToolFailureThenFinalModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "task-tool-failure",
                        "type": "function",
                        "function": {
                            "name": "missing_builtin_tool",
                            "arguments": "{}"
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "content": "FLOW_SHOULD_NOT_COMPLETE",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 0 {
                on_delta(&ModelStreamingDelta {
                    content: "FLOW_SHOULD_NOT_COMPLETE".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    #[test]
    fn execute_task_tool_call_rejects_internal_process_launch_surface() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task = make_task_loop_test_task("task-process-launch-rejected");
        let session_id = SessionId::new("session-process-launch-rejected");
        let workspace_id = Some(WorkspaceId::new("workspace-process-launch-rejected"));
        let worker_id = WorkerId::new("worker-process-launch-rejected");
        let call = ChatToolCall {
            id: "tool-call-process-launch".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "process_launch".to_string(),
                arguments: serde_json::json!({ "command": "sleep 60" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            Some(&worker_id),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "process_launch");
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["error"]
                .as_str()
                .expect("error should be string")
                .contains("shell_exec")
        );
    }

    #[test]
    fn execute_task_tool_call_rejects_write_tool_for_readonly_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let mut task = make_task_loop_test_task("task-readonly-write-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: vec!["file_read".to_string(), "shell_exec".to_string()],
            denied_tools: vec!["file_write".to_string()],
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "read_only".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-file-write-readonly".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_write".to_string(),
                arguments: serde_json::json!({
                    "path": "/tmp/readonly-policy.txt",
                    "content": "must-not-write"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task,
            &SessionId::new("session-readonly-write-policy"),
            &Some(WorkspaceId::new("workspace-readonly-write-policy")),
            None,
            Some(&WorkerId::new("worker-readonly-write-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "file_write");
        assert_eq!(parsed["status"], "rejected");
    }

    #[test]
    fn execute_task_tool_call_requires_readonly_shell_access_mode_for_readonly_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let mut task = make_task_loop_test_task("task-readonly-shell-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: vec!["shell_exec".to_string()],
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "read_only".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-shell-missing-access".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({ "command": "printf ok" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task,
            &SessionId::new("session-readonly-shell-policy"),
            &Some(WorkspaceId::new("workspace-readonly-shell-policy")),
            None,
            Some(&WorkerId::new("worker-readonly-shell-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("access_mode=read_only"));
    }

    #[test]
    fn execute_task_tool_call_rejects_every_tool_for_no_tool_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let mut task = make_task_loop_test_task("task-no-tool-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "no_tools".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-no-tool-file-read".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_read".to_string(),
                arguments: serde_json::json!({ "path": "/tmp/no-tool-policy.txt" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task,
            &SessionId::new("session-no-tool-policy"),
            &Some(WorkspaceId::new("workspace-no-tool-policy")),
            None,
            Some(&WorkerId::new("worker-no-tool-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("不允许调用工具"));
    }

    #[test]
    fn full_action_extracts_required_tool_chain_in_goal_order() {
        let mut task = make_task_loop_test_task("task-required-tool-chain");
        task.goal =
            "按顺序调用：1 shell_exec；2 file_mkdir；3 file_write；4 file_read；5 file_remove"
                .to_string();
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });

        assert_eq!(
            task_required_tool_chain(&task),
            vec![
                "shell_exec".to_string(),
                "file_mkdir".to_string(),
                "file_write".to_string(),
                "file_read".to_string(),
                "file_remove".to_string()
            ]
        );

        task.policy_snapshot.as_mut().expect("policy").command_mode = "read_only".to_string();
        assert!(
            task_required_tool_chain(&task).is_empty(),
            "只读阶段即使复述用户目标，也不能强制执行写工具链"
        );
    }

    #[test]
    fn task_tool_call_round_limit_keeps_final_round_after_explicit_chain() {
        let required_tool_chain = [
            "file_mkdir",
            "file_write",
            "file_read",
            "file_patch",
            "search_text",
            "shell_exec",
            "diff_preview",
            "diagram_render",
            "file_remove",
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

        assert!(
            tool_call_round_limit(&required_tool_chain) >= required_tool_chain.len() + 2,
            "显式工具链不能因为固定轮数耗尽而丢失最后的工具或总结轮"
        );
    }

    #[test]
    fn planning_no_tool_action_and_validation_are_deterministic() {
        let task_store = TaskStore::new();
        let mut planning = make_task_loop_test_task("task-planning-deterministic");
        planning.title = "梳理目标".to_string();
        planning.goal = "明确目标、边界和验收标准：<<<MAGI_DEEP_TASK_GOAL>>>\n执行指定工具链\n<<<END_MAGI_DEEP_TASK_GOAL>>>"
            .to_string();
        planning.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "no_tools".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let planning_content =
            deterministic_task_final_content(&planning, &task_store).expect("planning content");
        assert!(planning_content.contains("目标：执行指定工具链"));
        assert!(planning_content.contains("边界："));
        assert!(planning_content.contains("执行计划："));
        assert!(planning_content.contains("验收标准："));

        planning.output_refs = vec![planning_content];
        task_store.insert_task(planning);
        let mut validation = make_task_loop_test_task("task-planning-validation-deterministic");
        validation.kind = TaskKind::Validation;
        validation.title = "规划 验证".to_string();
        validation.goal =
            "验证 规划 阶段产出是否包含目标、边界、执行计划和验收标准；只验证规划文本完整性"
                .to_string();
        validation.dependency_ids = vec![TaskId::new("task-planning-deterministic")];
        let validation_content = deterministic_task_final_content(&validation, &task_store)
            .expect("planning validation content");

        assert!(validation_content.starts_with("通过。"));
    }

    #[test]
    fn execution_validation_uses_dependency_structured_output() {
        let task_store = TaskStore::new();
        let mut action = make_task_loop_test_task("task-execution-output");
        action.goal = "按顺序调用 file_mkdir、file_write、file_read、file_patch、search_text、shell_exec、diff_preview、diagram_render、file_remove"
            .to_string();
        action.output_refs = vec![
            serde_json::json!({
                "blocks": [
                    successful_tool_output_block("file_mkdir"),
                    successful_tool_output_block("file_write"),
                    successful_tool_output_block("file_read"),
                    successful_tool_output_block("file_patch"),
                    successful_tool_output_block("search_text"),
                    successful_tool_output_block("shell_exec"),
                    successful_tool_output_block("diff_preview"),
                    successful_tool_output_block("diagram_render"),
                    successful_tool_output_block("file_remove"),
                    {
                        "type": "text",
                        "content": "DEEP_TASK_DONE_TEST"
                    }
                ]
            })
            .to_string(),
        ];
        task_store.insert_task(action);

        let mut validation = make_task_loop_test_task("task-execution-validation");
        validation.kind = TaskKind::Validation;
        validation.title = "执行 验证".to_string();
        validation.goal = "验证 执行 阶段是否按用户目标完成实际执行和工具结果。".to_string();
        validation.dependency_ids = vec![TaskId::new("task-execution-output")];

        let validation_content = deterministic_task_final_content(&validation, &task_store)
            .expect("execution validation should be deterministic from dependency output");

        assert!(validation_content.starts_with("通过。"));
        assert!(validation_content.contains("file_remove"));
        assert!(!validation_result_rejects_delivery(&validation_content));
    }

    fn successful_tool_output_block(tool_name: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "tool_call",
            "content": format!("{tool_name}: ok"),
            "toolCall": {
                "id": format!("call-{tool_name}"),
                "name": tool_name,
                "arguments": {},
                "status": "success",
                "result": serde_json::json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "summary": "ok"
                }).to_string()
            }
        })
    }

    struct ConcurrentTaskToolProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    impl ConcurrentTaskToolProbe {
        fn new(delay: Duration) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                delay,
            }
        }

        fn max_active(&self) -> usize {
            self.max_active.load(Ordering::SeqCst)
        }

        fn record_active_call(&self) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(self.delay);
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ProbeTaskBuiltinTool {
        name: &'static str,
        probe: Arc<ConcurrentTaskToolProbe>,
    }

    impl ProbeTaskBuiltinTool {
        fn new(name: &'static str, probe: Arc<ConcurrentTaskToolProbe>) -> Self {
            Self { name, probe }
        }
    }

    impl BuiltinTool for ProbeTaskBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(&self, input: &str, _context: &ToolExecutionContext) -> String {
            self.probe.record_active_call();
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": format!("{} done", self.name),
                "input": input,
            })
            .to_string()
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    fn make_task_loop_test_task(task_id: &str) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-task-loop"),
            root_task_id: TaskId::new(task_id),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "验证 worker 工具并发".to_string(),
            goal: "同一轮只读 shell 工具需要并发执行，并保持消息顺序".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    fn run_static_task_final(task: &Task, content: &'static str) -> TaskOutcome {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let task_store = TaskStore::new();
        task_store.insert_task(task.clone());
        let worker_id = WorkerId::new(format!("worker-{}", task.task_id));
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "reviewer",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let client = StaticTaskFinalModelBridgeClient { content };
        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &SessionId::new(format!("session-{}", task.task_id)),
            workspace_id: &Some(WorkspaceId::new(format!("workspace-{}", task.task_id))),
            prompt: "请执行任务".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: Some(&worker_id),
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });
        outcome
    }

    #[test]
    fn validation_task_negative_final_marks_task_failed() {
        let mut task = make_task_loop_test_task("task-validation-negative-final");
        task.kind = TaskKind::Validation;

        let outcome = run_static_task_final(&task, "不通过。\n\n原因：缺少文件写入证据。");

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("验证未通过"));
                assert!(error.contains("缺少文件写入证据"));
            }
            other => panic!("validation negative final must fail task, got {other:?}"),
        }
    }

    #[test]
    fn action_task_negative_wording_does_not_fail_validation_gate() {
        let task = make_task_loop_test_task("task-action-negative-wording");

        let outcome = run_static_task_final(
            &task,
            "不通过这个词只是普通任务报告里的示例，不代表验证结论。",
        );

        match outcome {
            TaskOutcome::Completed { output_refs } => {
                assert_eq!(output_refs.len(), 1);
            }
            other => panic!("action task should not use validation wording gate, got {other:?}"),
        }
    }

    #[test]
    fn validation_gate_rejects_conclusion_negative_and_partial_pass() {
        assert!(validation_result_rejects_delivery(
            "结论：**不通过**。\n缺少关键证据。"
        ));
        assert!(validation_result_rejects_delivery(
            "已部分通过，完整验收未能确认后续步骤。"
        ));
        assert!(!validation_result_rejects_delivery(
            "通过。\n已核验 shell 输出、文件读取和删除结果。"
        ));
    }

    #[test]
    fn action_task_failed_tool_prevents_completed_final() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-failed-tool-final");
        let workspace_id = Some(WorkspaceId::new("workspace-task-failed-tool-final"));
        let task_store = TaskStore::new();
        let task = make_task_loop_test_task("task-failed-tool-final");
        task_store.insert_task(task.clone());
        let worker_id = WorkerId::new("worker-task-failed-tool-final");
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let client = TaskToolFailureThenFinalModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请调用一个失败工具后总结".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: Some(&worker_id),
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("工具执行失败"));
                assert!(error.contains("missing_builtin_tool"));
            }
            other => panic!("failed tool must fail action task, got {other:?}"),
        }
    }

    #[test]
    fn task_stream_item_id_reuses_main_timeline_streaming_entry_only_for_first_round() {
        let task_id = TaskId::new("task-stream-main");

        assert_eq!(
            task_stream_item_id(&task_id, 0, Some("timeline-streaming-task-stream-main")),
            "timeline-streaming-task-stream-main"
        );
        assert_eq!(
            task_stream_item_id(&task_id, 3, Some("timeline-streaming-task-stream-main")),
            "turn-item-assistant-stream-task-stream-main-3"
        );
    }

    #[test]
    fn task_stream_item_id_keeps_round_scope_without_main_streaming_entry() {
        let task_id = TaskId::new("task-stream-worker");

        assert_eq!(
            task_stream_item_id(&task_id, 2, None),
            "turn-item-assistant-stream-task-stream-worker-2"
        );
    }

    #[test]
    fn task_turn_visibility_does_not_promote_primary_role_to_worker_lane() {
        let task = make_task_loop_test_task("task-primary-role-only");

        let visibility = task_turn_visibility(&task, true, None, None, None, false);

        assert!(visibility.thread_visible);
        assert!(!visibility.worker_visible);
        assert_eq!(visibility.role_id.as_deref(), Some("integration-dev"));
        assert!(visibility.lane_id.is_none());
        assert!(visibility.lane_seq.is_none());
    }

    #[test]
    fn primary_deep_task_worker_details_move_to_sidechain() {
        let task = make_task_loop_test_task("task-primary-deep-sidechain");
        let worker_id = WorkerId::new("worker-primary-deep-sidechain");
        let visibility = task_turn_visibility(&task, true, None, None, Some(&worker_id), true);
        let mut tool_item = session_turn_item(
            "tool_call_started",
            "running",
            Some("shell_exec".to_string()),
            Some("正在调用工具：shell_exec".to_string()),
            Some("turn-item-primary-tool".to_string()),
        );

        apply_task_worker_detail_visibility(&mut tool_item, &task, &visibility);

        assert!(!tool_item.thread_visible);
        assert!(tool_item.worker_visible);
        assert_eq!(tool_item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(tool_item.role_id.as_deref(), Some("integration-dev"));
        assert_eq!(tool_item.source, "integration-dev");

        let mut final_item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("worker 输出".to_string()),
            Some("turn-item-primary-final".to_string()),
        );
        let task_store = TaskStore::new();
        task_store.insert_task(task.clone());
        apply_task_final_visibility(&mut final_item, &task_store, &task, &visibility);

        assert!(!final_item.thread_visible);
        assert!(final_item.worker_visible);
        assert_eq!(final_item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(final_item.role_id.as_deref(), Some("integration-dev"));
        assert_eq!(final_item.source, "integration-dev");
    }

    #[test]
    fn task_turn_visibility_uses_authoritative_worker_lane_from_plan() {
        let task = make_task_loop_test_task("task-worker-lane-order");
        let worker_id = WorkerId::new("worker-worker-lane-order");
        let visibility = task_turn_visibility(
            &task,
            false,
            Some("lane-task-worker-lane-order"),
            Some(3),
            Some(&worker_id),
            false,
        );
        let mut item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("worker 输出".to_string()),
            Some("turn-item-worker-final".to_string()),
        );

        apply_task_turn_visibility(&mut item, &task, &visibility);

        assert!(!item.thread_visible);
        assert!(item.worker_visible);
        assert_eq!(item.lane_id.as_deref(), Some("lane-task-worker-lane-order"));
        assert_eq!(item.lane_seq, Some(3));
        assert_eq!(item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(item.role_id.as_deref(), Some("integration-dev"));
    }

    #[test]
    fn task_final_turn_item_does_not_complete_turn_before_root_task_completes() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(16);
        let session_id = SessionId::new("session-task-final-root-running");
        session_store
            .create_session(session_id.clone(), "task final root running")
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-final-root-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("执行深度任务".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("turn should be stored");

        let task_store = TaskStore::new();
        let root_task_id = TaskId::new("task-root-final-root-running");
        let task_id = TaskId::new("task-action-final-root-running");
        let mut root_task = make_task_loop_test_task(root_task_id.as_str());
        root_task.kind = TaskKind::Objective;
        root_task.status = TaskStatus::Running;
        task_store.insert_task(root_task);
        let mut task = make_task_loop_test_task(task_id.as_str());
        task.root_task_id = root_task_id;
        task.status = TaskStatus::Completed;
        task_store.insert_task(task.clone());
        let visibility = task_turn_visibility(
            &task,
            true,
            None,
            None,
            Some(&WorkerId::new("worker-final-root-running")),
            false,
        );

        append_task_final_turn_item(
            &event_bus,
            &session_store,
            &task_store,
            &task,
            &session_id,
            &None,
            "primary action 已完成",
            Some("timeline-streaming-task-action-final-root-running"),
            Some("timeline-streaming-task-action-final-root-running"),
            &visibility,
        );

        let current_turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(current_turn.status, "running");
        assert!(current_turn.completed_at.is_none());
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| !matches!(entry.kind, TimelineEntryKind::AssistantMessage)),
            "root 未完成时不能写入 completed turn snapshot"
        );
    }

    #[test]
    fn task_llm_loop_model_failure_writes_failed_turn_item_and_canonical_turn() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-model-failure");
        let workspace_id = Some(WorkspaceId::new("workspace-task-model-failure"));
        let task_id = TaskId::new("task-model-failure");
        let worker_id = WorkerId::new("worker-task-model-failure");
        let streaming_entry_id = "timeline-streaming-task-model-failure";
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task model failure session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-model-failure".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证模型失败写回".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &FailingTaskModelBridgeClient,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请生成回复".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some(streaming_entry_id),
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: None,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("model bridge unavailable"));
            }
            other => panic!("model failure must fail the task loop, got {other:?}"),
        }

        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain inspectable");
        assert_eq!(turn.status, "failed");
        assert!(turn.completed_at.is_some());
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("assistant_error should be appended");
        assert!(error_item.thread_visible);
        assert_eq!(error_item.status, "failed");
        assert_eq!(error_item.task_id.as_ref(), Some(&task_id));
        assert!(
            error_item
                .content
                .as_deref()
                .is_some_and(|content| content.contains("model bridge unavailable"))
        );

        let canonical_turn = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-task-model-failure")
            .expect("failed canonical turn should be stored");
        assert_eq!(canonical_turn.status, CanonicalTurnStatus::Failed);
        assert!(canonical_turn.response_duration_ms.is_some());
        assert!(
            canonical_turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::AssistantText
                    && item.status == CanonicalTurnItemStatus::Failed
                    && item
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("model bridge unavailable"))
            }),
            "failed task loop must persist the visible failure as canonical assistant_text"
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != streaming_entry_id),
            "失败终态不能再写回 legacy completed snapshot"
        );

        let terminal_error_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == "session.turn.item"
                    && event.payload["item"]["kind"] == "assistant_error"
            })
            .expect("assistant_error item event should be published");
        assert_eq!(
            terminal_error_event.payload["current_turn"]["status"],
            serde_json::Value::String("failed".to_string())
        );
        assert!(
            terminal_error_event.payload["current_turn"]["response_duration_ms"].is_number(),
            "terminal error event must carry backend duration for live UI"
        );
    }

    #[test]
    fn task_llm_loop_read_only_shell_tools_execute_concurrently_and_preserve_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-tool-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-task-tool-batch"));
        let task_id = TaskId::new("task-tool-batch");
        let worker_id = WorkerId::new("worker-task-tool-batch");
        let lane_id = "lane-task-tool-batch".to_string();
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task tool batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-tool-batch".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证 worker 工具并发".to_string()),
                    items: Vec::new(),
                    worker_lanes: vec![ActiveExecutionTurnLane {
                        lane_id: lane_id.clone(),
                        lane_seq: 2,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        role_id: Some("integration-dev".to_string()),
                        title: "验证 worker 工具并发".to_string(),
                        is_primary: false,
                    }],
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");

        let probe = Arc::new(ConcurrentTaskToolProbe::new(Duration::from_millis(180)));
        let tool_event_bus = Arc::new(InMemoryEventBus::new(8));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::clone(&tool_event_bus),
        );
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell_exec",
            Arc::clone(&probe),
        )));
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell",
            Arc::clone(&probe),
        )));
        let client = TaskToolBatchModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: Some(&tool_registry),
            skill_runtime: None,
            task_store: &task_store,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行两个只读 shell 工具".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some("timeline-streaming-task-tool-batch"),
            worker_lane_id: Some(&lane_id),
            worker_lane_seq: Some(2),
            worker_id: Some(&worker_id),
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        assert!(
            probe.max_active() > 1,
            "task worker 中的多个只读 shell 工具调用必须并发执行"
        );
        let output_refs = match outcome {
            TaskOutcome::Completed { output_refs } => output_refs,
            other => panic!("task loop should complete, got {other:?}"),
        };
        let output: serde_json::Value =
            serde_json::from_str(&output_refs[0]).expect("output blocks json");
        assert_eq!(
            output["blocks"][0]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-a".to_string())
        );
        assert_eq!(
            output["blocks"][1]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-b".to_string())
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert!(
            turn.items.iter().all(|item| {
                item.worker_visible
                    && item.lane_id.as_deref() == Some(lane_id.as_str())
                    && item.lane_seq == Some(2)
            }),
            "worker 输出必须沿用执行计划中的 lane 归属与顺序"
        );
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (item.kind.as_str(), item.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("tool_call_result", Some("task-tool-shell-a")),
                ("tool_call_result", Some("task-tool-shell-b")),
                ("assistant_final", None),
            ]
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != "turn-item-assistant-stream-task-tool-batch-1"),
            "工具后的第二轮流式内容不能写成独立主线 timeline entry"
        );
        let tool_events = event_bus.snapshot().recent_events;
        let invoked_events = tool_events
            .iter()
            .filter(|event| event.event_type == "task.tool.invoked")
            .collect::<Vec<_>>();
        assert_eq!(invoked_events.len(), 2);
        assert!(
            invoked_events.iter().all(|event| event.payload["worker_id"]
                == serde_json::Value::String(worker_id.to_string())),
            "worker 工具事件必须携带执行 worker，供 worker tab 和 runtime 归属使用"
        );
        let runtime_tool_events = tool_event_bus.snapshot().recent_events;
        assert!(
            runtime_tool_events
                .iter()
                .filter(|event| {
                    event.event_type == "tool.invoked" || event.event_type == "tool.usage.recorded"
                })
                .all(|event| event.payload["worker_id"]
                    == serde_json::Value::String(worker_id.to_string())),
            "工具运行时事件也必须沿用同一个 worker 归属"
        );
    }
}
