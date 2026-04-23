use crate::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeBindingReference, BridgeClientError,
    BridgeDispatchAction, BridgeDispatchInput, BridgeDispatchRuntime, BridgeResponse,
    BridgeTransport, BridgeTransportError, BridgeTransportRequest, BridgeTransportResponse,
    HostBridgeClient, HostBridgeCommand, HostBridgeRequest, HostKind, JsonRpcHostBridgeClient,
    JsonRpcMcpBridgeClient, JsonRpcModelBridgeClient, JsonRpcStdioTransport, McpBridgeClient,
    McpToolCallRequest, ModelBridgeClient, ModelInvocationRequest,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct DummyHostClient;

impl HostBridgeClient for DummyHostClient {
    fn call(&self, request: HostBridgeRequest) -> Result<BridgeResponse, BridgeClientError> {
        Ok(BridgeResponse {
            ok: true,
            payload: format!("{:?}", request.command),
        })
    }
}

struct RecordingTransport {
    calls: Mutex<Vec<BridgeTransportRequest>>,
    response: Value,
}

impl RecordingTransport {
    fn new(response: Value) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response,
        }
    }

    fn calls(&self) -> Vec<BridgeTransportRequest> {
        self.calls.lock().expect("lock poisoned").clone()
    }
}

impl BridgeTransport for RecordingTransport {
    fn call(
        &self,
        request: BridgeTransportRequest,
    ) -> Result<BridgeTransportResponse, BridgeTransportError> {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(request.clone());
        Ok(BridgeTransportResponse {
            payload: self.response.clone(),
        })
    }
}

#[test]
fn host_binding_rejects_unknown_target() {
    let runtime = BridgeDispatchRuntime::new().with_host_client(Arc::new(DummyHostClient));
    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec!["skill-a".to_string()],
        bindings: vec![BridgeBindingReference {
            binding_id: "binding-a".to_string(),
            tool_name: "host.exec".to_string(),
            bridge_kind: BridgeBindingKind::Host,
            dispatch_action: BridgeDispatchAction::HostTerminalExec,
            bridge_target: "linux".to_string(),
        }],
    };

    let error = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "binding-a".to_string(),
                payload: "echo hello".to_string(),
                working_directory: Some("/tmp".to_string()),
            },
        )
        .expect_err("invalid host target should be rejected");

    match error {
        BridgeClientError::InvalidBindingTarget {
            binding_id,
            bridge_target,
        } => {
            assert_eq!(binding_id, "binding-a");
            assert_eq!(bridge_target, "linux");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn incompatible_kind_action_is_rejected() {
    let runtime = BridgeDispatchRuntime::new();
    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec!["skill-a".to_string()],
        bindings: vec![BridgeBindingReference {
            binding_id: "binding-a".to_string(),
            tool_name: "model.prompt".to_string(),
            bridge_kind: BridgeBindingKind::Model,
            dispatch_action: BridgeDispatchAction::HostTerminalExec,
            bridge_target: "openai".to_string(),
        }],
    };

    let error = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "binding-a".to_string(),
                payload: "hi".to_string(),
                working_directory: None,
            },
        )
        .expect_err("incompatible binding/action should be rejected");

    match error {
        BridgeClientError::IncompatibleBindingAction {
            binding_id,
            bridge_kind,
            dispatch_action,
        } => {
            assert_eq!(binding_id, "binding-a");
            assert_eq!(bridge_kind, BridgeBindingKind::Model);
            assert_eq!(dispatch_action, BridgeDispatchAction::HostTerminalExec);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn stdio_transport_round_trips_json_rpc_response() {
    let script = r#"read -r _line; printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"ok":true,"payload":"transport:ok"}}'"#;
    let transport =
        JsonRpcStdioTransport::new("sh").with_args(vec!["-c".to_string(), script.to_string()]);

    let response = transport
        .call(BridgeTransportRequest {
            method: "host.call".to_string(),
            params: json!({"hello":"world"}),
        })
        .expect("stdio transport should return a response");

    assert_eq!(response.payload["ok"], true);
    assert_eq!(response.payload["payload"], "transport:ok");
}

#[test]
fn stdio_transport_reports_protocol_and_remote_business_errors() {
    let protocol_transport = JsonRpcStdioTransport::new("sh").with_args(vec![
        "-c".to_string(),
        r#"read -r _line; printf '%s\n' 'not-json'"#.to_string(),
    ]);

    let protocol_error = protocol_transport
        .call(BridgeTransportRequest {
            method: "model.invoke".to_string(),
            params: json!({"prompt":"hello"}),
        })
        .expect_err("invalid payload should be protocol error");

    assert!(matches!(
        protocol_error,
        BridgeTransportError::Protocol { .. }
    ));

    let remote_transport = JsonRpcStdioTransport::new("sh").with_args(vec![
        "-c".to_string(),
        r#"read -r _line; printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"denied","data":{"reason":"policy"}}}'"#.to_string(),
    ]);

    let remote_error = remote_transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({"tool_name":"echo"}),
        })
        .expect_err("remote error should be surfaced");

    match remote_error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32001);
            assert_eq!(message, "denied");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn json_rpc_clients_share_the_same_transport_abstraction() {
    let transport = Arc::new(RecordingTransport::new(json!({
        "ok": true,
        "payload": "shared"
    })));

    let host = JsonRpcHostBridgeClient::new(transport.clone());
    let model = JsonRpcModelBridgeClient::new(transport.clone());
    let mcp = JsonRpcMcpBridgeClient::new(transport.clone());

    assert_eq!(
        host.call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        })
        .expect("host call should succeed")
        .payload,
        "shared"
    );
    assert_eq!(
        model
            .invoke(ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("model call should succeed")
            .payload,
        "shared"
    );
    assert_eq!(
        mcp.call_tool(McpToolCallRequest {
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            input: "{}".to_string(),
        })
        .expect("mcp call should succeed")
        .payload,
        "shared"
    );

    let calls = transport.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].method, "host.call");
    assert_eq!(calls[1].method, "model.invoke");
    assert_eq!(calls[2].method, "mcp.call_tool");
}

#[test]
fn dispatch_runtime_with_json_rpc_clients_is_end_to_end() {
    let transport = Arc::new(RecordingTransport::new(json!({
        "ok": true,
        "payload": "dispatch"
    })));

    let runtime = BridgeDispatchRuntime::new()
        .with_host_client(Arc::new(JsonRpcHostBridgeClient::new(transport.clone())))
        .with_model_client(Arc::new(JsonRpcModelBridgeClient::new(transport.clone())))
        .with_mcp_client(Arc::new(JsonRpcMcpBridgeClient::new(transport.clone())));

    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec!["skill-a".to_string()],
        bindings: vec![
            BridgeBindingReference {
                binding_id: "host-binding".to_string(),
                tool_name: "host.exec".to_string(),
                bridge_kind: BridgeBindingKind::Host,
                dispatch_action: BridgeDispatchAction::HostTerminalExec,
                bridge_target: "vscode".to_string(),
            },
            BridgeBindingReference {
                binding_id: "model-binding".to_string(),
                tool_name: "model.prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            },
            BridgeBindingReference {
                binding_id: "mcp-binding".to_string(),
                tool_name: "mcp.call".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "server-a".to_string(),
            },
        ],
    };

    let host = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "host-binding".to_string(),
                payload: "echo hello".to_string(),
                working_directory: Some("/tmp".to_string()),
            },
        )
        .expect("host dispatch should succeed");
    assert_eq!(host.response.payload, "dispatch");

    let model = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "model-binding".to_string(),
                payload: "hello".to_string(),
                working_directory: None,
            },
        )
        .expect("model dispatch should succeed");
    assert_eq!(model.response.payload, "dispatch");

    let mcp = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "mcp-binding".to_string(),
                payload: "{}".to_string(),
                working_directory: None,
            },
        )
        .expect("mcp dispatch should succeed");
    assert_eq!(mcp.response.payload, "dispatch");
}

// ============================================================================
// Phase 4: Orchestrator Termination Tests
// ============================================================================

#[test]
fn termination_resolution_picks_highest_priority() {
    use crate::orchestrator_termination::*;

    let candidates = vec![
        TerminationCandidate {
            reason: OrchestratorTerminationReason::Completed,
            event_id: "e1".to_string(),
            triggered_at: 100,
        },
        TerminationCandidate {
            reason: OrchestratorTerminationReason::Cancelled,
            event_id: "e2".to_string(),
            triggered_at: 200,
        },
        TerminationCandidate {
            reason: OrchestratorTerminationReason::BudgetExceeded,
            event_id: "e3".to_string(),
            triggered_at: 150,
        },
    ];

    let result = resolve_termination_reason(&candidates, OrchestratorTerminationReason::Completed);
    assert_eq!(result.reason, OrchestratorTerminationReason::Cancelled);
    assert_eq!(result.evidence_ids, vec!["e2"]);
}

#[test]
fn termination_resolution_uses_fallback_on_empty() {
    use crate::orchestrator_termination::*;
    let result = resolve_termination_reason(&[], OrchestratorTerminationReason::Failed);
    assert_eq!(result.reason, OrchestratorTerminationReason::Failed);
    assert!(result.evidence_ids.is_empty());
}

#[test]
fn progress_evaluation_detects_progress() {
    use crate::orchestrator_termination::*;

    let prev = TerminationSnapshot {
        snapshot_id: "s1".into(),
        plan_id: "p1".into(),
        attempt_seq: 1,
        progress_vector: ProgressVector {
            terminal_required_tasks: 2,
            accepted_criteria: 1,
            critical_path_resolved: 0,
            unresolved_blockers: 3,
        },
        review_state: ReviewState::default(),
        blocker_state: BlockerState::default(),
        budget_state: BudgetState::default(),
        cache_state: None,
        cp_version: 1,
        required_total: 5,
        failed_required: 0,
        running_or_pending_required: 3,
        running_required: Some(1),
        source_event_ids: vec![],
        computed_at: 0,
    };

    let curr = TerminationSnapshot {
        progress_vector: ProgressVector {
            terminal_required_tasks: 3,
            accepted_criteria: 2,
            critical_path_resolved: 0,
            unresolved_blockers: 2,
        },
        ..prev.clone()
    };

    let eval = evaluate_progress(Some(&prev), &curr);
    assert!(eval.progressed);
    assert!(!eval.regressed);
}

#[test]
fn progress_evaluation_detects_regression() {
    use crate::orchestrator_termination::*;

    let prev = TerminationSnapshot {
        snapshot_id: "s1".into(),
        plan_id: "p1".into(),
        attempt_seq: 1,
        progress_vector: ProgressVector {
            terminal_required_tasks: 3,
            accepted_criteria: 2,
            critical_path_resolved: 0,
            unresolved_blockers: 1,
        },
        review_state: ReviewState::default(),
        blocker_state: BlockerState::default(),
        budget_state: BudgetState::default(),
        cache_state: None,
        cp_version: 1,
        required_total: 5,
        failed_required: 0,
        running_or_pending_required: 2,
        running_required: None,
        source_event_ids: vec![],
        computed_at: 0,
    };

    let curr = TerminationSnapshot {
        progress_vector: ProgressVector {
            terminal_required_tasks: 2,
            accepted_criteria: 1,
            critical_path_resolved: 0,
            unresolved_blockers: 3,
        },
        ..prev.clone()
    };

    let eval = evaluate_progress(Some(&prev), &curr);
    assert!(!eval.progressed);
    assert!(eval.regressed);
}

// ============================================================================
// Phase 4: Decision Engine Tests
// ============================================================================

fn test_policy() -> crate::decision_engine::OrchestratorDecisionPolicy {
    crate::decision_engine::OrchestratorDecisionPolicy {
        stalled_window_size: 5,
        external_wait_sla_ms: 30_000,
        upstream_model_error_streak: 3,
        error_rate_min_samples: 5,
        budget_no_progress_streak_threshold: 3,
        budget_breach_streak_threshold: 2,
        external_wait_breach_streak_threshold: 2,
        budget_hard_limit_factor: 1.5,
        external_wait_hard_limit_factor: 2.0,
    }
}

fn test_budget() -> crate::decision_engine::OrchestratorExecutionBudget {
    crate::decision_engine::OrchestratorExecutionBudget {
        max_duration_ms: 60_000,
        max_token_usage: 100_000,
        max_error_rate: 0.5,
        max_rounds: 50,
    }
}

fn test_snapshot(required_total: u32) -> crate::orchestrator_termination::TerminationSnapshot {
    use crate::orchestrator_termination::*;
    TerminationSnapshot {
        snapshot_id: "snap-1".into(),
        plan_id: "plan-1".into(),
        attempt_seq: 10,
        progress_vector: ProgressVector::default(),
        review_state: ReviewState::default(),
        blocker_state: BlockerState::default(),
        budget_state: BudgetState::default(),
        cache_state: None,
        cp_version: 1,
        required_total,
        failed_required: 0,
        running_or_pending_required: 0,
        running_required: Some(0),
        source_event_ids: vec![],
        computed_at: 0,
    }
}

#[test]
fn decision_engine_budget_threshold() {
    use crate::decision_engine::*;
    let engine = OrchestratorDecisionEngine::new(test_policy());
    let budget = test_budget();

    let mut snap = test_snapshot(5);
    snap.budget_state.elapsed_ms = 70_000;
    assert!(engine.is_budget_threshold_breached(&snap, &budget));

    snap.budget_state.elapsed_ms = 50_000;
    assert!(!engine.is_budget_threshold_breached(&snap, &budget));
}

#[test]
fn decision_engine_hard_budget_breach() {
    use crate::decision_engine::*;
    let engine = OrchestratorDecisionEngine::new(test_policy());
    let budget = test_budget();

    let mut snap = test_snapshot(5);
    snap.budget_state.elapsed_ms = 90_000;
    assert!(engine.is_hard_budget_breach(&snap, &budget));
}

#[test]
fn decision_engine_shadow_reason_completed() {
    use crate::decision_engine::*;
    use crate::orchestrator_termination::*;

    let engine = OrchestratorDecisionEngine::new(test_policy());
    let budget = test_budget();

    let mut snap = test_snapshot(3);
    snap.progress_vector.terminal_required_tasks = 3;
    snap.running_or_pending_required = 0;
    let gate = OrchestratorGateState::default();

    let reason = engine.resolve_shadow_reason(&snap, &budget, &gate, "done");
    assert_eq!(reason, OrchestratorTerminationReason::Completed);
}

#[test]
fn decision_engine_shadow_reason_failed_on_empty_text() {
    use crate::decision_engine::*;
    use crate::orchestrator_termination::*;

    let engine = OrchestratorDecisionEngine::new(test_policy());
    let budget = test_budget();
    let snap = test_snapshot(0);
    let gate = OrchestratorGateState::default();

    let reason = engine.resolve_shadow_reason(&snap, &budget, &gate, "   ");
    assert_eq!(reason, OrchestratorTerminationReason::Failed);
}

// ============================================================================
// Phase 4: Round Policy Tests
// ============================================================================

#[test]
fn round_policy_continue_prompt_no_todos() {
    let snap = test_snapshot(0);
    let prompt = crate::round_policy::build_continue_prompt(&snap);
    assert!(prompt.contains("没有结构化的必需任务"));
}

#[test]
fn round_policy_continue_prompt_with_todos() {
    let mut snap = test_snapshot(5);
    snap.progress_vector.terminal_required_tasks = 2;
    let prompt = crate::round_policy::build_continue_prompt(&snap);
    assert!(prompt.contains("剩余必需任务: 3"));
}

#[test]
fn summary_hijack_correction_rounds() {
    let c1 = crate::round_policy::build_summary_hijack_correction(1);
    assert!(!c1.force_no_tools_next_round);
    let c2 = crate::round_policy::build_summary_hijack_correction(2);
    assert!(c2.force_no_tools_next_round);
    let c3 = crate::round_policy::build_summary_hijack_correction(5);
    assert!(c3.force_no_tools_next_round);
}

#[test]
fn no_task_plain_response_simple_text_terminates() {
    use crate::round_policy::*;
    let decision = decide_no_task_plain_response_action("hello", 0, false, None, 0, 0);
    assert!(matches!(
        decision,
        NoTaskPlainResponseDecision::TerminateCompleted { .. }
    ));
}

#[test]
fn no_task_plain_response_empty_text_requests_outcome() {
    use crate::round_policy::*;
    let decision = decide_no_task_plain_response_action("", 0, false, None, 0, 0);
    assert!(matches!(
        decision,
        NoTaskPlainResponseDecision::RequestOutcomeBlock { .. }
    ));
}

#[test]
fn pending_terminal_synthesis_retries_on_empty_text() {
    use crate::round_policy::*;
    let decision = decide_pending_terminal_synthesis_action("", false, false, 0, 3);
    assert!(matches!(
        decision,
        PendingTerminalSynthesisDecision::Retry {
            next_retry_count: 1
        }
    ));
}

#[test]
fn pending_terminal_synthesis_finalizes_on_max() {
    use crate::round_policy::*;
    let decision = decide_pending_terminal_synthesis_action("done", true, false, 3, 3);
    assert!(matches!(
        decision,
        PendingTerminalSynthesisDecision::Finalize
    ));
}

// ============================================================================
// Phase 4: Tool Concurrency Tests
// ============================================================================

#[test]
fn tool_concurrency_read_only_safe() {
    assert!(crate::tool_concurrency::is_concurrency_safe("file_view"));
    assert!(crate::tool_concurrency::is_concurrency_safe("web_search"));
    assert!(!crate::tool_concurrency::is_concurrency_safe("shell"));
    assert!(!crate::tool_concurrency::is_concurrency_safe("file_edit"));
}

#[test]
fn tool_concurrency_partition_mixed() {
    let tools = [
        "file_view",
        "file_view",
        "file_edit",
        "code_search_regex",
        "shell",
    ];
    let batches = crate::tool_concurrency::partition_tool_calls(&tools);
    assert_eq!(batches.len(), 4);
    assert!(matches!(
        batches[0].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[0].tool_indices, vec![0, 1]);
    assert!(matches!(
        batches[1].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[1].tool_indices, vec![2]);
    assert!(matches!(
        batches[2].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[2].tool_indices, vec![3]);
    assert!(matches!(
        batches[3].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[3].tool_indices, vec![4]);
}

// ============================================================================
// Phase 4: Worker Duplicate Guard Tests
// ============================================================================

#[test]
fn duplicate_guard_read_only_dedup() {
    use crate::worker_duplicate_guard::*;
    let mut guard = WorkerDuplicateGuard::new();
    let tool = ToolCallInfo {
        name: "file_view".into(),
        arguments: serde_json::json!({"path": "foo.rs"}),
    };

    guard.record_read_only_call(&tool, 1000);
    let dup = guard.check_read_only_duplicate(&tool, 2000);
    assert!(dup.is_some());
    assert!(dup.unwrap().contains("已执行过完全相同"));
}

#[test]
fn duplicate_guard_read_only_cleared_after_mutation() {
    use crate::worker_duplicate_guard::*;
    let mut guard = WorkerDuplicateGuard::new();
    let tool = ToolCallInfo {
        name: "file_view".into(),
        arguments: serde_json::json!({"path": "foo.rs"}),
    };

    guard.record_read_only_call(&tool, 1000);
    guard.last_mutation_at = 1500;
    let dup = guard.check_read_only_duplicate(&tool, 2000);
    assert!(dup.is_none());
}

#[test]
fn duplicate_guard_failed_write_intercept() {
    use crate::worker_duplicate_guard::*;
    let mut guard = WorkerDuplicateGuard::new();
    let tool = ToolCallInfo {
        name: "file_edit".into(),
        arguments: serde_json::json!({"path": "foo.rs", "content": "x"}),
    };

    guard.record_failed_write(&tool, "permission denied", 1000);
    let dup = guard.check_failed_write_duplicate(&tool, 2000);
    assert!(dup.is_some());
    assert!(dup.unwrap().contains("此操作已失败"));
}

#[test]
fn duplicate_guard_success_write_intercept() {
    use crate::worker_duplicate_guard::*;
    let mut guard = WorkerDuplicateGuard::new();
    let tool = ToolCallInfo {
        name: "file_edit".into(),
        arguments: serde_json::json!({"path": "foo.rs", "content": "x"}),
    };

    guard.record_success_write(&tool, 1000);
    let dup = guard.check_success_write_duplicate(&tool, 2000);
    assert!(dup.is_some());
    assert!(dup.unwrap().contains("此写操作已成功执行"));
}

#[test]
fn duplicate_guard_extract_paths() {
    use crate::worker_duplicate_guard::*;
    let tools = vec![
        ToolCallInfo {
            name: "file_view".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        },
        ToolCallInfo {
            name: "code_search_regex".into(),
            arguments: serde_json::json!({"query": "fn main"}),
        },
    ];
    let paths = WorkerDuplicateGuard::extract_accessed_paths(&tools);
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], "src/main.rs");
    assert!(paths[1].starts_with("__query:"));
}

// ============================================================================
// Phase 4: Micro Compaction Tests
// ============================================================================

#[test]
fn micro_compaction_preserves_recent() {
    use crate::micro_compaction::*;
    let mut history = vec![
        LlmMessage {
            role: "user".into(),
            content: LlmContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "x".repeat(500),
                is_error: false,
                tool_name: Some("shell".into()),
                status: Some("success".into()),
            }]),
        },
        LlmMessage {
            role: "user".into(),
            content: LlmContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "t2".into(),
                content: "y".repeat(500),
                is_error: false,
                tool_name: Some("file_view".into()),
                status: None,
            }]),
        },
        LlmMessage {
            role: "user".into(),
            content: LlmContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "t3".into(),
                content: "z".repeat(500),
                is_error: false,
                tool_name: Some("shell".into()),
                status: None,
            }]),
        },
    ];

    let stats = compact_old_tool_results(&mut history, 2, 200, MicroCompactionMode::Compact);
    assert_eq!(stats.compacted_count, 1);
    assert!(stats.tokens_saved > 0);

    if let LlmContent::Blocks(blocks) = &history[0].content {
        if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
            assert!(content.starts_with("[Compacted"));
        } else {
            panic!("expected ToolResult");
        }
    }
}

#[test]
fn micro_compaction_skips_short_content() {
    use crate::micro_compaction::*;
    let mut history = vec![LlmMessage {
        role: "user".into(),
        content: LlmContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "short".into(),
            is_error: false,
            tool_name: None,
            status: None,
        }]),
    }];

    let stats = compact_old_tool_results(&mut history, 0, 200, MicroCompactionMode::Compact);
    assert_eq!(stats.compacted_count, 0);
}

// ============================================================================
// Phase 4: Auto Compaction Tests
// ============================================================================

#[test]
fn auto_compact_threshold_calculation() {
    use crate::auto_compaction::*;
    let config = AutoCompactConfig {
        context_window_tokens: 200_000,
        custom_threshold: None,
    };
    assert_eq!(get_auto_compact_threshold(&config), 187_000);

    let config2 = AutoCompactConfig {
        context_window_tokens: 200_000,
        custom_threshold: Some(100_000),
    };
    assert_eq!(get_auto_compact_threshold(&config2), 100_000);
}

#[test]
fn auto_compact_session_memory_compaction() {
    use crate::auto_compaction::*;
    use crate::micro_compaction::*;

    let mut history: Vec<LlmMessage> = (0..20)
        .map(|i| LlmMessage {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: LlmContent::Text(format!("message {} {}", i, "x".repeat(4000))),
        })
        .collect();

    let sm = SessionMemoryContent {
        summary: "This is a session memory summary of past work.".into(),
        token_estimate: 20,
    };

    let config = AutoCompactConfig {
        context_window_tokens: 50_000,
        custom_threshold: None,
    };

    let result = try_session_memory_compaction(&mut history, Some(&sm), &config);
    assert!(result.was_compacted);
    assert_eq!(result.method, Some(CompactionMethod::SessionMemory));
    assert!(history[0].role == "user");
    if let LlmContent::Text(text) = &history[0].content {
        assert!(text.contains("Session Memory"));
    }
}

#[test]
fn force_truncation_works() {
    use crate::auto_compaction::*;
    use crate::micro_compaction::*;

    let mut history: Vec<LlmMessage> = (0..50)
        .map(|i| LlmMessage {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: LlmContent::Text(format!("message {} {}", i, "x".repeat(500))),
        })
        .collect();

    let config = AutoCompactConfig {
        context_window_tokens: 10_000,
        custom_threshold: None,
    };

    let result = force_history_truncation(&mut history, &config);
    assert!(result.was_compacted);
    assert_eq!(result.method, Some(CompactionMethod::Truncation));
    assert!(history.len() < 50);
}

#[test]
fn extract_summary_content_with_tags() {
    use crate::auto_compaction::*;
    let raw = "<analysis>thinking...</analysis>\n<summary>\nkey points\n</summary>";
    let result = extract_summary_content(raw);
    assert_eq!(result, "key points");
}

#[test]
fn extract_summary_content_fallback() {
    use crate::auto_compaction::*;
    let raw = "just plain text";
    let result = extract_summary_content(raw);
    assert_eq!(result, "just plain text");
}

// ============================================================================
// Phase 4: LLM Types Tests
// ============================================================================

#[test]
fn summary_hijack_detection() {
    use crate::llm_types::*;
    assert!(is_summary_hijack_text(
        "Your task is to create a detailed summary\nIMPORTANT: Do NOT use any tools"
    ));
    assert!(is_summary_hijack_text(
        "IMPORTANT: Do NOT use any tools\n<analysis>test</analysis>\n<summary>test</summary>"
    ));
    assert!(!is_summary_hijack_text("normal text"));
    assert!(!is_summary_hijack_text(""));
}

#[test]
fn sanitize_tool_order_removes_orphan_tool_results() {
    use crate::llm_types::*;
    let messages = vec![
        LlmMessage {
            role: "user".into(),
            content: LlmMessageContent::Text("hello".into()),
        },
        LlmMessage {
            role: "user".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "orphan".into(),
                content: "result".into(),
                is_error: false,
            }]),
        },
    ];

    let sanitized = sanitize_tool_order(&messages);
    assert_eq!(sanitized.len(), 1);
    assert_eq!(sanitized[0].role, "user");
}

#[test]
fn sanitize_tool_order_preserves_valid_pairs() {
    use crate::llm_types::*;
    let messages = vec![
        LlmMessage {
            role: "user".into(),
            content: LlmMessageContent::Text("hello".into()),
        },
        LlmMessage {
            role: "assistant".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolUse {
                id: "t1".into(),
                name: "file_view".into(),
                input: serde_json::json!({}),
            }]),
        },
        LlmMessage {
            role: "user".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "file content".into(),
                is_error: false,
            }]),
        },
    ];

    let sanitized = sanitize_tool_order(&messages);
    assert_eq!(sanitized.len(), 3);
}
