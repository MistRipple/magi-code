use super::*;
use magi_core::{ExecutionOwnership, SessionId, ThreadId, UtcMillis, WorkspaceId};
use magi_event_bus::RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY;
use magi_event_bus::SessionRuntimeUsageObservation;

#[test]
fn runtime_activity_uses_task_scope_and_clears_stale_event_identities() {
    use magi_core::{
        AgentActivation, AgentLifetime, AgentRunOwner, AgentRunTransfer, Task, TaskExecutorBinding,
        TaskKind, WorkerId,
    };
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    };
    let store = TaskStore::new();
    let session = SessionId::new("scope-session");
    let root_id = TaskId::new("scope-root");
    let now = UtcMillis::now();
    let root = Task {
        task_id: root_id.clone(),
        root_task_id: root_id.clone(),
        mission_id: magi_core::MissionId::new("shared-mission"),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: "root".into(),
        goal: "scope".into(),
        status: TaskStatus::Running,
        dependency_ids: vec![],
        required_children: vec![],
        policy_snapshot: None,
        executor_binding: Some(TaskExecutorBinding::for_role("coordinator")),
        completion_contract: Default::default(),
        recovery_checkpoint: None,
        knowledge_refs: vec![],
        workspace_scope: None,
        write_scope: None,
        input_refs: vec![],
        output_refs: vec![],
        evidence_refs: vec![],
        retry_count: 0,
        runtime_payload: Default::default(),
        created_at: now,
        updated_at: now,
    };
    store.insert_task(root.clone()).unwrap();
    let mut branches = vec![];
    for (id, status) in [
        ("scope-child", TaskStatus::Pending),
        ("scope-done", TaskStatus::Completed),
        ("scope-failed", TaskStatus::Failed),
        ("scope-killed", TaskStatus::Killed),
    ] {
        let mut binding = TaskExecutorBinding::for_role("executor")
            .with_capability_ids(vec!["general_engineering".into()]);
        binding.agent_activation = Some(AgentActivation {
            agent_id: TaskId::new(id),
            session_id: session.clone(),
            lifetime: AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        store
            .insert_task(Task {
                task_id: TaskId::new(id),
                parent_task_id: Some(root_id.clone()),
                status,
                executor_binding: Some(binding),
                ..root.clone()
            })
            .unwrap();
        branches.push(ActiveExecutionBranch {
            task_id: TaskId::new(id),
            worker_id: WorkerId::new(id),
            use_tools: true,
            skill_name: None,
            is_primary: false,
            thread_id: ThreadId::new(id),
        });
    }
    let export = SessionRuntimeSidecarExport {
        session_id: session.clone(),
        current_status: SessionExecutionSidecarStatus::Resumed,
        last_update: now,
        ownership: ExecutionOwnership {
            session_id: Some(session.clone()),
            task_id: Some(root_id.clone()),
            ..Default::default()
        },
        execution_chain_ref: Some("scope-chain".into()),
        recovery_ref: None,
        resume_candidate: None,
        current_turn: None,
        active_execution_chain: Some(ActiveExecutionChain {
            session_id: session.clone(),
            mission_id: root.mission_id.clone(),
            root_task_id: root_id.clone(),
            execution_chain_ref: "scope-chain".into(),
            workspace_id: None,
            branches,
            current_turn_id: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: now,
                entry_id: "scope-entry".into(),
                trimmed_text: None,
                skill_name: None,
            },
        }),
    };
    let mut events = RuntimeReadModelInput::default();
    let ghosts = vec![
        root_id.to_string(),
        "scope-child".into(),
        "scope-done".into(),
        "scope-failed".into(),
        "scope-killed".into(),
        "missing-task".into(),
    ];
    events.overview.activity.active_task_ids = ghosts.clone();
    events.overview.activity.active_execution_group_ids =
        vec![root.mission_id.to_string(), "missing-mission".into()];
    events.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: session.to_string(),
        active_task_ids: ghosts.clone(),
        ..Default::default()
    });
    events.details.tasks = ghosts
        .iter()
        .map(|id| magi_event_bus::TaskRuntimeSummaryEntry {
            task_id: id.clone(),
            current_status: Some("running".into()),
            ..Default::default()
        })
        .collect();
    let project = |authority, sidecar: &SessionRuntimeSidecarExport| {
        runtime_read_model_dto(
            events.clone(),
            std::slice::from_ref(sidecar),
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            authority,
        )
    };
    let initial = project(Some(&store), &export);
    assert_eq!(
        initial.details.sessions[0].active_task_ids,
        vec!["scope-child", "scope-root"]
    );
    assert_eq!(
        initial.details.sessions[0].active_branches.len(),
        4,
        "终态仍保留审计展示"
    );
    let encoded = serde_json::to_value(&initial.details.sessions[0].active_branches).unwrap();
    for branch in encoded.as_array().unwrap() {
        for retired in [
            "execution_intent_ref",
            "binding_lifecycle",
            "checkpoint_stage",
            "checkpoint_at",
            "next_step_index",
            "resume_mode",
        ] {
            assert!(branch.get(retired).is_none(), "不再发送旧检查点字段");
        }
    }
    store
        .transfer_agent_subtree(
            &root_id,
            &TaskId::new("scope-child"),
            AgentRunTransfer {
                operation_id: "transfer-scope".into(),
                session_id: session.clone(),
                source_turn_id: "scope-turn".into(),
                transferred_by: root_id.clone(),
                activation_generation: 1,
                owner: AgentRunOwner::Session,
                transferred_at: now,
            },
        )
        .unwrap();
    let transferred = project(Some(&store), &export);
    assert_eq!(
        transferred.details.sessions[0].active_task_ids,
        vec!["scope-root"]
    );
    assert!(
        !transferred.details.sessions[0]
            .active_branches
            .iter()
            .any(|branch| branch.task_id == "scope-child")
    );
    assert!(
        transferred
            .overview
            .activity
            .active_task_ids
            .contains(&"scope-child".to_string()),
        "独立代理仍在全局活动范围"
    );
    store
        .revoke_active_lease_and_set_task_terminal(&root_id, &root_id, TaskStatus::Failed, vec![])
        .unwrap();
    for _ in 0..2 {
        let stopped = project(Some(&store), &export);
        assert!(stopped.details.sessions[0].active_task_ids.is_empty());
        assert!(
            stopped.details.execution_groups[0]
                .active_task_ids
                .is_empty()
        );
        assert_eq!(
            stopped.details.sessions[0].root_task_status.as_deref(),
            Some("failed")
        );
        assert!(
            !stopped
                .overview
                .activity
                .active_task_ids
                .contains(&"missing-task".into())
        );
    }
    let unavailable = project(None, &export);
    assert!(unavailable.details.sessions[0].active_task_ids.is_empty());
    assert!(unavailable.details.sessions[0].active_branches.is_empty());
    assert!(unavailable.overview.activity.active_task_ids.is_empty());
    let mut missing_root = export.clone();
    missing_root
        .active_execution_chain
        .as_mut()
        .unwrap()
        .root_task_id = TaskId::new("missing-root");
    let missing = project(Some(&store), &missing_root);
    assert!(missing.details.sessions[0].active_task_ids.is_empty());
    assert!(missing.details.sessions[0].active_branches.is_empty());
}

#[test]
fn runtime_read_model_merges_sidecars_and_ledger_summary() {
    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-1"),
            current_status: SessionExecutionSidecarStatus::Resumed,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                mission_id: Some(magi_core::MissionId::new("mission-1")),
                task_id: Some(magi_core::TaskId::new("todo-1")),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-1".to_string()),
            recovery_ref: Some("recovery-1".to_string()),
            current_turn: None,
            active_execution_chain: None,
        }],
        &[WorkspaceRecoverySidecarExport {
            recovery_ref: "recovery-1".to_string(),
            workspace_id: WorkspaceId::new("workspace-1"),
            current_status: RecoveryStatus::Ready,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                mission_id: Some(magi_core::MissionId::new("mission-1")),
                task_id: Some(magi_core::TaskId::new("todo-1")),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-1".to_string()),
            snapshot_id: "snapshot-1".to_string(),
            diagnostic_summary: Some("resume".to_string()),
            consumed_at: None,
        }],
        ledger_dto(AuditUsageLedgerStatus {
            schema_version: "audit-usage-ledger-v1".to_string(),
            next_sequence: 12,
            audit_count: 5,
            usage_count: 7,
            persistence_path: None,
            last_persist_error: None,
        }),
        None,
    );

    assert_eq!(runtime_read_model.details.sessions.len(), 1);
    assert_eq!(
        runtime_read_model.details.sessions[0]
            .recovery_ref
            .as_deref(),
        Some("recovery-1")
    );
    assert_eq!(
        runtime_read_model.details.workspaces[0]
            .recovery_ref
            .as_deref(),
        Some("recovery-1")
    );
    assert_eq!(
        runtime_read_model.recovery.active_recovery_ids,
        vec!["recovery-1".to_string()]
    );
    assert_eq!(runtime_read_model.meta.ledger.next_sequence, 12);
    assert_eq!(runtime_read_model.meta.ledger.usage_count, 7);
}

#[test]
fn runtime_read_model_sanitizes_turn_item_tool_text_only() {
    let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                resume_candidate: None,
                session_id: SessionId::new("session-tool-text"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership::default(),
                execution_chain_ref: None,
                recovery_ref: None,
                current_turn: Some(magi_session_store::ActiveExecutionTurn {
                    turn_id: "turn-tool-text".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请检查 /Users/xie/code/TEST 里的文件".to_string()),
                    items: vec![magi_session_store::ActiveExecutionTurnItem {
                        item_id: "turn-item-tool".to_string(),
                        item_seq: 1,
                        kind: "tool_call".to_string(),
                        status: "failed".to_string(),
                        source: "worker".to_string(),
                        title: Some("读取文件".to_string()),
                        content: Some("用户要求检查 /Users/xie/code/TEST".to_string()),
                        task_id: None,
                        worker_id: None,
                        role_id: None,
                        tool_call_id: Some("tool-call-sensitive".to_string()),
                        tool_name: Some("read_file".to_string()),
                        tool_status: Some("failed".to_string()),
                        tool_arguments: Some(
                            r#"{"path":"/Users/xie/code/TEST/secret.txt","token":"sk-argument-secret"}"#
                                .to_string(),
                        ),
                        tool_result: Some(
                            "read /private/tmp/magi/result with Bearer resulttoken".to_string(),
                        ),
                        tool_error: Some(
                            "failed at /var/folders/magi/cache with sk-error-secret".to_string(),
                        ),
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: ThreadId::new("thread-tool-text"),
                    }],
                }),
                active_execution_chain: None,
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
        );

    let session = runtime_read_model
        .details
        .sessions
        .first()
        .expect("session summary should exist");
    let turn = session
        .current_turn
        .as_ref()
        .expect("current turn should exist");
    assert_eq!(
        turn.user_message.as_deref(),
        Some("请检查 /Users/xie/code/TEST 里的文件")
    );
    let item = session.turn_items.first().expect("turn item should exist");
    assert_eq!(
        item.content.as_deref(),
        Some("用户要求检查 /Users/xie/code/TEST")
    );

    let tool_arguments = item
        .tool_arguments
        .as_deref()
        .expect("tool arguments should exist");
    let tool_result = item
        .tool_result
        .as_deref()
        .expect("tool result should exist");
    let tool_error = item.tool_error.as_deref().expect("tool error should exist");
    for public_text in [tool_arguments, tool_result, tool_error] {
        assert!(public_text.contains("[path]"));
        assert!(!public_text.contains("/Users/xie"));
        assert!(!public_text.contains("/private/tmp"));
        assert!(!public_text.contains("/var/folders"));
        assert!(!public_text.contains("argument-secret"));
        assert!(!public_text.contains("resulttoken"));
        assert!(!public_text.contains("error-secret"));
    }
    assert!(tool_arguments.contains(r#""token":"[redacted]""#));
    assert!(tool_result.contains("Bearer [redacted]"));
    assert!(tool_error.contains("sk-[redacted]"));
}

#[test]
fn runtime_read_model_uses_task_store_projection_for_active_execution_group() {
    let task_store = TaskStore::new();
    let mission_id = magi_core::MissionId::new("mission-projection-authority");
    let root_task_id = TaskId::new("task-root-projection");
    let running_task_id = TaskId::new("task-running-projection");
    let pending_task_id = TaskId::new("task-pending-projection");
    let completed_task_id = TaskId::new("task-completed-projection");
    let failed_task_id = TaskId::new("task-failed-projection");
    let now = UtcMillis::now();
    for (task_id, parent_task_id, status) in [
        (&root_task_id, None, TaskStatus::Running),
        (
            &running_task_id,
            Some(root_task_id.clone()),
            TaskStatus::Running,
        ),
        (
            &pending_task_id,
            Some(root_task_id.clone()),
            TaskStatus::Pending,
        ),
        (
            &completed_task_id,
            Some(root_task_id.clone()),
            TaskStatus::Completed,
        ),
        (
            &failed_task_id,
            Some(root_task_id.clone()),
            TaskStatus::Failed,
        ),
    ] {
        task_store
            .insert_task(magi_core::Task {
                task_id: task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind: magi_core::TaskKind::LocalAgent,
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: if status == TaskStatus::Failed {
                    vec![
                        "provider timeout at /Users/xie/code/model.rs with sk-test-secret-value"
                            .to_string(),
                    ]
                } else {
                    Vec::new()
                },
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("任务投影应插入");
    }
    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-projection-authority"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: now,
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-projection-authority")),
                mission_id: Some(mission_id.clone()),
                task_id: Some(running_task_id.clone()),
                execution_chain_ref: Some("chain-projection-authority".to_string()),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-projection-authority".to_string()),
            recovery_ref: None,
            current_turn: None,
            active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                session_id: SessionId::new("session-projection-authority"),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-projection-authority".to_string(),
                workspace_id: None,
                branches: vec![magi_session_store::ActiveExecutionBranch {
                    task_id: running_task_id.clone(),
                    worker_id: magi_core::WorkerId::new("worker-projection"),
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-projection-authority"),
                }],
                dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "entry-projection-authority".to_string(),
                    trimmed_text: Some("projection authority".to_string()),
                    skill_name: None,
                },
                current_turn_id: None,
            }),
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        Some(&task_store),
    );

    let group = runtime_read_model
        .details
        .execution_groups
        .iter()
        .find(|entry| entry.mission_id == mission_id.to_string())
        .expect("TaskStore projection should create execution group entry");
    assert_eq!(group.current_status.as_deref(), Some("running"));
    assert!(group.active_task_ids.contains(&root_task_id.to_string()));
    assert!(group.active_task_ids.contains(&pending_task_id.to_string()));
    assert!(group.active_task_ids.contains(&running_task_id.to_string()));
    assert!(
        !group
            .active_task_ids
            .contains(&completed_task_id.to_string())
    );

    let task_statuses = runtime_read_model
        .details
        .tasks
        .iter()
        .map(|entry| (entry.task_id.as_str(), entry.current_status.as_deref()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        task_statuses
            .get(pending_task_id.as_str())
            .copied()
            .flatten(),
        Some("pending")
    );
    assert_eq!(
        task_statuses
            .get(completed_task_id.as_str())
            .copied()
            .flatten(),
        Some("completed")
    );
    let failed_task = runtime_read_model
        .details
        .tasks
        .iter()
        .find(|entry| entry.task_id == failed_task_id.to_string())
        .expect("failed TaskStore entry should be projected");
    let failure_detail = failed_task
        .failure_detail
        .as_deref()
        .expect("failed TaskStore entry should expose direct error");
    assert!(failure_detail.contains("provider timeout"));
    assert!(failure_detail.contains("[path]"));
    assert!(failure_detail.contains("sk-[redacted]"));
    assert!(!failure_detail.contains("/Users/xie"));
    assert!(!failure_detail.contains("test-secret-value"));
}

#[test]
fn runtime_read_model_counts_only_failed_tasks_as_recoverable() {
    let task_store = TaskStore::new();
    let mission_id = magi_core::MissionId::new("mission-recoverable-1");
    let root_task_id = magi_core::TaskId::new("task-root-recoverable-1");

    task_store
        .insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "root".to_string(),
            goal: "root".to_string(),
            status: TaskStatus::Failed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        })
        .expect("根任务应插入");
    for (task_id, status) in [
        ("task-branch-failed", TaskStatus::Failed),
        ("task-branch-pending", TaskStatus::Pending),
        ("task-branch-running", TaskStatus::Running),
        ("task-branch-completed", TaskStatus::Completed),
        ("task-branch-finished-failed", TaskStatus::Failed),
    ] {
        task_store
            .insert_task(magi_core::Task {
                task_id: magi_core::TaskId::new(task_id),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(root_task_id.clone()),
                kind: magi_core::TaskKind::LocalAgent,
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            })
            .expect("分支任务应插入");
    }

    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-recoverable-1"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-recoverable-1")),
                mission_id: Some(mission_id.clone()),
                task_id: Some(root_task_id.clone()),
                execution_chain_ref: Some("chain-recoverable-1".to_string()),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-recoverable-1".to_string()),
            recovery_ref: None,
            current_turn: None,
            active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                session_id: SessionId::new("session-recoverable-1"),
                mission_id,
                root_task_id,
                execution_chain_ref: "chain-recoverable-1".to_string(),
                workspace_id: None,
                branches: vec![
                    magi_session_store::ActiveExecutionBranch {
                        task_id: magi_core::TaskId::new("task-branch-failed"),
                        worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-branch-failed"),
                    },
                    magi_session_store::ActiveExecutionBranch {
                        task_id: magi_core::TaskId::new("task-branch-pending"),
                        worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-branch-pending"),
                    },
                    magi_session_store::ActiveExecutionBranch {
                        task_id: magi_core::TaskId::new("task-branch-running"),
                        worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-branch-running"),
                    },
                    magi_session_store::ActiveExecutionBranch {
                        task_id: magi_core::TaskId::new("task-branch-completed"),
                        worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-branch-completed"),
                    },
                    magi_session_store::ActiveExecutionBranch {
                        task_id: magi_core::TaskId::new("task-branch-finished-failed"),
                        worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-branch-finished-failed"),
                    },
                ],
                dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-recoverable-1".to_string(),
                    trimmed_text: Some("recoverable".to_string()),
                    skill_name: None,
                },
                current_turn_id: None,
            }),
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        Some(&task_store),
    );

    let session = &runtime_read_model.details.sessions[0];
    assert!(session.has_recoverable_chain);
    // 可恢复的执行链先于主线轮次：读模型把继续的方式投影给客户端，客户端不再自行推断。
    assert_eq!(session.continuation.as_deref(), Some("resume_chain"));
    assert_eq!(session.recoverable_branch_count, 2);
    assert_eq!(session.active_branches.len(), 5);
    for branch in &session.active_branches {
        assert_eq!(
            branch.stage,
            match branch.status.as_str() {
                "completed" => "finish",
                "running" => "execute",
                "killed" => "cancelled",
                status => status,
            }
        );
    }
}

#[test]
fn runtime_read_model_excludes_completed_root_from_recoverable_chain() {
    let task_store = TaskStore::new();
    let mission_id = magi_core::MissionId::new("mission-terminal-recoverable");
    let root_task_id = magi_core::TaskId::new("task-root-terminal-recoverable");
    let failed_child_id = magi_core::TaskId::new("task-child-terminal-recoverable");
    let now = UtcMillis::now();

    for (task_id, parent_task_id, status) in [
        (root_task_id.clone(), None, TaskStatus::Completed),
        (
            failed_child_id.clone(),
            Some(root_task_id.clone()),
            TaskStatus::Failed,
        ),
    ] {
        task_store
            .insert_task(magi_core::Task {
                task_id: task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind: magi_core::TaskKind::LocalAgent,
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("任务投影应插入");
    }

    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-terminal-recoverable"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: now,
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-terminal-recoverable")),
                mission_id: Some(mission_id.clone()),
                task_id: Some(root_task_id.clone()),
                execution_chain_ref: Some("chain-terminal-recoverable".to_string()),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-terminal-recoverable".to_string()),
            recovery_ref: None,
            current_turn: None,
            active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                session_id: SessionId::new("session-terminal-recoverable"),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-terminal-recoverable".to_string(),
                workspace_id: None,
                branches: vec![
                    magi_session_store::ActiveExecutionBranch {
                        task_id: root_task_id.clone(),
                        worker_id: magi_core::WorkerId::new("worker-terminal-recoverable"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-root-terminal-recoverable"),
                    },
                    magi_session_store::ActiveExecutionBranch {
                        task_id: failed_child_id.clone(),
                        worker_id: magi_core::WorkerId::new("worker-terminal-recoverable"),
                        use_tools: true,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-child-terminal-recoverable"),
                    },
                ],
                dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-terminal-recoverable".to_string(),
                    trimmed_text: Some("terminal recoverable".to_string()),
                    skill_name: None,
                },
                current_turn_id: None,
            }),
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        Some(&task_store),
    );

    let session = runtime_read_model
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-terminal-recoverable")
        .expect("session summary should exist");
    assert!(!session.has_recoverable_chain);
    assert_eq!(session.recoverable_branch_count, 0);
    assert_eq!(session.continuation, None);
    assert!(session.active_task_ids.is_empty());
    assert!(session.active_execution_group_ids.is_empty());
    assert!(
        runtime_read_model
            .overview
            .activity
            .active_task_ids
            .is_empty()
    );
    assert!(
        runtime_read_model
            .overview
            .activity
            .active_execution_group_ids
            .is_empty()
    );
}

#[test]
fn runtime_read_model_keeps_runtime_ledger_signals_from_exported_summary() {
    let mut audit_usage_ledger = ledger_dto(AuditUsageLedgerStatus {
        schema_version: "audit-usage-ledger-v1".to_string(),
        next_sequence: 9,
        audit_count: 3,
        usage_count: 4,
        persistence_path: None,
        last_persist_error: Some("blocked".to_string()),
    });
    audit_usage_ledger.pending_flush = true;
    audit_usage_ledger.last_persisted_at = Some(UtcMillis::now());
    audit_usage_ledger.refresh_readiness();

    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[],
        &[],
        audit_usage_ledger.clone(),
        None,
    );

    assert_eq!(
        runtime_read_model.meta.ledger.schema_version,
        "audit-usage-ledger-v1"
    );
    assert_eq!(runtime_read_model.meta.ledger.audit_count, 3);
    assert_eq!(runtime_read_model.meta.ledger.usage_count, 4);
    assert_eq!(
        runtime_read_model.meta.ledger.last_persist_error.as_deref(),
        Some(RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY)
    );
    assert!(runtime_read_model.meta.ledger.pending_flush);
    assert!(runtime_read_model.meta.ledger.last_persisted_at.is_some());
    assert_eq!(
        runtime_read_model.meta.ledger.cutover_readiness.is_ready,
        audit_usage_ledger.cutover_readiness.is_ready
    );
}

#[test]
fn runtime_read_model_excludes_consumed_recoveries_from_active_ids() {
    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[],
        &[
            WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-ready".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Ready,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership::default(),
                execution_chain_ref: None,
                snapshot_id: "snapshot-ready".to_string(),
                diagnostic_summary: None,
                consumed_at: None,
            },
            WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-consumed".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Consumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership::default(),
                execution_chain_ref: None,
                snapshot_id: "snapshot-consumed".to_string(),
                diagnostic_summary: Some("done".to_string()),
                consumed_at: Some(UtcMillis::now()),
            },
        ],
        ledger_dto(AuditUsageLedgerStatus::default()),
        None,
    );

    assert_eq!(
        runtime_read_model.recovery.active_recovery_ids,
        vec!["recovery-ready".to_string()]
    );
    assert_eq!(runtime_read_model.recovery.summaries.len(), 2);
    assert_eq!(
        runtime_read_model.recovery.summaries[0].current_status,
        "consumed".to_string()
    );
    assert_eq!(
        runtime_read_model.recovery.summaries[1].current_status,
        "ready".to_string()
    );
}

#[test]
fn runtime_read_model_sanitizes_workspace_recovery_diagnostic_summary() {
    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[],
        &[WorkspaceRecoverySidecarExport {
            recovery_ref: "recovery-public-sidecar".to_string(),
            workspace_id: WorkspaceId::new("workspace-public-sidecar"),
            current_status: RecoveryStatus::Ready,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership::default(),
            execution_chain_ref: None,
            snapshot_id: "snapshot-public-sidecar".to_string(),
            diagnostic_summary: Some(
                "resume from /Users/xie/.magi/recovery.json with Bearer abcdef and sk-test-secret"
                    .to_string(),
            ),
            consumed_at: None,
        }],
        ledger_dto(AuditUsageLedgerStatus::default()),
        None,
    );

    let summary = runtime_read_model
        .recovery
        .summaries
        .first()
        .and_then(|summary| summary.diagnostic_summary.as_deref())
        .expect("recovery sidecar diagnostic should exist");
    assert!(summary.contains("[path]"));
    assert!(summary.contains("Bearer [redacted]"));
    assert!(summary.contains("sk-[redacted]"));
    assert!(!summary.contains("/Users/xie"));
    assert!(!summary.contains("abcdef"));
    assert!(!summary.contains("sk-test-secret"));
}

#[test]
fn runtime_read_model_keeps_event_sourced_recovery_worker_when_workspace_sidecar_is_stale() {
    let mut input = RuntimeReadModelInput::default();
    input
        .recovery
        .summaries
        .push(RecoveryDiagnosticSummaryEntry {
            recovery_id: "recovery-worker-1".to_string(),
            event_count: 2,
            latest_stage: RecoveryActivityStage::WorkerResumed,
            latest_event_type: "worker.resumed.from_recovery".to_string(),
            latest_sequence: 3,
            latest_occurred_at: UtcMillis::now(),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            session_id: Some(magi_core::SessionId::new("session-1")),
            mission_id: Some(magi_core::MissionId::new("mission-1")),
            assignment_id: None,
            task_id: Some(magi_core::TaskId::new("todo-1")),
            worker_id: Some("worker-actual".to_string()),
            execution_chain_ref: Some("chain-1".to_string()),
            diagnostic_summary: Some("resume".to_string()),
            current_status: "worker_resumed".to_string(),
        });

    let runtime_read_model = runtime_read_model_dto(
        input,
        &[],
        &[WorkspaceRecoverySidecarExport {
            recovery_ref: "recovery-worker-1".to_string(),
            workspace_id: WorkspaceId::new("workspace-1"),
            current_status: RecoveryStatus::Consumed,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-1".to_string()),
            snapshot_id: "snapshot-1".to_string(),
            diagnostic_summary: Some("resume".to_string()),
            consumed_at: Some(UtcMillis::now()),
        }],
        ledger_dto(AuditUsageLedgerStatus::default()),
        None,
    );

    assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
    assert_eq!(
        runtime_read_model.recovery.summaries[0]
            .worker_id
            .as_deref(),
        Some("worker-actual")
    );
}

#[test]
fn runtime_read_model_preserves_event_sourced_recovery_outcome_when_workspace_sidecar_is_consumed_snapshot()
 {
    let mut input = RuntimeReadModelInput::default();
    input
        .recovery
        .summaries
        .push(RecoveryDiagnosticSummaryEntry {
            recovery_id: "recovery-outcome-1".to_string(),
            event_count: 2,
            latest_stage: RecoveryActivityStage::WorkerResumed,
            latest_event_type: "worker.resumed.from_recovery".to_string(),
            latest_sequence: 7,
            latest_occurred_at: UtcMillis(50),
            workspace_id: None,
            session_id: Some(magi_core::SessionId::new("session-outcome-1")),
            mission_id: Some(magi_core::MissionId::new("mission-outcome-1")),
            assignment_id: None,
            task_id: Some(magi_core::TaskId::new("todo-outcome-1")),
            worker_id: Some("worker-outcome-1".to_string()),
            execution_chain_ref: Some("chain-outcome-1".to_string()),
            diagnostic_summary: None,
            current_status: "worker_resumed".to_string(),
        });

    let runtime_read_model = runtime_read_model_dto(
        input,
        &[],
        &[WorkspaceRecoverySidecarExport {
            recovery_ref: "recovery-outcome-1".to_string(),
            workspace_id: WorkspaceId::new("workspace-outcome-1"),
            current_status: RecoveryStatus::Consumed,
            last_update: UtcMillis(99),
            ownership: ExecutionOwnership {
                session_id: Some(magi_core::SessionId::new("session-outcome-1")),
                mission_id: Some(magi_core::MissionId::new("mission-stale")),
                task_id: Some(magi_core::TaskId::new("todo-stale")),
                worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-stale".to_string()),
            snapshot_id: "snapshot-outcome-1".to_string(),
            diagnostic_summary: Some("resume detail".to_string()),
            consumed_at: Some(UtcMillis(99)),
        }],
        ledger_dto(AuditUsageLedgerStatus::default()),
        None,
    );

    assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
    let summary = &runtime_read_model.recovery.summaries[0];
    assert_eq!(summary.latest_stage, RecoveryActivityStage::WorkerResumed);
    assert_eq!(summary.current_status, "worker_resumed");
    assert_eq!(summary.latest_occurred_at, UtcMillis(50));
    assert_eq!(
        summary.workspace_id,
        Some(WorkspaceId::new("workspace-outcome-1"))
    );
    assert_eq!(summary.diagnostic_summary.as_deref(), Some("resume detail"));
    assert_eq!(summary.worker_id.as_deref(), Some("worker-outcome-1"));
    assert_eq!(
        summary.execution_chain_ref.as_deref(),
        Some("chain-outcome-1")
    );
}

#[test]
fn bound_session_sidecar_does_not_mark_runtime_session_active() {
    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-bound"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                mission_id: Some(magi_core::MissionId::new("mission-bound")),
                task_id: Some(magi_core::TaskId::new("task-bound")),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-bound".to_string()),
            recovery_ref: None,
            current_turn: None,
            active_execution_chain: None,
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        None,
    );

    assert_eq!(runtime_read_model.details.sessions.len(), 1);
    let session = &runtime_read_model.details.sessions[0];
    assert_eq!(session.session_id, "session-bound");
    assert_eq!(session.current_status.as_deref(), Some("bound"));
    assert!(session.active_execution_group_ids.is_empty());
    assert!(session.active_task_ids.is_empty());
    assert!(session.recovery_ids.is_empty());
    assert_eq!(session.execution_chain_ref.as_deref(), Some("chain-bound"));
}

#[test]
fn session_sidecar_overrides_stale_event_scoped_active_ids() {
    let mission_id = magi_core::MissionId::new("mission-current");
    let root_task_id = magi_core::TaskId::new("task-root-current");
    let branch_task_id = magi_core::TaskId::new("task-branch-current");
    let worker_id = magi_core::WorkerId::new("worker-current");
    let root_created_at = UtcMillis::now();
    let branch_created_at = UtcMillis::now();
    let task_store = TaskStore::new();
    task_store
        .insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "current root".to_string(),
            goal: "current root".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: root_created_at,
            updated_at: UtcMillis::now(),
        })
        .expect("根任务应插入");
    task_store
        .insert_task(magi_core::Task {
            task_id: branch_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: magi_core::TaskKind::LocalAgent,
            title: "current branch".to_string(),
            goal: "current branch".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: branch_created_at,
            updated_at: UtcMillis::now(),
        })
        .expect("分支任务应插入");

    let mut input = RuntimeReadModelInput::default();
    input.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: "session-stale".to_string(),
        active_execution_group_ids: vec!["mission-stale".to_string()],
        active_task_ids: vec!["task-stale".to_string()],
        recovery_ids: vec!["recovery-stale".to_string()],
        current_status: Some("failed".to_string()),
        mission_id: Some("mission-stale".to_string()),
        root_task_id: Some("task-root-stale".to_string()),
        root_task_status: Some("failed".to_string()),
        execution_chain_ref: Some("chain-stale".to_string()),
        recovery_ref: Some("recovery-stale".to_string()),
        ..SessionRuntimeSummaryEntry::default()
    });

    let runtime_read_model = runtime_read_model_dto(
        input,
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-stale"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: UtcMillis::now(),
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-stale")),
                mission_id: Some(mission_id.clone()),
                task_id: Some(branch_task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some("chain-current".to_string()),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-current".to_string()),
            recovery_ref: None,
            current_turn: None,
            active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                session_id: SessionId::new("session-stale"),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-current".to_string(),
                workspace_id: None,
                branches: vec![magi_session_store::ActiveExecutionBranch {
                    task_id: branch_task_id.clone(),
                    worker_id: worker_id.clone(),
                    use_tools: false,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-chain-current"),
                }],
                dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-current".to_string(),
                    trimmed_text: Some("current".to_string()),
                    skill_name: None,
                },
                current_turn_id: None,
            }),
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        Some(&task_store),
    );

    let session = runtime_read_model
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-stale")
        .expect("session summary should exist");
    assert!(session.active_execution_group_ids.is_empty());
    assert!(session.active_task_ids.is_empty());
    assert!(session.recovery_ids.is_empty());
    assert_eq!(session.current_status.as_deref(), Some("bound"));
    assert_eq!(session.mission_id.as_deref(), Some("mission-current"));
    assert_eq!(session.root_task_id.as_deref(), Some("task-root-current"));
    assert_eq!(session.root_task_status.as_deref(), Some("completed"));
    assert_eq!(session.root_task_created_at, Some(root_created_at));
    assert_eq!(
        session.execution_chain_ref.as_deref(),
        Some("chain-current")
    );
    assert!(session.current_turn.is_none());
    assert!(session.turn_items.is_empty());
}

#[test]
fn session_sidecar_keeps_completed_current_turn_as_display_source() {
    let mission_id = magi_core::MissionId::new("mission-turn-completed");
    let root_task_id = magi_core::TaskId::new("task-root-turn-completed");
    let branch_task_id = magi_core::TaskId::new("task-branch-turn-completed");
    let worker_id = magi_core::WorkerId::new("worker-turn-completed");
    let accepted_at = UtcMillis::now();
    let task_store = TaskStore::new();
    task_store
        .insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "completed root".to_string(),
            goal: "completed root".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        })
        .expect("根任务应插入");
    task_store
        .insert_task(magi_core::Task {
            task_id: branch_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: magi_core::TaskKind::LocalAgent,
            title: "completed branch".to_string(),
            goal: "completed branch".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(magi_core::TaskExecutorBinding::for_role("reviewer")),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        })
        .expect("分支任务应插入");

    let runtime_read_model = runtime_read_model_dto(
        RuntimeReadModelInput::default(),
        &[SessionRuntimeSidecarExport {
            resume_candidate: None,
            session_id: SessionId::new("session-turn-completed"),
            current_status: SessionExecutionSidecarStatus::Bound,
            last_update: accepted_at,
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-turn-completed")),
                mission_id: Some(mission_id.clone()),
                task_id: Some(branch_task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some("chain-turn-completed".to_string()),
                ..ExecutionOwnership::default()
            },
            execution_chain_ref: Some("chain-turn-completed".to_string()),
            recovery_ref: None,
            current_turn: Some(magi_session_store::ActiveExecutionTurn {
                turn_id: "turn-session-action-completed".to_string(),
                turn_seq: accepted_at.0,
                accepted_at,
                completed_at: None,
                status: "completed".to_string(),
                user_message: Some("请继续整理结果".to_string()),
                items: vec![magi_session_store::ActiveExecutionTurnItem {
                    item_id: "turn-item-final".to_string(),
                    item_seq: 1,
                    kind: "assistant_final".to_string(),
                    status: "completed".to_string(),
                    source: "orchestrator".to_string(),
                    title: Some("总结".to_string()),
                    content: Some("这是 turn 主线最终总结".to_string()),
                    task_id: Some(branch_task_id.clone()),
                    worker_id: Some(worker_id.clone()),
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                }],
            }),
            active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                session_id: SessionId::new("session-turn-completed"),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-turn-completed".to_string(),
                workspace_id: None,
                branches: vec![magi_session_store::ActiveExecutionBranch {
                    task_id: branch_task_id.clone(),
                    worker_id: worker_id.clone(),
                    use_tools: false,
                    skill_name: None,
                    is_primary: false,
                    thread_id: ThreadId::new("thread-turn-completed"),
                }],
                dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                    accepted_at,
                    entry_id: "timeline-turn-completed".to_string(),
                    trimmed_text: Some("已完成".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some("turn-session-action-completed".to_string()),
            }),
        }],
        &[],
        ledger_dto(AuditUsageLedgerStatus::default()),
        Some(&task_store),
    );

    let session = runtime_read_model
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-turn-completed")
        .expect("session summary should exist");
    assert!(session.active_execution_group_ids.is_empty());
    assert!(session.active_task_ids.is_empty());
    assert!(!session.active_branches.is_empty());
    assert!(!session.turn_items.is_empty());
    assert_eq!(
        session.turn_items[0].role_id.as_deref(),
        Some("reviewer"),
        "turn item role_id should be backfilled from task executor binding"
    );
    let current_turn = session
        .current_turn
        .as_ref()
        .expect("completed root should still preserve current turn display state");
    assert_eq!(current_turn.turn_id, "turn-session-action-completed");
    assert_eq!(current_turn.user_message.as_deref(), Some("请继续整理结果"));
    assert_eq!(session.root_task_status.as_deref(), Some("completed"));
    assert!(!session.has_recoverable_chain);
    assert_eq!(session.recoverable_branch_count, 0);
}

#[test]
fn runtime_read_model_fills_session_budget_from_usage_observation() {
    let mut input = RuntimeReadModelInput::default();
    input.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: "session-budget".to_string(),
        usage_observation: Some(SessionRuntimeUsageObservation {
            projected_request_tokens: 136_000,
            resolved_model: Some("gpt-5-codex".to_string()),
            context_window_limit_tokens: Some(272_000),
            pressure_level: Some("normal".to_string()),
            observed_at: Some(UtcMillis(1)),
            ..SessionRuntimeUsageObservation::default()
        }),
        ..SessionRuntimeSummaryEntry::default()
    });
    // 没有观测值的会话保持 budget == None。
    input.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: "session-no-usage".to_string(),
        ..SessionRuntimeSummaryEntry::default()
    });

    let dto = runtime_read_model_dto(
        input,
        &[],
        &[],
        ledger_dto(AuditUsageLedgerStatus {
            schema_version: "audit-usage-ledger-v1".to_string(),
            next_sequence: 1,
            audit_count: 0,
            usage_count: 0,
            persistence_path: None,
            last_persist_error: None,
        }),
        None,
    );

    let with_usage = dto
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-budget")
        .expect("session with usage should exist");
    let budget = with_usage
        .budget
        .as_ref()
        .expect("budget should be derived from usage observation");
    // 窗口与告警来自观测快照，不再根据模型名或当前设置重算。
    assert_eq!(budget.token_limit, 272_000);
    assert_eq!(budget.token_used, 136_000);
    assert_eq!(budget.remaining_tokens, 136_000);
    assert!(budget.usage_ratio > 0.0 && budget.usage_ratio < 1.0);
    assert!(!budget.warning_level.is_empty());

    let without_usage = dto
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-no-usage")
        .expect("session without usage should exist");
    assert!(without_usage.budget.is_none());
}

#[test]
fn budget_requires_bound_window_and_pressure_in_the_observation() {
    let mut input = RuntimeReadModelInput::default();
    for (window, pressure) in [
        (None, Some("normal")),
        (Some(0), Some("normal")),
        (Some(256_000), None),
    ] {
        input.details.sessions.push(SessionRuntimeSummaryEntry {
            usage_observation: Some(SessionRuntimeUsageObservation {
                projected_request_tokens: 64_000,
                resolved_model: Some("gpt-5.6-luna".into()),
                context_window_limit_tokens: window,
                pressure_level: pressure.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    merge_session_budgets(&mut input);
    assert!(
        input
            .details
            .sessions
            .iter()
            .all(|session| session.budget.is_none())
    );
}

#[test]
fn ledger_usage_observations_backfill_budget_after_restart() {
    // 重启场景:event-bus recent_events 为空,会话按 sidecar 重建,实时投影没有
    // usage_observation。账本回放出的观测值应回填并恢复 budget。
    let mut input = RuntimeReadModelInput::default();
    input.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: "session-restored".to_string(),
        ..SessionRuntimeSummaryEntry::default()
    });
    // 已有实时观测的会话:账本不得覆盖更新的实时值。
    input.details.sessions.push(SessionRuntimeSummaryEntry {
        session_id: "session-live".to_string(),
        usage_observation: Some(SessionRuntimeUsageObservation {
            projected_request_tokens: 136_000,
            resolved_model: Some("gpt-5-codex".to_string()),
            context_window_limit_tokens: Some(272_000),
            pressure_level: Some("normal".to_string()),
            observed_at: Some(UtcMillis(2)),
            ..SessionRuntimeUsageObservation::default()
        }),
        ..SessionRuntimeSummaryEntry::default()
    });

    let mut ledger_observations = BTreeMap::new();
    ledger_observations.insert(
        "session-restored".to_string(),
        SessionRuntimeUsageObservation {
            projected_request_tokens: 68_000,
            resolved_model: Some("gpt-5-codex".to_string()),
            context_window_limit_tokens: Some(272_000),
            pressure_level: Some("normal".to_string()),
            observed_at: Some(UtcMillis(1)),
            ..SessionRuntimeUsageObservation::default()
        },
    );
    // 账本里这条更旧/更小,不应覆盖 session-live 的实时观测。
    ledger_observations.insert(
        "session-live".to_string(),
        SessionRuntimeUsageObservation {
            projected_request_tokens: 1_000,
            resolved_model: Some("gpt-5-codex".to_string()),
            context_window_limit_tokens: Some(272_000),
            pressure_level: Some("normal".to_string()),
            observed_at: Some(UtcMillis(1)),
            ..SessionRuntimeUsageObservation::default()
        },
    );

    let dto = runtime_read_model_dto_with_usage(
        input,
        &[],
        &[],
        ledger_dto(AuditUsageLedgerStatus {
            schema_version: "audit-usage-ledger-v1".to_string(),
            next_sequence: 1,
            audit_count: 0,
            usage_count: 2,
            persistence_path: None,
            last_persist_error: None,
        }),
        None,
        &ledger_observations,
    );

    let restored = dto
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-restored")
        .expect("restored session should exist");
    let budget = restored
        .budget
        .as_ref()
        .expect("budget should be backfilled from ledger observation");
    assert_eq!(budget.token_limit, 272_000);
    assert_eq!(budget.token_used, 68_000);

    // 实时观测优先:仍按 136_000 计算,而非账本里的 1_000。
    let live = dto
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == "session-live")
        .expect("live session should exist");
    assert_eq!(
        live.budget
            .as_ref()
            .expect("live budget should exist")
            .token_used,
        136_000
    );
}
