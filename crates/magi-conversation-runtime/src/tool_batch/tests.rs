use super::*;
use crate::CanonicalTurnEventSink;
use crate::agent_spawn_preflight::{
    agent_spawn_child_policy_snapshot, child_canonical_task_name, parse_agent_context_package,
    valid_agent_task_name,
};
use magi_bridge_client::ChatToolFunction;
use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::tempdir;

fn test_task(task_id: &str, root_task_id: &str, parent_task_id: Option<TaskId>) -> Task {
    Task {
        task_id: TaskId::new(task_id),
        mission_id: MissionId::new("mission-mailbox"),
        root_task_id: TaskId::new(root_task_id),
        parent_task_id,
        kind: TaskKind::LocalAgent,
        title: format!("task {task_id}"),
        goal: format!("run task {task_id}"),
        status: TaskStatus::Running,
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
        runtime_payload: TaskRuntimePayload::default(),
        created_at: UtcMillis::now(),
        updated_at: UtcMillis::now(),
    }
}

fn coordinator_task(mut task: Task) -> Task {
    task.executor_binding = Some(TaskExecutorBinding::for_role("coordinator"));
    task
}

#[test]
fn direct_agent_spawn_rejects_invalid_inputs_before_registration() {
    let event_bus = InMemoryEventBus::new(16);
    let roles = magi_agent_role::AgentRoleRegistry::load_default();
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversations = ConversationRegistry::new();
    let session_id = SessionId::new("invalid-delegation-direct");
    session_store
        .create_session(session_id.clone(), "invalid delegation")
        .unwrap();
    let plans = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let parent = coordinator_task(test_task("delegation-root", "delegation-root", None));
    let assert_rejected = |arguments: serde_json::Value, error_code: &str| {
        let call = ChatToolCall {
            id: "invalid-spawn".into(),
            kind: "function".into(),
            function: ChatToolFunction {
                name: "agent_spawn".into(),
                arguments: arguments.to_string(),
            },
        };
        let definition =
            crate::builtin_tool_schema::public_builtin_tool_definition("agent_spawn").unwrap();
        let model_validation = crate::tool_call_validation::validate_tool_call_batch(
            std::slice::from_ref(&call),
            &[definition],
        );
        assert!(model_validation.valid_calls.is_empty(), "{arguments}");
        assert_eq!(model_validation.invalid_calls.len(), 1);
        let (payload, status) = execute_coordinator_tool(
            CoordinatorToolContext {
                event_bus: &event_bus,
                agent_role_registry: &roles,
                task_store: &task_store,
                session_store: &session_store,
                execution_registry: &execution_registry,
                conversation_registry: &conversations,
                plan_store: &plans,
                task: &parent,
                session_id: &session_id,
                workspace_id: &None,
                workspace_root_path: None,
                execution_allowed: &|| true,
            },
            BuiltinToolName::AgentSpawn,
            &call,
        );
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(status, ExecutionResultStatus::Rejected, "{arguments}");
        assert_eq!(payload["error_code"], error_code, "{arguments}: {payload}");
        assert_eq!(payload["failure_stage"], "input_validation");
        assert!(payload["child_task_id"].is_null());
        assert!(task_store.all_tasks().is_empty());
        assert!(session_store.runtime_sidecar(&session_id).is_none());
        assert!(
            session_store
                .thread_registry_snapshot(&session_id)
                .is_empty()
        );
        assert!(plans.snapshot().is_none());
    };
    for decision in [
        None,
        Some(serde_json::json!({"purpose":"long_task", "reason":"many files"})),
        Some(serde_json::json!({"purpose":"parallel_work", "reason":"independent modules"})),
        Some(serde_json::json!({"purpose":"user_requested", "reason":"  "})),
        Some(
            serde_json::json!({"purpose":"user_requested", "reason":format!(" {}", "独".repeat(600))}),
        ),
        Some(
            serde_json::json!({"purpose":"parallel_work", "reason":"独立模块", "parent_work":format!(" {}", "独".repeat(600))}),
        ),
        Some(serde_json::json!({"purpose":"user_requested", "reason":"request", "approved":true})),
    ] {
        let mut arguments = serde_json::json!({"task_name":"invalid_decision", "role":"executor"});
        if let Some(decision) = decision {
            arguments["delegation"] = decision;
        }
        assert_rejected(arguments, "invalid_delegation_decision");
    }
    let valid = serde_json::json!({
        "task_name":"schema_gate", "role":"executor", "display_name":"合同检查员",
        "goal":"检查独立模块合同", "delegation":{"purpose":"independent_review", "reason":"独立核对解析器授权边界"},
        "context_package":{"summary":"检查指定解析器", "constraints":[], "expected_output":"边界与证据", "references":[]}
    });
    // 直接入口不能接受模型入口已拒绝的旧形式；无需配置角色模型或活跃执行链，
    // 输入必须在触及这些动态事实之前被拒绝。
    for (pointer, value) in [
        (
            "/context_package",
            serde_json::json!(valid["context_package"].to_string()),
        ),
        ("/context_package/extra", serde_json::json!(true)),
        (
            "/context_package/references",
            serde_json::json!([{"kind":"file", "source_ref":"Cargo.toml", "extra":true}]),
        ),
        ("/capabilities", serde_json::Value::Null),
        ("/allow_delegation", serde_json::Value::Null),
        ("/inherit_skill", serde_json::Value::Null),
        ("/plan_item_id", serde_json::Value::Null),
        ("/parallelism_group", serde_json::Value::Null),
        ("/working_dir", serde_json::Value::Null),
        ("/lifetime", serde_json::Value::Null),
        ("/resume_task_id", serde_json::Value::Null),
        ("/model", serde_json::json!("caller-selected-model")),
        ("/engineId", serde_json::json!("caller-selected-engine")),
        ("/provider", serde_json::json!("caller-selected-provider")),
        ("/reasoning_effort", serde_json::json!("future")),
        ("/model_source", serde_json::json!("role_engine")),
    ] {
        let mut arguments = valid.clone();
        let (parent_pointer, field) = pointer.rsplit_once('/').unwrap();
        arguments
            .pointer_mut(parent_pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.to_string(), value);
        assert_rejected(arguments, "invalid_arguments");
    }
    for field in ["display_name", "context_package"] {
        let mut arguments = valid.clone();
        arguments.as_object_mut().unwrap().remove(field);
        assert_rejected(arguments, "invalid_arguments");
    }
}

#[test]
fn agent_spawn_rejects_incomplete_lifecycle_permissions_without_creating_resources() {
    for scope in [
        "deny_wait",
        "deny_cancel",
        "spawn_only",
        "skill_wait",
        "skill_cancel",
    ] {
        let event_bus = InMemoryEventBus::new(16);
        let roles = magi_agent_role::AgentRoleRegistry::load_default();
        let tasks = TaskStore::new();
        let sessions = SessionStore::new();
        let skills = magi_skill_runtime::SkillRegistry::new();
        let mut parent = coordinator_task(test_task("lifecycle-root", "lifecycle-root", None));
        let mut policy = default_agent_spawn_policy();
        match scope {
            "deny_wait" => policy.denied_tools = vec!["agent_wait".into()],
            "deny_cancel" => policy.denied_tools = vec!["agent_cancel".into()],
            "spawn_only" => policy.allowed_tools = vec!["agent_spawn".into()],
            _ => {
                skills.register(magi_skill_runtime::SkillDefinition {
                    description: String::new(),
                    skill_id: "incomplete-agent-scope".into(),
                    title: "不完整代理范围".into(),
                    instruction: "派发日志调查".into(),
                    metadata: magi_skill_runtime::SkillMetadata {
                        category: "test".into(),
                        tags: vec![],
                    },
                    restrict_standard_tools: true,
                    allowed_tools: vec![
                        "agent_spawn".into(),
                        if scope == "skill_wait" {
                            "agent_cancel"
                        } else {
                            "agent_wait"
                        }
                        .into(),
                    ],
                    custom_tool_bindings: vec![],
                    prompt_priority: 50,
                });
                parent.executor_binding.as_mut().unwrap().active_skill_id =
                    Some("incomplete-agent-scope".into());
            }
        }
        parent.policy_snapshot = Some(policy);
        let executions = TaskExecutionRegistry::default().with_agent_spawn_preflight_runtime(
            crate::task_execution_registry::AgentSpawnPreflightRuntime {
                skill_runtime: Some(Arc::new(magi_skill_runtime::SkillRuntime::new(skills))),
                ..Default::default()
            },
        );
        let conversations = ConversationRegistry::new();
        let session = SessionId::new(format!("incomplete-{scope}"));
        sessions
            .create_session(session.clone(), "lifecycle")
            .unwrap();
        let plans = magi_plan::PlanStore::from_store(&sessions, session.clone());
        let call = ChatToolCall {
                id: "incomplete-lifecycle".into(), kind: "function".into(),
                function: ChatToolFunction { name: "agent_spawn".into(), arguments: serde_json::json!({
                    "task_name":"log_investigation", "role":"explorer", "display_name":"日志调查员",
                    "goal":"调查日志错误", "inherit_skill":true,
                    "delegation":{"purpose":"context_isolation", "reason":"隔离大量日志中间输出，只返回定位证据"},
                    "context_package":{"summary":"定位指定日志错误", "constraints":[], "expected_output":"结论与行号", "references":[]}
                }).to_string() },
            };
        let (payload, status) = execute_coordinator_tool(
            CoordinatorToolContext {
                event_bus: &event_bus,
                agent_role_registry: &roles,
                task_store: &tasks,
                session_store: &sessions,
                execution_registry: &executions,
                conversation_registry: &conversations,
                plan_store: &plans,
                task: &parent,
                session_id: &session,
                workspace_id: &None,
                workspace_root_path: None,
                execution_allowed: &|| true,
            },
            BuiltinToolName::AgentSpawn,
            &call,
        );
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            status,
            ExecutionResultStatus::Rejected,
            "{scope}: {payload}"
        );
        assert_eq!(
            payload["error_code"], "agent_lifecycle_tool_unavailable",
            "{scope}: {payload}"
        );
        assert_eq!(payload["failure_stage"], "authorization");
        assert_eq!(payload["retryable"], false);
        assert!(payload["child_task_id"].is_null());
        assert!(tasks.all_tasks().is_empty());
        assert!(sessions.runtime_sidecar(&session).is_none());
        assert!(sessions.thread_registry_snapshot(&session).is_empty());
        assert_eq!(conversations.len(), 0);
        assert!(plans.snapshot().is_none());
    }
}

#[test]
fn duplicate_agent_spawn_points_to_existing_task_without_creating_resources() {
    for child_status in [
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let event_bus = InMemoryEventBus::new(16);
        let roles = magi_agent_role::AgentRoleRegistry::load_default();
        let tasks = TaskStore::new();
        let sessions = SessionStore::new();
        let executions = TaskExecutionRegistry::default();
        let conversations = ConversationRegistry::new();
        let session = SessionId::new("duplicate-agent");
        sessions
            .create_session(session.clone(), "duplicate agent")
            .unwrap();
        let plans = magi_plan::PlanStore::from_store(&sessions, session.clone());
        let parent = coordinator_task(test_task("root", "root", None));
        tasks.insert_task(parent.clone()).unwrap();
        let mut child = test_task("original-child", "root", Some("root".into()));
        child.executor_binding = Some(
            TaskExecutorBinding::for_role("reviewer")
                .with_canonical_task_name("/root/parser_review"),
        );
        child.status = child_status;
        tasks.insert_task(child.clone()).unwrap();
        let before_task_ids = tasks
            .all_tasks()
            .into_iter()
            .map(|task| task.task_id)
            .collect::<Vec<_>>();
        let call = ChatToolCall {
                id: "duplicate-call".into(), kind: "function".into(),
                function: ChatToolFunction {
                    name: "agent_spawn".into(),
                    arguments: serde_json::json!({
                        "task_name":"parser_review", "role":"reviewer", "display_name":"解析器复核",
                        "goal":"复核解析器的边界处理", "delegation":{
                            "purpose":"independent_review", "reason":"独立核对解析器边界，避免实现者的共同假设"
                        },
                        "context_package":{"summary":"指定解析器改动", "constraints":[],
                            "expected_output":"具体缺陷和定位证据", "references":[]}
                    }).to_string(),
                },
            };
        let (payload, status) = execute_coordinator_tool(
            CoordinatorToolContext {
                event_bus: &event_bus,
                agent_role_registry: &roles,
                task_store: &tasks,
                session_store: &sessions,
                execution_registry: &executions,
                conversation_registry: &conversations,
                plan_store: &plans,
                task: &parent,
                session_id: &session,
                workspace_id: &None,
                workspace_root_path: None,
                execution_allowed: &|| true,
            },
            BuiltinToolName::AgentSpawn,
            &call,
        );
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"], "duplicate_task_name");
        assert_eq!(payload["existing_task_id"], child.task_id.as_str());
        assert!(payload["child_task_id"].is_null());
        assert_eq!(payload["retryable"], false);
        assert!(
            payload["instruction"]
                .as_str()
                .unwrap()
                .contains("不得仅换名重派")
        );
        assert_eq!(
            tasks
                .all_tasks()
                .into_iter()
                .map(|task| task.task_id)
                .collect::<Vec<_>>(),
            before_task_ids
        );
        assert!(sessions.runtime_sidecar(&session).is_none());
        assert!(sessions.thread_registry_snapshot(&session).is_empty());
        assert_eq!(conversations.len(), 0);
        assert!(plans.snapshot().is_none());
        let admission = executions.execution_admission().snapshot();
        assert!(admission.active_task_ids.is_empty());
        assert!(admission.queued.is_empty());
    }
}

struct SnapshotReconcileProbeTool {
    name: &'static str,
    snapshot: Arc<SnapshotSession>,
}

struct PanicBuiltinTool {
    name: &'static str,
}

struct CountingBuiltinTool {
    name: &'static str,
    executions: Arc<AtomicUsize>,
}

impl SnapshotReconcileProbeTool {
    fn new(name: &'static str, snapshot: Arc<SnapshotSession>) -> Self {
        Self { name, snapshot }
    }
}

impl magi_tool_runtime::BuiltinTool for SnapshotReconcileProbeTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn execute(
        &self,
        _tool_call_id: &ToolCallId,
        input: &str,
        context: &ToolExecutionContext,
        _resources: &magi_tool_runtime::ToolRuntimeResources,
    ) -> String {
        let arguments = serde_json::from_str::<serde_json::Value>(input).unwrap_or_default();
        let path = arguments
            .get("changed_paths")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(serde_json::Value::as_str)
            .expect("probe changed path");
        let workspace_root = context
            .working_directory
            .as_ref()
            .expect("probe working directory");
        std::fs::write(workspace_root.join(path), format!("probe {path}"))
            .expect("probe file write");
        self.snapshot.reconcile().expect("probe reconcile");
        serde_json::json!({
            "tool": self.name,
            "status": "succeeded",
            "stdout": "snapshot reconciled"
        })
        .to_string()
    }

    fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
        magi_tool_runtime::BuiltinToolSpec {
            name: self.name.to_string(),
            risk_level: magi_core::RiskLevel::Low,
            approval_requirement: magi_core::ApprovalRequirement::None,
        }
    }
}

impl magi_tool_runtime::BuiltinTool for PanicBuiltinTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn execute(
        &self,
        _tool_call_id: &ToolCallId,
        _input: &str,
        _context: &ToolExecutionContext,
        _resources: &magi_tool_runtime::ToolRuntimeResources,
    ) -> String {
        panic!("internal task tool panic detail must stay private")
    }

    fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
        magi_tool_runtime::BuiltinToolSpec {
            name: self.name.to_string(),
            risk_level: magi_core::RiskLevel::Low,
            approval_requirement: magi_core::ApprovalRequirement::None,
        }
    }
}

/// 执行时模拟用户中断：把当前任务收口为 Killed，相当于 kill_task 在工具运行中生效。
struct InterruptingBuiltinTool {
    name: &'static str,
    task_store: Arc<TaskStore>,
    task_id: TaskId,
}

impl magi_tool_runtime::BuiltinTool for InterruptingBuiltinTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn execute(
        &self,
        _tool_call_id: &ToolCallId,
        _input: &str,
        _context: &ToolExecutionContext,
        _resources: &magi_tool_runtime::ToolRuntimeResources,
    ) -> String {
        self.task_store
            .revoke_active_lease_and_set_task_terminal(
                &self.task_id,
                &self.task_id,
                TaskStatus::Killed,
                Vec::new(),
            )
            .expect("task should be killed");
        serde_json::json!({
            "tool": self.name,
            "status": "failed",
            "error": "process killed by interrupt",
        })
        .to_string()
    }

    fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
        magi_tool_runtime::BuiltinToolSpec {
            name: self.name.to_string(),
            risk_level: magi_core::RiskLevel::Low,
            approval_requirement: magi_core::ApprovalRequirement::None,
        }
    }
}

impl magi_tool_runtime::BuiltinTool for CountingBuiltinTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn execute(
        &self,
        _tool_call_id: &ToolCallId,
        input: &str,
        _context: &ToolExecutionContext,
        _resources: &magi_tool_runtime::ToolRuntimeResources,
    ) -> String {
        self.executions.fetch_add(1, Ordering::SeqCst);
        serde_json::json!({
            "tool": self.name,
            "status": "succeeded",
            "input": serde_json::from_str::<serde_json::Value>(input).unwrap_or_default(),
        })
        .to_string()
    }

    fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
        magi_tool_runtime::BuiltinToolSpec {
            name: self.name.to_string(),
            risk_level: magi_core::RiskLevel::Low,
            approval_requirement: magi_core::ApprovalRequirement::None,
        }
    }
}

#[test]
fn idempotent_read_tools_execute_once_and_reuse_results_across_model_rounds() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-idempotent-tool-reuse");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let executions = Arc::new(AtomicUsize::new(0));
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_builtin(Arc::new(CountingBuiltinTool {
        name: BuiltinToolName::WebSearch.as_str(),
        executions: executions.clone(),
    }));
    let task = test_task(
        "task-idempotent-tool-reuse",
        "task-idempotent-tool-reuse",
        None,
    );
    let session_id = SessionId::new("session-idempotent-tool-reuse");
    let workspace_id = Some(WorkspaceId::new("workspace-idempotent-tool-reuse"));
    let mut ledger = ToolExecutionLedger::default();
    let first_round_calls = vec![
        ChatToolCall {
            id: "call-web-search-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::WebSearch.as_str().to_string(),
                arguments: r#"{"query":"Magi","locale":"zh-CN"}"#.to_string(),
            },
        },
        ChatToolCall {
            id: "call-web-search-2".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::WebSearch.as_str().to_string(),
                arguments: r#"{"locale":"zh-CN","query":"Magi"}"#.to_string(),
            },
        },
    ];

    let first_results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &first_round_calls,
        &mut ledger,
        None,
        None,
        None,
        &|| true,
    );

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(
        first_results
            .iter()
            .all(|(_, status)| *status == ExecutionResultStatus::Succeeded)
    );
    let reused: serde_json::Value =
        serde_json::from_str(&first_results[1].0).expect("reused result should be json");
    assert_eq!(reused["execution"], "reused");

    let second_round_call = ChatToolCall {
        id: "call-web-search-3".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::WebSearch.as_str().to_string(),
            arguments: r#"{"query":"Magi","locale":"zh-CN"}"#.to_string(),
        },
    };
    let second_results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        std::slice::from_ref(&second_round_call),
        &mut ledger,
        None,
        None,
        None,
        &|| true,
    );

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let reused: serde_json::Value =
        serde_json::from_str(&second_results[0].0).expect("reused result should be json");
    assert_eq!(reused["execution"], "reused");
    let obsolete_results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &[second_round_call],
        &mut ledger,
        None,
        None,
        None,
        &|| false,
    );
    assert_eq!(
        obsolete_results[0].1,
        ExecutionResultStatus::Cancelled,
        "执行资格失效时不能交付旧缓存中的成功"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[test]
fn explicit_single_tool_budget_prevents_different_read_calls() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-explicit-tool-budget");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let executions = Arc::new(AtomicUsize::new(0));
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_builtin(Arc::new(CountingBuiltinTool {
        name: BuiltinToolName::WebSearch.as_str(),
        executions: executions.clone(),
    }));
    let task = test_task(
        "task-explicit-tool-budget",
        "task-explicit-tool-budget",
        None,
    );
    let session_id = SessionId::new("session-explicit-tool-budget");
    let workspace_id = Some(WorkspaceId::new("workspace-explicit-tool-budget"));
    let mut ledger = ToolExecutionLedger::for_task_goal("请只调用一次 web_search。");

    let first_results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &[ChatToolCall {
            id: "call-web-search-budget-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::WebSearch.as_str().to_string(),
                arguments: r#"{"query":"Magi"}"#.to_string(),
            },
        }],
        &mut ledger,
        None,
        None,
        None,
        &|| true,
    );
    assert_eq!(first_results[0].1, ExecutionResultStatus::Succeeded);

    let second_results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &[ChatToolCall {
            id: "call-web-search-budget-2".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::WebSearch.as_str().to_string(),
                arguments: r#"{"query":"Grok"}"#.to_string(),
            },
        }],
        &mut ledger,
        None,
        None,
        None,
        &|| true,
    );

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let skipped: serde_json::Value =
        serde_json::from_str(&second_results[0].0).expect("budget result should be json");
    assert_eq!(skipped["execution"], "skipped");
    assert_eq!(skipped["reason"], "user_tool_call_limit_reached");
}

fn read_only_agent_spawn_policy() -> TaskPolicy {
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::ReadOnly;
    policy.command_mode = "read_only".to_string();
    policy
}

#[test]
fn agent_spawn_child_policy_preserves_parent_full_access() {
    let parent = default_agent_spawn_policy();
    let child = agent_spawn_child_policy_snapshot(Some(&parent));

    assert_eq!(child.access_profile, parent.access_profile);
    assert_eq!(child.command_mode, parent.command_mode);
    assert_eq!(child.network_mode, parent.network_mode);
    assert_eq!(child.task_tier, parent.task_tier);

    let mut full_access_parent = default_agent_spawn_policy();
    full_access_parent.access_profile = magi_core::AccessProfile::FullAccess;
    let full_access_child = agent_spawn_child_policy_snapshot(Some(&full_access_parent));
    assert_eq!(
        full_access_child.access_profile,
        magi_core::AccessProfile::FullAccess
    );
    assert_eq!(full_access_child.command_mode, "full");
}

#[test]
fn agent_spawn_child_policy_never_escalates_parent_read_only() {
    let parent = read_only_agent_spawn_policy();

    let child = agent_spawn_child_policy_snapshot(Some(&parent));

    assert_eq!(child.access_profile, magi_core::AccessProfile::ReadOnly);
    assert_eq!(child.command_mode, "read_only");
}

#[test]
fn agent_spawn_child_policy_inherits_parent_reference_boundaries() {
    let mut parent = default_agent_spawn_policy();
    parent.allowed_tools = vec!["file_read".to_string(), "shell_exec".to_string()];
    parent.denied_tools = vec!["memory_write".to_string()];
    parent.allowed_paths = vec!["/tmp/workspace".to_string(), "/tmp/reference".to_string()];
    parent.denied_paths = vec!["/tmp/workspace/private".to_string()];
    parent.read_only_paths = vec!["/tmp/reference".to_string()];
    parent.network_mode = "disabled".to_string();
    parent.command_mode = "read_only".to_string();

    let child = agent_spawn_child_policy_snapshot(Some(&parent));

    assert_eq!(child.allowed_tools, parent.allowed_tools);
    assert_eq!(child.denied_tools, parent.denied_tools);
    assert_eq!(child.allowed_paths, parent.allowed_paths);
    assert_eq!(child.denied_paths, parent.denied_paths);
    assert_eq!(child.read_only_paths, parent.read_only_paths);
    assert_eq!(child.network_mode, parent.network_mode);
    assert_eq!(child.command_mode, parent.command_mode);
}

#[test]
fn agent_spawn_context_package_is_structured_and_does_not_inherit_parent_refs() {
    let parent = test_task("task-agent-context-parent", "task-agent-context-root", None);
    let parsed = serde_json::json!({
        "context_package": {
            "summary": "修复会话同步",
            "constraints": ["不得跨 workspace 读取 session"],
            "expected_output": "给出根因和测试结果",
            "references": [{
                "kind": "task_evidence",
                "title": "bootstrap 证据",
                "source_ref": "task:task-agent-context-parent:evidence:0",
                "preview": "bootstrap 只接受后端 session"
            }]
        }
    });

    let package = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(100), 7)
        .expect("结构化上下文包应解析成功");

    assert_eq!(package.parent_task_id, parent.task_id);
    assert_eq!(package.revision, 1);
    assert_eq!(package.references.len(), 1);
    assert_eq!(
        package.references[0].source_ref,
        "task:task-agent-context-parent:evidence:0"
    );
    assert!(package.render_for_prompt().contains("context_read"));
}

#[test]
fn agent_spawn_context_package_rejects_model_json_string_encoding() {
    let parent = test_task(
        "task-agent-context-string-parent",
        "task-agent-context-string-root",
        None,
    );
    let package = serde_json::json!({
        "summary": "检查配置文件",
        "constraints": ["只读，不修改文件"],
        "expected_output": "明确报告文件是否存在",
        "references": []
    });
    let parsed = serde_json::json!({
        "context_package": serde_json::to_string(&package)
            .expect("context package JSON should serialize")
    });

    let error = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(101), 8)
        .expect_err("新调用必须直接传结构化对象，不再保留二次编码兜底");
    assert!(error.contains("不能二次编码"));
}

#[test]
fn agent_spawn_context_package_rejects_non_string_reference_title() {
    let parent = test_task(
        "task-agent-context-invalid-reference-parent",
        "task-agent-context-invalid-reference-root",
        None,
    );
    let parsed = serde_json::json!({
        "context_package": {
            "summary": "检查派发参数",
            "constraints": ["只读"],
            "expected_output": "返回校验结论",
            "references": [{
                "kind": "file",
                "title": { "label": "Cargo.toml" },
                "source_ref": "path:Cargo.toml"
            }]
        }
    });

    let error = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(102), 9)
        .expect_err("引用 title 不是字符串时必须拒绝创建代理");

    assert_eq!(error, "spawn.references[0].title 必须是字符串");
}

#[test]
fn agent_spawn_context_package_derives_missing_reference_title_from_source() {
    let parent = test_task(
        "task-agent-context-derived-title-parent",
        "task-agent-context-derived-title-root",
        None,
    );
    let parsed = serde_json::json!({
        "context_package": {
            "summary": "检查派发参数",
            "constraints": ["只读"],
            "expected_output": "返回校验结论",
            "references": [{
                "kind": "file",
                "source_ref": "path:Cargo.toml"
            }]
        }
    });

    let package = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(103), 10)
        .expect("缺少可选展示标题时应从 source_ref 派生");

    assert_eq!(package.references[0].title, "path:Cargo.toml");
}

#[test]
fn agent_spawn_failure_contract_preserves_safe_diagnosis() {
    let tool_call = ChatToolCall {
        id: "call-agent-spawn-invalid-context".to_string(),
        kind: "function".to_string(),
        function: magi_bridge_client::ChatToolFunction {
            name: "agent_spawn".to_string(),
            arguments: "{}".to_string(),
        },
    };

    let payload: serde_json::Value = serde_json::from_str(&agent_spawn_failure_payload(
        &tool_call,
        "failed",
        "invalid_context_package",
        "input_validation",
        "spawn.references[0].title 必须是字符串",
        "请按 Schema 修正引用字段后重试。",
    ))
    .expect("failure payload should be JSON");

    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "invalid_context_package");
    assert_eq!(payload["failure_stage"], "input_validation");
    assert_eq!(
        payload["diagnostic_ref"],
        "tool_call:call-agent-spawn-invalid-context"
    );
    assert_eq!(payload["child_task_id"], serde_json::Value::Null);
    assert_eq!(payload["retryable"], true, "参数错误可由主线修正后重试");

    let runtime_failure: serde_json::Value = serde_json::from_str(&agent_spawn_failure_payload(
        &tool_call,
        "rejected",
        "model_preflight_failed",
        "model",
        "代理模型配置不可用",
        "请检查模型配置后重试。",
    ))
    .expect("failure payload should be JSON");
    assert_eq!(runtime_failure["retryable"], false);
    assert!(
        runtime_failure["instruction"].as_str().is_some_and(
            |text| text.contains("直接接管") && text.contains("请检查模型配置后重试。")
        ),
        "恢复指导允许当前代理接管，并保留具体诊断"
    );
}

#[test]
fn read_only_agent_policy_rejects_write_tool() {
    let mut task = test_task("task-read-only-agent", "task-read-only-agent", None);
    task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
        &read_only_agent_spawn_policy(),
    )));

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::FileWrite.as_str(),
        r#"{"path":"probe.txt","content":""}"#,
    )
    .expect("read-only agent should reject file_write");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("rejection should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["status"].as_str(), Some("rejected"));
    assert_eq!(payload["tool"].as_str(), Some("file_write"));
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some("该工具在当前访问模式下不可用")
    );
    assert_eq!(
        payload["required_access_profile"].as_str(),
        Some("full_access")
    );
}

#[test]
fn read_only_policy_allows_internal_coordination_but_rejects_project_memory_write() {
    let mut task = test_task(
        "task-read-only-state-tools",
        "task-read-only-state-tools",
        None,
    );
    task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
        &read_only_agent_spawn_policy(),
    )));

    for tool in [
        BuiltinToolName::AgentSpawn,
        BuiltinToolName::CreateGoal,
        BuiltinToolName::UpdateGoal,
        BuiltinToolName::UpdatePlan,
    ] {
        assert!(
            task_policy_tool_decision(&task, tool.as_str(), "{}").is_none(),
            "只读访问只限制外部副作用，内部协调工具 {} 应保持可用",
            tool.as_str()
        );
    }

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::MemoryWrite.as_str(),
        r#"{"action":"save"}"#,
    )
    .expect("project memory write should remain blocked in read-only mode");
    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
}

/// 角色工具的执行级保护：没有声明它的 Skill、只读模式、无效参数、未获确认都不能写入角色。
#[test]
fn role_save_never_writes_without_a_declaring_skill_and_a_user_confirmation() {
    let dir = tempdir().unwrap();
    let roles =
        magi_agent_role::AgentRoleRegistry::builtin().with_user_role_dir(dir.path().join("roles"));
    let bus = InMemoryEventBus::new(16);
    let tasks = TaskStore::new();
    let sessions = SessionStore::new();
    let execution = TaskExecutionRegistry::default();
    let conversations = ConversationRegistry::new();
    let session = SessionId::new("role-save-guard");
    sessions
        .create_session(session.clone(), "role save")
        .unwrap();
    let plans = magi_plan::PlanStore::from_store(&sessions, session.clone());
    let skill_runtime = magi_skill_runtime::SkillRuntime::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "role-skill".to_string(),
            title: "role".to_string(),
            instruction: "管理角色。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "agent".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["agent_role_get".into(), "agent_role_save".into()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        registry
    });
    let valid = serde_json::json!({
        "role_id": "data-analyst", "display_name": "数据分析师",
        "description": "分析数据", "system_prompt": "你负责分析数据。",
        "capabilities": ["general_engineering"],
    });
    let invoke =
        |policy: TaskPolicy, skill: Option<&str>, name: &str, arguments: serde_json::Value| {
            let mut task = coordinator_task(test_task("role-root", "role-root", None));
            task.policy_snapshot = Some(policy);
            execute_task_tool_call(
                &bus,
                None,
                &roles,
                Some(&skill_runtime),
                None,
                skill,
                &tasks,
                &sessions,
                &execution,
                &conversations,
                None,
                &plans,
                None,
                &task,
                &session,
                &None,
                None,
                None,
                None,
                &ChatToolCall {
                    id: format!("role-call-{name}"),
                    kind: "function".into(),
                    function: ChatToolFunction {
                        name: name.into(),
                        arguments: arguments.to_string(),
                    },
                },
                None,
                &|| true,
                None,
            )
        };

    // 没有声明它的 Skill：即使完全授权也拒绝。
    let mut full = default_agent_spawn_policy();
    full.access_profile = magi_core::AccessProfile::FullAccess;
    for skill in [None, Some("missing-skill")] {
        let (payload, status) = invoke(full.clone(), skill, "agent_role_save", valid.clone());
        assert_eq!(status, ExecutionResultStatus::Rejected, "{payload}");
    }
    // 只读模式：写入工具被硬阻断，读取工具仍可用。
    let (payload, status) = invoke(
        read_only_agent_spawn_policy(),
        Some("role-skill"),
        "agent_role_save",
        valid.clone(),
    );
    assert_eq!(status, ExecutionResultStatus::Rejected, "{payload}");
    let (payload, status) = invoke(
        read_only_agent_spawn_policy(),
        Some("role-skill"),
        "agent_role_get",
        serde_json::json!({}),
    );
    assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
    // 参数无效：在请用户确认之前就失败。
    let (payload, status) = invoke(
        full.clone(),
        Some("role-skill"),
        "agent_role_save",
        serde_json::json!({"role_id": "executor", "display_name": "x", "system_prompt": "x", "capabilities": ["general_engineering"]}),
    );
    assert_eq!(status, ExecutionResultStatus::Failed, "{payload}");
    assert!(payload.contains("role_conflict"));
    // 声明了 Skill 且参数有效，但当前没有可确认的对话轮次：不会保存。
    let (payload, status) = invoke(full, Some("role-skill"), "agent_role_save", valid);
    assert_eq!(status, ExecutionResultStatus::Cancelled, "{payload}");
    assert!(
        roles.get("data-analyst").is_none(),
        "未获用户确认不能写入角色"
    );
}

#[test]
fn task_catalog_and_execution_share_explicit_tool_restrictions() {
    let bus = InMemoryEventBus::new(16);
    let tasks = TaskStore::new();
    let sessions = SessionStore::new();
    let execution = TaskExecutionRegistry::default();
    let conversations = ConversationRegistry::new();
    let roles = magi_agent_role::AgentRoleRegistry::load_default();
    let session = SessionId::new("task-catalog-policy");
    sessions
        .create_session(session.clone(), "tool scope")
        .unwrap();
    let plans = magi_plan::PlanStore::from_store(&sessions, session.clone());
    let root = tempdir().unwrap();
    let path = root.path().to_path_buf();
    std::fs::write(path.join("kept.txt"), "original").unwrap();
    let workspace = Some(WorkspaceId::new("task-catalog-workspace"));
    let external_calls = Arc::new(AtomicUsize::new(0));
    let mcp_calls = external_calls.clone();
    let mut tools = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(16)),
    )
    .with_external_tool_catalog_provider(Arc::new(|| {
        magi_tool_runtime::ExternalToolCatalogSnapshot {
            mcp_tools: ["read", "blocked", "excluded"]
                .map(|name| magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "repo".into(),
                    server_name: "Repo".into(),
                    model_tool_name: format!("mcp__repo__{name}"),
                    tool_name: name.into(),
                    description: name.into(),
                    read_only: true,
                    input_schema: serde_json::json!({"type":"object"}),
                })
                .into(),
            ..Default::default()
        }
    }))
    .with_external_mcp_tool_executor(Arc::new(move |_, _, _| {
        mcp_calls.fetch_add(1, Ordering::SeqCst);
        (
            r#"{"status":"succeeded"}"#.into(),
            ExecutionResultStatus::Succeeded,
        )
    }));
    tools.register_default_builtins();

    for child in [false, true] {
        let mut task = coordinator_task(test_task(
            "catalog-task",
            if child {
                "catalog-root"
            } else {
                "catalog-task"
            },
            child.then(|| TaskId::new("catalog-root")),
        ));
        assert_eq!(task_is_coordinator(Some(&task), Some(&roles)), !child);
        let mut policy = default_agent_spawn_policy();
        policy.allowed_tools = [
            "tool_catalog",
            "file_read",
            "mcp__repo__read",
            "mcp__repo__blocked",
        ]
        .map(str::to_string)
        .into();
        policy.denied_tools = ["file_remove", "mcp__repo__blocked"]
            .map(str::to_string)
            .into();
        task.policy_snapshot = Some(policy);
        let invoke = |name: &str, arguments: serde_json::Value| {
            execute_task_tool_call(
                &bus,
                Some(&tools),
                &roles,
                None,
                None,
                None,
                &tasks,
                &sessions,
                &execution,
                &conversations,
                None,
                &plans,
                None,
                &task,
                &session,
                &workspace,
                Some(&path),
                None,
                None,
                &ChatToolCall {
                    id: format!("{child}-{name}"),
                    kind: "function".into(),
                    function: ChatToolFunction {
                        name: name.into(),
                        arguments: arguments.to_string(),
                    },
                },
                None,
                &|| true,
                None,
            )
        };
        let (catalog, status) = invoke("tool_catalog", serde_json::json!({}));
        assert_eq!(status, ExecutionResultStatus::Succeeded, "{catalog}");
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let entry = |name| {
            catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()
        };
        assert_eq!(entry("file_read")["model_available"], true);
        for (name, reason) in [
            ("file_remove", "tool_explicitly_denied"),
            ("search_text", "tool_not_in_allowed_scope"),
        ] {
            assert_eq!(entry(name)["availability"], "denied", "{child}: {catalog}");
            assert_eq!(entry(name)["reason_code"], reason);
            assert_eq!(
                invoke(
                    name,
                    serde_json::json!({"path":"kept.txt", "query":"original"})
                )
                .1,
                ExecutionResultStatus::Rejected
            );
        }
        for (key, prefix) in [("mcp_tools", "mcp")] {
            for (suffix, reason) in [
                ("read", None),
                ("blocked", Some("tool_explicitly_denied")),
                ("excluded", Some("tool_not_in_allowed_scope")),
            ] {
                let name = format!("{prefix}__repo__{suffix}");
                let entry = catalog[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["model_tool_name"] == name)
                    .unwrap();
                assert_eq!(entry["reason_code"], serde_json::json!(reason));
                assert_eq!(entry["model_available"], reason.is_none());
                let before = external_calls.load(Ordering::SeqCst);
                assert_eq!(
                    invoke(&name, serde_json::json!({})).1,
                    if reason.is_some() {
                        ExecutionResultStatus::Rejected
                    } else {
                        ExecutionResultStatus::Succeeded
                    }
                );
                assert_eq!(
                    external_calls.load(Ordering::SeqCst) - before,
                    usize::from(reason.is_none()),
                    "{name}: denied tools must not reach an executor"
                );
            }
        }
        assert_eq!(
            invoke("file_read", serde_json::json!({"path":"kept.txt"})).1,
            ExecutionResultStatus::Succeeded
        );
        assert_eq!(
            std::fs::read_to_string(path.join("kept.txt")).unwrap(),
            "original"
        );
    }
}

#[test]
fn live_closed_skill_policy_precedes_mcp_plugin_and_orchestration_dispatch() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let session_id = SessionId::new("closed-skill-all-dispatch");
    session_store
        .create_session(session_id.clone(), "closed skill")
        .unwrap();
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let task = coordinator_task(test_task("skill-root", "skill-root", None));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    )
    .with_external_tool_catalog_provider(Arc::new(|| {
        magi_tool_runtime::ExternalToolCatalogSnapshot {
            mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                server_id: "repo".into(),
                server_name: "Repo".into(),
                model_tool_name: "mcp__repo__inspect".into(),
                tool_name: "inspect".into(),
                description: "inspect".into(),
                read_only: true,
                input_schema: serde_json::json!({"type":"object"}),
            }],
            ..Default::default()
        }
    }))
    .with_external_mcp_tool_executor(Arc::new(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        ("{}".into(), ExecutionResultStatus::Succeeded)
    }));
    let skill_registry = magi_skill_runtime::SkillRegistry::new();
    let skill = |restricted| magi_skill_runtime::SkillDefinition {
        description: String::new(),
        skill_id: "workflow".into(),
        title: "Workflow".into(),
        instruction: "Inspect".into(),
        metadata: magi_skill_runtime::SkillMetadata {
            category: "test".into(),
            tags: vec![],
        },
        restrict_standard_tools: restricted,
        allowed_tools: vec!["file_read".into()],
        custom_tool_bindings: vec![],
        prompt_priority: 50,
    };
    skill_registry.register(skill(false));
    let runtime = magi_skill_runtime::SkillRuntime::new(skill_registry.clone());
    let invoke = |name: &str| {
        execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            Some(&runtime),
            None,
            Some("workflow"),
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            None,
            &plan_store,
            None,
            &task,
            &session_id,
            &None,
            None,
            None,
            None,
            &ChatToolCall {
                id: name.into(),
                kind: "function".into(),
                function: ChatToolFunction {
                    name: name.into(),
                    arguments: "{}".into(),
                },
            },
            None,
            &|| true,
            None,
        )
    };
    assert_eq!(
        invoke("mcp__repo__inspect").1,
        ExecutionResultStatus::Succeeded
    );
    skill_registry.register(skill(true));
    for name in [
        "mcp__repo__inspect",
        "agent_spawn",
        "update_plan",
        "file_write",
    ] {
        let (payload, status) = invoke(name);
        assert_eq!(status, ExecutionResultStatus::Rejected, "{name}: {payload}");
        assert!(payload.contains("tool_policy_rejected"));
    }
    skill_registry.replace([]);
    assert_eq!(
        invoke("mcp__repo__inspect").1,
        ExecutionResultStatus::Rejected,
        "移除 Skill 不能使旧 schema 重新获得全部工具"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "关闭 Skill 后不能再进入 MCP executor"
    );
    assert!(session_store.plan(&session_id).is_none());
    assert!(task_store.all_tasks().is_empty());
}

#[test]
fn no_tools_policy_rejects_state_write_tool() {
    let mut task = test_task("task-no-tools-state-tool", "task-no-tools-state-tool", None);
    let mut policy = default_agent_spawn_policy();
    policy.command_mode = "no_tools".to_string();
    task.policy_snapshot = Some(policy);

    let decision = task_policy_tool_decision(&task, BuiltinToolName::AgentSpawn.as_str(), "{}")
        .expect("no_tools should reject agent_spawn");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("rejection should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(
        payload["tool"].as_str(),
        Some(BuiltinToolName::AgentSpawn.as_str())
    );
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
    );
    assert!(payload["required_access_profile"].is_null());
    assert_eq!(
        payload["retryable_with_same_arguments"].as_bool(),
        Some(false)
    );
    assert!(
        !decision.payload.contains("当前任务阶段不允许调用工具"),
        "公开 payload 不应泄漏内部任务策略原因: {}",
        decision.payload
    );
}

#[test]
fn task_runtime_allows_update_plan_in_read_only_mode() {
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-read-only-state-tool");
    let workspace_id = Some(WorkspaceId::new("workspace-read-only-state-tool"));
    session_store
        .create_session(session_id.clone(), "read only internal state")
        .expect("session should exist");
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let mut task = coordinator_task(test_task(
        "task-read-only-state-tool",
        "task-read-only-state-tool",
        None,
    ));
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::ReadOnly;
    task.policy_snapshot = Some(policy);
    let tool_call = ChatToolCall {
        id: "tool-call-read-only-plan-update".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::UpdatePlan.as_str().to_string(),
            arguments: serde_json::json!({
                "planId": null,
                "expectedRevision": 0,
                "expectedGoalId": null,
                "expectedGoalControlRevision": null,
                "language": "zh-CN",
                "plan": [
                    {
                        "itemId": "read-only-analysis",
                        "step": "保持只读任务进度可见",
                        "status": "in_progress"
                    }
                ]
            })
            .to_string(),
        },
    };

    let (payload, status) = execute_task_tool_call(
        &event_bus,
        None,
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &tool_call,
        None,
        &|| true,
        None,
    );
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("plan payload json");

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    assert_eq!(parsed["status"].as_str(), Some("succeeded"));
    assert!(!plan_store.is_empty(), "只读访问不能阻止会话内部计划更新");
}

#[test]
fn model_goal_completion_settles_only_revoked_agents_and_reports_failures() {
    for settlement_fails in [false, true] {
        let event_bus = InMemoryEventBus::new(16);
        let roles = magi_agent_role::AgentRoleRegistry::load_default();
        let tasks = TaskStore::new();
        let sessions = SessionStore::new();
        let registry = TaskExecutionRegistry::default();
        let conversations = ConversationRegistry::new();
        let plans = crate::test_plan_store("goal-agent-settlement");
        let session = SessionId::new("model-goal-agent-settlement");
        sessions
            .create_session(session.clone(), "model Goal control")
            .unwrap();
        let parent = coordinator_task(test_task("goal-controller", "goal-controller", None));
        tasks.insert_task(parent.clone()).unwrap();
        let (_, thread) = sessions
            .ensure_session_mission(&session, UtcMillis::now(), || parent.mission_id.clone());
        let goal = sessions
            .create_goal(
                session.clone(),
                thread,
                parent.task_id.as_str(),
                "核对代理资源",
                AccessProfile::FullAccess,
            )
            .unwrap();
        for (id, owner) in [
            (
                "goal-owned-agent",
                magi_core::AgentRunOwner::Goal {
                    goal_id: goal.goal_id.clone(),
                    control_revision: goal.control_revision,
                },
            ),
            ("session-owned-agent", magi_core::AgentRunOwner::Session),
        ] {
            let mut child = test_task(id, parent.task_id.as_str(), Some(parent.task_id.clone()));
            let lifetime = match &owner {
                magi_core::AgentRunOwner::Session => magi_core::AgentLifetime::Session,
                magi_core::AgentRunOwner::Goal { goal_id, .. } => magi_core::AgentLifetime::Goal {
                    goal_id: goal_id.clone(),
                },
            };
            let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
            binding.agent_activation = Some(magi_core::AgentActivation {
                agent_id: child.task_id.clone(),
                session_id: session.clone(),
                lifetime,
                generation: 1,
                previous_task_id: None,
            });
            child.executor_binding = Some(binding);
            tasks.insert_task(child.clone()).unwrap();
            tasks
                .transfer_agent_subtree(
                    &parent.task_id,
                    &child.task_id,
                    magi_core::AgentRunTransfer {
                        operation_id: format!("transfer-{id}"),
                        session_id: session.clone(),
                        source_turn_id: parent.task_id.to_string(),
                        transferred_by: parent.task_id.clone(),
                        activation_generation: 1,
                        owner,
                        transferred_at: UtcMillis::now(),
                    },
                )
                .unwrap();
        }
        let settled = Arc::new(AtomicUsize::new(0));
        let observed = settled.clone();
        registry.set_agent_subtree_settler(move |_, ids| {
            assert_eq!(ids, &[TaskId::new("goal-owned-agent")]);
            observed.fetch_add(1, Ordering::SeqCst);
            if settlement_fails {
                Err("进程尚未退出".into())
            } else {
                Ok(())
            }
        });
        let call = ChatToolCall { id: "goal-complete".into(), kind: "function".into(), function: ChatToolFunction {
                name: "update_goal".into(), arguments: serde_json::json!({
                    "goal_id":goal.goal_id, "status":"complete", "expected_revision":goal.control_revision,
                    "expected_plan_revision":null, "completion_summary":"已核对工作产物", "evidence_refs":[]
                }).to_string(),
            }};
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            None,
            &roles,
            None,
            None,
            None,
            &tasks,
            &sessions,
            &registry,
            &conversations,
            None,
            &plans,
            None,
            &parent,
            &session,
            &None,
            None,
            None,
            None,
            &call,
            None,
            &|| true,
            None,
        );
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["goal"]["status"], "complete");
        assert_eq!(payload["agent_resources_settled"], !settlement_fails);
        assert_eq!(
            status,
            if settlement_fails {
                ExecutionResultStatus::Failed
            } else {
                ExecutionResultStatus::Succeeded
            }
        );
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        assert_eq!(
            tasks
                .get_task(&TaskId::new("goal-owned-agent"))
                .unwrap()
                .status,
            TaskStatus::Killed
        );
        assert_eq!(
            tasks
                .get_task(&TaskId::new("session-owned-agent"))
                .unwrap()
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            tasks.get_task(&parent.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(tasks.all_tasks().len(), 3, "结算不能创建替代代理");
    }
}

#[test]
fn goal_tool_failures_are_classified_and_carry_current_state() {
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let session_id = SessionId::new("session-goal-tool-failures");
    let workspace_id = Some(WorkspaceId::new("workspace-goal-tool-failures"));
    session_store
        .create_session(session_id.clone(), "goal tool failures")
        .expect("session should exist for goal tools");
    let task = coordinator_task(test_task(
        "task-goal-tool-failures",
        "task-goal-tool-failures",
        None,
    ));
    let call = |name: BuiltinToolName, arguments: serde_json::Value| {
        let tool_call = ChatToolCall {
            id: format!("tool-call-{}", name.as_str()),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.as_str().to_string(),
                arguments: arguments.to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            None,
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            None,
            &tool_call,
            None,
            &|| true,
            None,
        );
        (
            serde_json::from_str::<serde_json::Value>(&payload).expect("payload is json"),
            status,
        )
    };

    let (created, created_status) = call(
        BuiltinToolName::CreateGoal,
        serde_json::json!({ "objective": "失败分类" }),
    );
    assert_eq!(created_status, ExecutionResultStatus::Succeeded);
    assert_eq!(created["status"], "succeeded", "状态只用规范标签");
    assert_eq!(created["outcome"], "created");
    let goal_id = created["goal"]["goalId"]
        .as_str()
        .expect("goal id")
        .to_string();
    let control_revision = created["goal"]["controlRevision"]
        .as_u64()
        .expect("revision");

    // 已有未结束的目标：不能再创建，并带回当前目标。
    let (duplicate, duplicate_status) = call(
        BuiltinToolName::CreateGoal,
        serde_json::json!({ "objective": "再来一个" }),
    );
    assert_eq!(duplicate_status, ExecutionResultStatus::Failed);
    assert_eq!(duplicate["error_code"], "create_goal_already_unfinished");
    assert_eq!(duplicate["goal"]["goalId"], goal_id.as_str());
    assert!(duplicate["instruction"].as_str().is_some());

    // 版本过期：带回当前版本，指引用它重新调用。
    let (stale, stale_status) = call(
        BuiltinToolName::UpdateGoal,
        serde_json::json!({
            "goal_id": goal_id,
            "status": "complete",
            "expected_revision": control_revision + 5,
            "expected_plan_revision": null,
            "completion_summary": "done",
            "evidence_refs": []
        }),
    );
    assert_eq!(stale_status, ExecutionResultStatus::Failed);
    assert_eq!(stale["error_code"], "update_goal_revision_conflict");
    assert_eq!(stale["goal"]["controlRevision"], control_revision);
    assert!(
        stale["instruction"]
            .as_str()
            .is_some_and(|text| text.contains("control_revision"))
    );

    // 参数缺失是 invalid_input，并指引先 get_goal。
    let (missing, missing_status) = call(
        BuiltinToolName::UpdateGoal,
        serde_json::json!({ "status": "complete" }),
    );
    assert_eq!(missing_status, ExecutionResultStatus::Failed);
    assert_eq!(missing["error_code"], "update_goal_invalid_input");
    assert!(
        missing["instruction"]
            .as_str()
            .is_some_and(|text| text.contains("get_goal"))
    );
}

#[test]
fn goal_tools_use_session_goal_store_as_single_state_source() {
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let session_id = SessionId::new("session-goal-tool-state");
    let workspace_id = Some(WorkspaceId::new("workspace-goal-tool-state"));
    session_store
        .create_session(session_id.clone(), "goal tool state")
        .expect("session should exist for goal tools");
    let task = coordinator_task(test_task(
        "task-goal-tool-state",
        "task-goal-tool-state",
        None,
    ));

    let call = |name: BuiltinToolName, arguments: serde_json::Value| {
        let tool_call = ChatToolCall {
            id: format!("tool-call-{}", name.as_str()),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.as_str().to_string(),
                arguments: arguments.to_string(),
            },
        };
        execute_task_tool_call(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            None,
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            None,
            &tool_call,
            None,
            &|| true,
            None,
        )
    };

    let (created_payload, created_status) = call(
        BuiltinToolName::CreateGoal,
        serde_json::json!({
            "objective": "完成 goal 模式升级",
        }),
    );
    assert_eq!(created_status, ExecutionResultStatus::Succeeded);
    let created: serde_json::Value =
        serde_json::from_str(&created_payload).expect("create_goal payload should be json");
    let goal_id = created["goal"]["goalId"]
        .as_str()
        .expect("created goal should expose goal id")
        .to_string();
    assert_eq!(created["goal"]["status"].as_str(), Some("active"));
    assert!(created["goal"].get("tokenBudget").is_none());

    let (current_payload, current_status) = call(BuiltinToolName::GetGoal, serde_json::json!({}));
    assert_eq!(current_status, ExecutionResultStatus::Succeeded);
    let current: serde_json::Value =
        serde_json::from_str(&current_payload).expect("get_goal payload should be json");
    assert_eq!(current["goal"]["goalId"].as_str(), Some(goal_id.as_str()));
    assert_eq!(
        session_store
            .current_goal(&session_id)
            .expect("store should hold current goal")
            .objective,
        "完成 goal 模式升级"
    );

    let (missing_plan_revision_payload, missing_plan_revision_status) = call(
        BuiltinToolName::UpdateGoal,
        serde_json::json!({
            "goal_id": goal_id,
            "expected_revision": current["goal"]["controlRevision"],
            "status": "complete",
            "completion_summary": "不应绕过计划版本校验",
        }),
    );
    assert_eq!(missing_plan_revision_status, ExecutionResultStatus::Failed);
    let missing_plan_revision: serde_json::Value =
        serde_json::from_str(&missing_plan_revision_payload)
            .expect("missing plan revision payload should be json");
    assert!(
        missing_plan_revision["error"]
            .as_str()
            .unwrap_or_default()
            .contains("expected_plan_revision")
    );

    let (updated_payload, updated_status) = call(
        BuiltinToolName::UpdateGoal,
        serde_json::json!({
            "goal_id": goal_id,
            "expected_revision": current["goal"]["controlRevision"],
            "expected_plan_revision": null,
            "status": "complete",
            "completion_summary": "Goal 模式升级已完成并通过状态存储验证",
            "evidence_refs": ["tool-call-get_goal"],
        }),
    );
    assert_eq!(updated_status, ExecutionResultStatus::Succeeded);
    let updated: serde_json::Value =
        serde_json::from_str(&updated_payload).expect("update_goal payload should be json");
    assert_eq!(updated["goal"]["goalId"].as_str(), Some(goal_id.as_str()));
    assert_eq!(updated["goal"]["status"].as_str(), Some("complete"));
    assert_eq!(
        session_store
            .current_goal(&session_id)
            .expect("store should hold updated goal")
            .status,
        GoalStatus::Complete
    );
}

#[test]
fn read_only_agent_policy_rejects_shell_with_write_redirection() {
    let mut task = test_task(
        "task-read-only-shell-redirection",
        "task-read-only-shell-redirection",
        None,
    );
    task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
        &read_only_agent_spawn_policy(),
    )));

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"printf hidden > out.txt","access_mode":"read_only"}"#,
    )
    .expect("read-only agent should reject write-like shell even when declared read_only");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("rejection should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["status"].as_str(), Some("rejected"));
    assert_eq!(payload["tool"].as_str(), Some("shell_exec"));
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some("该工具在当前访问模式下不可用")
    );
}

#[test]
fn full_access_policy_allows_read_only_shell_dev_null_probe() {
    let mut task = test_task(
        "task-full-access-shell-dev-null",
        "task-full-access-shell-dev-null",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    task.policy_snapshot = Some(policy);

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"if command -v rg >/dev/null 2>&1; then rg --files; fi","access_mode":"read_only"}"#,
    );

    assert!(
        decision.is_none(),
        "full_access 下只读探测丢弃输出到 /dev/null 不应被 preflight 拒绝"
    );
}

#[test]
fn restricted_policy_allows_real_goal_workspace_inspection_command() {
    let mut task = test_task(
        "task-restricted-goal-shell-inspection",
        "task-restricted-goal-shell-inspection",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());
    let arguments = serde_json::json!({
            "access_mode": "read_only",
            "command": "echo \"--- root ---\"; ls -la; echo \"--- src ---\"; find src -type f 2>/dev/null | sort; echo \"--- tests ---\"; find tests -type f 2>/dev/null | sort; echo \"--- docs ---\"; find docs -type f 2>/dev/null | sort; echo \"--- package.json ---\"; cat package.json 2>/dev/null; echo \"--- tsconfig ---\"; cat tsconfig.json 2>/dev/null"
        })
        .to_string();

    let decision =
        task_policy_tool_decision(&task, BuiltinToolName::ShellExec.as_str(), &arguments);

    assert!(
        decision.is_none(),
        "受限访问下的真实只读 Goal 工作区探查不能被权限预检误拒绝"
    );
}

#[test]
fn read_only_agent_policy_allows_compound_repository_inspection_with_remote_query() {
    let mut task = test_task(
        "task-read-only-shell-repository-inspection",
        "task-read-only-shell-repository-inspection",
        None,
    );
    task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
        &read_only_agent_spawn_policy(),
    )));
    let arguments = serde_json::json!({
            "access_mode": "read_only",
            "command": "ls -la && if [ -f README.md ]; then echo README; else echo NO_README; fi; if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then git remote -v 2>/dev/null | head -5; git log --oneline -5 2>/dev/null; else echo NOT_GIT_WORKTREE; fi"
        })
        .to_string();

    let decision =
        task_policy_tool_decision(&task, BuiltinToolName::ShellExec.as_str(), &arguments);

    assert!(decision.is_none(), "只读仓库探查不应在对话预检层被拒绝");
}

#[test]
fn full_access_policy_allows_search_patterns_named_after_write_commands() {
    let mut task = test_task(
        "task-full-access-shell-search-pattern",
        "task-full-access-shell-search-pattern",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "full".to_string();
    task.policy_snapshot = Some(policy);
    let arguments = serde_json::json!({
            "access_mode": "read_only",
            "command": "grep -rn \"compress\\|truncate\" crates/magi-context-runtime crates/magi-conversation-runtime --include='*.rs' 2>/dev/null | head -40"
        })
        .to_string();

    let decision =
        task_policy_tool_decision(&task, BuiltinToolName::ShellExec.as_str(), &arguments);

    assert!(
        decision.is_none(),
        "检索参数中的写命令名称不应被误判为实际写操作"
    );
}

#[test]
fn full_access_policy_reclassifies_misdeclared_shell_without_rejecting_it() {
    let mut task = test_task(
        "task-full-access-shell-reclassification",
        "task-full-access-shell-reclassification",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "full".to_string();
    task.policy_snapshot = Some(policy);

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"touch created.txt","access_mode":"read_only"}"#,
    );

    assert!(
        decision.is_none(),
        "完全访问不应因模型错误声明 read_only 而拒绝 shell"
    );
}

#[test]
fn full_access_task_constraint_is_not_reported_as_access_mode_failure() {
    let mut task = test_task(
        "task-full-access-explicit-tool-denial",
        "task-full-access-explicit-tool-denial",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy
        .denied_tools
        .push(BuiltinToolName::FileRead.as_str().to_string());
    task.policy_snapshot = Some(policy);

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::FileRead.as_str(),
        r#"{"path":"README.md"}"#,
    )
    .expect("explicit task constraint should reject the tool");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(
        payload["error"].as_str(),
        Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
    );
    assert_ne!(
        payload["error"].as_str(),
        Some("该工具在当前访问模式下不可用")
    );
}

#[test]
fn restricted_policy_reclassifies_misdeclared_shell_as_needs_approval() {
    let mut task = test_task(
        "task-restricted-shell-reclassification",
        "task-restricted-shell-reclassification",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"touch blocked.txt","access_mode":"read_only"}"#,
    )
    .expect("受限访问应把错误只读声明升级为审批，而不是直接拒绝");

    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
}

#[test]
fn restricted_policy_marks_write_shell_as_needs_approval() {
    let mut task = test_task(
        "task-human-approval-shell",
        "task-human-approval-shell",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    let decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"cargo test"}"#,
    )
    .expect("human checkpoint policy should require approval for write-like shell");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(payload["status"].as_str(), Some("needs_approval"));
    assert_eq!(
        payload["error_code"].as_str(),
        Some("tool_policy_needs_approval")
    );
    assert_eq!(
        payload["error"].as_str(),
        Some("该操作需要你的确认，授权后将继续当前调用")
    );
    assert_eq!(payload["access_profile"].as_str(), Some("restricted"));
    assert_eq!(
        payload["restriction_kind"].as_str(),
        Some("approval_required")
    );
    assert_eq!(
        payload["required_access_profile"].as_str(),
        Some("full_access")
    );
    assert_eq!(
        payload["retryable_with_same_arguments"].as_bool(),
        Some(false)
    );
    assert!(
        payload["instruction"]
            .as_str()
            .is_some_and(|instruction| instruction.contains("当前调用已暂停"))
    );
}

#[test]
fn full_access_policy_keeps_write_shell_autonomous() {
    let mut task = test_task("task-full-access-shell", "task-full-access-shell", None);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::FullAccess;

    assert!(
        task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"cargo test"}"#,
        )
        .is_none()
    );
}

#[test]
fn restricted_default_scope_requires_approval_for_paths_outside_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-outside-write-target.txt");
    let mut task = test_task(
        "task-restricted-default-path-scope",
        "task-restricted-default-path-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    assert!(
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileWrite.as_str(),
            r#"{"path":"src/lib.rs","content":"ok"}"#,
            Some(&workspace_root),
        )
        .is_none()
    );

    let decision = task_policy_tool_decision_with_workspace_root(
        &task,
        BuiltinToolName::FileWrite.as_str(),
        &serde_json::json!({
            "path": outside_path.display().to_string(),
            "content": "outside"
        })
        .to_string(),
        Some(&workspace_root),
    )
    .expect("restricted default scope should gate outside workspace path");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    // 受限执行写工作区外的路径等待用户授权，而不是直接拒绝让任务失败。
    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(
        payload["error_code"].as_str(),
        Some("tool_policy_needs_approval")
    );
    assert_eq!(
        payload["required_access_profile"].as_str(),
        Some("full_access")
    );
    assert!(
        !decision
            .payload
            .contains(outside_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["restriction_kind"].as_str(),
        Some("approval_required")
    );
}

#[cfg(unix)]
#[test]
fn restricted_shell_workspace_uses_canonical_root_for_symlink_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let real_root = workspace.path().join("real-root");
    let link_root = workspace.path().join("link-root");
    std::fs::create_dir_all(&real_root).expect("real workspace root should be creatable");
    std::os::unix::fs::symlink(&real_root, &link_root)
        .expect("workspace symlink should be creatable");

    let decision = access_profile_tool_decision(AccessProfileToolDecisionInput {
        access_profile: magi_core::AccessProfile::Restricted,
        command_mode: "",
        allowed_tools: &[],
        denied_tools: &[],
        allowed_paths: &[],
        denied_paths: &[],
        read_only_paths: &[],
        requested_tool_name: BuiltinToolName::ShellExec.as_str(),
        arguments: r#"{"command":"printf restricted > out.txt"}"#,
        workspace_root_path: Some(&link_root),
    })
    .expect("restricted write shell inside symlinked workspace should require approval");

    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
}

#[test]
fn reads_outside_the_workspace_are_allowed_but_writes_stay_scoped() {
    let workspace = tempdir().expect("workspace tempdir");
    let outside = tempdir().expect("outside tempdir");
    let root = workspace.path().to_path_buf();
    let outside_file = outside.path().join("note.txt");
    let outside_dir = outside.path().to_string_lossy().to_string();
    let decide = |profile: magi_core::AccessProfile,
                  tool: &str,
                  arguments: serde_json::Value,
                  allowed: &[String],
                  denied: &[String]| {
        access_profile_tool_decision(AccessProfileToolDecisionInput {
            access_profile: profile,
            command_mode: "",
            allowed_tools: &[],
            denied_tools: &[],
            allowed_paths: allowed,
            denied_paths: denied,
            read_only_paths: &[],
            requested_tool_name: tool,
            arguments: &arguments.to_string(),
            workspace_root_path: Some(&root),
        })
    };
    let read = serde_json::json!({ "path": outside_file.to_string_lossy() });
    let search = serde_json::json!({ "query": "x", "root": outside_dir });
    let write = serde_json::json!({ "path": outside_file.to_string_lossy(), "content": "x" });

    for profile in [
        magi_core::AccessProfile::ReadOnly,
        magi_core::AccessProfile::Restricted,
        magi_core::AccessProfile::FullAccess,
    ] {
        assert!(
            decide(profile, "file_read", read.clone(), &[], &[]).is_none(),
            "{profile:?} 应允许读取工作区外的文件"
        );
        assert!(
            decide(profile, "search_text", search.clone(), &[], &[]).is_none(),
            "{profile:?} 应允许在工作区外搜索"
        );
    }
    // 写入仍然限定在工作区：只读直接拒绝，受限对越界路径等待授权，完全访问不受默认范围约束。
    for (profile, expected) in [
        (
            magi_core::AccessProfile::ReadOnly,
            ExecutionResultStatus::Rejected,
        ),
        (
            magi_core::AccessProfile::Restricted,
            ExecutionResultStatus::NeedsApproval,
        ),
    ] {
        let decision = decide(profile, "file_write", write.clone(), &[], &[])
            .expect("工作区外的写入不能直接放行");
        assert_eq!(decision.status, expected, "{profile:?}");
    }
    // 任务策略里显式配置的范围仍然生效：显式 allowed_paths 之外、denied_paths 之内都不能读。
    let explicit = [root.join("src").to_string_lossy().to_string()];
    assert!(
        decide(
            magi_core::AccessProfile::Restricted,
            "file_read",
            read.clone(),
            &explicit,
            &[]
        )
        .is_some(),
        "显式 allowed_paths 之外不能读"
    );
    assert!(
        decide(
            magi_core::AccessProfile::Restricted,
            "file_read",
            read,
            &[],
            &[outside_dir]
        )
        .is_some(),
        "denied_paths 之内不能读"
    );
}

#[test]
fn read_only_shell_runs_any_command_that_does_not_write() {
    let workspace = tempdir().expect("workspace tempdir");
    let root = workspace.path().to_path_buf();
    let decide = |arguments: &str| {
        access_profile_tool_decision(AccessProfileToolDecisionInput {
            access_profile: magi_core::AccessProfile::ReadOnly,
            command_mode: "",
            allowed_tools: &[],
            denied_tools: &[],
            allowed_paths: &[],
            denied_paths: &[],
            read_only_paths: &[],
            requested_tool_name: "shell_exec",
            arguments,
            workspace_root_path: Some(&root),
        })
    };

    // 不写文件的命令都能执行：不必声明 access_mode，`2>/dev/null` / 管道不算写入，
    // 读取工作区之外的路径也可以（只读 shell 不受默认的工作区范围限制）。
    for command in [
        "pwd && ls -la",
        "rg -n foo src | head -20",
        "if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then git log --oneline -5 2>/dev/null; git status --short 2>/dev/null | head -30; else echo NO; fi",
        "cat /etc/hosts",
        "ls /tmp",
        "git -C /private/tmp log --oneline -3",
        "wc -l ../other/file.txt",
    ] {
        for arguments in [
            serde_json::json!({ "command": command }),
            serde_json::json!({ "command": command, "access_mode": "read_only" }),
        ] {
            assert!(
                decide(&arguments.to_string()).is_none(),
                "只读模式应允许：{arguments}"
            );
        }
    }

    // 会写文件的仍然拒绝：声明了写入、重定向到文件、写类命令、改仓库的 git 子命令。
    for arguments in [
        serde_json::json!({ "command": "ls", "access_mode": "explicit_write" }),
        serde_json::json!({ "command": "echo hi > out.txt" }),
        serde_json::json!({ "command": "echo hi > out.txt", "access_mode": "read_only" }),
        serde_json::json!({ "command": "touch out.txt" }),
        serde_json::json!({ "command": "rm -rf build" }),
        serde_json::json!({ "command": "git commit -m x" }),
        serde_json::json!({ "command": "curl -O https://example.test/a.bin" }),
    ] {
        let decision = decide(&arguments.to_string())
            .unwrap_or_else(|| panic!("只读模式应拒绝写入：{arguments}"));
        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    }
}

#[test]
fn browser_web_actions_follow_the_access_profile() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let decide = |profile: magi_core::AccessProfile, tool: &str, arguments: serde_json::Value| {
        access_profile_tool_decision(AccessProfileToolDecisionInput {
            access_profile: profile,
            command_mode: "",
            allowed_tools: &[],
            denied_tools: &[],
            allowed_paths: &[],
            denied_paths: &[],
            read_only_paths: &[],
            requested_tool_name: tool,
            arguments: &arguments.to_string(),
            workspace_root_path: Some(&workspace_root),
        })
    };
    use magi_core::AccessProfile::{FullAccess, ReadOnly, Restricted};

    // 观察类动作在任何档位都不拦。
    for tool in [
        "browser_navigate",
        "browser_snapshot",
        "browser_read",
        "browser_scroll",
    ] {
        for profile in [ReadOnly, Restricted, FullAccess] {
            assert!(
                decide(profile, tool, serde_json::json!({})).is_none(),
                "{tool} {profile:?}"
            );
        }
    }
    // 点击 / 输入等交互动作：只读拒绝，受限与完全访问放行。
    let click = serde_json::json!({ "element_ref": "e:1:2" });
    let rejected = decide(ReadOnly, "browser_click", click.clone()).expect("只读拒绝点击");
    assert_eq!(rejected.status, ExecutionResultStatus::Rejected);
    assert!(decide(Restricted, "browser_click", click.clone()).is_none());
    assert!(decide(FullAccess, "browser_click", click).is_none());
    // 执行脚本 / 写站点存储：只读拒绝，受限需要确认，完全访问放行。
    for (tool, arguments) in [
        (
            "browser_evaluate",
            serde_json::json!({ "expression": "1+1" }),
        ),
        (
            "browser_storage",
            serde_json::json!({ "area": "local", "action": "set", "key": "k", "value": "v" }),
        ),
        (
            "browser_storage",
            serde_json::json!({ "area": "cookies", "action": "clear" }),
        ),
        ("browser_webmcp", serde_json::json!({ "action": "execute" })),
    ] {
        let read_only = decide(ReadOnly, tool, arguments.clone()).expect("只读拒绝");
        assert_eq!(read_only.status, ExecutionResultStatus::Rejected, "{tool}");
        let restricted = decide(Restricted, tool, arguments.clone()).expect("受限需要确认");
        assert_eq!(
            restricted.status,
            ExecutionResultStatus::NeedsApproval,
            "{tool}"
        );
        assert!(decide(FullAccess, tool, arguments).is_none(), "{tool}");
    }
    // 只读类的存储 / 对话框 / WebMCP 操作不受影响。
    for (tool, arguments) in [
        (
            "browser_storage",
            serde_json::json!({ "area": "local", "action": "list" }),
        ),
        ("browser_dialog", serde_json::json!({ "action": "list" })),
        ("browser_webmcp", serde_json::json!({ "action": "list" })),
    ] {
        for profile in [ReadOnly, Restricted, FullAccess] {
            assert!(
                decide(profile, tool, arguments.clone()).is_none(),
                "{tool} {profile:?}"
            );
        }
    }
}

#[test]
fn browser_download_save_uses_the_same_write_scope_as_file_write() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-outside-download-target.bin");
    let mut task = test_task(
        "task-browser-download-scope",
        "task-browser-download-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());
    let decide = |arguments: serde_json::Value| {
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::BrowserDownload.as_str(),
            &arguments.to_string(),
            Some(&workspace_root),
        )
    };

    assert!(
        decide(serde_json::json!({ "action": "list" })).is_none(),
        "列出下载不涉及任何路径"
    );
    assert!(
        decide(serde_json::json!({
            "action": "save",
            "download_id": "d1",
            "destination_path": "build/report.pdf",
        }))
        .is_none(),
        "保存到工作区内可以"
    );
    let rejected = decide(serde_json::json!({
        "action": "save",
        "download_id": "d1",
        "destination_path": outside_path.display().to_string(),
    }))
    .expect("工作区外保存需要授权");
    assert_eq!(rejected.status, ExecutionResultStatus::NeedsApproval);
}

#[test]
fn browser_upload_file_uses_the_same_read_scope_as_file_read() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-outside-upload-source.txt");
    let mut task = test_task(
        "task-browser-upload-scope",
        "task-browser-upload-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());
    let decide = |arguments: serde_json::Value| {
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::BrowserUploadFile.as_str(),
            &arguments.to_string(),
            Some(&workspace_root),
        )
    };

    assert!(
        decide(serde_json::json!({
            "element_ref": "e:1:2",
            "file_path": "assets/logo.png",
        }))
        .is_none(),
        "工作区内的相对路径可以上传"
    );
    for arguments in [
        serde_json::json!({
            "element_ref": "e:1:2",
            "file_path": outside_path.display().to_string(),
        }),
        serde_json::json!({
            "element_ref": "e:1:2",
            "file_paths": ["assets/logo.png", outside_path.display().to_string()],
        }),
    ] {
        let decision = decide(arguments).expect("工作区外的文件不能直接上传");
        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    }
}

#[test]
fn restricted_default_scope_checks_filesystem_alias_paths() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-outside-alias-target.txt");
    let mut task = test_task(
        "task-restricted-default-alias-path-scope",
        "task-restricted-default-alias-path-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    for (tool, arguments) in [
        (
            BuiltinToolName::FileCopy,
            serde_json::json!({
                "source": "src/input.txt",
                "destination": outside_path.display().to_string()
            }),
        ),
        (
            BuiltinToolName::FileMove,
            serde_json::json!({
                "source": outside_path.display().to_string(),
                "destination": "src/output.txt"
            }),
        ),
        (
            BuiltinToolName::FileMkdir,
            serde_json::json!({
                "path": outside_path.display().to_string()
            }),
        ),
    ] {
        let decision = task_policy_tool_decision_with_workspace_root(
            &task,
            tool.as_str(),
            &arguments.to_string(),
            Some(&workspace_root),
        )
        .expect("restricted default scope should gate alias path outside workspace");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(
            payload["error_code"].as_str(),
            Some("tool_policy_needs_approval"),
            "unexpected payload: {payload}"
        );
        assert_eq!(
            payload["restriction_kind"].as_str(),
            Some("approval_required")
        );
        assert!(
            !decision
                .payload
                .contains(outside_path.to_string_lossy().as_ref())
        );
    }
}

#[test]
fn restricted_default_scope_allows_reading_paths_outside_the_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-outside-read-target.png");
    let mut task = test_task(
        "task-restricted-default-read-path-scope",
        "task-restricted-default-read-path-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    // 默认范围只约束写入；读取（含 view_image）不受工作区限制。
    for tool in [BuiltinToolName::ViewImage, BuiltinToolName::FileRead] {
        assert!(
            task_policy_tool_decision_with_workspace_root(
                &task,
                tool.as_str(),
                &serde_json::json!({ "path": outside_path.display().to_string() }).to_string(),
                Some(&workspace_root),
            )
            .is_none(),
            "{} 应允许读取工作区外的路径",
            tool.as_str()
        );
    }
}

#[cfg(unix)]
#[test]
fn restricted_default_scope_gates_symlink_escape_writes() {
    let workspace = tempdir().expect("workspace tempdir");
    let outside = tempdir().expect("outside tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_secret = outside.path().join("secret.txt");
    std::fs::write(&outside_secret, "secret").expect("write outside secret");
    std::os::unix::fs::symlink(&outside_secret, workspace_root.join("linked-secret.txt"))
        .expect("create file symlink");
    std::os::unix::fs::symlink(outside.path(), workspace_root.join("linked-dir"))
        .expect("create dir symlink");

    let mut task = test_task(
        "task-restricted-default-symlink-path-scope",
        "task-restricted-default-symlink-path-scope",
        None,
    );
    task.policy_snapshot = Some(default_agent_spawn_policy());

    // 读取指向工作区外的符号链接是允许的（读取不受工作区限制）；写入穿过链接逃出工作区需要授权。
    assert!(
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileRead.as_str(),
            &serde_json::json!({ "path": "linked-secret.txt" }).to_string(),
            Some(&workspace_root),
        )
        .is_none()
    );
    let arguments = serde_json::json!({
        "path": "linked-dir/new-file.txt",
        "content": "outside"
    });
    let decision = task_policy_tool_decision_with_workspace_root(
        &task,
        BuiltinToolName::FileWrite.as_str(),
        &arguments.to_string(),
        Some(&workspace_root),
    )
    .expect("restricted default scope should gate symlink escape path");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(
        payload["error_code"].as_str(),
        Some("tool_policy_needs_approval"),
        "unexpected payload: {payload}"
    );
    assert_eq!(
        payload["restriction_kind"].as_str(),
        Some("approval_required")
    );
}

#[test]
fn full_access_default_scope_allows_paths_outside_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-full-access-outside-target.txt");
    let mut task = test_task(
        "task-full-access-default-path-scope",
        "task-full-access-default-path-scope",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    task.policy_snapshot = Some(policy);

    assert!(
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileWrite.as_str(),
            &serde_json::json!({
                "path": outside_path.display().to_string(),
                "content": "outside"
            })
            .to_string(),
            Some(&workspace_root),
        )
        .is_none()
    );
}

#[test]
fn read_only_command_mode_constrains_full_access_preflight_scope() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let outside_path = workspace_root
        .parent()
        .expect("workspace should have parent")
        .join("magi-command-mode-outside-target.txt");
    let mut task = test_task(
        "task-command-mode-read-only-scope",
        "task-command-mode-read-only-scope",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "read_only".to_string();
    task.policy_snapshot = Some(policy);

    // 读取不受工作区限制；只读命令模式下写入（含工作区外）一律拒绝。
    assert!(
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileRead.as_str(),
            &serde_json::json!({ "path": outside_path.display().to_string() }).to_string(),
            Some(&workspace_root),
        )
        .is_none()
    );
    let decision = task_policy_tool_decision_with_workspace_root(
        &task,
        BuiltinToolName::FileWrite.as_str(),
        &serde_json::json!({
            "path": outside_path.display().to_string(),
            "content": "x"
        })
        .to_string(),
        Some(&workspace_root),
    )
    .expect("read_only command mode should reject writes");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["access_profile"].as_str(), Some("read_only"));
}

#[test]
fn task_policy_allowed_paths_are_resolved_against_workspace_root() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let mut task = test_task("task-allowed-paths", "task-allowed-paths", None);
    let mut policy = default_agent_spawn_policy();
    policy.allowed_paths = vec!["src".to_string()];
    task.policy_snapshot = Some(policy);

    assert!(
        task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileRead.as_str(),
            r#"{"path":"src/lib.rs"}"#,
            Some(&workspace_root),
        )
        .is_none()
    );

    let decision = task_policy_tool_decision_with_workspace_root(
        &task,
        BuiltinToolName::FileRead.as_str(),
        r#"{"path":"README.md"}"#,
        Some(&workspace_root),
    )
    .expect("path outside allow list should be rejected");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
    );
    assert!(payload["required_access_profile"].is_null());
    assert_eq!(
        payload["retryable_with_same_arguments"].as_bool(),
        Some(false)
    );
}

#[test]
fn denied_path_overrides_restricted_shell_approval() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace.path().to_path_buf();
    let mut task = test_task("task-denied-shell-path", "task-denied-shell-path", None);
    let mut policy = default_agent_spawn_policy();
    policy.denied_paths = vec!["private".to_string()];
    task.policy_snapshot = Some(policy);

    let decision = task_policy_tool_decision_with_workspace_root(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"cat secret.txt > out.txt","cwd":"private"}"#,
        Some(&workspace_root),
    )
    .expect("denied path should reject before approval");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
    );
    assert!(payload["required_access_profile"].is_null());
    assert_eq!(
        payload["retryable_with_same_arguments"].as_bool(),
        Some(false)
    );
}

#[test]
fn safety_gate_custom_rule_keeps_require_approval_status() {
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
        "deploy-prod",
        magi_safety_gate::SafetyCategory::Custom,
    )]);

    let decision = safety_gate_tool_decision(
        &gate,
        magi_core::AccessProfile::Restricted,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"deploy-prod"}"#,
    )
    .expect("custom rule should require approval");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(payload["status"].as_str(), Some("needs_approval"));
    assert_eq!(
        payload["error_code"].as_str(),
        Some("tool_safety_needs_approval")
    );
    assert_eq!(
        payload["error"].as_str(),
        Some("安全防护要求确认该操作，授权后将继续当前调用")
    );
    assert!(payload.get("safety_gate").is_none());
    assert!(!decision.payload.contains("deploy-prod"));
    assert!(!decision.payload.contains("custom"));
}

#[test]
fn safety_gate_audit_is_published_before_tool_execution() {
    let event_bus = InMemoryEventBus::new(16);
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
        "deploy-prod",
        magi_safety_gate::SafetyCategory::Custom,
        magi_safety_gate::SafetyAction::AuditOnly,
    )]);
    let session_id = SessionId::new("session-safety-audit");
    let workspace_id = WorkspaceId::new("workspace-safety-audit");
    let mission_id = magi_core::MissionId::new("mission-safety-audit");
    let task_id = TaskId::new("task-safety-audit");

    publish_safety_evaluation_audit(
        &event_bus,
        &gate,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"deploy-prod"}"#,
        "tool-call-safety-audit",
        SafetyEvaluationAuditContext {
            access_profile: magi_core::AccessProfile::Restricted,
            session_id: Some(&session_id),
            workspace_id: Some(&workspace_id),
            mission_id: Some(&mission_id),
            task_id: Some(&task_id),
        },
    );

    let audit = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "security.safety.evaluated")
        .expect("safety audit event should be published");
    assert_eq!(audit.category, magi_event_bus::EventCategory::Audit);
    assert_eq!(audit.payload["decision"], "audit_only");
    assert_eq!(event_bus.audit_usage_ledger_snapshot().audit_count(), 1);
}

#[test]
fn safety_gate_restricted_approval_is_skipped_in_full_access() {
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
        "deploy-prod",
        magi_safety_gate::SafetyCategory::Custom,
    )]);

    assert!(
        safety_gate_tool_decision(
            &gate,
            magi_core::AccessProfile::FullAccess,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"deploy-prod"}"#,
        )
        .is_none()
    );
}

#[test]
fn task_preflight_uses_effective_read_only_command_mode_for_safety_gate() {
    let mut task = test_task(
        "task-effective-read-only-safety",
        "task-effective-read-only-safety",
        None,
    );
    let mut policy = default_agent_spawn_policy();
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "read_only".to_string();
    task.policy_snapshot = Some(policy);
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
        "deploy-prod",
        magi_safety_gate::SafetyCategory::Custom,
    )]);

    let decision = task_tool_preflight_decision(
        &task,
        Some(&gate),
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"printf deploy-prod","access_mode":"read_only"}"#,
        None,
    )
    .expect("effective read-only mode should not skip restricted safety rules");
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
}

#[test]
fn safety_gate_hard_block_is_not_skipped_in_full_access() {
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
        "destroy-everything",
        magi_safety_gate::SafetyCategory::Custom,
        magi_safety_gate::SafetyAction::HardBlock,
    )]);

    let decision = safety_gate_tool_decision(
        &gate,
        magi_core::AccessProfile::FullAccess,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"destroy-everything"}"#,
    )
    .expect("hard block must reject even in full access");

    assert_eq!(decision.status, ExecutionResultStatus::Rejected);
    let payload: serde_json::Value =
        serde_json::from_str(&decision.payload).expect("decision should be json");
    assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
    assert_eq!(payload["error"].as_str(), Some("该操作已被安全防护阻止"));
    assert!(payload.get("safety_gate").is_none());
    assert!(!decision.payload.contains("destroy-everything"));
}

#[test]
fn preflight_selects_safety_hard_block_over_restricted_shell_approval() {
    let mut task = test_task("task-hard-block-shell", "task-hard-block-shell", None);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
        "destroy-everything",
        magi_safety_gate::SafetyCategory::Custom,
        magi_safety_gate::SafetyAction::HardBlock,
    )]);

    let policy_decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"destroy-everything"}"#,
    )
    .expect("restricted shell should require approval before safety merge");
    let safety_decision = safety_gate_tool_decision(
        &gate,
        magi_core::AccessProfile::Restricted,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"destroy-everything"}"#,
    )
    .expect("hard block should reject");

    let selected = select_preflight_decision(Some(policy_decision), Some(safety_decision))
        .expect("preflight should select a decision");
    let payload: serde_json::Value =
        serde_json::from_str(&selected.payload).expect("decision should be json");

    assert_eq!(selected.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
    assert_eq!(payload["error"].as_str(), Some("该操作已被安全防护阻止"));
    assert!(payload.get("safety_gate").is_none());
    assert!(!selected.payload.contains("destroy-everything"));
    assert!(!selected.payload.contains("hard_block"));
}

#[test]
fn preflight_keeps_task_rejection_over_safety_approval() {
    let mut task = test_task("task-no-tools", "task-no-tools", None);
    let mut policy = default_agent_spawn_policy();
    policy.command_mode = "no_tools".to_string();
    task.policy_snapshot = Some(policy);
    let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
        "deploy-prod",
        magi_safety_gate::SafetyCategory::Custom,
    )]);

    let policy_decision = task_policy_tool_decision(
        &task,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"deploy-prod"}"#,
    )
    .expect("no_tools should reject tool calls");
    let safety_decision = safety_gate_tool_decision(
        &gate,
        magi_core::AccessProfile::Restricted,
        BuiltinToolName::ShellExec.as_str(),
        r#"{"command":"deploy-prod"}"#,
    )
    .expect("safety should require approval");

    let selected = select_preflight_decision(Some(policy_decision), Some(safety_decision))
        .expect("preflight should select a decision");
    let payload: serde_json::Value =
        serde_json::from_str(&selected.payload).expect("decision should be json");

    assert_eq!(selected.status, ExecutionResultStatus::Rejected);
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
    );
    assert!(payload["required_access_profile"].is_null());
    assert_eq!(
        payload["retryable_with_same_arguments"].as_bool(),
        Some(false)
    );
    assert!(
        !selected.payload.contains("当前任务阶段不允许调用工具"),
        "公开 payload 不应泄漏内部任务策略原因: {}",
        selected.payload
    );
}

fn persist_test_approval_tool(
    store: &SessionStore,
    session: &SessionId,
    task: &magi_core::Task,
    call: &ChatToolCall,
) {
    let turn = store
        .runtime_sidecar(session)
        .unwrap()
        .current_turn
        .unwrap();
    store.upsert_current_turn_item_for_turn(session,Some(&turn.turn_id),serde_json::from_value(serde_json::json!({
            "itemId":format!("test-tool-{}",call.id),"itemSeq":turn.items.len()+1,"kind":"tool_call_started","status":"running",
            "source":"orchestrator","sourceThreadId":"thread-approval-test","taskId":task.task_id,
            "toolCallId":call.id,"toolName":call.function.name,"toolArguments":call.function.arguments,
        })).unwrap()).unwrap();
}

#[test]
fn task_tool_approval_resumes_original_file_remove_call() {
    run_remove_approval_case("file_remove", false, None);
}

#[test]
fn task_tool_approval_persistence_failure_prevents_real_file_remove() {
    run_remove_approval_case("file_remove", false, Some("awaiting_approval"));
    run_remove_approval_case("file_remove", false, Some("running"));
}

#[test]
fn task_tool_approval_recovers_original_permission_and_executes_real_file_remove_once() {
    run_remove_approval_case("file_remove", false, Some("recover_approval"));
}

#[test]
fn skill_revocation_during_approval_prevents_real_file_remove() {
    run_remove_approval_case("file_remove", false, Some("revoke_skill"));
}

#[test]
fn project_directory_revocation_during_approval_prevents_real_file_remove() {
    for tool in ["file_remove", "mcp__approval__remove"] {
        for child in [false, true] {
            run_remove_approval_case(tool, child, Some("revoke_directory"));
        }
    }
}

#[cfg(unix)]
#[test]
fn project_copy_replaced_during_approval_prevents_builtin_and_external_side_effects() {
    for tool in ["file_remove", "mcp__approval__remove"] {
        for child in [false, true] {
            run_remove_approval_case(tool, child, Some("replace_copy"));
        }
    }
}

#[test]
fn obsolete_execution_after_approval_cannot_remove_file() {
    run_remove_approval_case("file_remove", false, Some("revoke_execution"));
}

#[test]
fn mainline_and_child_external_tools_obey_approval_and_live_revocation() {
    for tool_name in ["mcp__approval__remove"] {
        for child in [false, true] {
            for outcome in [
                None,
                Some("deny"),
                Some("read_only"),
                Some("full_access"),
                Some("revoke_skill"),
            ] {
                run_remove_approval_case(tool_name, child, outcome);
            }
        }
    }
}

fn run_remove_approval_case(tool_name: &'static str, child: bool, failure_stage: Option<&str>) {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let skills = magi_skill_runtime::SkillRegistry::new();
    let definition = |restricted| magi_skill_runtime::SkillDefinition {
        description: String::new(),
        skill_id: "approval-skill".into(),
        title: "Approval".into(),
        instruction: "inspect".into(),
        metadata: magi_skill_runtime::SkillMetadata {
            category: "test".into(),
            tags: Vec::new(),
        },
        restrict_standard_tools: restricted,
        allowed_tools: Vec::new(),
        custom_tool_bindings: Vec::new(),
        prompt_priority: 50,
    };
    skills.register(definition(false));
    let skill_runtime = magi_skill_runtime::SkillRuntime::new(skills.clone());
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_default_builtins();
    let dir = tempdir().expect("temp dir");
    let source_root = if failure_stage == Some("replace_copy") {
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("probe.txt"), "source content").unwrap();
        source
    } else {
        dir.path().to_path_buf()
    };
    let execution_root = if failure_stage == Some("replace_copy") {
        let copy = dir.path().join("copy");
        std::fs::create_dir(&copy).unwrap();
        copy
    } else {
        source_root.clone()
    };
    let workspace_store = Arc::new(magi_workspace::WorkspaceStore::new());
    let directory_workspace = WorkspaceId::new("workspace-file-remove");
    workspace_store
        .register_native_path(directory_workspace.clone(), source_root.clone())
        .unwrap();
    let directory_binding = matches!(failure_stage, Some("revoke_directory" | "replace_copy"))
        .then(|| {
            workspace_store
                .capture_directory_binding(&directory_workspace, None, None)
                .unwrap()
        });
    let execution_registry = TaskExecutionRegistry::default().with_agent_spawn_preflight_runtime(
        crate::task_execution_registry::AgentSpawnPreflightRuntime {
            workspace_registry: Some(workspace_store.clone()),
            ..Default::default()
        },
    );
    let target = execution_root.join("probe.txt");
    std::fs::write(&target, "probe").expect("write probe");
    let external_calls = Arc::new(AtomicUsize::new(0));
    let calls = external_calls.clone();
    let execute_remove = Arc::new(move |arguments: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        let arguments: serde_json::Value = serde_json::from_str(arguments).unwrap();
        std::fs::remove_file(arguments["path"].as_str().unwrap()).unwrap();
        (
            serde_json::json!({"tool": tool_name, "status":"succeeded"}).to_string(),
            ExecutionResultStatus::Succeeded,
        )
    });
    let mcp_remove = execute_remove.clone();
    tool_registry = tool_registry
        .with_external_tool_catalog_provider(Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "approval".into(),
                    server_name: "Approval".into(),
                    model_tool_name: "mcp__approval__remove".into(),
                    tool_name: "remove".into(),
                    description: "Remove file".into(),
                    read_only: false,
                    input_schema: serde_json::json!({"type":"object"}),
                }],
                ..Default::default()
            }
        }))
        .with_external_mcp_tool_executor(Arc::new(move |_, _, arguments| mcp_remove(arguments)));
    let mut task = coordinator_task(test_task("task-file-remove", "task-file-remove", None));
    if child {
        let mut parent = test_task("approval-parent", "approval-parent", None);
        parent.executor_binding = Some(TaskExecutorBinding::for_role("coordinator"));
        parent
            .executor_binding
            .as_mut()
            .unwrap()
            .project_directories = directory_binding.clone();
        task.root_task_id = parent.task_id.clone();
        task.parent_task_id = Some(parent.task_id.clone());
        task.executor_binding = Some(TaskExecutorBinding::for_role("executor"));
        task_store.insert_task(parent).unwrap();
    }
    if let Some(binding) = directory_binding {
        task.executor_binding
            .get_or_insert_with(|| TaskExecutorBinding::for_role("coordinator"))
            .project_directories = Some(binding);
    }
    if matches!(failure_stage, Some("read_only" | "full_access")) {
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = if failure_stage == Some("read_only") {
            AccessProfile::ReadOnly
        } else {
            AccessProfile::FullAccess
        };
        task.policy_snapshot = Some(policy);
    }
    task_store.insert_task(task.clone()).expect("任务应插入");
    let session_id = SessionId::new("session-file-remove");
    session_store
        .create_session(session_id.clone(), "task approval test")
        .expect("approval test session should be creatable");
    let turn_id = "turn-file-remove";
    let attempt = match conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::Start(crate::TurnAdmission {
                turn_id: turn_id.to_string(),
                request_id: "request-file-remove".to_string(),
                request_fingerprint: "fingerprint-file-remove".to_string(),
                profile: crate::ExecutionProfile::Task,
            }),
        )
        .expect("approval test Turn should start")
    {
        crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
            attempt,
        )) => attempt,
        other => panic!("unexpected approval test admission: {other:?}"),
    };
    CanonicalTurnEventSink::for_store(&session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            None,
            magi_session_store::TimelineEntryInput::new(
                "timeline-file-remove",
                magi_session_store::TimelineEntryKind::UserMessage,
                "remove probe",
                UtcMillis::now(),
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: turn_id.to_string(),
                turn_seq: 1,
                accepted_at: UtcMillis::now(),
                status: "accepted".to_string(),
                user_message: Some("remove probe".to_string()),
                items: Vec::new(),
                completed_at: None,
            },
        )
        .expect("approval test Turn should persist through sink");
    conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: crate::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("approval test Turn should prepare");
    conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt,
                status: crate::CoordinatorTurnStatus::Running,
            },
        )
        .expect("approval test Turn should run");
    CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id), "running")
        .expect("approval test Turn running status should persist");
    let workspace_id = Some(WorkspaceId::new("workspace-file-remove"));
    let tool_call = ChatToolCall {
        id: "call-file-remove".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: tool_name.to_string(),
            arguments: serde_json::json!({
                "path": target.to_string_lossy()
            })
            .to_string(),
        },
    };

    persist_test_approval_tool(&session_store, &session_id, &task, &tool_call);
    let interrupted = std::sync::Mutex::new(None);
    let execution_active = std::sync::atomic::AtomicBool::new(true);
    let workspace_resources = task
        .executor_binding
        .as_ref()
        .and_then(|executor| executor.project_directories.as_ref())
        .map(
            |binding| crate::workspace_execution::WorkspaceExecutionResources {
                personal_root: None,
                directories: Some(
                    magi_core::ProjectDirectoryExecutionMap::from_binding(binding, |_| {
                        Ok(std::fs::canonicalize(&execution_root).unwrap())
                    })
                    .unwrap(),
                ),
                snapshots: Vec::new(),
            },
        );
    let result = thread::scope(|scope| {
        let resolver = scope.spawn(|| {
            if matches!(
                failure_stage,
                Some("awaiting_approval" | "recover_approval" | "read_only" | "full_access")
            ) {
                return;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if let Some(pending) = conversation_registry
                    .tool_approvals()
                    .pending_for_session(&session_id)
                    .into_iter()
                    .next()
                {
                    assert!(target.exists(), "授权前不能执行删除");
                    assert_eq!(pending.agent.is_some(), child);
                    conversation_registry
                        .tool_approvals()
                        .resolve(
                            &session_id,
                            &pending.approval_id,
                            if failure_stage == Some("deny") {
                                crate::ToolApprovalDecision::Deny
                            } else {
                                crate::ToolApprovalDecision::AllowOnce
                            },
                        )
                        .expect("task approval should resolve");
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "task tool approval should become pending"
                );
                std::thread::yield_now();
            }
        });
        let progress = |update: ToolExecutionProgress| {
            let payload: serde_json::Value = serde_json::from_str(&update.payload).unwrap();
            if failure_stage == Some("recover_approval") && payload["status"] == "awaiting_approval"
            {
                *interrupted.lock().unwrap() = Some((
                    session_store.durable_state(),
                    session_store.execution_sidecar_store_state(),
                ));
                return Err("simulated process exit after durable approval".into());
            }
            if failure_stage == Some("revoke_skill") && payload["status"] == "running" {
                skills.register(definition(true));
            }
            if failure_stage == Some("revoke_execution") && payload["status"] == "running" {
                execution_active.store(false, Ordering::SeqCst);
            }
            if failure_stage == Some("revoke_directory") && payload["status"] == "running" {
                workspace_store.deregister(&directory_workspace).unwrap();
            }
            #[cfg(unix)]
            if failure_stage == Some("replace_copy") && payload["status"] == "running" {
                std::fs::rename(&execution_root, dir.path().join("retained-copy")).unwrap();
                std::os::unix::fs::symlink(&source_root, &execution_root).unwrap();
            }
            if failure_stage.is_some() && payload["status"].as_str() == failure_stage {
                Err("injected durable write failure".to_string())
            } else {
                Ok(())
            }
        };
        let result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            Some(&skill_runtime),
            None,
            Some("approval-skill"),
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            None,
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&dir.path().to_path_buf()),
            None,
            None,
            &[tool_call],
            &mut ToolExecutionLedger::default(),
            Some(&progress),
            workspace_resources.clone(),
            None,
            &|| execution_active.load(Ordering::SeqCst),
        );
        resolver.join().expect("task approval resolver should join");
        result
    });

    assert!(
        conversation_registry
            .tool_approvals()
            .pending_for_session(&session_id)
            .is_empty()
    );
    if matches!(failure_stage, Some("read_only" | "deny")) {
        assert_eq!(
            result[0].1,
            ExecutionResultStatus::Rejected,
            "{}",
            result[0].0
        );
        assert!(target.exists(), "只读或明确拒绝必须保留文件");
        assert_eq!(external_calls.load(Ordering::SeqCst), 0);
        return;
    }
    if failure_stage == Some("revoke_execution") {
        assert_eq!(result[0].1, ExecutionResultStatus::Cancelled);
        assert!(target.exists(), "批准后执行资格失效必须阻止真实删除");
        return;
    }
    if matches!(failure_stage, Some("revoke_directory" | "replace_copy")) {
        assert_eq!(
            result[0].1,
            ExecutionResultStatus::Rejected,
            "{}",
            result[0].0
        );
        let value: serde_json::Value = serde_json::from_str(&result[0].0).unwrap();
        assert_eq!(value["error_code"], "project_directory_invalid");
        assert_eq!(value["execution_started"], false);
        assert!(target.exists(), "批准后目录授权撤销不得执行真实删除");
        assert_eq!(external_calls.load(Ordering::SeqCst), 0);
        if failure_stage == Some("replace_copy") {
            assert_eq!(
                std::fs::read_to_string(source_root.join("probe.txt")).unwrap(),
                "source content"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("retained-copy/probe.txt")).unwrap(),
                "probe"
            );
        }
        return;
    }
    if failure_stage == Some("revoke_skill") {
        assert_eq!(result[0].1, ExecutionResultStatus::Rejected);
        assert_eq!(external_calls.load(Ordering::SeqCst), 0);
        assert!(
            target.exists(),
            "等待期间撤销 Skill 必须阻止获批后的真实删除"
        );
        return;
    }
    if failure_stage.is_some() && failure_stage != Some("full_access") {
        assert_eq!(result[0].1, ExecutionResultStatus::Failed);
        assert!(target.exists(), "持久化失败时授权不能触发真实删除");
        let payload: serde_json::Value = serde_json::from_str(&result[0].0).unwrap();
        assert_eq!(payload["error_code"], "tool_progress_persist_failed");
        assert_eq!(payload["execution_started"], false);
        if failure_stage == Some("recover_approval") {
            let (durable, sidecars) = interrupted.into_inner().unwrap().unwrap();
            let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
            let now = UtcMillis::now();
            restored
                .seed_execution_chain_for_test(
                    session_id.clone(),
                    magi_session_store::ActiveExecutionChain {
                        session_id: session_id.clone(),
                        mission_id: task.mission_id.clone(),
                        root_task_id: task.task_id.clone(),
                        execution_chain_ref: "approval-recovery-chain".into(),
                        workspace_id: workspace_id.clone(),
                        branches: vec![],
                        dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                            accepted_at: now,
                            entry_id: "approval-recovery-entry".into(),
                            trimmed_text: Some("remove".into()),
                            skill_name: None,
                        },
                        current_turn_id: (restored
                            .runtime_sidecar(&session_id)
                            .unwrap()
                            .current_turn)
                            .map(|turn| turn.turn_id),
                    },
                )
                .unwrap();
            restored
                .interrupt_current_turn_by_daemon_restart(&session_id)
                .unwrap();
            let pending = restored
                .pending_tool_approvals(&session_id)
                .unwrap()
                .remove(0);
            restored
                .resolve_tool_approvals(
                    &session_id,
                    std::slice::from_ref(&pending.approval_id),
                    crate::ToolApprovalDecision::AllowOnce,
                )
                .unwrap();
            let call = ChatToolCall {
                id: "resumed-file-remove-call".into(),
                kind: "function".into(),
                function: ChatToolFunction {
                    name: "file_remove".into(),
                    arguments: serde_json::json!({"path":target.to_string_lossy()}).to_string(),
                },
            };
            restored
                .accept_conversation_turn_with_timeline_entry(
                    session_id.clone(),
                    workspace_id.clone(),
                    magi_session_store::TimelineEntryInput::new(
                        "resume-file-remove",
                        magi_session_store::TimelineEntryKind::UserMessage,
                        "继续",
                        now,
                    ),
                    magi_session_store::ActiveExecutionTurn {
                        turn_id: "resumed-file-remove-turn".into(),
                        turn_seq: now.0,
                        accepted_at: now,
                        status: "running".into(),
                        completed_at: None,
                        user_message: Some("继续".into()),
                        items: vec![],
                    },
                )
                .unwrap();
            persist_test_approval_tool(&restored, &session_id, &task, &call);
            let registry = ConversationRegistry::new();
            let progress = |update: ToolExecutionProgress| {
                if update.payload.contains("awaiting_approval") {
                    return Err("recovery unexpectedly asked a second approval".into());
                }
                Ok(())
            };
            let (payload, status) = execute_task_tool_call(
                &event_bus,
                Some(&tool_registry),
                &agent_role_registry,
                Some(&skill_runtime),
                None,
                Some("approval-skill"),
                &task_store,
                &restored,
                &execution_registry,
                &registry,
                None,
                &plan_store,
                None,
                &task,
                &session_id,
                &workspace_id,
                Some(&dir.path().to_path_buf()),
                None,
                None,
                &call,
                Some(&progress),
                &|| true,
                None,
            );
            assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
            assert!(!target.exists());
            let thread = magi_core::ThreadId::new("thread-approval-test");
            let before = ToolExecutionLedger::from_canonical_task(
                &restored,
                &task_store,
                &session_id,
                &task,
                &thread,
                Some(&tool_registry),
            )
            .unwrap();
            assert!(matches!(
                before
                    .ledger
                    .plan(std::slice::from_ref(&call), Some(&tool_registry))[0],
                crate::tool_execution_ledger::ToolCallExecutionDecision::RecoveryBlocked { .. }
            ));
            let mut item = restored
                .runtime_sidecar(&session_id)
                .unwrap()
                .current_turn
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.tool_call_id.as_deref() == Some(call.id.as_str()))
                .unwrap();
            item.kind = "tool_call_result".into();
            item.status = "completed".into();
            item.tool_status = Some("succeeded".into());
            item.tool_result = Some(payload.clone());
            restored
                .upsert_current_turn_item_for_turn(
                    &session_id,
                    Some("resumed-file-remove-turn"),
                    item,
                )
                .unwrap();
            let after = ToolExecutionLedger::from_canonical_task(
                &restored,
                &task_store,
                &session_id,
                &task,
                &thread,
                Some(&tool_registry),
            )
            .unwrap();
            assert!(matches!(
                after
                    .ledger
                    .plan(std::slice::from_ref(&call), Some(&tool_registry))[0],
                crate::tool_execution_ledger::ToolCallExecutionDecision::Reuse { .. }
            ));
            assert!(
                registry
                    .tool_approvals()
                    .pending_for_session(&session_id)
                    .is_empty()
            );
            let original = restored
                .canonical_turn_for_session_turn_id(&session_id, turn_id)
                .unwrap();
            assert!(original.items.iter().any(|item| {
                item.tool_approval()
                    .unwrap()
                    .is_some_and(|approval| approval.execution.is_some())
            }));
        }
        return;
    }
    assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
    let payload: serde_json::Value =
        serde_json::from_str(&result[0].0).expect("tool payload should be json");
    assert_eq!(payload["tool"].as_str(), Some(tool_name));
    assert_eq!(payload["status"].as_str(), Some("succeeded"));
    assert!(!target.exists(), "授权后必须恢复并完成原始删除调用");
    assert_eq!(
        external_calls.load(Ordering::SeqCst),
        usize::from(tool_name != "file_remove")
    );
}

#[test]
fn task_tool_approval_channel_disconnect_remains_runtime_failure() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let session_id = SessionId::new("session-task-approval-disconnect");
    session_store
        .create_session(session_id.clone(), "task approval disconnect")
        .expect("session should be creatable");
    let turn_id = "turn-task-approval-disconnect";
    let conversation_registry = ConversationRegistry::new();
    let attempt = match conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::Start(crate::TurnAdmission {
                turn_id: turn_id.to_string(),
                request_id: "request-task-approval-disconnect".to_string(),
                request_fingerprint: "fingerprint-task-approval-disconnect".to_string(),
                profile: crate::ExecutionProfile::Task,
            }),
        )
        .expect("approval disconnect Turn should start")
    {
        crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
            attempt,
        )) => attempt,
        other => panic!("unexpected approval disconnect admission: {other:?}"),
    };
    CanonicalTurnEventSink::for_store(&session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            None,
            magi_session_store::TimelineEntryInput::new(
                "timeline-task-approval-disconnect",
                magi_session_store::TimelineEntryKind::UserMessage,
                "等待审批通道断开",
                UtcMillis::now(),
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: turn_id.to_string(),
                turn_seq: 1,
                accepted_at: UtcMillis::now(),
                status: "accepted".to_string(),
                user_message: Some("等待审批通道断开".to_string()),
                items: Vec::new(),
                completed_at: None,
            },
        )
        .expect("approval disconnect Turn should persist through sink");
    conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: crate::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("approval disconnect Turn should prepare");
    conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt,
                status: crate::CoordinatorTurnStatus::Running,
            },
        )
        .expect("approval disconnect Turn should run");
    CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id), "running")
        .expect("approval disconnect Turn running status should persist");
    // 由子代理发起审批：父任务应收到 agent_awaiting_approval 信号。
    let parent_task_id = TaskId::new("task-approval-parent");
    let mut task = test_task(
        "task-approval-disconnect",
        "task-approval-parent",
        Some(parent_task_id.clone()),
    );
    task.executor_binding = Some(TaskExecutorBinding::for_role("executor"));
    task.title = "构建修复代理".to_string();
    task_store
        .insert_task(task.clone())
        .expect("task should be inserted");
    conversation_registry.open_task_signal_channel(&session_id, &parent_task_id);
    let registry = conversation_registry.tool_approvals();
    let workspace_id = None;
    let tool_call = ChatToolCall {
        id: "call-task-approval-disconnect".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::ShellExec.as_str().to_string(),
            arguments: r#"{"command":"printf disconnected"}"#.to_string(),
        },
    };
    persist_test_approval_tool(&session_store, &session_id, &task, &tool_call);
    let decision = ToolPreflightDecision {
        payload: serde_json::json!({ "error": "需要用户审批" }).to_string(),
        status: ExecutionResultStatus::NeedsApproval,
    };

    let (payload, status) = thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            await_task_tool_approval(
                &event_bus,
                &conversation_registry,
                &task_store,
                &session_store,
                &task,
                &session_id,
                &workspace_id,
                None,
                None,
                &tool_call,
                &decision,
                None,
                &|| true,
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(pending) = registry.pending_for_session(&session_id).into_iter().next() {
                assert_eq!(
                    pending.agent,
                    Some(crate::ApprovalRequestAgent {
                        role: "executor".to_string(),
                        title: "构建修复代理".to_string(),
                    }),
                    "审批请求应带上发起代理的身份"
                );
                registry.cancel(&pending.approval_id);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "approval request should become pending"
            );
            std::thread::yield_now();
        }
        waiter.join().expect("approval waiter should join")
    })
    .expect_err("channel disconnect must return a runtime failure");

    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("failure payload");
    assert_eq!(status, ExecutionResultStatus::Failed);
    assert_eq!(parsed["error_code"], "tool_approval_runtime_failed");
    assert!(
        parsed["approval_id"]
            .as_str()
            .unwrap()
            .starts_with("tool-approval-")
    );
    let signals = conversation_registry.drain_task_signals(&session_id, &parent_task_id);
    assert!(
        signals.iter().any(|signal| {
            signal.payload["type"] == "agent_awaiting_approval"
                && signal.payload["child_task_id"] == "task-approval-disconnect"
        }),
        "父任务应被通知子代理正在等待审批：{signals:?}"
    );
}

#[test]
fn task_shell_tool_forwards_running_output_progress() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_default_builtins();
    let dir = tempdir().expect("temp dir");
    let mut task = test_task("task-shell-progress", "task-shell-progress", None);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::FullAccess;
    let session_id = SessionId::new("session-shell-progress");
    let workspace_id = Some(WorkspaceId::new("workspace-shell-progress"));
    let tool_call = ChatToolCall {
        id: "call-shell-progress".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::ShellExec.as_str().to_string(),
            arguments: serde_json::json!({
                "command": "printf first; sleep 1; printf second",
                "access_mode": "read_only",
                "timeout_ms": 5_000,
            })
            .to_string(),
        },
    };
    let progress = Arc::new(std::sync::Mutex::new(Vec::<ToolExecutionProgress>::new()));
    let captured = Arc::clone(&progress);
    let on_progress = move |update| {
        captured.lock().expect("progress lock").push(update);
        Ok(())
    };

    let result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        Some(&dir.path().to_path_buf()),
        None,
        None,
        &[tool_call],
        &mut ToolExecutionLedger::default(),
        Some(&on_progress),
        None,
        None,
        &|| true,
    );

    assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
    let progress = progress.lock().expect("progress lock");
    assert!(progress.iter().any(|update| {
        let payload = serde_json::from_str::<serde_json::Value>(&update.payload)
            .expect("progress payload should be json");
        update.tool_call_id.as_str() == "call-shell-progress"
            && payload["status"] == "running"
            && payload["stdout"] == "first"
    }));
}

#[test]
fn execution_steps_update_shares_a_batch_with_real_tools_in_original_order() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_default_builtins();
    let dir = tempdir().expect("temp dir");
    let mut task = test_task("task-steps-batch", "task-steps-batch", None);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::FullAccess;
    let session_id = SessionId::new("session-steps-batch");
    let workspace_id = Some(WorkspaceId::new("workspace-steps-batch"));
    let call = |id: &str, name: &str, arguments: serde_json::Value| ChatToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    };
    // 模型把“进入下一步”放在实际工具之前或之后都应当可用，结果顺序与调用顺序一致。
    let calls = [
        call(
            "call-shell",
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({"command": "printf ok", "access_mode": "read_only", "timeout_ms": 5_000}),
        ),
        call(
            "call-steps",
            "update_execution_steps",
            serde_json::json!({"steps": [
                {"stepId": "a", "title": "第一步", "status": "running"},
                {"stepId": "b", "title": "第二步", "status": "pending"}
            ]}),
        ),
    ];
    let result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        Some(&dir.path().to_path_buf()),
        None,
        None,
        &calls,
        &mut ToolExecutionLedger::default(),
        None,
        None,
        None,
        &|| true,
    );
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[0].1,
        ExecutionResultStatus::Succeeded,
        "{:?}",
        result[0]
    );
    // 测试里没有可写入的 Turn，步骤更新本身失败；关键是它没有拖垮同批次的实际工具，
    // 也不再因“混合批次”被整体拒绝。
    assert!(
        !result[1].0.contains("execution_steps_separate_batch"),
        "{:?}",
        result[1]
    );
    assert!(result[1].0.contains("update_execution_steps"));
}

#[test]
fn task_interrupt_during_serial_tool_skips_remaining_calls_in_batch() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = Arc::new(TaskStore::new());
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let task = test_task("task-interrupt-batch", "task-interrupt-batch", None);
    task_store.insert_task(task.clone()).expect("任务应插入");
    let side_effects = Arc::new(AtomicUsize::new(0));
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_builtin(Arc::new(InterruptingBuiltinTool {
        name: "interrupted_shell_probe",
        task_store: Arc::clone(&task_store),
        task_id: task.task_id.clone(),
    }));
    for name in ["write_probe", "remove_probe"] {
        tool_registry.register_builtin(Arc::new(CountingBuiltinTool {
            name,
            executions: Arc::clone(&side_effects),
        }));
    }
    let tool_calls = ["interrupted_shell_probe", "write_probe", "remove_probe"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| ChatToolCall {
            id: format!("call-interrupt-{index}"),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        })
        .collect::<Vec<_>>();
    let mut ledger = ToolExecutionLedger::default();

    let results = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &SessionId::new("session-interrupt-batch"),
        &Some(WorkspaceId::new("workspace-interrupt-batch")),
        None,
        None,
        None,
        &tool_calls,
        &mut ledger,
        None,
        None,
        None,
        &|| true,
    );

    assert_eq!(results.len(), 3);
    assert_eq!(
        side_effects.load(Ordering::SeqCst),
        0,
        "中断后同批剩余的写操作不得执行"
    );
    for (result, status) in &results[1..] {
        assert_eq!(*status, ExecutionResultStatus::Cancelled);
        let payload: serde_json::Value =
            serde_json::from_str(result).expect("result should be json");
        assert_eq!(payload["status"], "interrupted");
        assert_eq!(payload["execution"], "not_started");
    }
    let replanned = ledger.plan(&tool_calls[1..], Some(&tool_registry));
    assert!(
        replanned.iter().all(|decision| matches!(
            decision,
            crate::tool_execution_ledger::ToolCallExecutionDecision::Execute { .. }
        )),
        "未执行的调用不能被账本记为已执行，恢复后应可正常重新执行"
    );
}

#[test]
fn task_serial_tool_panic_returns_terminal_public_failure() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_builtin(Arc::new(PanicBuiltinTool {
        name: "unstable_tool",
    }));

    let task = test_task("task-panic-tool", "task-panic-tool", None);
    let session_id = SessionId::new("session-panic-tool");
    let workspace_id = Some(WorkspaceId::new("workspace-panic-tool"));
    let tool_call = ChatToolCall {
        id: "call-panic-tool".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: "unstable_tool".to_string(),
            arguments: "{}".to_string(),
        },
    };

    let result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &[tool_call],
        &mut ToolExecutionLedger::default(),
        None,
        None,
        None,
        &|| true,
    );

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1, ExecutionResultStatus::Failed);
    let payload: serde_json::Value =
        serde_json::from_str(&result[0].0).expect("panic result should be json");
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "tool_execution_failed");
    assert_eq!(payload["error"], "工具执行失败，请稍后重试");
    assert!(!result[0].0.contains("panic"));
    assert!(!result[0].0.contains("线程"));
}

#[tokio::test(flavor = "multi_thread")]
async fn task_tool_batch_records_snapshot_worker_attribution() {
    let dir = tempdir().expect("temp dir");
    let workspace_root = dir.path().to_path_buf();
    let snapshot = magi_snapshot::SnapshotManager::new()
        .start_session("session-task-snapshot".to_string(), workspace_root.clone())
        .await
        .expect("snapshot session should start");

    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_default_builtins();

    let task = test_task("task-snapshot-agent", "task-snapshot-agent", None);
    let session_id = SessionId::new("session-task-snapshot");
    let workspace_id = Some(WorkspaceId::new("workspace-task-snapshot"));
    let worker_id = magi_core::WorkerId::new("worker-agent-edit");
    let tool_call = ChatToolCall {
        id: "call-file-write-agent".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::FileWrite.as_str().to_string(),
            arguments: serde_json::json!({
                "path": "agent.txt",
                "content": "hello from agent"
            })
            .to_string(),
        },
    };

    let result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        Some(&workspace_root),
        Some(&worker_id),
        None,
        &[tool_call],
        &mut ToolExecutionLedger::default(),
        None,
        Some(
            crate::workspace_execution::WorkspaceExecutionResources::from_snapshot(
                snapshot.clone(),
            ),
        ),
        Some(task.mission_id.to_string()),
        &|| true,
    );

    assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
    let pending = snapshot.pending_changes().expect("pending changes");
    let change = pending
        .iter()
        .find(|change| change.path == "agent.txt")
        .expect("agent.txt should be tracked");
    assert_eq!(change.source, magi_snapshot::SourceKind::Tool);
    assert_eq!(
        change.tool_call_id.as_deref(),
        Some("call-file-write-agent")
    );
    assert_eq!(change.worker_id.as_deref(), Some("worker-agent-edit"));
    assert_eq!(
        change.execution_group_id.as_deref(),
        Some("mission-mailbox")
    );

    let mainline_tool_call = ChatToolCall {
        id: "call-file-write-mainline".to_string(),
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: BuiltinToolName::FileWrite.as_str().to_string(),
            arguments: serde_json::json!({
                "path": "mainline.txt",
                "content": "hello from mainline"
            })
            .to_string(),
        },
    };
    let mainline_result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        Some(&workspace_root),
        None,
        None,
        &[mainline_tool_call],
        &mut ToolExecutionLedger::default(),
        None,
        Some(
            crate::workspace_execution::WorkspaceExecutionResources::from_snapshot(
                snapshot.clone(),
            ),
        ),
        Some(task.mission_id.to_string()),
        &|| true,
    );

    assert_eq!(mainline_result[0].1, ExecutionResultStatus::Succeeded);
    let pending = snapshot.pending_changes().expect("pending changes");
    let mainline_change = pending
        .iter()
        .find(|change| change.path == "mainline.txt")
        .expect("mainline.txt should be tracked");
    assert_eq!(mainline_change.source, magi_snapshot::SourceKind::Tool);
    assert_eq!(
        mainline_change.tool_call_id.as_deref(),
        Some("call-file-write-mainline")
    );
    assert_eq!(mainline_change.worker_id, None);
    assert_eq!(
        mainline_change.execution_group_id.as_deref(),
        Some("mission-mailbox")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_tool_batch_uses_each_copy_for_permissions_and_same_name_snapshot_attribution() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let sources = [root.join("source-a"), root.join("source-b")];
    let copies = [root.join("copy-a"), root.join("copy-b")];
    for path in sources.iter().chain(copies.iter()) {
        std::fs::create_dir(path).unwrap();
        std::fs::write(path.join("same.txt"), "baseline").unwrap();
    }
    let workspaces = Arc::new(magi_workspace::WorkspaceStore::default());
    let workspace = WorkspaceId::new("multi-copy-tools");
    let project = workspaces
        .register_native_path(workspace.clone(), sources[0].clone())
        .unwrap();
    let mut second = project.primary_source_folder().clone();
    let host = magi_core::HostPath::from_path(sources[1].clone());
    second.source_folder_id = "second".into();
    second.path = magi_core::AbsolutePath::new(host.display_string());
    second.path_ref = host.to_path_ref().as_str().into();
    let mut folders = project.source_folders;
    folders.push(second);
    workspaces
        .edit_project(
            &workspace,
            project.revision,
            magi_workspace::WorkspaceProjectEdit {
                name: None,
                source_folders: folders,
                primary_source_folder_id: project.primary_source_folder_id,
            },
            |_| Ok(()),
        )
        .unwrap();
    let binding = workspaces
        .capture_directory_binding(&workspace, None, None)
        .unwrap();
    let map = magi_core::ProjectDirectoryExecutionMap::from_binding(&binding, |grant| {
        Ok(if grant.source_folder_id == "second" {
            copies[1].clone()
        } else {
            copies[0].clone()
        })
    })
    .unwrap();
    let session = SessionId::new("multi-copy-tools");
    let manager = magi_snapshot::SnapshotManager::new();
    let snapshots = vec![
        manager
            .start_session(session.to_string(), copies[0].clone())
            .await
            .unwrap(),
        manager
            .start_session(session.to_string(), copies[1].clone())
            .await
            .unwrap(),
    ];
    let resources = crate::workspace_execution::WorkspaceExecutionResources {
        personal_root: None,
        directories: Some(map),
        snapshots: snapshots.clone(),
    };
    let mut task = coordinator_task(test_task("multi-copy-task", "multi-copy-task", None));
    task.executor_binding.as_mut().unwrap().project_directories = Some(binding);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    task.policy_snapshot.as_mut().unwrap().denied_paths = vec!["protected.txt".into()];
    let tasks = TaskStore::new();
    tasks.insert_task(task.clone()).unwrap();
    let sessions = SessionStore::new();
    let execution = TaskExecutionRegistry::default().with_agent_spawn_preflight_runtime(
        crate::task_execution_registry::AgentSpawnPreflightRuntime {
            workspace_registry: Some(workspaces),
            ..Default::default()
        },
    );
    let conversations = ConversationRegistry::new();
    let plans = crate::test_plan_store("multi-copy-tools");
    let roles = magi_agent_role::AgentRoleRegistry::load_default();
    let bus = InMemoryEventBus::new(32);
    let worker = magi_core::WorkerId::new("multi-copy-worker");
    let mut registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(16)),
    );
    registry.register_default_builtins();
    let invoke = |id: &str, arguments: serde_json::Value| {
        execute_task_tool_call_batch(
            &bus,
            Some(&registry),
            &roles,
            None,
            None,
            None,
            &tasks,
            &sessions,
            &execution,
            &conversations,
            None,
            &plans,
            None,
            &task,
            &session,
            &Some(workspace.clone()),
            Some(&copies[0]),
            Some(&worker),
            None,
            &[ChatToolCall {
                id: id.into(),
                kind: "function".into(),
                function: ChatToolFunction {
                    name: "file_write".into(),
                    arguments: arguments.to_string(),
                },
            }],
            &mut ToolExecutionLedger::default(),
            None,
            Some(resources.clone()),
            None,
            &|| true,
        )
        .remove(0)
    };
    for (id, arguments) in [
        (
            "first",
            serde_json::json!({"path":"same.txt","content":"first copy"}),
        ),
        (
            "second",
            serde_json::json!({"source_folder_id":"second","path":"same.txt","content":"second copy"}),
        ),
        (
            "second-policy",
            serde_json::json!({"source_folder_id":"second","path":"protected.txt","content":"permitted second root"}),
        ),
    ] {
        let result = invoke(id, arguments);
        assert_eq!(result.1, ExecutionResultStatus::Succeeded, "{}", result.0);
    }
    for (id, arguments) in [
        (
            "first-denied",
            serde_json::json!({"path":"protected.txt","content":"blocked"}),
        ),
        (
            "source-denied",
            serde_json::json!({"path":sources[1].join("same.txt"),"content":"blocked"}),
        ),
        (
            "unknown-source",
            serde_json::json!({"source_folder_id":"third","path":"same.txt","content":"blocked"}),
        ),
    ] {
        let result = invoke(id, arguments);
        assert_eq!(result.1, ExecutionResultStatus::Rejected, "{}", result.0);
    }
    for source in &sources {
        assert_eq!(
            std::fs::read_to_string(source.join("same.txt")).unwrap(),
            "baseline"
        );
    }
    assert_eq!(
        std::fs::read_to_string(copies[0].join("same.txt")).unwrap(),
        "first copy"
    );
    assert_eq!(
        std::fs::read_to_string(copies[1].join("same.txt")).unwrap(),
        "second copy"
    );
    assert!(!copies[0].join("protected.txt").exists());
    for (index, snapshot) in snapshots.iter().enumerate() {
        let changes = snapshot.pending_changes().unwrap();
        let same = changes
            .iter()
            .find(|change| change.path == "same.txt")
            .unwrap();
        assert_eq!(same.source, magi_snapshot::SourceKind::Tool);
        assert_eq!(
            same.tool_call_id.as_deref(),
            Some(["first", "second"][index])
        );
        assert_eq!(same.worker_id.as_deref(), Some(worker.as_str()));
        assert_eq!(changes.len(), [1, 2][index]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn task_concurrent_tool_batch_keeps_snapshot_context_during_execution() {
    let dir = tempdir().expect("temp dir");
    let workspace_root = dir.path().to_path_buf();
    let snapshot = magi_snapshot::SnapshotManager::new()
        .start_session(
            "session-task-concurrent-snapshot".to_string(),
            workspace_root.clone(),
        )
        .await
        .expect("snapshot session should start");

    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let mut tool_registry = ToolRegistry::new(
        Arc::new(magi_governance::GovernanceService::default()),
        Arc::new(InMemoryEventBus::new(8)),
    );
    tool_registry.register_builtin(Arc::new(SnapshotReconcileProbeTool::new(
        BuiltinToolName::ShellExec.as_str(),
        snapshot.clone(),
    )));

    let mut task = test_task("task-concurrent-snapshot", "task-concurrent-snapshot", None);
    task.policy_snapshot = Some(default_agent_spawn_policy());
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::FullAccess;
    let session_id = SessionId::new("session-task-concurrent-snapshot");
    let workspace_id = Some(WorkspaceId::new("workspace-task-concurrent-snapshot"));
    let worker_id = magi_core::WorkerId::new("worker-concurrent-snapshot");
    let tool_calls = vec![
        ChatToolCall {
            id: "call-concurrent-snapshot-a".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::ShellExec.as_str().to_string(),
                arguments: serde_json::json!({
                    "command": "printf a",
                    "access_mode": "read_only",
                    "changed_paths": ["agent-a.txt"]
                })
                .to_string(),
            },
        },
        ChatToolCall {
            id: "call-concurrent-snapshot-b".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::ShellExec.as_str().to_string(),
                arguments: serde_json::json!({
                    "command": "printf b",
                    "access_mode": "read_only",
                    "changed_paths": ["agent-b.txt"]
                })
                .to_string(),
            },
        },
    ];

    let result = execute_task_tool_call_batch(
        &event_bus,
        Some(&tool_registry),
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &task,
        &session_id,
        &workspace_id,
        Some(&workspace_root),
        Some(&worker_id),
        None,
        &tool_calls,
        &mut ToolExecutionLedger::default(),
        None,
        Some(
            crate::workspace_execution::WorkspaceExecutionResources::from_snapshot(
                snapshot.clone(),
            ),
        ),
        Some(task.mission_id.to_string()),
        &|| true,
    );

    assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
    assert_eq!(result[1].1, ExecutionResultStatus::Succeeded);
    let pending = snapshot.pending_changes().expect("pending changes");
    for (path, call_id) in [
        ("agent-a.txt", "call-concurrent-snapshot-a"),
        ("agent-b.txt", "call-concurrent-snapshot-b"),
    ] {
        let change = pending
            .iter()
            .find(|change| change.path == path)
            .expect("concurrent tool change should be tracked");
        assert_eq!(change.source, magi_snapshot::SourceKind::Tool);
        assert_eq!(change.tool_call_id.as_deref(), Some(call_id));
        assert_eq!(
            change.worker_id.as_deref(),
            Some("worker-concurrent-snapshot")
        );
        assert_eq!(
            change.execution_group_id.as_deref(),
            Some("mission-mailbox")
        );
    }
}

#[test]
fn agent_wait_and_cancel_read_persisted_partial_executor_evidence_without_replay() {
    use magi_bridge_client::{ModelBridgeClient, ModelInvocationRequest, ModelResponse};
    use magi_session_store::{ActiveExecutionTurn, TimelineEntryInput, TimelineEntryKind};

    #[derive(Clone)]
    struct PartialExecutor {
        calls: Arc<AtomicUsize>,
        evidence: magi_core::ExecutorFailureEvidence,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        cancel: bool,
    }
    impl ModelBridgeClient for PartialExecutor {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            let mut caps = magi_bridge_client::text_executor_capabilities();
            caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
            caps
        }
        fn bind_execution_scope(
            &self,
            _: magi_bridge_client::ModelExecutionScope,
        ) -> Result<Option<Arc<dyn ModelBridgeClient>>, magi_bridge_client::BridgeClientError>
        {
            Ok(Some(Arc::new(self.clone())))
        }
        fn invoke(
            &self,
            _: ModelInvocationRequest,
        ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.stopped.store(self.cancel, Ordering::SeqCst);
            Err(magi_bridge_client::BridgeClientError::ExecutorFailed {
                source: Box::new(magi_bridge_client::BridgeClientError::CallFailed {
                    layer: magi_bridge_client::BridgeErrorLayer::Transport,
                    code: None,
                    message: "fixture interrupted after command started".into(),
                }),
                evidence: Box::new(self.evidence.clone()),
            })
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
        ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    for cancel in [false, true] {
        let tasks = TaskStore::new();
        let sessions = SessionStore::new();
        let session = SessionId::new(format!("partial-executor-{cancel}"));
        let parent = test_task("partial-parent", "partial-parent", None);
        let mut child = test_task(
            "partial-child",
            "partial-parent",
            Some(parent.task_id.clone()),
        );
        child.status = if cancel {
            TaskStatus::Killed
        } else {
            TaskStatus::Failed
        };
        child.output_refs = vec!["executor outcome unknown".into()];
        tasks.insert_task(parent.clone()).unwrap();
        tasks.insert_task(child.clone()).unwrap();
        sessions
            .create_session(session.clone(), "partial executor")
            .unwrap();
        sessions
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "partial-input",
                    TimelineEntryKind::UserMessage,
                    "inspect",
                    UtcMillis(1),
                ),
                ActiveExecutionTurn {
                    turn_id: "partial-turn".into(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    status: "running".into(),
                    completed_at: None,
                    user_message: Some("inspect".into()),
                    items: vec![],
                },
            )
            .unwrap();
        let executor = PartialExecutor {
                calls: Default::default(), stopped: Default::default(), cancel,
                evidence: serde_json::from_value(serde_json::json!({
                    "receipts":[{"executor":"fixture", "execution_id":"original-run", "commands":[
                        {"command":"inspect", "exit_code":null, "status":"outcome_unknown", "output":"partial observation"}
                    ]}],
                    "usage":{"input_tokens":3, "output_tokens":2}, "items":[], "resourcesSettled":true
                })).unwrap(),
            };
        let thread = magi_core::ThreadId::new("partial-thread");
        let request = || ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "fixture".into(),
            prompt: "inspect".into(),
            messages: None,
            tools: None,
            tool_choice: None,
        };
        let scope = || magi_bridge_client::ModelExecutionScope {
            working_dir: None,
            policy: default_agent_spawn_policy(),
            requires_native_tools: false,
            closed_skill_scope: false,
        };
        let client = crate::model_step::StepModelClient::new(
            &executor,
            &sessions,
            &session,
            Some("partial-turn"),
            Some(&child.task_id),
            &thread,
            "partial-step",
            0,
        )
        .with_execution_scope(scope());
        assert!(
            client
                .invoke_with_cancellation(request(), &|| executor.stopped.load(Ordering::SeqCst))
                .is_err()
        );
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);

        // 真正经过序列化与读取，避免测试只验证共享内存中的证据。
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&sessions.durable_state()).unwrap(),
        )
        .unwrap();
        let restored = SessionStore::from_persisted_parts(
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
            sessions.execution_sidecar_store_state(),
        )
        .unwrap();
        executor.stopped.store(false, Ordering::SeqCst);
        let replay = crate::model_step::StepModelClient::new(
            &executor,
            &restored,
            &session,
            Some("partial-turn"),
            Some(&child.task_id),
            &thread,
            "partial-step",
            0,
        )
        .with_execution_scope(scope());
        assert!(replay.invoke(request()).is_err());
        let before = restored
            .canonical_turn_for_session_turn_id(&session, "partial-turn")
            .unwrap();
        assert_eq!(
            before.model_steps[0].status,
            magi_session_store::ModelStepStatus::OutcomeUnknown
        );
        assert_eq!(before.model_steps[0].usage_tokens, Some(5));

        let registry = TaskExecutionRegistry::default();
        let (wait, status) = execute_agent_wait_with_runtime(
            AgentWaitRuntime {
                task_store: &tasks,
                session_store: &restored,
                admission: &registry.execution_admission(),
                conversation_registry: &ConversationRegistry::new(),
                session_id: &session,
                execution_allowed: &|| true,
            },
            &parent,
            BuiltinToolName::AgentWait,
            &serde_json::json!({"task_ids":[child.task_id]}),
        );
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let wait: serde_json::Value = serde_json::from_str(&wait).unwrap();
        let receipt = &wait["results"][0];
        assert_eq!(receipt["activity"]["external_commands"], 1);
        assert_eq!(receipt["activity"]["tool_calls"], 0);
        assert_eq!(
            receipt["executor_receipts"][0]["model_step_id"],
            "partial-step"
        );
        assert_eq!(
            receipt["executor_receipts"][0]["commands"][0]["status"],
            "outcome_unknown"
        );
        assert!(receipt["executor_receipts"][0]["commands"][0]["exit_code"].is_null());
        assert_eq!(receipt["executor_failures"][0]["resources_settled"], true);
        let (cancelled, status) = execute_agent_cancel(
            &tasks,
            &registry,
            &restored,
            &parent,
            &session,
            BuiltinToolName::AgentCancel,
            &serde_json::json!({"task_id":child.task_id, "reason":"核对原任务"}),
        );
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let cancelled: serde_json::Value = serde_json::from_str(&cancelled).unwrap();
        assert_eq!(cancelled["results"], wait["results"]);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(tasks.all_tasks().len(), 2);
        assert_eq!(
            restored
                .canonical_turn_for_session_turn_id(&session, "partial-turn")
                .unwrap(),
            before
        );
    }
}

#[test]
fn agent_wait_returns_completed_agent_final_text() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut child = test_task(
        "task-agent-wait-child",
        "task-agent-wait-root",
        Some(TaskId::new("task-agent-wait-root")),
    );
    child.status = TaskStatus::Completed;
    child.title = "目录探索".to_string();
    child.goal = "列出目录并汇报".to_string();
    child.executor_binding = Some(TaskExecutorBinding::for_role("explorer"));
    child.output_refs = vec![
        serde_json::json!({
            "blocks": [
                {
                    "type": "text",
                    "content": "已完成目录探索，发现 README.md。"
                }
            ]
        })
        .to_string(),
    ];
    task_store.insert_task(child).expect("子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-child"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait result should be json");
    assert_eq!(parsed["status"].as_str(), Some("completed"));
    assert_eq!(parsed["timed_out"].as_bool(), Some(false));
    assert_eq!(
        parsed["results"][0]["child_status"].as_str(),
        Some("completed")
    );
    assert_eq!(
        parsed["results"][0]["assignment"]["goal"].as_str(),
        Some("列出目录并汇报")
    );
    assert_eq!(
        parsed["results"][0]["result"]["final_text"].as_str(),
        Some("已完成目录探索，发现 README.md。")
    );
}

#[test]
fn agent_wait_rejects_missing_task_ids() {
    let task_store = TaskStore::new();
    let parent = test_task(
        "task-agent-wait-missing-ids",
        "task-agent-wait-missing-ids",
        None,
    );

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({}),
    );

    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait rejection should be json");
    assert_eq!(parsed["status"].as_str(), Some("rejected"));
    assert_eq!(parsed["error_code"].as_str(), Some("invalid_arguments"));
    assert_eq!(parsed["failure_stage"].as_str(), Some("input_validation"));
    assert!(parsed["child_task_id"].is_null());
}

#[test]
fn agent_wait_rejects_malformed_task_ids_instead_of_silently_filtering() {
    for arguments in [
        serde_json::json!({"task_ids": ["valid-child", 123]}),
        serde_json::json!({"task_ids": ["valid-child", null]}),
        serde_json::json!({"task_ids": ["valid-child", "valid-child"]}),
        serde_json::json!({"task_ids": ["valid-child"], "unexpected": true}),
        serde_json::json!({"task_ids": ["valid-child"], "timeout_ms": 999}),
    ] {
        let error = parse_agent_wait_request(&arguments)
            .expect_err("malformed agent_wait request must be rejected");
        assert!(!error.is_empty());
    }
}

#[test]
fn agent_wait_rejects_unknown_child_without_exposing_other_tasks() {
    let task_store = TaskStore::new();
    let parent = test_task(
        "task-agent-wait-missing-child-root",
        "task-agent-wait-missing-child-root",
        None,
    );
    let missing_child_id = TaskId::new("task-agent-wait-missing-child");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": [missing_child_id.as_str()],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait missing result should be json");
    assert_eq!(parsed["error_code"].as_str(), Some("scope_mismatch"));
    assert!(!payload.contains("foreign"));
}

#[test]
fn agent_wait_returns_timeout_with_pending_task_ids() {
    let task_store = TaskStore::new();
    let parent = test_task(
        "task-agent-wait-timeout-root",
        "task-agent-wait-timeout-root",
        None,
    );
    let mut child = test_task(
        "task-agent-wait-timeout-child",
        parent.task_id.as_str(),
        Some(parent.task_id.clone()),
    );
    child.status = TaskStatus::Pending;
    task_store
        .insert_task(child.clone())
        .expect("待等待子任务应插入");

    let started = std::time::Instant::now();
    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": [child.task_id.as_str()],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    assert!(started.elapsed() >= std::time::Duration::from_millis(900));
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait timeout should be json");
    assert_eq!(parsed["status"].as_str(), Some("timed_out"));
    assert_eq!(parsed["timed_out"].as_bool(), Some(true));
    assert_eq!(
        parsed["pending_task_ids"],
        serde_json::json!([child.task_id.as_str()])
    );
}

#[test]
fn agent_wait_reports_killed_child_as_failed_recovery_result() {
    let task_store = TaskStore::new();
    let parent = test_task(
        "task-agent-wait-killed-root",
        "task-agent-wait-killed-root",
        None,
    );
    let mut child = test_task(
        "task-agent-wait-killed-child",
        parent.task_id.as_str(),
        Some(parent.task_id.clone()),
    );
    child.status = TaskStatus::Killed;
    task_store
        .insert_task(child.clone())
        .expect("已终止子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({"task_ids": [child.task_id.as_str()]}),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait killed result should be json");
    assert_eq!(parsed["status"].as_str(), Some("completed"));
    assert_eq!(parsed["results"][0]["status"].as_str(), Some("failed"));
    assert_eq!(
        parsed["results"][0]["child_status"].as_str(),
        Some("killed")
    );
    assert_eq!(
        parsed["results"][0]["error_code"].as_str(),
        Some("agent_killed")
    );
    assert_eq!(
        parsed["results"][0]["failure_stage"].as_str(),
        Some("cancellation")
    );
    assert_eq!(
        parsed["results"][0]["instruction"],
        magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE,
    );
    assert!(
        parsed["instruction"]
            .as_str()
            .unwrap()
            .contains(magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE)
    );
}

fn completed_output_ref(final_text: &str) -> String {
    serde_json::json!({
        "blocks": [
            {"type": "text", "content": "我先看一下相关文件。"},
            {
                "type": "tool_call",
                "toolCall": {
                    "id": "call-shell",
                    "name": "shell_exec",
                    "arguments": {"command": "cargo test -p demo"},
                    "status": "success",
                    "result": serde_json::json!({"exit_code": 0}).to_string(),
                }
            },
            {"type": "text", "content": final_text},
        ]
    })
    .to_string()
}

#[test]
fn agent_wait_reads_final_answer_committed_when_child_finishes_during_wait() {
    let task_store = std::sync::Arc::new(TaskStore::new());
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let child = test_task(
        "task-agent-wait-late-child",
        "task-agent-wait-root",
        Some(parent.task_id.clone()),
    );
    task_store
        .insert_task(parent.clone())
        .expect("父任务应插入");
    task_store.insert_task(child.clone()).expect("子任务应插入");
    let finisher = {
        let task_store = std::sync::Arc::clone(&task_store);
        let child_id = child.task_id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            task_store
                .complete_task(
                    &child_id,
                    magi_core::TaskCompletionAttempt {
                        output_refs: vec![completed_output_ref("审查结论：实现正确。")],
                        final_response: Some("审查结论：实现正确。".to_string()),
                        evidence: Vec::new(),
                    },
                )
                .expect("子任务应完成");
        })
    };

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({"task_ids": [child.task_id.as_str()], "timeout_ms": 5000}),
    );
    finisher.join().expect("完成线程应结束");

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["status"].as_str(), Some("completed"));
    assert_eq!(
        parsed["results"][0]["result"]["final_text"].as_str(),
        Some("审查结论：实现正确。"),
        "等待期间完成的代理必须返回最终答复，而不是中间过程文字"
    );
    assert_eq!(
        parsed["results"][0]["activity"]["commands"][0]["command"].as_str(),
        Some("cargo test -p demo")
    );
    assert_eq!(parsed["model_budgeted"].as_bool(), Some(true));
    assert!(parsed["results"][0].get("summary").is_none());
}

#[test]
fn agent_wait_any_mode_returns_first_finished_child_and_keeps_others_pending() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-any", "task-agent-wait-any", None);
    let mut finished = test_task(
        "task-agent-wait-any-done",
        "task-agent-wait-any",
        Some(parent.task_id.clone()),
    );
    finished.status = TaskStatus::Completed;
    finished.output_refs = vec![completed_output_ref("第一个结论")];
    let running = test_task(
        "task-agent-wait-any-running",
        "task-agent-wait-any",
        Some(parent.task_id.clone()),
    );
    task_store.insert_task(finished.clone()).expect("应插入");
    task_store.insert_task(running.clone()).expect("应插入");

    let started = std::time::Instant::now();
    let (payload, _) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": [finished.task_id.as_str(), running.task_id.as_str()],
            "timeout_ms": 30000,
            "mode": "any",
        }),
    );

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["status"].as_str(), Some("completed"));
    assert_eq!(parsed["all_terminal"].as_bool(), Some(false));
    assert_eq!(parsed["completed_count"].as_u64(), Some(1));
    assert_eq!(
        parsed["pending_task_ids"],
        serde_json::json!([running.task_id.as_str()])
    );
    assert_eq!(parsed["results"].as_array().map(Vec::len), Some(1));
}

#[test]
fn agent_wait_timeout_still_returns_results_of_finished_children() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-partial", "task-agent-wait-partial", None);
    let mut finished = test_task(
        "task-agent-wait-partial-done",
        "task-agent-wait-partial",
        Some(parent.task_id.clone()),
    );
    finished.status = TaskStatus::Failed;
    finished.output_refs = vec!["测试失败：断言不匹配".to_string()];
    let running = test_task(
        "task-agent-wait-partial-running",
        "task-agent-wait-partial",
        Some(parent.task_id.clone()),
    );
    task_store.insert_task(finished.clone()).expect("应插入");
    task_store.insert_task(running.clone()).expect("应插入");

    let (payload, _) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": [finished.task_id.as_str(), running.task_id.as_str()],
            "timeout_ms": 1000,
        }),
    );

    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["status"].as_str(), Some("timed_out"));
    assert_eq!(parsed["failed_count"].as_u64(), Some(1));
    assert_eq!(
        parsed["results"][0]["child_task_id"].as_str(),
        Some(finished.task_id.as_str())
    );
    assert_eq!(
        parsed["results"][0]["error"].as_str(),
        Some("测试失败：断言不匹配")
    );
}

#[test]
fn agent_spawn_instruction_points_to_agent_wait_task_ids_contract() {
    assert!(
        AGENT_SPAWN_STARTED_INSTRUCTION.contains("agent_wait")
            && AGENT_SPAWN_STARTED_INSTRUCTION.contains("task_ids=[child_task_id]"),
        "agent_spawn 成功回执必须指向 agent_wait 的唯一 task_ids 参数契约"
    );
    assert!(
        !AGENT_SPAWN_STARTED_INSTRUCTION.contains("传入 child_task_id 收集"),
        "agent_spawn 回执不能把返回字段误写成 agent_wait 输入字段"
    );
}

#[test]
fn agent_spawn_uses_stable_hierarchical_task_names() {
    assert!(valid_agent_task_name("windows_validation"));
    assert!(!valid_agent_task_name("Windows Validation"));
    assert_eq!(
        child_canonical_task_name("/root", "windows_validation"),
        "/root/windows_validation"
    );
    assert_eq!(
        child_canonical_task_name("/root/windows_validation", "path_test"),
        "/root/windows_validation/path_test"
    );
}

#[test]
fn agent_wait_rejects_task_outside_current_spawn_scope() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut foreign_child = test_task(
        "task-agent-wait-foreign-child",
        "task-other-root",
        Some(TaskId::new("task-other-root")),
    );
    foreign_child.status = TaskStatus::Completed;
    foreign_child.output_refs = vec!["foreign result".to_string()];
    task_store
        .insert_task(foreign_child)
        .expect("跨作用域子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-foreign-child"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait rejection should be json");
    assert_eq!(parsed["status"].as_str(), Some("rejected"));
    assert_eq!(parsed["error_code"].as_str(), Some("scope_mismatch"));
    assert!(!payload.contains("foreign result"));
}

#[test]
fn agent_wait_rejects_same_parent_id_outside_execution_scope() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut foreign_child = test_task(
        "task-agent-wait-same-parent-foreign-scope",
        "task-other-root",
        Some(parent.task_id.clone()),
    );
    foreign_child.mission_id = MissionId::new("mission-other");
    foreign_child.status = TaskStatus::Completed;
    foreign_child.output_refs = vec!["foreign scoped result".to_string()];
    task_store
        .insert_task(foreign_child)
        .expect("跨作用域子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-same-parent-foreign-scope"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait rejection should be json");
    assert_eq!(parsed["error_code"].as_str(), Some("scope_mismatch"));
    assert!(!payload.contains("foreign scoped result"));
}

#[test]
fn agent_wait_rejects_direct_child_with_different_workspace_scope() {
    let task_store = TaskStore::new();
    let mut parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    parent.workspace_scope = Some("/workspace-a".to_string());
    parent.write_scope = Some("/workspace-a/src".to_string());
    let mut child = test_task(
        "task-agent-wait-workspace-mismatch",
        "task-agent-wait-root",
        Some(parent.task_id.clone()),
    );
    child.workspace_scope = Some("/workspace-b".to_string());
    child.write_scope = Some("/workspace-b/src".to_string());
    child.status = TaskStatus::Completed;
    child.output_refs = vec!["workspace mismatched result".to_string()];
    task_store.insert_task(child).expect("子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-workspace-mismatch"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait rejection should be json");
    assert_eq!(parsed["error_code"].as_str(), Some("scope_mismatch"));
    assert!(!payload.contains("workspace mismatched result"));
}

#[test]
fn agent_wait_classifies_model_invocation_failure_without_degrading() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut child = test_task(
        "task-agent-wait-unavailable",
        "task-agent-wait-root",
        Some(TaskId::new("task-agent-wait-root")),
    );
    child.status = TaskStatus::Failed;
    child.title = "配置审查代理".to_string();
    child.goal = "检查模型配置是否可用".to_string();
    child.executor_binding = Some(TaskExecutorBinding::for_role("reviewer"));
    child.output_refs = vec!["provider transport failed: connection refused".to_string()];
    task_store.insert_task(child).expect("子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-unavailable"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait result should be json");
    let result = &parsed["results"][0];
    assert_eq!(result["status"].as_str(), Some("failed"));
    assert_eq!(result["child_status"].as_str(), Some("failed"));
    assert_eq!(
        result["fallback_mode"].as_str(),
        Some("mainline_or_reassign")
    );
    assert_eq!(result["failure_stage"].as_str(), Some("model_invocation"));
    assert_eq!(
        result["error_code"].as_str(),
        Some("model_invocation_failed")
    );
    assert_eq!(result["error"].as_str(), Some("子代理模型调用失败。"));
    assert_eq!(
        result["result"]["final_text"].as_str(),
        Some("子代理模型调用失败。")
    );
    assert!(
        !result.to_string().contains("provider transport failed"),
        "agent_wait failed payload should not expose provider transport detail"
    );
}

#[test]
fn agent_wait_preserves_non_degradable_agent_failure() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut child = test_task(
        "task-agent-wait-real-failure",
        "task-agent-wait-root",
        Some(TaskId::new("task-agent-wait-root")),
    );
    child.status = TaskStatus::Failed;
    child.title = "冒烟测试代理".to_string();
    child.goal = "运行冒烟测试并报告失败原因".to_string();
    child.executor_binding = Some(TaskExecutorBinding::for_role("tester"));
    child.output_refs = vec!["测试失败：断言不匹配".to_string()];
    task_store.insert_task(child).expect("子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-real-failure"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait result should be json");
    let result = &parsed["results"][0];
    assert_eq!(result["status"].as_str(), Some("failed"));
    assert_eq!(result["child_status"].as_str(), Some("failed"));
    assert_eq!(result["failure_stage"].as_str(), Some("task_execution"));
    assert_eq!(result["error_code"].as_str(), Some("task_execution_failed"));
    assert_eq!(
        result["fallback_mode"].as_str(),
        Some("mainline_or_reassign")
    );
    assert_eq!(
        result["result"]["final_text"].as_str(),
        Some("测试失败：断言不匹配")
    );
}

#[test]
fn agent_wait_redacts_internal_failure_details_from_failed_agent_output() {
    let task_store = TaskStore::new();
    let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
    let mut child = test_task(
        "task-agent-wait-redacted-failure",
        "task-agent-wait-root",
        Some(TaskId::new("task-agent-wait-root")),
    );
    child.status = TaskStatus::Failed;
    child.title = "验证代理".to_string();
    child.goal = "运行验证并报告失败原因".to_string();
    child.executor_binding = Some(TaskExecutorBinding::for_role("tester"));
    child.output_refs = vec![
        "测试失败：断言不匹配".to_string(),
        "provider transport failed: connection refused".to_string(),
    ];
    task_store.insert_task(child).expect("子任务应插入");

    let (payload, status) = execute_agent_wait(
        &task_store,
        &parent,
        BuiltinToolName::AgentWait,
        &serde_json::json!({
            "task_ids": ["task-agent-wait-redacted-failure"],
            "timeout_ms": 1000,
        }),
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("agent_wait result should be json");
    let result = &parsed["results"][0];
    assert_eq!(result["status"].as_str(), Some("failed"));
    assert_eq!(result["error"].as_str(), Some("测试失败：断言不匹配"));
    assert_eq!(
        result["result"]["final_text"].as_str(),
        Some("测试失败：断言不匹配")
    );
    assert!(
        !result.to_string().contains("provider transport failed"),
        "agent_wait failed payload should not expose internal provider detail"
    );
}

#[test]
fn agent_spawn_assignment_message_enters_child_signal_fifo() {
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-assignment-mailbox");
    let parent = coordinator_task(test_task(
        "task-agent-assignment-parent",
        "task-agent-assignment-parent",
        None,
    ));
    let mut child = test_task(
        "task-agent-assignment-child",
        "task-agent-assignment-parent",
        Some(parent.task_id.clone()),
    );
    child.title = "目录探索".to_string();
    child.goal = "列出目录并汇报".to_string();
    child.dependency_ids = vec![TaskId::new("task-agent-assignment-dependency")];
    child.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "agent-context-assignment".to_string(),
            revision: 1,
            parent_task_id: parent.task_id.clone(),
            summary: "只读检查目录".to_string(),
            constraints: vec!["不得修改文件".to_string()],
            expected_output: "返回目录清单".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(42),
            updated_at: UtcMillis(42),
        }),
        accesses: Vec::new(),
    };

    registry.open_task_signal_channel(&session_id, &child.task_id);
    enqueue_agent_assignment_message(
        &registry,
        &session_id,
        &parent,
        &child,
        "explorer",
        UtcMillis(42),
    )
    .unwrap();

    let items = registry.drain_task_signals(&session_id, &child.task_id);

    assert_eq!(items.len(), 1);
    let signal = &items[0];
    assert_eq!(
        signal.author,
        MailboxAuthor::Parent("task-agent-assignment-parent".to_string())
    );
    assert_eq!(signal.kind, MailboxKind::Message);
    assert!(signal.trigger_turn);
    assert_eq!(signal.payload["type"].as_str(), Some("agent_assignment"));
    assert_eq!(signal.payload["goal"].as_str(), Some("列出目录并汇报"));
    assert_eq!(
        signal.payload["dependency_ids"],
        serde_json::json!(["task-agent-assignment-dependency"])
    );
    assert_eq!(
        signal.payload["context_package_id"],
        "agent-context-assignment"
    );
    assert_eq!(signal.payload["context_revision"], 1);
}

#[test]
fn previous_agent_context_uses_canonical_history_and_rejects_broken_lineage() {
    let tasks = TaskStore::new();
    tasks
        .insert_task(test_task("context-root", "context-root", None))
        .unwrap();
    let sessions = SessionStore::new();
    let session = SessionId::new("canonical-agent-context");
    sessions
        .create_session(session.clone(), "agent context")
        .unwrap();
    let (_, thread) = sessions.ensure_session_mission(&session, UtcMillis(1), || {
        magi_core::MissionId::new("context")
    });
    let mut prior = test_task(
        "prior-context",
        "context-root",
        Some(TaskId::new("context-root")),
    );
    prior.status = TaskStatus::Completed;
    let mut current = test_task(
        "current-context",
        "context-root",
        Some(TaskId::new("context-root")),
    );
    for (task, generation, previous_task_id) in [
        (&mut prior, 1, None),
        (&mut current, 2, Some(TaskId::new("prior-context"))),
    ] {
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: TaskId::new("prior-context"),
            session_id: session.clone(),
            lifetime: magi_core::AgentLifetime::Session,
            generation,
            previous_task_id,
        });
        task.executor_binding = Some(binding);
    }
    tasks.insert_task(prior.clone()).unwrap();
    let sessions = crate::test_history::with_messages(
        &sessions,
        &thread,
        serde_json::from_value(serde_json::json!([
            {"role": "assistant", "content": "canonical prior result"},
        ]))
        .unwrap(),
        UtcMillis(2),
    );
    let mut state = sessions.export_state();
    state.thread_registry[0].handled_task_ids = vec![prior.task_id.clone()];
    let sessions = SessionStore::from_state_for_test(state);
    let candidates = collect_context_candidates(&tasks, &sessions, &current, &session).unwrap();
    let history = candidates
        .iter()
        .find(|candidate| candidate.reference_id == "task:prior-context:transcript")
        .unwrap();
    assert!(history.content.contains("canonical prior result"));
    current
        .executor_binding
        .as_mut()
        .unwrap()
        .agent_activation
        .as_mut()
        .unwrap()
        .previous_task_id = Some(TaskId::new("missing-activation"));
    let (payload, status) = execute_context_read(
        &tasks,
        &sessions,
        &current,
        &session,
        BuiltinToolName::ContextRead,
        &serde_json::json!({"reference_ids": ["task:prior-context:transcript"]}),
    );
    assert_eq!(status, ExecutionResultStatus::Failed);
    assert!(payload.contains("context_history_unavailable"));
    assert!(payload.contains("前序 activation 缺失"));
}

#[test]
fn context_search_and_read_are_scoped_and_audited() {
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let session_id = SessionId::new("session-context-gateway");
    session_store
        .create_session(session_id.clone(), "上下文网关")
        .expect("session should create");
    session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "登录流程必须校验 workspaceId",
    );
    let mut task = test_task(
        "task-context-worker",
        "task-context-root",
        Some(TaskId::new("task-context-root")),
    );
    task.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "agent-context-gateway".to_string(),
            revision: 1,
            parent_task_id: TaskId::new("task-context-root"),
            summary: "检查登录流程".to_string(),
            constraints: Vec::new(),
            expected_output: "返回检查结果".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };
    task_store.insert_task(task.clone()).expect("任务应插入");

    let (search_payload, search_status) = execute_context_search(
        &task_store,
        &session_store,
        &task,
        &session_id,
        BuiltinToolName::ContextSearch,
        &serde_json::json!({"query": "workspaceId"}),
    );
    assert_eq!(search_status, ExecutionResultStatus::Succeeded);
    let search: serde_json::Value = serde_json::from_str(&search_payload).unwrap();
    let reference_id = search["results"][0]["reference_id"]
        .as_str()
        .expect("search should return a scoped turn reference")
        .to_string();

    let (read_payload, read_status) = execute_context_read(
        &task_store,
        &session_store,
        &task,
        &session_id,
        BuiltinToolName::ContextRead,
        &serde_json::json!({"reference_ids": [reference_id]}),
    );
    assert_eq!(read_status, ExecutionResultStatus::Succeeded);
    assert!(read_payload.contains("登录流程必须校验 workspaceId"));
    let stored = task_store.get_task(&task.task_id).unwrap();
    assert_eq!(stored.agent_context_accesses().len(), 2);
    assert_eq!(
        stored.agent_context_accesses()[0].operation,
        AgentContextAccessOperation::Search
    );
    assert_eq!(
        stored.agent_context_accesses()[1].operation,
        AgentContextAccessOperation::Read
    );
}

#[test]
fn agent_cancel_kills_direct_child_subtree_and_rejects_foreign_tasks() {
    let task_store = TaskStore::new();
    let registry = TaskExecutionRegistry::default();
    let parent = test_task("task-cancel-parent", "task-cancel-parent", None);
    let child = test_task(
        "task-cancel-child",
        "task-cancel-parent",
        Some(parent.task_id.clone()),
    );
    let mut queued_sibling = test_task(
        "task-cancel-sibling",
        "task-cancel-parent",
        Some(parent.task_id.clone()),
    );
    queued_sibling.status = TaskStatus::Pending;
    for task in [parent.clone(), child.clone(), queued_sibling.clone()] {
        task_store.insert_task(task).expect("任务应插入");
    }
    let grandchild = test_task(
        "task-cancel-grandchild",
        "task-cancel-parent",
        Some(child.task_id.clone()),
    );
    task_store.insert_task(grandchild.clone()).unwrap();

    let (payload, status) = execute_agent_cancel(
        &task_store,
        &registry,
        &SessionStore::new(),
        &parent,
        &SessionId::new("agent-cancel-session"),
        BuiltinToolName::AgentCancel,
        &serde_json::json!({"task_id": child.task_id, "reason": "方向错误"}),
    );
    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["status"].as_str(), Some("cancelled"));
    assert_eq!(parsed["child_status"].as_str(), Some("killed"));
    assert_eq!(
        parsed["resources_settled"], false,
        "未接宿主结算不能声称资源退出"
    );
    assert_eq!(parsed["results"][0]["child_status"], "killed");
    assert_eq!(
        parsed["results"][0]["result"]["final_text"],
        "主线取消：方向错误"
    );
    let killed = task_store.get_task(&child.task_id).expect("子任务应存在");
    assert_eq!(killed.status, TaskStatus::Killed);
    assert_eq!(
        task_store.get_task(&grandchild.task_id).unwrap().status,
        TaskStatus::Killed
    );
    assert!(
        killed
            .output_refs
            .iter()
            .any(|output| output.contains("方向错误"))
    );
    assert_eq!(
        task_store.get_task(&queued_sibling.task_id).unwrap().status,
        TaskStatus::Pending,
        "取消只作用于目标代理"
    );

    let (payload, _) = execute_agent_cancel(
        &task_store,
        &registry,
        &SessionStore::new(),
        &parent,
        &SessionId::new("agent-cancel-session"),
        BuiltinToolName::AgentCancel,
        &serde_json::json!({"task_id": child.task_id, "reason": "重复取消"}),
    );
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["status"].as_str(), Some("already_terminal"));

    let (payload, status) = execute_agent_cancel(
        &task_store,
        &registry,
        &SessionStore::new(),
        &child,
        &SessionId::new("agent-cancel-session"),
        BuiltinToolName::AgentCancel,
        &serde_json::json!({"task_id": queued_sibling.task_id, "reason": "越权"}),
    );
    assert_eq!(status, ExecutionResultStatus::Rejected);
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("结果应为 JSON");
    assert_eq!(parsed["error_code"].as_str(), Some("scope_mismatch"));
    assert_eq!(
        task_store.get_task(&queued_sibling.task_id).unwrap().status,
        TaskStatus::Pending
    );
}

#[test]
fn agent_cancel_settlement_failure_keeps_original_result_and_can_be_rechecked() {
    let store = Arc::new(TaskStore::new());
    let registry = TaskExecutionRegistry::default();
    let session = SessionId::new("agent-settlement-session");
    let parent = test_task("settle-parent", "settle-parent", None);
    let child = test_task(
        "settle-child",
        parent.task_id.as_str(),
        Some(parent.task_id.clone()),
    );
    let leaf = test_task(
        "settle-leaf",
        parent.task_id.as_str(),
        Some(child.task_id.clone()),
    );
    let sibling = test_task(
        "settle-sibling",
        parent.task_id.as_str(),
        Some(parent.task_id.clone()),
    );
    for task in [&parent, &child, &leaf, &sibling] {
        store.insert_task(task.clone()).unwrap();
    }
    let expected_session = session.clone();
    let expected = vec![child.task_id.clone(), leaf.task_id.clone()];
    let facts = store.clone();
    registry.set_agent_subtree_settler(move |session, tasks| {
        assert_eq!(session, &expected_session);
        assert_eq!(tasks, expected);
        assert!(
            tasks
                .iter()
                .all(|id| facts.get_task(id).unwrap().status == TaskStatus::Killed)
        );
        Err("进程仍在退出".into())
    });
    let args = serde_json::json!({"task_id":child.task_id,"reason":"停止无用分支"});
    let (payload, status) = execute_agent_cancel(
        &store,
        &registry,
        &SessionStore::new(),
        &parent,
        &session,
        BuiltinToolName::AgentCancel,
        &args,
    );
    assert_eq!(status, ExecutionResultStatus::Failed);
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["cancellation_status"], "cancelled");
    assert_eq!(payload["error_code"], "cancel_unsettled");
    assert_eq!(payload["resources_settled"], false);
    assert_eq!(payload["settlement_error"], "进程仍在退出");
    let original = serde_json::to_value(store.get_task(&child.task_id).unwrap()).unwrap();
    registry.set_agent_subtree_settler(|_, _| Ok(()));
    let (recheck, status) = execute_agent_cancel(
        &store,
        &registry,
        &SessionStore::new(),
        &parent,
        &session,
        BuiltinToolName::AgentCancel,
        &args,
    );
    assert_eq!(status, ExecutionResultStatus::Succeeded);
    let recheck: serde_json::Value = serde_json::from_str(&recheck).unwrap();
    assert_eq!(recheck["status"], "already_terminal");
    assert_eq!(recheck["resources_settled"], true);
    assert_eq!(recheck["results"], payload["results"]);
    assert_eq!(
        serde_json::to_value(store.get_task(&child.task_id).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        store.get_task(&parent.task_id).unwrap().status,
        parent.status
    );
    assert_eq!(
        store.get_task(&sibling.task_id).unwrap().status,
        sibling.status
    );
    assert_eq!(store.all_tasks().len(), 4);
}

#[test]
fn agent_cancel_after_completion_preserves_the_original_receipt_and_task() {
    for terminal in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let task_store = TaskStore::new();
        let registry = TaskExecutionRegistry::default();
        let parent = test_task("cancel-terminal-parent", "cancel-terminal-parent", None);
        let mut child = test_task(
            "cancel-terminal-child",
            parent.task_id.as_str(),
            Some(parent.task_id.clone()),
        );
        child.status = terminal;
        child.output_refs = if terminal == TaskStatus::Completed {
            vec![
                completed_output_ref(&format!("已完成{}结尾证据", "原始结果".repeat(3_000))),
                serde_json::json!({"agent_workspace": {
                    "branch":"magi/agent/original", "base":"base", "head":"head",
                    "changed_paths":["src/parser.rs"]
                }})
                .to_string(),
            ]
        } else {
            vec!["原任务已停止，先核对副作用".into()]
        };
        task_store.insert_task(parent.clone()).unwrap();
        task_store.insert_task(child.clone()).unwrap();
        let before = serde_json::to_value(&child).unwrap();
        let (wait, _) = execute_agent_wait(
            &task_store,
            &parent,
            BuiltinToolName::AgentWait,
            &serde_json::json!({"task_ids":[child.task_id]}),
        );
        let wait: serde_json::Value = serde_json::from_str(&wait).unwrap();
        for _ in 0..2 {
            let (payload, status) = execute_agent_cancel(
                &task_store,
                &registry,
                &SessionStore::new(),
                &parent,
                &SessionId::new("agent-cancel-session"),
                BuiltinToolName::AgentCancel,
                &serde_json::json!({"task_id":child.task_id, "reason":"已不再需要"}),
            );
            assert_eq!(status, ExecutionResultStatus::Succeeded);
            assert_eq!(
                crate::tool_result_utils::model_visible_tool_result(&payload, status),
                payload,
                "长回执必须以有效 JSON 实际交付给模型，不能在通用截断中丢失 results"
            );
            let result: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(result["status"], "already_terminal");
            assert_eq!(result["results"], wait["results"]);
            assert!(
                result["instruction"]
                    .as_str()
                    .unwrap()
                    .contains(magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE)
            );
            assert_eq!(
                serde_json::to_value(task_store.get_task(&child.task_id).unwrap()).unwrap(),
                before
            );
            assert_eq!(task_store.get_children(&parent.task_id).len(), 1);
            if terminal == TaskStatus::Completed {
                assert_eq!(
                    result["results"][0]["workspace"]["branch"],
                    "magi/agent/original"
                );
                assert_eq!(
                    result["results"][0]["activity"]["commands"][0]["exit_code"],
                    0
                );
                assert_eq!(result["results"][0]["result"]["truncated"], true);
            }
        }
    }
}

#[test]
fn agent_send_updates_package_revision_and_reaches_running_child() {
    run_agent_send_to_running_child(false, false);
}

#[test]
fn agent_send_checkpoint_failure_leaves_no_partial_supplement_or_wakeup() {
    run_agent_send_to_running_child(true, false);
}

#[test]
fn agent_send_pending_to_running_race_delivers_once_via_the_live_channel() {
    run_agent_send_to_running_child(false, true);
    run_agent_send_to_running_child(true, true);
}

fn run_agent_send_to_running_child(reject_checkpoint: bool, stale_pending: bool) {
    let task_store = TaskStore::new();
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-send");
    let parent = coordinator_task(test_task(
        "task-agent-send-parent",
        "task-agent-send-parent",
        None,
    ));
    let mut child = test_task(
        "task-agent-send-child",
        "task-agent-send-parent",
        Some(parent.task_id.clone()),
    );
    child.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "agent-context-send".to_string(),
            revision: 1,
            parent_task_id: parent.task_id.clone(),
            summary: "初始任务".to_string(),
            constraints: Vec::new(),
            expected_output: "完成检查".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };
    task_store
        .insert_task(parent.clone())
        .expect("父任务应插入");
    task_store.insert_task(child.clone()).expect("子任务应插入");
    registry.open_task_signal_channel(&session_id, &child.task_id);
    let checkpoint_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = Arc::clone(&checkpoint_count);
    task_store.set_checkpoint_callback(Box::new(move |_| {
        captured.fetch_add(1, Ordering::SeqCst);
        if reject_checkpoint {
            Err(magi_core::DomainError::Persistence {
                message: "injected".into(),
            })
        } else {
            Ok(())
        }
    }));

    let observed_version = task_store.status_change_version();
    let (payload, status) = if stale_pending {
        // 调用方看到 Pending 后，启动事务已经把真实子代理推进至 Running。
        assert_eq!(child.status, TaskStatus::Running);
        deliver_agent_send(
            &task_store,
            &registry,
            &parent,
            &session_id,
            BuiltinToolName::AgentSend,
            &child.task_id,
            TaskStatus::Pending,
            AgentContextSupplement {
                supplement_id: "raced-supplement".into(),
                author_task_id: parent.task_id.clone(),
                message: "补充：只检查当前分支".into(),
                references: vec![],
                estimated_tokens: 10,
                created_at: UtcMillis::now(),
            },
            AgentContextAccessRecord {
                record_id: "raced-access".into(),
                operation: AgentContextAccessOperation::Send,
                query: Some("补充：只检查当前分支".into()),
                reference_ids: vec![],
                estimated_tokens: 10,
                occurred_at: UtcMillis::now(),
            },
            10,
            &|_, _| {},
        )
    } else {
        execute_agent_send(
            &task_store,
            &registry,
            &parent,
            &session_id,
            BuiltinToolName::AgentSend,
            &serde_json::json!({
                "task_id": child.task_id,
                "message": "补充：只检查当前分支"
            }),
            &|_, _| {},
        )
    };
    assert_eq!(
        checkpoint_count.load(Ordering::SeqCst),
        1,
        "一次发送只提交一次任务事务"
    );
    if reject_checkpoint {
        assert_eq!(status, ExecutionResultStatus::Failed);
        assert_eq!(task_store.status_change_version(), observed_version);
        let stored = task_store.get_task(&child.task_id).unwrap();
        assert_eq!(stored.agent_context_package().unwrap().revision, 1);
        assert!(
            stored
                .agent_context_package()
                .unwrap()
                .supplements
                .is_empty()
        );
        assert!(stored.agent_context_accesses().is_empty());
        assert!(
            registry
                .drain_task_signals(&session_id, &child.task_id)
                .is_empty()
        );
        return;
    }
    assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
    assert_ne!(
        task_store.wait_for_status_change_since(observed_version, std::time::Duration::ZERO),
        observed_version,
        "运行中代理可能正阻塞在 agent_wait，补充提交后必须唤醒它，不能等孙代理完成或超时"
    );
    let stored = task_store.get_task(&child.task_id).unwrap();
    let package = stored.agent_context_package().unwrap();
    assert_eq!(package.revision, 2);
    assert_eq!(package.supplements.len(), 1);
    assert_eq!(stored.agent_context_accesses().len(), 1);
    let signals = registry.drain_task_signals(&session_id, &child.task_id);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].payload["revision"], 2);
}

#[test]
fn agent_send_to_queued_child_is_delivered_with_its_start_context() {
    let task_store = TaskStore::new();
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-send-queued");
    let parent = coordinator_task(test_task(
        "task-agent-send-queued-parent",
        "task-agent-send-queued-parent",
        None,
    ));
    let mut child = test_task(
        "task-agent-send-queued-child",
        "task-agent-send-queued-parent",
        Some(parent.task_id.clone()),
    );
    child.status = TaskStatus::Pending;
    child.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "agent-context-queued".to_string(),
            revision: 1,
            parent_task_id: parent.task_id.clone(),
            summary: "初始任务".to_string(),
            constraints: Vec::new(),
            expected_output: "完成检查".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };
    task_store
        .insert_task(parent.clone())
        .expect("父任务应插入");
    task_store.insert_task(child.clone()).expect("子任务应插入");
    // 不打开信号通道：排队中的代理（例如重启后恢复的）没有运行期通道。

    let (payload, status) = execute_agent_send(
        &task_store,
        &registry,
        &parent,
        &session_id,
        BuiltinToolName::AgentSend,
        &serde_json::json!({
            "task_id": child.task_id,
            "message": "补充：只看 src/auth"
        }),
        &|_, _| {},
    );

    assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
    assert_eq!(parsed["delivery"], "on_start");
    let stored = task_store.get_task(&child.task_id).unwrap();
    let package = stored.agent_context_package().unwrap();
    assert_eq!(package.supplements.len(), 1);
    assert!(
        package.render_for_prompt().contains("补充：只看 src/auth"),
        "代理开始执行时的上下文必须包含排队期间收到的补充"
    );
    assert!(
        registry
            .drain_task_signals(&session_id, &child.task_id)
            .is_empty()
    );
}

#[test]
fn agent_send_rejects_non_direct_child_and_scope_mismatch() {
    let task_store = TaskStore::new();
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-send-scope");
    let parent = coordinator_task(test_task(
        "task-agent-send-scope-parent",
        "task-agent-send-scope-parent",
        None,
    ));
    let child = test_task(
        "task-agent-send-scope-child",
        "task-agent-send-scope-parent",
        Some(parent.task_id.clone()),
    );
    let grandchild = test_task(
        "task-agent-send-scope-grandchild",
        "task-agent-send-scope-parent",
        Some(child.task_id.clone()),
    );
    task_store
        .insert_task(parent.clone())
        .expect("父任务应插入");
    task_store.insert_task(child).expect("子任务应插入");
    task_store
        .insert_task(grandchild.clone())
        .expect("孙任务应插入");
    registry.open_task_signal_channel(&session_id, &grandchild.task_id);

    let (payload, status) = execute_agent_send(
        &task_store,
        &registry,
        &parent,
        &session_id,
        BuiltinToolName::AgentSend,
        &serde_json::json!({
            "task_id": grandchild.task_id,
            "message": "不应越级发送",
        }),
        &|_, _| {},
    );
    assert_eq!(status, ExecutionResultStatus::Failed);
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["error_code"], "target_out_of_scope");
    assert!(
        registry
            .drain_task_signals(&session_id, &grandchild.task_id)
            .is_empty()
    );
}

#[test]
fn context_request_reaches_parent_task_channel() {
    for delivery in [
        "reply",
        "task_closed",
        "session_removed",
        "parent_stopped",
        "reply_then_parent_stopped",
    ] {
        let task_store = TaskStore::new();
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-context-request");
        let parent = coordinator_task(test_task(
            "task-context-request-parent",
            "task-context-request-parent",
            None,
        ));
        let mut child = test_task(
            "task-context-request-child",
            "task-context-request-parent",
            Some(parent.task_id.clone()),
        );
        child.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(AgentContextPackage {
                package_id: "agent-context-request".to_string(),
                revision: 1,
                parent_task_id: parent.task_id.clone(),
                summary: "检查配置".to_string(),
                constraints: Vec::new(),
                expected_output: "完成检查".to_string(),
                references: Vec::new(),
                fork: None,
                supplements: Vec::new(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }),
            accesses: Vec::new(),
        };
        task_store
            .insert_task(parent.clone())
            .expect("父任务应插入");
        task_store.insert_task(child.clone()).expect("子任务应插入");
        registry.open_task_signal_channel(&session_id, &parent.task_id);
        registry.open_task_signal_channel(&session_id, &child.task_id);

        let execution_registry =
            TaskExecutionRegistry::default().with_execution_admission(Arc::new(
                crate::execution_admission::ExecutionAdmissionController::new(
                    crate::execution_admission::ExecutionAdmissionLimits {
                        max_active_tasks: 1,
                        max_active_tasks_per_session: 1,
                        max_active_tasks_per_role: 1,
                        min_available_memory_bytes: 0,
                    },
                ),
            ));
        let admission = execution_registry.execution_admission();
        let _parent_permit = admission
            .acquire(
                parent.task_id.clone(),
                Some(session_id.clone()),
                "coordinator",
            )
            .unwrap();
        std::thread::scope(|scope| {
            let request_handle = scope.spawn(|| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let _child_permit = loop {
                    if let Ok(permit) = admission.acquire(
                        child.task_id.clone(),
                        Some(session_id.clone()),
                        "explorer",
                    ) {
                        break permit;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "父任务等待后必须让出唯一执行名额"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                };
                run_with_task_execution_wait(
                    &task_store,
                    &execution_registry,
                    &child,
                    "context_request",
                    || {
                        execute_context_request(
                            &task_store,
                            &registry,
                            &child,
                            &session_id,
                            BuiltinToolName::ContextRequest,
                            &serde_json::json!({
                                "request": "请提供模型配置来源",
                                "timeout_ms": 5000,
                            }),
                        )
                    },
                    &|| true,
                )
                .unwrap()
            });
            let (wait_payload, wait_status) = execute_agent_wait_with_runtime(
                AgentWaitRuntime {
                    session_store: &SessionStore::new(),
                    task_store: &task_store,
                    admission: &admission,
                    conversation_registry: &registry,
                    session_id: &session_id,
                    execution_allowed: &|| true,
                },
                &parent,
                BuiltinToolName::AgentWait,
                &serde_json::json!({
                    "task_ids": [child.task_id],
                    "timeout_ms": 5000,
                }),
            );
            assert_eq!(wait_status, ExecutionResultStatus::Succeeded);
            let wait: serde_json::Value = serde_json::from_str(&wait_payload).unwrap();
            assert_eq!(wait["status"], "attention_required");
            assert_eq!(
                wait["runtime_signals"][0]["payload"]["type"],
                "agent_context_request"
            );

            match delivery {
                "task_closed" => {
                    registry.close_task_signal_channel(&session_id, &child.task_id);
                }
                "session_removed" => {
                    registry.remove_session(&session_id);
                }
                "parent_stopped" => {}
                _ => {
                    let (send_payload, send_status) = execute_agent_send(
                        &task_store,
                        &registry,
                        &parent,
                        &session_id,
                        BuiltinToolName::AgentSend,
                        &serde_json::json!({
                            "task_id": child.task_id,
                            "message": "模型配置来自当前 session 的 orchestrator 设置",
                        }),
                        &|_, _| {},
                    );
                    assert_eq!(
                        send_status,
                        ExecutionResultStatus::Succeeded,
                        "{send_payload}"
                    );
                }
            }
            if matches!(delivery, "parent_stopped" | "reply_then_parent_stopped") {
                task_store
                    .revoke_active_lease_and_set_task_terminal(
                        &parent.task_id,
                        &parent.root_task_id,
                        TaskStatus::Killed,
                        Vec::new(),
                    )
                    .unwrap();
                registry.close_task_signal_channel(&session_id, &parent.task_id);
            }
            // 父任务再次等待，子任务才可重新取得唯一名额并消费答复。
            let parent_wait = admission.suspend_while_waiting(&parent.task_id).unwrap();
            let (request_payload, request_status) = request_handle.join().unwrap();
            assert!(parent_wait.resume(|| true));
            if matches!(
                delivery,
                "task_closed" | "session_removed" | "parent_stopped"
            ) {
                assert_eq!(
                    request_status,
                    ExecutionResultStatus::Failed,
                    "{request_payload}"
                );
                let request: serde_json::Value = serde_json::from_str(&request_payload).unwrap();
                assert_eq!(
                    request["error_code"],
                    if delivery == "parent_stopped" {
                        "parent_not_running"
                    } else {
                        "task_interrupted"
                    }
                );
            } else {
                assert_eq!(
                    request_status,
                    ExecutionResultStatus::Succeeded,
                    "{request_payload}"
                );
                let request: serde_json::Value = serde_json::from_str(&request_payload).unwrap();
                assert_eq!(request["status"], "resolved");
                assert_eq!(
                    request["responses"][0]["payload"]["type"],
                    "agent_context_supplement"
                );
            }
        });
        assert_eq!(
            task_store
                .get_task(&child.task_id)
                .unwrap()
                .agent_context_accesses()[0]
                .operation,
            AgentContextAccessOperation::Request
        );
    }
}

#[test]
fn context_search_matches_unsegmented_chinese_queries() {
    let candidate = |content: &str| ResolvedAgentContext {
        reference_id: "turn:1".into(),
        kind: AgentContextReferenceKind::ConversationTurn,
        title: "用户消息".into(),
        content: content.into(),
    };
    let relevant = candidate("登录接口返回 500，失败原因是会话令牌过期。");
    let unrelated = candidate("请把按钮颜色改成蓝色。");
    for query in ["登录失败的原因", "登录失败，原因", "login 失败原因"] {
        assert!(
            context_candidate_matches(query, &relevant),
            "{query} 应命中中文原文"
        );
        assert!(
            !context_candidate_matches(query, &unrelated),
            "{query} 不应命中无关内容"
        );
    }
    assert!(
        context_candidate_score("登录失败的原因", &relevant)
            > context_candidate_score("原因", &unrelated),
    );
    assert!(!context_candidate_matches("token refresh", &relevant));
}

#[test]
fn context_request_ignores_signals_that_are_not_the_parent_reply() {
    let task_store = TaskStore::new();
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-context-request-foreign");
    let parent = coordinator_task(test_task(
        "task-foreign-parent",
        "task-foreign-parent",
        None,
    ));
    let mut child = test_task(
        "task-foreign-child",
        "task-foreign-parent",
        Some(parent.task_id.clone()),
    );
    child.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "agent-context-foreign".to_string(),
            revision: 1,
            parent_task_id: parent.task_id.clone(),
            summary: "检查配置".to_string(),
            constraints: Vec::new(),
            expected_output: "完成检查".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };
    task_store.insert_task(parent.clone()).unwrap();
    task_store.insert_task(child.clone()).unwrap();
    registry.open_task_signal_channel(&session_id, &parent.task_id);
    registry.open_task_signal_channel(&session_id, &child.task_id);
    // 孙代理向这个 worker 发来的请求：不是父任务对本次 context_request 的答复。
    registry
        .enqueue_task_signal(
            &session_id,
            &child.task_id,
            RuntimeSignal {
                author: MailboxAuthor::Child("task-grandchild".to_string()),
                kind: MailboxKind::Message,
                trigger_turn: true,
                payload: serde_json::json!({"type": "agent_context_request"}),
                enqueued_at: UtcMillis(1),
            },
        )
        .unwrap();

    let (payload, status) = execute_context_request(
        &task_store,
        &registry,
        &child,
        &session_id,
        BuiltinToolName::ContextRequest,
        &serde_json::json!({"request": "请提供配置来源", "timeout_ms": 1000}),
    );
    assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
    let result: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(result["status"], "timeout", "{payload}");
    // 未被消费的信号仍在通道里，交给任务循环或 agent_wait 处理。
    let remaining = registry.drain_task_signals(&session_id, &child.task_id);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].payload["type"], "agent_context_request");
}

#[test]
fn worker_question_returns_the_answer_only_to_its_original_call() {
    let bus = InMemoryEventBus::new(32);
    let tasks = TaskStore::new();
    let sessions = SessionStore::new();
    let registry = ConversationRegistry::new();
    let roles = magi_agent_role::AgentRoleRegistry::load_default();
    let execution = TaskExecutionRegistry::default();
    let session_id = SessionId::new("session-worker-question");
    sessions
        .create_session(session_id.clone(), "子代理提问")
        .unwrap();
    let now = UtcMillis::now();
    CanonicalTurnEventSink::for_store(&sessions, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            None,
            magi_session_store::TimelineEntryInput::new(
                "worker-question-timeline",
                magi_session_store::TimelineEntryKind::UserMessage,
                "开始工作",
                now,
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: "turn-worker-question".into(),
                turn_seq: 1,
                accepted_at: now,
                status: "running".into(),
                user_message: Some("开始工作".into()),
                items: Vec::new(),
                completed_at: None,
            },
        )
        .unwrap();
    let parent = coordinator_task(test_task("question-parent", "question-parent", None));
    let child = test_task(
        "question-child",
        "question-parent",
        Some(parent.task_id.clone()),
    );
    tasks.insert_task(parent.clone()).unwrap();
    tasks.insert_task(child.clone()).unwrap();
    let plan = magi_plan::PlanStore::new(Arc::new(sessions.clone()), session_id.clone());
    let call = ChatToolCall {
            id: "question-call-child".into(), kind: "function".into(),
            function: ChatToolFunction { name: "ask_user_question".into(), arguments: serde_json::json!({
                "questions": [{ "header": "范围", "question": "检查哪些文件？", "multiSelect": false,
                    "options": [{ "label": "相关文件", "description": "定向检查" },
                        { "label": "全部文件", "description": "完整检查" }] }]
            }).to_string() },
        };
    sessions.upsert_current_turn_item_for_turn(&session_id, Some("turn-worker-question"),
            serde_json::from_value(serde_json::json!({
                "itemId": "question-child-item", "itemSeq": 1, "kind": "tool_call_started", "status": "running",
                "source": "worker", "sourceThreadId": "thread-child", "taskId": child.task_id,
                "toolName": "ask_user_question", "toolCallId": call.id, "toolArguments": call.function.arguments
            })).unwrap()).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            execute_task_tool_call(
                &bus,
                None,
                &roles,
                None,
                None,
                None,
                &tasks,
                &sessions,
                &execution,
                &registry,
                None,
                &plan,
                None,
                &child,
                &session_id,
                &None,
                None,
                None,
                None,
                &call,
                None,
                &|| true,
                None,
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let pending = loop {
            if let Some(question) = registry
                .user_questions()
                .pending_for_session(&session_id)
                .into_iter()
                .next()
            {
                break question;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "子代理必须能产生真实待回答交互"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(pending.task_id.as_ref(), Some(&child.task_id));
        assert_eq!(pending.tool_call_id, call.id);
        registry
            .user_questions()
            .resolve(
                &session_id,
                &pending.question_id,
                crate::UserQuestionResponse::Answered {
                    answers: vec![crate::UserQuestionAnswer {
                        selected: vec!["相关文件".into()],
                        other: None,
                    }],
                },
            )
            .unwrap();
        let (payload, status) = worker.join().unwrap();
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        assert!(payload.contains("相关文件"));
    });
    assert!(
        registry
            .user_questions()
            .pending_for_session(&session_id)
            .is_empty()
    );
    assert_eq!(
        tasks.get_task(&parent.task_id).unwrap().status,
        TaskStatus::Running
    );
}

#[test]
fn tool_execution_rejects_role_invisible_builtin_tools() {
    let event_bus = InMemoryEventBus::new(16);
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let conversation_registry = ConversationRegistry::new();
    let plan_store = crate::test_plan_store("test-plan");
    let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-tool-scope");
    let workspace_id = Some(WorkspaceId::new("workspace-tool-scope"));

    let worker = test_task("task-worker", "task-worker", None);
    let worker_result = execute_task_tool_call_batch(
        &event_bus,
        None,
        &agent_role_registry,
        None,
        None,
        None,
        &task_store,
        &session_store,
        &execution_registry,
        &conversation_registry,
        None,
        &plan_store,
        None,
        &worker,
        &session_id,
        &workspace_id,
        None,
        None,
        None,
        &[ChatToolCall {
            id: "call-agent-spawn-rejected".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                arguments: "{}".to_string(),
            },
        }],
        &mut ToolExecutionLedger::default(),
        None,
        None,
        None,
        &|| true,
    );
    assert_eq!(worker_result[0].1, ExecutionResultStatus::Rejected);
    let worker_payload: serde_json::Value =
        serde_json::from_str(&worker_result[0].0).expect("worker rejection should be json");
    assert_eq!(worker_payload["status"].as_str(), Some("rejected"));
    assert_eq!(
        worker_payload["error_code"].as_str(),
        Some("tool_policy_rejected")
    );
    assert_eq!(
        worker_payload["error"].as_str(),
        Some(TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR)
    );
    assert!(!worker_result[0].0.contains("tool is not visible"));
}
