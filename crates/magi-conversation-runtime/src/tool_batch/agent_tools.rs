use super::*;

/// agent_spawn 生成 child task_id 时使用的进程内单调序号。
///
/// 仅靠 `UtcMillis::now()` 在同一毫秒内的多次并行 agent_spawn 会产生重复
/// child_id，进而触发 TaskStore 的重复插入。配合毫秒时间戳一起拼到 task_id
/// 末尾，保证同一进程内绝对唯一。
pub(super) static AGENT_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) const AGENT_SPAWN_STARTED_INSTRUCTION: &str = "代理已异步启动。若后续结论依赖该代理结果，必须调用 agent_wait，并传入 task_ids=[child_task_id] 收集终态结果；不要在未等待必要代理结果时直接给最终答复。";

pub(super) const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 300_000;

pub(super) const AGENT_WAIT_MIN_TIMEOUT_MS: u64 = 1_000;

pub(super) const AGENT_WAIT_MAX_TIMEOUT_MS: u64 = 1_800_000;

/// `agent_spawn` 在创建前失败时仍要返回可被模型、前端和通知中心共同消费的诊断合同。
///
/// 这里不携带底层错误链或配置内容；完整原因仅写入 tracing。`diagnostic_ref` 用于把
/// 用户可见结果关联回本次工具调用，而不是伪造一个不存在的子代理任务。
pub(super) fn agent_spawn_failure_payload(
    tool_call: &ChatToolCall,
    status: &str,
    error_code: &str,
    failure_stage: &str,
    error: impl Into<String>,
    instruction: impl Into<String>,
) -> String {
    agent_spawn_failure_payload_for_existing_task(
        tool_call,
        status,
        error_code,
        failure_stage,
        error,
        instruction,
        None,
    )
}

pub(super) fn agent_spawn_failure_payload_for_existing_task(
    tool_call: &ChatToolCall,
    status: &str,
    error_code: &str,
    failure_stage: &str,
    error: impl Into<String>,
    instruction: impl Into<String>,
    existing_task_id: Option<&TaskId>,
) -> String {
    // retryable 只说明可按诊断修正输入/注册冲突，不构成再次派发的决定。
    let retryable =
        existing_task_id.is_none() && matches!(failure_stage, "input_validation" | "registration");
    let instruction = format!(
        "{} 具体诊断：{}",
        magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE,
        instruction.into()
    );
    let mut payload = serde_json::json!({
        "tool": BuiltinToolName::AgentSpawn.as_str(),
        "status": status,
        "error_code": error_code,
        "failure_stage": failure_stage,
        "retryable": retryable,
        "error": error.into(),
        "instruction": instruction,
        "diagnostic_ref": format!("tool_call:{}", tool_call.id),
        "child_task_id": null,
    });
    if let Some(existing_task_id) = existing_task_id {
        // 这是已存在任务的引用，不能伪装成本次拒绝调用创建的新 child。
        payload["existing_task_id"] = serde_json::json!(existing_task_id);
    }
    payload.to_string()
}

pub(super) fn execute_agent_spawn(
    context: CoordinatorToolContext<'_>,
    tool_call: &ChatToolCall,
    parsed: &serde_json::Value,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let CoordinatorToolContext {
        event_bus,
        agent_role_registry,
        task_store,
        session_store,
        execution_registry,
        conversation_registry,
        plan_store,
        task,
        session_id,
        workspace_id,
        workspace_root_path,
        ..
    } = context;

    // 预检只读事实；直到它成功后才生成 child_task_id 或写入任何任务状态。
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let preflight = match preflight_agent_spawn(AgentSpawnPreflightInput {
        parsed,
        tool_call_id: &tool_call.id,
        parent_task: task,
        task_store,
        session_store,
        execution_registry,
        agent_role_registry,
        plan_store,
        session_id,
        workspace_id,
        execution_root: workspace_root_path,
        now,
        sequence,
    }) {
        Ok(preflight) => preflight,
        Err(error) => {
            tracing::info!(
                error_code = %error.error_code,
                failure_stage = %error.failure_stage,
                parent_task_id = %task.task_id,
                "agent_spawn 创建前预检拒绝"
            );
            return (
                agent_spawn_failure_payload_for_existing_task(
                    tool_call,
                    "rejected",
                    &error.error_code,
                    &error.failure_stage,
                    error.message,
                    error.instruction,
                    error.existing_task_id.as_ref(),
                ),
                ExecutionResultStatus::Rejected,
            );
        }
    };

    let child_task_id = TaskId::new(format!(
        "task-spawn-{}-{}-{}",
        task.task_id.as_str(),
        now.0,
        sequence
    ));
    let activation = magi_core::AgentActivation {
        agent_id: preflight
            .previous_activation
            .as_ref()
            .map(|source| source.agent_id.clone())
            .unwrap_or_else(|| child_task_id.clone()),
        session_id: session_id.clone(),
        lifetime: preflight.agent_lifetime.clone(),
        generation: match preflight.previous_activation.as_ref() {
            Some(source) => match source.generation.checked_add(1) {
                Some(value) => value,
                None => {
                    return context_tool_failure(
                        BuiltinToolName::AgentSpawn,
                        "agent_activation_overflow",
                        "Agent activation generation 溢出",
                    );
                }
            },
            None => 1,
        },
        previous_task_id: preflight.previous_task_id.clone(),
    };
    let context_package_id = preflight.context_package.package_id.clone();
    let context_revision = preflight.context_package.revision;
    let context_mode = if preflight.context_package.fork.is_some() {
        "fork"
    } else {
        "fresh"
    };
    let context_source = preflight.context_package.fork.as_ref().map(|snapshot| {
        serde_json::json!({
            "task_id": snapshot.source_task_id, "thread_id": snapshot.source_thread_id,
            "turn_id": snapshot.source_turn_id, "step_id": snapshot.source_step_id,
            "before_item_id": snapshot.before_item_id, "item_count": snapshot.items.len(),
        })
    });
    let plan_item_id = preflight.plan_item_id.clone();
    let child = magi_core::Task {
        task_id: child_task_id.clone(),
        mission_id: task.mission_id.clone(),
        root_task_id: task.root_task_id.clone(),
        parent_task_id: Some(task.task_id.clone()),
        kind: magi_core::TaskKind::LocalAgent,
        title: preflight.display_name.clone(),
        goal: preflight.goal.clone(),
        status: TaskStatus::Pending,
        dependency_ids: preflight.child_dependency_ids.clone(),
        required_children: Vec::new(),
        policy_snapshot: Some(preflight.child_policy_snapshot.clone()),
        executor_binding: Some(
            TaskExecutorBinding::for_role(&preflight.role)
                .with_capability_ids(preflight.capability_ids.clone())
                .with_parallelism_group(preflight.parallelism_group.clone())
                .with_canonical_task_name(preflight.canonical_task_name.clone())
                .with_plan_item_id(plan_item_id.clone()),
        ),
        completion_contract: magi_core::TaskCompletionContract::default(),
        recovery_checkpoint: None,
        knowledge_refs: Vec::new(),
        workspace_scope: task.workspace_scope.clone(),
        write_scope: task.write_scope.clone(),
        input_refs: preflight.child_input_refs.clone(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: TaskRuntimePayload::AgentContext {
            package: Box::new(preflight.context_package.clone()),
            accesses: Vec::new(),
        },
        created_at: now,
        updated_at: now,
    };

    let mut child = child;
    child
        .executor_binding
        .as_mut()
        .expect("child executor binding")
        .project_directories = task
        .executor_binding
        .as_ref()
        .and_then(|binding| binding.project_directories.clone());
    child
        .executor_binding
        .as_mut()
        .expect("spawn binding")
        .model_selection = preflight.model_selection.clone();
    child
        .executor_binding
        .as_mut()
        .expect("spawn role binding")
        .agent_activation = Some(activation.clone());
    child
        .executor_binding
        .as_mut()
        .expect("spawn role binding")
        .delegation_allowed = preflight.delegation_allowed;
    // 初始消息必须早于 TaskStore 的可调度发布，和后续 agent_send 共用同一 FIFO。
    conversation_registry.open_task_signal_channel(session_id, &child_task_id);
    if let Err(error) = enqueue_agent_assignment_message(
        conversation_registry,
        session_id,
        task,
        &child,
        &preflight.role,
        now,
    ) {
        conversation_registry.close_task_signal_channel(session_id, &child_task_id);
        return context_tool_failure(BuiltinToolName::AgentSpawn, "agent_input_closed", error);
    }
    let registered_execution = match execution_registry.register_spawned_local_agent_child(
        SpawnedChildExecutionRequest {
            task_store,
            session_store,
            child_task: &child,
            session_id,
            workspace_id,
            role: &preflight.role,
            delegation_purpose: preflight.delegation.purpose,
            role_parallelism_limit: agent_role_registry
                .get(&preflight.role)
                .and_then(|definition| definition.parallelism_limit),
            plan_store: preflight.plan_item_id.as_ref().map(|_| plan_store),
            plan_item_id: plan_item_id.clone(),
            execution_root: preflight
                .working_dir
                .clone()
                .or_else(|| workspace_root_path.cloned()),
            inherit_skill: preflight.inherit_skill,
            now,
        },
    ) {
        Ok(registered) => registered,
        Err(error) => {
            conversation_registry.close_task_signal_channel(session_id, &child_task_id);
            tracing::warn!(
                error = %error,
                parent_task_id = %task.task_id,
                child_task_id = %child_task_id,
                "agent_spawn 子任务原子注册失败"
            );
            let (error_code, failure_stage, status, execution_status, message, instruction) =
                match error {
                    SpawnedChildExecutionError::GoalPaused(message) => (
                        "agent_goal_paused",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        message,
                        "由用户显式恢复当前 Goal 后再创建新的子代理。",
                    ),
                    SpawnedChildExecutionError::RoleCapacityExceeded {
                        role,
                        active,
                        limit,
                    } => (
                        "agent_capacity_exceeded",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        format!(
                            "角色 {role} 已达到代理实例上限：最多 {limit} 个活跃实例，当前 {active} 个"
                        ),
                        "请先用 agent_wait 收集该角色已启动代理的结果；有实例退出活跃状态后再继续创建同角色代理。",
                    ),
                    SpawnedChildExecutionError::SharedWorkspaceWriterActive => (
                        "agent_workspace_busy",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        SpawnedChildExecutionError::SharedWorkspaceWriterActive.to_string(),
                        "本次未创建代理。请用 agent_wait 等待已有代理结束后再派发；用户明确要求多个真实代理时可以按序完成，不能用主线自述代替。角色的只读职责不等于只读授权。",
                    ),
                    SpawnedChildExecutionError::DuplicateTaskName { existing_task_id } => {
                        return (
                            agent_spawn_failure_payload_for_existing_task(
                                tool_call,
                                "rejected",
                                "duplicate_task_name",
                                "registration",
                                "同一父任务下 task_name 已存在",
                                AGENT_SPAWN_DUPLICATE_INSTRUCTION,
                                Some(&existing_task_id),
                            ),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    SpawnedChildExecutionError::DuplicateAgentWork { existing_task_id } => {
                        return (
                            agent_spawn_failure_payload_for_existing_task(
                                tool_call,
                                "rejected",
                                "duplicate_agent_work",
                                "registration",
                                "相同工作包已有活跃代理，本次未创建新代理",
                                AGENT_SPAWN_DUPLICATE_INSTRUCTION,
                                Some(&existing_task_id),
                            ),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    SpawnedChildExecutionError::InvalidState(_) => (
                        "agent_spawn_registration_failed",
                        "registration",
                        "failed",
                        ExecutionResultStatus::Failed,
                        "代理启动失败".to_string(),
                        "使用本次诊断引用定位工具调用，核对当前执行链和注册状态；不要重复相同调用。",
                    ),
                };
            return (
                agent_spawn_failure_payload(
                    tool_call,
                    status,
                    error_code,
                    failure_stage,
                    message,
                    instruction,
                ),
                execution_status,
            );
        }
    };

    if let Some(plan) = registered_execution.plan.as_ref() {
        magi_plan::publish_plan_event(
            event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            workspace_id.as_ref(),
            Some(&child_task_id),
            Some(&task.mission_id),
        );
    }
    publish_event(
        "task.coordinator.agent_spawn",
        serde_json::json!({
            "delegation": preflight.delegation,
            "parent_task_id": task.task_id.to_string(),
            "child_task_id": child_task_id.to_string(),
            "agent_id": activation.agent_id,
            "activation": activation,
            "task_name": preflight.task_name.clone(),
            "canonical_task_name": preflight.canonical_task_name,
            "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
            "role": preflight.role,
            "capabilities": preflight.capability_ids,
            "access_profile": preflight.child_access_profile.as_str(),
            "goal": preflight.goal,
            "context_package_id": context_package_id,
            "context_revision": context_revision,
            "context_mode": context_mode,
            "context_source": context_source,
            "task_kind": "local_agent",
            "worker_id": registered_execution.worker_id.to_string(),
            "thread_id": registered_execution.thread_id.to_string(),
            "execution_chain_ref": registered_execution.execution_chain_ref,
            "model_source": preflight.model_source,
            "model_selection": preflight.model_selection,
            "queue_reason": registered_execution.queue_reason,
        }),
    );
    let queue_reason = registered_execution.queue_reason.clone();
    let status = if queue_reason.is_some() {
        "queued"
    } else {
        "started"
    };
    let instruction = queue_reason
        .as_ref()
        .map(|_| "代理已创建并排队等待执行资源。资源可用后会自动启动；若后续结论依赖该代理，必须调用 agent_wait 收集终态结果。".to_string())
        .unwrap_or_else(|| AGENT_SPAWN_STARTED_INSTRUCTION.to_string());
    (
        serde_json::json!({
            "tool": BuiltinToolName::AgentSpawn.as_str(),
            "status": status,
            "delegation": preflight.delegation,
            "child_task_id": child_task_id.to_string(),
            "agent_id": activation.agent_id,
            "activation": activation,
            "task_name": preflight.task_name,
            "canonical_task_name": preflight.canonical_task_name,
            "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
            "role": preflight.role,
            "capabilities": preflight.capability_ids,
            "access_profile": preflight.child_access_profile.as_str(),
            "title": child.title,
            "assignment": {
                "title": child.title,
                "goal": child.goal,
                "role": preflight.role,
                "capabilities": preflight.capability_ids,
                "access_profile": preflight.child_access_profile.as_str(),
                "context_package_id": context_package_id,
                "context_revision": context_revision,
                "context_mode": context_mode,
                "context_source": context_source,
            },
            "worker_id": registered_execution.worker_id.to_string(),
            "thread_id": registered_execution.thread_id.to_string(),
            "execution_chain_ref": registered_execution.execution_chain_ref,
            "model_selection": preflight.model_selection,
            "model_source": preflight.model_source.unwrap_or_else(|| "unconfigured".to_string()),
            "queue_reason": queue_reason,
            "instruction": instruction,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

pub(super) fn execute_agent_send(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let Some(object) = parsed.as_object() else {
        return context_tool_failure(tool, "invalid_arguments", "agent_send 参数必须是 JSON 对象");
    };
    for key in object.keys() {
        if !matches!(key.as_str(), "task_id" | "message" | "references") {
            return context_tool_failure(
                tool,
                "invalid_arguments",
                format!("agent_send 不支持字段 {key}"),
            );
        }
    }
    let target_task_id = match object
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(task_id) => TaskId::new(task_id),
        None => {
            return context_tool_failure(
                tool,
                "invalid_arguments",
                "agent_send.task_id 必须是非空字符串",
            );
        }
    };
    let message = match required_bounded_context_text(
        parsed.get("message"),
        "agent_send.message",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    ) {
        Ok(message) => message,
        Err(error) => return context_tool_failure(tool, "invalid_arguments", error),
    };
    let Some(target_task) = task_store.get_task(&target_task_id) else {
        return context_tool_failure(tool, "target_not_found", "目标代理任务不存在");
    };
    if target_task.parent_task_id.as_ref() != Some(&parent_task.task_id)
        || target_task.root_task_id != parent_task.root_task_id
        || target_task.mission_id != parent_task.mission_id
        || target_task.workspace_scope != parent_task.workspace_scope
        || target_task.write_scope != parent_task.write_scope
    {
        return context_tool_failure(
            tool,
            "target_out_of_scope",
            "agent_send 只能向当前任务直接派发且作用域一致的子代理发送上下文",
        );
    }
    if !matches!(
        target_task.status,
        TaskStatus::Pending | TaskStatus::Running
    ) {
        return context_tool_failure(
            tool,
            "target_terminal",
            "目标代理已进入终态，不能继续发送上下文",
        );
    }
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let references =
        match parse_agent_context_references(parsed.get("references"), now, sequence, "agent_send")
        {
            Ok(references) => references,
            Err(error) => return context_tool_failure(tool, "invalid_references", error),
        };
    let estimated_tokens = estimate_text_tokens(&message)
        + references
            .iter()
            .map(|reference| reference.estimated_tokens)
            .sum::<usize>();
    let supplement = AgentContextSupplement {
        supplement_id: format!("ctxsupp-{}-{}-{sequence}", target_task_id.as_str(), now.0),
        author_task_id: parent_task.task_id.clone(),
        message: message.clone(),
        references: references.clone(),
        estimated_tokens,
        created_at: now,
    };
    let access = AgentContextAccessRecord {
        record_id: format!(
            "ctxaccess-send-{}-{}-{sequence}",
            target_task_id.as_str(),
            now.0
        ),
        operation: AgentContextAccessOperation::Send,
        query: Some(message.clone()),
        reference_ids: references
            .iter()
            .map(|reference| reference.reference_id.clone())
            .collect(),
        estimated_tokens,
        occurred_at: now,
    };
    deliver_agent_send(
        task_store,
        conversation_registry,
        parent_task,
        session_id,
        tool,
        &target_task_id,
        target_task.status,
        supplement,
        access,
        estimated_tokens,
        publish_event,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn deliver_agent_send(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    target_task_id: &TaskId,
    observed_status: TaskStatus,
    supplement: AgentContextSupplement,
    access: AgentContextAccessRecord,
    estimated_tokens: usize,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let now = supplement.created_at;
    // 排队中的代理尚未开始：补充写入其上下文包即可，启动时随上下文包一起渲染进提示词，
    // 不依赖只在运行期存在的信号通道。
    if observed_status == TaskStatus::Pending {
        let committed = task_store.deliver_agent_context_supplement(
            target_task_id,
            TaskStatus::Pending,
            supplement.clone(),
            access.clone(),
        );
        let package = match committed {
            Ok(package) => Some(package),
            // 启动事务可能已在读取状态后提交。原 Pending 写入未发生，沿运行中
            // FIFO 入口提交同一个 supplement；不能把正常启动竞态报成写盘失败。
            Err(magi_core::DomainError::InvalidState { .. })
                if task_store
                    .get_task(target_task_id)
                    .is_some_and(|task| task.status == TaskStatus::Running) =>
            {
                None
            }
            Err(error) => {
                return context_tool_failure(tool, "context_persist_failed", error.to_string());
            }
        };
        if let Some(package) = package {
            task_store.notify_runtime_change();
            publish_event(
                "task.coordinator.agent_send",
                serde_json::json!({
                    "parent_task_id": parent_task.task_id,
                    "child_task_id": target_task_id,
                    "package_id": package.package_id,
                    "revision": package.revision,
                    "estimated_tokens": estimated_tokens,
                }),
            );
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "sent",
                    "task_id": target_task_id.to_string(),
                    "package_id": package.package_id,
                    "revision": package.revision,
                    "estimated_tokens": estimated_tokens,
                    "delivery": "on_start",
                    "instruction": "代理尚在排队，补充上下文会在它开始执行时一并提供。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            );
        }
    }
    let commit = conversation_registry.enqueue_task_signal_with(session_id, target_task_id, || {
        let package = task_store.deliver_agent_context_supplement(
            target_task_id,
            TaskStatus::Running,
            supplement.clone(),
            access.clone(),
        )?;
        let signal = RuntimeSignal {
            author: MailboxAuthor::Parent(parent_task.task_id.to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({
                "type": "agent_context_supplement",
                "package_id": package.package_id,
                "revision": package.revision,
                "supplement": supplement,
            }),
            enqueued_at: now,
        };
        Ok::<_, magi_core::DomainError>((package, signal))
    });
    let package = match commit {
        Ok(package) => package,
        Err(crate::TaskSignalCommitError::ChannelClosed) => {
            return context_tool_failure(
                tool,
                "target_not_running",
                "目标代理尚未启动或已结束，无法接收运行中上下文",
            );
        }
        Err(crate::TaskSignalCommitError::Commit(error)) => {
            return context_tool_failure(tool, "context_persist_failed", error.to_string());
        }
    };
    // agent_wait 等待 TaskStore 的变化通知；FIFO 就绪通知只唤醒 context_request。
    // 补充和运行中投递都已提交后，才能唤醒正在等待其子代理的接收方。
    task_store.notify_runtime_change();
    publish_event(
        "task.coordinator.agent_send",
        serde_json::json!({
            "parent_task_id": parent_task.task_id,
            "child_task_id": target_task_id,
            "package_id": package.package_id,
            "revision": package.revision,
            "estimated_tokens": estimated_tokens,
        }),
    );
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "sent",
            "task_id": target_task_id,
            "package_id": package.package_id,
            "revision": package.revision,
            "estimated_tokens": estimated_tokens,
            "delivery": "next_model_round",
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

#[cfg(test)]
pub(super) fn default_agent_spawn_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile: magi_core::AccessProfile::Restricted,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        read_only_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: true,
        escalation_conditions: Vec::new(),
    }
}

pub(super) fn enqueue_agent_assignment_message(
    conversation_registry: &ConversationRegistry,
    session_id: &SessionId,
    parent: &magi_core::Task,
    child: &magi_core::Task,
    role: &str,
    now: UtcMillis,
) -> Result<(), String> {
    let access_profile = child
        .policy_snapshot
        .as_ref()
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default()
        .as_str();
    conversation_registry.enqueue_task_signal(session_id, &child.task_id, RuntimeSignal {
            author: MailboxAuthor::Parent(parent.task_id.to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({
                "type": "agent_assignment",
                "parent_task_id": parent.task_id.to_string(),
                "child_task_id": child.task_id.to_string(),
                "title": child.title,
                "goal": child.goal,
                "role": role,
                "access_profile": access_profile,
                "dependency_ids": child.dependency_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "context_package_id": child.agent_context_package().map(|package| package.package_id.as_str()),
                "context_revision": child.agent_context_package().map(|package| package.revision),
            }),
            enqueued_at: now,
        })
}

pub(super) struct AgentWaitRuntime<'a> {
    pub(super) task_store: &'a TaskStore,
    pub(super) session_store: &'a SessionStore,
    pub(super) admission: &'a crate::execution_admission::ExecutionAdmissionController,
    pub(super) conversation_registry: &'a ConversationRegistry,
    pub(super) session_id: &'a SessionId,
    pub(super) execution_allowed: &'a (dyn Fn() -> bool + Sync),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentWaitMode {
    /// 全部代理进入终态才返回。
    All,
    /// 任一代理进入终态即返回。
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentWaitRequest {
    pub(super) task_ids: Vec<TaskId>,
    pub(super) timeout_ms: u64,
    pub(super) mode: AgentWaitMode,
}

pub(super) fn execute_agent_wait_with_runtime(
    runtime: AgentWaitRuntime<'_>,
    parent_task: &magi_core::Task,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let AgentWaitRuntime {
        task_store,
        session_store,
        admission,
        conversation_registry,
        session_id,
        execution_allowed,
    } = runtime;
    let request = match parse_agent_wait_request(parsed) {
        Ok(request) => request,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "rejected",
                    "error_code": "invalid_arguments",
                    "failure_stage": "input_validation",
                    "child_task_id": serde_json::Value::Null,
                    "error": error,
                    "instruction": "请传入不含未知字段的对象；task_ids 必须是至少一个不重复的 child_task_id，timeout_ms 可省略或使用 1000-1800000 的整数，mode 可省略或为 all/any。",
                })
                .to_string(),
                ExecutionResultStatus::Rejected,
            );
        }
    };
    let task_ids = request.task_ids;
    if let Some(task_id) = task_ids
        .iter()
        .find(|task_id| !agent_wait_task_is_direct_child(task_store, parent_task, task_id))
    {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error_code": "scope_mismatch",
                "failure_stage": "scope_validation",
                "child_task_id": task_id.to_string(),
                "error": "agent_wait 只能等待当前任务派发的代理",
                "instruction": "请使用最近一次 agent_spawn 返回的 child_task_id，且只能等待当前任务直接派发的代理。",
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
        );
    }
    let started_at = std::time::Instant::now();
    let mut observed_status_version = task_store.status_change_version();
    // 等待子代理期间让出执行名额，避免与自己派发的子代理争抢同一批名额。
    let waiting = admission.suspend_while_waiting(&parent_task.task_id);
    let parent_is_active = || {
        execution_allowed()
            && task_store
                .get_task(&parent_task.task_id)
                .is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
    };
    loop {
        if !execution_allowed()
            || waiting.is_some() && !parent_is_active()
            || task_store
                .get_task(&parent_task.task_id)
                .is_some_and(|task| {
                    !matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
        {
            return (
                serde_json::json!({"tool":tool.as_str(),"status":"cancelled",
                "error_code":"agent_wait_cancelled","execution_started":false})
                .to_string(),
                ExecutionResultStatus::Cancelled,
            );
        }
        let runtime_signals =
            conversation_registry.drain_task_signals(session_id, &parent_task.task_id);
        let mut receipts = Vec::new();
        let mut pending_task_ids = Vec::new();
        for task_id in &task_ids {
            match task_store.get_task(task_id).and_then(|child| {
                crate::agent_receipt::AgentReceipt::from_terminal_task_in_session(
                    &child,
                    session_store,
                    session_id,
                )
            }) {
                Some(receipt) => receipts.push(receipt),
                None => pending_task_ids.push(task_id.to_string()),
            }
        }
        let condition_met = match request.mode {
            AgentWaitMode::All => pending_task_ids.is_empty(),
            AgentWaitMode::Any => !receipts.is_empty(),
        };
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let timed_out = !condition_met && elapsed_ms >= request.timeout_ms;
        if runtime_signals.is_empty() && !condition_met && !timed_out {
            observed_status_version = task_store.wait_for_status_change_since(
                observed_status_version,
                std::time::Duration::from_millis(request.timeout_ms - elapsed_ms),
            );
            continue;
        }
        let count = |child_status: &str| {
            receipts
                .iter()
                .filter(|receipt| receipt.child_status() == child_status)
                .count()
        };
        let (completed, failed, killed) = (count("completed"), count("failed"), count("killed"));
        let (status, instruction) = if !runtime_signals.is_empty() {
            (
                "attention_required",
                "代理等待期间收到运行时消息。若为 agent_context_request，请先读取 child_task_id 和 request，使用 agent_send 回复对应代理；若为审批等待，告知用户需要处理的审批；若来自 parent:* 的 agent_context_supplement，是你的父任务补充的事实与约束，按当前任务补充处理，不能扩大目标范围或权限。之后对 pending_task_ids 继续 agent_wait。",
            )
        } else if timed_out {
            (
                "timed_out",
                "仍有代理未结束。results 中已结束代理的结果已经收集；可以先处理不依赖未结束代理的工作，需要时再对 pending_task_ids 调用 agent_wait，或用 agent_cancel 取消不再需要的代理。",
            )
        } else if pending_task_ids.is_empty() {
            (
                "completed",
                "对照每个代理的 assignment.goal 阅读 result.final_text 与 activity（实际执行的命令、退出码和改动文件），合并结论、证据、风险与缺口。",
            )
        } else {
            (
                "completed",
                "已有代理结束，results 中的结果已经收集。处理后对 pending_task_ids 继续 agent_wait。",
            )
        };
        let instruction = if failed > 0 || killed > 0 {
            format!(
                "{instruction} {}",
                magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE
            )
        } else {
            instruction.to_string()
        };
        let mut payload = serde_json::json!({
            "tool": tool.as_str(),
            "status": status,
            "mode": match request.mode {
                AgentWaitMode::All => "all",
                AgentWaitMode::Any => "any",
            },
            "timed_out": timed_out,
            "all_terminal": pending_task_ids.is_empty(),
            "completed_count": completed,
            "failed_count": failed,
            "killed_count": killed,
            "pending_task_ids": pending_task_ids,
            "results": crate::agent_receipt::render_receipts(
                receipts,
                crate::agent_receipt::AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES,
            ),
            // agent_wait 已按代理分配最终答复的展示预算，模型视图不再做通用截断。
            "model_budgeted": true,
            "instruction": instruction,
        });
        if !runtime_signals.is_empty() {
            payload["runtime_signals"] = serde_json::json!(runtime_signals);
        }
        if waiting
            .as_ref()
            .is_some_and(|guard| !guard.resume(parent_is_active))
        {
            return (
                serde_json::json!({"tool":tool.as_str(),"status":"cancelled",
                "error_code":"agent_wait_cancelled","execution_started":false})
                .to_string(),
                ExecutionResultStatus::Cancelled,
            );
        }
        return (payload.to_string(), ExecutionResultStatus::Succeeded);
    }
}

#[cfg(test)]
pub(super) fn execute_agent_wait(
    task_store: &TaskStore,
    parent_task: &magi_core::Task,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-wait-test");
    registry.open_task_signal_channel(&session_id, &parent_task.task_id);
    execute_agent_wait_with_runtime(
        AgentWaitRuntime {
            session_store: &SessionStore::new(),
            task_store,
            admission: &crate::execution_admission::ExecutionAdmissionController::default(),
            conversation_registry: &registry,
            session_id: &session_id,
            execution_allowed: &|| true,
        },
        parent_task,
        tool,
        parsed,
    )
}

/// 主线取消自己直接派发的代理：级联终止该代理子树。进程、浏览器租约等执行资源
/// 由 TaskStore 终态回调统一释放，这里不另走一条清理路径。
pub(super) fn execute_agent_cancel(
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    session_store: &SessionStore,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let rejected = |error_code: &str, error: String, instruction: &str| {
        (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error_code": error_code,
                "error": error,
                "instruction": instruction,
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
        )
    };
    let non_empty = |key: &str| {
        parsed
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let (Some(task_id), Some(reason)) = (non_empty("task_id"), non_empty("reason")) else {
        return rejected(
            "invalid_arguments",
            "agent_cancel 需要非空的 task_id 和 reason".to_string(),
            "请传入 agent_spawn 返回的 child_task_id，并说明取消原因。",
        );
    };
    let child_task_id = TaskId::new(task_id);
    if !agent_wait_task_is_direct_child(task_store, parent_task, &child_task_id) {
        return rejected(
            "scope_mismatch",
            format!("{child_task_id} 不是当前任务直接派发的代理"),
            "只能取消当前任务通过 agent_spawn 直接派发的代理。",
        );
    }
    let changed = match crate::task_runner::kill_subtree_in_store(
        task_store,
        &execution_registry.execution_admission(),
        &child_task_id,
        vec![format!("主线取消：{reason}")],
    ) {
        Ok(changed) => changed,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error_code": "cancel_failed",
                    "child_task_id": child_task_id.to_string(),
                    "error": error,
                    "instruction": "取消未完全成功；可稍后用 agent_wait 查看该代理状态。",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    // TaskStore 终态只撤销执行资格；线程、进程及 worktree 仍可能正在退出。
    // 父任务本身也在执行，不能等待整个 root，否则会等待当前调用而死锁。
    let task_ids = task_store.collect_subtree_ids(&child_task_id);
    let settlement = execution_registry.settle_agent_subtree(session_id, &task_ids);
    // 取消与完成可能竞争。没有改变状态不等于主线已经读过产物，不能把
    // already_terminal 当作无结果的取消成功，也不能引导模型重新派发同一工作。
    let receipt = task_store.get_task(&child_task_id).and_then(|child| {
        crate::agent_receipt::AgentReceipt::from_terminal_task_in_session(
            &child,
            session_store,
            session_id,
        )
    });
    let Some(receipt) = receipt else {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error_code": "cancel_unsettled",
                "child_task_id": child_task_id,
                "instruction": "取消后未能核对代理终态；先用 agent_wait 核对原任务，不得假定已停止或重复执行其工作。",
            }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let child_status = receipt.child_status().to_string();
    let resources_settled = matches!(settlement, Ok(true));
    let settlement_error = settlement.err();
    let instruction = if !resources_settled {
        "Task 已进入终态，但尚未确认执行资源退出。不得开始与该子树冲突的替代工作或宣称资源已释放；先核对原产物及结算诊断。"
    } else if changed {
        "取消已结算且执行资源已退出，results 包含终态回执；取消不保证此前没有副作用。"
    } else {
        "代理在取消生效前已进入终态，原结果未被覆盖。先读取 results 中的 assignment.goal、result.final_text、activity 和 workspace，核对已有产物，不要把 already_terminal 当成任务尚未执行。"
    };
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": if settlement_error.is_some() { "failed" } else if changed { "cancelled" } else { "already_terminal" },
            "cancellation_status": if changed { "cancelled" } else { "already_terminal" },
            "error_code": settlement_error.as_ref().map(|_| "cancel_unsettled"),
            "child_task_id": child_task_id.to_string(),
            "child_status": child_status,
            "reason": reason,
            "resources_settled": resources_settled,
            "settlement_error": settlement_error,
            "results": crate::agent_receipt::render_receipts(
                vec![receipt],
                crate::agent_receipt::AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES,
            ),
            "model_budgeted": true,
            "instruction": format!("{instruction} {}", magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE),
        })
        .to_string(),
        if settlement_error.is_none() {
            ExecutionResultStatus::Succeeded
        } else {
            ExecutionResultStatus::Failed
        },
    )
}

pub(super) fn agent_wait_task_is_direct_child(
    task_store: &TaskStore,
    parent_task: &magi_core::Task,
    child_task_id: &TaskId,
) -> bool {
    task_store.get_task(child_task_id).is_some_and(|child| {
        child.parent_task_id.as_ref() == Some(&parent_task.task_id)
            && agent_wait_child_execution_scope_matches(parent_task, &child)
    })
}

pub(super) fn agent_wait_child_execution_scope_matches(
    parent_task: &magi_core::Task,
    child: &magi_core::Task,
) -> bool {
    child.mission_id == parent_task.mission_id
        && child.root_task_id == parent_task.root_task_id
        && child.workspace_scope == parent_task.workspace_scope
        && child.write_scope == parent_task.write_scope
}

pub(super) fn parse_agent_wait_request(
    parsed: &serde_json::Value,
) -> Result<AgentWaitRequest, String> {
    let object = parsed
        .as_object()
        .ok_or_else(|| "agent_wait 参数必须是 JSON 对象".to_string())?;
    for key in object.keys() {
        if !matches!(key.as_str(), "task_ids" | "timeout_ms" | "mode") {
            return Err(format!("agent_wait 不支持字段 {key}"));
        }
    }
    let values = object
        .get("task_ids")
        .ok_or_else(|| "agent_wait 缺少必需字段 task_ids".to_string())?
        .as_array()
        .ok_or_else(|| "agent_wait task_ids 必须是数组".to_string())?;
    if values.is_empty() {
        return Err("agent_wait task_ids 至少需要一个任务 ID".to_string());
    }
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    let mut task_ids = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let task_id = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("agent_wait task_ids[{index}] 必须是非空字符串"))?;
        if !seen.insert(task_id.to_string()) {
            return Err(format!("agent_wait task_ids 不能重复: {task_id}"));
        }
        task_ids.push(TaskId::new(task_id));
    }
    let timeout_ms = match object.get("timeout_ms") {
        None => AGENT_WAIT_DEFAULT_TIMEOUT_MS,
        Some(value) => {
            let timeout_ms = value
                .as_u64()
                .ok_or_else(|| "agent_wait timeout_ms 必须是整数".to_string())?;
            if !(AGENT_WAIT_MIN_TIMEOUT_MS..=AGENT_WAIT_MAX_TIMEOUT_MS).contains(&timeout_ms) {
                return Err(format!(
                    "agent_wait timeout_ms 必须在 {AGENT_WAIT_MIN_TIMEOUT_MS}-{AGENT_WAIT_MAX_TIMEOUT_MS} 之间"
                ));
            }
            timeout_ms
        }
    };
    let mode = match object.get("mode") {
        None => AgentWaitMode::All,
        Some(value) => match value.as_str() {
            Some("all") => AgentWaitMode::All,
            Some("any") => AgentWaitMode::Any,
            _ => return Err("agent_wait mode 只能是 all 或 any".to_string()),
        },
    };
    Ok(AgentWaitRequest {
        task_ids,
        timeout_ms,
        mode,
    })
}
