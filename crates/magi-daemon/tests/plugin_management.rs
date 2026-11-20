//! Cross-process tests for candidate project materialization, `ctx.pluginManager`,
//! install cancellation, self-removal fencing, native atomic upgrade, and the real
//! `dsh-plugin-mgr@0.2.12` / `0.2.13` HTTP business routes over `PluginWebGateway`.

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path as AxumPath, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use flate2::{Compression, write::GzEncoder};
use magi_api::NpmArchive;
use magi_daemon::{
    plugin_dependencies::{PluginDependencyToolchain, prepare_plugin_dependencies},
    plugin_management::PluginBundleManagementConfig,
    plugin_runtime::{PluginRuntime, PluginRuntimeConfig},
    plugin_web_gateway::PluginWebGateway,
};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
};
use magi_plugin_system::{
    PluginManager,
    loading_runtime::{LoadingPreparation, LoadingSourceObservation, PluginRoutePage},
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha512};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};

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

#[derive(Clone)]
struct MockRelease {
    manifest: Value,
    tarball: Vec<u8>,
    integrity: String,
    published_at: String,
}

type UpstreamCache = Arc<Mutex<BTreeMap<String, (StatusCode, String, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct MockRegistry {
    packages: Arc<Mutex<BTreeMap<String, BTreeMap<String, MockRelease>>>>,
    origin: String,
    upstream: Option<String>,
    http_client: reqwest::Client,
    upstream_cache: UpstreamCache,
}

impl MockRegistry {
    async fn spawn() -> (Self, tokio::task::JoinHandle<()>) {
        Self::spawn_inner(None).await
    }

    async fn spawn_with_upstream(upstream: &str) -> (Self, tokio::task::JoinHandle<()>) {
        Self::spawn_inner(Some(upstream.trim_end_matches('/').to_owned())).await
    }

    async fn spawn_inner(upstream: Option<String>) -> (Self, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let reg = Self {
            packages: Arc::new(Mutex::new(BTreeMap::new())),
            origin: format!("http://{addr}"),
            upstream,
            http_client: reqwest::Client::new(),
            upstream_cache: Arc::default(),
        };
        let app_state = reg.clone();
        let app = Router::new()
            .route("/tarballs/{file}", get(serve_tarball))
            .route("/{pkg}", get(serve_packument))
            .route("/{pkg}/{ver}", get(serve_version))
            .fallback(proxy_fallback)
            .with_state(app_state);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (reg, handle)
    }

    fn publish(&self, tarball: Vec<u8>, published_at: &str) -> NpmArchive {
        let archive = NpmArchive::from_tarball_bytes(tarball.clone()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        archive.stage(tmp.path()).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(tmp.path().join("package.json")).unwrap()).unwrap();
        let integrity = format!("sha512-{}", BASE64.encode(Sha512::digest(&tarball)));
        let mut guard = self.packages.lock().unwrap();
        guard.entry(archive.name().to_owned()).or_default().insert(
            archive.version().to_owned(),
            MockRelease {
                manifest,
                tarball,
                integrity,
                published_at: published_at.to_owned(),
            },
        );
        archive
    }
}

async fn serve_tarball(
    State(reg): State<MockRegistry>,
    AxumPath(file): AxumPath<String>,
) -> Response {
    let Some(stem) = file.strip_suffix(".tgz") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((pkg, ver)) = stem.rsplit_once('@') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let guard = reg.packages.lock().unwrap();
    let Some(release) = guard.get(pkg).and_then(|vers| vers.get(ver)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [(axum::http::header::CONTENT_TYPE, "application/gzip")],
        release.tarball.clone(),
    )
        .into_response()
}

fn version_doc(origin: &str, pkg: &str, ver: &str, rel: &MockRelease) -> Value {
    let mut doc = rel.manifest.clone();
    doc["dist"] = json!({
        "tarball": format!("{origin}/tarballs/{pkg}@{ver}.tgz"),
        "integrity": rel.integrity,
    });
    doc
}

async fn serve_packument(
    State(reg): State<MockRegistry>,
    AxumPath(pkg): AxumPath<String>,
) -> Response {
    let local_json = {
        let guard = reg.packages.lock().unwrap();
        guard.get(&pkg).map(|vers| {
            let mut latest_ver: Option<semver::Version> = None;
            let mut versions_map = serde_json::Map::new();
            let mut time_map = serde_json::Map::new();
            for (ver_str, rel) in vers {
                if let Ok(parsed) = semver::Version::parse(ver_str)
                    && latest_ver.as_ref().is_none_or(|cur| &parsed > cur)
                {
                    latest_ver = Some(parsed);
                }
                versions_map.insert(
                    ver_str.clone(),
                    version_doc(&reg.origin, &pkg, ver_str, rel),
                );
                time_map.insert(ver_str.clone(), Value::String(rel.published_at.clone()));
            }
            let latest_str = latest_ver.map(|v| v.to_string()).unwrap_or_default();
            json!({
                "name": pkg,
                "dist-tags": { "latest": latest_str },
                "versions": versions_map,
                "time": time_map,
            })
        })
    };
    if let Some(doc) = local_json {
        return Json(doc).into_response();
    }
    let encoded = pkg.replace('/', "%2f");
    proxy_to_upstream(&reg, &format!("/{encoded}")).await
}

async fn serve_version(
    State(reg): State<MockRegistry>,
    AxumPath((pkg, ver)): AxumPath<(String, String)>,
) -> Response {
    let local_json = {
        let guard = reg.packages.lock().unwrap();
        guard.get(&pkg).and_then(|vers| {
            let resolved_ver = if ver == "latest" {
                vers.keys()
                    .filter_map(|v| semver::Version::parse(v).ok())
                    .max()
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            } else {
                ver.clone()
            };
            vers.get(&resolved_ver)
                .map(|rel| version_doc(&reg.origin, &pkg, &resolved_ver, rel))
        })
    };
    if let Some(doc) = local_json {
        return Json(doc).into_response();
    }
    let encoded = pkg.replace('/', "%2f");
    proxy_to_upstream(&reg, &format!("/{encoded}/{ver}")).await
}

async fn proxy_fallback(State(reg): State<MockRegistry>, req: Request<Body>) -> Response {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    proxy_to_upstream(&reg, path_and_query).await
}

async fn proxy_to_upstream(reg: &MockRegistry, path: &str) -> Response {
    let Some(upstream) = &reg.upstream else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some((status, ct, bytes)) = reg.upstream_cache.lock().unwrap().get(path).cloned() {
        return (status, [(axum::http::header::CONTENT_TYPE, ct)], bytes).into_response();
    }
    let url = format!("{upstream}{path}");
    let Ok(resp) = reg.http_client.get(&url).send().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let Ok(bytes) = resp.bytes().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let vec = bytes.to_vec();
    if status.is_success() {
        reg.upstream_cache
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, ct.clone(), vec.clone()));
    }
    (status, [(axum::http::header::CONTENT_TYPE, ct)], vec).into_response()
}

fn build_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        for (rel_path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("package/{rel_path}"), *contents)
                .unwrap();
        }
        tar.finish().unwrap();
    }
    gz.finish().unwrap()
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
        if path.extension().and_then(|s| s.to_str()) == Some("mjs") {
            fs::copy(&path, runtime_dir.join(entry.file_name())).ok()?;
        }
    }
    fs::copy(
        dsh_source.join("package.json"),
        runtime_dir.join("package.json"),
    )
    .ok()?;
    fs::copy(
        dsh_source.join("package.json"),
        sdk_project.join("package.json"),
    )
    .ok()?;
    fs::copy(
        dsh_source.join("package-lock.json"),
        sdk_project.join("package-lock.json"),
    )
    .ok()?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dsh_source.join("node_modules"),
        sdk_project.join("node_modules"),
    )
    .ok()?;
    Some((node, npm_cli, fs::canonicalize(sdk_project).ok()?))
}

async fn gateway_json(
    gateway: &PluginWebGateway,
    page: &PluginRoutePage,
    host_header: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host_header);
    let body_bytes = if let Some(payload) = body {
        builder = builder
            .header("content-type", "application/json")
            .header("origin", format!("http://{host_header}"));
        serde_json::to_vec(&payload).unwrap()
    } else {
        Vec::new()
    };
    let req = builder.body(Body::from(body_bytes)).unwrap();
    let resp = gateway.forward(page, req).await.into_response();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let parsed = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, parsed)
}

async fn wait_instance_state(
    statuses: &mut mpsc::UnboundedReceiver<Value>,
    instance_id: &str,
    expected_state: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msg = statuses.recv().await.expect("status channel open");
            if msg["instanceId"] == instance_id && msg["state"] == expected_state {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {instance_id} -> {expected_state}"));
}

#[tokio::test]
async fn ctx_plugin_manager_and_candidate_view_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let (registry, _reg_task) = MockRegistry::spawn().await;

    let mgr_driver_tgz = build_tarball(&[
        (
            "package.json",
            serde_json::to_vec(&json!({
                "name": "mgr-driver",
                "version": "1.0.0",
                "type": "module",
                "exports": "./index.mjs",
                "dsh": { "bundle": { "patch": "./cordis.patch.yml" } },
            }))
            .unwrap()
            .as_slice(),
        ),
        (
            "cordis.patch.yml",
            b"- insert:\n    - id: mgr-driver\n      name: mgr-driver\n",
        ),
        (
            "index.mjs",
            br#"
export const name = 'mgr-driver';
export const inject = ['webServer', 'pluginManager', 'loader'];
export function apply(ctx) {
  ctx.webServer.register({
    kind: 'exact',
    path: '/driver/call',
    async handler(req, res) {
      let raw = '';
      for await (const chunk of req) raw += chunk;
      const { op, args = [] } = JSON.parse(raw || '{}');
      try {
        const out = await ctx.pluginManager[op](...args);
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ ok: true, out }));
      } catch (err) {
        res.writeHead(500, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ ok: false, code: err.code ?? null, message: err.message }));
      }
    },
  });
}
"#,
        ),
    ]);

    let make_demo_tgz = |version: &str| -> Vec<u8> {
        let index_js = format!(
            r#"
export const name = 'demo-target';
export const inject = ['webServer'];
export function apply(ctx) {{
  ctx.webServer.register({{
    kind: 'exact',
    path: '/demo/version',
    handler(_req, res) {{
      res.writeHead(200, {{ 'content-type': 'application/json' }});
      res.end(JSON.stringify({{ version: '{version}' }}));
    }},
  }});
}}
"#
        );
        build_tarball(&[
            (
                "package.json",
                serde_json::to_vec(&json!({
                    "name": "demo-target",
                    "version": version,
                    "description": format!("Demo target {version}"),
                    "type": "module",
                    "exports": "./index.mjs",
                    "dsh": { "bundle": { "patch": "./cordis.patch.yml" } },
                }))
                .unwrap()
                .as_slice(),
            ),
            (
                "README.md",
                format!("# Demo {version}\nDetails here.\n").as_bytes(),
            ),
            (
                "cordis.patch.yml",
                b"- insert:\n    - id: demo-target\n      name: demo-target\n",
            ),
            ("index.mjs", index_js.as_bytes()),
        ])
    };

    let not_bundle_tgz = build_tarball(&[
        (
            "package.json",
            serde_json::to_vec(&json!({
                "name": "plain-utility",
                "version": "1.0.0",
                "type": "module",
                "main": "index.js",
            }))
            .unwrap()
            .as_slice(),
        ),
        ("index.js", b"export default 1;\n"),
    ]);

    let incompatible_tgz = build_tarball(&[
        (
            "package.json",
            serde_json::to_vec(&json!({
                "name": "future-plugin",
                "version": "1.0.0",
                "type": "module",
                "exports": "./index.mjs",
                "engines": { "dsh": ">=9.0.0" },
                "dsh": { "bundle": { "patch": "./cordis.patch.yml" } },
            }))
            .unwrap()
            .as_slice(),
        ),
        (
            "cordis.patch.yml",
            b"- insert:\n    - id: future-plugin\n      name: future-plugin\n",
        ),
        ("index.mjs", b"export function apply() {}\n"),
    ]);

    let driver_archive = registry.publish(mgr_driver_tgz, "2026-11-18T00:00:00.000Z");
    registry.publish(make_demo_tgz("1.0.0"), "2026-11-18T01:00:00.000Z");
    registry.publish(make_demo_tgz("1.1.0"), "2026-11-18T02:00:00.000Z");
    registry.publish(not_bundle_tgz, "2026-11-18T01:00:00.000Z");
    registry.publish(incompatible_tgz, "2026-11-18T01:00:00.000Z");

    let processes = ManagedProcessGroup::new();
    let initial_project = root.join("initial-project");
    fs::create_dir_all(&initial_project).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry.origin,
    };
    let prepared =
        prepare_plugin_dependencies(&processes, &toolchain, &initial_project, &[driver_archive])
            .await
            .unwrap();
    let (driver_insp, _) = prepared.into_iter().next().unwrap();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: mgr-driver\n      name: mgr-driver\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&initial_project).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: vec![driver_insp],
    };

    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49100";
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node: node.clone(),
            sdk_project: sdk_project.clone(),
            endpoint_dir: root.join("sockets"),
            public_origin: format!("http://{public_host}"),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli: npm_cli.clone(),
                projects_dir: root.join("projects"),
                registry: registry.origin.clone(),
            }),
        },
        initial_prep,
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();

    wait_instance_state(&mut statuses, "include:mgr-driver", "running").await;

    // Candidate project view materialized on startup:
    let profile_pkg: Value =
        serde_json::from_slice(&fs::read(source_root.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        profile_pkg["dependencies"],
        json!({ "mgr-driver": "1.0.0" })
    );
    assert!(
        source_root
            .join("node_modules/mgr-driver/package.json")
            .is_file()
    );

    let gateway = runtime.gateway();
    let driver_activation = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:mgr-driver")
        .unwrap()
        .clone();
    let driver_page = gateway.bind_page(&driver_activation).unwrap();

    // 1. inspect: accepted, already-installed, and not-a-bundle
    let (st, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "inspect", "args": ["demo-target@1.0.0"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "inspect failed: {res}");
    assert_eq!(res["out"]["status"], "accepted");
    assert_eq!(res["out"]["version"], "1.0.0");

    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "inspect", "args": ["plain-utility@1.0.0"] })),
    )
    .await;
    assert_eq!(res["out"]["status"], "refused");
    assert_eq!(res["out"]["problem"], "not-a-bundle");

    // 2. Refusal on incompatible runtime version leaves state untouched
    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "installBundle", "args": ["future-plugin@1.0.0"] })),
    )
    .await;
    assert_eq!(res["out"]["application"], "failed");
    assert_eq!(res["out"]["error"]["code"], "incompatible-version");

    // 3. Install demo-target@1.0.0 via ctx.pluginManager
    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "installBundle", "args": ["demo-target@1.0.0", { "enabled": true }] })),
    )
    .await;
    assert_eq!(res["out"]["application"], "applied");
    assert_eq!(res["out"]["version"], "1.0.0");

    let demo_v1_act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:demo-target")
        .unwrap()
        .clone();
    let demo_v1_page = gateway.bind_page(&demo_v1_act).unwrap();
    let (st, ver_body) = gateway_json(
        &gateway,
        &demo_v1_page,
        public_host,
        Method::GET,
        "/demo/version",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver_body["version"], "1.0.0");

    // Candidate view updated immediately:
    let profile_pkg: Value =
        serde_json::from_slice(&fs::read(source_root.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        profile_pkg["dependencies"],
        json!({ "demo-target": "1.0.0", "mgr-driver": "1.0.0" })
    );
    assert!(
        source_root
            .join("node_modules/demo-target/package.json")
            .is_file()
    );

    // 4. listVersions & readBundleDetail
    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "listVersions", "args": ["demo-target"] })),
    )
    .await;
    assert_eq!(res["out"]["status"], "listed");
    assert_eq!(res["out"]["installed"], "1.0.0");
    assert_eq!(res["out"]["versions"][0]["version"], "1.1.0");

    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "readBundleDetail", "args": ["demo-target", "zh-CN"] })),
    )
    .await;
    assert_eq!(res["out"]["status"], "found");
    assert!(
        res["out"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Demo 1.0.0")
    );

    // 5. cancelInstall against a stalled registry cleans up and preserves revision
    let stalled_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_origin = format!("http://{}", stalled_listener.local_addr().unwrap());
    let mgmt = runtime.management().unwrap();
    let mgmt_for_install = mgmt.clone();
    let install_task = tokio::spawn(async move {
        mgmt_for_install
            .install_bundle(
                "demo-target@1.1.0",
                true,
                Some("req-cancel-1"),
                Some(&stalled_origin),
            )
            .await
            .unwrap()
    });
    let (_accepted_stream, _) =
        tokio::time::timeout(Duration::from_secs(10), stalled_listener.accept())
            .await
            .unwrap()
            .unwrap();
    let cancel_res = mgmt.cancel_install("req-cancel-1").await;
    assert_eq!(cancel_res["status"], "cancelled");
    let settled_install = install_task.await.unwrap();
    assert_eq!(settled_install["application"], "cancelled");

    // 6. Self-removal guard refuses removing caller's own running package
    let (_, res) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "removeBundle", "args": ["mgr-driver"] })),
    )
    .await;
    assert_eq!(res["out"]["application"], "failed");
    assert_eq!(res["out"]["error"]["code"], "stop-profile");

    // 7. Two-step ordinary target update (removeBundle + installBundle@latest)
    let (_, rem) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "removeBundle", "args": ["demo-target"] })),
    )
    .await;
    assert_eq!(rem["out"]["application"], "applied");
    // Old page lease must be revoked immediately!
    let (old_st, _) = gateway_json(
        &gateway,
        &demo_v1_page,
        public_host,
        Method::GET,
        "/demo/version",
        None,
    )
    .await;
    assert_eq!(old_st, StatusCode::CONFLICT);

    let (_, ins) = gateway_json(
        &gateway,
        &driver_page,
        public_host,
        Method::POST,
        "/driver/call",
        Some(json!({ "op": "installBundle", "args": ["demo-target@latest", { "enabled": true }] })),
    )
    .await;
    assert_eq!(ins["out"]["application"], "applied");
    assert_eq!(ins["out"]["version"], "1.1.0");

    let demo_v2_act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:demo-target")
        .unwrap()
        .clone();
    let demo_v2_page = gateway.bind_page(&demo_v2_act).unwrap();
    let (st, ver_body) = gateway_json(
        &gateway,
        &demo_v2_page,
        public_host,
        Method::GET,
        "/demo/version",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver_body["version"], "1.1.0");

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn fixed_original_dsh_plugin_mgr_http_business_flow() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mgr_0212_path = repo_root.join("target/dsh-phase-1/originals/dsh-plugin-mgr-0.2.12.tgz");
    let mgr_0213_path = repo_root.join("target/dsh-phase-1/originals/dsh-plugin-mgr-0.2.13.tgz");
    if !mgr_0212_path.is_file() || !mgr_0213_path.is_file() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let Some((node, npm_cli, sdk_project)) = resolve_test_toolchain(&root) else {
        return;
    };
    let upstream_registry =
        std::env::var("MAGI_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let (registry, _reg_task) = MockRegistry::spawn_with_upstream(&upstream_registry).await;

    let mgr_0212_archive = registry.publish(
        fs::read(&mgr_0212_path).unwrap(),
        "2026-11-10T00:00:00.000Z",
    );
    registry.publish(
        fs::read(&mgr_0213_path).unwrap(),
        "2026-11-15T00:00:00.000Z",
    );

    let make_target_bundle = |ver: &str| {
        build_tarball(&[
            (
                "package.json",
                serde_json::to_vec(&json!({
                    "name": "demo-target",
                    "version": ver,
                    "description": format!("Target bundle {ver}"),
                    "type": "module",
                    "exports": "./index.mjs",
                    "dsh": { "bundle": { "patch": "./cordis.patch.yml" } },
                }))
                .unwrap()
                .as_slice(),
            ),
            (
                "cordis.patch.yml",
                b"- insert:\n    - id: demo-target\n      name: demo-target\n",
            ),
            (
                "index.mjs",
                br#"export const name = 'demo-target'; export function apply() {}"#,
            ),
        ])
    };
    let target_v1 = registry.publish(make_target_bundle("1.0.0"), "2026-11-12T00:00:00.000Z");
    registry.publish(make_target_bundle("1.1.0"), "2026-11-17T00:00:00.000Z");

    let processes = ManagedProcessGroup::new();
    let initial_project = root.join("initial-project");
    fs::create_dir_all(&initial_project).unwrap();
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &sdk_project,
        registry: &registry.origin,
    };
    let prepared = prepare_plugin_dependencies(
        &processes,
        &toolchain,
        &initial_project,
        &[mgr_0212_archive, target_v1],
    )
    .await
    .unwrap();
    let inspections: Vec<_> = prepared.into_iter().map(|(i, _)| i).collect();

    let owner = PluginManager::open(root.join("state")).unwrap();
    let source_root = owner.loading_source_root();
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        source_root.join("bundle.patch.yml"),
        "- insert:\n    - id: dsh-plugin-mgr\n      name: dsh-plugin-mgr\n- insert:\n    - id: demo-target\n      name: demo-target\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();

    let initial_prep = LoadingPreparation {
        project_directory: fs::canonicalize(&initial_project).unwrap(),
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: inspections,
    };

    let (status_tx, mut statuses) = mpsc::unbounded_channel();
    let public_host = "127.0.0.1:49101";
    let owner_arc = Arc::new(Mutex::new(owner));
    let runtime = PluginRuntime::launch(
        owner_arc.clone(),
        &processes,
        PluginRuntimeConfig {
            node: node.clone(),
            sdk_project: sdk_project.clone(),
            endpoint_dir: root.join("sockets"),
            public_origin: format!("http://{public_host}"),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli: npm_cli.clone(),
                projects_dir: root.join("projects"),
                registry: registry.origin.clone(),
            }),
        },
        initial_prep,
        Arc::new(StatusSink(status_tx)),
    )
    .await
    .unwrap();

    wait_instance_state(&mut statuses, "include:dsh-plugin-mgr", "running").await;

    let gateway = runtime.gateway();
    let mgr_0212_act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert_eq!(mgr_0212_act.executable.version, "0.2.12");
    let mgr_0212_page = gateway.bind_page(&mgr_0212_act).unwrap();

    // 1. 0.2.12 GET /api/plugin-manager/list
    let (st, list_0212) = gateway_json(
        &gateway,
        &mgr_0212_page,
        public_host,
        Method::GET,
        "/api/plugin-manager/list",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list_0212["ok"], true);
    assert_eq!(list_0212["plugins"].as_array().unwrap().len(), 2);

    // 2. Native atomic upgrade dsh-plugin-mgr 0.2.12 -> 0.2.13
    let upgrade_out = runtime
        .management()
        .unwrap()
        .upgrade_bundle("dsh-plugin-mgr", Some("0.2.13"), None, None)
        .await
        .unwrap();
    assert_eq!(upgrade_out["from"], "0.2.12");
    assert_eq!(upgrade_out["to"], "0.2.13");
    assert_eq!(upgrade_out["result"]["application"], "applied");

    // Old 0.2.12 route page is immediately fenced:
    let (fenced_st, _) = gateway_json(
        &gateway,
        &mgr_0212_page,
        public_host,
        Method::GET,
        "/api/plugin-manager/list",
        None,
    )
    .await;
    assert_eq!(fenced_st, StatusCode::CONFLICT);

    let mgr_0213_act = owner_arc
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert_eq!(mgr_0213_act.executable.version, "0.2.13");
    let mgr_0213_page = gateway.bind_page(&mgr_0213_act).unwrap();

    // 3. 0.2.13 GET /api/plugin-manager/list & /api/plugin-manager/updates
    let (st, list_0213) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::GET,
        "/api/plugin-manager/list",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mgr_entry = list_0213["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "dsh-plugin-mgr")
        .unwrap();
    assert_eq!(mgr_entry["version"], "0.2.13");
    assert_eq!(mgr_entry["self"], true);

    let (st, updates_body) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::GET,
        "/api/plugin-manager/updates",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(updates_body["updates"]["demo-target"]["latest"], "1.1.0");
    assert_eq!(updates_body["updates"]["demo-target"]["update"], true);

    // 4. 0.2.13 POST /api/plugin-manager/toggle (disable then re-enable demo-target)
    let mut obs_rx = runtime.source_observations();
    let rev_before = owner_arc
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .revision;
    let (st, toggle_off) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::POST,
        "/api/plugin-manager/toggle",
        Some(json!({ "name": "demo-target", "disable": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(toggle_off["disabled"], true);

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let LoadingSourceObservation::Reconciled { revision, .. } =
                obs_rx.borrow_and_update().clone()
                && revision > rev_before
            {
                return;
            }
            obs_rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        owner_arc
            .lock()
            .unwrap()
            .loading_instance_binding("include:demo-target")
            .is_none()
    );

    let rev_disabled = owner_arc
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .revision;
    let (st, toggle_on) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::POST,
        "/api/plugin-manager/toggle",
        Some(json!({ "name": "demo-target", "disable": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(toggle_on["disabled"], false);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let LoadingSourceObservation::Reconciled { revision, .. } =
                obs_rx.borrow_and_update().clone()
                && revision > rev_disabled
            {
                return;
            }
            obs_rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        owner_arc
            .lock()
            .unwrap()
            .loading_instance_binding("include:demo-target")
            .is_some()
    );

    // 5. 0.2.13 POST /api/plugin-manager/update on demo-target (1.0.0 -> 1.1.0 via ctx.pluginManager)
    let (st, upd_target) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::POST,
        "/api/plugin-manager/update",
        Some(json!({ "name": "demo-target" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "update response: {upd_target}");
    assert_eq!(upd_target["ok"], true);
    assert_eq!(upd_target["version"], "1.1.0");
    assert_eq!(upd_target["updated"], true);

    // 6. 0.2.13 self-update via two-step remove+install is safely refused without deleting self
    let (st, self_upd) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::POST,
        "/api/plugin-manager/update",
        Some(json!({ "name": "dsh-plugin-mgr", "selfConfirm": true })),
    )
    .await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(self_upd["error"].as_str().unwrap().contains("stop-profile"));

    // 7. 0.2.13 POST /api/plugin-manager/uninstall on demo-target
    let (st, uninst) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::POST,
        "/api/plugin-manager/uninstall",
        Some(json!({ "name": "demo-target" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "uninstall response: {uninst}");
    assert_eq!(uninst["ok"], true);

    let (st, final_list) = gateway_json(
        &gateway,
        &mgr_0213_page,
        public_host,
        Method::GET,
        "/api/plugin-manager/list",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let names: Vec<&str> = final_list["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(names, vec!["dsh-plugin-mgr"]);

    runtime.stop().await.unwrap();
}
