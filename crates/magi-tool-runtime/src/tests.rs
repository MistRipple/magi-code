use super::*;
use magi_core::{
    ApprovalRequirement, ExecutionResultStatus, RiskLevel, SessionId, TaskId, ToolCallId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::EventCategory;
use magi_governance::{DecisionPhase, GovernanceOutcome, GovernanceService, ToolKind};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn unique_temp_dir(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{}-{}-{}", name, std::process::id(), suffix));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[cfg(unix)]
fn long_running_shell_command() -> &'static str {
    "sleep 5"
}

#[cfg(windows)]
fn long_running_shell_command() -> &'static str {
    "ping 127.0.0.1 -n 6 >NUL"
}

fn all_builtin_tools() -> [BuiltinToolName; BuiltinToolName::ALL.len()] {
    BuiltinToolName::ALL
}

#[test]
fn external_mcp_model_tool_names_do_not_collapse_distinct_identifiers() {
    let dotted = external_mcp_model_tool_name("repo.tools", "file/read");
    let underscored = external_mcp_model_tool_name("repo_tools", "file_read");

    assert_ne!(dotted, underscored);
    assert!(dotted.len() <= 64);
    assert!(underscored.len() <= 64);
}

#[test]
fn file_read_uses_schema_path_and_directory_listing() {
    let root = unique_temp_dir("magi-tool-file-read");
    let file_path = root.join("hello.txt");
    fs::write(&file_path, "hello\nworld").expect("write file");

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-read"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file_path.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "file_read");
    assert_eq!(payload["access_mode"], "read_only");
    assert_eq!(payload["mode"], "file");
    assert_eq!(payload["truncated"], false);
    assert!(
        payload["content"]
            .as_str()
            .expect("content")
            .contains("hello")
    );

    let dir_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-read-dir"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "path": root.to_string_lossy(),
                "max_bytes": 8
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(dir_output.status, ExecutionResultStatus::Succeeded);
    let dir_payload: Value = serde_json::from_str(&dir_output.payload).expect("dir payload json");
    assert_eq!(dir_payload["mode"], "directory");
    assert_eq!(dir_payload["entries"].as_array().expect("entries").len(), 1);
}

#[test]
fn file_read_accepts_host_path_ref_without_reconstructing_native_path() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let root = std::env::temp_dir().join(format!("magi-tool-path-ref-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("root should create");
    let file = root.join("path-ref.txt");
    std::fs::write(&file, "path-ref-content").expect("file should write");
    let path_ref = magi_core::HostPath::from_path(file)
        .to_path_ref()
        .as_str()
        .to_string();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-read-path-ref"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": path_ref }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["content"], "path-ref-content");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_read_rejects_raw_path_input() {
    let root = unique_temp_dir("magi-tool-file-read-raw-rejected");
    let file_path = root.join("hello.txt");
    fs::write(&file_path, "hello").expect("write file");
    let registry = make_registry();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileRead,
        file_path.to_string_lossy().as_ref(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["tool"], BuiltinToolName::FileRead.as_str());
    assert_eq!(payload["error"], "输入必须为 JSON 对象，包含 path 字段");
}

#[test]
fn file_read_caps_preview_size_and_reports_actual_bytes_read() {
    let root = unique_temp_dir("magi-tool-file-read-cap");
    let file_path = root.join("large.txt");
    fs::write(&file_path, vec![b'x'; 1024 * 1024 + 32]).expect("write large file");
    let registry = make_registry();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileRead,
        &serde_json::json!({
            "path": file_path.to_string_lossy(),
            "max_bytes": 8 * 1024 * 1024,
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["file_size_bytes"], 1024 * 1024 + 32);
    assert_eq!(payload["bytes_read"], 1024 * 1024);
    assert_eq!(payload["max_bytes"], 1024 * 1024);
    assert_eq!(payload["truncated"], true);
    assert_eq!(
        payload["content"].as_str().expect("content").len(),
        1024 * 1024
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn shell_pipe_reader_keeps_bounded_tail_and_reports_truncation() {
    let mut source = vec![b'a'; 1024 * 1024];
    source.extend(vec![b'b'; 32]);

    let output = crate::builtin::read_child_pipe(Some(std::io::Cursor::new(source)));

    assert_eq!(output.bytes.len(), 1024 * 1024);
    assert!(output.truncated);
    assert!(output.bytes.ends_with(&[b'b'; 32]));
}

#[test]
fn search_text_filesystem_failure_uses_public_error_message() {
    let root = unique_temp_dir("magi-tool-search-text-error");
    let missing_path = root.join("missing");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-text-error"),
            tool_name: BuiltinToolName::SearchText.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "query": "needle",
                "root": missing_path.to_string_lossy(),
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "search_text");
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "search_text_failed");
    assert_eq!(payload["error"], "文本搜索暂不可用，请检查路径或权限");
    assert!(
        !output.payload.contains("missing")
            && !output.payload.contains("No such")
            && !output.payload.contains("os error"),
        "搜索工具失败结果不能暴露底层路径或 IO 错误: {}",
        output.payload
    );
}

#[test]
fn builtin_execution_emits_usage_event_and_updates_ledger() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-usage"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": "/tmp/nonexistent" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);

    let snapshot = event_bus.snapshot();
    let usage_events = snapshot
        .recent_events
        .iter()
        .filter(|event| event.category == EventCategory::Usage)
        .collect::<Vec<_>>();
    assert!(!usage_events.is_empty());
    let usage_payload = &usage_events[0].payload;
    assert_eq!(usage_payload["tool_name"], "file_read");
    assert_eq!(usage_payload["status"], "Failed");
    assert_eq!(usage_payload["risk_level"], "Low");

    let ledger_status = event_bus.audit_usage_ledger_status();
    assert!(ledger_status.usage_count >= 1);
    assert_eq!(ledger_status.audit_count, 1);
}

#[test]
fn search_text_supports_json_input() {
    let root = unique_temp_dir("magi-tool-search");
    fs::create_dir_all(root.join("nested")).expect("nested");
    fs::write(root.join("nested").join("one.txt"), "alpha\nneedle\nbeta").expect("write");
    fs::write(root.join("two.txt"), "needle here too").expect("write");

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search"),
            tool_name: BuiltinToolName::SearchText.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "root": root.to_string_lossy(),
                "query": "needle",
                "limit": 10,
                "case_sensitive": true
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "search_text");
    assert_eq!(payload["access_mode"], "read_only");
    assert!(
        payload["returned_matches"]
            .as_u64()
            .expect("returned matches")
            >= 2
    );
    assert!(!payload["matches"].as_array().expect("matches").is_empty());
}

#[test]
fn search_text_supports_cross_platform_regex_queries() {
    let root = unique_temp_dir("magi-tool-search-regex");
    fs::write(
        root.join("runtime.rs"),
        "fn compress_context() {}\nfn truncate_output() {}\n",
    )
    .expect("write runtime.rs");
    let registry = make_registry();

    let output = exec_tool(
        &registry,
        BuiltinToolName::SearchText,
        &serde_json::json!({
            "root": root.to_string_lossy(),
            "query": "compress|truncate",
            "query_mode": "regex",
            "case_sensitive": true,
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["query_mode"], "regex");
    assert_eq!(payload["returned_matches"], 2);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn search_text_rejects_invalid_regex_without_exposing_engine_details() {
    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::SearchText,
        &serde_json::json!({
            "query": "(",
            "query_mode": "regex",
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["error_code"], "search_text_invalid_regex");
    assert_eq!(payload["error"], "正则表达式无效，请检查 query");
    assert!(!output.payload.contains("unclosed"));
}

#[test]
fn shell_exec_runs_and_reports_failure_semantics() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "printf hello" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["access_mode"], "maybe_write");
    assert_eq!(payload["stdout"], "hello");

    let empty_shell_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-empty-shell"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf empty-shell-ok",
                "shell": "",
                "access_mode": "read_only",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(empty_shell_output.status, ExecutionResultStatus::Succeeded);
    let empty_shell_payload: Value =
        serde_json::from_str(&empty_shell_output.payload).expect("payload json");
    assert_eq!(empty_shell_payload["stdout"], "empty-shell-ok");

    let blocked = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-blocked"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "printf blocked" }).to_string(),
            approval_requirement: ApprovalRequirement::Required,
            risk_level: RiskLevel::High,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(blocked.status, ExecutionResultStatus::NeedsApproval);
}

#[test]
fn shell_exec_spawn_failure_uses_public_error_message() {
    let registry = make_registry();
    let missing_shell = "magi-missing-shell-for-public-error-test";

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-spawn-failed"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf hidden",
                "shell": missing_shell,
                "access_mode": "read_only",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "shell_exec_failed");
    assert_eq!(payload["error"], "shell 命令暂不可执行，请检查运行环境");
    assert!(
        !output.payload.contains(missing_shell)
            && !output.payload.contains("No such")
            && !output.payload.contains("os error"),
        "shell_exec 启动失败不能暴露底层运行态细节: {}",
        output.payload
    );
}

#[cfg(unix)]
#[test]
fn shell_exec_reports_missing_executable_in_compound_command() {
    let registry = make_registry();
    let missing = "magi-command-that-does-not-exist";
    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-command-not-found"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": format!("printf before; {missing}"),
                "access_mode": "read_only",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["error_code"], "shell_exec_command_not_found");
    assert!(
        payload["missing_executables"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|command| command == missing))
    );
}

#[cfg(unix)]
#[test]
fn shell_exec_detects_missing_pipeline_dependency_even_when_stderr_is_suppressed() {
    let registry = make_registry();
    let missing = "magi-missing-pipeline-command";
    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-missing-pipeline-command"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": format!("{missing} 2>/dev/null | head -5; printf done"),
                "access_mode": "read_only",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["error_code"], "shell_exec_command_not_found");
    assert!(
        payload["missing_executables"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|command| command == missing))
    );
}

#[test]
fn shell_exec_accepts_shell_program_with_arguments() {
    let registry = make_registry();
    let workspace = unique_temp_dir("magi-shell-prefix-workspace");
    let (shell, command) = if cfg!(windows) {
        ("cmd.exe /C", "echo shell-prefix-ok")
    } else {
        ("sh -lc", "printf shell-prefix-ok")
    };

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-prefix"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": command,
                "shell": shell,
                "cwd": ".",
                "access_mode": "read_only",
                "action": "run",
                "background": false,
                "terminal_id": 0,
                "input": "",
                "max_bytes": 20_000,
                "timeout_ms": 10_000,
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(workspace),
            ..ToolExecutionContext::default()
        },
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(
        payload["stdout"].as_str().map(str::trim),
        Some("shell-prefix-ok")
    );
}

#[test]
fn shell_exec_reports_unavailable_workspace_before_starting_shell() {
    let registry = make_registry();
    let missing_workspace = std::env::temp_dir().join(format!(
        "magi-missing-shell-workspace-{}",
        magi_core::UtcMillis::now().0
    ));

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-missing-workspace"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "echo ok",
                "access_mode": "read_only",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(missing_workspace),
            ..ToolExecutionContext::default()
        },
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(
        payload["error_code"],
        "shell_exec_working_directory_unavailable"
    );
    assert_eq!(payload["error"], "当前工作区目录不可访问，请重新选择工作区");
}

#[test]
fn shell_exec_reclassifies_read_only_mode_with_write_redirection() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let root = unique_temp_dir("magi-tool-shell-read-only-redirection");
    let target = root.join("should-not-exist.txt");

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-read-only-redirection"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": format!("printf hidden > {}", target.display()),
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::NeedsApproval);
    assert!(!target.exists(), "read_only shell 不应执行写入重定向");
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["status"], "needs_approval");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shell_exec_read_only_allows_dev_null_probe_redirection() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-dev-null-redirection"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "if command -v sh >/dev/null 2>&1; then printf dev-null-ok; fi",
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["stdout"], "dev-null-ok");
}

#[test]
fn shell_exec_rejects_writes_to_policy_read_only_paths_in_full_access() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let root = unique_temp_dir("magi-tool-shell-policy-read-only");
    let read_only_dir = root.join("reference");
    fs::create_dir_all(&read_only_dir).expect("create read-only reference dir");
    let target = read_only_dir.join("reference.txt");
    let source = root.join("source.txt");
    fs::write(&target, "ORIGINAL_REFERENCE").expect("write reference fixture");
    fs::write(&source, "SOURCE_CONTENT").expect("write source fixture");
    let policy = ToolExecutionPolicy {
        read_only_paths: vec![read_only_dir.display().to_string()],
        ..full_access_policy()
    };
    let cases = [
        ("redirect", format!("printf changed > {}", target.display())),
        ("append", format!("printf changed >> {}", target.display())),
        ("tee", format!("printf changed | tee {}", target.display())),
        (
            "copy",
            format!("cp {} {}", source.display(), target.display()),
        ),
        (
            "create",
            format!(
                "printf created > {}",
                read_only_dir.join("new.txt").display()
            ),
        ),
    ];

    for (case, command) in cases {
        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tool-call-shell-policy-{case}")),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": command,
                    "access_mode": "maybe_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            },
            &policy,
        );

        assert_eq!(
            output.status,
            ExecutionResultStatus::Rejected,
            "{case} 应在执行前被只读引用策略拒绝: {}",
            output.payload
        );
        assert_eq!(
            fs::read_to_string(&target).expect("read reference fixture"),
            "ORIGINAL_REFERENCE",
            "{case} 不得修改只读引用文件"
        );
        assert!(
            !read_only_dir.join("new.txt").exists(),
            "{case} 不得在只读引用目录创建文件"
        );
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn shell_exec_allows_reading_or_copying_from_policy_read_only_paths() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let root = unique_temp_dir("magi-tool-shell-policy-read-only-source");
    let read_only_dir = root.join("reference");
    let workspace_dir = root.join("workspace");
    fs::create_dir_all(&read_only_dir).expect("create read-only reference dir");
    fs::create_dir_all(&workspace_dir).expect("create workspace dir");
    let source = read_only_dir.join("reference.txt");
    let destination = workspace_dir.join("copied.txt");
    fs::write(&source, "REFERENCE_CONTENT").expect("write reference fixture");
    let policy = ToolExecutionPolicy {
        read_only_paths: vec![read_only_dir.display().to_string()],
        ..full_access_policy()
    };

    let read_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-policy-read-source"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": format!("cat {}", source.display()),
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(workspace_dir.clone()),
            ..ToolExecutionContext::default()
        },
        &policy,
    );
    assert_eq!(read_output.status, ExecutionResultStatus::Succeeded);

    let copy_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-policy-copy-source"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": format!("cp {} {}", source.display(), destination.display()),
                "access_mode": "maybe_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            working_directory: Some(workspace_dir),
            ..ToolExecutionContext::default()
        },
        &policy,
    );
    assert_eq!(copy_output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(destination).expect("read copied file"),
        "REFERENCE_CONTENT"
    );
    assert_eq!(
        fs::read_to_string(source).expect("read original reference"),
        "REFERENCE_CONTENT"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn shell_exec_background_process_can_be_controlled_through_shell_surface() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let root = unique_temp_dir("magi-tool-shell-background-control");
    let context = ToolExecutionContext {
        session_id: Some(SessionId::new("session-shell-background-control")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-background-control")),
        working_directory: Some(root),
        ..ToolExecutionContext::default()
    };

    let launch = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-background-launch"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf ready; sleep 5",
                "background": true
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );

    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
    let launch_payload: Value = serde_json::from_str(&launch.payload).expect("launch json");
    assert_eq!(launch_payload["tool"], "shell_exec");
    assert_eq!(launch_payload["mode"], "background");
    let terminal_id = launch_payload["terminal_id"]
        .as_u64()
        .expect("terminal_id should be returned");

    thread::sleep(Duration::from_millis(100));
    let read = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-background-read"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "action": "read",
                "terminal_id": terminal_id,
                "max_bytes": 1024
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );

    assert_eq!(read.status, ExecutionResultStatus::Succeeded);
    let read_payload: Value = serde_json::from_str(&read.payload).expect("read json");
    assert_eq!(read_payload["tool"], "shell_exec");
    assert_eq!(read_payload["mode"], "background_read");
    assert!(
        read_payload["stdout"]
            .as_str()
            .expect("stdout")
            .contains("ready")
    );

    let list = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-background-list"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "action": "list" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );
    let list_payload: Value = serde_json::from_str(&list.payload).expect("list json");
    assert_eq!(list_payload["mode"], "background_list");
    assert!(
        list_payload["processes"]
            .as_array()
            .expect("processes")
            .iter()
            .any(|process| process["terminal_id"].as_u64() == Some(terminal_id))
    );

    let kill = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-background-kill"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "action": "kill",
                "terminal_id": terminal_id
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );

    assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    let kill_payload: Value = serde_json::from_str(&kill.payload).expect("kill json");
    assert_eq!(kill_payload["tool"], "shell_exec");
    assert_eq!(kill_payload["mode"], "background_kill");
}

#[test]
fn shell_exec_read_only_git_status_in_non_git_workspace_is_stable_probe() {
    let root = unique_temp_dir("magi-tool-shell-non-git-probe");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        session_id: Some(SessionId::new("session-shell-non-git")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-non-git")),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-non-git-status"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "git status --short",
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["git_worktree"], false);
    assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
}

#[test]
fn shell_exec_read_only_compound_git_status_in_non_git_workspace_is_stable_probe() {
    let root = unique_temp_dir("magi-tool-shell-compound-non-git-probe");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        session_id: Some(SessionId::new("session-shell-compound-non-git")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-compound-non-git")),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };
    let command = format!(
        "pwd && printf '\\n---\\n' && ls -1 {} | head -n 3 && printf '\\n---\\n' && git -C {} status --short",
        root.display(),
        root.display()
    );

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-compound-non-git-status"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": command,
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["git_worktree"], false);
    assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
}

#[test]
fn shell_exec_records_git_worktree_changed_paths() {
    let root = unique_temp_dir("magi-tool-shell-change-capture");
    Command::new("git")
        .args(["init"])
        .current_dir(&root)
        .output()
        .expect("git init should run");
    Command::new("git")
        .args(["config", "user.email", "codex@example.com"])
        .current_dir(&root)
        .output()
        .expect("git email config should run");
    Command::new("git")
        .args(["config", "user.name", "Codex"])
        .current_dir(&root)
        .output()
        .expect("git name config should run");
    fs::write(root.join("tracked-a.txt"), "alpha\n").expect("tracked a should write");
    fs::write(root.join("tracked-b.txt"), "beta\n").expect("tracked b should write");
    Command::new("git")
        .args(["add", "--", "tracked-a.txt", "tracked-b.txt"])
        .current_dir(&root)
        .output()
        .expect("git add should run");
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&root)
        .output()
        .expect("git commit should run");

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        session_id: Some(SessionId::new("session-shell-change-capture")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-change-capture")),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-change-capture"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf 'alpha changed\\n' > tracked-a.txt && rm tracked-b.txt && mkdir -p tmp && printf 'new file\\n' > tmp/new-a.txt",
                    "access_mode": "explicit_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    let changed_paths = payload["changed_paths"]
        .as_array()
        .expect("changed paths should be recorded")
        .iter()
        .map(|value| value.as_str().expect("path should be string"))
        .collect::<Vec<_>>();
    assert!(changed_paths.contains(&"tracked-a.txt"));
    assert!(changed_paths.contains(&"tracked-b.txt"));
    assert!(changed_paths.contains(&"tmp/new-a.txt"));
}

#[test]
fn shell_exec_records_non_git_filesystem_changed_paths() {
    let root = unique_temp_dir("magi-tool-shell-non-git-change-capture");
    fs::write(root.join("remove-me.txt"), "remove me\n").expect("seed file should write");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        session_id: Some(SessionId::new("session-shell-non-git-change")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-non-git-change")),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-non-git-change"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf 'new file\\n' > new-a.txt && rm remove-me.txt",
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    let changed_paths = payload["changed_paths"]
        .as_array()
        .expect("changed paths should be recorded")
        .iter()
        .map(|value| value.as_str().expect("path should be string"))
        .collect::<Vec<_>>();
    assert!(changed_paths.contains(&"new-a.txt"));
    assert!(changed_paths.contains(&"remove-me.txt"));
}

#[test]
fn shell_exec_cancel_active_session_kills_running_command() {
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-shell-cancel")),
        session_id: Some(SessionId::new("session-shell-cancel")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-cancel")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let runner_registry = registry.clone();
    let runner_context = context.clone();
    let runner = std::thread::spawn(move || {
        runner_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-cancel"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": long_running_shell_command(),
                    "timeout_ms": 5000
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            runner_context,
            &full_access_policy(),
        )
    });

    std::thread::sleep(Duration::from_millis(100));
    let cancel_started = Instant::now();
    let cancelled = registry.cancel_active_processes(&ToolExecutionContextQuery {
        session_id: context.session_id.clone(),
        workspace_id: context.workspace_id.clone(),
        task_id: None,
        worker_id: None,
    });

    assert_eq!(cancelled, 1);
    let output = runner.join().expect("shell execution thread should join");
    assert!(
        cancel_started.elapsed() < Duration::from_millis(1500),
        "取消 shell_exec 后不应等待 sleep 自然结束"
    );
    assert_eq!(output.status, ExecutionResultStatus::Cancelled);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["cancelled"], true);
}

#[test]
fn shell_exec_cancel_active_scope_requires_matching_workspace() {
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-shell-cancel-workspace-scope")),
        session_id: Some(SessionId::new("session-shell-cancel-workspace-scope")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-cancel-workspace-scope")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let runner_registry = registry.clone();
    let runner_context = context.clone();
    let runner = std::thread::spawn(move || {
        runner_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-cancel-workspace-scope"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": long_running_shell_command(),
                    "timeout_ms": 5000
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            runner_context,
            &full_access_policy(),
        )
    });

    std::thread::sleep(Duration::from_millis(150));
    let wrong_workspace_cancelled = registry.cancel_active_processes(&ToolExecutionContextQuery {
        session_id: context.session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-shell-cancel-other")),
        task_id: context.task_id.clone(),
        worker_id: None,
    });
    assert_eq!(wrong_workspace_cancelled, 0);

    let cancelled = registry.cancel_active_processes(&ToolExecutionContextQuery {
        session_id: context.session_id.clone(),
        workspace_id: context.workspace_id.clone(),
        task_id: context.task_id.clone(),
        worker_id: None,
    });
    assert_eq!(cancelled, 1);

    let output = runner.join().expect("shell execution thread should join");
    assert_eq!(output.status, ExecutionResultStatus::Cancelled);
}

#[test]
fn session_cancellation_stops_background_processes_in_the_same_scope() {
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: Some(WorkerId::new("worker-background-cancel")),
        task_id: Some(TaskId::new("task-background-cancel")),
        session_id: Some(SessionId::new("session-background-cancel")),
        workspace_id: Some(WorkspaceId::new("workspace-background-cancel")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let launch = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-background-cancel"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": long_running_shell_command(),
                "background": true
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );
    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);

    let cancelled = registry.cancel_active_processes(&ToolExecutionContextQuery {
        worker_id: None,
        task_id: context.task_id.clone(),
        session_id: context.session_id.clone(),
        workspace_id: context.workspace_id.clone(),
    });

    assert_eq!(cancelled, 1);
    let list = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-background-list-after-cancel"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "action": "list" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );
    let payload: Value = serde_json::from_str(&list.payload).expect("process list json");
    assert!(
        payload["processes"]
            .as_array()
            .expect("processes")
            .is_empty()
    );
}

#[test]
fn shell_exec_rejects_blank_json_command() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-blank"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "   " }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert!(output.payload.contains("缺少 shell 命令"));
}

#[test]
fn builtin_required_fields_reject_empty_json_objects() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let cases = [
        ("file_read", "缺少 path 字段"),
        ("search_text", "缺少搜索关键词"),
        ("shell_exec", "缺少 shell 命令"),
        ("file_remove", "缺少 path 字段"),
        ("file_mkdir", "缺少 path 字段"),
        ("web_search", "缺少搜索关键词 query"),
        ("web_fetch", "缺少 URL"),
        ("search_semantic", "缺少 query 字段"),
        ("knowledge_query", "缺少 query 字段"),
    ];

    for (tool_name, expected_error) in cases {
        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tool-call-empty-{tool_name}")),
                tool_name: tool_name.to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({}).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(
            output.status,
            ExecutionResultStatus::Failed,
            "{tool_name} should reject empty JSON object"
        );
        assert!(
            output.payload.contains(expected_error),
            "{tool_name} payload should contain {expected_error}, got {}",
            output.payload
        );
    }
}

#[test]
fn builtins_use_context_working_directory_for_relative_inputs() {
    let root = unique_temp_dir("magi-tool-context-cwd");
    fs::write(root.join("marker.txt"), "workspace-marker").expect("write marker");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let shell_command = if cfg!(windows) {
        "if exist marker.txt echo workspace-ok"
    } else {
        "test -f marker.txt && printf workspace-ok"
    };
    let shell_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-context-shell"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": shell_command,
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    let shell_payload: Value =
        serde_json::from_str(&shell_output.payload).expect("shell payload should parse");
    assert_eq!(shell_output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(
        shell_payload["stdout"].as_str().map(str::trim),
        Some("workspace-ok")
    );

    let file_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-context-file-read"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": "marker.txt" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    let file_payload: Value =
        serde_json::from_str(&file_output.payload).expect("file payload should parse");
    assert_eq!(file_output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(
        file_payload["path"],
        root.join("marker.txt").display().to_string()
    );
    assert_eq!(file_payload["content"], "workspace-marker");

    let search_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-context-search"),
            tool_name: BuiltinToolName::SearchText.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "workspace-marker" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &ToolExecutionPolicy::default(),
    );
    let search_payload: Value =
        serde_json::from_str(&search_output.payload).expect("search payload should parse");
    assert_eq!(search_output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(search_payload["root"], root.display().to_string());
    assert_eq!(search_payload["returned_matches"], 1);
}

#[test]
fn shell_exec_blocks_conflicting_write_scope_until_guard_drops() {
    let root = unique_temp_dir("magi-tool-shell-write");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("todo-write")),
        session_id: Some(SessionId::new("session-write")),
        workspace_id: Some(WorkspaceId::new("workspace-write")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let guarded_input = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tool-call-shell-write-guard"),
        tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "command": "printf guarded",
            "cwd": root.to_string_lossy(),
            "access_mode": "explicit_write"
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };

    let write_guard = tool_registry
        .acquire_write_guard(
            &guarded_input,
            &context,
            BuiltinToolAccessMode::ExplicitWrite,
        )
        .expect("guard acquisition")
        .expect("writeful guard");

    let blocked = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-write-blocked"),
            ..guarded_input.clone()
        },
        context.clone(),
        &full_access_policy(),
    );
    assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
    let blocked_payload: Value =
        serde_json::from_str(&blocked.payload).expect("blocked payload json");
    assert_eq!(blocked_payload["tool"], "shell_exec");
    assert_eq!(blocked_payload["access_mode"], "explicit_write");
    assert_eq!(blocked_payload["error_code"], "write_conflict");
    assert!(
        blocked_payload["error"]
            .as_str()
            .expect("blocked error")
            .contains("并发写冲突")
    );
    assert!(blocked_payload.get("write_scope").is_none());
    assert!(blocked_payload.get("conflicting_claim").is_none());
    assert!(!blocked.payload.contains(root.to_string_lossy().as_ref()));
    assert!(
        blocked
            .governance
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains(root.to_string_lossy().as_ref())),
        "内部治理记录应保留冲突诊断"
    );

    drop(write_guard);

    let allowed = tool_registry.execute_with_policy(guarded_input, context, &full_access_policy());
    assert_eq!(allowed.status, ExecutionResultStatus::Succeeded);
}

#[test]
fn write_guard_tracks_file_copy_destination_path() {
    let root = unique_temp_dir("magi-tool-copy-write-guard");
    let source = root.join("source.txt");
    let destination = root.join("target.txt");
    fs::write(&source, "source").expect("source file should write");

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let guarded_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("copy-task")),
        session_id: Some(SessionId::new("session-copy-guard")),
        workspace_id: Some(WorkspaceId::new("workspace-copy-guard")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let blocked_context = ToolExecutionContext {
        task_id: Some(TaskId::new("write-task")),
        ..guarded_context.clone()
    };
    let guarded_input = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tool-call-copy-guard"),
        tool_name: BuiltinToolName::FileCopy.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "source": source.to_string_lossy(),
            "destination": destination.to_string_lossy()
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };

    let write_guard = tool_registry
        .acquire_write_guard(
            &guarded_input,
            &guarded_context,
            BuiltinToolAccessMode::ExplicitWrite,
        )
        .expect("guard acquisition")
        .expect("writeful guard");

    let blocked = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-copy-guard-blocked-write"),
            tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "path": destination.to_string_lossy(),
                "content": "blocked"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        blocked_context,
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
    let blocked_payload: Value =
        serde_json::from_str(&blocked.payload).expect("blocked payload json");
    assert_eq!(blocked_payload["error_code"], "write_conflict");
    assert!(
        blocked_payload["error"]
            .as_str()
            .expect("blocked error")
            .contains("并发写冲突")
    );
    assert!(blocked_payload.get("write_scope").is_none());
    assert!(blocked_payload.get("conflicting_claim").is_none());
    assert!(!destination.exists());

    drop(write_guard);
}

#[test]
fn shell_exec_isolates_write_guards_by_workspace_and_session() {
    let root = unique_temp_dir("magi-tool-shell-workdir");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let guarded_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("todo-a")),
        session_id: Some(SessionId::new("session-a")),
        workspace_id: Some(WorkspaceId::new("workspace-a")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let other_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("todo-b")),
        session_id: Some(SessionId::new("session-b")),
        workspace_id: Some(WorkspaceId::new("workspace-b")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let guarded_input = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tool-call-shell-workdir-guard"),
        tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "command": "printf guarded",
            "cwd": root.to_string_lossy(),
            "access_mode": "maybe_write"
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };

    let write_guard = tool_registry
        .acquire_write_guard(
            &guarded_input,
            &guarded_context,
            BuiltinToolAccessMode::MaybeWrite,
        )
        .expect("guard acquisition")
        .expect("writeful guard");

    let allowed_other_context = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-workdir-other-context"),
            ..guarded_input.clone()
        },
        other_context,
        &full_access_policy(),
    );
    assert_eq!(
        allowed_other_context.status,
        ExecutionResultStatus::Succeeded
    );

    let blocked = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-workdir-blocked"),
            ..guarded_input.clone()
        },
        guarded_context,
        &full_access_policy(),
    );
    assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
    let blocked_payload: Value =
        serde_json::from_str(&blocked.payload).expect("blocked payload json");
    assert_eq!(blocked_payload["access_mode"], "maybe_write");
    assert_eq!(blocked_payload["error_code"], "write_conflict");
    assert!(
        blocked_payload["error"]
            .as_str()
            .expect("blocked error")
            .contains("并发写冲突")
    );
    assert!(blocked_payload.get("write_scope").is_none());
    assert!(blocked_payload.get("conflicting_claim").is_none());

    drop(write_guard);
}

#[test]
fn process_inspect_reports_current_process() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let current_pid = std::process::id();
    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process"),
            tool_name: BuiltinToolName::ProcessInspect.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "pid": current_pid }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "process_inspect");
    assert_eq!(payload["access_mode"], "read_only");
    assert!(
        payload["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .any(|item| {
                item["pid"]
                    .as_u64()
                    .map(|pid| pid as u32 == current_pid)
                    .unwrap_or(false)
            })
    );
}

#[test]
fn process_launch_does_not_block_followup_shell_in_same_session() {
    let root = unique_temp_dir("magi-tool-process-launch");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-launch")),
        session_id: Some(SessionId::new("session-process-launch")),
        workspace_id: Some(WorkspaceId::new("workspace-process-launch")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    let launch = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-launch"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "sleep 2",
                "cwd": root.to_string_lossy(),
                "access_mode": "maybe_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );
    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
    let launch_payload: Value = serde_json::from_str(&launch.payload).expect("launch payload json");
    let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

    let started = Instant::now();
    let followup = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-followup-shell"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf followup",
                "cwd": root.to_string_lossy(),
                "access_mode": "maybe_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );

    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "后台进程不应阻塞后续 shell"
    );
    assert_eq!(followup.status, ExecutionResultStatus::Succeeded);
    let followup_payload: Value =
        serde_json::from_str(&followup.payload).expect("followup payload json");
    assert_eq!(followup_payload["stdout"], "followup");

    let kill = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-kill"),
            tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );
    assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
}

#[test]
fn process_launch_rejects_blank_json_command() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-blank"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "   " }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert!(output.payload.contains("缺少 shell 命令"));
}

#[test]
fn process_launch_spawn_failure_uses_public_error_message() {
    let root = unique_temp_dir("magi-tool-process-spawn-error");
    let registry = make_registry();
    let missing_shell = "magi-missing-process-shell-for-public-error-test";
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-spawn-error")),
        session_id: Some(SessionId::new("session-process-spawn-error")),
        workspace_id: Some(WorkspaceId::new("workspace-process-spawn-error")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: Some(root.clone()),
    };

    let output = registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-spawn-error"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf hidden",
                "shell": missing_shell,
                "cwd": root.to_string_lossy(),
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::ProcessLaunch.as_str());
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "process_launch_failed");
    assert_eq!(payload["error"], "后台进程暂不可启动，请检查运行环境");
    assert!(
        !output.payload.contains(missing_shell)
            && !output.payload.contains("No such")
            && !output.payload.contains("os error"),
        "process_launch 启动失败不能暴露底层运行态细节: {}",
        output.payload
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn process_write_failure_uses_public_error_message() {
    let root = unique_temp_dir("magi-tool-process-write-error");
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-write-error")),
        session_id: Some(SessionId::new("session-process-write-error")),
        workspace_id: Some(WorkspaceId::new("workspace-process-write-error")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: Some(root.clone()),
    };

    let launch = registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-write-launch"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "true",
                "cwd": root.to_string_lossy(),
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );
    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
    let launch_payload: Value = serde_json::from_str(&launch.payload).expect("payload json");
    let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

    thread::sleep(Duration::from_millis(100));
    let write = registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-write-error"),
            tool_name: BuiltinToolName::ProcessWrite.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "terminal_id": terminal_id,
                "input": "hidden",
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );

    assert_eq!(write.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&write.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::ProcessWrite.as_str());
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "process_write_failed");
    assert_eq!(payload["error"], "后台进程暂不可写入，请稍后重试");
    assert!(
        !write.payload.contains("Broken pipe")
            && !write.payload.contains("os error")
            && !write.payload.contains("hidden"),
        "process_write 写入失败不能暴露底层运行态细节: {}",
        write.payload
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn process_tools_reject_missing_session_or_workspace_context() {
    let root = unique_temp_dir("magi-tool-process-context");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-launch-no-context"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "sleep 1",
                "cwd": root.to_string_lossy()
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &full_access_policy(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert!(output.payload.contains("需要 session 或 workspace 上下文"));

    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-context")),
        session_id: Some(SessionId::new("session-process-context")),
        workspace_id: Some(WorkspaceId::new("workspace-process-context")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let launch = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-launch-context"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "sleep 2",
                "cwd": root.to_string_lossy()
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &full_access_policy(),
    );
    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
    let launch_payload: Value = serde_json::from_str(&launch.payload).expect("launch payload json");
    let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

    let read_without_context = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-read-no-context"),
            tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(read_without_context.status, ExecutionResultStatus::Failed);
    assert!(
        read_without_context
            .payload
            .contains("需要 session 或 workspace 上下文")
    );

    let kill = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-kill-context"),
            tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );
    assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
}

#[test]
fn process_tools_do_not_cross_sessions_with_workspace_only_context() {
    let root = unique_temp_dir("magi-tool-process-session-scope");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let owner_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-owner")),
        session_id: Some(SessionId::new("session-process-owner")),
        workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let workspace_only_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-workspace-only")),
        session_id: None,
        workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let other_session_context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-process-other")),
        session_id: Some(SessionId::new("session-process-other")),
        workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    let launch = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-launch-owner"),
            tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "sleep 2",
                "cwd": root.to_string_lossy()
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        owner_context.clone(),
        &full_access_policy(),
    );
    assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
    let launch_payload: Value = serde_json::from_str(&launch.payload).expect("launch payload json");
    let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

    let read_workspace_only = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-read-workspace-only"),
            tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        workspace_only_context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(read_workspace_only.status, ExecutionResultStatus::Failed);
    assert!(
        read_workspace_only
            .payload
            .contains("进程不属于当前 session/workspace")
    );

    let read_other_session = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-read-other-session"),
            tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        other_session_context,
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(read_other_session.status, ExecutionResultStatus::Failed);
    assert!(
        read_other_session
            .payload
            .contains("进程不属于当前 session/workspace")
    );

    let process_list = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-list-workspace-only"),
            tool_name: BuiltinToolName::ProcessList.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({}).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        workspace_only_context,
        &ToolExecutionPolicy::default(),
    );
    let list_payload: Value =
        serde_json::from_str(&process_list.payload).expect("list payload json");
    assert_eq!(process_list.status, ExecutionResultStatus::Succeeded);
    assert!(
        list_payload["processes"]
            .as_array()
            .expect("processes should be array")
            .is_empty()
    );

    let kill = tool_registry.execute_internal_builtin_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-process-kill-owner"),
            tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        owner_context,
        &full_access_policy(),
    );
    assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
}

#[test]
fn diff_preview_reports_text_deltas() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-diff"),
            tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "before": "line1\nsame\nold",
                "after": "line1\nsame\nnew"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "diff_preview");
    assert_eq!(payload["access_mode"], "read_only");
    assert!(
        payload["preview"]
            .as_str()
            .expect("preview")
            .contains("+new")
    );
    assert!(
        payload["preview"]
            .as_str()
            .expect("preview")
            .contains("-old")
    );
}

#[test]
fn diff_preview_prefers_inline_text_when_path_labels_are_present() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-diff-inline-first"),
            tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "before": "alpha\nbeta",
                "after": "alpha\nBETA",
                "before_path": "before",
                "after_path": "after"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], "diff_preview");
    assert!(
        payload["preview"]
            .as_str()
            .expect("preview")
            .contains("+BETA")
    );
}

#[test]
fn diff_preview_resolves_relative_paths_from_tool_working_directory() {
    let root = unique_temp_dir("magi-tool-diff-preview-relative");
    fs::write(root.join("before.txt"), "alpha\nold\n").expect("write before");
    fs::write(root.join("after.txt"), "alpha\nnew\n").expect("write after");
    let registry = make_registry();
    let context = ToolExecutionContext {
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-diff-relative"),
            BuiltinToolName::DiffPreview.as_str(),
            serde_json::json!({
                "before_path": "before.txt",
                "after_path": "after.txt",
            })
            .to_string(),
        ),
        context,
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert!(
        payload["preview"]
            .as_str()
            .is_some_and(|value| { value.contains("-old") && value.contains("+new") })
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn diff_preview_source_read_failure_uses_public_error_message() {
    let root = unique_temp_dir("magi-tool-diff-preview-error");
    let missing_path = root.join("missing-before.txt");
    let registry = make_registry();

    let output = exec_tool(
        &registry,
        BuiltinToolName::DiffPreview,
        &serde_json::json!({
            "before_path": missing_path.to_string_lossy(),
            "after": "after",
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::DiffPreview.as_str());
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "diff_preview_failed");
    assert_eq!(payload["error"], "差异预览源暂不可读取，请检查路径或权限");
    assert!(
        !output.payload.contains("missing-before")
            && !output.payload.contains("No such")
            && !output.payload.contains("os error"),
        "diff_preview 源读取失败不能暴露路径或 IO 细节: {}",
        output.payload
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn builtin_invocation_emits_usage_event_and_updates_ledger() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let missing_path = unique_temp_dir("magi-tool-usage").join("missing.txt");

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-usage"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": missing_path.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let status = event_bus.audit_usage_ledger_status();
    assert_eq!(status.audit_count, 1);
    assert_eq!(status.usage_count, 1);
    let snapshot = event_bus.audit_usage_ledger_snapshot();
    assert_eq!(snapshot.usage_entries.len(), 1);
    assert_eq!(snapshot.usage_entries[0].event_type, "tool.usage.recorded");
    let usage_payload = snapshot.usage_entries[0].payload.clone();
    assert_eq!(usage_payload["tool_name"], "file_read");
    assert_eq!(usage_payload["status"], "Failed");
    assert_eq!(usage_payload["risk_level"], "Low");
}

// ── T-204: governance / summary / usage 三者一致性验证 ──────────────────

#[test]
fn governance_blocked_invocations_appear_in_summary_and_events() {
    // ShellExec is registered as High risk + Required approval, so default
    // GovernanceService (auto_allow_max_risk=Medium) will block it.
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();

    // 1) Successful invocation: file.read (Low risk, no approval needed)
    let root = unique_temp_dir("magi-tool-gov-summary");
    let file_path = root.join("ok.txt");
    fs::write(&file_path, "content").expect("write file");

    let ctx = ToolExecutionContext {
        worker_id: Some(WorkerId::new("worker-gov")),
        task_id: Some(TaskId::new("todo-gov")),
        session_id: Some(SessionId::new("session-gov")),
        workspace_id: Some(WorkspaceId::new("workspace-gov")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    let ok_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-gov-ok"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file_path.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(ok_output.governance.outcome, GovernanceOutcome::Allowed);

    // 2) Governance-blocked invocation: shell.exec (High risk)
    let blocked_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-gov-blocked"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "printf blocked" }).to_string(),
            approval_requirement: ApprovalRequirement::Required,
            risk_level: RiskLevel::High,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(blocked_output.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(
        blocked_output.governance.outcome,
        GovernanceOutcome::NeedsApproval
    );

    // 3) Failed invocation: file.read on nonexistent path
    let fail_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-gov-fail"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": root.join("no-such-file.txt").to_string_lossy() })
                .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(fail_output.status, ExecutionResultStatus::Failed);
    assert_eq!(fail_output.governance.outcome, GovernanceOutcome::Allowed);

    // ── Verify summary reflects all three outcomes ──
    let summary = tool_registry.summary();
    assert_eq!(summary.total_invocations, 3);
    assert_eq!(summary.successful_invocations, 1);
    assert_eq!(summary.blocked_invocations, 1);
    assert_eq!(summary.failed_invocations, 1);

    // ── Verify event bus has matching audit + usage events ──
    let snapshot = event_bus.snapshot();
    let audit_events: Vec<_> = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
        .collect();
    assert_eq!(audit_events.len(), 3);

    let usage_events: Vec<_> = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
        .collect();
    assert_eq!(usage_events.len(), 3);

    // Verify the blocked event carries NeedsApproval status
    let blocked_usage = usage_events
        .iter()
        .find(|e| e.payload["tool_call_id"] == "tc-gov-blocked")
        .expect("blocked usage event");
    assert_eq!(blocked_usage.payload["status"], "NeedsApproval");
    assert_eq!(blocked_usage.payload["risk_level"], "High");

    // Verify the successful event carries Succeeded status
    let ok_usage = usage_events
        .iter()
        .find(|e| e.payload["tool_call_id"] == "tc-gov-ok")
        .expect("ok usage event");
    assert_eq!(ok_usage.payload["status"], "Succeeded");
}

#[test]
fn path_level_write_protection_detects_overlapping_paths() {
    let root = unique_temp_dir("magi-tool-path-conflict");
    let shared_file = root.join("shared.txt");
    fs::write(&shared_file, "data").expect("write shared file");

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    // Context A holds a write guard on shared_file via paths
    let ctx_a = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("todo-path-a")),
        session_id: Some(SessionId::new("session-path-a")),
        workspace_id: None,
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let input_a = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tc-path-a"),
        tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "command": "printf writing",
            "path": shared_file.to_string_lossy(),
            "access_mode": "explicit_write"
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };

    let guard = tool_registry
        .acquire_write_guard(&input_a, &ctx_a, BuiltinToolAccessMode::ExplicitWrite)
        .expect("guard acquisition ok")
        .expect("writeful guard");

    // Context B tries to write to the same path — should conflict
    let ctx_b = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("todo-path-b")),
        session_id: Some(SessionId::new("session-path-a")),
        workspace_id: None,
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let input_b = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tc-path-b"),
        tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "command": "printf conflict",
            "path": shared_file.to_string_lossy(),
            "access_mode": "explicit_write"
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };
    let blocked_result =
        tool_registry.acquire_write_guard(&input_b, &ctx_b, BuiltinToolAccessMode::ExplicitWrite);
    assert!(
        blocked_result.is_err(),
        "should be blocked by path-level conflict"
    );
    let err_output = blocked_result.unwrap_err();
    assert_eq!(err_output.status, ExecutionResultStatus::Rejected);
    let err_payload: Value =
        serde_json::from_str(&err_output.payload).expect("conflict payload json");
    assert_eq!(err_payload["error_code"], "write_conflict");
    assert!(
        err_payload["error"]
            .as_str()
            .expect("error")
            .contains("并发写冲突")
    );
    assert!(err_payload.get("write_scope").is_none());
    assert!(err_payload.get("conflicting_claim").is_none());

    // After dropping guard A, context B should succeed
    drop(guard);
    let after_result =
        tool_registry.acquire_write_guard(&input_b, &ctx_b, BuiltinToolAccessMode::ExplicitWrite);
    assert!(after_result.is_ok());
    assert!(after_result.unwrap().is_some());
}

#[test]
fn summary_for_query_filters_by_context_fields() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let root = unique_temp_dir("magi-tool-query-filter");
    let file = root.join("q.txt");
    fs::write(&file, "query").expect("write");

    let ctx_w1 = ToolExecutionContext {
        worker_id: Some(WorkerId::new("w1")),
        task_id: Some(TaskId::new("t1")),
        session_id: Some(SessionId::new("s1")),
        workspace_id: Some(WorkspaceId::new("ws1")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let ctx_w2 = ToolExecutionContext {
        worker_id: Some(WorkerId::new("w2")),
        task_id: Some(TaskId::new("t2")),
        session_id: Some(SessionId::new("s1")),
        workspace_id: Some(WorkspaceId::new("ws1")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    // Execute 2 invocations in context w1
    for i in 0..2 {
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tc-q-w1-{}", i)),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx_w1.clone(),
            &ToolExecutionPolicy::default(),
        );
    }
    // Execute 1 invocation in context w2
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-q-w2-0"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx_w2.clone(),
        &ToolExecutionPolicy::default(),
    );

    // Global summary: 3 total
    let all_summary = tool_registry.summary();
    assert_eq!(all_summary.total_invocations, 3);

    // Query by worker_id=w1: 2
    let w1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
        worker_id: Some(WorkerId::new("w1")),
        ..Default::default()
    });
    assert_eq!(w1_summary.total_invocations, 2);
    assert_eq!(w1_summary.successful_invocations, 2);

    // Query by worker_id=w2: 1
    let w2_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
        worker_id: Some(WorkerId::new("w2")),
        ..Default::default()
    });
    assert_eq!(w2_summary.total_invocations, 1);

    // Query by task_id=t1: 2
    let t1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
        task_id: Some(TaskId::new("t1")),
        ..Default::default()
    });
    assert_eq!(t1_summary.total_invocations, 2);

    // Query by session_id=s1: 3 (shared)
    let s1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
        session_id: Some(SessionId::new("s1")),
        ..Default::default()
    });
    assert_eq!(s1_summary.total_invocations, 3);

    // Query by nonexistent worker: 0
    let none_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
        worker_id: Some(WorkerId::new("w-nope")),
        ..Default::default()
    });
    assert_eq!(none_summary.total_invocations, 0);
}

#[test]
fn policy_rejection_reflected_in_summary_and_events() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
    tool_registry.register_default_builtins();

    let root = unique_temp_dir("magi-tool-policy-reject");
    let file = root.join("p.txt");
    fs::write(&file, "policy").expect("write");

    let ctx = ToolExecutionContext::default();

    // Policy that explicitly denies file.read
    let deny_policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::Restricted,
        source_skill_ids: vec!["skill-x".to_string()],
        allowed_tool_names: vec![
            BuiltinToolName::FileRead.as_str().to_string(),
            BuiltinToolName::SearchText.as_str().to_string(),
        ],
        denied_tool_names: vec![BuiltinToolName::FileRead.as_str().to_string()],
        ..ToolExecutionPolicy::default()
    };

    let denied_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-policy-denied"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &deny_policy,
    );
    assert_eq!(denied_output.status, ExecutionResultStatus::Rejected);
    let denied_payload: Value =
        serde_json::from_str(&denied_output.payload).expect("policy payload json");
    assert_eq!(denied_payload["tool"], BuiltinToolName::FileRead.as_str());
    assert_eq!(denied_payload["status"], "rejected");
    assert_eq!(denied_payload["error_code"], "tool_policy_rejected");
    assert_eq!(denied_payload["error"], "该工具在当前上下文中不可用");
    assert!(!denied_output.payload.contains("skill-x"));
    assert!(!denied_output.payload.contains("skill runtime"));
    assert_eq!(
        denied_output.governance.outcome,
        GovernanceOutcome::Rejected
    );
    assert_eq!(denied_output.governance.phase, DecisionPhase::ToolPolicy);

    // Policy that only allows search.text — file.read is not in allowed list
    let not_allowed_policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::Restricted,
        source_skill_ids: vec!["skill-y".to_string()],
        allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
        denied_tool_names: vec![],
        ..ToolExecutionPolicy::default()
    };

    let not_allowed_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-policy-not-allowed"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &not_allowed_policy,
    );
    assert_eq!(not_allowed_output.status, ExecutionResultStatus::Rejected);
    assert!(!not_allowed_output.payload.contains("skill-y"));
    assert!(!not_allowed_output.payload.contains("skill runtime"));

    // Now do a successful one with default policy
    let ok_output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-policy-ok"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx,
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);

    // ── Summary: 3 total, 1 success, 2 blocked (policy rejections are Rejected status) ──
    let summary = tool_registry.summary();
    assert_eq!(summary.total_invocations, 3);
    assert_eq!(summary.successful_invocations, 1);
    assert_eq!(summary.blocked_invocations, 2);
    assert_eq!(summary.failed_invocations, 0);

    // ── Events must also carry 3 audit + 3 usage entries ──
    let snapshot = event_bus.snapshot();
    let audit_count = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
        .count();
    assert_eq!(audit_count, 3);

    let usage_count = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
        .count();
    assert_eq!(usage_count, 3);

    // Verify ledger counts match
    let ledger = event_bus.audit_usage_ledger_status();
    assert_eq!(ledger.audit_count, 3);
    assert!(ledger.usage_count >= 3);
}

#[test]
fn full_chain_invocations_events_summary_consistent() {
    // Execute a diverse set of operations and verify every accounting surface agrees.
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(64));
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();

    let root = unique_temp_dir("magi-tool-full-chain");
    let file = root.join("chain.txt");
    fs::write(&file, "chain data").expect("write");

    let ctx = ToolExecutionContext {
        worker_id: Some(WorkerId::new("wk-chain")),
        task_id: Some(TaskId::new("td-chain")),
        session_id: Some(SessionId::new("ss-chain")),
        workspace_id: Some(WorkspaceId::new("ws-chain")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    // 1) Successful file read
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("chain-1-ok"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );

    // 2) Successful diff preview
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("chain-2-diff"),
            tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({"before": "a", "after": "b"}).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );

    // 3) Governance-blocked shell exec (high risk)
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("chain-3-blocked"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "rm -rf /" }).to_string(),
            approval_requirement: ApprovalRequirement::Required,
            risk_level: RiskLevel::High,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );

    // 4) Failed file read (nonexistent)
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("chain-4-fail"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": root.join("nonexistent.txt").to_string_lossy() })
                .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy::default(),
    );

    // 5) Policy-rejected
    tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("chain-5-policy"),
            tool_name: BuiltinToolName::FileRead.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ctx.clone(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::Restricted,
            source_skill_ids: vec!["sk-locked".to_string()],
            allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
            denied_tool_names: vec![],
            ..ToolExecutionPolicy::default()
        },
    );

    // ── Source 1: invocations list ──
    let invocations = tool_registry.invocations();
    assert_eq!(invocations.len(), 5, "invocations vec has 5 records");

    // ── Source 2: summary ──
    let summary = tool_registry.summary();
    assert_eq!(summary.total_invocations, 5);
    assert_eq!(summary.successful_invocations, 2); // chain-1, chain-2
    assert_eq!(summary.blocked_invocations, 2); // chain-3 (NeedsApproval), chain-5 (Rejected)
    assert_eq!(summary.failed_invocations, 1); // chain-4

    // ── Source 3: event_bus audit events ──
    let snapshot = event_bus.snapshot();
    let audit_events: Vec<_> = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
        .collect();
    assert_eq!(audit_events.len(), 5, "5 audit events");

    // ── Source 4: event_bus usage events ──
    let usage_events: Vec<_> = snapshot
        .recent_events
        .iter()
        .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
        .collect();
    assert_eq!(usage_events.len(), 5, "5 usage events");

    // ── Cross-check: each invocation has matching audit + usage events ──
    for record in &invocations {
        let call_id = record.tool_call_id.to_string();
        let matching_audit = audit_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == call_id);
        assert!(matching_audit.is_some(), "audit event for {}", call_id);

        let matching_usage = usage_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == call_id);
        assert!(matching_usage.is_some(), "usage event for {}", call_id);

        // Status must agree between invocation record and usage event
        let usage_status = matching_usage.unwrap().payload["status"].as_str().unwrap();
        assert_eq!(
            usage_status,
            format!("{:?}", record.status),
            "status match for {}",
            call_id
        );
    }

    // ── Ledger counts match ──
    let ledger = event_bus.audit_usage_ledger_status();
    assert_eq!(ledger.audit_count, 5);
    assert!(ledger.usage_count >= 5);
}

#[test]
fn full_access_policy_skips_regular_tool_approval() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-full-access-shell"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "command": "printf full-access" }).to_string(),
            approval_requirement: ApprovalRequirement::Required,
            risk_level: RiskLevel::High,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(output.governance.outcome, GovernanceOutcome::Allowed);
}

#[test]
fn registry_applies_policy_access_profile_to_tool_catalog_context() {
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-tool-catalog-full-access"),
            BuiltinToolName::ToolCatalog.as_str(),
            "{}",
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["current_access_profile"], "full_access");
    let shell_exec = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == BuiltinToolName::ShellExec.as_str())
        .expect("shell_exec should be listed");
    assert_eq!(
        shell_exec["effective_approval_policy"],
        "regular_risk_block_skipped"
    );
}

#[test]
fn registry_reports_read_only_command_mode_as_effective_profile() {
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-tool-catalog-read-only-command"),
            BuiltinToolName::ToolCatalog.as_str(),
            "{}",
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            command_mode: "read_only".to_string(),
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["current_access_profile"], "read_only");
    let file_write = payload["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == BuiltinToolName::FileWrite.as_str())
        .expect("file_write should be listed");
    assert_eq!(file_write["effective_approval_policy"], "not_applicable");
}

#[test]
fn registry_enforces_effective_read_only_profile_default_path_scope() {
    let workspace = unique_temp_dir("magi-tool-runtime-effective-profile-workspace");
    let outside = unique_temp_dir("magi-tool-runtime-effective-profile-outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, "outside").expect("write outside fixture");
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-effective-read-only-path"),
            BuiltinToolName::FileRead.as_str(),
            serde_json::json!({
                "path": outside_file.to_string_lossy()
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(workspace),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            command_mode: "read_only".to_string(),
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::FileRead.as_str());
    assert_eq!(payload["access_profile"], "read_only");
}

#[test]
fn registry_does_not_skip_approval_when_command_mode_downgrades_full_access() {
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-effective-read-only-approval"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf approval",
                "access_mode": "read_only"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::Required,
            risk_level: RiskLevel::High,
        },
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            command_mode: "read_only".to_string(),
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(output.governance.outcome, GovernanceOutcome::NeedsApproval);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::ShellExec.as_str());
    assert_eq!(payload["access_profile"], "read_only");
}

#[test]
fn tool_execution_policy_applies_task_policy_without_losing_skill_scope() {
    let mut policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::Restricted,
        source_skill_ids: vec!["skill-a".to_string()],
        allowed_tool_names: vec![
            BuiltinToolName::FileRead.as_str().to_string(),
            BuiltinToolName::SearchText.as_str().to_string(),
        ],
        denied_tool_names: vec![BuiltinToolName::FileRemove.as_str().to_string()],
        ..ToolExecutionPolicy::default()
    };
    let task_policy = test_task_policy(
        magi_core::AccessProfile::FullAccess,
        vec![BuiltinToolName::FileRead.as_str().to_string()],
        vec![BuiltinToolName::ShellExec.as_str().to_string()],
    );

    policy.apply_task_policy(&task_policy);

    assert_eq!(policy.access_profile, magi_core::AccessProfile::FullAccess);
    assert_eq!(
        policy.allowed_tool_names,
        vec![BuiltinToolName::FileRead.as_str().to_string()]
    );
    assert_eq!(
        policy.denied_tool_names,
        vec![
            BuiltinToolName::FileRemove.as_str().to_string(),
            BuiltinToolName::ShellExec.as_str().to_string()
        ]
    );
    assert_eq!(policy.allowed_paths, vec!["/tmp/allowed".to_string()]);
    assert_eq!(policy.denied_paths, vec!["/tmp/denied".to_string()]);
    assert_eq!(policy.command_mode, "read_only");
    assert_eq!(
        policy.effective_access_profile(),
        magi_core::AccessProfile::ReadOnly
    );
}

fn test_task_policy(
    access_profile: magi_core::AccessProfile,
    allowed_tools: Vec<String>,
    denied_tools: Vec<String>,
) -> magi_core::TaskPolicy {
    magi_core::TaskPolicy {
        autonomy_level: "assisted".to_string(),
        access_profile,
        allowed_tools,
        denied_tools,
        allowed_paths: vec!["/tmp/allowed".to_string()],
        denied_paths: vec!["/tmp/denied".to_string()],
        read_only_paths: Vec::new(),
        network_mode: "default".to_string(),
        command_mode: "read_only".to_string(),
        retry_limit: 0,
        validation_profile: None,
        checkpoint_mode: "none".to_string(),
        task_tier: magi_core::TaskTier::ExecutionChain,
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

fn make_registry() -> ToolRegistry {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut r = ToolRegistry::new(governance, event_bus);
    r.register_default_builtins();
    r
}

fn full_access_policy() -> ToolExecutionPolicy {
    ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionPolicy::default()
    }
}

#[test]
fn registry_enforces_read_only_profile_for_write_tools() {
    let root = unique_temp_dir("magi-tool-read-only-profile");
    let registry = make_registry();
    let target = root.join("blocked.txt");

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-read-only-file-write"),
            BuiltinToolName::FileWrite.as_str(),
            serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "blocked"
            })
            .to_string(),
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    assert!(!target.exists());
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::FileWrite.as_str());
    assert_eq!(payload["access_profile"], "read_only");
}

#[test]
fn registry_requires_approval_for_restricted_write_shell() {
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-restricted-shell-write"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "printf write",
                "access_mode": "maybe_write"
            })
            .to_string(),
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::NeedsApproval);
    assert_eq!(output.governance.outcome, GovernanceOutcome::NeedsApproval);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::ShellExec.as_str());
    assert_eq!(payload["access_profile"], "restricted");
}

#[test]
fn registry_allows_restricted_read_only_shell() {
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-restricted-shell-read"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "printf hello",
                "access_mode": "read_only"
            })
            .to_string(),
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["access_mode"], "read_only");
    assert_eq!(payload["stdout"], "hello");
}

#[test]
fn registry_reclassifies_misdeclared_shell_without_blocking_full_access() {
    let root = unique_temp_dir("magi-tool-full-access-shell-reclassification");
    let target = root.join("created.txt");
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-full-access-shell-reclassification"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "touch created.txt",
                "access_mode": "read_only"
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(target.exists());
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["access_mode"], "maybe_write");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn registry_requires_approval_for_misdeclared_shell_in_restricted_access() {
    let root = unique_temp_dir("magi-tool-restricted-shell-reclassification");
    let target = root.join("blocked.txt");
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-restricted-shell-reclassification"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "touch blocked.txt",
                "access_mode": "read_only"
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::NeedsApproval);
    assert!(!target.exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn registry_rejects_process_side_effects_in_read_only_access() {
    let registry = make_registry();

    let output = registry.execute_internal_builtin_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-read-only-process-launch"),
            BuiltinToolName::ProcessLaunch.as_str(),
            serde_json::json!({ "command": "printf blocked" }).to_string(),
        ),
        ToolExecutionContext::default(),
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["access_profile"], "read_only");
}

#[test]
fn registry_rejects_background_shell_declared_read_only_in_read_only_access() {
    let root = unique_temp_dir("magi-tool-read-only-background-shell");
    let target = root.join("must-not-exist.txt");
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-read-only-background-shell"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "touch must-not-exist.txt",
                "access_mode": "read_only",
                "background": true
            })
            .to_string(),
        ),
        ToolExecutionContext {
            session_id: Some(magi_core::SessionId::new("session-read-only-background")),
            workspace_id: Some(magi_core::WorkspaceId::new(
                "workspace-read-only-background",
            )),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    assert!(!target.exists());

    let _ = fs::remove_dir_all(root);
}

fn exec_tool(registry: &ToolRegistry, tool: BuiltinToolName, input: &str) -> ToolExecutionOutput {
    exec_tool_with_context_and_policy(
        registry,
        tool,
        input,
        ToolExecutionContext::default(),
        ToolExecutionPolicy::default(),
    )
}

fn exec_tool_with_context_and_policy(
    registry: &ToolRegistry,
    tool: BuiltinToolName,
    input: &str,
    context: ToolExecutionContext,
    policy: ToolExecutionPolicy,
) -> ToolExecutionOutput {
    registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new(format!("tc-{}", tool.as_str())),
            tool_name: tool.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: input.to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &policy,
    )
}

#[test]
fn registry_rejects_restricted_file_write_outside_workspace_root() {
    let workspace = unique_temp_dir("magi-tool-runtime-policy-workspace");
    let outside = unique_temp_dir("magi-tool-runtime-policy-outside").join("blocked.txt");
    let inside = workspace.join("allowed.txt");
    let registry = make_registry();
    let context = ToolExecutionContext {
        working_directory: Some(workspace.clone()),
        ..ToolExecutionContext::default()
    };
    let policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::Restricted,
        ..ToolExecutionPolicy::default()
    };

    let blocked = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": outside.to_string_lossy(),
            "content": "blocked"
        })
        .to_string(),
        context.clone(),
        policy.clone(),
    );

    assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
    assert!(!outside.exists());
    let blocked_payload: Value =
        serde_json::from_str(&blocked.payload).expect("blocked payload json");
    assert_eq!(blocked_payload["tool"], BuiltinToolName::FileWrite.as_str());
    assert_eq!(
        blocked_payload["error_code"].as_str(),
        Some("tool_policy_rejected")
    );
    assert_eq!(
        blocked_payload["error"].as_str(),
        Some("该工具在当前上下文中不可用")
    );
    assert!(!blocked.payload.contains(outside.to_string_lossy().as_ref()));

    let allowed = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": inside.to_string_lossy(),
            "content": "allowed"
        })
        .to_string(),
        context,
        policy,
    );

    assert_eq!(allowed.status, ExecutionResultStatus::Succeeded);
    assert_eq!(fs::read_to_string(&inside).unwrap(), "allowed");
}

#[test]
fn referenced_external_paths_are_readable_but_never_writable() {
    let workspace = unique_temp_dir("magi-tool-runtime-reference-workspace");
    let external = unique_temp_dir("magi-tool-runtime-reference-external");
    let external_file = external.join("reference.txt");
    fs::write(&external_file, "reference").expect("reference fixture should write");
    let registry = make_registry();
    let context = ToolExecutionContext {
        working_directory: Some(workspace.clone()),
        ..ToolExecutionContext::default()
    };
    let policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::Restricted,
        allowed_paths: vec![
            workspace.display().to_string(),
            external.display().to_string(),
        ],
        read_only_paths: vec![external.display().to_string()],
        ..ToolExecutionPolicy::default()
    };

    let read = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRead,
        &serde_json::json!({ "path": external_file }).to_string(),
        context.clone(),
        policy.clone(),
    );
    assert_eq!(read.status, ExecutionResultStatus::Succeeded);

    let write = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": external_file,
            "content": "mutated"
        })
        .to_string(),
        context,
        policy,
    );
    assert_eq!(write.status, ExecutionResultStatus::Rejected);
    assert_eq!(
        fs::read_to_string(&external_file).expect("reference fixture should remain readable"),
        "reference"
    );
}

#[test]
fn registry_rejects_outside_shell_path_before_approval() {
    let workspace = unique_temp_dir("magi-tool-runtime-shell-workspace");
    let outside = unique_temp_dir("magi-tool-runtime-shell-outside");
    let registry = make_registry();
    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-shell-outside-before-approval"),
            BuiltinToolName::ShellExec.as_str(),
            serde_json::json!({
                "command": "printf outside",
                "cwd": outside.to_string_lossy(),
                "access_mode": "maybe_write"
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(workspace),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::Restricted,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    assert_eq!(output.governance.outcome, GovernanceOutcome::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::ShellExec.as_str());
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some("该工具在当前上下文中不可用")
    );
}

#[test]
fn registry_applies_path_policy_to_code_symbols_path() {
    let workspace = unique_temp_dir("magi-tool-runtime-code-symbols-policy-workspace");
    let outside = unique_temp_dir("magi-tool-runtime-code-symbols-policy-outside");
    let registry = make_registry();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tc-code-symbols-file-path-policy"),
            BuiltinToolName::CodeSymbols.as_str(),
            serde_json::json!({
                "action": "list_file_symbols",
                "path": outside.join("src/auth.rs").to_string_lossy()
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(workspace),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::Restricted,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::CodeSymbols.as_str());
    assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
    assert_eq!(
        payload["error"].as_str(),
        Some("该工具在当前上下文中不可用")
    );
}

#[test]
fn file_write_creates_and_overwrites() {
    let root = unique_temp_dir("magi-tool-file-write");
    let registry = make_registry();
    let file = root.join("new_file.txt");

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "content": "hello world"
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["tool"], BuiltinToolName::FileWrite.as_str());
    assert_eq!(payload["created"], true);
    assert_eq!(payload["overwritten"], false);
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");

    let output2 = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "content": "updated"
        })
        .to_string(),
    );
    assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
    let payload2: Value = serde_json::from_str(&output2.payload).unwrap();
    assert_eq!(payload2["created"], false);
    assert_eq!(payload2["overwritten"], true);
    assert_eq!(fs::read_to_string(&file).unwrap(), "updated");
}

#[test]
fn file_tools_reject_non_schema_path_aliases() {
    let root = unique_temp_dir("magi-tool-file-aliases");
    let registry = make_registry();
    let file = root.join("alias.txt");
    let dir = root.join("alias-dir").join("nested");
    fs::write(&file, "canonical content").expect("write source");

    let write = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "filePath": file.to_string_lossy(),
            "content": "alias content"
        })
        .to_string(),
    );
    assert_eq!(write.status, ExecutionResultStatus::Failed);

    let read = exec_tool(
        &registry,
        BuiltinToolName::FileRead,
        &serde_json::json!({ "filePath": file.to_string_lossy() }).to_string(),
    );
    assert_eq!(read.status, ExecutionResultStatus::Failed);

    let mkdir = exec_tool(
        &registry,
        BuiltinToolName::FileMkdir,
        &serde_json::json!({ "dirPath": dir.to_string_lossy() }).to_string(),
    );
    assert_eq!(mkdir.status, ExecutionResultStatus::Failed);
    assert!(!dir.exists());

    let copied = exec_tool(
        &registry,
        BuiltinToolName::FileCopy,
        &serde_json::json!({
            "sourcePath": file.to_string_lossy(),
            "destinationPath": root.join("alias-copy.txt").to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(copied.status, ExecutionResultStatus::Failed);

    let moved_output = exec_tool(
        &registry,
        BuiltinToolName::FileMove,
        &serde_json::json!({
            "sourcePath": file.to_string_lossy(),
            "destinationPath": root.join("alias-moved.txt").to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(moved_output.status, ExecutionResultStatus::Failed);
    assert_eq!(fs::read_to_string(&file).unwrap(), "canonical content");
}

#[test]
fn file_write_creates_parent_dirs() {
    let root = unique_temp_dir("magi-tool-file-write-mkdir");
    let registry = make_registry();
    let file = root.join("a").join("b").join("c.txt");

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "content": "deep"
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(fs::read_to_string(&file).unwrap(), "deep");
}

#[test]
fn file_write_filesystem_failure_uses_public_message() {
    let root = unique_temp_dir("magi-tool-file-write-public-error");
    let registry = make_registry();
    let occupied = root.join("occupied");
    fs::write(&occupied, "not a directory").unwrap();
    let target = occupied.join("child.txt");

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "content"
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error"], "文件暂不可写入，请检查路径或权限");
    let text = output.payload.to_string();
    assert!(!text.contains("occupied"));
    assert!(!text.contains("Not a directory"));
    assert!(!text.contains("os error"));
}

#[test]
fn file_write_rejects_overwrite_when_disabled() {
    let root = unique_temp_dir("magi-tool-file-write-no-overwrite");
    let registry = make_registry();
    let file = root.join("existing.txt");
    fs::write(&file, "original").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileWrite,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "content": "replaced",
            "overwrite": false
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_write_failed");
    assert_eq!(payload["error"], "目标路径已存在，请确认是否允许覆盖");
    assert!(!output.payload.contains(file.to_string_lossy().as_ref()));
    assert!(!output.payload.contains("overwrite=false"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "original");
}

#[test]
fn file_patch_applies_single_replacement() {
    let root = unique_temp_dir("magi-tool-file-patch");
    let registry = make_registry();
    let file = root.join("patch_me.txt");
    fs::write(&file, "line1\nold_value\nline3").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FilePatch,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "old_string": "old_value",
            "new_string": "new_value"
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["applied"], 1);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "line1\nnew_value\nline3"
    );
}

#[test]
fn file_patch_empty_patches_falls_back_to_old_new_fields() {
    let root = unique_temp_dir("magi-tool-file-patch-empty-array");
    let registry = make_registry();
    let file = root.join("patch_me.txt");
    fs::write(&file, "alpha needle beta").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FilePatch,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "old_string": "needle",
            "new_string": "needle_patched",
            "patches": []
        })
        .to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["applied"], 1);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "alpha needle_patched beta"
    );
}

#[test]
fn file_patch_applies_multiple_patches() {
    let root = unique_temp_dir("magi-tool-file-patch-multi");
    let registry = make_registry();
    let file = root.join("multi.txt");
    fs::write(&file, "aaa\nbbb\nccc").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FilePatch,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "patches": [
                { "old_string": "aaa", "new_string": "AAA" },
                { "old_string": "ccc", "new_string": "CCC" }
            ]
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["applied"], 2);
    assert_eq!(fs::read_to_string(&file).unwrap(), "AAA\nbbb\nCCC");
}

#[test]
fn file_patch_rejects_ambiguous_match() {
    let root = unique_temp_dir("magi-tool-file-patch-ambig");
    let registry = make_registry();
    let file = root.join("dup.txt");
    fs::write(&file, "same\nsame\nother").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FilePatch,
        &serde_json::json!({
            "path": file.to_string_lossy(),
            "old_string": "same",
            "new_string": "replaced"
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert_eq!(fs::read_to_string(&file).unwrap(), "same\nsame\nother");
}

#[test]
fn apply_patch_tool_applies_patch_envelope_through_registry() {
    let root = unique_temp_dir("magi-tool-apply-patch");
    let registry = make_registry();
    fs::write(root.join("existing.txt"), "alpha\nbeta\n").unwrap();

    let input = serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: existing.txt\n@@\n-alpha\n+ALPHA\n beta\n*** End Patch\n"
        })
        .to_string();
    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-apply-patch"),
            tool_name: BuiltinToolName::ApplyPatch.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input,
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Medium,
        },
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["tool"], "apply_patch");
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["operations"], 2);
    assert_eq!(
        fs::read_to_string(root.join("created.txt")).unwrap(),
        "created\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("existing.txt")).unwrap(),
        "ALPHA\nbeta\n"
    );
}

#[test]
fn file_remove_deletes_file_and_directory() {
    let root = unique_temp_dir("magi-tool-file-remove");
    let registry = make_registry();
    let file = root.join("del_me.txt");
    fs::write(&file, "bye").unwrap();

    let output = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRemove,
        &serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
        ToolExecutionContext::default(),
        full_access_policy(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(!file.exists());

    let subdir = root.join("nested");
    fs::create_dir_all(subdir.join("child")).unwrap();
    fs::write(subdir.join("child").join("f.txt"), "x").unwrap();

    let output2 = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRemove,
        &serde_json::json!({ "path": subdir.to_string_lossy(), "recursive": true }).to_string(),
        ToolExecutionContext::default(),
        full_access_policy(),
    );
    assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
    assert!(!subdir.exists());
}

#[test]
fn file_remove_rejects_workspace_root_even_in_full_access() {
    let root = unique_temp_dir("magi-tool-file-remove-protected-root");
    fs::write(root.join("keep.txt"), "keep").unwrap();
    let registry = make_registry();

    let output = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRemove,
        &serde_json::json!({ "path": ".", "recursive": true }).to_string(),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_remove_rejected");
    assert_eq!(payload["error"], "该路径受保护，不能删除");
    assert!(!output.payload.contains(root.to_string_lossy().as_ref()));
    assert!(!output.payload.contains("keep.txt"));
    assert!(root.join("keep.txt").exists());
}

#[test]
fn file_remove_rejects_absolute_working_directory_even_in_full_access() {
    let root = unique_temp_dir("magi-tool-file-remove-protected-absolute");
    fs::write(root.join("keep.txt"), "keep").unwrap();
    let registry = make_registry();

    let output = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRemove,
        &serde_json::json!({ "path": root.to_string_lossy(), "recursive": true }).to_string(),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_remove_rejected");
    assert_eq!(payload["error"], "该路径受保护，不能删除");
    assert!(!output.payload.contains(root.to_string_lossy().as_ref()));
    assert!(!output.payload.contains("keep.txt"));
    assert!(root.join("keep.txt").exists());
}

#[test]
fn file_read_reports_missing_path_without_mislabeling_it_as_permission_failure() {
    let root = unique_temp_dir("magi-tool-file-read-missing");
    let registry = make_registry();

    let output = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRead,
        &serde_json::json!({ "path": "missing.txt" }).to_string(),
        ToolExecutionContext {
            working_directory: Some(root),
            ..ToolExecutionContext::default()
        },
        ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_read_not_found");
    assert_eq!(payload["error"], "目标路径不存在，请检查路径");
}

#[test]
fn file_remove_rejects_filesystem_root_even_in_full_access() {
    let registry = make_registry();

    let output = exec_tool_with_context_and_policy(
        &registry,
        BuiltinToolName::FileRemove,
        &serde_json::json!({ "path": "/", "recursive": true }).to_string(),
        ToolExecutionContext::default(),
        ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_remove_rejected");
    assert_eq!(payload["error"], "该路径受保护，不能删除");
}

#[test]
fn file_mkdir_creates_nested_dirs() {
    let root = unique_temp_dir("magi-tool-file-mkdir");
    let registry = make_registry();
    let deep = root.join("x").join("y").join("z");

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileMkdir,
        &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(deep.is_dir());

    let output2 = exec_tool(
        &registry,
        BuiltinToolName::FileMkdir,
        &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
    );
    assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output2.payload).unwrap();
    assert_eq!(payload["already_existed"], true);
}

#[test]
fn file_copy_copies_file_and_directory() {
    let root = unique_temp_dir("magi-tool-file-copy");
    let registry = make_registry();

    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "copy me").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileCopy,
        &serde_json::json!({
            "source": src.to_string_lossy(),
            "destination": dst.to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(fs::read_to_string(&dst).unwrap(), "copy me");
    assert!(src.exists());

    let src_dir = root.join("src_dir");
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    fs::write(src_dir.join("sub").join("f.txt"), "nested").unwrap();
    let dst_dir = root.join("dst_dir");

    let output2 = exec_tool(
        &registry,
        BuiltinToolName::FileCopy,
        &serde_json::json!({
            "source": src_dir.to_string_lossy(),
            "destination": dst_dir.to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(dst_dir.join("sub").join("f.txt")).unwrap(),
        "nested"
    );
}

#[test]
fn file_copy_failure_uses_public_error() {
    let root = unique_temp_dir("magi-tool-file-copy-public-error");
    let registry = make_registry();
    let missing = root.join("missing.txt");
    let destination = root.join("destination.txt");
    fs::write(&destination, "existing").unwrap();

    let missing_output = exec_tool(
        &registry,
        BuiltinToolName::FileCopy,
        &serde_json::json!({
            "source": missing.to_string_lossy(),
            "destination": root.join("copy.txt").to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(missing_output.status, ExecutionResultStatus::Failed);
    let missing_payload: Value = serde_json::from_str(&missing_output.payload).unwrap();
    assert_eq!(missing_payload["error_code"], "file_copy_failed");
    assert_eq!(missing_payload["error"], "文件暂不可复制，请检查路径或权限");
    assert!(
        !missing_output
            .payload
            .contains(missing.to_string_lossy().as_ref())
    );

    let exists_output = exec_tool(
        &registry,
        BuiltinToolName::FileCopy,
        &serde_json::json!({
            "source": destination.to_string_lossy(),
            "destination": destination.to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(exists_output.status, ExecutionResultStatus::Failed);
    let exists_payload: Value = serde_json::from_str(&exists_output.payload).unwrap();
    assert_eq!(exists_payload["error_code"], "file_copy_failed");
    assert_eq!(
        exists_payload["error"],
        "目标路径已存在，请确认是否允许覆盖"
    );
    assert!(
        !exists_output
            .payload
            .contains(destination.to_string_lossy().as_ref())
    );
    assert!(!exists_output.payload.contains("overwrite=false"));
}

#[test]
fn file_move_renames_file() {
    let root = unique_temp_dir("magi-tool-file-move");
    let registry = make_registry();

    let src = root.join("old.txt");
    let dst = root.join("new.txt");
    fs::write(&src, "move me").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileMove,
        &serde_json::json!({
            "source": src.to_string_lossy(),
            "destination": dst.to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(!src.exists());
    assert_eq!(fs::read_to_string(&dst).unwrap(), "move me");
}

#[test]
fn file_move_rejects_existing_destination_without_overwrite() {
    let root = unique_temp_dir("magi-tool-file-move-no-overwrite");
    let registry = make_registry();

    let src = root.join("a.txt");
    let dst = root.join("b.txt");
    fs::write(&src, "from").unwrap();
    fs::write(&dst, "existing").unwrap();

    let output = exec_tool(
        &registry,
        BuiltinToolName::FileMove,
        &serde_json::json!({
            "source": src.to_string_lossy(),
            "destination": dst.to_string_lossy()
        })
        .to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["error_code"], "file_move_failed");
    assert_eq!(payload["error"], "目标路径已存在，请确认是否允许覆盖");
    assert!(!output.payload.contains(src.to_string_lossy().as_ref()));
    assert!(!output.payload.contains(dst.to_string_lossy().as_ref()));
    assert!(!output.payload.contains("overwrite=false"));
    assert!(src.exists());
    assert_eq!(fs::read_to_string(&dst).unwrap(), "existing");
}

// ── from_str 映射 + helper 方法 ──

#[test]
fn from_str_handles_all_canonical_names() {
    for tool in all_builtin_tools() {
        assert_eq!(
            BuiltinToolName::from_name(tool.as_str()),
            Some(tool),
            "canonical name {} should resolve",
            tool.as_str()
        );
    }
}

#[test]
fn from_str_rejects_non_canonical_aliases() {
    for alias in [
        "file_view",
        "image_view",
        "file_create",
        "file_edit",
        "file_insert",
        "code_search_regex",
        "code_search_semantic",
        "project_knowledge_query",
        "tool_diagnostics",
        "spawn_agent",
        "todowrite",
        "todo",
        "memory",
        "plan",
        "snapshot",
    ] {
        assert_eq!(BuiltinToolName::from_name(alias), None);
    }
    assert_eq!(BuiltinToolName::from_name("nonexistent_tool"), None);
    assert_eq!(BuiltinToolName::from_name("mermaid_diagram"), None);
}

#[test]
fn from_str_roundtrips_through_as_str() {
    for tool in all_builtin_tools() {
        assert_eq!(
            BuiltinToolName::from_name(tool.as_str()),
            Some(tool),
            "{:?} roundtrip failed",
            tool
        );
    }
}

#[test]
fn canonical_builtin_tool_name_accepts_only_canonical_names() {
    assert_eq!(
        canonical_builtin_tool_name("file_patch"),
        Some("file_patch".to_string())
    );
    assert_eq!(canonical_builtin_tool_name("file_edit"), None);
    assert_eq!(canonical_builtin_tool_name("tool_diagnostics"), None);
    assert_eq!(canonical_builtin_tool_name("unknown_tool"), None);
}

#[test]
fn is_write_operation_identifies_correct_tools() {
    let write_ops = [
        BuiltinToolName::FileWrite,
        BuiltinToolName::FilePatch,
        BuiltinToolName::ApplyPatch,
        BuiltinToolName::FileRemove,
        BuiltinToolName::FileMkdir,
        BuiltinToolName::FileCopy,
        BuiltinToolName::FileMove,
        BuiltinToolName::AgentSpawn,
        BuiltinToolName::CreateGoal,
        BuiltinToolName::UpdateGoal,
        BuiltinToolName::TodoWrite,
        BuiltinToolName::MemoryWrite,
    ];
    let non_write = [
        BuiltinToolName::FileRead,
        BuiltinToolName::ViewImage,
        BuiltinToolName::SearchText,
        BuiltinToolName::ShellExec,
        BuiltinToolName::GetGoal,
        BuiltinToolName::AgentWait,
        BuiltinToolName::WebSearch,
        BuiltinToolName::DiffPreview,
        BuiltinToolName::DiagramRender,
        BuiltinToolName::ToolCatalog,
    ];
    for tool in &write_ops {
        assert!(tool.is_write_operation(), "{:?} should be write", tool);
    }
    for tool in &non_write {
        assert!(!tool.is_write_operation(), "{:?} should not be write", tool);
    }
}

#[test]
fn restricted_profile_write_policy_is_explicitly_classified() {
    for tool in all_builtin_tools() {
        if tool.is_write_operation() {
            assert!(
                tool.restricted_write_profile_policy().is_some(),
                "{tool:?} 是写工具，必须显式声明受限模式策略"
            );
        }
    }
    assert_eq!(
        BuiltinToolName::FileRemove.restricted_write_profile_policy(),
        Some(RestrictedWriteProfilePolicy::AutoAllowed)
    );
    assert_eq!(
        BuiltinToolName::ShellExec.restricted_write_profile_policy(),
        None
    );
    assert_eq!(
        BuiltinToolName::FileRead.restricted_write_profile_policy(),
        None
    );
}

#[test]
fn read_only_access_profile_only_blocks_external_side_effect_operations() {
    for internal in [
        BuiltinToolName::AgentSpawn,
        BuiltinToolName::CreateGoal,
        BuiltinToolName::UpdateGoal,
        BuiltinToolName::TodoWrite,
    ] {
        assert!(internal.is_write_operation());
        assert!(
            !internal.is_access_profile_write_operation(),
            "内部协调状态不应被只读工作区权限屏蔽: {internal:?}"
        );
    }
    for external in [
        BuiltinToolName::FileWrite,
        BuiltinToolName::FileRemove,
        BuiltinToolName::MemoryWrite,
        BuiltinToolName::ProcessLaunch,
        BuiltinToolName::ProcessWrite,
        BuiltinToolName::ProcessKill,
    ] {
        assert!(external.is_access_profile_write_operation());
    }
}

#[test]
fn permission_engine_read_only_tool_axis_matches_builtin_side_effect_classification() {
    let engine = builtin_permission_engine();
    let policy = magi_permissions::PermissionPolicy::default();

    for tool in BuiltinToolName::ALL {
        let request = magi_permissions::PermissionRequest::ToolInvocation {
            tool_name: tool.as_str(),
            is_write_tool: tool.is_access_profile_write_operation(),
        };
        let decision = engine.decide(&request, &policy, magi_core::AccessProfile::ReadOnly);

        assert_eq!(
            decision.is_deny(),
            tool.is_access_profile_write_operation(),
            "只读权限工具分类与内置工具副作用分类不一致: {tool:?}"
        );
    }
}

#[test]
fn builtin_permission_engine_uses_restricted_write_policy() {
    let engine = builtin_permission_engine();
    let policy = magi_permissions::PermissionPolicy::default();

    let patch_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: BuiltinToolName::FilePatch.as_str(),
        is_write_tool: true,
    };
    assert_eq!(
        engine.decide(
            &patch_request,
            &policy,
            magi_core::AccessProfile::Restricted
        ),
        magi_permissions::Decision::Allow
    );

    let memory_write_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: BuiltinToolName::MemoryWrite.as_str(),
        is_write_tool: true,
    };
    assert_eq!(
        engine.decide(
            &memory_write_request,
            &policy,
            magi_core::AccessProfile::Restricted
        ),
        magi_permissions::Decision::Allow
    );
    assert!(matches!(
        engine.decide(
            &memory_write_request,
            &policy,
            magi_core::AccessProfile::ReadOnly
        ),
        magi_permissions::Decision::Deny { .. }
    ));

    let shell_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: BuiltinToolName::ShellExec.as_str(),
        is_write_tool: true,
    };
    assert!(matches!(
        engine.decide(
            &shell_request,
            &policy,
            magi_core::AccessProfile::Restricted
        ),
        magi_permissions::Decision::NeedsApproval { .. }
    ));

    assert!(engine.is_read_only_tool(BuiltinToolName::ToolCatalog.as_str()));
    assert!(!engine.is_read_only_tool("tool_diagnostics"));
}

// ── diagram.render 验证 ──

#[test]
fn diagram_render_schema_guides_mind_maps_to_structured_payload() {
    let schema = BuiltinToolName::DiagramRender.parameters_schema();
    let kind_description = schema["properties"]["kind"]["description"]
        .as_str()
        .unwrap_or_default();
    let source_description = schema["properties"]["source"]["description"]
        .as_str()
        .unwrap_or_default();
    let graph_description = schema["properties"]["graph"]["description"]
        .as_str()
        .unwrap_or_default();

    assert!(kind_description.contains("思维导图"));
    assert!(kind_description.contains("不要使用 Mermaid mindmap"));
    assert!(source_description.contains("不支持 Mermaid mindmap"));
    assert!(graph_description.contains("中心主题"));
}

#[test]
fn diagram_render_recognizes_mermaid_types() {
    let registry = make_registry();
    let valid_codes = [
        ("graph TD\n  A --> B", "flowchart"),
        ("flowchart LR\n  A --> B", "flowchart"),
        (
            "---\nconfig:\n  layout: elk\n---\nflowchart LR\n  A --> B",
            "flowchart",
        ),
        ("sequenceDiagram\n  A->>B: Hello", "sequence"),
        ("classDiagram\n  class A", "class"),
        ("stateDiagram-v2\n  [*] --> S", "state"),
        ("erDiagram\n  A ||--o{ B : has", "er"),
        ("gantt\n  title Plan", "gantt"),
        ("pie\n  title Usage", "pie"),
        ("gitGraph\n  commit", "git"),
        ("timeline\n  2024", "timeline"),
    ];
    for (code, expected_type) in &valid_codes {
        let output = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({ "kind": "mermaid", "source": code, "layout": "elk" }).to_string(),
        );
        assert_eq!(
            output.status,
            ExecutionResultStatus::Succeeded,
            "code: {}",
            code
        );
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "diagram_render");
        assert_eq!(payload["type"], "diagram_render");
        assert_eq!(payload["kind"], "mermaid");
        assert_eq!(payload["layout"], "elk");
        assert_eq!(payload["diagram_type"], *expected_type, "code: {}", code);
    }
}

#[test]
fn diagram_render_accepts_dot_graph_and_flow_kinds() {
    let registry = make_registry();

    let dot = exec_tool(
        &registry,
        BuiltinToolName::DiagramRender,
        &serde_json::json!({
            "kind": "dot",
            "source": "digraph G { A -> B }",
            "title": "DOT"
        })
        .to_string(),
    );
    assert_eq!(dot.status, ExecutionResultStatus::Succeeded);
    let dot_payload: Value = serde_json::from_str(&dot.payload).unwrap();
    assert_eq!(dot_payload["kind"], "dot");
    assert_eq!(dot_payload["diagram_type"], "dot");

    for kind in ["graph", "flow"] {
        let output = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": kind,
                "layout": "cose",
                "graph": {
                    "nodes": [
                        { "id": "a", "label": "A" },
                        { "id": "b", "label": "B" }
                    ],
                    "edges": [
                        { "source": "a", "target": "b", "label": "relates" }
                    ]
                }
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{kind}");
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["kind"], kind);
        assert_eq!(payload["layout"], "cose");
        assert_eq!(payload["interactive"], true);
        assert_eq!(payload["graph"]["nodes"].as_array().unwrap().len(), 2);
    }

    for layout in ["fcose", "cose-bilkent"] {
        let output = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "graph",
                "layout": layout,
                "graph": {
                    "nodes": [
                        { "id": "a", "label": "A" },
                        { "id": "b", "label": "B" }
                    ],
                    "edges": [
                        { "source": "a", "target": "b" }
                    ]
                }
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{layout}");
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["layout"], layout);
    }
}

#[test]
fn diagram_render_rejects_invalid_inputs() {
    let registry = make_registry();
    for input in [
        serde_json::json!({ "kind": "mermaid", "source": "invalid_diagram\n  A --> B" }),
        serde_json::json!({ "kind": "mermaid", "source": "mindmap\n  root\n    child" }),
        serde_json::json!({ "kind": "mermaid", "source": "  " }),
        serde_json::json!({ "kind": "dot", "source": "A -> B" }),
        serde_json::json!({ "kind": "graph", "graph": { "nodes": [] } }),
        serde_json::json!({ "kind": "cytoscape", "graph": { "nodes": [], "edges": [] } }),
    ] {
        let output = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &input.to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed, "{input}");
    }
}

#[test]
fn diagram_render_requires_structured_payload_for_mind_maps() {
    let registry = make_registry();

    let mermaid_mindmap = exec_tool(
        &registry,
        BuiltinToolName::DiagramRender,
        &serde_json::json!({
            "kind": "mermaid",
            "source": "mindmap\n  root((验证自动保存规则))\n    目标\n      确认输出结果"
        })
        .to_string(),
    );
    assert_eq!(mermaid_mindmap.status, ExecutionResultStatus::Failed);
    let failed_payload: Value = serde_json::from_str(&mermaid_mindmap.payload).unwrap();
    assert!(
        failed_payload["error"]
            .as_str()
            .unwrap_or_default()
            .contains("kind=flow 或 kind=graph"),
        "{failed_payload}"
    );

    let flow_mindmap = exec_tool(
        &registry,
        BuiltinToolName::DiagramRender,
        &serde_json::json!({
            "kind": "flow",
            "graph": {
                "nodes": [
                    { "id": "root", "label": "验证自动保存规则" },
                    { "id": "goal", "label": "目标" },
                    { "id": "result", "label": "确认输出结果" }
                ],
                "edges": [
                    { "source": "root", "target": "goal" },
                    { "source": "goal", "target": "result" }
                ]
            }
        })
        .to_string(),
    );
    assert_eq!(flow_mindmap.status, ExecutionResultStatus::Succeeded);
    let flow_payload: Value = serde_json::from_str(&flow_mindmap.payload).unwrap();
    assert_eq!(flow_payload["kind"], "flow");
    assert_eq!(flow_payload["interactive"], true);
}

// ── 实际工具行为验证 ──

#[test]
fn search_semantic_requires_workspace_index() {
    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::SearchSemantic,
        &serde_json::json!({ "query": "test query" }).to_string(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["tool"], "search_semantic");
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error"], "代码索引引擎不可用");
}

#[test]
fn knowledge_query_reads_workspace_knowledge_store() {
    let store = Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-knowledge-query");
    store.upsert(magi_knowledge_store::KnowledgeRecord {
        knowledge_id: "kb-runtime-architecture".to_string(),
        kind: magi_knowledge_store::KnowledgeKind::Learning,
        title: "Runtime architecture".to_string(),
        content: "The runtime architecture keeps knowledge in the governed workspace store."
            .to_string(),
        tags: vec!["runtime".to_string()],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some("memory/runtime.md".to_string()),
        created_at: UtcMillis(100),
        updated_at: UtcMillis(100),
    });
    store.upsert(magi_knowledge_store::KnowledgeRecord {
        knowledge_id: "kb-other-workspace".to_string(),
        kind: magi_knowledge_store::KnowledgeKind::Learning,
        title: "Other workspace".to_string(),
        content: "The same architecture term must not leak across workspaces.".to_string(),
        tags: vec!["runtime".to_string()],
        workspace_id: Some(WorkspaceId::new("workspace-knowledge-query-other")),
        source_ref: Some("memory/other.md".to_string()),
        created_at: UtcMillis(200),
        updated_at: UtcMillis(200),
    });

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    registry.register_default_builtins();

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-knowledge-query"),
            tool_name: BuiltinToolName::KnowledgeQuery.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "query": "runtime architecture",
                "kind": "learning",
                "tags": ["runtime"],
                "limit": 5
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_id.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["tool"], "knowledge_query");
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["workspace_id"], workspace_id.as_str());
    assert_eq!(payload["kind"], "learning");
    assert_eq!(payload["total_matches"], 1);
    assert_eq!(payload["returned_matches"], 1);
    assert_eq!(
        payload["results"][0]["knowledge_id"],
        "kb-runtime-architecture"
    );
    assert_eq!(payload["results"][0]["source_ref"], "memory/runtime.md");
}

#[test]
fn knowledge_query_uses_business_chinese_recall_and_typed_fields() {
    let store = Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-knowledge-query-chinese");
    store.upsert(magi_knowledge_store::KnowledgeRecord {
        knowledge_id: "faq-refresh-token".to_string(),
        kind: magi_knowledge_store::KnowledgeKind::Faq,
        title: "登录失败后如何刷新令牌".to_string(),
        content: "先刷新令牌，再重试原请求。".to_string(),
        tags: vec!["登录".to_string(), "令牌".to_string()],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some("faq:auth".to_string()),
        created_at: UtcMillis(100),
        updated_at: UtcMillis(100),
    });

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    registry.register_default_builtins();

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-knowledge-query-chinese"),
            tool_name: BuiltinToolName::KnowledgeQuery.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "query": "登录失败时怎么刷新令牌",
                "limit": 5
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_id),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).unwrap();
    assert_eq!(payload["returned_matches"], 1);
    assert_eq!(payload["results"][0]["knowledge_id"], "faq-refresh-token");
    assert_eq!(payload["results"][0]["kind"], "faq");
    assert_eq!(payload["results"][0]["source_ref"], "faq:auth");
    assert!(payload["results"][0]["matched_terms"].as_array().is_some());
}

#[test]
fn skill_apply_is_not_registered_as_builtin() {
    let registry = make_registry();
    assert!(registry.builtin_access_mode("skill_apply").is_none());
    assert!(
        registry
            .builtin_specs()
            .iter()
            .all(|spec| spec.name != "skill_apply")
    );
}

#[test]
fn orchestration_tools_are_not_registered_as_builtins() {
    let registry = make_registry();
    for tool_name in [
        "task_split",
        "task_list",
        "task_update",
        "task_claim_next",
        "context_compact",
    ] {
        assert!(
            BuiltinToolName::from_name(tool_name).is_none(),
            "{tool_name}"
        );
        assert!(
            registry.builtin_access_mode(tool_name).is_none(),
            "{tool_name}"
        );
        assert!(
            registry
                .builtin_specs()
                .iter()
                .all(|spec| spec.name != tool_name),
            "{tool_name}"
        );
    }
}

// ── web 工具 access mode ──

#[test]
fn web_tools_are_read_only() {
    let registry = make_registry();
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ViewImage.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::WebSearch.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::WebFetch.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::DiagramRender.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ToolCatalog.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::GetGoal.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
}

#[test]
fn web_fetch_reads_local_http_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("local test server address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept web_fetch request");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = r#"<!doctype html><html><body><main><h1>Smoke Web Fetch</h1><p>alpha beta</p></main></body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write web_fetch response");
    });

    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::WebFetch,
        &serde_json::json!({ "url": url }).to_string(),
    );
    server.join().expect("local web_fetch server should finish");

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::WebFetch.as_str());
    assert_eq!(payload["status"], "succeeded");
    assert!(payload.get("prompt").is_none());
    assert!(
        payload["content"]
            .as_str()
            .expect("content should be string")
            .contains("Smoke Web Fetch")
    );
}

#[test]
fn web_fetch_truncates_multibyte_text_without_panicking() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("local test server address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept web_fetch request");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = "中".repeat(60_000);
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .expect("write web_fetch headers");
        stream
            .write_all(body.as_bytes())
            .expect("write web_fetch body");
    });

    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::WebFetch,
        &serde_json::json!({ "url": url }).to_string(),
    );
    server.join().expect("local web_fetch server should finish");

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["truncated"], true);
    let content = payload["content"].as_str().expect("content should be text");
    assert!(content.starts_with(&"中".repeat(50_000)));
    assert!(content.contains("内容已截断至 50,000 字符"));
}

#[test]
fn web_fetch_network_failure_uses_public_error_message() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind closed local port");
    let url = format!("http://{}", listener.local_addr().expect("local address"));
    drop(listener);

    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::WebFetch,
        &serde_json::json!({ "url": url }).to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::WebFetch.as_str());
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "web_fetch_failed");
    assert_eq!(payload["error"], "网页内容暂不可获取，请稍后重试");
    assert!(
        !output.payload.contains("Connection")
            && !output.payload.contains("refused")
            && !output.payload.contains("tcp")
            && !output.payload.contains("127.0.0.1"),
        "web_fetch 运行态失败不能暴露底层网络细节: {}",
        output.payload
    );
}

#[test]
fn web_fetch_http_status_keeps_actionable_status_code() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("local test server address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept web_fetch request");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = "temporarily unavailable";
        let response = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write web_fetch response");
    });

    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::WebFetch,
        &serde_json::json!({ "url": url }).to_string(),
    );
    server.join().expect("local web_fetch server should finish");

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::WebFetch.as_str());
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error_code"], "web_fetch_failed");
    assert_eq!(payload["error"], "网页返回 HTTP 503");
}

#[test]
#[ignore = "live network smoke for manually verifying Bing-backed web_search"]
fn web_search_live_smoke_returns_json_payload() {
    let registry = make_registry();
    let output = exec_tool(
        &registry,
        BuiltinToolName::WebSearch,
        &serde_json::json!({ "query": "OpenAI" }).to_string(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["tool"], BuiltinToolName::WebSearch.as_str());
    assert_eq!(payload["status"], "succeeded");
    assert!(payload["result_count"].as_u64().unwrap_or_default() > 0);
    let results = payload["results"].as_array().expect("results array");
    assert!(results.iter().all(|result| {
        result["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("http"))
    }));
}

// ── 默认内置工具全覆盖注册验证 ──

#[test]
fn all_default_builtins_are_registered() {
    let registry = make_registry();
    let specs = registry.builtin_specs();
    let all_tools = all_builtin_tools();
    assert_eq!(specs.len(), all_tools.len(), "应注册全部默认内置工具");
    for tool in &all_tools {
        assert!(
            registry.builtin_access_mode(tool.as_str()).is_some(),
            "{:?} should be registered",
            tool
        );
    }
}

#[test]
fn public_builtin_specs_exclude_shell_internal_process_tools() {
    let registry = make_registry();
    let public_specs = registry.public_builtin_specs();
    let public_names: Vec<_> = public_specs.iter().map(|spec| spec.name.as_str()).collect();

    assert_eq!(
        public_names,
        vec![
            "file_read",
            "view_image",
            "file_write",
            "file_patch",
            "apply_patch",
            "file_remove",
            "file_mkdir",
            "file_copy",
            "file_move",
            "search_text",
            "search_semantic",
            "shell_exec",
            "process_inspect",
            "diff_preview",
            "web_search",
            "web_fetch",
            "diagram_render",
            "image_generate",
            "knowledge_query",
            "code_symbols",
            "tool_catalog",
            "get_goal",
            "create_goal",
            "update_goal",
            "agent_spawn",
            "agent_wait",
            "todo_write",
            "memory_write",
        ],
        "public builtin specs must remain the single canonical tool surface"
    );

    assert!(is_public_builtin_tool_surface("shell_exec"));
    assert!(!is_public_builtin_tool_surface("process_launch"));
    assert!(!is_public_builtin_tool_surface("process_read"));
    assert!(!is_public_builtin_tool_surface("process_write"));
    assert!(!is_public_builtin_tool_surface("process_kill"));
    assert!(!is_public_builtin_tool_surface("process_list"));
    assert!(is_public_builtin_tool_surface("process_inspect"));
    assert!(
        public_specs
            .iter()
            .any(|spec| spec.name == BuiltinToolName::ShellExec.as_str())
    );
    for internal_tool in [
        BuiltinToolName::ProcessLaunch,
        BuiltinToolName::ProcessRead,
        BuiltinToolName::ProcessWrite,
        BuiltinToolName::ProcessKill,
        BuiltinToolName::ProcessList,
    ] {
        assert!(
            public_specs
                .iter()
                .all(|spec| spec.name != internal_tool.as_str())
        );
    }
}

#[test]
fn public_object_schemas_always_define_required_array() {
    for tool in BuiltinToolName::ALL {
        if !tool.is_public_tool_surface() {
            continue;
        }
        let schema = tool.parameters_schema();
        if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
            continue;
        }
        assert!(
            schema
                .get("required")
                .is_some_and(serde_json::Value::is_array),
            "{} schema must define required as an array for OpenAI-compatible providers",
            tool.as_str()
        );
    }
}

#[test]
fn builtin_catalog_contracts_are_complete_and_self_consistent() {
    let mut names = std::collections::BTreeSet::new();
    for tool in BuiltinToolName::ALL {
        assert!(
            names.insert(tool.as_str()),
            "工具名必须唯一: {}",
            tool.as_str()
        );
        assert_eq!(BuiltinToolName::from_name(tool.as_str()), Some(tool));
        assert!(!tool.category().trim().is_empty());
        assert!(!tool.description().trim().is_empty());

        let schema = tool.parameters_schema();
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{} 必须使用 object 参数 schema",
            tool.as_str()
        );
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("object schema must define properties");
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("object schema must define required");
        for required_name in required {
            let required_name = required_name
                .as_str()
                .expect("required entries must be property names");
            assert!(
                properties.contains_key(required_name),
                "{} required 字段 {} 未在 properties 中定义",
                tool.as_str(),
                required_name
            );
        }
    }
}

#[test]
fn create_goal_schema_uses_explicit_nullable_budget_contract() {
    let schema = BuiltinToolName::CreateGoal.parameters_schema();
    assert_eq!(
        schema["required"],
        serde_json::json!(["objective", "token_budget"])
    );
    assert_eq!(
        schema["properties"]["token_budget"]["anyOf"],
        serde_json::json!([
            { "type": "integer", "minimum": 16000 },
            { "type": "null" }
        ])
    );
}

#[test]
fn public_tool_schemas_do_not_expose_legacy_argument_aliases() {
    let cases = [
        (
            BuiltinToolName::ShellExec,
            &[
                "script",
                "line",
                "operation",
                "op",
                "long_running",
                "longRunning",
                "write_mode",
                "intent",
                "working_directory",
                "workdir",
                "content",
                "text",
            ][..],
        ),
        (
            BuiltinToolName::ProcessWrite,
            &["terminalId", "id", "content", "text"][..],
        ),
        (
            BuiltinToolName::ProcessInspect,
            &["process_id", "name", "pattern", "max_results"][..],
        ),
        (
            BuiltinToolName::DiffPreview,
            &[
                "left",
                "right",
                "left_path",
                "right_path",
                "left_label",
                "right_label",
            ][..],
        ),
        (
            BuiltinToolName::SearchText,
            &["text", "needle", "path", "workspace", "max_results"][..],
        ),
        (BuiltinToolName::WebSearch, &["q", "search"][..]),
        (BuiltinToolName::WebFetch, &["href", "link"][..]),
        (BuiltinToolName::DiagramRender, &["code"][..]),
    ];

    for (tool, forbidden_fields) in cases {
        let schema = tool.parameters_schema();
        let properties = schema["properties"].as_object().expect("schema properties");
        for field in forbidden_fields {
            assert!(
                !properties.contains_key(*field),
                "{} schema must not expose legacy field {}",
                tool.as_str(),
                field
            );
        }
    }
}

#[test]
fn public_tools_reject_legacy_argument_aliases_at_runtime() {
    let registry = make_registry();
    let failures = [
        (
            BuiltinToolName::SearchText,
            serde_json::json!({ "root": ".", "text": "needle" }),
            "缺少搜索关键词",
        ),
        (
            BuiltinToolName::WebSearch,
            serde_json::json!({ "q": "magi" }),
            "缺少搜索关键词 query",
        ),
        (
            BuiltinToolName::WebFetch,
            serde_json::json!({ "href": "http://127.0.0.1" }),
            "缺少 URL",
        ),
        (
            BuiltinToolName::DiagramRender,
            serde_json::json!({ "kind": "mermaid", "code": "graph TD\nA-->B" }),
            "该图表源码格式需要 source 字段",
        ),
        (
            BuiltinToolName::DiffPreview,
            serde_json::json!({ "left": "a", "right": "b" }),
            "差异预览需要 before/after 文本或路径",
        ),
    ];

    for (tool, input, expected_error) in failures {
        let output = exec_tool(&registry, tool, &input.to_string());
        assert_eq!(output.status, ExecutionResultStatus::Failed, "{tool:?}");
        assert!(
            output.payload.contains(expected_error),
            "{} should reject legacy alias input with '{}', payload: {}",
            tool.as_str(),
            expected_error,
            output.payload
        );
    }

    let process_alias = exec_tool(
        &registry,
        BuiltinToolName::ProcessInspect,
        &serde_json::json!({ "process_id": 1 }).to_string(),
    );
    assert_eq!(process_alias.status, ExecutionResultStatus::Failed);
    assert!(
        process_alias
            .payload
            .contains("process_inspect 只接受 pid/query/limit 字段"),
        "process_inspect should reject process_id alias, payload: {}",
        process_alias.payload
    );
}

#[test]
fn builtin_object_tools_reject_raw_string_inputs_at_runtime() {
    let registry = make_registry();
    let failures = [
        (BuiltinToolName::SearchText, "needle", "缺少搜索关键词"),
        (
            BuiltinToolName::ShellExec,
            "printf hello",
            "缺少 shell 命令",
        ),
        (
            BuiltinToolName::ProcessInspect,
            "12345",
            "输入必须为 JSON 对象",
        ),
        (
            BuiltinToolName::DiffPreview,
            "before\n---\nafter",
            "输入必须为 JSON 对象",
        ),
        (BuiltinToolName::WebSearch, "magi", "缺少搜索关键词 query"),
        (BuiltinToolName::WebFetch, "http://127.0.0.1", "缺少 URL"),
        (
            BuiltinToolName::SearchSemantic,
            "code search",
            "缺少 query 字段",
        ),
        (
            BuiltinToolName::KnowledgeQuery,
            "project memory",
            "缺少 query 字段",
        ),
    ];

    for (tool, input, expected_error) in failures {
        let policy = if tool == BuiltinToolName::ShellExec {
            full_access_policy()
        } else {
            ToolExecutionPolicy::default()
        };
        let output = exec_tool_with_context_and_policy(
            &registry,
            tool,
            input,
            ToolExecutionContext::default(),
            policy,
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed, "{tool:?}");
        assert!(
            output.payload.contains(expected_error),
            "{} should reject raw string input with '{}', payload: {}",
            tool.as_str(),
            expected_error,
            output.payload
        );
    }
}

#[test]
fn shell_exec_schema_warns_against_read_only_temp_writes() {
    let schema = BuiltinToolName::ShellExec.parameters_schema();
    let description = schema["properties"]["access_mode"]["description"]
        .as_str()
        .expect("access_mode description");

    assert!(description.contains("read_only"));
    assert!(description.contains("临时文件"));
    assert!(description.contains("重定向到普通文件"));
    assert!(description.contains("maybe_write"));
    assert!(description.contains("explicit_write"));
}

#[test]
fn search_text_schema_requires_query_and_keeps_root_optional() {
    let schema = BuiltinToolName::SearchText.parameters_schema();

    assert_eq!(schema["required"], serde_json::json!(["query"]));
    let query_description = schema["properties"]["query"]["description"]
        .as_str()
        .expect("query description");
    assert!(query_description.contains("必填"));
    assert!(query_description.contains("非空"));
    assert_eq!(
        schema["properties"]["query_mode"]["enum"],
        serde_json::json!(["literal", "regex"])
    );
}

#[test]
fn shell_schema_forbids_inventing_test_runner_arguments() {
    let description = BuiltinToolName::ShellExec.description();

    assert!(description.contains("package.json"));
    assert!(description.contains("禁止臆造"));
    assert!(description.contains("禁止调用 rg"));
}

#[test]
fn filesystem_schemas_prefer_workspace_relative_or_native_absolute_paths() {
    for tool in [
        BuiltinToolName::FileRead,
        BuiltinToolName::ViewImage,
        BuiltinToolName::FileWrite,
        BuiltinToolName::FilePatch,
        BuiltinToolName::FileRemove,
        BuiltinToolName::FileMkdir,
    ] {
        let schema = tool.parameters_schema();
        let description = schema["properties"]["path"]["description"]
            .as_str()
            .expect("path description");
        assert!(
            description.contains("工作区相对路径"),
            "{tool:?}: {description}"
        );
        assert!(description.contains("当前平台"), "{tool:?}: {description}");
    }

    for tool in [BuiltinToolName::FileCopy, BuiltinToolName::FileMove] {
        let schema = tool.parameters_schema();
        for field in ["source", "destination"] {
            let description = schema["properties"][field]["description"]
                .as_str()
                .expect("copy/move path description");
            assert!(
                description.contains("工作区相对路径"),
                "{tool:?}.{field}: {description}"
            );
            assert!(
                description.contains("当前平台"),
                "{tool:?}.{field}: {description}"
            );
        }
    }
}

#[test]
fn shell_schema_describes_platform_native_dialect() {
    let schema = BuiltinToolName::ShellExec.parameters_schema();
    let command = schema["properties"]["command"]["description"]
        .as_str()
        .expect("command description");
    let shell = schema["properties"]["shell"]["description"]
        .as_str()
        .expect("shell description");
    let access_mode = schema["properties"]["access_mode"]["description"]
        .as_str()
        .expect("access mode description");

    assert!(command.contains("当前平台"));
    assert!(shell.contains("Windows"));
    assert!(shell.contains("Linux"));
    assert!(access_mode.contains("NUL"));
    assert!(access_mode.contains("/dev/null"));
}

#[test]
fn diagram_renderer_names_are_not_builtin_tools() {
    for name in [
        "mermaid_diagram",
        "mermaid",
        "graphviz",
        "dot",
        "cytoscape",
        "svelte_flow",
        "svelte-flow",
    ] {
        assert_eq!(
            BuiltinToolName::from_name(name),
            None,
            "{name} must stay a renderer/kind behind diagram_render, not a builtin tool"
        );
        assert!(
            !is_public_builtin_tool_surface(name),
            "{name} must not be accepted as a public builtin surface"
        );
    }
}

#[test]
fn builtin_invocation_policy_classifies_shell_exec_by_runtime_intent() {
    let read_only = BuiltinToolName::ShellExec.invocation_policy_for_input(
        &serde_json::json!({
            "command": "git status --short",
            "access_mode": "read_only"
        })
        .to_string(),
    );
    assert_eq!(read_only.risk_level, RiskLevel::Low);
    assert_eq!(read_only.approval_requirement, ApprovalRequirement::None);

    let misdeclared_read_only = BuiltinToolName::ShellExec.invocation_policy_for_input(
        &serde_json::json!({
            "command": "printf hidden > out.txt",
            "access_mode": "read_only"
        })
        .to_string(),
    );
    assert_eq!(misdeclared_read_only.risk_level, RiskLevel::Medium);
    assert_eq!(
        misdeclared_read_only.approval_requirement,
        ApprovalRequirement::None
    );

    let background = BuiltinToolName::ShellExec.invocation_policy_for_input(
        &serde_json::json!({
            "command": "cargo check",
            "background": true
        })
        .to_string(),
    );
    assert_eq!(background.risk_level, RiskLevel::Medium);
    assert_eq!(background.approval_requirement, ApprovalRequirement::None);

    let background_read = BuiltinToolName::ShellExec.invocation_policy_for_input(
        &serde_json::json!({
            "action": "read",
            "terminal_id": 1
        })
        .to_string(),
    );
    assert_eq!(background_read.risk_level, RiskLevel::Low);
    assert_eq!(
        background_read.approval_requirement,
        ApprovalRequirement::None
    );

    let non_json_shell = BuiltinToolName::ShellExec.invocation_policy_for_input("cargo test");
    assert_eq!(non_json_shell.risk_level, RiskLevel::Medium);
    assert_eq!(
        non_json_shell.approval_requirement,
        ApprovalRequirement::None
    );
}

#[test]
fn builtin_invocation_policy_requires_approval_for_file_remove() {
    let single_file = BuiltinToolName::FileRemove
        .invocation_policy_for_input(&serde_json::json!({ "path": "tmp.txt" }).to_string());
    assert_eq!(single_file.risk_level, RiskLevel::High);
    assert_eq!(
        single_file.approval_requirement,
        ApprovalRequirement::Required
    );

    let recursive = BuiltinToolName::FileRemove.invocation_policy_for_input(
        &serde_json::json!({ "path": "target/tmp", "recursive": true }).to_string(),
    );
    assert_eq!(recursive.risk_level, RiskLevel::High);
    assert_eq!(
        recursive.approval_requirement,
        ApprovalRequirement::Required
    );
}

#[test]
fn builtin_execution_input_keeps_canonical_name_and_applies_invocation_policy() {
    let file_read = ToolExecutionInput::for_builtin_invocation(
        ToolCallId::new("tool-call-file-read"),
        "file_read",
        "/tmp/example.txt",
    );
    assert_eq!(file_read.tool_name, BuiltinToolName::FileRead.as_str());
    assert_eq!(file_read.risk_level, RiskLevel::Low);
    assert_eq!(file_read.approval_requirement, ApprovalRequirement::None);

    let recursive_remove = ToolExecutionInput::for_builtin_invocation(
        ToolCallId::new("tool-call-file-remove"),
        "file_remove",
        serde_json::json!({ "path": "target/tmp", "recursive": true }).to_string(),
    );
    assert_eq!(
        recursive_remove.tool_name,
        BuiltinToolName::FileRemove.as_str()
    );
    assert_eq!(recursive_remove.risk_level, RiskLevel::High);
    assert_eq!(
        recursive_remove.approval_requirement,
        ApprovalRequirement::Required
    );
}

#[test]
fn registry_rejects_internal_process_tools_as_public_builtin_calls() {
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-internal-process")),
        session_id: Some(SessionId::new("session-internal-process")),
        workspace_id: Some(WorkspaceId::new("workspace-internal-process")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    for (tool_name, input) in [
        (
            BuiltinToolName::ProcessLaunch.as_str(),
            serde_json::json!({ "command": "sleep 1" }),
        ),
        (
            BuiltinToolName::ProcessRead.as_str(),
            serde_json::json!({ "terminal_id": 1 }),
        ),
        (
            BuiltinToolName::ProcessWrite.as_str(),
            serde_json::json!({ "terminal_id": 1, "input": "x" }),
        ),
        (
            BuiltinToolName::ProcessKill.as_str(),
            serde_json::json!({ "terminal_id": 1 }),
        ),
        (BuiltinToolName::ProcessList.as_str(), serde_json::json!({})),
    ] {
        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tool-call-{tool_name}-internal")),
                tool_name: tool_name.to_string(),
                tool_kind: ToolKind::Builtin,
                input: input.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(
            output.status,
            ExecutionResultStatus::Rejected,
            "{tool_name} must be rejected before internal process execution"
        );
        assert!(
            output.payload.contains("shell_exec"),
            "{tool_name} rejection should point callers to shell_exec, got {}",
            output.payload
        );
    }
}

#[test]
fn shell_exec_background_keeps_shell_public_payload() {
    let registry = make_registry();
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-shell-background")),
        session_id: Some(SessionId::new("session-shell-background")),
        workspace_id: Some(WorkspaceId::new("workspace-shell-background")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-background"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf shell-background-ok",
                "background": true
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &full_access_policy(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
    assert_eq!(payload["tool"], "shell_exec");
    assert_eq!(payload["mode"], "background");
    assert!(payload["terminal_id"].as_u64().is_some());
}

#[test]
fn file_write_execution_respects_active_write_guard() {
    let registry = make_registry();
    let root = unique_temp_dir("magi-tool-file-write-guard");
    let file = root.join("guarded.txt");
    let context = ToolExecutionContext {
        worker_id: None,
        task_id: Some(TaskId::new("task-file-write-guard")),
        session_id: Some(SessionId::new("session-file-write-guard")),
        workspace_id: Some(WorkspaceId::new("workspace-file-write-guard")),
        access_profile: magi_core::AccessProfile::Restricted,
        working_directory: None,
    };
    let held_input = ToolExecutionInput {
        tool_call_id: ToolCallId::new("tool-call-file-write-held"),
        tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
        tool_kind: ToolKind::Builtin,
        input: serde_json::json!({
            "path": file.to_string_lossy(),
            "content": "held"
        })
        .to_string(),
        approval_requirement: ApprovalRequirement::None,
        risk_level: RiskLevel::Low,
    };
    let _held_guard = registry
        .acquire_write_guard(&held_input, &context, BuiltinToolAccessMode::ExplicitWrite)
        .expect("held write guard should acquire");

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-write-conflict"),
            tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "conflict"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let payload: Value = serde_json::from_str(&output.payload).expect("conflict payload json");
    assert_eq!(payload["error_code"], "write_conflict");
    assert!(
        payload["error"]
            .as_str()
            .expect("error")
            .contains("并发写冲突"),
        "file_write should be protected by the write guard, got {}",
        output.payload
    );
    assert!(payload.get("write_scope").is_none());
    assert!(payload.get("conflicting_claim").is_none());
}

#[test]
fn builtin_access_mode_reports_write_tools_correctly() {
    let registry = make_registry();
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileRead.as_str()),
        Some(BuiltinToolAccessMode::ReadOnly)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileWrite.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FilePatch.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ApplyPatch.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileRemove.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileMkdir.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileCopy.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::FileMove.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::AgentSpawn.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::CreateGoal.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::UpdateGoal.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::TodoWrite.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::MemoryWrite.as_str()),
        Some(BuiltinToolAccessMode::ExplicitWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ShellExec.as_str()),
        Some(BuiltinToolAccessMode::MaybeWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ProcessWrite.as_str()),
        Some(BuiltinToolAccessMode::MaybeWrite)
    );
    assert_eq!(
        registry.builtin_access_mode(BuiltinToolName::ProcessKill.as_str()),
        Some(BuiltinToolAccessMode::MaybeWrite)
    );
}

#[test]
fn search_semantic_uses_workspace_local_index() {
    // 造一个含已知符号的小仓库，验证 search_semantic 只走工作区本地代码索引。
    let root = unique_temp_dir("magi-tool-search-fuse");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("src/auth.rs"),
        "pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n",
    )
    .expect("write auth.rs");

    // 构建索引并注入 KnowledgeStore。
    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-search-fuse");
    store.build_workspace_index(&workspace_id, &root);

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    tool_registry.register_default_builtins();

    let context = ToolExecutionContext {
        workspace_id: Some(workspace_id),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-fuse"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "query": "authenticate user",
                "max_context_tokens": 512,
                "preferred_scopes": ["src"],
                "prefer_recent_edits": false
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["engine"], "local_search_engine");
    assert_eq!(payload["workspace_id"], "workspace-search-fuse");
    assert_eq!(payload["max_context_tokens"], 512);
    assert_eq!(payload["preferred_scopes"], serde_json::json!(["src"]));
    assert_eq!(payload["prefer_recent_edits"], false);
    assert!(payload["index"]["query_count"].as_u64().is_some());
    let results = payload["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "应有命中结果");
    assert!(results[0]["score"].is_number(), "分数应保持数值类型");
    assert!(
        results.iter().any(|r| r["source"] == "engine"
            && r["path"].as_str().is_some_and(|p| p.contains("auth.rs"))),
        "本地索引应命中 auth.rs，实际: {results:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_semantic_lazily_builds_missing_workspace_index() {
    let root = unique_temp_dir("magi-tool-search-lazy-index");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("src/security.rs"),
        "pub fn require_bearer_token(value: &str) -> bool { !value.is_empty() }\n",
    )
    .expect("write security.rs");

    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-search-lazy-index");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry =
        ToolRegistry::new(governance, event_bus).with_knowledge_store(store.clone());
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-lazy-index"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "bearer token" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_id.clone()),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(store.workspace_index_ready(&workspace_id));
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert!(payload["results"].as_array().is_some_and(|results| {
        results
            .iter()
            .any(|result| result["path"] == "src/security.rs")
    }));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_semantic_returns_empty_success_for_empty_workspace() {
    let root = unique_temp_dir("magi-tool-search-empty-index");
    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-search-empty-index");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry =
        ToolRegistry::new(governance, event_bus).with_knowledge_store(store.clone());
    tool_registry.register_default_builtins();

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-empty-index"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "anything" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_id.clone()),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert!(store.workspace_index_available(&workspace_id));
    assert!(!store.workspace_index_ready(&workspace_id));
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["returned_matches"], 0);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn external_mcp_tool_surface_and_execution_share_one_registry_snapshot() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let registry = ToolRegistry::new(governance, event_bus)
        .with_external_tool_catalog_provider(Arc::new(|| ExternalToolCatalogSnapshot {
            instruction_skill_count: 0,
            mcp_tools: vec![ExternalMcpToolCatalogEntry {
                server_id: "repo-tools".to_string(),
                server_name: "Repository Tools".to_string(),
                model_tool_name: "mcp__repo_tools__inspect".to_string(),
                tool_name: "inspect".to_string(),
                description: "Inspect repository".to_string(),
                read_only: false,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            }],
            ..ExternalToolCatalogSnapshot::default()
        }))
        .with_external_mcp_tool_executor(Arc::new(|server_id, tool_name, arguments| {
            assert_eq!(server_id, "repo-tools");
            assert_eq!(tool_name, "inspect");
            assert_eq!(arguments, r#"{"path":"src"}"#);
            (
                serde_json::json!({ "status": "succeeded", "files": 3 }).to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }));

    let snapshot = registry.external_tool_catalog_snapshot();
    assert_eq!(snapshot.mcp_tools.len(), 1);
    let result = registry
        .execute_external_mcp_tool(
            "mcp__repo_tools__inspect",
            r#"{"path":"src"}"#,
            magi_core::AccessProfile::FullAccess,
        )
        .expect("model-visible MCP tool should execute through the same snapshot");
    assert_eq!(result.1, ExecutionResultStatus::Succeeded);
    assert!(result.0.contains("\"files\":3"));
}

#[test]
fn external_mcp_tool_is_blocked_before_executor_in_read_only_profile() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let execute_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let execute_count_for_executor = execute_count.clone();
    let registry = ToolRegistry::new(governance, event_bus)
        .with_external_tool_catalog_provider(Arc::new(|| ExternalToolCatalogSnapshot {
            mcp_tools: vec![ExternalMcpToolCatalogEntry {
                server_id: "repo-tools".to_string(),
                server_name: "Repository Tools".to_string(),
                model_tool_name: "mcp__repo_tools__inspect".to_string(),
                tool_name: "inspect".to_string(),
                description: "Inspect repository".to_string(),
                read_only: false,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            }],
            ..ExternalToolCatalogSnapshot::default()
        }))
        .with_external_mcp_tool_executor(Arc::new(move |_, _, _| {
            execute_count_for_executor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                serde_json::json!({ "status": "succeeded" }).to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }));

    let result = registry
        .execute_external_mcp_tool(
            "mcp__repo_tools__inspect",
            r#"{"path":"src"}"#,
            magi_core::AccessProfile::ReadOnly,
        )
        .expect("registered MCP tool should return an access rejection");

    assert_eq!(result.1, ExecutionResultStatus::Rejected);
    assert_eq!(
        execute_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "read-only rejection must happen before the external MCP executor runs"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&result.0).expect("blocked MCP payload json");
    assert_eq!(payload["error_code"], "mcp_blocked_in_read_only");
}

#[test]
fn external_mcp_write_tool_requires_full_access_before_executor_runs() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let execute_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let execute_count_for_executor = execute_count.clone();
    let registry = ToolRegistry::new(governance, event_bus)
        .with_external_tool_catalog_provider(Arc::new(|| ExternalToolCatalogSnapshot {
            mcp_tools: vec![ExternalMcpToolCatalogEntry {
                server_id: "repo-tools".to_string(),
                server_name: "Repository Tools".to_string(),
                model_tool_name: "mcp__repo_tools__write".to_string(),
                tool_name: "write".to_string(),
                description: "Write repository".to_string(),
                read_only: false,
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            ..ExternalToolCatalogSnapshot::default()
        }))
        .with_external_mcp_tool_executor(Arc::new(move |_, _, _| {
            execute_count_for_executor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                serde_json::json!({ "status": "succeeded" }).to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }));

    let result = registry
        .execute_external_mcp_tool(
            "mcp__repo_tools__write",
            "{}",
            magi_core::AccessProfile::Restricted,
        )
        .expect("registered MCP tool should return an approval decision");

    assert_eq!(result.1, ExecutionResultStatus::NeedsApproval);
    assert_eq!(
        execute_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "受限访问必须在调用外部 MCP 前阻断未知写副作用"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&result.0).expect("restricted MCP payload json");
    assert_eq!(payload["error_code"], "mcp_requires_full_access");
    assert_eq!(payload["access_profile"], "restricted");
}

#[test]
fn external_mcp_read_only_tool_can_execute_in_read_only_profile() {
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let registry = ToolRegistry::new(governance, event_bus)
        .with_external_tool_catalog_provider(Arc::new(|| ExternalToolCatalogSnapshot {
            mcp_tools: vec![ExternalMcpToolCatalogEntry {
                server_id: "repo-tools".to_string(),
                server_name: "Repository Tools".to_string(),
                model_tool_name: "mcp__repo_tools__inspect".to_string(),
                tool_name: "inspect".to_string(),
                description: "Inspect repository".to_string(),
                read_only: true,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            }],
            ..ExternalToolCatalogSnapshot::default()
        }))
        .with_external_mcp_tool_executor(Arc::new(|_, _, _| {
            (
                serde_json::json!({ "status": "succeeded", "files": 3 }).to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }));

    let result = registry
        .execute_external_mcp_tool(
            "mcp__repo_tools__inspect",
            r#"{"path":"src"}"#,
            magi_core::AccessProfile::ReadOnly,
        )
        .expect("registered read-only MCP tool should execute");

    assert_eq!(result.1, ExecutionResultStatus::Succeeded);
}

#[test]
fn search_semantic_uses_context_workspace_when_multiple_indexes_are_ready() {
    let root_a = unique_temp_dir("magi-tool-search-scope-a");
    let root_b = unique_temp_dir("magi-tool-search-scope-b");
    fs::create_dir_all(root_a.join("src")).expect("create workspace a src");
    fs::create_dir_all(root_b.join("src")).expect("create workspace b src");
    fs::write(
        root_a.join("src/alpha.rs"),
        "pub fn exclusive_alpha_tool_probe() -> bool { true }\n",
    )
    .expect("write workspace a source");
    fs::write(
        root_b.join("src/beta.rs"),
        "pub fn exclusive_beta_tool_probe() -> bool { true }\n",
    )
    .expect("write workspace b source");

    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_a = WorkspaceId::new("workspace-tool-search-scope-a");
    let workspace_b = WorkspaceId::new("workspace-tool-search-scope-b");
    store.build_workspace_index(&workspace_a, &root_a);
    store.build_workspace_index(&workspace_b, &root_b);

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    tool_registry.register_default_builtins();

    let output_from_a = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-scope-a"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "exclusive beta tool probe" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_a),
            working_directory: Some(root_a.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(output_from_a.status, ExecutionResultStatus::Succeeded);
    let payload_from_a: Value =
        serde_json::from_str(&output_from_a.payload).expect("workspace a payload json");
    assert_eq!(
        payload_from_a["workspace_id"],
        "workspace-tool-search-scope-a"
    );
    let results_from_a = payload_from_a["results"]
        .as_array()
        .expect("workspace a results array");
    assert!(
        results_from_a.iter().all(|result| !result["path"]
            .as_str()
            .unwrap_or_default()
            .contains("beta.rs")),
        "workspace a 不应返回 workspace b 的 beta.rs，实际: {results_from_a:?}"
    );

    let output_from_b = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-scope-b"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "exclusive beta tool probe" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            workspace_id: Some(workspace_b),
            working_directory: Some(root_b.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(output_from_b.status, ExecutionResultStatus::Succeeded);
    let payload_from_b: Value =
        serde_json::from_str(&output_from_b.payload).expect("workspace b payload json");
    assert_eq!(
        payload_from_b["workspace_id"],
        "workspace-tool-search-scope-b"
    );
    let results_from_b = payload_from_b["results"]
        .as_array()
        .expect("workspace b results array");
    assert!(
        results_from_b.iter().any(|result| result["path"]
            .as_str()
            .is_some_and(|path| path.contains("beta.rs"))),
        "workspace b 应命中 beta.rs，实际: {results_from_b:?}"
    );

    let _ = fs::remove_dir_all(&root_a);
    let _ = fs::remove_dir_all(&root_b);
}

#[test]
fn search_semantic_does_not_fallback_to_text_scan() {
    let root = unique_temp_dir("magi-tool-search-no-scan");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("src/main.rs"), "pub fn unrelated_code() {}\n").expect("write main.rs");
    fs::write(
        root.join("notes.txt"),
        "only_in_txt_note should not be returned by code index search\n",
    )
    .expect("write notes.txt");

    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-search-no-scan");
    store.build_workspace_index(&workspace_id, &root);

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    tool_registry.register_default_builtins();

    let context = ToolExecutionContext {
        workspace_id: Some(workspace_id),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    let output = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-search-no-scan"),
            tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "query": "only_in_txt_note" }).to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context,
        &ToolExecutionPolicy::default(),
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
    assert_eq!(payload["engine"], "local_search_engine");
    assert_eq!(payload["returned_matches"], 0);
    assert!(
        payload["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "非代码文件不应通过旧文本扫描兜底命中"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn code_symbols_definition_and_file_symbols() {
    let root = unique_temp_dir("magi-tool-code-symbols");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("src/auth.rs"),
        "pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n\
             struct Session { id: u32 }\n",
    )
    .expect("write auth.rs");

    let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-code-symbols");
    store.build_workspace_index(&workspace_id, &root);

    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut tool_registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
    tool_registry.register_default_builtins();

    let context = ToolExecutionContext {
        workspace_id: Some(workspace_id),
        working_directory: Some(root.clone()),
        ..ToolExecutionContext::default()
    };

    // definition：按名查定义
    let def = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-def"),
            tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "action": "definition", "name": "authenticate_user" })
                .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(def.status, ExecutionResultStatus::Succeeded);
    let def_payload: Value = serde_json::from_str(&def.payload).expect("def json");
    let def_results = def_payload["results"].as_array().expect("def results");
    assert!(
        def_results.iter().any(|r| r["name"] == "authenticate_user"
            && r["path"].as_str().is_some_and(|p| p.contains("auth.rs"))),
        "definition 应命中 authenticate_user@auth.rs，实际: {def_results:?}"
    );

    // file_symbols：列文件符号
    let list = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-list"),
            tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({ "action": "file_symbols", "path": "src/auth.rs" })
                .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(list.status, ExecutionResultStatus::Succeeded);
    let list_payload: Value = serde_json::from_str(&list.payload).expect("list json");
    let names: Vec<&str> = list_payload["results"]
        .as_array()
        .expect("list results")
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names.contains(&"authenticate_user"),
        "file_symbols 应含函数，实际: {names:?}"
    );
    assert!(
        names.contains(&"Session"),
        "file_symbols 应含 struct，实际: {names:?}"
    );

    let absolute_list = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-list-absolute"),
            tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "action": "file_symbols",
                "path": root.join("src/auth.rs").to_string_lossy()
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(absolute_list.status, ExecutionResultStatus::Succeeded);
    let absolute_payload: Value =
        serde_json::from_str(&absolute_list.payload).expect("absolute list json");
    assert_eq!(absolute_payload["path"], "src/auth.rs");
    assert_eq!(
        absolute_payload["returned_matches"], list_payload["returned_matches"],
        "工作区内绝对路径必须归一化到符号索引使用的相对路径"
    );

    let action_alias = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-list-alias"),
            tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "action": "list_file_symbols",
                "path": "src/auth.rs"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(action_alias.status, ExecutionResultStatus::Succeeded);
    let alias_payload: Value =
        serde_json::from_str(&action_alias.payload).expect("action alias json");
    assert_eq!(alias_payload["action"], "file_symbols");
    assert_eq!(
        alias_payload["returned_matches"],
        list_payload["returned_matches"]
    );

    let goto = tool_registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-goto-alias"),
            tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "action": "goto_definition",
                "name": "Session"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        context.clone(),
        &ToolExecutionPolicy::default(),
    );
    assert_eq!(goto.status, ExecutionResultStatus::Succeeded);
    let goto_payload: Value = serde_json::from_str(&goto.payload).expect("goto json");
    assert_eq!(goto_payload["action"], "definition");
    assert!(
        goto_payload["results"]
            .as_array()
            .expect("goto results")
            .iter()
            .any(|r| r["name"] == "Session"),
        "goto_definition alias 应命中 Session，实际: {}",
        goto_payload["results"]
    );

    for input in [
        serde_json::json!({ "action": "definition", "query": "Session" }),
        serde_json::json!({ "action": "definition", "symbol": "Session" }),
        serde_json::json!({ "action": "file_symbols", "filePath": "src/auth.rs" }),
        serde_json::json!({ "action": "file_symbols", "file_path": "src/auth.rs" }),
        serde_json::json!({ "action": "file_symbols", "file": "src/auth.rs" }),
    ] {
        let rejected = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-code-symbols-reject-legacy-field"),
                tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: input.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(rejected.status, ExecutionResultStatus::Failed);
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn image_generate_writes_provider_bytes_to_workspace_without_persisting_base64() {
    let root = unique_temp_dir("magi-tool-image-generate");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let executor: ImageGenerationExecutor = Arc::new(|request, execution_context| {
        assert_eq!(request.prompt, "生成一个蓝色方块");
        assert_eq!(request.size, "1024x1024");
        assert_eq!(request.quality.as_deref(), Some("high"));
        assert_eq!(
            execution_context
                .session_id
                .as_ref()
                .map(ToString::to_string),
            Some("session-image-generate".to_string())
        );
        assert_eq!(
            execution_context
                .workspace_id
                .as_ref()
                .map(ToString::to_string),
            Some("workspace-image-generate".to_string())
        );
        assert_eq!(execution_context.call_id, "tool-call-image-generate");
        Ok(GeneratedImageData {
            bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
            media_type: "image/png".to_string(),
            revised_prompt: Some("一个蓝色方块".to_string()),
        })
    });
    let mut registry = ToolRegistry::new(governance, event_bus)
        .with_image_generation_runtime(executor, Arc::new(|| true));
    registry.register_default_builtins();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-image-generate"),
            "image_generate",
            serde_json::json!({
                "prompt": "生成一个蓝色方块",
                "size": "1024x1024",
                "quality": "high",
                "output_path": "generated-images/blue-square.png"
            })
            .to_string(),
        ),
        ToolExecutionContext {
            session_id: Some(magi_core::SessionId::new("session-image-generate")),
            workspace_id: Some(magi_core::WorkspaceId::new("workspace-image-generate")),
            working_directory: Some(root.clone()),
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Succeeded);
    assert_eq!(
        fs::read(root.join("generated-images/blue-square.png")).expect("generated image"),
        b"\x89PNG\r\n\x1a\n"
    );
    let payload: Value = serde_json::from_str(&output.payload).expect("image payload json");
    assert_eq!(payload["tool"], "image_generate");
    assert_eq!(payload["path"], "generated-images/blue-square.png");
    assert_eq!(payload["media_type"], "image/png");
    assert_eq!(payload["bytes"], 8);
    assert_eq!(payload["revised_prompt"], "一个蓝色方块");
    assert!(!output.payload.contains("iVBOR"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn image_generate_is_unavailable_in_read_only_access_profile() {
    let root = unique_temp_dir("magi-tool-image-generate-readonly");
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let executor: ImageGenerationExecutor =
        Arc::new(|_, _| panic!("read-only policy must reject before provider invocation"));
    let mut registry = ToolRegistry::new(governance, event_bus)
        .with_image_generation_runtime(executor, Arc::new(|| true));
    registry.register_default_builtins();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-image-generate-readonly"),
            "image_generate",
            serde_json::json!({ "prompt": "test" }).to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Rejected);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn image_generate_rejects_unconfigured_runtime_before_provider_call() {
    let root = unique_temp_dir("magi-tool-image-generate-unconfigured");
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let executor: ImageGenerationExecutor = {
        let provider_calls = Arc::clone(&provider_calls);
        Arc::new(move |_, _| {
            provider_calls.fetch_add(1, Ordering::SeqCst);
            panic!("unconfigured image runtime must not call provider")
        })
    };
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut registry = ToolRegistry::new(governance, event_bus)
        .with_image_generation_runtime(executor, Arc::new(|| false));
    registry.register_default_builtins();
    let context = ToolExecutionContext {
        working_directory: Some(root.clone()),
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionContext::default()
    };
    let policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionPolicy::default()
    };

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-image-generate-unconfigured"),
            "image_generate",
            serde_json::json!({ "prompt": "test image" }).to_string(),
        ),
        context,
        &policy,
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    let payload: Value = serde_json::from_str(&output.payload).expect("image error payload");
    assert_eq!(payload["error_code"], "image_generate_not_configured");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn image_generate_rejects_workspace_escape_before_calling_provider() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = unique_temp_dir("magi-tool-image-generate-workspace");
    let outside = unique_temp_dir("magi-tool-image-generate-outside").join("escaped.png");
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&provider_calls);
    let executor: ImageGenerationExecutor = Arc::new(move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(GeneratedImageData {
            bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
            media_type: "image/png".to_string(),
            revised_prompt: None,
        })
    });
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut registry = ToolRegistry::new(governance, event_bus)
        .with_image_generation_runtime(executor, Arc::new(|| true));
    registry.register_default_builtins();

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-image-generate-workspace-escape"),
            "image_generate",
            serde_json::json!({
                "prompt": "test",
                "output_path": outside,
            })
            .to_string(),
        ),
        ToolExecutionContext {
            working_directory: Some(root.clone()),
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        },
        &ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        },
    );

    assert_eq!(output.status, ExecutionResultStatus::Failed);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    let payload: Value = serde_json::from_str(&output.payload).expect("image error payload");
    assert_eq!(payload["error_code"], "image_generate_invalid_output_path");

    let _ = fs::remove_dir_all(root);
    if let Some(parent) = outside.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

#[test]
fn image_generate_normalizes_extension_and_avoids_overwriting_existing_image() {
    let root = unique_temp_dir("magi-tool-image-generate-extension");
    fs::write(root.join("poster.png"), b"keep-existing-png").expect("seed existing png");
    let executor: ImageGenerationExecutor = Arc::new(|_, _| {
        Ok(GeneratedImageData {
            bytes: vec![0xff, 0xd8, 0xff, 0xd9],
            media_type: "image/jpeg".to_string(),
            revised_prompt: None,
        })
    });
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
    let mut registry = ToolRegistry::new(governance, event_bus)
        .with_image_generation_runtime(executor, Arc::new(|| true));
    registry.register_default_builtins();
    let context = ToolExecutionContext {
        working_directory: Some(root.clone()),
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionContext::default()
    };
    let policy = ToolExecutionPolicy {
        access_profile: magi_core::AccessProfile::FullAccess,
        ..ToolExecutionPolicy::default()
    };
    let invocation = || {
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-image-generate-extension"),
            "image_generate",
            serde_json::json!({
                "prompt": "poster",
                "output_path": "poster.png",
            })
            .to_string(),
        )
    };

    let first = registry.execute_with_policy(invocation(), context.clone(), &policy);
    assert_eq!(first.status, ExecutionResultStatus::Succeeded);
    let first_payload: Value = serde_json::from_str(&first.payload).expect("first payload");
    assert_eq!(first_payload["path"], "poster.jpg");
    assert_eq!(
        fs::read(root.join("poster.png")).unwrap(),
        b"keep-existing-png"
    );
    assert_eq!(
        fs::read(root.join("poster.jpg")).unwrap(),
        vec![0xff, 0xd8, 0xff, 0xd9]
    );

    let second = registry.execute_with_policy(invocation(), context, &policy);
    assert_eq!(second.status, ExecutionResultStatus::Succeeded);
    let second_payload: Value = serde_json::from_str(&second.payload).expect("second payload");
    assert_eq!(second_payload["path"], "poster-1.jpg");
    assert_eq!(
        fs::read(root.join("poster.jpg")).unwrap(),
        vec![0xff, 0xd8, 0xff, 0xd9]
    );
    assert_eq!(
        fs::read(root.join("poster-1.jpg")).unwrap(),
        vec![0xff, 0xd8, 0xff, 0xd9]
    );

    let _ = fs::remove_dir_all(root);
}
