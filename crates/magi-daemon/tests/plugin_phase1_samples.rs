//! Cross-process original DSH package verification over `PluginRuntime`,
//! `PluginWebGateway`, `PluginServicesBridge`, and `SettingsStore`:
//! 1. `dsh-rea@1.2.0`
//! 2. `dsh-plugin-ops-bundle@0.20.8`
//! 3. `dsh-free-search@0.9.41`
//! 4. `dsh-connect-workbuddy@5.21.2`

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use magi_api::NpmArchive;
use magi_conversation_runtime::main_model_sources;
use magi_daemon::{
    plugin_dependencies::{PluginDependencyToolchain, prepare_plugin_dependencies},
    plugin_management::PluginBundleManagementConfig,
    plugin_runtime::{PluginRuntime, PluginRuntimeConfig},
    plugin_services::HostLlmInvokeResponse,
    plugin_web_gateway::PluginWebGateway,
};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
};
use magi_plugin_system::{
    PluginManager,
    loading_runtime::{LoadingPreparation, PluginRoutePage},
};
use magi_process::ManagedProcessGroup;
use magi_settings_store::SettingsStore;
use magi_tool_runtime::ToolExecutionContext;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};
use tokio::sync::mpsc;

struct StatusSink(mpsc::UnboundedSender<Value>);

impl Incoming for StatusSink {
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
            let _ = self.0.send(params);
        }
    }
}

fn resolve_test_toolchain(temp_root: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let packaged_root =
        repo_root.join("target/electron-dist/mac-arm64/Magi.app/Contents/Resources/daemon");
    let packaged_node = packaged_root.join("node/node");
    let packaged_npm = packaged_root.join("node/npm/bin/npm-cli.js");
    let packaged_dsh = packaged_root.join("dsh");

    if packaged_node.is_file() && packaged_npm.is_file() && packaged_dsh.is_dir() {
        return Some((
            fs::canonicalize(packaged_node).ok()?,
            fs::canonicalize(packaged_npm).ok()?,
            fs::canonicalize(packaged_dsh).ok()?,
        ));
    }

    let node = std::env::var_os("MAGI_EXTENSION_NODE")
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"))?;
    let npm_cli = std::env::var_os("MAGI_DSH_NPM_CLI")
        .map(PathBuf::from)
        .or_else(|| {
            let npm_bin = magi_process::resolve_executable("npm")?;
            fs::canonicalize(npm_bin).ok()
        })?;
    let dsh_source = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host/runtime/dsh"),
    )
    .ok()?;
    if !dsh_source.join("node_modules/@deepseek-ai/cordis").is_dir() {
        return None;
    }
    let sdk_project = temp_root.join("staged-sdk");
    let runtime_dir = sdk_project.join("runtime");
    fs::create_dir_all(&runtime_dir).ok()?;
    for entry in fs::read_dir(&dsh_source).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("mjs") {
            fs::copy(&path, runtime_dir.join(entry.file_name())).ok()?;
        }
    }
    // 依赖准备脚本从 `runtime/` 旁读取 package.json，与打包布局（dsh/runtime/package.json）一致。
    fs::copy(
        dsh_source.join("package.json"),
        runtime_dir.join("package.json"),
    )
    .ok()?;
    for name in ["package.json", "package-lock.json"] {
        fs::copy(dsh_source.join(name), sdk_project.join(name)).ok()?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dsh_source.join("node_modules"),
        sdk_project.join("node_modules"),
    )
    .ok()?;
    Some((node, npm_cli, sdk_project))
}

async fn wait_instance_running(
    rx: &mut mpsc::UnboundedReceiver<Value>,
    instance_id: &str,
    min_generation: u64,
) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(msg) = rx.recv().await {
            if msg["instanceId"].as_str() == Some(instance_id)
                && msg["state"].as_str() == Some("running")
                && msg["hostGeneration"].as_u64().unwrap_or(0) >= min_generation
            {
                return;
            }
        }
        panic!(
            "status channel closed before {instance_id} reached running (gen >= {min_generation})"
        );
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {instance_id} to reach running"));
}

async fn gateway_call(
    gateway: &PluginWebGateway,
    page: &PluginRoutePage,
    public_host: &str,
    origin: Option<&str>,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header(header::HOST, public_host);
    if let Some(orig) = origin {
        builder = builder.header(header::ORIGIN, orig);
    }
    let req_body = match body {
        Some(val) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&val).unwrap())
        }
        None => Body::empty(),
    };
    let req = builder.body(req_body).unwrap();
    let resp = gateway.forward(page, req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
    let parsed = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| json!({ "_raw": String::from_utf8_lossy(&bytes).to_string() }));
    (status, parsed)
}

async fn typert_rpc(
    gateway: &PluginWebGateway,
    page: &PluginRoutePage,
    public_host: &str,
    endpoint: &str,
    args: Value,
) -> (StatusCode, Value) {
    let origin = format!("http://{public_host}");
    gateway_call(
        gateway,
        page,
        public_host,
        Some(&origin),
        Method::POST,
        &format!("/api/{endpoint}"),
        Some(json!({
            "type": "client-request",
            "rpcId": "rpc-1",
            "method": endpoint,
            "payload": { "args": args },
        })),
    )
    .await
}

#[tokio::test]
async fn fixed_original_dsh_rea_1_2_0_remote_status_and_volatile_settings() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rea_tgz = repo_root.join("target/dsh-phase-1/originals/dsh-rea-1.2.0.tgz");
    if !rea_tgz.is_file() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let registry =
        std::env::var("MAGI_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let processes = ManagedProcessGroup::new();
    let project_dir = root.join("project");
    fs::create_dir_all(&project_dir).unwrap();
    let archive = NpmArchive::from_tarball_bytes(fs::read(&rea_tgz).unwrap()).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry,
    };
    let prepared = prepare_plugin_dependencies(&processes, &toolchain, &project_dir, &[archive])
        .await
        .unwrap();
    let inspections: Vec<_> = prepared.into_iter().map(|(i, _)| i).collect();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: rea\n      name: dsh-rea\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&project_dir).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: inspections,
    };

    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49201";
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node,
            sdk_project,
            endpoint_dir: root.join("sockets"),
            public_origin: format!("http://{public_host}"),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli,
                projects_dir: root.join("projects"),
                registry,
            }),
        },
        initial_prep,
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();

    wait_instance_running(&mut statuses, "include:rea", 1).await;

    let gateway = runtime.gateway();
    let host_page = gateway.bind_host_page().unwrap();

    // 1. Call `rea/status` Remote endpoint registered by original `dsh-rea@1.2.0`
    let (st, status_res) =
        typert_rpc(&gateway, &host_page, public_host, "rea/status", json!({})).await;
    assert_eq!(st, StatusCode::OK, "rea/status response: {status_res}");
    assert_eq!(status_res["result"]["ok"], true);
    let rea_status = &status_res["result"]["value"];
    assert_eq!(rea_status["timeoutMs"], 300_000);

    // 2. Live volatile settings mutation via `settings/mutate` -> `loading.profilePatch.edit`
    let ghidra_dir = root.join("mock-ghidra").to_string_lossy().to_string();
    let (st, mutate_res) = typert_rpc(
        &gateway,
        &host_page,
        public_host,
        "settings/mutate",
        json!({
            "ns": "rea",
            "ops": [
                { "op": "set", "path": ["ghidraInstallDir"], "value": ghidra_dir },
                { "op": "set", "path": ["toolCallTimeoutMs"], "value": 120_000 },
            ],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "settings/mutate response: {mutate_res}");
    assert_eq!(mutate_res["result"]["ok"], true);

    // 3. Verify `rea/status` immediately reflects the updated volatile settings without restarting host
    let (st, status_after) =
        typert_rpc(&gateway, &host_page, public_host, "rea/status", json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(status_after["result"]["value"]["timeoutMs"], 120_000);
    let patch_text = fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap();
    assert!(
        patch_text.contains("120000"),
        "cordis.patch.yml should persist mutated timeout: {patch_text}"
    );

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn fixed_original_dsh_plugin_ops_bundle_0_20_8_guard_chat_deposit_and_restart() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ops_tgz = repo_root.join("target/dsh-phase-1/originals/dsh-plugin-ops-bundle-0.20.8.tgz");
    if !ops_tgz.is_file() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let registry =
        std::env::var("MAGI_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let processes = ManagedProcessGroup::new();
    let project_dir = root.join("project");
    fs::create_dir_all(&project_dir).unwrap();
    let archive = NpmArchive::from_tarball_bytes(fs::read(&ops_tgz).unwrap()).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry,
    };
    let prepared = prepare_plugin_dependencies(&processes, &toolchain, &project_dir, &[archive])
        .await
        .unwrap();
    let inspections: Vec<_> = prepared.into_iter().map(|(i, _)| i).collect();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: dsh-ops-bundle\n      name: dsh-plugin-ops-bundle\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&project_dir).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: inspections,
    };

    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49202";
    let origin = format!("http://{public_host}");
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node: node.clone(),
            sdk_project: sdk_project.clone(),
            endpoint_dir: root.join("sockets"),
            public_origin: origin.clone(),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli: npm_cli.clone(),
                projects_dir: root.join("projects"),
                registry: registry.clone(),
            }),
        },
        initial_prep.clone(),
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();

    wait_instance_running(&mut statuses, "include:dsh-ops-bundle", 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 1. Sandbox tools registered into `ctx.tools` and surfaced in `tool_catalog_provider`
    let catalog = (runtime.services().tool_catalog_provider())();
    assert!(
        !catalog.is_empty(),
        "expected dsh-plugin-ops-bundle sandbox tools in catalog"
    );

    // 2. Bind page & verify `HostConnectionService` Origin fence on `/dsh-ops/*`
    let act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-ops-bundle")
        .unwrap()
        .clone();
    let gateway = runtime.gateway();
    let page = gateway.bind_page(&act).unwrap();

    let (evil_st, _) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some("https://evil.example"),
        Method::GET,
        "/dsh-ops/api/sandbox/status",
        None,
    )
    .await;
    assert_eq!(evil_st, StatusCode::FORBIDDEN);

    let (ok_st, sb_status) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::GET,
        "/dsh-ops/api/sandbox/status",
        None,
    )
    .await;
    assert_eq!(ok_st, StatusCode::OK, "sandbox status: {sb_status}");

    // 3. Inject deterministic `HostLlmInvoker` and run `/dsh-ops/api/chat` + `/dsh-ops/api/knowledge/deposit`
    runtime.services().set_llm_invoker(Some(Arc::new(|req| {
        Box::pin(async move {
            let content = if req.messages.len() == 1 {
                r#"{"title":"Schemastery Duplicate Fix","symptom":"z.string().volatile is not a function","cause":"Nested @deepseek-ai/schemastery 3.18.1 shadowed host singleton","fix":"Remove nested schemastery copy and use host singleton","tags":["schemastery","volatile"]}"#.to_string()
            } else {
                format!(
                    "诊断结论（模型 {}）：插件树健康，无需额外修复。",
                    req.model
                )
            };
            Ok(HostLlmInvokeResponse {
                content,
                reasoning: None,
                tool_calls: Vec::new(),
                usage: None,
            })
        })
    })));

    let (chat_st, chat_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/dsh-ops/api/chat",
        Some(json!({
            "lang": "zh",
            "messages": [{ "role": "user", "content": "请检查当前插件树状态" }],
        })),
    )
    .await;
    // The official Ops package reads the DSH CLI `web` profile before it
    // invokes the model. Magi deliberately does not materialize or emulate
    // that profile. Keep the real upstream failure as the product result and
    // continue with the independent knowledge business below.
    assert_eq!(chat_st, StatusCode::BAD_REQUEST, "chat body: {chat_body}");
    assert!(
        chat_body["error"].as_str().is_some_and(
            |message| message.contains("profile \"web\"") && message.contains("package.json")
        ),
        "unexpected Ops profile diagnostic: {chat_body}"
    );

    let (dep_st, dep_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/dsh-ops/api/knowledge/deposit",
        Some(json!({
            "lang": "zh",
            "messages": [
                { "role": "user", "content": "volatile 报错怎么解决？" },
                { "role": "assistant", "content": "清理嵌套的旧版 schemastery 即可。" }
            ],
        })),
    )
    .await;
    assert_eq!(dep_st, StatusCode::OK, "deposit body: {dep_body}");
    assert_eq!(dep_body["ok"], true);
    let deposited_id = dep_body["id"].as_str().unwrap().to_owned();

    // 4. Restart shared host and verify deposited knowledge persists across restart!
    let new_gen = runtime.restart().await.unwrap();
    wait_instance_running(&mut statuses, "include:dsh-ops-bundle", new_gen).await;
    let act_after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(binding) = owner_arc
                .lock()
                .unwrap()
                .loading_instance_binding("include:dsh-ops-bundle")
                .cloned()
                && binding.host_generation == new_gen
            {
                return binding;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let page_after = gateway.bind_page(&act_after).unwrap();
    let (kn_st, kn_body) = gateway_call(
        &gateway,
        &page_after,
        public_host,
        Some(&origin),
        Method::GET,
        "/dsh-ops/api/knowledge",
        None,
    )
    .await;
    assert_eq!(kn_st, StatusCode::OK, "knowledge list: {kn_body}");
    let entries_str = kn_body.to_string();
    assert!(
        entries_str.contains(&deposited_id) || entries_str.contains("Schemastery Duplicate Fix"),
        "expected deposited knowledge after host restart, got: {kn_body}"
    );

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn fixed_original_dsh_free_search_0_9_41_settings_credentials_tools_and_fetch() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fs_tgz = repo_root.join("target/dsh-phase-1/originals/dsh-free-search-0.9.41.tgz");
    if !fs_tgz.is_file() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let registry =
        std::env::var("MAGI_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let processes = ManagedProcessGroup::new();
    let project_dir = root.join("project");
    fs::create_dir_all(&project_dir).unwrap();
    let archive = NpmArchive::from_tarball_bytes(fs::read(&fs_tgz).unwrap()).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry,
    };
    let prepared = prepare_plugin_dependencies(&processes, &toolchain, &project_dir, &[archive])
        .await
        .unwrap();
    let inspections: Vec<_> = prepared.into_iter().map(|(i, _)| i).collect();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: web-search-free\n      name: dsh-free-search\n      config:\n        disabledEngines: []\n        bingMarket: zh-CN\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&project_dir).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: inspections,
    };

    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49203";
    let origin = format!("http://{public_host}");
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node,
            sdk_project,
            endpoint_dir: root.join("sockets"),
            public_origin: origin.clone(),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli,
                projects_dir: root.join("projects"),
                registry,
            }),
        },
        initial_prep,
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();

    wait_instance_running(&mut statuses, "include:web-search-free", 1).await;
    tokio::time::sleep(Duration::from_millis(80)).await;

    // 1. Verify web provider registration + automatic takeover of `searchProvider` (`ddg`)
    let web_snap = runtime.services().web_snapshot();
    assert!(
        web_snap.search.iter().any(|p| p.id == "ddg"),
        "expected ddg search provider: {web_snap:?}"
    );
    assert!(
        web_snap.fetch.iter().any(|p| p.id == "fs-quality-fetch"),
        "expected fs-quality-fetch provider: {web_snap:?}"
    );
    assert_eq!(web_snap.search_provider.as_deref(), Some("ddg"));

    // 2. Verify tools registered in `PluginToolCatalogProvider`
    let catalog = (runtime.services().tool_catalog_provider())();
    assert!(
        catalog
            .iter()
            .any(|t| t.model_tool_name == "free_search_test"),
        "expected free_search_test in catalog: {catalog:?}"
    );

    // 3. Exercise `/api/dsh-free-search-settings/describe`, `credentials-set`, `credentials-status`, and `mutate`
    let act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:web-search-free")
        .unwrap()
        .clone();
    let gateway = runtime.gateway();
    let page = gateway.bind_page(&act).unwrap();

    let (desc_st, desc_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/api/dsh-free-search-settings/describe",
        Some(json!({})),
    )
    .await;
    assert_eq!(desc_st, StatusCode::OK, "describe: {desc_body}");
    assert_eq!(desc_body["ok"], true);

    let secret_val = "tvly-super-secret-test-key-999";
    let (cred_st, cred_set) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/api/dsh-free-search-settings/credentials-set",
        Some(json!({ "key": "tavilyApiKey", "value": secret_val })),
    )
    .await;
    assert_eq!(cred_st, StatusCode::OK, "cred set: {cred_set}");
    assert_eq!(cred_set["ok"], true);

    let (cs_st, cs_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/api/dsh-free-search-settings/credentials-status",
        Some(json!({})),
    )
    .await;
    assert_eq!(cs_st, StatusCode::OK);
    assert_eq!(cs_body["value"]["configured"]["tavilyApiKey"], true);
    assert!(
        !cs_body.to_string().contains(secret_val),
        "secret value must never leak in credentials-status"
    );

    let (mut_st, mut_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::POST,
        "/api/dsh-free-search-settings/mutate",
        Some(json!({
            "ns": "web-search-free",
            "ops": [{ "op": "set", "path": ["provider"], "value": "bing" }],
        })),
    )
    .await;
    assert_eq!(mut_st, StatusCode::OK, "mutate: {mut_body}");
    assert_eq!(mut_body["ok"], true);
    let patch_text = fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap();
    assert!(
        patch_text.contains("bing"),
        "expected provider: bing written to cordis.patch.yml, got: {patch_text}"
    );

    // 4. Verify `web_executor` routes `web_fetch` to `fs-quality-fetch` (deterministic SSRF rejection on loopback)
    let web_exec = runtime
        .services()
        .web_executor(tokio::runtime::Handle::current());
    let fetch_loopback = tokio::task::spawn_blocking(move || {
        let cancelled = AtomicBool::new(false);
        web_exec(
            "web_fetch",
            r#"{"url":"http://127.0.0.1:9/private"}"#,
            &ToolExecutionContext::default(),
            &cancelled,
        )
    })
    .await
    .unwrap();
    let fetch_json: Value = serde_json::from_str(&fetch_loopback.unwrap()).unwrap();
    assert_eq!(fetch_json["tool"], "web_fetch");
    assert_eq!(fetch_json["status"], "failed");

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn fixed_original_dsh_connect_workbuddy_5_21_2_llm_catalog_routes_and_lifecycle() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wb_tgz = repo_root.join("target/dsh-phase-1/originals/dsh-connect-workbuddy-5.21.2.tgz");
    if !wb_tgz.is_file() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let registry =
        std::env::var("MAGI_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let processes = ManagedProcessGroup::new();
    let project_dir = root.join("project");
    fs::create_dir_all(&project_dir).unwrap();
    let archive = NpmArchive::from_tarball_bytes(fs::read(&wb_tgz).unwrap()).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry,
    };
    let prepared = prepare_plugin_dependencies(&processes, &toolchain, &project_dir, &[archive])
        .await
        .unwrap();
    let inspections: Vec<_> = prepared.into_iter().map(|(i, _)| i).collect();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: dsh-connect-workbuddy\n      name: dsh-connect-workbuddy\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&project_dir).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: inspections,
    };

    let settings_store = Arc::new(SettingsStore::new());
    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49204";
    let origin = format!("http://{public_host}");
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node,
            sdk_project,
            endpoint_dir: root.join("sockets"),
            public_origin: origin.clone(),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli,
                projects_dir: root.join("projects"),
                registry,
            }),
        },
        initial_prep,
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();
    runtime
        .services()
        .bind_settings_store(settings_store.clone());

    wait_instance_running(&mut statuses, "include:dsh-connect-workbuddy", 1).await;

    // Wait up to 5s for async `llm/adapters-updated` -> `host.llm.providersChanged` projection
    let projected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sources = main_model_sources::sources(&settings_store).unwrap();
            let wb: Vec<_> = sources
                .into_iter()
                .filter(|s| s.plugin_id.as_deref() == Some("dsh-connect-workbuddy"))
                .collect();
            if !wb.is_empty() {
                return wb;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("expected dsh-connect-workbuddy LLM sources projected into SettingsStore");
    assert!(
        projected
            .iter()
            .any(|s| s.identity.starts_with("plugin/dsh-connect-workbuddy/")),
        "projected identities: {projected:?}"
    );

    // 2. Call plugin's own HTTP route `/plugins/dsh-connect-workbuddy/usage?region=cn&detail=fast`
    let act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-connect-workbuddy")
        .unwrap()
        .clone();
    let gateway = runtime.gateway();
    let page = gateway.bind_page(&act).unwrap();
    let (usage_st, usage_body) = gateway_call(
        &gateway,
        &page,
        public_host,
        Some(&origin),
        Method::GET,
        "/plugins/dsh-connect-workbuddy/usage?region=cn&detail=fast",
        None,
    )
    .await;
    assert_eq!(
        usage_st,
        StatusCode::OK,
        "workbuddy usage route: {usage_body}"
    );

    // 3. Verify `settings/describe` exposes `dsh-connect-workbuddy` schema namespace
    let host_page = gateway.bind_host_page().unwrap();
    let (desc_st, desc_res) = typert_rpc(
        &gateway,
        &host_page,
        public_host,
        "settings/describe",
        json!({}),
    )
    .await;
    assert_eq!(desc_st, StatusCode::OK);
    let namespaces = desc_res["result"]["value"]["namespaces"]
        .as_array()
        .unwrap();
    assert!(
        namespaces
            .iter()
            .any(|ns| ns["ns"] == "dsh-connect-workbuddy"),
        "expected dsh-connect-workbuddy in settings namespaces"
    );

    // 4. Stopping the runtime immediately clears plugin LLM sources from `SettingsStore`!
    runtime.stop().await.unwrap();
    let remaining = main_model_sources::sources(&settings_store).unwrap();
    assert!(
        remaining
            .iter()
            .all(|s| s.plugin_id.as_deref() != Some("dsh-connect-workbuddy")),
        "plugin LLM sources must be revoked after host stops: {remaining:?}"
    );
}
