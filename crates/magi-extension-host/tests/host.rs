//! 与真实 Node 进程的端到端验收：握手、注册、钩子裁决、超时取消、崩溃与观察投递。

use magi_extension_host::{
    HostService,
    bus::{BusPolicy, ExtensionBus, HookContext, TraceOutcome, Verdict},
    instance::{LaunchConfig, LaunchError, launch},
    peer::BoxFuture,
    protocol::{ErrorKind, RpcError},
    protocol::{HookMode, HookRegistration},
    supervisor::{RestartPolicy, SupervisorState, supervise},
};
use magi_process::ManagedProcessGroup;
use serde_json::Value;
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

struct MockService {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl HostService for MockService {
    fn request_with_identity(
        &self,
        plugin_id: String,
        method: String,
        params: Value,
    ) -> BoxFuture<Result<Value, RpcError>> {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            calls.lock().unwrap().push(format!("{plugin_id}:{method}"));
            match method.as_str() {
                "resource.read" => Ok(json!({"version": 0, "value": null})),
                "resource.write" => Ok(
                    json!({"version": params["expectedVersion"].as_u64().unwrap_or(0) + 1, "value": params["value"]}),
                ),
                "settings.read" => Ok(json!({"version": 2, "value": {"enabled": true}})),
                _ => Err(RpcError::new(ErrorKind::MethodNotFound, method)),
            }
        })
    }
}

#[tokio::test]
async fn node_extension_can_call_declared_daemon_resource_and_settings_services() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("index.mjs");
    std::fs::write(
        &entry,
        r#"
export default async function activate(api) {
  const resource = await api.resource.read('notes');
  await api.resource.write('notes', resource.version, { text: 'ok' });
  const settings = await api.settings.read();
  if (settings.value.enabled !== true || api.configRevision !== 2) throw new Error('bad settings');
}


"#,
    )
    .unwrap();
    let mut config = LaunchConfig::new(node(), runtime(), entry, dir.path().join("sock"));
    config.plugin_id = "acme.services".into();
    config.version = "1.0.0".into();
    config.digest = "sha256:services".into();
    config.service = Some(Arc::new(MockService {
        calls: Arc::default(),
    }));
    config.config = json!({"enabled": false});
    config.config_revision = 2;
    let instance = launch(&ManagedProcessGroup::new(), config, 1)
        .await
        .unwrap();
    instance.stop().await;
}

#[tokio::test]
async fn plugin_directory_notifications_are_instance_local_and_subscriptions_can_be_released() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("index.mjs");
    std::fs::write(
        &entry,
        r#"
export default async api => {
  let count=0;
  const off=api.plugins.onChanged(() => { count++; });
  await api.route('/count', () => ({status:200,body:{count}}));
  await api.route('/off', () => { off(); return {status:200,body:{count}}; });
};
"#,
    )
    .unwrap();
    let config = LaunchConfig::new(node(), runtime(), entry, dir.path().join("sock"));
    let group = ManagedProcessGroup::new();
    let instance = launch(&group, config, 1).await.unwrap();
    let call = |path: &'static str, id: &'static str| {
        instance.invoke_route(
            path,
            json!({"method":"GET","path":path}),
            json!({"invocationId":id}),
            Duration::from_secs(5),
            std::future::pending(),
        )
    };
    instance.plugin_directory_changed().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if call("/count", "first").await.unwrap()["body"]["count"] == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    call("/off", "off").await.unwrap();
    instance.plugin_directory_changed().unwrap();
    assert_eq!(call("/count", "second").await.unwrap()["body"]["count"], 1);
    instance.stop().await;
    assert!(!instance.is_alive());
}

#[tokio::test]
async fn background_management_uses_instance_identity_after_http_reply_without_reviving_the_parent()
{
    struct Management(std::sync::Mutex<Vec<Value>>);
    impl HostService for Management {
        fn request_with_identity(
            &self,
            _: String,
            method: String,
            params: Value,
        ) -> BoxFuture<Result<Value, RpcError>> {
            if method == "plugins.setEnabled" {
                self.0.lock().unwrap().push(params);
                return Box::pin(async { Ok(json!({"applied":true})) });
            }
            Box::pin(async move { Err(RpcError::new(ErrorKind::MethodNotFound, method)) })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("index.mjs");
    std::fs::write(&entry, r#"
export default async api => {
  const result = {};
  await api.route('/start', () => {
    setTimeout(async () => {
      try { await api.plugins.setEnabled('target', true); result.foreground='unexpected'; } catch(error) { result.foreground=error.kind; }
      try { result.background=await api.plugins.background.setEnabled('target', false, 'current', {expectedState:{enabled:true},reason:'diagnostic pause'}); } catch(error) { result.background=error.message; }
    }, 50);
    return {status:202,body:{accepted:true}};
  });
  await api.route('/status', () => ({status:200,body:result}));
};
"#).unwrap();
    let service = Arc::new(Management(std::sync::Mutex::new(Vec::new())));
    let mut config = LaunchConfig::new(node(), runtime(), entry, dir.path().join("sock"));
    config.plugin_id = "acme.management".into();
    config.version = "1.0.0".into();
    config.digest = "sha256:management".into();
    config.service = Some(service.clone());
    let instance = launch(&ManagedProcessGroup::new(), config, 1)
        .await
        .unwrap();
    let call = |path: &'static str, id: String| {
        instance.invoke_route(
            path,
            json!({"method":"GET","path":path}),
            json!({"invocationId":id}),
            Duration::from_secs(5),
            std::future::pending(),
        )
    };
    assert_eq!(call("/start", "start".into()).await.unwrap()["status"], 202);
    let mut status = Value::Null;
    for n in 0..40 {
        status = call("/status", format!("status-{n}")).await.unwrap();
        if status["body"].get("background").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(status["body"]["foreground"], "Cancelled");
    assert_eq!(status["body"]["background"]["applied"], true);
    let calls = service.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["enabled"], false);
    assert_eq!(calls[0]["expectedDigest"], "current");
    assert_eq!(calls[0]["expectedState"], json!({"enabled":true}));
    assert_eq!(calls[0]["reason"], "diagnostic pause");
    assert_eq!(calls[0]["_background"], true);
    assert!(calls[0]["_instance"].is_string());
    assert!(calls[0].get("_context").is_none());
    drop(calls);
    instance.stop().await;
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
async fn stopping_a_host_cancels_even_a_handler_that_returns_success_after_abort() {
    let mut env = env("tools", vec![]);
    env.config.plugin_entry = env._dir.path().join("stop-race.mjs");
    env.config.commands = vec!["wait".into()];
    std::fs::write(
        &env.config.plugin_entry,
        r#"
import { writeFile } from 'node:fs/promises';
export default async api => {
  await api.command('wait', async (_, ctx) => {
    const aborted = new Promise(resolve => ctx.signal.addEventListener('abort', resolve, {once: true}));
    await writeFile(api.config.marker, 'started');
    await aborted;
    return 'late success';
  });
};
"#,
    )
    .unwrap();
    let instance = launch(&env.group, env.config, 1).await.unwrap();
    let running = Arc::clone(&instance);
    let call = tokio::spawn(async move {
        running
            .invoke_command(
                "wait",
                json!({}),
                json!({}),
                Duration::from_secs(10),
                std::future::pending(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !env.marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    instance.stop().await;
    assert_eq!(
        call.await.unwrap().unwrap_err().kind(),
        ErrorKind::Cancelled
    );
    assert!(!instance.is_alive());
    let refused = instance
        .invoke_command(
            "wait",
            json!({}),
            json!({}),
            Duration::from_secs(1),
            std::future::pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::HostGone);
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
    assert!(
        ok["output"]["invocation"]
            .as_str()
            .unwrap()
            .starts_with("acme.guard:1:")
    );
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

#[tokio::test]
async fn concurrent_calls_with_the_same_caller_identity_cancel_independently() {
    use magi_extension_host::protocol::ErrorKind;
    let mut env = env("tools", Vec::new());
    env.config.tools = vec!["echo".into(), "slow".into(), "boom".into()];
    env.config.commands = vec!["hello".into(), "bad".into(), "slow-cmd".into()];
    let bus = ExtensionBus::new(BusPolicy::default());
    let instance = up(&env, &bus).await;
    let mut calls = Vec::new();
    let mut cancellations = Vec::new();
    for _ in 0..2 {
        let (cancel, waiting) = tokio::sync::oneshot::channel::<()>();
        cancellations.push(cancel);
        let instance = Arc::clone(&instance);
        calls.push(tokio::spawn(async move {
            instance
                .invoke_tool(
                    "slow",
                    json!({}),
                    json!({"invocationId":"same-caller"}),
                    Duration::from_secs(5),
                    async {
                        let _ = waiting.await;
                    },
                )
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancellations.remove(0).send(()).unwrap();
    assert_eq!(
        calls.remove(0).await.unwrap().unwrap_err().kind(),
        ErrorKind::Cancelled
    );
    assert!(!calls[0].is_finished(), "取消第一个调用不得结束第二个调用");
    cancellations.remove(0).send(()).unwrap();
    assert_eq!(
        calls.remove(0).await.unwrap().unwrap_err().kind(),
        ErrorKind::Cancelled
    );
    instance.stop().await;
}

#[tokio::test]
async fn supervisor_reclaims_a_live_host_whose_event_loop_is_stuck() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("spin.mjs");
    std::fs::write(&entry, "export default async function(api) { await api.on('tool.call', () => { while (true) {} }); }").unwrap();
    let mut config = LaunchConfig::new(node(), runtime(), entry, dir.path().join("sock"));
    config.plugin_id = "acme.stuck".into();
    config.hooks = vec![hook("tool.call", HookMode::Guard, Some(50))];
    let bus = Arc::new(ExtensionBus::default());
    bus.register_plugin("acme.stuck", &config.hooks).unwrap();
    let group = ManagedProcessGroup::new();
    let supervisor = supervise(
        group,
        Arc::clone(&bus),
        config,
        RestartPolicy {
            max_crashes: 0,
            ..RestartPolicy::default()
        },
    );
    supervisor
        .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
        .await;
    let outcome = bus.invoke("tool.call", &ctx(), json!({})).await.unwrap();
    assert!(matches!(outcome.verdict, Verdict::Blocked { .. }));
    let state = tokio::time::timeout(
        Duration::from_secs(15),
        supervisor.wait_for(|state| matches!(state, SupervisorState::Failed(_))),
    )
    .await
    .expect("卡死宿主必须在有限时间内回收");
    assert!(matches!(state, SupervisorState::Failed(_)));
    assert!(bus.instance("acme.stuck").is_none());
    supervisor.stop().await;
}

#[tokio::test]
async fn job_runs_use_signed_owners_and_enforce_model_budgets() {
    struct ModelService {
        calls: Arc<std::sync::Mutex<Vec<Value>>>,
    }
    impl HostService for ModelService {
        fn request_with_identity(
            &self,
            _: String,
            method: String,
            params: Value,
        ) -> BoxFuture<Result<Value, RpcError>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                assert_eq!(method, "model.invoke");
                calls.lock().unwrap().push(params);
                Ok(json!({"content":"ok"}))
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("index.mjs");
    std::fs::write(&entry,r#"export default async api => {
        await api.command('run', async (_,ctx) => {
            if(ctx.job.jobId !== 'signed-job') throw Error('missing job');
            const first=await api.model.invoke({prompt:'hello'});
            const denied=await Promise.allSettled([api.model.invoke({prompt:'two'}),api.model.invoke({prompt:'three'})]);
            if(denied.some(v=>v.status!=='rejected' || v.reason.kind!=='PermissionDenied')) throw Error('budget bypass');
            return first.content;
        });
    }"#).unwrap();
    let calls = Arc::new(std::sync::Mutex::new(vec![]));
    let mut config = LaunchConfig::new(node(), runtime(), entry, dir.path().join("sock"));
    config.plugin_id = "acme.jobs".into();
    config.commands = vec!["run".into()];
    config.service = Some(Arc::new(ModelService {
        calls: calls.clone(),
    }));
    let group = ManagedProcessGroup::new();
    let host = launch(&group, config, 1).await.unwrap();
    let response=host.invoke_command("run",json!({"text":""}),json!({"owner":{"sessionId":"signed-session","turnId":"signed-turn"},"job":{"jobId":"signed-job","scheduleId":"plan","budget":{"maxModelCalls":1,"maxOutputTokens":123,"timeoutMs":5000}}}),Duration::from_secs(5),std::future::pending()).await.unwrap();
    assert_eq!(response["content"], "ok");
    {
        let received = calls.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["maxOutputTokens"], 123);
        assert_eq!(received[0]["_context"]["owner"]["turnId"], "signed-turn");
    }
    host.stop().await;
}

#[tokio::test]
async fn tools_can_be_unbound_and_rebound_at_runtime_without_widening_approval() {
    let mut env = env("toggle", Vec::new());
    env.config.tools = vec!["gated".into()];
    env.config.commands = vec!["toggle".into(), "swap".into()];
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let instance = up(&env, &bus).await;
    assert!(instance.is_tool_bound("gated"));
    let toggle = |id: &'static str| {
        let instance = Arc::clone(&instance);
        async move {
            instance
                .invoke_command(
                    "toggle",
                    json!({"text": ""}),
                    json!({"invocationId": id}),
                    Duration::from_secs(5),
                    std::future::pending(),
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(toggle("t-1").await["content"], "off");
    assert!(!instance.is_tool_bound("gated"), "释放后工具不可调用");
    let refused = instance
        .invoke_tool(
            "gated",
            json!({}),
            json!({"invocationId": "c-1"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::MethodNotFound);
    assert_eq!(toggle("t-2").await["content"], "on");
    assert!(instance.is_tool_bound("gated"), "旧释放函数不得收回新绑定");
    let again = instance
        .invoke_tool(
            "gated",
            json!({}),
            json!({"invocationId": "c-2"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(again["output"], "on-again");
}

#[tokio::test]
async fn rebinding_in_the_same_tick_keeps_the_new_binding() {
    let mut env = env("toggle", Vec::new());
    env.config.tools = vec!["gated".into()];
    env.config.commands = vec!["toggle".into(), "swap".into()];
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let instance = up(&env, &bus).await;
    assert!(instance.is_tool_bound("gated"));
    let swapped = instance
        .invoke_command(
            "swap",
            json!({"text": ""}),
            json!({"invocationId": "s-1"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(swapped["content"], "swapped");
    assert!(
        instance.is_tool_bound("gated"),
        "新注册先于旧释放发出时，旧释放不得收回新绑定"
    );
    let answer = instance
        .invoke_tool(
            "gated",
            json!({}),
            json!({"invocationId": "c-3"}),
            Duration::from_secs(5),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(answer["output"], "swapped");
}

#[tokio::test]
async fn view_routes_bind_exact_and_prefix_dispatch_and_release_at_runtime() {
    let env = env("routes", Vec::new());
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let instance = up(&env, &bus).await;
    let call = |path: &'static str, id: &'static str| {
        let instance = Arc::clone(&instance);
        async move {
            instance
                .invoke_route(
                    path,
                    json!({"method": "GET", "path": path, "query": "a=1", "body": null}),
                    json!({"invocationId": id}),
                    Duration::from_secs(5),
                    std::future::pending(),
                )
                .await
        }
    };
    // 精确匹配：处理器收到请求描述，状态码缺省为 200。
    let status = call("/status", "r-1").await.unwrap();
    assert_eq!(status["status"], 200);
    assert_eq!(status["body"]["seen"]["query"], "a=1");
    // 前缀匹配取最长前缀；精确绑定优先于前缀。
    let prefixed = call("/items/42", "r-2").await.unwrap();
    assert_eq!(prefixed["status"], 201);
    assert_eq!(prefixed["contentType"], "application/x-ndjson");
    assert_eq!(prefixed["body"], "/items/42\n");
    assert_eq!(
        call("/items/special", "r-3").await.unwrap()["body"],
        "exact-wins"
    );
    // 处理器抛错是视图可见的 500，不是协议错误。
    let boom = call("/boom", "r-4").await.unwrap();
    assert_eq!(boom["status"], 500);
    assert!(
        boom["body"]["error"]
            .as_str()
            .unwrap()
            .contains("处理器失败")
    );
    // 未绑定的路径明确拒绝，不落到别的处理器。
    assert_eq!(
        call("/nothing", "r-5").await.unwrap_err().kind(),
        ErrorKind::MethodNotFound
    );
    // 运行期释放：释放后不可再调用。
    assert_eq!(call("/gone", "r-6").await.unwrap()["body"], "here");
    assert_eq!(call("/drop", "r-7").await.unwrap()["body"], "dropped");
    assert!(
        instance.resolve_route("/gone").is_none(),
        "释放后路由不再命中"
    );
    assert_eq!(
        call("/gone", "r-8").await.unwrap_err().kind(),
        ErrorKind::MethodNotFound
    );
    let bytes = call("/bytes", "r-bytes").await.unwrap();
    assert_eq!(bytes["bodyBase64"], "AP+A");
    assert!(bytes.get("body").is_none());
    assert_eq!(bytes["headers"]["x-original"], "bytes");
    for (path, id) in [
        ("/conflicting-body", "r-conflict"),
        ("/too-big", "r-too-big"),
    ] {
        let failed = call(path, id).await.unwrap();
        assert_eq!(failed["status"], 500);
        assert!(!failed["body"]["error"].as_str().unwrap().is_empty());
    }
    assert_eq!(
        call("/status", "r-after-errors").await.unwrap()["status"],
        200,
        "非法/超限路由响应不能击穿帧限制并终止宿主"
    );
}

#[tokio::test]
async fn view_route_calls_are_cancelled_with_their_http_request() {
    let env = env("routes", Vec::new());
    let bus = ExtensionBus::new(BusPolicy::default());
    bus.register_plugin("acme.guard", &[]).unwrap();
    let instance = up(&env, &bus).await;
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    let started = std::time::Instant::now();
    let pending = {
        let instance = Arc::clone(&instance);
        tokio::spawn(async move {
            instance
                .invoke_route(
                    "/slow",
                    json!({"method": "GET", "path": "/slow", "query": "", "body": null}),
                    json!({"invocationId": "slow-1"}),
                    Duration::from_secs(20),
                    async move {
                        let _ = cancelled.await;
                    },
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.send(()).unwrap();
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "取消不得等到处理器的 30 秒"
    );
}

#[tokio::test]
async fn malformed_or_excessive_route_registrations_are_refused() {
    let env = env("routes-bad", Vec::new());
    let error = launch(&env.group, env.config.clone(), 1).await;
    assert!(error.is_err(), "非法路由名必须让激活失败");
}
