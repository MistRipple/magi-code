use super::*;
use magi_bridge_client::ChatToolFunction;
use magi_session_store::{ThreadChatToolCall, ThreadChatToolFunction};

fn recovery_task() -> Task {
    Task {
        task_id: magi_core::TaskId::new("recovery-task"),
        mission_id: magi_core::MissionId::new("recovery-mission"),
        root_task_id: magi_core::TaskId::new("recovery-task"),
        parent_task_id: None,
        kind: magi_core::TaskKind::LocalAgent,
        title: "恢复".into(),
        goal: "继续工作".into(),
        status: magi_core::TaskStatus::Running,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        completion_contract: Default::default(),
        recovery_checkpoint: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: Default::default(),
        created_at: magi_core::UtcMillis(1),
        updated_at: magi_core::UtcMillis(1),
    }
}

fn canonical_history(task: &Task, outcomes: &[(&str, Option<Value>)]) -> SessionStore {
    let turns = outcomes
        .iter()
        .enumerate()
        .map(|(index, (status, result))| {
            let status: magi_session_store::CanonicalTurnItemStatus =
                serde_json::from_value(Value::String((*status).into())).unwrap();
            let session = SessionId::new("recovery-session");
            let turn_id = format!("turn-{index}");
            magi_session_store::CanonicalTurn {
                session_id: session.clone(),
                turn_id: turn_id.clone(),
                turn_seq: index as u64 + 1,
                accepted_at: magi_core::UtcMillis(1),
                completed_at: Some(magi_core::UtcMillis(2)),
                status: magi_session_store::CanonicalTurnStatus::Interrupted,
                response_duration_ms: None,
                usage: None,
                metadata: Default::default(),
                model_steps: Vec::new(),
                items: vec![magi_session_store::CanonicalTurnItem {
                    session_id: session,
                    turn_id,
                    turn_seq: index as u64 + 1,
                    item_id: "tool-item".into(),
                    item_seq: 1,
                    kind: magi_session_store::CanonicalTurnItemKind::ToolCall,
                    created_at: magi_core::UtcMillis(1),
                    updated_at: magi_core::UtcMillis(2),
                    status,
                    item_version: Some(1),
                    title: None,
                    content: None,
                    blocks: Vec::new(),
                    tool: Some(magi_session_store::CanonicalToolCall {
                        call_id: "provider-reused-id".into(),
                        name: "shell_exec".into(),
                        arguments: Some(serde_json::json!({"command":"publish"})),
                        result: result.clone(),
                        error: None,
                    }),
                    worker: Some(magi_session_store::CanonicalWorkerRef {
                        task_id: Some(task.task_id.clone()),
                        worker_id: None,
                        role_id: None,
                        title: None,
                    }),
                    source_thread_id: ThreadId::new("recovery-thread"),
                    visibility: Default::default(),
                    metadata: Default::default(),
                }],
            }
        })
        .collect();
    SessionStore::from_state_for_test(magi_session_store::SessionStoreState {
        canonical_turns: turns,
        ..Default::default()
    })
}

#[test]
fn new_agent_activation_keeps_unknown_guard_without_old_success_evidence() {
    for (status, result, should_block) in [
        (
            "completed",
            Some(serde_json::json!({"status":"succeeded"})),
            false,
        ),
        (
            "indeterminate",
            Some(serde_json::json!({"status":"outcome_unknown"})),
            true,
        ),
    ] {
        let mut parent = recovery_task();
        parent.task_id = magi_core::TaskId::new("parent");
        parent.root_task_id = parent.task_id.clone();
        let mut previous = recovery_task();
        previous.parent_task_id = Some(parent.task_id.clone());
        previous.root_task_id = parent.task_id.clone();
        previous.status = magi_core::TaskStatus::Completed;
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: previous.task_id.clone(),
            session_id: SessionId::new("recovery-session"),
            lifetime: magi_core::AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        previous.executor_binding = Some(binding.clone());
        let mut current = previous.clone();
        current.task_id = magi_core::TaskId::new("new-activation");
        current.status = magi_core::TaskStatus::Running;
        let identity = binding.agent_activation.as_mut().unwrap();
        identity.generation = 2;
        identity.previous_task_id = Some(previous.task_id.clone());
        current.executor_binding = Some(binding);
        let tasks = TaskStore::new();
        tasks.insert_task(parent).unwrap();
        tasks.insert_task(previous.clone()).unwrap();
        tasks.insert_task(current.clone()).unwrap();
        let store = canonical_history(&previous, &[(status, result)]);
        store
            .register_thread(magi_session_store::ExecutionThread {
                thread_id: ThreadId::new("recovery-thread"),
                session_id: SessionId::new("recovery-session"),
                mission_id: current.mission_id.clone(),
                role_id: "executor".into(),
                worker_instance_id: magi_core::WorkerId::new("previous-worker"),
                status: magi_session_store::ExecutionThreadStatus::Active,
                created_at: magi_core::UtcMillis(1),
                last_used_at: magi_core::UtcMillis(2),
                observed_context_window_tokens: None,
                handled_task_ids: vec![previous.task_id.clone()],
            })
            .unwrap();
        let recovered = ToolExecutionLedger::from_canonical_task(
            &store,
            &tasks,
            &SessionId::new("recovery-session"),
            &current,
            &ThreadId::new("new-thread"),
            None,
        )
        .unwrap();
        assert!(
            recovered.history.is_empty(),
            "新 activation 不复制旧成功证据"
        );
        let decisions = recovered.ledger.plan(
            &[call("new", "shell_exec", r#"{"command":"publish"}"#)],
            None,
        );
        assert_eq!(
            matches!(decisions[0], ToolCallExecutionDecision::Execute { .. }),
            !should_block
        );
    }
}

#[test]
fn cancelled_background_launch_is_not_success_evidence_or_replay_authority() {
    let task = recovery_task();
    for status in [
        magi_tool_runtime::BackgroundJobStatus::Running,
        magi_tool_runtime::BackgroundJobStatus::Cancelled,
        magi_tool_runtime::BackgroundJobStatus::OutcomeUnknown,
    ] {
        let store = canonical_history(
            &task,
            &[(
                "completed",
                Some(
                    serde_json::json!({"status":"succeeded","running":true,"job_id":"job-recovery"}),
                ),
            )],
        );
        let mut state = store.export_state();
        let item = &mut state.canonical_turns[0].items[0];
        let job = magi_tool_runtime::BackgroundJob {
            job_id: "job-recovery".into(),
            session_id: item.session_id.clone(),
            workspace_id: None,
            task_id: Some(task.task_id.clone()),
            turn_id: item.turn_id.clone(),
            item_id: item.item_id.clone(),
            tool_call_id: item.tool.as_ref().unwrap().call_id.clone(),
            revision: 2,
            command: "publish".into(),
            cwd: "/workspace".into(),
            status,
            started_at: magi_core::UtcMillis(1),
            settled_at: status.is_terminal().then_some(magi_core::UtcMillis(2)),
            exit_code: None,
            stdout_path: std::env::temp_dir().join("recovery-stdout.log"),
            stderr_path: std::env::temp_dir().join("recovery-stderr.log"),
        };
        item.metadata.insert(
            magi_tool_runtime::BACKGROUND_JOB_METADATA_KEY.into(),
            serde_json::to_value(job).unwrap(),
        );
        let store = SessionStore::from_state_for_test(state);
        let recovered = ToolExecutionLedger::from_canonical_task(
            &store,
            &TaskStore::new(),
            &SessionId::new("recovery-session"),
            &task,
            &ThreadId::new("recovery-thread"),
            None,
        )
        .unwrap();
        assert_eq!(
            infer_tool_call_status(recovered.history[1].content.as_deref().unwrap()),
            "error"
        );
        assert!(!matches!(
            recovered.ledger.plan(
                &[call("next", "shell_exec", r#"{"command":"publish"}"#)],
                None
            )[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
        assert!(
            recovered.history[1]
                .content
                .as_deref()
                .unwrap()
                .contains("job-recovery")
        );
    }
}

#[test]
fn recovered_user_question_uses_durable_answer_instead_of_cancelled_tool_progress() {
    let task = recovery_task();
    let arguments = serde_json::json!({"questions":[{
        "header":"方案","question":"选哪个？","options":[{"label":"A"},{"label":"B"}]
    }]})
    .to_string();
    for answered in [false, true] {
        let store = canonical_history(
            &task,
            &[(
                "cancelled",
                Some(serde_json::json!({"status":"awaiting_user_input"})),
            )],
        );
        let mut state = store.export_state();
        let item = &mut state.canonical_turns[0].items[0];
        let tool = item.tool.as_mut().unwrap();
        tool.name = "ask_user_question".into();
        tool.arguments = Some(serde_json::from_str(&arguments).unwrap());
        let question = magi_session_store::user_question::UserQuestionState {
            request: crate::PendingUserQuestion {
                question_id: "recovered-question".into(),
                session_id: item.session_id.clone(),
                task_id: Some(task.task_id.clone()),
                turn_id: item.turn_id.clone(),
                tool_call_id: tool.call_id.clone(),
                questions: crate::user_question::parse_questions(&arguments).unwrap(),
                requested_at: magi_core::UtcMillis(1),
                can_skip: true,
            },
            cancelled_at: None,
            response: answered.then_some(crate::UserQuestionResponse::Answered {
                answers: vec![crate::UserQuestionAnswer {
                    selected: vec!["B".into()],
                    other: None,
                }],
            }),
        };
        item.metadata.insert(
            magi_session_store::user_question::USER_QUESTION_METADATA_KEY.into(),
            serde_json::to_value(question).unwrap(),
        );
        let store = SessionStore::from_state_for_test(state);
        let recovered = ToolExecutionLedger::from_canonical_task(
            &store,
            &TaskStore::new(),
            &SessionId::new("recovery-session"),
            &task,
            &ThreadId::new("recovery-thread"),
            None,
        )
        .unwrap();
        let decision = recovered
            .ledger
            .plan(&[call("new-call", "ask_user_question", &arguments)], None);
        if answered {
            assert!(matches!(
                decision[0],
                ToolCallExecutionDecision::Reuse { .. }
            ));
            assert!(
                recovered.history[1]
                    .content
                    .as_deref()
                    .unwrap()
                    .contains("B")
            );
        } else {
            assert!(matches!(
                decision[0],
                ToolCallExecutionDecision::Execute { .. }
            ));
        }
    }
}

#[test]
fn recovered_agent_cancel_rechecks_settlement_instead_of_reusing_old_receipts() {
    let arguments = r#"{"task_id":"original-child","reason":"方向变更"}"#;
    for result in [
        None,
        Some(r#"{"status":"interrupted","execution":"unknown"}"#),
        Some(r#"{"status":"cancelled","resources_settled":false}"#),
        Some(r#"{"status":"already_terminal","resources_settled":true}"#),
        Some(r#"{"status":"failed","error_code":"cancel_unsettled"}"#),
    ] {
        let mut history = vec![ThreadChatMessage {
            input_request_id: None,
            role: "assistant".into(),
            content: None,
            images: vec![],
            tool_calls: vec![ThreadChatToolCall {
                id: "cancel-before-restart".into(),
                kind: "function".into(),
                function: ThreadChatToolFunction {
                    name: "agent_cancel".into(),
                    arguments: arguments.into(),
                },
            }],
            tool_call_id: None,
            provider_context: vec![],
        }];
        if let Some(result) = result {
            history.push(ThreadChatMessage {
                input_request_id: None,
                role: "tool".into(),
                content: Some(result.into()),
                images: vec![],
                tool_calls: vec![],
                tool_call_id: Some("cancel-before-restart".into()),
                provider_context: vec![],
            });
        }
        let ledger = ToolExecutionLedger::from_thread_history("continue", &history, None);
        assert!(
            matches!(
                ledger.plan(&[call("cancel-recheck", "agent_cancel", arguments)], None)[0],
                ToolCallExecutionDecision::Execute { .. }
            ),
            "{result:?}"
        );
    }
}

#[test]
fn recovered_agent_wait_observes_current_state_without_replaying_or_caching_effects() {
    for result in [
        None,
        Some(r#"{"status":"interrupted","execution":"unknown"}"#),
        Some(r#"{"status":"completed","all_terminal":true}"#),
    ] {
        let mut history = vec![ThreadChatMessage {
            input_request_id: None,
            role: "assistant".into(),
            content: None,
            images: vec![],
            tool_calls: vec![ThreadChatToolCall {
                id: "wait-before-restart".into(),
                kind: "function".into(),
                function: ThreadChatToolFunction {
                    name: "agent_wait".into(),
                    arguments: r#"{"task_ids":["child"]}"#.into(),
                },
            }],
            tool_call_id: None,
            provider_context: vec![],
        }];
        if let Some(result) = result {
            history.push(ThreadChatMessage {
                input_request_id: None,
                role: "tool".into(),
                content: Some(result.into()),
                images: vec![],
                tool_calls: vec![],
                tool_call_id: Some("wait-before-restart".into()),
                provider_context: vec![],
            });
        }
        let ledger = ToolExecutionLedger::from_thread_history("continue", &history, None);
        assert!(matches!(
            ledger.plan(
                &[call("wait-now", "agent_wait", r#"{"task_ids":["child"]}"#)],
                None
            )[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }
}

#[test]
fn recovered_job_observation_can_repeat_but_process_mutations_remain_protected() {
    for action in ["read", "list", "wait", "write", "kill"] {
        let arguments = serde_json::json!({"action":action,"job_id":"durable-job"}).to_string();
        let history = vec![ThreadChatMessage {
            input_request_id: None,
            role: "assistant".into(),
            content: None,
            images: vec![],
            tool_calls: vec![ThreadChatToolCall {
                id: "old-job-call".into(),
                kind: "function".into(),
                function: ThreadChatToolFunction {
                    name: "shell_exec".into(),
                    arguments: arguments.clone(),
                },
            }],
            tool_call_id: None,
            provider_context: vec![],
        }];
        let ledger = ToolExecutionLedger::from_thread_history("continue", &history, None);
        let decision = ledger.plan(&[call("current-job-call", "shell_exec", &arguments)], None);
        if matches!(action, "read" | "list" | "wait") {
            assert!(
                matches!(decision[0], ToolCallExecutionDecision::Execute { .. }),
                "{action}"
            );
        } else {
            assert!(
                matches!(
                    decision[0],
                    ToolCallExecutionDecision::RecoveryBlocked { .. }
                ),
                "{action}"
            );
        }
    }
}

#[test]
fn canonical_recovery_keeps_unknown_effects_without_model_history_or_a_user_boundary() {
    let task = recovery_task();
    for (status, result) in [
        ("cancelled", None),
        (
            "failed",
            Some(serde_json::json!({"status":"outcome_unknown"})),
        ),
        (
            "cancelled",
            Some(serde_json::json!({"status":"running","output":"in progress"})),
        ),
        (
            "indeterminate",
            Some(serde_json::json!({"status":"outcome_unknown"})),
        ),
    ] {
        let store = canonical_history(&task, &[(status, result)]);
        assert!(
            store
                .thread_registry_snapshot(&SessionId::new("recovery-session"))
                .is_empty(),
            "未知副作用从 canonical 工具账本恢复，不依赖线程注册表或模型历史"
        );
        let recovered = ToolExecutionLedger::from_canonical_task(
            &store,
            &TaskStore::new(),
            &SessionId::new("recovery-session"),
            &task,
            &ThreadId::new("recovery-thread"),
            None,
        )
        .unwrap();
        assert_eq!(
            infer_tool_call_status(recovered.history[1].content.as_deref().unwrap()),
            "error",
            "未知结果不能成为任务完成证据"
        );
        let mut ledger = recovered.ledger;
        let results = ledger.execute_batch_with(
            &[call("new-call", "shell_exec", r#"{"command":"publish"}"#)],
            None,
            |calls| {
                assert!(calls.is_empty(), "恢复不能再次发布结果未知的操作");
                Vec::new()
            },
        );
        assert!(results[0].0.contains("interrupted_tool_outcome_unknown"));
    }
}

#[test]
fn canonical_recovery_reuses_success_allows_known_unexecuted_and_does_not_mix_tasks() {
    let task = recovery_task();
    for (status, result, reuse) in [
        ("completed", serde_json::json!({"status":"succeeded"}), true),
        (
            "failed",
            serde_json::json!({"status":"failed","execution_started":false}),
            false,
        ),
        ("failed", Value::String("command failed".into()), false),
        (
            "blocked",
            serde_json::json!({"status":"awaiting_approval"}),
            false,
        ),
        (
            "cancelled",
            Value::String(r#"{"status":"awaiting_approval"}"#.into()),
            false,
        ),
    ] {
        let store = canonical_history(&task, &[(status, Some(result))]);
        let ledger = ToolExecutionLedger::from_canonical_task(
            &store,
            &TaskStore::new(),
            &SessionId::new("recovery-session"),
            &task,
            &ThreadId::new("recovery-thread"),
            None,
        )
        .unwrap()
        .ledger;
        let decisions = ledger.plan(
            &[call("next", "shell_exec", r#"{"command":"publish"}"#)],
            None,
        );
        assert_eq!(
            matches!(decisions[0], ToolCallExecutionDecision::Reuse { .. }),
            reuse
        );
        if !reuse {
            assert!(matches!(
                decisions[0],
                ToolCallExecutionDecision::Execute { .. }
            ));
        }
        let mut unrelated = task.clone();
        unrelated.task_id = magi_core::TaskId::new("other");
        let ledger = ToolExecutionLedger::from_canonical_task(
            &store,
            &TaskStore::new(),
            &SessionId::new("recovery-session"),
            &unrelated,
            &ThreadId::new("recovery-thread"),
            None,
        )
        .unwrap()
        .ledger;
        assert!(matches!(
            ledger.plan(
                &[call("next", "shell_exec", r#"{"command":"publish"}"#)],
                None
            )[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }
}

#[test]
fn canonical_recovery_qualifies_provider_ids_by_turn_and_unknown_effect_wins_over_success() {
    let mut task = recovery_task();
    let store = canonical_history(
        &task,
        &[
            ("completed", Some(serde_json::json!({"status":"succeeded"}))),
            ("cancelled", None),
        ],
    );
    let ledger = ToolExecutionLedger::from_canonical_task(
        &store,
        &TaskStore::new(),
        &SessionId::new("recovery-session"),
        &task,
        &ThreadId::new("recovery-thread"),
        None,
    )
    .unwrap()
    .ledger;
    assert!(matches!(
        ledger.plan(
            &[call("next", "shell_exec", r#"{"command":"publish"}"#)],
            None
        )[0],
        ToolCallExecutionDecision::RecoveryBlocked { .. }
    ));
    task.recovery_checkpoint = Some(magi_core::TaskRecoveryCheckpoint {
        source_session_id: SessionId::new("recovery-session"),
        source_task_id: task.task_id.clone(),
        source_turn_id: "turn-1".into(),
        source_thread_id: ThreadId::new("recovery-thread"),
    });
    let task_store = TaskStore::new();
    let mut source_task = task.clone();
    source_task.recovery_checkpoint = None;
    task_store.insert_task(source_task).unwrap();
    task.task_id = magi_core::TaskId::new("new-activation-task");
    let ledger = ToolExecutionLedger::from_canonical_task(
        &store,
        &task_store,
        &SessionId::new("recovery-session"),
        &task,
        &ThreadId::new("new-thread"),
        None,
    )
    .unwrap()
    .ledger;
    assert!(matches!(
        ledger.plan(
            &[call("next", "shell_exec", r#"{"command":"publish"}"#)],
            None
        )[0],
        ToolCallExecutionDecision::RecoveryBlocked { .. }
    ));
}

#[test]
fn repeated_continue_retains_unknown_effects_from_the_entire_persisted_lineage() {
    let original = recovery_task();
    let canonical = canonical_history(&original, &[("indeterminate", None)]);
    let task_store = TaskStore::new();
    task_store.insert_task(original.clone()).unwrap();
    let mut current = original.clone();
    let mut prior_thread = ThreadId::new("recovery-thread");
    for generation in 1..=3 {
        let prior = current.task_id.clone();
        current.task_id = magi_core::TaskId::new(format!("continued-{generation}"));
        current.root_task_id = current.task_id.clone();
        current.recovery_checkpoint = Some(magi_core::TaskRecoveryCheckpoint {
            source_session_id: SessionId::new("recovery-session"),
            source_task_id: prior,
            source_turn_id: format!("turn-{}", generation - 1),
            source_thread_id: prior_thread,
        });
        prior_thread = ThreadId::new(format!("thread-{generation}"));
        task_store.insert_task(current.clone()).unwrap();
        // 模型 transcript 已不含原调用；恢复只能依赖 canonical 与 Task checkpoint。
        let ledger = ToolExecutionLedger::from_canonical_task(
            &canonical,
            &task_store,
            &SessionId::new("recovery-session"),
            &current,
            &prior_thread,
            None,
        )
        .unwrap()
        .ledger;
        assert!(matches!(
            ledger.plan(
                &[call(
                    "new-publish",
                    "shell_exec",
                    r#"{"command":"publish"}"#
                )],
                None,
            )[0],
            ToolCallExecutionDecision::RecoveryBlocked { .. }
        ));
    }
}

#[test]
fn executor_owned_source_cannot_be_replayed_after_switching_to_a_native_model() {
    let original = recovery_task();
    let canonical = canonical_history(&original, &[("indeterminate", None)]);
    let mut durable = canonical.durable_state();
    let now = magi_core::UtcMillis(1);
    let mut caps = magi_bridge_client::text_executor_capabilities();
    caps.context = magi_bridge_client::ExecutorContextOwnership::ExecutorOwned;
    durable.canonical_turns[0]
        .model_steps
        .push(magi_session_store::ModelStep {
            step_id: "external".into(),
            attempts: vec![magi_session_store::ModelAttempt {
                transport: None,
                attempt_id: "external:attempt:1".into(),
                started_at: now,
                settled_at: None,
                status: magi_session_store::ModelStepStatus::Started,
            }],
            task_id: Some(original.task_id.clone()),
            goal_id: None,
            usage_tokens: None,
            goal_tokens_after: None,
            thread_id: ThreadId::new("recovery-thread"),
            checkpoint_generation: 0,
            request_fingerprint: "a".repeat(64),
            provider: "external".into(),
            binding: None,
            executor_capabilities: Some(caps),
            purpose: Some(magi_session_store::ModelStepPurpose::Execution),
            message_count: 1,
            tool_count: 0,
            max_output_tokens: None,
            input_request_ids: vec![],
            started_at: now,
            settled_at: None,
            status: magi_session_store::ModelStepStatus::Started,
            response_fingerprint: None,
            prompt_frame: None,
            cache_epoch: 0,
        });
    let canonical =
        SessionStore::from_persisted_parts(durable, canonical.execution_sidecar_store_state())
            .unwrap();
    let tasks = TaskStore::new();
    tasks.insert_task(original.clone()).unwrap();
    let mut resumed = original.clone();
    resumed.task_id = magi_core::TaskId::new("continued");
    resumed.root_task_id = resumed.task_id.clone();
    resumed.recovery_checkpoint = Some(magi_core::TaskRecoveryCheckpoint {
        source_session_id: SessionId::new("recovery-session"),
        source_task_id: original.task_id,
        source_turn_id: "turn-0".into(),
        source_thread_id: ThreadId::new("recovery-thread"),
    });
    let error = ToolExecutionLedger::from_canonical_task(
        &canonical,
        &tasks,
        &SessionId::new("recovery-session"),
        &resumed,
        &ThreadId::new("continued-thread"),
        None,
    )
    .unwrap_err();
    assert!(error.contains("executor_resume_unsupported"));
}

#[test]
fn incomplete_or_invalid_recovery_lineage_fails_closed() {
    let original = recovery_task();
    let canonical = canonical_history(&original, &[("indeterminate", None)]);
    let mut current = original.clone();
    current.task_id = magi_core::TaskId::new("continued");
    current.root_task_id = current.task_id.clone();
    current.recovery_checkpoint = Some(magi_core::TaskRecoveryCheckpoint {
        source_session_id: SessionId::new("recovery-session"),
        source_task_id: original.task_id.clone(),
        source_turn_id: "turn-0".into(),
        source_thread_id: ThreadId::new("recovery-thread"),
    });
    let store = TaskStore::new();
    let recover = |task: &Task| {
        ToolExecutionLedger::from_canonical_task(
            &canonical,
            &store,
            &SessionId::new("recovery-session"),
            task,
            &ThreadId::new("continued-thread"),
            None,
        )
    };
    assert!(recover(&current).unwrap_err().contains("缺失"));
    let mut wrong_mission = original.clone();
    wrong_mission.mission_id = magi_core::MissionId::new("other");
    store.insert_task(wrong_mission).unwrap();
    assert!(recover(&current).unwrap_err().contains("mission"));
    current
        .recovery_checkpoint
        .as_mut()
        .unwrap()
        .source_session_id = SessionId::new("other");
    assert!(recover(&current).unwrap_err().contains("跨会话"));
    current
        .recovery_checkpoint
        .as_mut()
        .unwrap()
        .source_session_id = SessionId::new("recovery-session");
    current.recovery_checkpoint.as_mut().unwrap().source_task_id = current.task_id.clone();
    assert!(recover(&current).unwrap_err().contains("循环"));
}

fn call(id: &str, name: &str, arguments: &str) -> ChatToolCall {
    ChatToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn persisted_tool_history(call: &ChatToolCall, result: String) -> Vec<ThreadChatMessage> {
    vec![
        ThreadChatMessage {
            input_request_id: None,
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: call.id.clone(),
                kind: call.kind.clone(),
                function: ThreadChatToolFunction {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(result),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call.id.clone()),
            provider_context: Vec::new(),
        },
    ]
}

#[test]
fn switching_execution_directory_invalidates_reads_and_known_unexecuted_calls_can_retry() {
    let read = call("read-old", "file_read", r#"{"path":"a.txt"}"#);
    let write = call(
        "write-old",
        "file_write",
        r#"{"path":"a.txt","content":"new"}"#,
    );
    let mut ledger = ToolExecutionLedger::for_task_goal("read and write");
    let planned = ledger.plan(std::slice::from_ref(&read), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &planned[0] else {
        panic!("first read must execute");
    };
    ledger.record_execution(
        &read,
        fingerprint.as_ref(),
        &(
            r#"{"status":"succeeded","content":"old"}"#.into(),
            ExecutionResultStatus::Succeeded,
        ),
    );
    assert!(matches!(
        ledger.plan(std::slice::from_ref(&read), None)[0],
        ToolCallExecutionDecision::Reuse { .. }
    ));
    ledger.rebind_workspace_root(Some(Path::new("/isolated")));
    assert!(matches!(
        ledger.plan(&[read], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
    let history = persisted_tool_history(&write, r#"{"status":"failed","execution_started":false,"error_code":"workspace_execution_root_changed"}"#.into());
    let recovered = ToolExecutionLedger::from_thread_history("read and write", &history, None);
    assert!(matches!(
        recovered.plan(&[write], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}

#[test]
fn restores_file_read_only_when_content_hash_is_current() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("facts.txt");
    std::fs::write(&path, "stable fact").expect("write fixture");
    let content_hash = magi_snapshot::path_content_hash(&path).expect("hash fixture");
    let file_read = call(
        "call-file-read",
        "file_read",
        &serde_json::json!({"path": path}).to_string(),
    );
    let result = serde_json::json!({
        "tool": "file_read",
        "status": "succeeded",
        "path": path,
        "content_hash": content_hash,
        "content": "stable fact"
    })
    .to_string();
    let history = persisted_tool_history(&file_read, result);
    let ledger = ToolExecutionLedger::from_thread_history("继续", &history, None);
    assert!(matches!(
        ledger.plan(std::slice::from_ref(&file_read), None)[0],
        ToolCallExecutionDecision::Reuse { .. }
    ));

    std::fs::write(&path, "changed fact").expect("change fixture");
    let ledger = ToolExecutionLedger::from_thread_history("继续", &history, None);
    assert!(matches!(
        ledger.plan(&[file_read], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}

#[test]
fn successful_write_invalidates_file_read_fact_in_same_turn() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("facts.txt");
    std::fs::write(&path, "stable fact").expect("write fixture");
    let file_read = call(
        "call-file-read",
        "file_read",
        &serde_json::json!({"path": path}).to_string(),
    );
    let mut ledger = ToolExecutionLedger::default();
    let read_plan = ledger.plan(std::slice::from_ref(&file_read), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &read_plan[0] else {
        panic!("first file read must execute");
    };
    let content_hash = magi_snapshot::path_content_hash(&path).expect("hash fixture");
    ledger.record_execution(
        &file_read,
        fingerprint.as_ref(),
        &(
            serde_json::json!({
                "tool": "file_read",
                "status": "succeeded",
                "path": path,
                "content_hash": content_hash,
                "content": "stable fact"
            })
            .to_string(),
            ExecutionResultStatus::Succeeded,
        ),
    );
    let file_write = call(
        "call-file-write",
        "file_write",
        &serde_json::json!({"path": path, "content": "changed fact"}).to_string(),
    );
    ledger.record_execution(
        &file_write,
        None,
        &("ok".to_string(), ExecutionResultStatus::Succeeded),
    );
    assert!(matches!(
        ledger.plan(&[file_read], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}

#[test]
fn executed_write_invalidates_every_read_only_result_in_same_turn() {
    let mut ledger = ToolExecutionLedger::default();
    let search = call("call-search", "search_text", r#"{"pattern":"TODO"}"#);
    let git_status = call("call-git-status", "git_status", "{}");
    for read in [&search, &git_status] {
        let plan = ledger.plan(std::slice::from_ref(read), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &plan[0] else {
            panic!("first read call must execute");
        };
        ledger.record_execution(
            read,
            fingerprint.as_ref(),
            &(
                r#"{"status":"succeeded","matches":[]}"#.to_string(),
                ExecutionResultStatus::Succeeded,
            ),
        );
    }
    // 失败的命令也可能已经改动了工作区，同样必须让只读结果失效。
    ledger.record_execution(
        &call(
            "call-shell",
            "shell_exec",
            r#"{"command":"sed -i s/a/b/ x"}"#,
        ),
        None,
        &("exit 1".to_string(), ExecutionResultStatus::Failed),
    );

    assert!(
        ledger
            .plan(&[search, git_status], None)
            .iter()
            .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. })),
        "写操作之后 search_text / git_status 必须重新执行"
    );
}

#[test]
fn restored_history_reuses_only_current_turn_reads_issued_after_last_write() {
    let previous_turn_status = call("call-prev-status", "git_status", "{}");
    let current_search = call("call-search", "search_text", r#"{"pattern":"TODO"}"#);
    let current_write = call(
        "call-write",
        "file_write",
        r#"{"path":"a.txt","content":"changed"}"#,
    );
    let current_listing = call("call-branches", "git_branch_list", "{}");
    let read_result = r#"{"status":"succeeded","matches":[]}"#.to_string();
    let mut history = persisted_tool_history(&previous_turn_status, read_result.clone());
    history.push(ThreadChatMessage {
        input_request_id: None,
        role: "user".to_string(),
        content: Some("继续修改".to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    });
    history.extend(persisted_tool_history(&current_search, read_result.clone()));
    history.extend(persisted_tool_history(
        &current_write,
        r#"{"status":"succeeded"}"#.to_string(),
    ));
    history.extend(persisted_tool_history(&current_listing, read_result));

    let ledger = ToolExecutionLedger::from_thread_history("继续修改", &history, None);
    let decisions = ledger.plan(
        &[previous_turn_status, current_search, current_listing],
        None,
    );

    assert!(
        matches!(decisions[0], ToolCallExecutionDecision::Execute { .. }),
        "上一轮的 git_status 不能复用到本轮"
    );
    assert!(
        matches!(decisions[1], ToolCallExecutionDecision::Execute { .. }),
        "写入之前的 search_text 结果已过期"
    );
    assert!(
        matches!(decisions[2], ToolCallExecutionDecision::Reuse { .. }),
        "写入之后的只读结果仍可复用"
    );
}

#[test]
fn reuses_successful_read_only_call_across_model_rounds() {
    let mut ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
    let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
    let first_plan = ledger.plan(std::slice::from_ref(&first), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
        panic!("first read call must execute");
    };
    let result = (
        r#"{"tool":"web_search","status":"succeeded","results":["Rust"]}"#.to_string(),
        ExecutionResultStatus::Succeeded,
    );
    ledger.record_execution(&first, fingerprint.as_ref(), &result);

    let repeat = call("call-2", "web_search", r#"{"query":"Rust"}"#);
    assert!(matches!(
        ledger.plan(&[repeat], None)[0],
        ToolCallExecutionDecision::Reuse { .. }
    ));
}

#[test]
fn get_goal_is_never_reused_across_state_mutations() {
    let mut ledger = ToolExecutionLedger::for_task_goal("推进 Goal");
    let first = call("call-goal-1", "get_goal", "{}");
    let first_plan = ledger.plan(std::slice::from_ref(&first), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
        panic!("first get_goal must execute");
    };
    assert!(fingerprint.is_none());
    ledger.record_execution(
        &first,
        None,
        &(
            r#"{"tool":"get_goal","status":"succeeded","goal":null,"plan":null}"#.to_string(),
            ExecutionResultStatus::Succeeded,
        ),
    );

    let repeat = call("call-goal-2", "get_goal", "{}");
    assert!(matches!(
        ledger.plan(&[repeat], None)[0],
        ToolCallExecutionDecision::Execute { fingerprint: None }
    ));
}

#[test]
fn browser_observations_are_never_reused_across_page_state_changes() {
    for tool_name in ["browser_snapshot", "browser_screenshot"] {
        let mut ledger = ToolExecutionLedger::for_task_goal("验证浏览器交互前后状态");
        let first = call("call-browser-1", tool_name, r#"{"full_page":false}"#);
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("首次 {tool_name} 必须执行");
        };
        assert!(fingerprint.is_none(), "{tool_name} 不应进入可复用结果账本");
        ledger.record_execution(
            &first,
            None,
            &(
                serde_json::json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "page_revision": 1,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            ),
        );

        let repeat = call("call-browser-2", tool_name, r#"{"full_page":false}"#);
        assert!(matches!(
            ledger.plan(&[repeat], None)[0],
            ToolCallExecutionDecision::Execute { fingerprint: None }
        ));
    }
}

#[test]
fn treats_object_key_order_as_the_same_idempotent_call() {
    let ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
    let calls = [
        call(
            "call-1",
            "web_search",
            r#"{"query":"Rust","locale":"zh-CN"}"#,
        ),
        call(
            "call-2",
            "web_search",
            r#"{"locale":"zh-CN","query":"Rust"}"#,
        ),
    ];
    let plan = ledger.plan(&calls, None);
    assert!(matches!(
        plan[1],
        ToolCallExecutionDecision::ReuseAfterExecution {
            source_index: 0,
            ..
        }
    ));
}

#[test]
fn honors_user_single_call_limit_for_different_arguments() {
    let mut ledger =
        ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
    let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
    let first_plan = ledger.plan(std::slice::from_ref(&first), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
        panic!("first call must execute");
    };
    ledger.record_execution(
        &first,
        fingerprint.as_ref(),
        &("ok".to_string(), ExecutionResultStatus::Succeeded),
    );

    let second = call("call-2", "web_search", r#"{"query":"Cargo"}"#);
    assert!(matches!(
        ledger.plan(&[second], None)[0],
        ToolCallExecutionDecision::UserCallLimitReached { .. }
    ));
}

#[test]
fn applies_user_single_call_limit_within_one_model_response() {
    let ledger = ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
    let calls = [
        call("call-1", "web_search", r#"{"query":"Rust"}"#),
        call("call-2", "web_search", r#"{"query":"Cargo"}"#),
    ];
    let plan = ledger.plan(&calls, None);
    assert!(matches!(plan[0], ToolCallExecutionDecision::Execute { .. }));
    assert!(matches!(
        plan[1],
        ToolCallExecutionDecision::UserCallLimitReached { .. }
    ));
}

#[test]
fn additional_call_instruction_does_not_create_single_call_limit() {
    let ledger = ToolExecutionLedger::for_task_goal(
        "先调用 shell_exec 探查；权限失败时再调用一次 shell_exec 验证止损。",
    );
    let calls = [
        call("call-1", "shell_exec", r#"{"command":"pwd"}"#),
        call("call-2", "shell_exec", r#"{"command":"printf ok"}"#),
    ];

    assert!(
        ledger
            .plan(&calls, None)
            .iter()
            .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. }))
    );
}

#[test]
fn never_deduplicates_write_or_process_operations() {
    let ledger = ToolExecutionLedger::for_task_goal("执行写入");
    let calls = [
        call("call-1", "file_write", r#"{"path":"a","content":"x"}"#),
        call("call-2", "file_write", r#"{"path":"a","content":"x"}"#),
    ];
    assert!(
        ledger
            .plan(&calls, None)
            .iter()
            .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. }))
    );
}

#[test]
fn restores_successful_read_call_from_persisted_thread_history() {
    let history = vec![
        ThreadChatMessage {
            input_request_id: None,
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: "call-read".to_string(),
                kind: "function".to_string(),
                function: ThreadChatToolFunction {
                    name: "web_search".to_string(),
                    arguments: r#"{"query":"Rust"}"#.to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(
                r#"{"tool":"web_search","status":"succeeded","results":["Rust"]}"#.to_string(),
            ),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-read".to_string()),
            provider_context: Vec::new(),
        },
    ];

    let ledger = ToolExecutionLedger::from_thread_history("搜索 Rust", &history, None);
    assert!(matches!(
        ledger.plan(
            &[call("call-repeat", "web_search", r#"{"query":"Rust"}"#)],
            None
        )[0],
        ToolCallExecutionDecision::Reuse { .. }
    ));
}

#[test]
fn blocks_replaying_unknown_write_call_after_interruption() {
    let history = vec![ThreadChatMessage {
        input_request_id: None,
        role: "assistant".to_string(),
        content: None,
        images: Vec::new(),
        tool_calls: vec![ThreadChatToolCall {
            id: "call-write".to_string(),
            kind: "function".to_string(),
            function: ThreadChatToolFunction {
                name: "file_write".to_string(),
                arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
            },
        }],
        tool_call_id: None,
        provider_context: Vec::new(),
    }];

    let ledger = ToolExecutionLedger::from_thread_history("写入文件", &history, None);
    let decision = ledger
        .plan(
            &[call(
                "call-write-repeat",
                "file_write",
                r#"{"path":"a.txt","content":"x"}"#,
            )],
            None,
        )
        .remove(0);
    assert!(matches!(
        decision,
        ToolCallExecutionDecision::RecoveryBlocked { .. }
    ));
    assert_eq!(
        decision
            .immediate_result()
            .expect("blocked decision should resolve immediately")
            .1,
        ExecutionResultStatus::Failed
    );
}

#[test]
fn reuses_successful_write_call_restored_from_thread_history() {
    let history = vec![
        ThreadChatMessage {
            input_request_id: None,
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: "call-write".to_string(),
                kind: "function".to_string(),
                function: ThreadChatToolFunction {
                    name: "file_write".to_string(),
                    arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(
                r#"{"tool":"file_write","status":"succeeded","changed_paths":["a.txt"]}"#
                    .to_string(),
            ),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-write".to_string()),
            provider_context: Vec::new(),
        },
    ];

    let ledger = ToolExecutionLedger::from_thread_history("写入文件", &history, None);
    let decision = ledger
        .plan(
            &[call(
                "call-write-repeat",
                "file_write",
                r#"{"path":"a.txt","content":"x"}"#,
            )],
            None,
        )
        .remove(0);
    let ToolCallExecutionDecision::Reuse { result } = decision else {
        panic!("恢复后相同写操作必须复用已完成结果");
    };
    let payload: Value = serde_json::from_str(&result).expect("reuse result should be json");
    assert_eq!(payload["execution"], "reused");
    assert_eq!(payload["reason"], "completed_before_recovery");
}

#[test]
fn new_user_turn_does_not_reuse_prior_side_effect_or_call_limit() {
    let mut history = vec![ThreadChatMessage {
        input_request_id: None,
        role: "user".to_string(),
        content: Some("写入文件".to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }];
    let write = call(
        "call-write-prior-turn",
        "file_write",
        r#"{"path":"a.txt","content":"x"}"#,
    );
    history.extend(persisted_tool_history(
        &write,
        r#"{"tool":"file_write","status":"succeeded","changed_paths":["a.txt"]}"#.to_string(),
    ));
    history.push(ThreadChatMessage {
        input_request_id: None,
        role: "user".to_string(),
        content: Some("再次写入文件".to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    });

    let ledger = ToolExecutionLedger::from_thread_history(
        "只调用一次 file_write，再次写入文件",
        &history,
        None,
    );
    assert!(matches!(
        ledger.plan(
            &[call(
                "call-write-current-turn",
                "file_write",
                r#"{"path":"a.txt","content":"x"}"#,
            )],
            None,
        )[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}

#[test]
fn new_user_turn_reuses_current_file_fact_but_invalidates_changed_file() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("facts.txt");
    std::fs::write(&path, "stable fact").expect("write fixture");
    let file_read = call(
        "call-read-prior-turn",
        "file_read",
        &serde_json::json!({"path": path}).to_string(),
    );
    let mut history = vec![ThreadChatMessage {
        input_request_id: None,
        role: "user".to_string(),
        content: Some("读取事实".to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }];
    history.extend(persisted_tool_history(
        &file_read,
        serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "path": path,
            "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
            "content": "stable fact"
        })
        .to_string(),
    ));
    history.push(ThreadChatMessage {
        input_request_id: None,
        role: "user".to_string(),
        content: Some("继续使用事实".to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    });

    let ledger = ToolExecutionLedger::from_thread_history("继续使用事实", &history, None);
    assert!(matches!(
        ledger.plan(std::slice::from_ref(&file_read), None)[0],
        ToolCallExecutionDecision::Reuse { .. }
    ));

    std::fs::write(&path, "changed fact").expect("change fixture");
    let ledger = ToolExecutionLedger::from_thread_history("继续使用事实", &history, None);
    assert!(matches!(
        ledger.plan(&[file_read], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}

#[test]
fn session_file_fact_reuse_requires_the_same_read_range_and_budget() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("Cargo.toml");
    let content = "[workspace]\nresolver = \"2\"\n";
    std::fs::write(&path, content).expect("write fixture");
    let fact = CurrentFileFact {
        absolute_path: path.display().to_string(),
        content_hash: magi_snapshot::path_content_hash(&path).expect("hash fixture"),
        result: serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "mode": "file",
            "path": path,
            "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
            "file_size_bytes": content.len(),
            "max_bytes": 4096,
            "start_line": 1,
            "requested_end_line": null,
            "bytes_read": content.len(),
            "truncated": false,
            "encoding": "utf-8-lossy",
            "content": content,
            "summary": "已读取文件"
        })
        .to_string(),
        summary: content.to_string(),
    };
    let ledger = ToolExecutionLedger::for_task_goal("继续使用文件事实")
        .with_current_file_facts(&[fact], Some(directory.path()));
    for arguments in [
        serde_json::json!({"path":"Cargo.toml", "max_bytes":12}),
        serde_json::json!({"path":"Cargo.toml", "max_bytes":4096, "start_line":2}),
        serde_json::json!({"path":"Cargo.toml", "max_bytes":4096, "end_line":1}),
        serde_json::json!({"path":"Cargo.toml"}),
    ] {
        let request = call("range", "file_read", &arguments.to_string());
        assert!(matches!(
            ledger.plan(&[request], None)[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }
    let request = call(
        "same-range",
        "file_read",
        r#"{"path":"Cargo.toml","max_bytes":4096}"#,
    );
    let ToolCallExecutionDecision::Reuse { result } = &ledger.plan(&[request], None)[0] else {
        panic!("相同范围应复用");
    };
    let payload: Value = serde_json::from_str(result).unwrap();
    assert_eq!(payload["source_result"]["content"], content);
}

#[test]
fn project_file_facts_resolve_each_selected_root_and_invalidate_changed_maps() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let roots = [root.join("first"), root.join("second"), root.join("copy")];
    let mut facts = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        std::fs::create_dir(root).unwrap();
        let path = root.join("same.txt");
        let content = format!("root {index}");
        std::fs::write(&path, &content).unwrap();
        let hash = magi_snapshot::path_content_hash(&path).unwrap();
        facts.push(CurrentFileFact {
                absolute_path: path.display().to_string(), content_hash: hash.clone(),
                result: serde_json::json!({"tool":"file_read","status":"succeeded","mode":"file",
                    "path":path,"content_hash":hash,"file_size_bytes":content.len(),"max_bytes":4096,
                    "bytes_read":content.len(),"truncated":false,"start_line":1,
                    "requested_end_line":null,"content":content}).to_string(),
                summary: content,
            });
    }
    let binding = magi_core::ProjectDirectoryBinding {
        workspace_id: magi_core::WorkspaceId::new("facts-project"),
        project_revision: 1,
        default_source_folder_id: "first".into(),
        source_folders: ["first", "second"]
            .iter()
            .zip(&roots)
            .map(|(id, root)| magi_core::ProjectSourceFolderGrant {
                source_folder_id: (*id).into(),
                source_path_ref: magi_core::HostPath::from_path(root.clone())
                    .to_path_ref()
                    .as_str()
                    .into(),
            })
            .collect(),
    };
    let map = magi_core::ProjectDirectoryExecutionMap::from_binding(&binding, |grant| {
        Ok(roots[usize::from(grant.source_folder_id == "second")].clone())
    })
    .unwrap();
    let mut ledger =
        ToolExecutionLedger::default().with_current_file_facts(&facts, Some(&roots[0]));
    ledger.rebind_project_directories(Some(&map));
    for (index, id) in ["first", "second"].iter().enumerate() {
        let request = call(
            id,
            "file_read",
            &serde_json::json!({"source_folder_id":id,"path":"same.txt","max_bytes":4096})
                .to_string(),
        );
        let plan = ledger.plan(&[request], None);
        let ToolCallExecutionDecision::Reuse { result } = &plan[0] else {
            panic!("must reuse matching root")
        };
        assert_eq!(
            serde_json::from_str::<Value>(result).unwrap()["source_result"]["content"],
            format!("root {index}")
        );
    }
    let read = call(
        "read-second",
        "search_text",
        r#"{"source_folder_id":"second","query":"root"}"#,
    );
    let decision = ledger.plan(std::slice::from_ref(&read), None);
    let ToolCallExecutionDecision::Execute { fingerprint } = &decision[0] else {
        panic!("first search")
    };
    ledger.record_execution(
        &read,
        fingerprint.as_ref(),
        &(
            r#"{"status":"succeeded"}"#.into(),
            ExecutionResultStatus::Succeeded,
        ),
    );
    let mut changed = map;
    changed.roots[1].execution_root_ref = magi_core::HostPath::from_path(roots[2].clone())
        .to_path_ref()
        .as_str()
        .into();
    ledger.rebind_project_directories(Some(&changed));
    assert!(matches!(
        ledger.plan(&[read], None)[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
    let read = call(
        "copy",
        "file_read",
        r#"{"source_folder_id":"second","path":"same.txt","max_bytes":4096}"#,
    );
    let plan = ledger.plan(&[read], None);
    let ToolCallExecutionDecision::Reuse { result } = &plan[0] else {
        panic!("must use copy fact")
    };
    assert_eq!(
        serde_json::from_str::<Value>(result).unwrap()["source_result"]["content"],
        "root 2"
    );
}

#[test]
fn new_task_does_not_reuse_session_file_fact_after_external_change() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("facts.txt");
    std::fs::write(&path, "stable fact").expect("write fixture");
    let fact = CurrentFileFact {
        absolute_path: path.display().to_string(),
        content_hash: magi_snapshot::path_content_hash(&path).expect("hash fixture"),
        result: serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "mode": "file",
            "path": path,
            "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
            "file_size_bytes": 11,
            "max_bytes": 4096,
            "bytes_read": 11,
            "truncated": false,
            "content": "stable fact"
        })
        .to_string(),
        summary: "stable fact".to_string(),
    };
    std::fs::write(&path, "changed fact").expect("change fixture");
    let ledger = ToolExecutionLedger::for_task_goal("继续使用文件事实")
        .with_current_file_facts(&[fact], Some(directory.path()));
    assert!(matches!(
        ledger.plan(
            &[call(
                "call-changed-fact",
                "file_read",
                r#"{"path":"facts.txt"}"#,
            )],
            None,
        )[0],
        ToolCallExecutionDecision::Execute { .. }
    ));
}
