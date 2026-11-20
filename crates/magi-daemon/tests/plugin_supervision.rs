//! Real cross-process regression for shared-host crash supervision, route lease
//! fencing across generations, activating-crash auto-disable and public ctx.loader.exit().

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_daemon::plugin_runtime::{
    PluginRuntime, PluginRuntimeConfig, SharedHostRestartPolicy, SharedHostSupervisorState,
};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::PackageInspection,
};
use magi_plugin_system::{
    PluginManager,
    loading_runtime::{LoadingPreparation, LoadingSourceObservation},
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;

struct EventSink {
    statuses: mpsc::UnboundedSender<Value>,
    diagnostics: mpsc::UnboundedSender<Value>,
}

impl Incoming for EventSink {
    fn request(&self, _: String, _: Value) -> BoxFuture<Result<Value, RpcError>> {
        Box::pin(async {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                "Service unavailable",
            ))
        })
    }

    fn notification(&self, method: String, params: Value) {
        if method == "plugin.status" {
            let _ = self.statuses.send(params);
        } else if method == "host.diagnostic" {
            let _ = self.diagnostics.send(params);
        }
    }
}

fn runtime_sdk_project(project_dir: &Path) -> PathBuf {
    let sdk = project_dir.join("sdk");
    fs::create_dir_all(sdk.join("runtime")).unwrap();
    let repo_runtime =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/magi-extension-host/runtime/dsh");
    let repo_runtime = fs::canonicalize(repo_runtime).unwrap();
    for entry in fs::read_dir(&repo_runtime).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".mjs") || name_str == "package.json" {
            fs::copy(entry.path(), sdk.join("runtime").join(&name)).unwrap();
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        repo_runtime.join("node_modules"),
        sdk.join("runtime/node_modules"),
    )
    .unwrap();
    sdk
}

fn write_package(project_dir: &Path, name: &str, code: &str) -> PackageInspection {
    let pkg_dir = project_dir.join("node_modules").join(name);
    fs::create_dir_all(&pkg_dir).unwrap();
    let manifest = json!({
        "name": name,
        "version": "1.0.0",
        "type": "module",
        "exports": "./index.mjs",
        "engines": { "dsh": ">=0.1.0" }
    })
    .to_string();
    fs::write(pkg_dir.join("package.json"), &manifest).unwrap();
    fs::write(pkg_dir.join("index.mjs"), code).unwrap();
    PackageInspection {
        project_directory: project_dir.to_path_buf(),
        directory: pkg_dir,
        name: name.into(),
        version: "1.0.0".into(),
        manifest_sha256: magi_core::hex_lower(sha2::Sha256::digest(manifest.as_bytes())),
    }
}

async fn wait_running(
    label: &str,
    statuses: &mut mpsc::UnboundedReceiver<Value>,
    instance_id: &str,
    min_host_gen: u64,
) -> Value {
    let mut seen = Vec::new();
    match tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = statuses.recv().await.expect("status channel open");
            seen.push(status.clone());
            if status["instanceId"] == instance_id
                && status["state"] == "running"
                && status["hostGeneration"].as_u64().unwrap_or(0) >= min_host_gen
            {
                return status;
            }
        }
    })
    .await
    {
        Ok(status) => status,
        Err(_) => panic!(
            "{label}: timed out waiting for {instance_id} >= gen {min_host_gen}; seen={seen:?}"
        ),
    }
}

#[tokio::test]
async fn supervised_runtime_handles_loader_exit_activating_crash_disable_and_three_crash_circuit_breaker()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let project_dir = root.join("project");
    fs::create_dir_all(project_dir.join("node_modules")).unwrap();
    fs::write(project_dir.join("package.json"), r#"{"private":true}"#).unwrap();

    let repo_dsh_modules = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/magi-extension-host/runtime/dsh/node_modules/@deepseek-ai"),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &repo_dsh_modules,
        project_dir.join("node_modules/@deepseek-ai"),
    )
    .unwrap();

    let sdk_project = runtime_sdk_project(&root);
    let healthy_pkg = write_package(
        &project_dir,
        "healthy",
        r#"
        import { readJsonBody } from './helper.mjs';
        export const inject = ['webServer', 'loader'];
        export function apply(ctx, config) {
          ctx.webServer.register({
            kind: 'exact',
            path: '/api/healthy/info',
            handler: (_req, res) => {
              const entries = [...ctx.loader.entries()].map(e => e.id);
              res.setHeader('content-type', 'application/json');
              res.end(JSON.stringify({ label: config?.label ?? 'init', entries }));
            },
          });
          ctx.webServer.register({
            kind: 'exact',
            path: '/api/healthy/reload-host',
            handler: async (_req, res) => {
              await ctx.loader.resolve('include').subtree.create({ id: 'extra-group', name: 'cordis:group', group: true, config: [] });
              ctx.loader.exit();
              res.end('restarting');
            },
          });
          ctx.webServer.register({
            kind: 'exact',
            path: '/api/healthy/crash-now',
            handler: () => {
              setImmediate(() => process.exit(23));
            },
          });
        }
        "#,
    );
    fs::write(
        project_dir.join("node_modules/healthy/helper.mjs"),
        "export const readJsonBody = () => ({});\n",
    )
    .unwrap();
    let bad_pkg = write_package(
        &project_dir,
        "crash-on-activate",
        r#"
        export function apply() {
          process.exit(17);
        }
        "#,
    );
    #[cfg(unix)]
    let interrupted_pkg = write_package(
        &project_dir,
        "interrupted",
        r#"
        import { writeFileSync } from 'node:fs';
        export async function apply(_, { ready }) {
          try {
            writeFileSync(ready, String(process.pid), { flag: 'wx' });
          } catch (error) {
            if (error.code === 'EEXIST') return;
            throw error;
          }
          await new Promise(() => {});
        }
        "#,
    );

    let state_dir = root.join("state");
    let manager = PluginManager::open(&state_dir).unwrap();
    let sources_dir = manager.loading_source_root();
    fs::create_dir_all(&sources_dir).unwrap();
    fs::write(
        sources_dir.join("cordis.yml"),
        "- id: healthy\n  name: healthy\n  config:\n    label: v1\n",
    )
    .unwrap();
    fs::write(sources_dir.join("cordis.patch.yml"), "[]\n").unwrap();

    let node = std::env::var_os("MAGI_EXTENSION_NODE")
        .map(PathBuf::from)
        .or_else(|| {
            let candidate = PathBuf::from("/opt/homebrew/bin/node");
            candidate.is_file().then_some(candidate)
        })
        .or_else(|| magi_process::resolve_executable("node"))
        .expect("Node binary required");

    let processes = ManagedProcessGroup::new();
    let config = PluginRuntimeConfig {
        node,
        sdk_project,
        endpoint_dir: root.join("sockets"),
        public_origin: "http://127.0.0.1:48123".into(),
        bundle_management: None,
    };
    let initial = LoadingPreparation {
        project_directory: project_dir.clone(),
        base: "cordis.yml".into(),
        overlays: vec!["cordis.patch.yml".into()],
        packages: vec![
            healthy_pkg,
            bad_pkg,
            #[cfg(unix)]
            interrupted_pkg,
        ],
    };
    let policy = SharedHostRestartPolicy {
        initial_backoff: Duration::from_millis(40),
        max_backoff: Duration::from_millis(120),
        crash_window: Duration::from_secs(300),
        crash_limit: 3,
        probe_interval: Duration::from_secs(2),
        probe_timeout: Duration::from_secs(2),
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
    };
    let owner = Arc::new(Mutex::new(manager));
    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let (diag_tx, mut diagnostics) = mpsc::unbounded_channel();
    let runtime = PluginRuntime::launch_with_policy(
        owner.clone(),
        &processes,
        config,
        initial,
        Arc::new(EventSink {
            statuses: status_tx,
            diagnostics: diag_tx,
        }),
        policy,
    )
    .await
    .unwrap();

    // 1. Generation 1 reaches running and serves HTTP through PluginWebGateway.
    let gen1_status = wait_running("gen1", &mut statuses, "include:healthy", 1).await;
    let gen1 = gen1_status["hostGeneration"].as_u64().unwrap();
    let gateway = runtime.gateway();
    let gen1_binding = owner
        .lock()
        .unwrap()
        .loading_instance_binding("include:healthy")
        .unwrap()
        .clone();
    let gen1_page = gateway.bind_page(&gen1_binding).unwrap();
    let resp = gateway
        .forward(
            &gen1_page,
            Request::builder()
                .uri("/api/healthy/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["label"], "v1");
    assert_eq!(body["entries"], json!(["include", "include:healthy"]));

    // 2. Public ctx.loader.exit() commits pending config edit and cleanly restarts into generation 2
    // without counting as a crash; old gen1 page lease is immediately fenced.
    let _ = gateway
        .forward(
            &gen1_page,
            Request::builder()
                .method("POST")
                .uri("/api/healthy/reload-host")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let gen2_status = wait_running("gen2", &mut statuses, "include:healthy", gen1 + 1).await;
    let gen2 = gen2_status["hostGeneration"].as_u64().unwrap();
    assert!(gen2 > gen1);
    assert!(
        gateway
            .forward(
                &gen1_page,
                Request::builder()
                    .uri("/api/healthy/info")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .is_err(),
        "old host generation page must be revoked after restart"
    );
    let gen2_binding = owner
        .lock()
        .unwrap()
        .loading_instance_binding("include:healthy")
        .unwrap()
        .clone();
    let gen2_page = gateway.bind_page(&gen2_binding).unwrap();
    let resp = gateway
        .forward(
            &gen2_page,
            Request::builder()
                .uri("/api/healthy/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["label"], "v1");
    assert_eq!(
        body["entries"],
        json!(["include", "include:healthy", "include:extra-group"])
    );

    // 3. Enabling a plugin that crashes during activation auto-disables that plugin and recovers healthy!
    fs::write(
        sources_dir.join("cordis.yml"),
        "- id: healthy\n  name: healthy\n  config:\n    label: reloaded\n- id: crasher\n  name: crash-on-activate\n",
    )
    .unwrap();
    let gen3_status = wait_running("gen3", &mut statuses, "include:healthy", gen2 + 1).await;
    let gen3 = gen3_status["hostGeneration"].as_u64().unwrap();
    assert!(gen3 > gen2);
    let saved_yaml = fs::read_to_string(sources_dir.join("cordis.yml")).unwrap();
    assert!(
        saved_yaml.contains("disabled: true"),
        "crashing plugin was auto-disabled in canonical source: {saved_yaml}"
    );
    let auto_disable_diag = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let d = diagnostics.recv().await.unwrap();
            if d["pluginId"] == "crash-on-activate"
                && d["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("已自动停用"))
            {
                return d;
            }
        }
    })
    .await
    .expect("recorded auto-disable diagnostic");
    assert_eq!(auto_disable_diag["instanceId"], "include:crasher");

    // 4. Crash window already has 1 crash from step 3; two more runtime crashes reach 3 crashes in 5 min
    // and trip StoppedByCrashLimit!
    let trigger_runtime_crash = |host_gen: u64| {
        let gateway = gateway.clone();
        let owner = owner.clone();
        async move {
            let binding = loop {
                if let Some(b) = owner
                    .lock()
                    .unwrap()
                    .loading_instance_binding("include:healthy")
                    .cloned()
                    && b.host_generation == host_gen
                {
                    break b;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            let page = loop {
                if let Ok(p) = gateway.bind_page(&binding) {
                    break p;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            let _ = gateway
                .forward(
                    &page,
                    Request::builder()
                        .method("POST")
                        .uri("/api/healthy/crash-now")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
        }
    };

    // Crash #2 -> auto-restarts into gen4.
    trigger_runtime_crash(gen3).await;
    let gen4_status = wait_running("gen4", &mut statuses, "include:healthy", gen3 + 1).await;
    let gen4 = gen4_status["hostGeneration"].as_u64().unwrap();

    // Crash #3 -> hits 3 crashes in 5 minutes -> stops automatic restart!
    let mut state_rx = runtime.subscribe_state();
    trigger_runtime_crash(gen4).await;
    let stopped_state = tokio::time::timeout(
        Duration::from_secs(10),
        state_rx.wait_for(|state| {
            matches!(state, SharedHostSupervisorState::StoppedByCrashLimit { .. })
        }),
    )
    .await
    .expect("supervisor reached StoppedByCrashLimit")
    .unwrap()
    .clone();
    match &stopped_state {
        SharedHostSupervisorState::StoppedByCrashLimit {
            crashes_in_window,
            active_plugins,
            disabled_instances,
            ..
        } => {
            assert_eq!(*crashes_in_window, 3);
            assert_eq!(active_plugins, &vec!["healthy".to_string()]);
            assert_eq!(disabled_instances, &vec!["include:crasher".to_string()]);
        }
        other => panic!("unexpected state: {other:?}"),
    }
    assert!(runtime.try_loading().is_err());

    // 5. Manual restart resets crash window and restores healthy service in generation 5!
    let gen5 = runtime.restart().await.unwrap();
    assert!(gen5 > gen4);
    wait_running("gen5", &mut statuses, "include:healthy", gen5).await;
    let mut obs = runtime.source_observations();
    tokio::time::timeout(
        Duration::from_secs(5),
        obs.wait_for(|v| matches!(v, LoadingSourceObservation::Reconciled { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    let gen5_binding = owner
        .lock()
        .unwrap()
        .loading_instance_binding("include:healthy")
        .unwrap()
        .clone();
    let gen5_page = gateway.bind_page(&gen5_binding).unwrap();
    let resp = gateway
        .forward(
            &gen5_page,
            Request::builder()
                .uri("/api/healthy/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    #[cfg(unix)]
    {
        let ready = root.join("starting-child.pid");
        let revision = owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision;
        runtime
            .loading()
            .edit_sources(
                revision,
                json!({"kind":"create","treeId":"include","options":{
                    "id":"interrupted","name":"interrupted","config":{"ready":ready}
                }}),
            )
            .await
            .unwrap();
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(contents) = fs::read_to_string(&ready)
                    && let Ok(pid) = contents.parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the new plugin was still starting");
        assert!(pid > 1);
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .loading_instance_status("include:interrupted")
                .map(|(_, state)| state.clone()),
            Some(magi_plugin_system::LoadingInstanceStatus::Starting)
        );
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        let observation = wait_running(
            "after external signal",
            &mut statuses,
            "include:interrupted",
            gen5 + 1,
        )
        .await;
        let owner = owner.lock().unwrap();
        assert!(
            owner
                .loading_configuration()
                .unwrap()
                .executables
                .iter()
                .any(|entry| entry.instance_id == "include:interrupted")
        );
        assert!(observation["hostGeneration"].as_u64().unwrap() > gen5);
    }

    runtime.stop().await.unwrap();
}
