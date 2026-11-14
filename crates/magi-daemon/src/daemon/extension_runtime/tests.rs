use super::*;
use magi_app_server_protocol::PluginManifest;
use magi_extension_host::{
    bus::{BusPolicy, HookContext, Verdict},
    supervisor::SupervisorState,
};
use magi_plugin_system::PluginSource;

#[test]
fn node_is_resolved_from_the_override_or_next_to_the_daemon_and_never_from_path() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = dir.path().join("magi-daemon-app");
    std::fs::write(&daemon, b"").unwrap();
    // 没有随包 Node，也没有覆盖：即使 PATH 里有 node 也不使用。
    assert_eq!(resolve_node(None, Some(daemon.clone())), None);

    let name = if cfg!(windows) { "node.exe" } else { "node" };
    std::fs::create_dir_all(dir.path().join("node")).unwrap();
    let bundled = dir.path().join("node").join(name);
    std::fs::write(&bundled, b"").unwrap();
    assert_eq!(
        resolve_node(None, Some(daemon.clone())),
        Some(bundled.clone())
    );

    // 覆盖入口优先（开发/测试）；指向不存在的文件则不生效，回到随包 Node。
    let custom = dir.path().join("custom-node");
    std::fs::write(&custom, b"").unwrap();
    assert_eq!(
        resolve_node(Some(custom.clone().into()), Some(daemon.clone())),
        Some(custom)
    );
    assert_eq!(
        resolve_node(Some(dir.path().join("missing").into()), Some(daemon)),
        Some(bundled)
    );
}
use serde_json::json;
use std::io::Write;

const ENTRY: &str = r#"
export default async function activate(api) {
  await api.on('tool.call', async (event) => {
    if (event.input?.command?.includes('rm')) return { decision: 'block', reason: '禁止 rm' };
    return { decision: 'continue' };
  });
}
"#;

/// 测试用 Node：优先 `MAGI_EXTENSION_NODE`（可指向随包的固定版本），否则 PATH 中的 node。
fn test_node() -> PathBuf {
    std::env::var_os(EXTENSION_NODE_ENV)
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"))
        .expect("需要 Node")
}

fn package() -> PluginPackage {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Guard", "description": "",
        "extension": {"hooks": [{"name": "tool.call", "mode": "guard", "deadlineMs": 3000}]},
        "permissions": [{"kind": "extension", "targets": ["full-trust"]}],
        "dataSchemaVersion": 1,
        "settingsSchema": {"type": "object", "additionalProperties": false},
        "contributions": {}
    }))
    .unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", ENTRY.as_bytes().to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    manager: Arc<Mutex<PluginManager>>,
    bus: Arc<ExtensionBus>,
    runtime: ExtensionRuntime,
}

fn fixture(node: Option<PathBuf>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(
        PluginManager::open(dir.path().join("plugins")).unwrap(),
    ));
    let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
    let mut runtime = ExtensionRuntime::new(
        Arc::clone(&manager),
        Arc::clone(&bus),
        ManagedProcessGroup::new(),
        dir.path(),
    );
    runtime.node = node;
    Fixture {
        _dir: dir,
        manager,
        bus,
        runtime,
    }
}

#[test]
#[cfg(unix)]
fn materialized_native_helper_executes_and_assets_stay_non_executable() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture(None);
    let manifest = serde_json::to_value(package().manifest()).unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes, mode) in [
        (
            "manifest.json",
            serde_json::to_vec(&manifest).unwrap(),
            0o644,
        ),
        (
            "extension/index.mjs",
            b"export default () => {};".to_vec(),
            0o644,
        ),
        (
            "extension/upstream/native/helper",
            b"#!/bin/sh\nprintf native-helper-ok".to_vec(),
            0o755,
        ),
        (
            "extension/upstream/native/data",
            b"plain data".to_vec(),
            0o644,
        ),
    ] {
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default().unix_permissions(mode),
            )
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    let (_, entry) = fixture.runtime.materialize("acme.guard", &package).unwrap();
    let helper = entry.parent().unwrap().join("upstream/native/helper");
    let result = magi_process::std_command(&helper).output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"native-helper-ok");
    assert_eq!(
        std::fs::metadata(entry.parent().unwrap().join("upstream/native/data"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    // 重用已落盘的同摘要目录时也恢复执行权限。
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o644)).unwrap();
    fixture.runtime.materialize("acme.guard", &package).unwrap();
    assert!(
        magi_process::std_command(helper)
            .status()
            .unwrap()
            .success()
    );
}

fn install_enabled(fixture: &Fixture, package: &PluginPackage) {
    let mut manager = fixture.manager.lock().unwrap();
    manager
        .install(
            package,
            PluginSource::Local {
                name: "guard.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.guard").unwrap();
}

fn call(command: &str) -> serde_json::Value {
    json!({"input": {"command": command}})
}

const TOOL_ENTRY: &str = r#"
export default async function activate(api) {
  await api.tool('echo', async (input) => ({ echoed: input.text }));
  await api.tool('boom', async () => { throw new Error('工具失败'); });
  await api.command('hello', async (input, ctx) => input.text === 'context'
    ? JSON.stringify({owner:ctx.owner,execution:ctx.execution}) : `你好，${input.text || '世界'}`);
  await api.command('bad', async () => { throw new Error('命令失败'); });
  await api.command('wait', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.tool('slow', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.tool('slow-write', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
}
"#;

fn tool_package() -> PluginPackage {
    let manifest: PluginManifest = serde_json::from_value(json!({
            "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Tools", "description": "",
            "extension": {"hooks": []},
            "permissions": [
                {"kind": "extension", "targets": ["full-trust"]},
                {"kind": "tools", "targets": ["echo", "boom", "slow", "slow-write"]}
            ],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {"commands": [
                {"id": "hello", "title": "Hello", "description": "打招呼"},
                {"id": "bad", "title": "Bad", "description": "总是失败"},
                {"id": "wait", "title": "Wait", "description": "等待取消"}
            ], "tools": [
                {"id": "echo", "title": "Echo", "description": "回显", "readOnly": true,
                 "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}, "additionalProperties": false}},
                {"id": "boom", "title": "Boom", "description": "总是失败", "readOnly": true,
                 "inputSchema": {"type": "object"}},
                {"id": "slow", "title": "Slow", "description": "等待取消", "readOnly": true,
                 "inputSchema": {"type": "object"}},
                {"id": "slow-write", "title": "SlowWrite", "description": "等待取消", "readOnly": false,
                 "inputSchema": {"type": "object"}}
            ]}
        }))
        .unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", TOOL_ENTRY.as_bytes().to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

const ROUTE_ENTRY: &str = r#"
export default async function activate(api) {
  await api.command('noop', async () => 'ok');
  await api.route('/status', async (input) => ({ body: { echoed: input.query, headers: input.headers } }));
  await api.route('/hang', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return { body: 'aborted' };
  });
}
"#;

fn route_package(id: &str, with_view: bool) -> PluginPackage {
    let views = if with_view {
        json!([{"id": "page", "title": "Page", "entry": "ui/index.html", "placements": ["settings"]}])
    } else {
        json!([])
    };
    // 包必须至少有一项贡献；无视图的包以一个命令满足，路由准入仍由「有无视图」决定。
    let commands = json!([{"id": "noop", "title": "Noop", "description": "占位命令"}]);
    let manifest: PluginManifest = serde_json::from_value(json!({
        "sdkVersion": 2, "id": id, "version": "1.0.0", "name": "Routes", "description": "",
        "extension": {"hooks": []},
        "permissions": [{"kind": "extension", "targets": ["full-trust"]}],
        "dataSchemaVersion": 1,
        "settingsSchema": {"type": "object", "additionalProperties": false},
        "contributions": {"views": views, "commands": commands}
    }))
    .unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut files = vec![
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", ROUTE_ENTRY.as_bytes().to_vec()),
    ];
    if with_view {
        files.push(("ui/index.html", b"<!doctype html><title>x</title>".to_vec()));
    }
    for (name, bytes) in files {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn view_routes_follow_plugin_admission_and_are_cancelled_with_the_request() {
    use magi_api::PluginRouteExecutor;
    use magi_app_server_protocol::generated::{PluginRouteCallRequest, PluginRouteMethod};
    let fixture = fixture(Some(test_node()));
    let with_view = route_package("acme.guard", true);
    let without_view = route_package("acme.bare", false);
    {
        let mut manager = fixture.manager.lock().unwrap();
        for (package, name) in [(&with_view, "a.zip"), (&without_view, "b.zip")] {
            let id = package.manifest().id.clone();
            manager
                .install(package, PluginSource::Local { name: name.into() })
                .unwrap();
            manager.enable(&id).unwrap();
        }
    }
    fixture.runtime.reconcile().await;
    for id in ["acme.guard", "acme.bare"] {
        fixture
            .runtime
            .running
            .lock()
            .await
            .get(id)
            .and_then(|entry| entry.supervisor.as_ref())
            .expect("已启动监管")
            .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
            .await;
    }
    let executor = super::super::plugin_bridge::NodeRouteExecutor::for_test(
        Arc::clone(&fixture.manager),
        Arc::clone(&fixture.bus),
    );
    let request = |path: &str, query: &str| PluginRouteCallRequest {
        method: PluginRouteMethod::GET,
        path: path.into(),
        query: Some(query.into()),
        headers: Some(json!({"x-panel-key": "panel-key"})),
        body: None,
        body_base64: None,
        expected_digest: None,
        expected_instance_id: None,
    };
    // 有视图的插件：请求经 daemon 到达宿主里的处理器。
    let ok = executor
        .call("acme.guard".into(), request("/status", "a=1"))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
    let body = ok.body.unwrap();
    assert_eq!(body["echoed"], "a=1");
    assert_eq!(body["headers"]["x-panel-key"], "panel-key");
    let instance = fixture.bus.instance("acme.guard").unwrap();
    let mut bound = request("/status", "bound=1");
    bound.expected_digest = Some(instance.digest.clone());
    bound.expected_instance_id = Some(instance.instance_id.clone());
    assert_eq!(
        executor
            .call("acme.guard".into(), bound.clone())
            .await
            .unwrap()
            .status,
        200
    );
    for stale in ["digest", "instance"] {
        let mut invalid = bound.clone();
        if stale == "digest" {
            invalid.expected_digest = Some("stale-digest".into());
        } else {
            invalid.expected_instance_id = Some("stale-instance".into());
        }
        assert!(matches!(
            executor.call("acme.guard".into(), invalid).await,
            Err(magi_api::ApiError::Conflict(_))
        ));
    }
    // 没有视图的插件不开放后端路由，即使宿主里绑定了处理器。
    let refused = executor
        .call("acme.bare".into(), request("/status", ""))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, magi_api::ApiError::Forbidden(_)),
        "{refused:?}"
    );
    // 未绑定的路径：明确 NotFound。
    let missing = executor
        .call("acme.guard".into(), request("/nothing", ""))
        .await
        .unwrap_err();
    assert!(
        matches!(missing, magi_api::ApiError::NotFound(_)),
        "{missing:?}"
    );
    // HTTP 请求被丢弃：宿主侧的处理器收到取消，而不是跑到期限。
    let hung = executor.call("acme.guard".into(), request("/hang", ""));
    let started = std::time::Instant::now();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), hung)
            .await
            .is_err(),
        "处理器在等待取消，调用不应自行结束"
    );
    // 取消后宿主立即可以服务下一次调用（旧调用没有占住处理器）。
    let after = executor
        .call("acme.guard".into(), request("/status", "b=2"))
        .await
        .unwrap();
    assert_eq!(after.body.unwrap()["echoed"], "b=2");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    // 插件停用后：不再接受路由调用。
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    let stopped = executor
        .call("acme.guard".into(), request("/status", ""))
        .await
        .unwrap_err();
    assert!(
        matches!(stopped, magi_api::ApiError::Conflict(_)),
        "{stopped:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_commands_run_through_the_real_host_and_follow_stop_requests() {
    use magi_conversation_runtime::plugin_command::{
        PluginCommandExecutor, PluginCommandInvocation,
    };
    use std::sync::atomic::AtomicBool;
    let fixture = fixture(Some(test_node()));
    let package = tool_package();
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "c.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.guard").unwrap();
        assert_eq!(manager.command_contributions().unwrap().len(), 3);
    }
    fixture.runtime.reconcile().await;
    fixture
        .runtime
        .running
        .lock()
        .await
        .get("acme.guard")
        .and_then(|entry| entry.supervisor.as_ref())
        .expect("已启动监管")
        .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
        .await;
    let executor = Arc::new(super::super::plugin_bridge::NodeCommandExecutor::for_test(
        Arc::clone(&fixture.manager),
        Arc::clone(&fixture.bus),
        tokio::runtime::Handle::current(),
    ));
    let call = |command: &'static str, flag: Arc<AtomicBool>| {
        let executor = Arc::clone(&executor);
        tokio::task::spawn_blocking(move || {
            executor.invoke(
                PluginCommandInvocation {
                    job: None,
                    command_id: format!("plugin/acme.guard/{command}"),
                    turn_id: "t".into(),
                    context: magi_tool_runtime::ToolExecutionContext {
                        session_id: Some(magi_core::SessionId::new("s")),
                        ..Default::default()
                    },
                    input: "Magi".into(),
                },
                &flag,
            )
        })
    };
    let ok = call("hello", Arc::new(AtomicBool::new(false)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok.content, "你好，Magi");
    let directory = std::fs::canonicalize(fixture._dir.path()).unwrap();
    let context_executor = Arc::clone(&executor);
    let expected_directory = directory.clone();
    let projected = tokio::task::spawn_blocking(move || {
        context_executor.invoke(
            PluginCommandInvocation {
                command_id: "plugin/acme.guard/hello".into(),
                turn_id: "context-turn".into(),
                context: magi_tool_runtime::ToolExecutionContext {
                    session_id: Some(magi_core::SessionId::new("context-session")),
                    task_id: Some(magi_core::TaskId::new("context-task")),
                    worker_id: Some(magi_core::WorkerId::new("context-worker")),
                    working_directory: Some(directory),
                    ..Default::default()
                },
                input: "context".into(),
                job: None,
            },
            &AtomicBool::new(false),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let projected: serde_json::Value = serde_json::from_str(&projected.content).unwrap();
    assert_eq!(projected["owner"]["sessionId"], "context-session");
    assert_eq!(projected["owner"]["taskId"], "context-task");
    assert_eq!(projected["owner"]["workerId"], "context-worker");
    assert_eq!(
        projected["execution"]["workingDirectory"],
        expected_directory.to_str().unwrap()
    );
    assert_eq!(projected["execution"]["sourceFolders"], json!([]));
    let before_start = call("hello", Arc::new(AtomicBool::new(true)))
        .await
        .unwrap()
        .unwrap_err();
    assert!(before_start.contains("命令未执行"), "{before_start}");
    let error = call("bad", Arc::new(AtomicBool::new(false)))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("命令失败"), "{error}");
    // 未声明的命令身份在到达宿主之前被拒绝。
    assert!(
        call("ghost", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap()
            .is_err()
    );
    // 停止所属 Turn：运行中的命令立即返回错误，宿主收到取消。
    let flag = Arc::new(AtomicBool::new(false));
    let waiting = call("wait", Arc::clone(&flag));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    let stopped = waiting.await.unwrap().unwrap_err();
    assert!(stopped.contains("取消"), "{stopped}");
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_tools_are_listed_for_enabled_plugins_and_execute_through_the_real_host() {
    use super::super::plugin_bridge::execute_tool;
    use magi_core::ExecutionResultStatus::{Cancelled, Failed, Indeterminate, Rejected, Succeeded};
    use std::sync::atomic::AtomicBool;
    let fixture = fixture(Some(test_node()));
    let package = tool_package();
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "t.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.guard").unwrap();
        assert_eq!(manager.tool_contributions().unwrap().len(), 4);
    }
    fixture.runtime.reconcile().await;
    fixture
        .runtime
        .running
        .lock()
        .await
        .get("acme.guard")
        .and_then(|entry| entry.supervisor.as_ref())
        .expect("已启动监管")
        .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
        .await;

    let handle = tokio::runtime::Handle::current();
    let manager = Arc::clone(&fixture.manager);
    let bus = Arc::clone(&fixture.bus);
    let echo = magi_plugin_system::model_tool_name("acme.guard", "echo");
    let boom = magi_plugin_system::model_tool_name("acme.guard", "boom");
    let run_with = move |name: String, args: &'static str, flag: Arc<AtomicBool>| {
        let (manager, bus, handle) = (Arc::clone(&manager), Arc::clone(&bus), handle.clone());
        tokio::task::spawn_blocking(move || {
            execute_tool(
                &manager,
                &bus,
                &handle,
                &name,
                args,
                &magi_tool_runtime::ToolExecutionContext::default(),
                &flag,
            )
        })
    };
    let run =
        |name: String, args: &'static str| run_with(name, args, Arc::new(AtomicBool::new(false)));
    let (ok, status) = run(echo.clone(), r#"{"text":"hi"}"#).await.unwrap();
    assert_eq!(status, Succeeded, "{ok}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&ok).unwrap()["output"]["echoed"],
        "hi"
    );
    // 参数不符合 inputSchema：不会到达宿主。
    let (_, status) = run(echo, r#"{"text":1}"#).await.unwrap();
    assert_eq!(status, Rejected);
    // 工具抛错是模型可见的工具失败。
    let (failed, status) = run(boom, "{}").await.unwrap();
    assert_eq!(status, Failed);
    assert!(failed.contains("工具失败"), "{failed}");
    // 用户停止：运行中的调用被取消，宿主收到取消信号；可能写入的工具记为 Indeterminate。
    for (tool, expected) in [("slow", Cancelled), ("slow-write", Indeterminate)] {
        let flag = Arc::new(AtomicBool::new(false));
        let call = run_with(
            magi_plugin_system::model_tool_name("acme.guard", tool),
            "{}",
            Arc::clone(&flag),
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let (payload, status) = call.await.unwrap();
        assert_eq!(status, expected, "{payload}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
    // 已取消的调用不会启动。
    let flag = Arc::new(AtomicBool::new(true));
    let (_, status) = run_with(
        magi_plugin_system::model_tool_name("acme.guard", "echo"),
        r#"{"text":"x"}"#,
        flag,
    )
    .await
    .unwrap();
    assert_eq!(status, Cancelled);
    // 未知工具名被拒绝。
    let (_, status) = run("plugin__nope".into(), "{}").await.unwrap();
    assert_eq!(status, Rejected);
}

#[tokio::test]
async fn enabled_extension_packages_become_supervised_hosts_and_stop_on_disable() {
    let fixture = fixture(Some(test_node()));
    let package = package();
    install_enabled(&fixture, &package);
    fixture.runtime.reconcile().await;
    fixture
        .runtime
        .running
        .lock()
        .await
        .get("acme.guard")
        .and_then(|entry| entry.supervisor.as_ref())
        .expect("已启动监管")
        .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
        .await;

    let ctx = HookContext::new("inv");
    let blocked = fixture
        .bus
        .invoke("tool.call", &ctx, call("rm -rf x"))
        .await
        .unwrap();
    assert!(
        matches!(blocked.verdict, Verdict::Blocked { .. }),
        "{blocked:?}"
    );
    let ok = fixture
        .bus
        .invoke("tool.call", &ctx, call("ls"))
        .await
        .unwrap();
    assert_eq!(ok.verdict, Verdict::Proceed(call("ls")));

    // 用户停用：先排空并回收宿主，再注销登记，之后才按变更后的策略放行。
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    fixture.runtime.reconcile().await;
    assert!(!fixture.runtime.is_running("acme.guard").await);
    let freed = fixture
        .bus
        .invoke("tool.call", &ctx, call("rm -rf x"))
        .await
        .unwrap();
    assert_eq!(freed.verdict, Verdict::Proceed(call("rm -rf x")));
}

fn model_source_package() -> PluginPackage {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Proxy", "description": "",
        "extension": {"hooks": []},
        "permissions": [
            {"kind": "extension", "targets": ["full-trust"]},
            {"kind": "modelSources", "targets": ["proxy"]}
        ],
        "dataSchemaVersion": 1,
        "settingsSchema": {"type": "object", "additionalProperties": false},
        "contributions": {"modelSources": [{"id": "proxy", "title": "Proxy", "description": ""}]}
    }))
    .unwrap();
    let entry = r#"
export default async function activate(api) {
  await api.modelSource.set('proxy', {
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'k', apiProtocol: 'openai_chat', models: [{ id: 'm1' }],
  });
}
"#;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", entry.as_bytes().to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

#[tokio::test]
async fn plugin_model_sources_follow_the_supervised_instance_lifecycle() {
    let fixture = fixture(Some(test_node()));
    let settings = Arc::new(magi_settings_store::SettingsStore::new());
    fixture.runtime.bind_settings(&settings);
    install_enabled(&fixture, &model_source_package());
    fixture.runtime.reconcile().await;
    fixture
        .runtime
        .running
        .lock()
        .await
        .get("acme.guard")
        .and_then(|entry| entry.supervisor.as_ref())
        .expect("已启动监管")
        .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
        .await;
    let identities = |settings: &magi_settings_store::SettingsStore| {
        magi_conversation_runtime::main_model_sources::sources(settings)
            .unwrap()
            .into_iter()
            .map(|source| source.identity)
            .collect::<Vec<_>>()
    };
    assert_eq!(identities(&settings), ["plugin/acme.guard/proxy"]);

    // 用户停用插件：宿主回收后，数据源随之撤销。
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    fixture.runtime.reconcile().await;
    assert!(identities(&settings).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_catalog_changes_reach_the_real_tool_entry_and_stop_with_the_host() {
    use super::super::plugin_bridge::execute_tool;
    use magi_core::ExecutionResultStatus::{Rejected, Succeeded};
    let fixture = fixture(Some(test_node()));
    let settings = Arc::new(magi_settings_store::SettingsStore::new());
    fixture.runtime.bind_settings(&settings);
    let manifest = json!({
        "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Dynamic", "description": "",
        "extension": {"hooks": [], "runtimeContributions": ["tools", "modelSources"]},
        "permissions": [{"kind": "extension", "targets": ["full-trust"]}, {"kind": "tools", "targets": ["*"]}, {"kind": "modelSources", "targets": ["*"]}],
        "dataSchemaVersion": 1, "settingsSchema": {"type": "object"},
        "contributions": {"commands": [{"id": "configure", "title": "Configure", "description": ""}]}
    });
    let entry = br#"
export default async function activate(api) {
  let revision = 0, release;
  await api.command('configure', async ({ enabled }) => {
    await release?.();
    const tools = enabled ? [{ id: 'late', title: 'Late', description: '', readOnly: true,
      inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } }] : [];
    const modelSources = enabled ? [{ id: 'proxy', title: 'Proxy', description: '' }] : [];
    ({ revision } = await api.catalog.replace(revision, { tools, modelSources }));
    release = enabled ? await api.tool('late', async ({ text }) => ({ echoed: text })) : undefined;
    if (enabled) await api.modelSource.set('proxy', {
      baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'k', apiProtocol: 'openai_chat', models: [{ id: 'm1' }],
    });
    return 'updated';
  });
}
"#;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", entry.to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    install_enabled(&fixture, &package);
    fixture.runtime.reconcile().await;
    fixture
        .runtime
        .running
        .lock()
        .await
        .get("acme.guard")
        .and_then(|entry| entry.supervisor.as_ref())
        .unwrap()
        .wait_for(|state| {
            matches!(
                state,
                SupervisorState::Running { .. } | SupervisorState::Failed(_)
            )
        })
        .await;
    let instance = fixture.bus.instance("acme.guard").expect("真实宿主已就绪");
    assert!(
        fixture
            .manager
            .lock()
            .unwrap()
            .tool_contributions()
            .unwrap()
            .is_empty()
    );
    let configure = |enabled| {
        instance.invoke_command(
            "configure",
            json!({"enabled": enabled}),
            json!({}),
            std::time::Duration::from_secs(3),
            std::future::pending(),
        )
    };
    configure(true).await.unwrap();
    assert_eq!(
        fixture
            .manager
            .lock()
            .unwrap()
            .tool_contributions()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        magi_conversation_runtime::main_model_sources::sources(&settings)
            .unwrap()
            .len(),
        1
    );
    let run = |input: &'static str| {
        let manager = Arc::clone(&fixture.manager);
        let bus = Arc::clone(&fixture.bus);
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            execute_tool(
                &manager,
                &bus,
                &handle,
                &magi_plugin_system::model_tool_name("acme.guard", "late"),
                input,
                &magi_tool_runtime::ToolExecutionContext::default(),
                &std::sync::atomic::AtomicBool::new(false),
            )
        })
    };
    let (output, status) = run(r#"{"text":"live"}"#).await.unwrap();
    assert_eq!(status, Succeeded, "{output}");
    assert!(output.contains("live"));
    assert_eq!(run(r#"{"text":1}"#).await.unwrap().1, Rejected);
    configure(false).await.unwrap();
    assert!(
        fixture
            .manager
            .lock()
            .unwrap()
            .tool_contributions()
            .unwrap()
            .is_empty()
    );
    assert!(
        magi_conversation_runtime::main_model_sources::sources(&settings)
            .unwrap()
            .is_empty()
    );
    assert_eq!(run(r#"{"text":"removed"}"#).await.unwrap().1, Rejected);
    configure(true).await.unwrap();
    assert_eq!(run(r#"{"text":"rebound"}"#).await.unwrap().1, Succeeded);
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    fixture.runtime.reconcile().await;
    assert!(
        fixture
            .manager
            .lock()
            .unwrap()
            .tool_contributions()
            .unwrap()
            .is_empty()
    );
    assert!(!instance.is_alive());
}

#[tokio::test]
async fn registry_change_events_drive_the_reconciler() {
    use magi_event_bus::EventEnvelope;
    let fixture = fixture(Some(test_node()));
    let events = Arc::new(InMemoryEventBus::new(32));
    fixture.runtime.start(Arc::clone(&events));
    install_enabled(&fixture, &package());
    events.publish(EventEnvelope::domain(
        magi_core::EventId::unique("plugin-registry-changed"),
        "plugin.registry.changed",
        json!({}),
    ));
    let mut blocked = false;
    for _ in 0..100 {
        let outcome = fixture
            .bus
            .invoke("tool.call", &HookContext::new("inv"), call("rm x"))
            .await
            .unwrap();
        if matches!(outcome.verdict, Verdict::Blocked { ref reason, .. } if reason.contains("禁止 rm"))
        {
            blocked = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(blocked, "事件之后扩展必须被拉起并生效");
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    events.publish(EventEnvelope::domain(
        magi_core::EventId::unique("plugin-registry-changed"),
        "plugin.registry.changed",
        json!({}),
    ));
    for _ in 0..100 {
        if !fixture.runtime.is_running("acme.guard").await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("停用事件之后扩展必须被回收");
}

#[tokio::test]
async fn without_a_node_runtime_guard_hooks_fail_closed_instead_of_silently_disappearing() {
    let fixture = fixture(None);
    let package = package();
    install_enabled(&fixture, &package);
    fixture.runtime.reconcile().await;
    assert!(
        fixture
            .runtime
            .supervisor_state("acme.guard")
            .await
            .is_none()
    );
    let outcome = fixture
        .bus
        .invoke("tool.call", &HookContext::new("inv"), call("ls"))
        .await
        .unwrap();
    assert!(
        matches!(outcome.verdict, Verdict::Blocked { .. }),
        "{outcome:?}"
    );
}

#[test]
fn supervisor_states_map_to_user_visible_runtime_status_with_the_blocking_guards() {
    let guards = vec!["tool.call".to_string()];
    let running = runtime_status(&SupervisorState::Running { generation: 3 }, &guards, 1);
    assert_eq!(running.state, PluginRuntimeState::Running);
    assert_eq!(
        (running.generation, running.crashes),
        (3, 2),
        "代次推出的重启次数"
    );
    assert!(running.blocking.is_empty(), "运行中没有被阻止的动作");

    let restarting = runtime_status(
        &SupervisorState::Backoff {
            crashes: 2,
            reason: "宿主进程退出".into(),
        },
        &guards,
        0,
    );
    assert_eq!(restarting.state, PluginRuntimeState::Restarting);
    assert_eq!(restarting.reason.as_deref(), Some("宿主进程退出"));
    assert_eq!(
        restarting.blocking, guards,
        "不可用期间 guard 钩子在阻止动作"
    );

    let failed = runtime_status(&SupervisorState::Failed("崩溃过于频繁".into()), &guards, 6);
    assert_eq!(
        (failed.state, failed.crashes),
        (PluginRuntimeState::Failed, 6)
    );
    assert_eq!(failed.blocking, guards);
}

#[tokio::test]
async fn runtime_status_follows_the_lifecycle_and_notifies_the_ui() {
    use magi_api::ExtensionStatusSource;
    let fixture = fixture(Some(test_node()));
    let events = Arc::new(InMemoryEventBus::new(32));
    let mut feed = events.subscribe();
    fixture.runtime.start(Arc::clone(&events));
    install_enabled(&fixture, &package());
    fixture.runtime.reconcile().await;
    // 状态变化事件到达，状态最终为运行中，且没有被阻止的动作。
    let mut running = None;
    for _ in 0..100 {
        if let Some(status) = fixture.runtime.status("acme.guard")
            && status.state == PluginRuntimeState::Running
        {
            running = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let running = running.expect("扩展应进入运行状态");
    assert!(running.blocking.is_empty());
    assert_eq!(
        running.instance_id.as_deref(),
        Some(
            fixture
                .bus
                .instance("acme.guard")
                .unwrap()
                .instance_id
                .as_str()
        )
    );
    let mut saw_event = false;
    while let Ok(event) = feed.try_recv() {
        saw_event |= event.event_type == "plugin.extension.status.changed";
    }
    assert!(saw_event, "状态变化必须通知界面");
    // 停用后状态消失（界面回到普通的已停用显示）。
    fixture
        .manager
        .lock()
        .unwrap()
        .disable("acme.guard")
        .unwrap();
    fixture.runtime.reconcile().await;
    assert!(fixture.runtime.status("acme.guard").is_none());
    fixture
        .manager
        .lock()
        .unwrap()
        .enable("acme.guard")
        .unwrap();
    fixture.runtime.reconcile().await;
    let next = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(status) = fixture.runtime.status("acme.guard")
                && status.state == PluginRuntimeState::Running
                && status.instance_id.is_some()
            {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        next.generation, running.generation,
        "监管局部代次会重复，不能作为文档身份"
    );
    assert_ne!(
        next.instance_id, running.instance_id,
        "同版重新启用必须签发新实例身份"
    );
}

#[tokio::test]
async fn a_missing_node_runtime_is_visible_as_failed_with_the_reason_and_blocking_guards() {
    use magi_api::ExtensionStatusSource;
    let fixture = fixture(None);
    install_enabled(&fixture, &package());
    fixture.runtime.reconcile().await;
    let status = fixture
        .runtime
        .status("acme.guard")
        .expect("失败也必须可见");
    assert_eq!(status.state, PluginRuntimeState::Failed);
    assert!(status.reason.unwrap().contains("随包 Node"));
    assert_eq!(
        status.blocking,
        ["tool.call"],
        "用户要能看到哪些动作正被 guard 阻止"
    );
}
