//! 与真实 Node 进程的端到端验收：握手、注册、钩子裁决、超时取消、崩溃与观察投递。

use magi_extension_host::{
    bus::{BusPolicy, ExtensionBus, HookContext, TraceOutcome, Verdict},
    instance::{LaunchConfig, LaunchError, launch},
    protocol::{HookMode, HookRegistration},
    supervisor::{RestartPolicy, SupervisorState, supervise},
};
use magi_process::ManagedProcessGroup;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// 优先使用 `MAGI_EXTENSION_NODE`（可指向随包的固定版本 Node），否则用 PATH 里的 node。
fn node() -> PathBuf {
    std::env::var_os("MAGI_EXTENSION_NODE")
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"))
        .expect("集成测试需要 Node")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn runtime() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/host.mjs")
}

fn hook(name: &str, mode: HookMode, deadline_ms: Option<u64>) -> HookRegistration {
    HookRegistration {
        name: name.into(),
        mode,
        deadline_ms,
    }
}

struct Env {
    _dir: tempfile::TempDir,
    group: ManagedProcessGroup,
    config: LaunchConfig,
    marker: PathBuf,
}

fn env(fixture: &str, hooks: Vec<HookRegistration>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.log");
    let mut config = LaunchConfig::new(
        node(),
        runtime(),
        fixtures().join(fixture).join("extension/index.mjs"),
        dir.path().join("sock"),
    );
    config.plugin_id = "acme.guard".into();
    config.version = "1.0.0".into();
    config.digest = "sha256:test".into();
    config.config = json!({"marker": marker, "hooks": hooks.iter().map(|h| h.name.clone()).collect::<Vec<_>>()});
    config.hooks = hooks;
    config.ready_timeout = Duration::from_secs(10);
    Env {
        _dir: dir,
        group: ManagedProcessGroup::new(),
        config,
        marker,
    }
}

async fn up(env: &Env, bus: &ExtensionBus) -> Arc<magi_extension_host::instance::HostInstance> {
    bus.register_plugin("acme.guard", &env.config.hooks)
        .unwrap();
    let instance = launch(&env.group, env.config.clone(), 1)
        .await
        .expect("launch");
    assert!(bus.attach(Arc::clone(&instance)));
    instance
}

fn ctx() -> HookContext {
    HookContext::new("inv_1")
}

#[tokio::test]
async fn guard_blocks_rewrites_and_continues_through_a_real_node_host() {
    let env = env(
        "guard",
        vec![hook("tool.call", HookMode::Guard, Some(3000))],
    );
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    assert_eq!(instance.generation, 1);

    let blocked = bus
        .invoke(
            "tool.call",
            &ctx(),
            json!({"input": {"command": "rm -rf /"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        blocked.verdict,
        Verdict::Blocked {
            plugin: "acme.guard".into(),
            reason: "禁止递归删除".into()
        }
    );

    let ok = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert_eq!(
        ok.verdict,
        Verdict::Proceed(json!({"input": {"command": "ls"}}))
    );
    assert_eq!(ok.trace[0].outcome, TraceOutcome::Continue);

    // guard 型钩子可以 replace（tool.call 允许）；候选被改写，仍待原生复核。
    let rewritten = bus
        .invoke(
            "tool.call",
            &ctx(),
            json!({"input": {"command": "rewrite", "cwd": "/w"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        rewritten.verdict,
        Verdict::Proceed(json!({"input": {"command": "echo safe", "cwd": "/w"}}))
    );

    // 非法 decision（allow）是协议错误：guard 阻止，而不是放行。
    let bad = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "bad"}}))
        .await
        .unwrap();
    assert!(matches!(bad.verdict, Verdict::Blocked { .. }), "{bad:?}");

    instance.stop().await;
    assert!(!instance.is_alive());
}

#[tokio::test]
async fn guard_timeout_cancels_the_extension_and_blocks() {
    let env = env("guard", vec![hook("tool.call", HookMode::Guard, Some(400))]);
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    let outcome = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "slow"}}))
        .await
        .unwrap();
    assert!(
        matches!(outcome.verdict, Verdict::Blocked { .. }),
        "{outcome:?}"
    );
    // 取消通知到达扩展侧。
    let mut seen = false;
    for _ in 0..50 {
        if std::fs::read_to_string(&env.marker).is_ok_and(|log| log.contains("cancelled")) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "host.cancel 必须到达扩展");
    instance.stop().await;
}

#[tokio::test]
async fn host_crash_blocks_guard_actions_instead_of_defaulting_to_allow() {
    let env = env(
        "guard",
        vec![hook("tool.call", HookMode::Guard, Some(2000))],
    );
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    let crashed = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "crash"}}))
        .await
        .unwrap();
    assert!(
        matches!(crashed.verdict, Verdict::Blocked { .. }),
        "{crashed:?}"
    );
    instance.wait_exit().await;
    // 宿主已不在：登记仍保留，受影响动作继续被阻止。
    let after = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert!(
        matches!(after.verdict, Verdict::Blocked { .. }),
        "{after:?}"
    );
    // 用户显式停用（注销登记）后才按变更后的策略放行。
    bus.unregister_plugin("acme.guard");
    let freed = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert_eq!(
        freed.verdict,
        Verdict::Proceed(json!({"input": {"command": "ls"}}))
    );
}

#[tokio::test]
async fn advise_failures_keep_native_behaviour_and_replace_is_applied() {
    let env = env(
        "guard",
        vec![hook("model.request", HookMode::Advise, Some(300))],
    );
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    let ok = bus
        .invoke(
            "model.request",
            &ctx(),
            json!({"temperature": 1.0, "maxTokens": 10}),
        )
        .await
        .unwrap();
    assert_eq!(
        ok.verdict,
        Verdict::Proceed(json!({"temperature": 0.1, "maxTokens": 10}))
    );
    let slow = bus
        .invoke(
            "model.request",
            &ctx(),
            json!({"temperature": 1.0, "sleep": 2000}),
        )
        .await
        .unwrap();
    assert_eq!(
        slow.verdict,
        Verdict::Proceed(json!({"temperature": 1.0, "sleep": 2000}))
    );
    assert!(matches!(slow.trace[0].outcome, TraceOutcome::Failed(_)));
    instance.stop().await;
}

#[tokio::test]
async fn observe_notifications_are_delivered_in_order_with_sequence_numbers() {
    let env = env("guard", vec![hook("turn.end", HookMode::Observe, None)]);
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    for turn in ["t1", "t2", "t3"] {
        bus.notify("turn.end", json!({"turnId": turn}));
    }
    let mut log = String::new();
    for _ in 0..60 {
        log = std::fs::read_to_string(&env.marker).unwrap_or_default();
        if log.lines().count() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["turn.end:1:t1", "turn.end:2:t2", "turn.end:3:t3"]
    );
    instance.stop().await;
}

#[tokio::test]
async fn registering_an_undeclared_hook_fails_activation() {
    let env = env("undeclared", vec![hook("tool.call", HookMode::Guard, None)]);
    let error = launch(&env.group, env.config.clone(), 1)
        .await
        .err()
        .expect("must fail");
    assert!(matches!(error, LaunchError::NotReady(_)), "{error}");
}

#[tokio::test]
async fn bad_tokens_and_unsupported_protocol_versions_are_rejected_at_handshake() {
    for script in ["bad-token.mjs", "old-protocol.mjs"] {
        let mut env = env("guard", vec![]);
        env.config.runtime_entry = fixtures().join(script);
        let error = launch(&env.group, env.config.clone(), 1)
            .await
            .err()
            .expect("must fail");
        assert!(
            matches!(error, LaunchError::Handshake(_)),
            "{script}: {error}"
        );
    }
}

#[tokio::test]
async fn catalog_violations_are_refused_before_any_process_starts() {
    let env = env("guard", vec![hook("turn.end", HookMode::Guard, None)]);
    let error = launch(&env.group, env.config.clone(), 1)
        .await
        .err()
        .expect("must fail");
    assert!(matches!(error, LaunchError::InvalidHook(_)), "{error}");
}

#[tokio::test]
async fn supervisor_restarts_after_a_crash_and_stops_restarting_after_repeated_failures() {
    let env = env(
        "guard",
        vec![hook("tool.call", HookMode::Guard, Some(2000))],
    );
    let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
    bus.register_plugin("acme.guard", &env.config.hooks)
        .unwrap();
    let policy = RestartPolicy {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
        window: Duration::from_secs(60),
        max_crashes: 2,
    };
    let supervisor = supervise(
        env.group.clone(),
        Arc::clone(&bus),
        env.config.clone(),
        policy,
    );
    supervisor
        .wait_for(|s| matches!(s, SupervisorState::Running { generation: 1 }))
        .await;

    // 第一次崩溃：自动重启到下一代，代次递增。
    bus.invoke("tool.call", &ctx(), json!({"input": {"command": "crash"}}))
        .await
        .unwrap();
    supervisor
        .wait_for(|s| matches!(s, SupervisorState::Running { generation: 2 }))
        .await;
    let healthy = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert_eq!(
        healthy.verdict,
        Verdict::Proceed(json!({"input": {"command": "ls"}}))
    );

    // 反复崩溃超过阈值：停止自动重启，guard 持续阻止。
    bus.invoke("tool.call", &ctx(), json!({"input": {"command": "crash"}}))
        .await
        .unwrap();
    supervisor
        .wait_for(|s| matches!(s, SupervisorState::Running { generation: 3 }))
        .await;
    bus.invoke("tool.call", &ctx(), json!({"input": {"command": "crash"}}))
        .await
        .unwrap();
    let state = supervisor
        .wait_for(|s| matches!(s, SupervisorState::Failed(_)))
        .await;
    assert!(matches!(state, SupervisorState::Failed(_)));
    let blocked = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert!(matches!(blocked.verdict, Verdict::Blocked { .. }));
    supervisor.stop().await;
}

#[tokio::test]
async fn stopping_the_supervisor_terminates_the_host_process() {
    let env = env(
        "guard",
        vec![hook("tool.call", HookMode::Guard, Some(2000))],
    );
    let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
    bus.register_plugin("acme.guard", &env.config.hooks)
        .unwrap();
    let supervisor = supervise(
        env.group.clone(),
        Arc::clone(&bus),
        env.config.clone(),
        RestartPolicy::default(),
    );
    supervisor
        .wait_for(|s| matches!(s, SupervisorState::Running { .. }))
        .await;
    supervisor.stop().await;
    // 停止后宿主不再响应：guard 阻止（登记仍在，直到用户注销）。
    let after = bus
        .invoke("tool.call", &ctx(), json!({"input": {"command": "ls"}}))
        .await
        .unwrap();
    assert!(matches!(after.verdict, Verdict::Blocked { .. }));
}

#[tokio::test]
async fn a_host_that_exits_before_connecting_fails_fast_instead_of_waiting_for_the_deadline() {
    let mut env = env("guard", vec![]);
    // 入口脚本立即退出：不会连接套接字。
    env.config.runtime_entry = fixtures().join("exit-immediately.mjs");
    env.config.connect_timeout = Duration::from_secs(30);
    let started = std::time::Instant::now();
    let error = launch(&env.group, env.config.clone(), 1)
        .await
        .err()
        .expect("must fail");
    assert!(
        matches!(error, LaunchError::ExitedBeforeConnect(_)),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "必须快速失败: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn tools_are_invoked_through_the_real_host_and_failures_stay_tool_results() {
    let mut env = env("tools", Vec::new());
    env.config.tools = vec!["echo".into(), "slow".into(), "boom".into()];
    env.config.commands = vec!["hello".into(), "bad".into(), "slow-cmd".into()];
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let instance = up(&env, &bus).await;
    assert!(instance.is_tool_bound("echo"));
    let ctx = json!({"invocationId": "inv-1"});
    let ok = instance
        .invoke_tool(
            "echo",
            json!({"a": 1}),
            ctx.clone(),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(ok["output"]["echoed"], json!({"a": 1}));
    assert_eq!(ok["output"]["invocation"], "inv-1");
    let failed = instance
        .invoke_tool(
            "boom",
            json!({}),
            ctx.clone(),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(failed["isError"], true);
    assert!(failed["output"].as_str().unwrap().contains("boom failed"));
    // 超时：宿主收到取消信号，调用返回 DeadlineExceeded。
    let slow = instance
        .invoke_tool(
            "slow",
            json!({}),
            ctx,
            Duration::from_millis(300),
            std::future::pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        slow.kind(),
        magi_extension_host::protocol::ErrorKind::DeadlineExceeded
    );
    // 调用方取消：宿主收到取消信号，调用立即返回 Cancelled，不等待期限。
    let started = std::time::Instant::now();
    let cancelled = instance
        .invoke_tool(
            "slow",
            json!({}),
            json!({"invocationId": "inv-cancel"}),
            Duration::from_secs(30),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap_err();
    assert_eq!(
        cancelled.kind(),
        magi_extension_host::protocol::ErrorKind::Cancelled
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // 取消后宿主仍可服务下一次调用，挂起槽位已释放。
    let again = instance
        .invoke_tool(
            "echo",
            json!(1),
            json!({"invocationId": "inv-2"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(again["output"]["echoed"], 1);
    // 用户命令：返回文本；抛错与空返回是命令失败；调用方取消立即返回。
    let hello = instance
        .invoke_command(
            "hello",
            json!({"text": "Magi"}),
            json!({"invocationId": "c1"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(hello["content"], "你好，Magi");
    let bad = instance
        .invoke_command(
            "bad",
            json!({}),
            json!({"invocationId": "c2"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(bad["isError"], true);
    let stopped = instance
        .invoke_command(
            "slow-cmd",
            json!({}),
            json!({"invocationId": "c3"}),
            Duration::from_secs(30),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap_err();
    assert_eq!(
        stopped.kind(),
        magi_extension_host::protocol::ErrorKind::Cancelled
    );
    // 未绑定的工具不会被分发。
    assert!(
        instance
            .invoke_tool(
                "missing",
                json!({}),
                json!({}),
                Duration::from_secs(1),
                std::future::pending()
            )
            .await
            .is_err()
    );
    instance.stop().await;
}

#[tokio::test]
async fn registering_an_unapproved_tool_fails_activation() {
    let mut env = env("tools", Vec::new());
    env.config.tools = vec!["echo".into()];
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let error = launch(&env.group, env.config.clone(), 1).await.err();
    assert!(error.is_some(), "未获批的工具注册必须使激活失败");
}
