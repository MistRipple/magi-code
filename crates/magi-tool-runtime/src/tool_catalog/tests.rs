use super::*;

#[test]
fn delegated_catalog_uses_execution_scope_and_policy_without_claiming_schema_loading() {
    let context = ToolExecutionContext {
        caller_scope: crate::ToolCallerScope::DelegatedTask,
        task_id: Some(magi_core::TaskId::new("catalog-worker")),
        workspace_id: Some(magi_core::WorkspaceId::new("catalog-workspace")),
        ..Default::default()
    };
    let resources = ToolRuntimeResources {
        execution_policy: Some(crate::ToolExecutionPolicy {
            denied_tool_names: vec!["file_read".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let catalog = build_tool_catalog_value("{}", &context, &resources);
    let tool = |name| {
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
    };
    assert_eq!(tool("git_status")["availability"], "available");
    assert_eq!(
        tool("git_branch_switch")["reason_code"],
        "model_call_scope_mismatch"
    );
    assert_eq!(tool("file_read")["reason_code"], "tool_explicitly_denied");
    assert_eq!(tool("file_read")["model_available"], false);
    assert_eq!(tool("git_status")["schema_included"], false);
    assert!(tool("git_status").get("schema_loaded").is_none());
}

#[test]
fn taskless_mainline_catalog_matches_execution_scope_for_root_tools() {
    let catalog = build_tool_catalog_value(
        r#"{"include_internal":true,"include_external":false}"#,
        &ToolExecutionContext::default(),
        &ToolRuntimeResources::default(),
    );
    let tool = |name| {
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
    };
    assert_eq!(tool("git_status")["availability"], "available");
    assert_eq!(tool("git_branch_switch")["availability"], "needs_approval");
    assert_eq!(tool("git_branch_switch")["model_available"], true);
    assert_eq!(
        tool("agent_spawn")["reason_code"],
        "model_call_scope_mismatch"
    );
    assert_eq!(
        tool("memory_write")["reason_code"],
        "model_call_scope_mismatch"
    );
}

#[test]
fn catalog_scope_reason_matches_execution_scope_for_every_caller_kind() {
    let callers = [
        (crate::ToolCallerScope::SessionMainline, None, true, false),
        (
            crate::ToolCallerScope::SessionMainline,
            Some("root-task"),
            true,
            true,
        ),
        (
            crate::ToolCallerScope::DelegatedTask,
            Some("worker-task"),
            false,
            false,
        ),
        (
            crate::ToolCallerScope::DelegatingTask,
            Some("delegator-task"),
            false,
            true,
        ),
    ];
    for (caller_scope, task_id, is_root, can_delegate) in callers {
        let context = ToolExecutionContext {
            caller_scope,
            task_id: task_id.map(magi_core::TaskId::new),
            ..Default::default()
        };
        let payload = build_tool_catalog_value(
            r#"{"include_internal":true,"include_external":false}"#,
            &context,
            &ToolRuntimeResources::default(),
        );
        for builtin in BuiltinToolName::ALL {
            let expected_allowed =
                builtin
                    .model_call_scope()
                    .allows(context.task_id.is_some(), is_root, can_delegate);
            let actual_mismatch = payload["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == builtin.as_str())
                .and_then(|tool| tool["reason_code"].as_str())
                == Some("model_call_scope_mismatch");
            assert_eq!(
                actual_mismatch,
                !expected_allowed,
                "{} scope mismatch for {caller_scope:?}",
                builtin.as_str()
            );
        }
    }
}

#[test]
fn catalog_scope_describes_task_owned_interactions_and_shared_state() {
    assert_eq!(
        BuiltinToolName::AskUserQuestion.model_call_scope().as_str(),
        "task_only"
    );
    assert!(
        BuiltinToolName::AskUserQuestion
            .model_call_scope()
            .allows(true, false, false)
    );
    assert!(
        !BuiltinToolName::UpdatePlan
            .model_call_scope()
            .allows(true, false, false)
    );
    assert_eq!(
        BuiltinToolName::GitStatus.model_call_scope().as_str(),
        "session_or_task"
    );
    assert!(
        BuiltinToolName::GitStatus
            .model_call_scope()
            .allows(true, false, false)
    );
    assert!(
        !BuiltinToolName::AgentSpawn
            .model_call_scope()
            .allows(true, false, false)
    );
    assert!(
        !BuiltinToolName::AgentSpawn
            .model_call_scope()
            .allows(false, false, false)
    );
    assert!(
        BuiltinToolName::AgentSpawn
            .model_call_scope()
            .allows(true, true, true)
    );
}
#[test]
fn delegator_catalog_keeps_shared_state_unavailable() {
    let scope = BuiltinToolName::AgentSpawn.model_call_scope();
    assert!(scope.allows(true, false, true));
    assert!(
        BuiltinToolName::AgentApply
            .model_call_scope()
            .allows(true, false, true)
    );
    for tool in [
        BuiltinToolName::UpdatePlan,
        BuiltinToolName::CreateGoal,
        BuiltinToolName::MemoryWrite,
        BuiltinToolName::GitBranchSwitch,
    ] {
        assert!(
            !tool.model_call_scope().allows(true, false, true),
            "{}",
            tool.as_str()
        );
    }
}

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn tool_catalog_reports_builtin_health_from_single_source() {
    let output = execute_tool_catalog(
        "{}",
        &ToolExecutionContext::default(),
        &ToolRuntimeResources::default(),
    );
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

    assert_eq!(payload["status"], "succeeded");
    assert!(payload.get("access_mode").is_none());
    assert_eq!(payload["catalog_access_mode"], "read_only");
    assert_eq!(payload["current_access_profile"], "restricted");
    assert!(
        payload["approval_policy_summary"]
            .as_str()
            .expect("approval_policy_summary")
            .contains("受限执行"),
        "default tool catalog should explain restricted execution semantics"
    );
    let summary = payload["summary"].as_str().expect("summary should be text");
    assert!(
        summary.starts_with("工具目录已更新："),
        "tool catalog summary should use product language"
    );
    for forbidden in [
        "builtin_public",
        "builtin_internal",
        "skill_tools",
        "mcp_servers",
        "schema_warnings",
        "runtime_warnings",
    ] {
        assert!(
            !summary.contains(forbidden),
            "tool catalog summary should not expose internal field {forbidden}"
        );
    }
    assert_eq!(payload["schema_warning_count"], 0);
    assert!(
        payload["runtime_warning_count"]
            .as_u64()
            .expect("runtime_warning_count")
            > 0,
        "catalog should expose runtime dependency warnings when resources are missing"
    );
    assert_eq!(
        payload["builtin_total"].as_u64().expect("builtin_total"),
        BuiltinToolName::ALL
            .iter()
            .map(BuiltinToolName::category)
            .collect::<BTreeSet<_>>()
            .len() as u64
    );
    assert_eq!(payload["external_catalog_status"], "unavailable");
    let names = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"apply_patch"));
    assert!(names.contains(&"view_image"));
    assert!(names.contains(&"tool_catalog"));
    assert!(!names.contains(&"process_launch"));
    let shell_exec = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "shell_exec")
        .expect("shell_exec should be listed");
    assert_eq!(shell_exec["policy_scope"], "input_sensitive");
    assert_eq!(shell_exec["input_sensitive_policy"], true);
    assert_eq!(shell_exec["effective_approval_policy"], "input_sensitive");
    assert_eq!(
        shell_exec["access_profile_behavior"],
        "restricted_input_sensitive"
    );
    assert!(
        shell_exec["policy_summary"]
            .as_str()
            .expect("policy_summary")
            .contains("逐次判定"),
        "input-sensitive tools should explain that runtime policy is decided per invocation"
    );
    let file_remove = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "file_remove")
        .expect("file_remove should be listed");
    assert_eq!(file_remove["policy_scope"], "input_sensitive");
    assert_eq!(file_remove["input_sensitive_policy"], true);
    assert_eq!(file_remove["effective_approval_policy"], "input_sensitive");
    assert_eq!(
        file_remove["access_profile_behavior"],
        "restricted_input_sensitive"
    );
    let file_read = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "file_read")
        .expect("file_read should be listed");
    assert_eq!(file_read["policy_scope"], "fixed");
    assert_eq!(file_read["input_sensitive_policy"], false);
    let search_semantic = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "search_semantic")
        .expect("search_semantic should be listed");
    assert_eq!(search_semantic["runtime_status"], "unavailable");
    assert_eq!(
        search_semantic["runtime_warnings"],
        serde_json::json!(["知识检索能力暂不可用"])
    );
    let agent_spawn = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "agent_spawn")
        .expect("agent_spawn should be listed");
    assert_eq!(agent_spawn["public"], true);
    assert_eq!(agent_spawn["runtime_internal"], true);
    assert_eq!(
        agent_spawn["model_call_scope"], "delegator_task_only",
        "runtime_internal 不能覆盖 agent_spawn 的模型可调用范围"
    );
    assert_eq!(agent_spawn["runtime_status"], "unavailable");
    assert_eq!(
        agent_spawn["runtime_warnings"],
        serde_json::json!(["子代理能力暂不可用"])
    );
    let warning_text = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .flat_map(|tool| {
            tool["runtime_warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in ["KnowledgeStore", "AgentRoleRegistry", "未注入", "workspace"] {
        assert!(
            !warning_text.contains(forbidden),
            "runtime warnings should use product language, found {forbidden}"
        );
    }
    assert_eq!(
        payload["runtime_dependencies"][0]["name"],
        "knowledge_store"
    );
    assert_eq!(payload["runtime_dependencies"][0]["status"], "unavailable");
    assert_eq!(
        payload["runtime_dependencies"][2]["name"],
        "agent_role_registry"
    );
    assert_eq!(payload["runtime_dependencies"][2]["status"], "unavailable");
}

#[test]
fn image_generation_tool_health_tracks_live_model_configuration() {
    let executor: crate::ImageGenerationExecutor =
        Arc::new(|_, _| panic!("catalog health must not invoke image generation"));
    let not_configured = ToolRuntimeResources {
        image_generation_executor: Some(Arc::clone(&executor)),
        image_generation_readiness_provider: Some(Arc::new(|| false)),
        ..ToolRuntimeResources::default()
    };
    let configured = ToolRuntimeResources {
        image_generation_executor: Some(executor),
        image_generation_readiness_provider: Some(Arc::new(|| true)),
        ..ToolRuntimeResources::default()
    };

    for (resources, expected) in [
        (&ToolRuntimeResources::default(), "unavailable"),
        (&not_configured, "not_ready"),
        (&configured, "ready"),
    ] {
        let payload = build_tool_catalog_value("{}", &ToolExecutionContext::default(), resources);
        let image_tool = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "image_generate")
            .expect("image_generate should be listed");
        assert_eq!(image_tool["runtime_status"], expected);
        let dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "image_generation_model")
            .expect("image generation dependency should be listed");
        assert_eq!(dependency["status"], expected);
    }
}

#[test]
fn tool_catalog_reports_effective_access_profile_policy() {
    let full_access_context = ToolExecutionContext {
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionContext::default()
    };
    let full_access_output =
        execute_tool_catalog("{}", &full_access_context, &ToolRuntimeResources::default());
    let full_access_payload: serde_json::Value =
        serde_json::from_str(&full_access_output).expect("json output");
    assert_eq!(full_access_payload["current_access_profile"], "full_access");
    assert!(
        full_access_payload["approval_policy_summary"]
            .as_str()
            .expect("approval_policy_summary")
            .contains("常规风险拦截会跳过")
    );
    let shell_exec = full_access_payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "shell_exec")
        .expect("shell_exec should be listed");
    assert_eq!(
        shell_exec["effective_approval_policy"],
        "regular_risk_block_skipped"
    );
    assert_eq!(
        shell_exec["access_profile_behavior"],
        "full_access_skips_regular_risk_blocks"
    );

    let read_only_context = ToolExecutionContext {
        access_profile: magi_core::AccessProfile::ReadOnly,
        ..ToolExecutionContext::default()
    };
    let read_only_output =
        execute_tool_catalog("{}", &read_only_context, &ToolRuntimeResources::default());
    let read_only_payload: serde_json::Value =
        serde_json::from_str(&read_only_output).expect("json output");
    assert_eq!(read_only_payload["current_access_profile"], "read_only");
    let file_write = read_only_payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "file_write")
        .expect("file_write should be listed");
    assert_eq!(file_write["effective_approval_policy"], "not_applicable");
    assert_eq!(
        file_write["access_profile_behavior"],
        "unavailable_in_read_only"
    );
}

#[test]
fn personal_model_catalog_has_local_tools_without_project_identity() {
    let context = ToolExecutionContext {
        task_id: Some(magi_core::TaskId::new("personal-task")),
        working_directory: Some(std::env::temp_dir()),
        ..ToolExecutionContext::default()
    };
    let payload = build_tool_catalog_value(
        r#"{"include_external":false}"#,
        &context,
        &ToolRuntimeResources::default(),
    );
    for name in ["file_read", "file_write", "shell_exec", "search_text"] {
        let tool = payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert_eq!(tool["workspace_required"], false, "{name}");
        assert_eq!(tool["model_available"], true, "{name}");
    }
}

#[test]
fn model_catalog_marks_workspace_tools_unavailable_without_workspace_scope() {
    let context = ToolExecutionContext {
        task_id: Some(magi_core::TaskId::new("task-no-workspace")),
        ..ToolExecutionContext::default()
    };
    let payload: serde_json::Value = serde_json::from_str(&execute_tool_catalog(
        r#"{"include_internal":true,"include_external":false}"#,
        &context,
        &ToolRuntimeResources::default(),
    ))
    .expect("catalog output should be json");

    assert_eq!(payload["workspace_bound"], false);
    for name in [
        "memory_write",
        "knowledge_query",
        "search_semantic",
        "code_symbols",
    ] {
        let tool = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} should be listed"));
        assert_eq!(tool["workspace_required"], true, "{name}");
        assert_eq!(tool["model_available"], false, "{name}");
        assert_eq!(tool["runtime_status"], "unavailable", "{name}");
        assert_eq!(
            tool["runtime_warnings"],
            serde_json::json!(["当前任务未绑定工作区"]),
            "{name}"
        );
    }

    let browser = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "browser_navigate")
        .expect("browser_navigate should be listed");
    assert_eq!(browser["workspace_required"], false);
    assert_eq!(browser["model_available"], true);
}

#[test]
fn tool_catalog_policy_view_matches_builtin_policy_axes_for_all_tools() {
    for access_profile in [
        magi_core::AccessProfile::ReadOnly,
        magi_core::AccessProfile::Restricted,
        magi_core::AccessProfile::FullAccess,
    ] {
        let context = ToolExecutionContext {
            access_profile,
            ..ToolExecutionContext::default()
        };
        let payload: serde_json::Value = serde_json::from_str(&execute_tool_catalog(
            r#"{"include_internal":true}"#,
            &context,
            &ToolRuntimeResources::default(),
        ))
        .expect("catalog output should be json");
        let tools = payload["tools"].as_array().expect("tools");

        for builtin in BuiltinToolName::ALL {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == builtin.as_str())
                .unwrap_or_else(|| panic!("{} should be listed", builtin.as_str()));
            let expected_input_sensitive = builtin.uses_input_sensitive_invocation_policy();
            assert_eq!(
                tool["policy_scope"],
                if expected_input_sensitive {
                    "input_sensitive"
                } else {
                    "fixed"
                },
                "{} policy_scope should match input-sensitive axis",
                builtin.as_str()
            );
            assert_eq!(
                tool["input_sensitive_policy"],
                expected_input_sensitive,
                "{} input_sensitive_policy should match builtin policy",
                builtin.as_str()
            );

            let (expected_approval, expected_behavior) = match access_profile {
                magi_core::AccessProfile::ReadOnly
                    if builtin.is_access_profile_write_operation() =>
                {
                    ("not_applicable", "unavailable_in_read_only")
                }
                magi_core::AccessProfile::ReadOnly => ("none", "read_only_allowed"),
                magi_core::AccessProfile::Restricted if expected_input_sensitive => {
                    ("input_sensitive", "restricted_input_sensitive")
                }
                magi_core::AccessProfile::Restricted
                    if builtin.default_approval_requirement()
                        == magi_core::ApprovalRequirement::Required =>
                {
                    ("required", "restricted_blocks_high_risk")
                }
                magi_core::AccessProfile::Restricted => ("none", "restricted_allowed"),
                magi_core::AccessProfile::FullAccess
                    if expected_input_sensitive
                        || builtin.default_approval_requirement()
                            == magi_core::ApprovalRequirement::Required =>
                {
                    (
                        "regular_risk_block_skipped",
                        "full_access_skips_regular_risk_blocks",
                    )
                }
                magi_core::AccessProfile::FullAccess => ("none", "full_access_allowed"),
            };
            assert_eq!(
                tool["effective_approval_policy"],
                expected_approval,
                "{} effective_approval_policy should match access profile",
                builtin.as_str()
            );
            assert_eq!(
                tool["access_profile_behavior"],
                expected_behavior,
                "{} access_profile_behavior should match access profile",
                builtin.as_str()
            );
        }
    }
}

#[test]
fn tool_catalog_can_include_internal_and_schema() {
    let output = execute_tool_catalog(
        r#"{"include_internal":true,"include_schema":true}"#,
        &ToolExecutionContext::default(),
        &ToolRuntimeResources::default(),
    );
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
    let tools = payload["tools"].as_array().expect("tools");
    let process_launch = tools
        .iter()
        .find(|tool| tool["name"] == "process_launch")
        .expect("internal tool should be included");

    assert_eq!(process_launch["public"], false);
    assert_eq!(process_launch["parameters_schema"]["type"], "object");
}

#[test]
fn tool_catalog_includes_runtime_capability_dependency_provider_entries() {
    let resources = ToolRuntimeResources {
        runtime_capability_dependency_provider: Some(std::sync::Arc::new(|| {
            vec![crate::RuntimeCapabilityDependencyEntry {
                name: "file_snapshot".to_string(),
                status: "ready".to_string(),
                required_by: vec!["changes/diff".to_string()],
                role_count: None,
                spawnable_role_count: None,
                configured_count: None,
                enabled_count: None,
                ready_count: None,
                tool_count: None,
            }]
        })),
        ..ToolRuntimeResources::default()
    };
    let context = ToolExecutionContext {
        session_id: Some(magi_core::SessionId::new("session-tool-catalog")),
        workspace_id: Some(magi_core::WorkspaceId::new("workspace-tool-catalog")),
        ..ToolExecutionContext::default()
    };

    let output = execute_tool_catalog("{}", &context, &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
    let dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "file_snapshot")
        .expect("provider dependency should be included");

    assert_eq!(dependency["status"], "ready");
    assert!(dependency.get("workspace_id").is_none());
    assert!(dependency.get("session_id").is_none());
}

#[test]
fn tool_catalog_includes_external_skill_and_mcp_health_when_provider_exists() {
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
            instruction_skill_count: 2,
            skill_tools: vec![crate::ExternalToolCatalogEntry {
                source: "skill".to_string(),
                skill_id: Some("code-review".to_string()),
                binding_id: Some("review-mcp".to_string()),
                name: "echo.describe".to_string(),
                description: "回显描述".to_string(),
                bridge_kind: "Mcp".to_string(),
                dispatch_action: "McpToolCall".to_string(),
                bridge_target: "loopback-mcp".to_string(),
                access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                risk_level: "high".to_string(),
                approval_requirement: "required".to_string(),
                status: "available".to_string(),
            }],
            mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                server_id: "loopback-mcp".to_string(),
                name: "loopback-mcp".to_string(),
                enabled: true,
                connected: true,
                health: "connected".to_string(),
                tool_count: Some(1),
                error: None,
            }],
            mcp_tools: Vec::new(),
        })),
        ..ToolRuntimeResources::default()
    };
    let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

    assert_eq!(payload["external_catalog_status"], "available");
    assert_eq!(payload["instruction_skill_count"], 2);
    assert_eq!(payload["skill_tool_count"], 1);
    assert!(
        payload["summary"]
            .as_str()
            .expect("summary should be text")
            .contains("2 个 Skill、1 个自定义工具")
    );
    assert_eq!(payload["mcp_server_count"], 1);
    assert_eq!(payload["connected_mcp_server_count"], 1);
    assert_eq!(payload["skill_tools"][0]["name"], "echo.describe");
    assert_eq!(payload["mcp_servers"][0]["health"], "connected");
    let skill_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "skill_runtime")
        .expect("skill runtime dependency should be listed");
    let mcp_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "mcp_servers")
        .expect("mcp dependency should be listed");
    assert_eq!(skill_dependency["status"], "ready");
    assert_eq!(skill_dependency["configured_count"], 2);
    assert_eq!(skill_dependency["tool_count"], 1);
    assert_eq!(mcp_dependency["status"], "ready");
    assert_eq!(mcp_dependency["enabled_count"], 1);
    assert_eq!(mcp_dependency["ready_count"], 1);
    assert_eq!(mcp_dependency["enabled_tool_count"], 1);
    assert_eq!(mcp_dependency["ready_tool_count"], 1);
    assert_eq!(mcp_dependency["tool_count"], 1);
}

#[test]
fn tool_catalog_does_not_call_external_provider_when_external_disabled() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(move || {
            provider_calls.fetch_add(1, Ordering::SeqCst);
            ExternalToolCatalogSnapshot {
                instruction_skill_count: 1,
                skill_tools: vec![crate::ExternalToolCatalogEntry {
                    source: "skill".to_string(),
                    skill_id: Some("slow-skill".to_string()),
                    binding_id: Some("slow-mcp".to_string()),
                    name: "slow.describe".to_string(),
                    description: "不应被加载的外部工具".to_string(),
                    bridge_kind: "Mcp".to_string(),
                    dispatch_action: "McpToolCall".to_string(),
                    bridge_target: "slow-mcp".to_string(),
                    access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                    risk_level: "high".to_string(),
                    approval_requirement: "required".to_string(),
                    status: "available".to_string(),
                }],
                mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                    server_id: "slow-mcp".to_string(),
                    name: "slow-mcp".to_string(),
                    enabled: true,
                    connected: true,
                    health: "connected".to_string(),
                    tool_count: Some(1),
                    error: None,
                }],
                mcp_tools: Vec::new(),
            }
        })),
        ..ToolRuntimeResources::default()
    };

    let output = execute_tool_catalog(
        r#"{"include_external":false,"include_mcp_servers":false}"#,
        &ToolExecutionContext::default(),
        &resources,
    );
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "include_external=false must not hydrate external tool providers"
    );
    assert_eq!(payload["external_catalog_status"], "disabled");
    assert_eq!(payload["skill_tool_count"], 0);
    assert_eq!(payload["mcp_server_count"], 0);
    let skill_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "skill_runtime")
        .expect("skill runtime dependency should be listed");
    let mcp_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "mcp_servers")
        .expect("mcp dependency should be listed");
    assert_eq!(skill_dependency["status"], "disabled");
    assert_eq!(mcp_dependency["status"], "disabled");
}

#[test]
fn tool_catalog_reuses_single_external_snapshot_for_dependencies() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(move || {
            provider_calls.fetch_add(1, Ordering::SeqCst);
            ExternalToolCatalogSnapshot {
                instruction_skill_count: 1,
                skill_tools: vec![crate::ExternalToolCatalogEntry {
                    source: "skill".to_string(),
                    skill_id: Some("code-review".to_string()),
                    binding_id: Some("review-mcp".to_string()),
                    name: "echo.describe".to_string(),
                    description: "回显描述".to_string(),
                    bridge_kind: "Mcp".to_string(),
                    dispatch_action: "McpToolCall".to_string(),
                    bridge_target: "loopback-mcp".to_string(),
                    access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                    risk_level: "high".to_string(),
                    approval_requirement: "required".to_string(),
                    status: "available".to_string(),
                }],
                mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                    server_id: "loopback-mcp".to_string(),
                    name: "loopback-mcp".to_string(),
                    enabled: true,
                    connected: true,
                    health: "connected".to_string(),
                    tool_count: Some(1),
                    error: None,
                }],
                mcp_tools: Vec::new(),
            }
        })),
        ..ToolRuntimeResources::default()
    };

    let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "external provider should be called once and reused for dependency health"
    );
    assert_eq!(payload["external_catalog_status"], "available");
    assert_eq!(payload["skill_tool_count"], 1);
    assert_eq!(payload["mcp_server_count"], 1);
    let skill_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "skill_runtime")
        .expect("skill runtime dependency should be listed");
    let mcp_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "mcp_servers")
        .expect("mcp dependency should be listed");
    assert_eq!(skill_dependency["tool_count"], 1);
    assert_eq!(mcp_dependency["ready_tool_count"], 1);
}

#[test]
fn tool_catalog_reports_effective_external_mcp_skill_policy_by_access_profile() {
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
            instruction_skill_count: 1,
            skill_tools: vec![crate::ExternalToolCatalogEntry {
                source: "skill".to_string(),
                skill_id: Some("code-review".to_string()),
                binding_id: Some("review-mcp".to_string()),
                name: "echo.describe".to_string(),
                description: "回显描述".to_string(),
                bridge_kind: "Mcp".to_string(),
                dispatch_action: "McpToolCall".to_string(),
                bridge_target: "loopback-mcp".to_string(),
                access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                risk_level: "high".to_string(),
                approval_requirement: "required".to_string(),
                status: "available".to_string(),
            }],
            mcp_servers: Vec::new(),
            mcp_tools: Vec::new(),
        })),
        ..ToolRuntimeResources::default()
    };

    let read_only_context = ToolExecutionContext {
        access_profile: magi_core::AccessProfile::ReadOnly,
        ..ToolExecutionContext::default()
    };
    let read_only_payload: serde_json::Value =
        serde_json::from_str(&execute_tool_catalog("{}", &read_only_context, &resources))
            .expect("json output");
    assert_eq!(
        read_only_payload["skill_tools"][0]["status"],
        "unavailable_in_read_only"
    );
    assert_eq!(
        read_only_payload["skill_tools"][0]["access_profile_behavior"],
        "unavailable_in_read_only"
    );
    assert_eq!(
        read_only_payload["skill_tools"][0]["approval_requirement"],
        "not_applicable"
    );

    let full_access_context = ToolExecutionContext {
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionContext::default()
    };
    let full_access_payload: serde_json::Value = serde_json::from_str(&execute_tool_catalog(
        "{}",
        &full_access_context,
        &resources,
    ))
    .expect("json output");
    assert_eq!(full_access_payload["skill_tools"][0]["status"], "available");
    assert_eq!(
        full_access_payload["skill_tools"][0]["access_profile_behavior"],
        "full_access_skips_regular_risk_blocks"
    );
    assert_eq!(
        full_access_payload["skill_tools"][0]["approval_requirement"],
        "none"
    );
}

#[test]
fn tool_catalog_treats_disabled_mcp_servers_as_configured_not_active() {
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
            instruction_skill_count: 0,
            skill_tools: Vec::new(),
            mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                server_id: "disabled-mcp".to_string(),
                name: "disabled-mcp".to_string(),
                enabled: false,
                connected: true,
                health: "disabled".to_string(),
                tool_count: Some(3),
                error: Some("mcp_connection_failed".to_string()),
            }],
            mcp_tools: Vec::new(),
        })),
        ..ToolRuntimeResources::default()
    };
    let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
    let mcp_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "mcp_servers")
        .expect("mcp dependency should be listed");

    assert_eq!(payload["mcp_server_count"], 1);
    assert_eq!(payload["connected_mcp_server_count"], 0);
    assert_eq!(mcp_dependency["status"], "ready");
    assert_eq!(mcp_dependency["configured_count"], 1);
    assert_eq!(mcp_dependency["enabled_count"], 0);
    assert_eq!(mcp_dependency["ready_count"], 0);
    assert_eq!(mcp_dependency["enabled_tool_count"], 0);
    assert_eq!(mcp_dependency["ready_tool_count"], 0);
    assert_eq!(mcp_dependency["tool_count"], 0);
}

#[test]
fn tool_catalog_counts_only_connected_mcp_tools_as_ready() {
    let resources = ToolRuntimeResources {
        external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
            instruction_skill_count: 0,
            skill_tools: Vec::new(),
            mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                server_id: "disconnected-mcp".to_string(),
                name: "disconnected-mcp".to_string(),
                enabled: true,
                connected: false,
                health: "disconnected".to_string(),
                tool_count: Some(7),
                error: Some("mcp_connection_failed".to_string()),
            }],
            mcp_tools: Vec::new(),
        })),
        ..ToolRuntimeResources::default()
    };
    let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
    let mcp_dependency = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies")
        .iter()
        .find(|dependency| dependency["name"] == "mcp_servers")
        .expect("mcp dependency should be listed");

    assert_eq!(mcp_dependency["status"], "not_ready");
    assert_eq!(mcp_dependency["enabled_count"], 1);
    assert_eq!(mcp_dependency["ready_count"], 0);
    assert_eq!(mcp_dependency["enabled_tool_count"], 7);
    assert_eq!(mcp_dependency["ready_tool_count"], 0);
    assert_eq!(
        mcp_dependency["tool_count"], 0,
        "tool_count must mean currently usable MCP tools, not configured tools on disconnected servers"
    );
}

#[test]
fn tool_catalog_reports_agent_role_registry_health_when_provider_exists() {
    let resources = ToolRuntimeResources {
        agent_role_catalog_provider: Some(Arc::new(|| {
            vec![
                crate::AgentRoleCatalogEntry {
                    role_id: "coordinator".to_string(),
                    display_name: "协调器".to_string(),
                    description: "主线任务编排角色".to_string(),
                    spawnable: false,
                    coordinator_mode: true,
                    supported_kinds: vec!["local_agent".to_string()],
                    capability_ids: vec!["general_engineering".to_string()],
                    parallelism_limit: None,
                    status: "coordinator_only".to_string(),
                    model_binding_status: "inherited_orchestrator".to_string(),
                },
                crate::AgentRoleCatalogEntry {
                    role_id: "executor".to_string(),
                    display_name: "执行器".to_string(),
                    description: "执行具体工作单元".to_string(),
                    spawnable: true,
                    coordinator_mode: false,
                    supported_kinds: vec!["local_agent".to_string()],
                    capability_ids: vec!["general_engineering".to_string()],
                    parallelism_limit: Some(2),
                    status: "spawnable".to_string(),
                    model_binding_status: "unconfigured".to_string(),
                },
            ]
        })),
        ..ToolRuntimeResources::default()
    };

    let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
    let agent_spawn = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "agent_spawn")
        .expect("agent_spawn should be listed");
    let agent_wait = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "agent_wait")
        .expect("agent_wait should be listed");

    assert_eq!(payload["agent_role_catalog_status"], "available");
    assert_eq!(payload["agent_role_count"], 2);
    assert_eq!(payload["spawnable_agent_role_count"], 1);
    assert_eq!(agent_spawn["runtime_status"], "ready");
    assert_eq!(agent_spawn["public"], true);
    assert_eq!(agent_spawn["runtime_internal"], true);
    assert_eq!(agent_spawn["model_call_scope"], "delegator_task_only");
    assert_eq!(agent_wait["runtime_status"], "ready");
    assert_eq!(payload["runtime_dependencies"][2]["status"], "ready");
    assert_eq!(payload["agent_roles"][0]["role_id"], "coordinator");
    assert_eq!(payload["agent_roles"][1]["role_id"], "executor");
    assert_eq!(payload["agent_roles"][1]["parallelism_limit"], 2);
}

#[test]
fn tool_catalog_reports_initialized_knowledge_capabilities_without_workspace_context() {
    let resources = ToolRuntimeResources {
        knowledge_store: Some(std::sync::Arc::new(
            magi_knowledge_store::KnowledgeStore::new(),
        )),
        ..ToolRuntimeResources::default()
    };

    let payload = build_tool_catalog_value("{}", &ToolExecutionContext::default(), &resources);
    let tools = payload["tools"].as_array().expect("tools should be listed");
    for name in [
        "knowledge_query",
        "knowledge_graph_query",
        "search_semantic",
        "code_symbols",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} should be listed"));
        assert_eq!(tool["runtime_status"], "ready");
        assert_eq!(tool["runtime_warnings"], serde_json::json!([]));
    }

    let code_index = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies should be listed")
        .iter()
        .find(|dependency| dependency["name"] == "workspace_code_index")
        .expect("workspace code index dependency should be listed");
    assert_eq!(code_index["status"], "ready");
    assert!(code_index.get("workspace_id").is_none());
    assert!(code_index.get("file_count").is_none());
    assert!(code_index.get("last_indexed").is_none());
    assert!(code_index.get("cache_status").is_none());
}

#[test]
fn tool_catalog_product_health_is_independent_of_workspace_index_state() {
    let resources = ToolRuntimeResources {
        knowledge_store: Some(std::sync::Arc::new(
            magi_knowledge_store::KnowledgeStore::new(),
        )),
        ..ToolRuntimeResources::default()
    };
    let context = ToolExecutionContext {
        workspace_id: Some(magi_core::WorkspaceId::new("empty-workspace")),
        working_directory: Some(std::env::temp_dir().join("missing-magi-workspace")),
        ..ToolExecutionContext::default()
    };

    let payload = build_tool_catalog_value("{}", &context, &resources);
    let search_semantic = payload["tools"]
        .as_array()
        .expect("tools should be listed")
        .iter()
        .find(|tool| tool["name"] == "search_semantic")
        .expect("search_semantic should be listed");
    let code_index = payload["runtime_dependencies"]
        .as_array()
        .expect("runtime dependencies should be listed")
        .iter()
        .find(|dependency| dependency["name"] == "workspace_code_index")
        .expect("workspace code index dependency should be listed");

    assert_eq!(search_semantic["runtime_status"], "ready");
    assert_eq!(code_index["status"], "ready");
    assert!(code_index.get("workspace_id").is_none());
    assert!(code_index.get("file_count").is_none());
}
