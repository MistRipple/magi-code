use super::*;

pub(super) const AGENT_CONTEXT_ACCESS_LIMIT: usize = 8;

pub(super) fn context_tool_failure(
    tool: BuiltinToolName,
    error_code: &str,
    error: impl ToString,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "failed",
            "error_code": error_code,
            "error": error.to_string(),
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

#[derive(Clone)]
pub(super) struct ResolvedAgentContext {
    pub(super) reference_id: String,
    pub(super) kind: AgentContextReferenceKind,
    pub(super) title: String,
    pub(super) content: String,
}

pub(super) fn execute_agent_context_tool(
    task_store: &TaskStore,
    session_store: &SessionStore,
    conversation_registry: &ConversationRegistry,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    arguments: &str,
) -> (String, ExecutionResultStatus) {
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return context_tool_failure(tool, "invalid_arguments", error),
    };
    match tool {
        BuiltinToolName::ContextSearch => {
            execute_context_search(task_store, session_store, task, session_id, tool, &parsed)
        }
        BuiltinToolName::ContextRead => {
            execute_context_read(task_store, session_store, task, session_id, tool, &parsed)
        }
        BuiltinToolName::ContextRequest => execute_context_request(
            task_store,
            conversation_registry,
            task,
            session_id,
            tool,
            &parsed,
        ),
        _ => unreachable!("execute_agent_context_tool 只接收上下文工具"),
    }
}

pub(super) fn execute_context_search(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let query =
        match required_bounded_context_text(parsed.get("query"), "context_search.query", 1_000) {
            Ok(query) => query,
            Err(error) => return context_tool_failure(tool, "invalid_query", error),
        };
    let limit = parsed
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20) as usize;
    let mut candidates =
        match collect_context_candidates(task_store, session_store, task, session_id) {
            Ok(candidates) => candidates,
            Err(error) => return context_tool_failure(tool, "context_history_unavailable", error),
        };
    candidates.retain(|candidate| context_candidate_matches(&query, candidate));
    candidates.sort_by(|left, right| {
        context_candidate_score(&query, right)
            .cmp(&context_candidate_score(&query, left))
            .then_with(|| left.reference_id.cmp(&right.reference_id))
    });
    candidates.truncate(limit);
    let estimated_tokens = candidates
        .iter()
        .map(|candidate| estimate_text_tokens(&candidate.content))
        .sum();
    let reference_ids = candidates
        .iter()
        .map(|candidate| candidate.reference_id.clone())
        .collect::<Vec<_>>();
    if let Err(error) = record_agent_context_access(
        task_store,
        task,
        AgentContextAccessOperation::Search,
        Some(query.clone()),
        reference_ids,
        estimated_tokens,
    ) {
        return context_tool_failure(tool, "context_audit_failed", error);
    }
    let results = candidates
        .into_iter()
        .map(|candidate| {
            serde_json::json!({
                "reference_id": candidate.reference_id,
                "kind": candidate.kind,
                "title": candidate.title,
                "preview": compact_context_preview(&candidate.content),
                "estimated_tokens": estimate_text_tokens(&candidate.content),
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "ok",
            "query": query,
            "results": results,
            "estimated_tokens": estimated_tokens,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

pub(super) fn execute_context_read(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let Some(reference_values) = parsed
        .get("reference_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return context_tool_failure(
            tool,
            "invalid_references",
            "context_read.reference_ids 必须是非空数组",
        );
    };
    if reference_values.is_empty() || reference_values.len() > AGENT_CONTEXT_ACCESS_LIMIT {
        return context_tool_failure(
            tool,
            "invalid_references",
            format!("context_read 单次必须读取 1-{AGENT_CONTEXT_ACCESS_LIMIT} 条引用"),
        );
    }
    let mut requested_ids = Vec::with_capacity(reference_values.len());
    for value in reference_values {
        let Some(reference_id) = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return context_tool_failure(
                tool,
                "invalid_references",
                "reference_ids 只能包含非空字符串",
            );
        };
        if !requested_ids
            .iter()
            .any(|existing| existing == reference_id)
        {
            requested_ids.push(reference_id.to_string());
        }
    }
    let candidates = match collect_context_candidates(task_store, session_store, task, session_id) {
        Ok(candidates) => candidates,
        Err(error) => return context_tool_failure(tool, "context_history_unavailable", error),
    };
    let mut results = Vec::with_capacity(requested_ids.len());
    for reference_id in &requested_ids {
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| &candidate.reference_id == reference_id)
        else {
            return context_tool_failure(
                tool,
                "reference_out_of_scope",
                format!("引用 {reference_id} 不属于当前 session 或执行链"),
            );
        };
        results.push(candidate.clone());
    }
    let estimated_tokens = results
        .iter()
        .map(|candidate| estimate_text_tokens(&candidate.content))
        .sum();
    if let Err(error) = record_agent_context_access(
        task_store,
        task,
        AgentContextAccessOperation::Read,
        None,
        requested_ids.clone(),
        estimated_tokens,
    ) {
        return context_tool_failure(tool, "context_audit_failed", error);
    }
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "ok",
            "results": results.into_iter().map(|candidate| serde_json::json!({
                "reference_id": candidate.reference_id,
                "kind": candidate.kind,
                "title": candidate.title,
                "content": candidate.content,
                "estimated_tokens": estimate_text_tokens(&candidate.content),
            })).collect::<Vec<_>>(),
            "estimated_tokens": estimated_tokens,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

pub(super) fn is_parent_context_reply(signal: &RuntimeSignal, parent_task_id: &TaskId) -> bool {
    matches!(&signal.author, MailboxAuthor::Parent(id) if id == parent_task_id.as_str())
        && signal
            .payload
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("agent_context_supplement")
}

pub(super) fn execute_context_request(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let request = match required_bounded_context_text(
        parsed.get("request"),
        "context_request.request",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    ) {
        Ok(request) => request,
        Err(error) => return context_tool_failure(tool, "invalid_request", error),
    };
    let Some(package) = task.agent_context_package() else {
        return context_tool_failure(
            tool,
            "context_package_missing",
            "当前任务没有 AgentContextPackage",
        );
    };
    let parent_task_id = package.parent_task_id.clone();
    let Some(parent_task) = task_store.get_task(&parent_task_id) else {
        return context_tool_failure(tool, "parent_not_found", "父任务不存在");
    };
    if parent_task.root_task_id != task.root_task_id
        || parent_task.mission_id != task.mission_id
        || !matches!(
            parent_task.status,
            TaskStatus::Pending | TaskStatus::Running
        )
    {
        return context_tool_failure(tool, "parent_not_running", "父任务不在当前活跃执行链中");
    }
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let estimated_tokens = estimate_text_tokens(&request);
    let access = AgentContextAccessRecord {
        record_id: format!(
            "ctxaccess-request-{}-{}-{sequence}",
            task.task_id.as_str(),
            now.0
        ),
        operation: AgentContextAccessOperation::Request,
        query: Some(request.clone()),
        reference_ids: Vec::new(),
        estimated_tokens,
        occurred_at: now,
    };
    let commit =
        conversation_registry.enqueue_task_signal_with(session_id, &parent_task_id, || {
            task_store.append_agent_context_access(&task.task_id, access.clone())?;
            Ok::<_, magi_core::DomainError>((
                (),
                RuntimeSignal {
                    author: MailboxAuthor::Child(task.task_id.to_string()),
                    kind: MailboxKind::Message,
                    trigger_turn: true,
                    payload: serde_json::json!({
                        "type": "agent_context_request",
                        "child_task_id": task.task_id,
                        "package_id": package.package_id,
                        "revision": package.revision,
                        "request": request,
                    }),
                    enqueued_at: now,
                },
            ))
        });
    match commit {
        Ok(()) => {
            task_store.notify_runtime_change();
            let timeout_ms = parsed
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(AGENT_WAIT_DEFAULT_TIMEOUT_MS)
                .clamp(AGENT_WAIT_MIN_TIMEOUT_MS, AGENT_WAIT_MAX_TIMEOUT_MS);
            let started_at = std::time::Instant::now();
            loop {
                let elapsed_ms =
                    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed_ms >= timeout_ms {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "timeout",
                            "parent_task_id": parent_task_id,
                            "estimated_tokens": estimated_tokens,
                            "instruction": "父任务尚未回复。只在现有证据足以完成目标时继续；否则在最终答复中明确说明缺失上下文。",
                        })
                        .to_string(),
                        ExecutionResultStatus::Succeeded,
                    );
                }
                if task_store.get_task(&task.task_id).is_none_or(|current| {
                    !matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                }) {
                    return context_tool_failure(
                        tool,
                        "task_interrupted",
                        "等待上下文回复时任务已中断",
                    );
                }
                let wait_ms = (timeout_ms - elapsed_ms).min(250);
                // 只有直接父任务的补充才是这次请求的答复；孙代理的请求、审批通知等
                // 留在通道里，由任务循环或 agent_wait 按原路径处理。
                let Some(responses) = conversation_registry.wait_for_matching_task_signals(
                    session_id,
                    &task.task_id,
                    std::time::Duration::from_millis(wait_ms),
                    |signal| is_parent_context_reply(signal, &parent_task_id),
                ) else {
                    return context_tool_failure(
                        tool,
                        "task_interrupted",
                        "等待上下文回复时任务输入已关闭",
                    );
                };
                if !responses.is_empty() {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "resolved",
                            "parent_task_id": parent_task_id,
                            "responses": responses,
                            "estimated_tokens": estimated_tokens,
                            "instruction": "父任务已回复。请把 responses 中的 agent_context_supplement 作为当前任务补充事实继续执行。",
                        })
                        .to_string(),
                        ExecutionResultStatus::Succeeded,
                    );
                }
                // 已投递的补充先交付；没有答复且父任务已退出时不再等到工具超时。
                if task_store.get_task(&parent_task_id).is_none_or(|parent| {
                    !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
                }) {
                    return context_tool_failure(
                        tool,
                        "parent_not_running",
                        "等待上下文回复时父任务已结束",
                    );
                }
            }
        }
        Err(crate::TaskSignalCommitError::ChannelClosed) => context_tool_failure(
            tool,
            "parent_channel_closed",
            "父任务当前无法接收上下文请求",
        ),
        Err(crate::TaskSignalCommitError::Commit(error)) => {
            context_tool_failure(tool, "context_audit_failed", error)
        }
    }
}

/// 子代理 context_read 能够解析的引用 ID（会话消息、同一执行链的任务输出与证据）。
pub(crate) fn context_reference_ids(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
) -> magi_core::DomainResult<std::collections::HashSet<String>> {
    Ok(
        collect_context_candidates(task_store, session_store, task, session_id)?
            .into_iter()
            .map(|candidate| candidate.reference_id)
            .collect(),
    )
}

pub(super) fn collect_context_candidates(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
) -> magi_core::DomainResult<Vec<ResolvedAgentContext>> {
    let mut candidates = session_store
        .timeline_for_session(session_id)
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
            )
        })
        .filter_map(|entry| {
            let content = timeline_entry_visible_text(&entry.message)?;
            let title = match entry.kind {
                TimelineEntryKind::UserMessage => "用户消息",
                TimelineEntryKind::AssistantMessage => "助手消息",
                _ => return None,
            };
            Some(ResolvedAgentContext {
                reference_id: format!("turn:{}", entry.entry_id),
                kind: AgentContextReferenceKind::ConversationTurn,
                title: title.to_string(),
                content,
            })
        })
        .collect::<Vec<_>>();
    let previous_activations =
        crate::agent_activation::previous_activation_tasks(task_store, task, session_id)
            .map_err(|message| magi_core::DomainError::InvalidState { message })?
            .into_iter()
            .map(|task| task.task_id)
            .collect::<Vec<_>>();
    for candidate_task in task_store.all_tasks().into_iter().filter(|candidate| {
        (candidate.root_task_id == task.root_task_id && candidate.mission_id == task.mission_id)
            || previous_activations.contains(&candidate.task_id)
    }) {
        for (index, content) in candidate_task.output_refs.iter().enumerate() {
            candidates.push(ResolvedAgentContext {
                reference_id: format!("task:{}:output:{index}", candidate_task.task_id.as_str()),
                kind: AgentContextReferenceKind::TaskOutput,
                title: format!("{} · 输出 {}", candidate_task.title, index + 1),
                content: content.clone(),
            });
        }
        for (index, content) in candidate_task.evidence_refs.iter().enumerate() {
            candidates.push(ResolvedAgentContext {
                reference_id: format!("task:{}:evidence:{index}", candidate_task.task_id.as_str()),
                kind: AgentContextReferenceKind::TaskEvidence,
                title: format!("{} · 证据 {}", candidate_task.title, index + 1),
                content: content.clone(),
            });
        }
    }
    if !previous_activations.is_empty() {
        for thread in session_store.thread_registry_snapshot(session_id) {
            for previous in &previous_activations {
                if !thread.handled_task_ids.contains(previous) {
                    continue;
                }
                // Thread 仍归原 activation；按引用读取的 transcript 不成为新调用授权。
                let snapshot = session_store.thread_context_snapshot(&thread.thread_id)?;
                let history = snapshot.messages().iter().map(|message| serde_json::json!({
                    "role":message.role, "content":message.content, "tool_calls":message.tool_calls,
                    "tool_call_id":message.tool_call_id,
                })).collect::<Vec<_>>();
                candidates.push(ResolvedAgentContext {
                    reference_id: format!("task:{previous}:transcript"),
                    kind: AgentContextReferenceKind::Other,
                    title: "Agent 前序执行记录（历史参考）".into(),
                    content: serde_json::to_string(&history).expect("thread projection serializes"),
                });
            }
        }
    }
    if let Some(package) = task.agent_context_package() {
        for reference in package.references.iter().chain(
            package
                .supplements
                .iter()
                .flat_map(|supplement| supplement.references.iter()),
        ) {
            let content = resolve_package_reference_content(&candidates, reference)
                .unwrap_or_else(|| reference.preview.clone());
            candidates.push(ResolvedAgentContext {
                reference_id: reference.reference_id.clone(),
                kind: reference.kind,
                title: reference.title.clone(),
                content,
            });
        }
        for supplement in &package.supplements {
            candidates.push(ResolvedAgentContext {
                reference_id: supplement.supplement_id.clone(),
                kind: AgentContextReferenceKind::Other,
                title: "主线补充上下文".to_string(),
                content: supplement.message.clone(),
            });
        }
    }
    Ok(candidates)
}

pub(super) fn resolve_package_reference_content(
    candidates: &[ResolvedAgentContext],
    reference: &AgentContextReference,
) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| candidate.reference_id == reference.source_ref)
        .map(|candidate| candidate.content.clone())
}

pub(super) fn context_candidate_matches(query: &str, candidate: &ResolvedAgentContext) -> bool {
    context_candidate_score(query, candidate) > 0
}

pub(super) fn context_candidate_score(query: &str, candidate: &ResolvedAgentContext) -> usize {
    let query = query.to_lowercase();
    let haystack = format!("{} {}", candidate.title, candidate.content).to_lowercase();
    if haystack.contains(&query) {
        return 100 + query.chars().count();
    }
    // 以非字母数字字符（含中文标点）切词；中文没有空格，整句词项再退化为相邻二字组匹配，
    // 命中至少一半二字组才算相关，避免单个常用字组造成噪声。
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| {
            if haystack.contains(term) {
                return term.chars().count();
            }
            if term.is_ascii() {
                return 0;
            }
            let characters = term.chars().collect::<Vec<_>>();
            let total = characters.len().saturating_sub(1);
            let matched = characters
                .windows(2)
                .filter(|pair| haystack.contains(pair.iter().collect::<String>().as_str()))
                .count();
            if total > 0 && matched * 2 >= total {
                matched * 2
            } else {
                0
            }
        })
        .sum()
}

pub(super) fn compact_context_preview(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= AGENT_CONTEXT_PREVIEW_MAX_CHARS {
        compact
    } else {
        compact
            .chars()
            .take(AGENT_CONTEXT_PREVIEW_MAX_CHARS)
            .collect()
    }
}

pub(super) fn record_agent_context_access(
    task_store: &TaskStore,
    task: &magi_core::Task,
    operation: AgentContextAccessOperation,
    query: Option<String>,
    reference_ids: Vec<String>,
    estimated_tokens: usize,
) -> Result<(), magi_core::DomainError> {
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    task_store.append_agent_context_access(
        &task.task_id,
        AgentContextAccessRecord {
            record_id: format!("ctxaccess-{}-{}-{sequence}", task.task_id.as_str(), now.0),
            operation,
            query,
            reference_ids,
            estimated_tokens,
            occurred_at: now,
        },
    )
}
