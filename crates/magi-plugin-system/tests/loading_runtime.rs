//! Domain-owned revisions executed in a real shared Node host. These are runtime,
//! not Electron or upstream business acceptance tests.
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::{PackageInspection, SharedHost, SharedLaunchConfig},
};
use magi_plugin_system::{
    LoadingActivation, LoadingApplication, LoadingCandidate, LoadingEditProposal,
    LoadingSourceEdit, PluginManager,
    loading_runtime::{
        LoadingHostServices, LoadingPreparation, LoadingRuntime, LoadingSourceObservation,
    },
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::SocketAddrV4,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, watch},
};

struct Events(mpsc::UnboundedSender<(String, Value)>);

impl Incoming for Events {
    fn request(&self, _: String, _: Value) -> BoxFuture<Result<Value, RpcError>> {
        Box::pin(async {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                "This test provides no business services",
            ))
        })
    }

    fn notification(&self, method: String, params: Value) {
        let _ = self.0.send((method, params));
    }
}

struct Runtime {
    temp: tempfile::TempDir,
    owner: Arc<Mutex<PluginManager>>,
    host: Arc<SharedHost>,
    runtime: Arc<LoadingRuntime>,
    services: Arc<LoadingHostServices>,
    events: mpsc::UnboundedReceiver<(String, Value)>,
    packages: Vec<PackageInspection>,
    project: PathBuf,
    // Keep the process owner alive for the whole test.
    _group: ManagedProcessGroup,
}

impl Runtime {
    async fn new(modules: &[(&str, &str)]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let owner = PluginManager::open(temp.path().join("state")).unwrap();
        fs::create_dir_all(owner.loading_source_root()).unwrap();
        let project = fs::canonicalize(temp.path()).unwrap().join("immutable");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("package.json"), "{\"private\":true}").unwrap();
        let mut packages = Vec::new();
        for (name, source) in modules {
            let directory = project.join("node_modules").join(name);
            fs::create_dir_all(&directory).unwrap();
            let manifest = json!({"name":name,"version":"1.0.0","main":"index.mjs"}).to_string();
            fs::write(directory.join("package.json"), &manifest).unwrap();
            fs::write(directory.join("index.mjs"), source).unwrap();
            packages.push(PackageInspection {
                project_directory: project.clone(),
                directory,
                name: (*name).into(),
                version: "1.0.0".into(),
                manifest_sha256: magi_core::hex_lower(Sha256::digest(manifest.as_bytes())),
            });
        }
        let (sender, events) = mpsc::unbounded_channel();
        let group = ManagedProcessGroup::new();
        let (owner, host, runtime, services) = Self::launch(owner, &group, sender).await;
        Self {
            runtime,
            services,
            temp,
            owner,
            host,
            events,
            packages,
            project,
            _group: group,
        }
    }

    async fn launch(
        mut owner: PluginManager,
        group: &ManagedProcessGroup,
        events: mpsc::UnboundedSender<(String, Value)>,
    ) -> (
        Arc<Mutex<PluginManager>>,
        Arc<SharedHost>,
        Arc<LoadingRuntime>,
        Arc<LoadingHostServices>,
    ) {
        let services = Arc::new(LoadingHostServices::new(Arc::new(Events(events))));
        let host = Arc::new(
            SharedHost::launch(
                group,
                SharedLaunchConfig {
                    node: std::env::var_os("MAGI_EXTENSION_NODE")
                        .map(PathBuf::from)
                        .or_else(|| magi_process::resolve_executable("node"))
                        .expect("Node required"),
                    runtime_entry: Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../magi-extension-host/runtime/dsh/entry.mjs"),
                    endpoint_dir: owner.root().join("socket"),
                    host_generation: owner.allocate_loading_host_generation().unwrap(),
                    incoming: services.clone(),
                },
            )
            .await
            .unwrap(),
        );
        let owner = Arc::new(Mutex::new(owner));
        let runtime = Arc::new(LoadingRuntime::new(owner.clone(), host.clone()));
        services.bind(&runtime).unwrap();
        (owner, host, runtime, services)
    }

    async fn accept(&self, document: &Value) -> u64 {
        let candidate = {
            let owner = self.owner.lock().unwrap();
            fs::write(
                owner.loading_source_root().join("cordis.yml"),
                document.to_string(),
            )
            .unwrap();
            let revision = owner.loading_configuration().map_or(0, |c| c.revision);
            owner
                .prepare_loading_configuration(revision, &["cordis.yml".into()])
                .unwrap()
        };
        let previous = self
            .owner
            .lock()
            .unwrap()
            .loading_configuration()
            .map(|c| c.graph.clone());
        let plan = self
            .host
            .prepare_configuration(json!({
                "sourceDirectory":candidate.source_root(), "projectDirectory":self.project,
                "sources":candidate.sources(), "base":"cordis.yml", "packages":self.packages,
                "previous":previous,
            }))
            .await
            .unwrap();
        self.owner
            .lock()
            .unwrap()
            .accept_loading_configuration(candidate, serde_json::from_value(plan).unwrap())
            .unwrap()
            .revision
    }

    fn binding(&self, id: &str) -> LoadingActivation {
        self.owner
            .lock()
            .unwrap()
            .loading_instance_binding(id)
            .unwrap()
            .clone()
    }

    fn preparation(&self) -> LoadingPreparation {
        LoadingPreparation {
            project_directory: self.project.clone(),
            base: "cordis.yml".into(),
            overlays: vec!["cordis.patch.yml".into()],
            packages: self.packages.clone(),
        }
    }

    async fn propose(
        &self,
        preparation: &LoadingPreparation,
        operation: Value,
    ) -> (LoadingCandidate, LoadingEditProposal) {
        self.propose_with_sources(preparation, operation, &[]).await
    }

    async fn propose_with_sources(
        &self,
        preparation: &LoadingPreparation,
        operation: Value,
        additional: &[&str],
    ) -> (LoadingCandidate, LoadingEditProposal) {
        let (candidate, previous) = {
            let owner = self.owner.lock().unwrap();
            let current = owner.loading_configuration().unwrap();
            let names = current
                .sources
                .iter()
                .map(|source| source.name().to_owned())
                .chain(additional.iter().map(|name| (*name).to_owned()))
                .collect::<Vec<_>>();
            (
                owner
                    .prepare_loading_configuration(current.revision, &names)
                    .unwrap(),
                current.graph.clone(),
            )
        };
        let proposal = self.host.prepare_configuration_edit(json!({
            "projectDirectory": preparation.project_directory,"sourceDirectory":candidate.source_root(),
            "sources":candidate.sources(),"base":preparation.base,"overlays":preparation.overlays,
            "packages":preparation.packages,"previous":previous,"operation":operation,
        })).await.unwrap();
        (candidate, serde_json::from_value(proposal).unwrap())
    }

    fn observe_pending(&mut self) {
        while let Ok((method, status)) = self.events.try_recv() {
            if method == "plugin.status" {
                self.runtime.observe_instance_status(&status).unwrap();
            }
        }
    }

    async fn wait_host_invalidated(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            self.host.closed().await;
            loop {
                if matches!(
                    self.owner.lock().unwrap().loading_application(),
                    Some(LoadingApplication::Failed(_, _))
                ) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the connection observer must invalidate runtime facts");
    }
}

#[tokio::test]
async fn web_route_page_and_inflight_lease_end_when_owner_or_cordis_scope_ends() {
    let f = Runtime::new(&[
        ("observer", r#"
            import { appendFileSync } from 'node:fs';
            export function apply(ctx, config) {
                ctx.on('loader/partial-dispose', entry => appendFileSync(config.receipt, entry.id + '\n'), { global: true });
            }
        "#),
        ("provider", "export function apply(ctx) { ctx.provide('gate', {}); }"),
        ("business", r#"
            export const inject = ['webServer', 'gate'];
            export function apply(ctx) {
                ctx.webServer.register({ kind: 'exact', path: '/ok', handler(req, res) { res.end('current'); } });
                ctx.webServer.register({ kind: 'exact', path: '/stream', handler(req, res) {
                    res.writeHead(200, { 'content-type': 'text/event-stream' });
                    res.write('data: first\n\n');
                } });
            }
        "#),
    ]).await;
    let routes = f
        .runtime
        .open_web_routes("http://127.0.0.1:38123")
        .await
        .unwrap();
    let refresh_receipt = f.temp.path().join("refreshed.log");
    let document = |provider_disabled, business_disabled| {
        json!([
            {"id":"observer","name":"observer","config":{"receipt":refresh_receipt}},
            {"id":"provider","name":"provider","disabled":provider_disabled},
            {"id":"route","name":"business","disabled":business_disabled},
        ])
    };
    f.accept(&document(false, false)).await;
    f.runtime.apply_current().await.unwrap();
    let original = f.binding("include:route");
    let old_page = routes.bind_page(&original).unwrap();
    let old_call = routes.admit(&old_page).unwrap();
    let (address, old_headers) = old_call.target().unwrap();
    assert!(
        through(address, "/ok", &old_headers)
            .await
            .ends_with("current")
    );

    f.accept(&document(true, false)).await;
    f.runtime.apply_current().await.unwrap();
    revoked(&old_call).await;
    assert!(routes.admit(&old_page).is_err());
    assert!(routes.bind_page(&original).is_err());

    f.accept(&document(false, false)).await;
    f.runtime.apply_current().await.unwrap();
    assert_eq!(
        f.binding("include:route"),
        original,
        "restoring a provider retains dependent activation identity"
    );
    let new_page = routes.bind_page(&original).unwrap();
    let live = routes.admit(&new_page).unwrap();
    let (_, headers) = live.target().unwrap();
    assert!(routes.admit(&old_page).is_err());
    assert!(
        through(address, "/ok", &old_headers)
            .await
            .starts_with("HTTP/1.1 409")
    );
    assert!(through(address, "/ok", &headers).await.ends_with("current"));

    let mut stream = connect(address, "/stream", &headers).await;
    let read_first = async {
        let mut seen = Vec::new();
        while !seen
            .windows(b"data: first".len())
            .any(|prefix| prefix == b"data: first")
        {
            seen.push(stream.read_u8().await.unwrap());
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read_first)
        .await
        .unwrap();

    // A durable disable revokes the daemon lease before any application RPC.
    f.accept(&document(false, true)).await;
    revoked(&live).await;
    assert!(
        f.host
            .snapshot("business", "include:route", original.activation_generation)
            .await
            .is_ok()
    );
    assert!(routes.admit(&new_page).is_err());
    assert!(live.target().is_err());
    f.runtime.apply_current().await.unwrap();
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !tail.ends_with(b"0\r\n\r\n"),
        "the revoked stream cannot be reported as a completed HTTP body"
    );

    let revision = f.accept(&document(false, false)).await;
    let applied = f.runtime.apply_current().await.unwrap();
    let observer = &applied["instances"][0];
    let provider = f.binding("include:provider");
    let route = f.binding("include:route");
    let page = routes.bind_page(&route).unwrap();
    let call = routes.admit(&page).unwrap();
    let source = f
        .owner
        .lock()
        .unwrap()
        .loading_source_root()
        .join("cordis.yml");
    let before = fs::read(&source).unwrap();
    let caller = json!({
            "hostGeneration":observer["hostGeneration"], "pluginId":observer["pluginId"],
            "instanceId":observer["instanceId"], "version":observer["version"],
            "activationGeneration":observer["activationGeneration"], "configurationEpoch":observer["configurationEpoch"],
    });
    fs::write(&refresh_receipt, "").unwrap();
    let mut refresh = json!({
        "caller":caller, "expectedRevision":revision, "action":"refresh", "targets":[
            {"instanceId":"include:route", "activationGeneration":route.activation_generation},
        ],
    });
    refresh["expectedRevision"] = json!(revision - 1);
    assert!(
        f.services
            .request("loading.instances.change".into(), refresh.clone())
            .await
            .is_err()
    );
    assert!(fs::read_to_string(&refresh_receipt).unwrap().is_empty());
    refresh["expectedRevision"] = json!(revision);
    assert_eq!(
        f.services
            .request("loading.instances.change".into(), refresh.clone())
            .await
            .unwrap(),
        json!({"revision":revision})
    );
    assert_eq!(
        fs::read_to_string(&refresh_receipt).unwrap(),
        "include:route\n"
    );
    assert_eq!(f.binding("include:route"), route);
    assert_eq!(fs::read(&source).unwrap(), before);
    call.assert_current().unwrap();

    let mut stop_request = json!({
        "caller": caller,
        "expectedRevision":revision, "action":"stop", "targets":[
            {"instanceId":"include:provider", "activationGeneration":provider.activation_generation},
            {"instanceId":"include:route", "activationGeneration":route.activation_generation + 1},
        ],
    });
    assert!(
        f.services
            .request("loading.instances.change".into(), stop_request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        f.binding("include:provider"),
        provider,
        "an invalid second target leaves the whole batch intact"
    );
    call.assert_current().unwrap();
    stop_request["targets"][1]["activationGeneration"] = json!(route.activation_generation);
    let stop = f
        .services
        .request("loading.instances.change".into(), stop_request);
    let (receipt, ()) = tokio::join!(stop, revoked(&call));
    assert_eq!(receipt.unwrap(), json!({"revision":revision}));
    assert!(routes.admit(&page).is_err());
    assert_eq!(fs::read(&source).unwrap(), before);
    {
        let owner = f.owner.lock().unwrap();
        assert_eq!(owner.loading_configuration().unwrap().revision, revision);
        assert!(owner.loading_instance_binding("include:provider").is_none());
        assert!(owner.loading_instance_binding("include:route").is_none());
        assert!(owner.loading_instance_binding("include:observer").is_some());
    }
    let settled = f.runtime.apply_current().await.unwrap();
    assert_eq!(settled["instances"][0]["state"], "running");
    assert_eq!(settled["instances"][1]["state"], "stopped");
    assert_eq!(settled["instances"][2]["state"], "stopped");
    assert!(
        f.services
            .request("loading.instances.change".into(), refresh)
            .await
            .is_err()
    );
    f.host.stop().await.unwrap();

    async fn revoked(lease: &magi_plugin_system::loading_runtime::PluginRouteLease) {
        tokio::time::timeout(Duration::from_secs(5), lease.revoked())
            .await
            .unwrap();
        assert!(lease.assert_current().is_err());
    }

    async fn connect(address: SocketAddrV4, path: &str, headers: &[(&str, String)]) -> TcpStream {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut request =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:38123\r\nConnection: close\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        stream
    }

    async fn through(address: SocketAddrV4, path: &str, headers: &[(&str, String)]) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = connect(address, path, headers).await;
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn recovery_rechecks_newly_referenced_source_before_accepting_a_saved_parent() {
    let f = Runtime::new(&[]).await;
    let root = f.owner.lock().unwrap().loading_source_root();
    fs::write(root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(root.join("cordis.patch.yml"), "[]\n").unwrap();
    let new_source = root.join("added.yml");
    let before = "- id: dormant\n  name: business\n  disabled: true\n";
    fs::write(&new_source, before).unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let (candidate, proposal) = f
        .propose_with_sources(
            &preparation,
            json!({"kind":"create", "treeId":"include", "options":{
                "id":"added","name":"cordis:include","config":{"path":"added.yml"}
            }}),
            &["added.yml"],
        )
        .await;
    let [edit] = proposal.edits.as_slice() else {
        panic!("only the parent is written")
    };
    let pending_parent = edit.content.clone();
    let expected_plan = proposal.plan.clone();
    f.owner
        .lock()
        .unwrap()
        .begin_loading_edit(candidate, proposal)
        .unwrap();
    // The parent rename happened before the interruption. The newly read
    // file was not written by the transaction, so its new bytes are external.
    fs::write(root.join("cordis.yml"), &pending_parent).unwrap();
    fs::write(&new_source, "[]\n").unwrap();
    assert!(f.runtime.reconcile_sources(1, &preparation).await.is_err());
    f.host.stop().await.unwrap();
    let mut reopened = PluginManager::open(f.owner.lock().unwrap().root()).unwrap();
    assert!(reopened.finish_loading_edit().is_err());
    assert_eq!(reopened.loading_configuration().unwrap().revision, 1);
    assert!(reopened.pending_loading_edit().is_some());
    assert_eq!(fs::read_to_string(&new_source).unwrap(), "[]\n");
    assert_eq!(
        fs::read_to_string(root.join("cordis.yml")).unwrap(),
        pending_parent
    );
    fs::write(&new_source, before).unwrap();
    let restored = reopened.finish_loading_edit().unwrap();
    assert_eq!(restored.revision, 2);
    assert_eq!(restored.graph, expected_plan.graph);
    assert_eq!(restored.sources_sha256, expected_plan.sources_sha256);
    assert_eq!(restored.sources.len(), 3);
}

#[tokio::test]
async fn source_editor_uses_durable_revision_before_code_and_preserves_state_on_stale_or_external_edits()
 {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `${revision}:${config.value}\n`);
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let receipt = f.temp.path().join("source-edit.log");
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let create = f.runtime.edit_sources(1, json!({"kind":"create","treeId":"include",
        "options":{"id":"business","name":"business","config":{"receipt":receipt,"stateFile":state_file,"value":"first"}}})).await.unwrap();
    assert_eq!(create.result, "include:business");
    assert_eq!(create.reconciliation.revision, 2);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "2:first\n");
    let first = f.binding("include:business");
    let update = f
        .runtime
        .edit_sources(
            2,
            json!({"kind":"update","treeId":"include","id":"business",
        "options":{"config":{"receipt":receipt,"stateFile":state_file,"value":"second"}}}),
        )
        .await
        .unwrap();
    assert_eq!(update.reconciliation.revision, 3);
    assert_eq!(f.binding("include:business"), first);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "2:first\n3:second\n");
    let persisted = fs::read(&state_file).unwrap();
    let unchanged = f
        .runtime
        .edit_sources(
            3,
            json!({"kind":"update","treeId":"include","id":"business",
        "options":{"config":{"receipt":receipt,"stateFile":state_file,"value":"second"}}}),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.reconciliation.revision, 3);
    assert!(unchanged.reconciliation.application.is_none());
    assert_eq!(fs::read(&state_file).unwrap(), persisted);
    let source = fs::read_to_string(source_root.join("cordis.yml")).unwrap();
    assert!(
        f.runtime
            .edit_sources(
                2,
                json!({"kind":"remove","treeId":"include","id":"business"})
            )
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        source
    );
    fs::write(
        source_root.join("cordis.yml"),
        format!("{source}\n# external editor\n"),
    )
    .unwrap();
    assert!(
        f.runtime
            .edit_sources(
                3,
                json!({"kind":"remove","treeId":"include","id":"business"})
            )
            .await
            .is_err()
    );
    assert!(
        fs::read_to_string(source_root.join("cordis.yml"))
            .unwrap()
            .ends_with("# external editor\n")
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    assert_eq!(f.binding("include:business"), first);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "2:first\n3:second\n");
    f.runtime.reconcile_sources(3, &preparation).await.unwrap();
    let removed = f
        .runtime
        .edit_sources(
            4,
            json!({"kind":"remove","treeId":"include","id":"business"}),
        )
        .await
        .unwrap();
    assert_eq!(removed.reconciliation.revision, 5);
    assert_eq!(
        removed.reconciliation.application.as_ref().unwrap()["instances"],
        json!([])
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn fiber_save_during_group_startup_round_trips_through_the_owner_before_the_new_config_runs()
{
    let f = Runtime::new(&[("business", r#"
        import { writeFileSync, readFileSync } from 'node:fs';
        export async function apply(ctx, config) {
            if (!config.saved) {
                await ctx.fiber.update({ ...config, saved: true });
                const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
                writeFileSync(config.ack, `saved:${revision}`);
            } else {
                const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
                writeFileSync(config.receipt, `running:${revision}`);
            }
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let receipt = f.temp.path().join("fiber-running.log");
    let ack = f.temp.path().join("fiber-accepted.log");
    fs::write(source_root.join("cordis.yml"), json!([
        {"id":"group","name":"cordis:group","group":true,"config":[
            {"id":"business","name":"business","config":{"stateFile":state_file,"receipt":receipt,"ack":ack}}
        ]}
    ]).to_string()).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let monitor = f.runtime.monitor_sources(f.preparation()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 2, .. }
        )
    })
    .await;
    assert_eq!(fs::read_to_string(&ack).unwrap(), "saved:2");
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "running:2");
    let current = f
        .owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .clone();
    assert_eq!(
        current.graph["root"]["entries"][0]["children"][0]["children"][0]["options"]["config"]["saved"],
        true
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    assert_eq!(
        f.host
            .snapshot(
                "business",
                "include:business",
                f.binding("include:business").activation_generation
            )
            .await
            .unwrap()["state"],
        "running"
    );
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn source_host_requests_reject_stale_or_malformed_identity_without_saving_and_binding_is_one_time()
 {
    let f = Runtime::new(&[("business", "export function apply() {}")]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("cordis.yml"),
        "- id: business\n  name: business\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    f.runtime
        .reconcile_sources(0, &f.preparation())
        .await
        .unwrap();
    let activation = f.binding("include:business");
    let status = f
        .host
        .snapshot(
            "business",
            "include:business",
            activation.activation_generation,
        )
        .await
        .unwrap();
    let caller = json!({"hostGeneration":activation.host_generation, "pluginId":"business",
        "instanceId":"include:business", "activationGeneration":activation.activation_generation, "version":"1.0.0",
        "configurationEpoch":status["configurationEpoch"]});
    let request = json!({"caller":caller,"expectedRevision":1,
        "operation":{"kind":"update","treeId":"include","id":"business","options":{"disabled":true}}});
    let state_before = fs::read(f.owner.lock().unwrap().root().join("state.json")).unwrap();
    let source_before = fs::read(source_root.join("cordis.yml")).unwrap();
    for (field, value) in [
        ("hostGeneration", json!(activation.host_generation + 1)),
        ("pluginId", json!("other")),
        ("instanceId", json!("include:other")),
        (
            "activationGeneration",
            json!(activation.activation_generation + 1),
        ),
        ("version", json!("2.0.0")),
        ("configurationEpoch", json!(0)),
    ] {
        let mut invalid = request.clone();
        invalid["caller"][field] = value;
        let error = f
            .services
            .request("loading.edit.submit".into(), invalid)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
    }
    let mut malformed = request.clone();
    malformed["projectDirectory"] = json!(f.project);
    assert_eq!(
        f.services
            .request("loading.edit.submit".into(), malformed)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidParams
    );
    let mut stale_config = request.clone();
    stale_config["operation"]["expectedConfig"] = json!({"stale":"config"});
    assert_eq!(
        f.services
            .request("loading.edit.submit".into(), stale_config)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    assert_eq!(
        fs::read(f.owner.lock().unwrap().root().join("state.json")).unwrap(),
        state_before
    );
    assert_eq!(
        fs::read(source_root.join("cordis.yml")).unwrap(),
        source_before
    );
    assert!(f.services.bind(&f.runtime).is_err());
    f.owner.lock().unwrap().loading_instance_ended(&activation);
    assert!(
        f.services
            .request("loading.edit.submit".into(), request)
            .await
            .is_err()
    );
    f.host.stop().await.unwrap();
    drop(f.runtime);
    assert_eq!(
        f.services
            .request("loading.edit.submit".into(), json!({}))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::HostGone
    );
}

#[tokio::test]
async fn explicitly_unsaved_fiber_updates_remain_instance_local_and_do_not_change_the_accepted_source()
 {
    let f = Runtime::new(&[(
        "business",
        r#"
        import { writeFileSync } from 'node:fs';
        export function apply(ctx, config) {
            ctx.on('internal/update', next => { writeFileSync(config.receipt, next.value); });
            ctx.fiber.update({ ...config, value: 'instance-local' }, true);
        }
    "#,
    )])
    .await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("local-update.log");
    let document =
        json!([{"id":"business","name":"business","config":{"receipt":receipt,"value":"source"}}])
            .to_string();
    fs::write(source_root.join("cordis.yml"), &document).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    f.runtime
        .reconcile_sources(0, &f.preparation())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "instance-local");
    let current = f
        .owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .clone();
    assert_eq!(current.revision, 1);
    assert_eq!(
        current.graph["root"]["entries"][0]["children"][0]["options"]["config"]["value"],
        "source"
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        document
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn dependency_recovery_keeps_old_source_save_scopes_revoked_without_replacing_the_activation()
{
    let f = Runtime::new(&[
        (
            "provider",
            "export function apply(ctx) { ctx.provide('gate', {}); }",
        ),
        (
            "consumer",
            "export const inject = ['gate']; export function apply() {}",
        ),
    ])
    .await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("cordis.yml"),
        json!([
            {"id":"provider","name":"provider"},{"id":"consumer","name":"consumer"}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let binding = f.binding("include:consumer");
    let before = f
        .host
        .snapshot(
            "consumer",
            "include:consumer",
            binding.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(before["state"], "running");
    let mut request = json!({"caller":{"hostGeneration":binding.host_generation,"pluginId":"consumer",
        "instanceId":"include:consumer","activationGeneration":binding.activation_generation,"version":"1.0.0",
        "configurationEpoch":before["configurationEpoch"]}, "expectedRevision":2,
        "operation":{"kind":"update","treeId":"include","id":"consumer","options":{"config":{"value":"saved"}}}});
    f.runtime
        .edit_sources(
            1,
            json!({"kind":"update","treeId":"include","id":"provider","options":{"disabled":true}}),
        )
        .await
        .unwrap();
    assert_eq!(
        f.host
            .snapshot(
                "consumer",
                "include:consumer",
                binding.activation_generation
            )
            .await
            .unwrap()["state"],
        "waiting"
    );
    assert!(
        f.services
            .request("loading.edit.submit".into(), request.clone())
            .await
            .is_err()
    );
    f.runtime.edit_sources(2, json!({"kind":"update","treeId":"include","id":"provider","options":{"disabled":false}})).await.unwrap();
    assert_eq!(f.binding("include:consumer"), binding);
    let resumed = f
        .host
        .snapshot(
            "consumer",
            "include:consumer",
            binding.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(resumed["state"], "running");
    assert!(
        resumed["configurationEpoch"].as_u64().unwrap()
            > before["configurationEpoch"].as_u64().unwrap()
    );
    request["expectedRevision"] = json!(3);
    // A duplicate old running notification cannot resurrect its source lease.
    f.runtime.observe_instance_status(&before).unwrap();
    assert!(
        f.services
            .request("loading.edit.submit".into(), request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        3
    );
    request["caller"]["configurationEpoch"] = resumed["configurationEpoch"].clone();
    let saved = f
        .services
        .request("loading.edit.submit".into(), request)
        .await
        .unwrap();
    assert_eq!(saved["revision"], 4);
    f.runtime.apply_current().await.unwrap();
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn sequential_startup_saves_follow_their_own_accepted_config_without_restarting_the_caller() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, writeFileSync } from 'node:fs';
        export async function apply(ctx, config) {
            appendFileSync(config.starts, 'started\n');
            ctx.on('internal/update', next => { writeFileSync(config.receipt, String(next.value)); });
            await ctx.fiber.update({ ...config, value: 1 });
            await ctx.fiber.update({ ...config, value: 2 });
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("sequential-save.txt");
    let starts = f.temp.path().join("sequential-starts.txt");
    fs::write(source_root.join("cordis.yml"), json!([
        {"id":"business","name":"business","config":{"receipt":receipt,"starts":starts,"value":0}}
    ]).to_string()).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let monitor = f.runtime.monitor_sources(f.preparation()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 3, .. }
        )
    })
    .await;
    assert_eq!(fs::read_to_string(receipt).unwrap(), "2");
    assert_eq!(fs::read_to_string(starts).unwrap(), "started\n");
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .graph["root"]["entries"][0]["children"][0]["options"]["config"]["value"],
        2
    );
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();
}

async fn moving_fixture() -> (Runtime, LoadingPreparation) {
    let f = Runtime::new(&[(
        "business",
        "throw new Error('a disabled source edit must not import this module');",
    )])
    .await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(source_root.join("cordis.yml"), "- id: business\n  name: business\n  disabled: true\n- id: nested\n  name: cordis:include\n  config: { path: nested.yml }\n").unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    fs::write(source_root.join("nested.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    (f, preparation)
}

#[tokio::test]
async fn include_initial_sources_commit_before_activation_and_preserve_existing_files() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(_, config) {
            const revision = JSON.parse(readFileSync(config.state, 'utf8')).loading.configuration.revision;
            const content = readFileSync(config.source, 'utf8');
            if (!content.includes('name: business')) throw new Error('source was not saved');
            appendFileSync(config.receipt, `running:${revision}\n`);
        }
    "#)]).await;
    let (root, state) = {
        let owner = f.owner.lock().unwrap();
        (owner.loading_source_root(), owner.root().join("state.json"))
    };
    let receipt = f.temp.path().join("include-initial.log");
    let base = root.join("cordis.yml");
    let source = root.join("created.yml");
    fs::write(root.join("cordis.patch.yml"), "[]\n").unwrap();
    fs::write(&base, "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();

    let include = |path: &str, initial: Value| json!({"id":"nested","name":"cordis:include","config":{"path":path,"initial":initial}});
    let defaults = json!([{"id":"job","name":"business","config":{"state":state,"source":source,"receipt":receipt}}]);
    let failed = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"create","treeId":"include",
        "options":include("created.yml", json!([{"name":"missing-artifact"}]))}),
        )
        .await;
    assert!(failed.is_err());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&base).unwrap(), "[]\n");
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());

    let saved = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"create","treeId":"include",
        "options":include("created.yml", defaults.clone())}),
        )
        .await
        .unwrap();
    assert_eq!(saved.accepted_revision, 2);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "running:2\n");
    let content = fs::read_to_string(&source).unwrap();
    assert!(content.contains("id: job"));
    assert_eq!(
        f.runtime
            .reconcile_sources(2, &preparation)
            .await
            .unwrap()
            .revision,
        2
    );

    let existing = root.join("existing.yml");
    fs::write(&existing, "[]\n").unwrap();
    fs::write(
        &base,
        json!([include("existing.yml", defaults)]).to_string(),
    )
    .unwrap();
    assert_eq!(
        f.runtime
            .reconcile_sources(2, &preparation)
            .await
            .unwrap()
            .revision,
        3
    );
    assert_eq!(fs::read_to_string(existing).unwrap(), "[]\n");
    assert_eq!(fs::read_to_string(&source).unwrap(), content);
    assert_eq!(fs::read_to_string(receipt).unwrap(), "running:2\n");

    let notified = root.join("notified.yml");
    fs::write(
        &base,
        json!([include("notified.yml", json!([]))]).to_string(),
    )
    .unwrap();
    assert_eq!(
        f.runtime
            .reconcile_sources(3, &preparation)
            .await
            .unwrap()
            .revision,
        4
    );
    assert_eq!(fs::read_to_string(notified).unwrap(), "[]\n");
    assert_eq!(fs::read_to_string(source).unwrap(), content);
}

#[tokio::test]
async fn initial_file_recovery_keeps_a_conflicting_external_creation() {
    let f = Runtime::new(&[]).await;
    let owner_root = f.owner.lock().unwrap().root().to_owned();
    let root = f.owner.lock().unwrap().loading_source_root();
    let file = root.join("initial.yml");
    fs::write(root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(root.join("cordis.patch.yml"), "[]\n").unwrap();
    let prepared = f.preparation();
    assert_eq!(
        f.runtime
            .reconcile_sources(0, &prepared)
            .await
            .unwrap()
            .revision,
        1
    );
    let initial = "[]\n";
    // Compile the desired graph with the initialized file among its inputs.
    let candidate = f
        .owner
        .lock()
        .unwrap()
        .prepare_loading_configuration(1, &["cordis.yml".into(), "cordis.patch.yml".into()])
        .unwrap();
    fs::write(&file, initial).unwrap();
    let (_, mut proposal) = f.propose_with_sources(&prepared, json!({
        "kind":"create", "treeId":"include", "options":{
            "id":"nested","name":"cordis:include","config":{"path":"initial.yml","initial":[]}
        }
    }), &["initial.yml"]).await;
    fs::remove_file(&file).unwrap();
    proposal.base_sources_sha256 = candidate.sources_sha256().into();
    proposal.edits.push(LoadingSourceEdit {
        name: "initial.yml".into(),
        expected_sha256: None,
        content: initial.into(),
    });
    let saved_parent = proposal.edits[0].content.clone();
    f.owner
        .lock()
        .unwrap()
        .begin_loading_edit(candidate, proposal)
        .unwrap();
    // The application stopped after saving the parent. While it was closed,
    // an external editor created the child with a different body.
    fs::write(root.join("cordis.yml"), saved_parent).unwrap();
    let external = "- id: external\n  name: cordis:group\n  group: true\n  config: []\n";
    fs::write(&file, external).unwrap();
    let mut reopened = PluginManager::open(owner_root).unwrap();
    assert!(reopened.finish_loading_edit().is_err());
    assert!(reopened.pending_loading_edit().is_some());
    assert_eq!(reopened.loading_configuration().unwrap().revision, 1);
    assert_eq!(fs::read_to_string(&file).unwrap(), external);

    fs::remove_file(&file).unwrap();
    let config = reopened.finish_loading_edit().unwrap();
    assert_eq!(config.revision, 2);
    assert_eq!(fs::read_to_string(file).unwrap(), initial);
}

#[tokio::test]
async fn discovered_include_sources_recover_from_missing_files_and_leave_the_admitted_set_when_removed()
 {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = JSON.parse(readFileSync(config.state, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `running:${revision}\n`);
            ctx.effect(() => () => appendFileSync(config.receipt, 'stopped\n'));
        }
    "#)]).await;
    let root = f.owner.lock().unwrap().loading_source_root();
    let state = f.owner.lock().unwrap().root().join("state.json");
    let receipt = f.temp.path().join("dynamic-include.log");
    let document =
        json!([{"id":"business","name":"business","config":{"state":state,"receipt":receipt}}])
            .to_string();
    fs::write(root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(root.join("cordis.patch.yml"), "[]\n").unwrap();
    fs::create_dir(root.join("modules")).unwrap();
    let file = root.join("modules/managed.yml");
    let preparation = f.preparation();
    let monitor = f.runtime.monitor_sources(preparation.clone()).unwrap();
    let mut observations = monitor.observations();
    for_revision(&mut observations, 1).await;

    let include =
        json!({"id":"nested","name":"cordis:include","config":{"path":"modules/managed.yml"}});
    magi_core::fs_atomic::write_atomic(&root.join("cordis.yml"), json!([include]).to_string())
        .unwrap();
    source_observation(&mut observations, |observation| {
        matches!(observation,
        LoadingSourceObservation::Rejected { message } if message.contains("modules/managed.yml"))
    })
    .await;
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    assert!(!receipt.exists());

    // Only the missing file changes. The monitor must track the unfulfilled
    // reference and enter the ordinary admission path without a manual refresh.
    magi_core::fs_atomic::write_atomic(&file, &document).unwrap();
    for_revision(&mut observations, 2).await;
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "running:2\n");
    let first = f.binding("include:nested:business");

    let removed = f
        .runtime
        .edit_sources(2, json!({"kind":"remove","treeId":"include","id":"nested"}))
        .await
        .unwrap();
    assert_eq!(removed.accepted_revision, 3);
    for_revision(&mut observations, 3).await;
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "running:2\nstopped\n"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), document);
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .sources
            .len(),
        2
    );

    // Creating a reference through the editor must commit the parent file and
    // the new closure together, with a fresh runtime identity.
    let added = f
        .runtime
        .edit_sources(
            3,
            json!({"kind":"create","treeId":"include","options":include}),
        )
        .await
        .unwrap();
    assert_eq!(added.accepted_revision, 4);
    for_revision(&mut observations, 4).await;
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "running:2\nstopped\nrunning:4\n"
    );
    assert!(
        f.binding("include:nested:business").activation_generation > first.activation_generation
    );
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .sources
            .len(),
        3
    );

    // Changing the discovered file itself must still notify after the editor
    // returns and the monitor replaces its tracked closure.
    magi_core::fs_atomic::write_atomic(&file, "[]\n").unwrap();
    for_revision(&mut observations, 5).await;
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "running:2\nstopped\nrunning:4\nstopped\n"
    );
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();

    async fn for_revision(
        observations: &mut watch::Receiver<LoadingSourceObservation>,
        expected: u64,
    ) {
        source_observation(observations, |observation| {
            matches!(observation,
            LoadingSourceObservation::Reconciled { revision, .. } if *revision == expected)
        })
        .await;
    }
}

fn move_to_nested() -> Value {
    json!({"kind":"update","treeId":"include","id":"business","options":{},"targetTreeId":"include:nested","parent":null})
}

#[tokio::test]
async fn a_partial_multi_source_save_survives_restart_without_admitting_the_partial_graph() {
    let (f, preparation) = moving_fixture().await;
    let (candidate, proposal) = f.propose(&preparation, move_to_nested()).await;
    assert_eq!(proposal.edits.len(), 2);
    let first_edit = proposal.edits[0].clone();
    let expected_plan = proposal.plan.clone();
    f.owner
        .lock()
        .unwrap()
        .begin_loading_edit(candidate, proposal)
        .unwrap();
    let source_root = f.owner.lock().unwrap().loading_source_root();
    // Reproduce a process exit after the first atomic replacement, before the
    // second file and canonical revision commit. The journal is the real one.
    magi_core::fs_atomic::write_atomic(&source_root.join(first_edit.name), first_edit.content)
        .unwrap();
    assert!(f.runtime.reconcile_sources(1, &preparation).await.is_err());
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    f.host.stop().await.unwrap();
    let Runtime {
        temp,
        owner,
        runtime,
        host,
        _group,
        ..
    } = f;
    let state_root = owner.lock().unwrap().root().to_path_buf();
    drop(runtime);
    drop(owner);
    drop(host);
    let reopened = PluginManager::open(&state_root).unwrap();
    assert!(reopened.pending_loading_edit().is_some());
    let (reopened, host, runtime, _) =
        Runtime::launch(reopened, &_group, mpsc::unbounded_channel().0).await;
    let monitor = runtime.monitor_sources(preparation).unwrap();
    source_observation(&mut monitor.observations(), |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 2, .. }
        )
    })
    .await;
    let completed = {
        let reopened = reopened.lock().unwrap();
        assert!(reopened.pending_loading_edit().is_none());
        assert!(
            matches!(reopened.loading_application(), Some(LoadingApplication::Applied(token)) if token.revision == 2)
        );
        reopened.loading_configuration().unwrap().clone()
    };
    assert_eq!(completed.revision, 2);
    assert_eq!(completed.graph, expected_plan.graph);
    assert_eq!(completed.sources_sha256, expected_plan.sources_sha256);
    monitor.stop().await.unwrap();
    host.stop().await.unwrap();
    drop(runtime);
    drop(reopened);
    let reopened = PluginManager::open(&state_root).unwrap();
    assert_eq!(reopened.loading_configuration(), Some(&completed));
    assert!(reopened.pending_loading_edit().is_none());
    drop(reopened);
    drop(temp);
}

#[tokio::test]
async fn interrupted_source_edits_preserve_conflicting_external_text_until_explicit_recovery() {
    let (f, preparation) = moving_fixture().await;
    let (candidate, proposal) = f.propose(&preparation, move_to_nested()).await;
    f.owner
        .lock()
        .unwrap()
        .begin_loading_edit(candidate, proposal)
        .unwrap();
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let original_root = fs::read(source_root.join("cordis.yml")).unwrap();
    let external = "- id: external\n  name: other\n  disabled: true\n";
    fs::write(source_root.join("nested.yml"), external).unwrap();
    assert!(f.owner.lock().unwrap().finish_loading_edit().is_err());
    assert_eq!(
        fs::read_to_string(source_root.join("nested.yml")).unwrap(),
        external
    );
    assert_eq!(
        fs::read(source_root.join("cordis.yml")).unwrap(),
        original_root
    );
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_some());
    fs::write(source_root.join("nested.yml"), "[]\n").unwrap();
    let recovered = f.runtime.resume_source_edit().await.unwrap();
    assert_eq!(recovered.revision, 2);
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn a_group_edit_commits_all_source_owners_before_membership_and_config_changes_execute() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = () => JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `${config.name}:${config.value}:${revision()}\n`);
            ctx.effect(() => () => appendFileSync(config.receipt, `stopped:${config.name}:${revision()}\n`));
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("group-edit.log");
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let config = |name: &str, value: &str| json!({"name":name,"value":value,"receipt":receipt,"stateFile":state_file});
    fs::write(
        source_root.join("cordis.yml"),
        json!([
            {"id":"g","name":"cordis:group","group":true,"config":[
                {"id":"a","name":"business","config":config("a","base")},
                {"id":"removed","name":"business","config":config("removed","base")}
            ]}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(
        source_root.join("cordis.patch.yml"),
        json!([
            {"id":"a","config":config("a","user")},
            {"id":"g","insert":[{"id":"extra","name":"business","config":config("extra","user")}]}
        ])
        .to_string(),
    )
    .unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original = f.binding("include:a");
    let expected = f
        .owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .graph["root"]["entries"][0]["children"][0]["options"]["config"]
        .clone();
    let before_source = fs::read(source_root.join("cordis.yml")).unwrap();
    let before_patch = fs::read(source_root.join("cordis.patch.yml")).unwrap();
    let before_log = fs::read_to_string(&receipt).unwrap();
    assert!(
        f.runtime
            .edit_sources(
                1,
                json!({"kind":"reconcile","treeId":"include","parent":"g",
        "entries":[{"id":"a","name":"business","config":config("a","uncommitted")},
            {"id":"bad","name":"not-installed"}]})
            )
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(source_root.join("cordis.yml")).unwrap(),
        before_source
    );
    assert_eq!(
        fs::read(source_root.join("cordis.patch.yml")).unwrap(),
        before_patch
    );
    assert_eq!(fs::read_to_string(&receipt).unwrap(), before_log);
    assert_eq!(f.binding("include:a"), original);
    let members = json!([
        {"id":"created","name":"business","config":config("created","new")},
        {"id":"a","name":"business","config":config("a","edited")}
    ]);
    let edited = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"reconcile","treeId":"include","parent":"g",
        "expectedEntries":expected,"entries":members}),
        )
        .await
        .unwrap();
    assert_eq!(edited.accepted_revision, 2);
    assert_eq!(edited.result, json!(["include:created", "include:a"]));
    assert_eq!(f.binding("include:a"), original);
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:removed")
            .is_none()
    );
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:extra")
            .is_none()
    );
    let lines = fs::read_to_string(&receipt).unwrap();
    assert!(lines.contains("a:edited:2\n"));
    assert!(lines.contains("created:new:2\n"));
    assert!(lines.contains("stopped:removed:2\n"));
    assert!(lines.contains("stopped:extra:2\n"));
    assert!(!lines.contains("uncommitted"));
    let repeat = f
        .runtime
        .edit_sources(
            2,
            json!({"kind":"reconcile","treeId":"include","parent":"g","entries":members}),
        )
        .await
        .unwrap();
    assert_eq!(repeat.accepted_revision, 2);
    assert!(repeat.reconciliation.application.is_none());
    assert_eq!(fs::read_to_string(&receipt).unwrap(), lines);
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn regrouping_existing_members_preserves_source_values_and_retires_obsolete_overrides() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, JSON.stringify({revision, value: config.value, explicit: config.explicit}) + '\n');
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("regroup.log");
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let row = |value: &str| {
        json!({"id":"member","name":"business","config":{
        "value":value,"receipt":receipt,"stateFile":state_file,"explicit":null}})
    };
    fs::write(
        source_root.join("cordis.yml"),
        json!([row("base")]).to_string(),
    )
    .unwrap();
    fs::write(
        source_root.join("cordis.patch.yml"),
        json!([{
            "id":"member","config":{"value":"override","receipt":receipt,"stateFile":state_file}
        }])
        .to_string(),
    )
    .unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original = f.binding("include:member");
    let wanted =
        json!([{"id":"group","name":"cordis:group","group":true,"config":[row("grouped")]}]);
    let rejected = json!([{"id":"group","name":"cordis:group","group":true,
        "config":[row("must-not-run"),{"id":"unavailable","name":"not-installed"}]}]);
    let before = fs::read(&state_file).unwrap();
    assert!(
        f.runtime
            .edit_sources(
                1,
                json!({"kind":"reconcile","treeId":"include","entries":rejected})
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(&state_file).unwrap(), before);
    assert_eq!(f.binding("include:member"), original);

    let edited = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"reconcile","treeId":"include","entries":wanted}),
        )
        .await
        .unwrap();
    assert_eq!(edited.accepted_revision, 2);
    let grouped = f.binding("include:member");
    assert!(grouped.activation_generation > original.activation_generation);
    let patch = f
        .host
        .parse_configuration(
            "cordis.patch.yml",
            &fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch["entries"], json!([]));
    let saved = f
        .host
        .parse_configuration(
            "cordis.yml",
            &fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved["entries"], wanted);
    let lines = fs::read_to_string(&receipt).unwrap();
    let records: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records,
        vec![
            json!({"revision":1,"value":"override"}),
            json!({"revision":2,"value":"grouped","explicit":null})
        ]
    );
    assert!(
        f.host
            .snapshot("business", "include:member", original.activation_generation)
            .await
            .is_err()
    );

    let repeat = f
        .runtime
        .edit_sources(
            2,
            json!({"kind":"reconcile","treeId":"include","entries":wanted}),
        )
        .await
        .unwrap();
    assert_eq!(repeat.accepted_revision, 2);
    assert!(repeat.reconciliation.application.is_none());
    assert_eq!(fs::read_to_string(&receipt).unwrap(), lines);

    let ungrouped = f
        .runtime
        .edit_sources(
            2,
            json!({"kind":"reconcile","treeId":"include","entries":[row("ungrouped")]}),
        )
        .await
        .unwrap();
    assert_eq!(ungrouped.accepted_revision, 3);
    assert!(f.binding("include:member").activation_generation > grouped.activation_generation);
    let reopened = PluginManager::open(f.owner.lock().unwrap().root()).unwrap();
    assert_eq!(reopened.loading_configuration().unwrap().revision, 3);
    assert_eq!(
        reopened.loading_configuration().unwrap().graph["root"]["entries"][0]["children"][0]["options"],
        row("ungrouped")
    );
    assert!(reopened.loading_application().is_none());
    assert!(
        reopened
            .loading_instance_binding("include:member")
            .is_none()
    );
    let records: Vec<Value> = fs::read_to_string(&receipt)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records.last().unwrap(),
        &json!({"revision":3,"value":"ungrouped","explicit":null})
    );
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn memory_root_entries_use_the_same_commit_and_survive_a_real_host_rebuild() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `${ctx.fiber.entry.id}:${config.value}:${revision}\n`);
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let receipt = f.temp.path().join("memory-root.log");
    let row = |id: &str, value: &str| {
        json!({"id":id,"name":"business","config":{
        "stateFile":state_file,"receipt":receipt,"value":value}})
    };
    let original_source = json!([row("from-file", "file")]).to_string();
    fs::write(source_root.join("cordis.yml"), &original_source).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original_file = f.binding("include:from-file");
    let created = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"create","treeId":"","options":row("root-member","first")}),
        )
        .await
        .unwrap();
    assert_eq!(created.result, "root-member");
    assert_eq!(created.accepted_revision, 2);
    assert_eq!(f.binding("include:from-file"), original_file);
    let root_binding = f.binding("root-member");
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "include:from-file:file:1\nroot-member:first:2\n"
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        original_source
    );
    let before = fs::read(&state_file).unwrap();
    assert!(
        f.runtime
            .edit_sources(
                2,
                json!({"kind":"create","treeId":"","options":{"id":"bad","name":"not-installed"}})
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(&state_file).unwrap(), before);
    let updated = f.runtime.edit_sources(2,
        json!({"kind":"update","treeId":"","id":"root-member","options":{"config":row("root-member","second")["config"]}})).await.unwrap();
    assert_eq!(updated.accepted_revision, 3);
    assert_eq!(f.binding("root-member"), root_binding);
    assert!(
        f.runtime
            .edit_sources(2, json!({"kind":"remove","treeId":"","id":"root-member"}))
            .await
            .is_err()
    );
    let moved = f.runtime.edit_sources(3,
        json!({"kind":"update","treeId":"include","id":"from-file","options":{},"targetTreeId":""})).await.unwrap();
    assert_eq!(moved.accepted_revision, 4);
    assert_eq!(moved.result, "from-file");
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:from-file")
            .is_none()
    );
    f.binding("from-file");
    assert!(
        f.host
            .snapshot(
                "business",
                "include:from-file",
                original_file.activation_generation
            )
            .await
            .is_err()
    );
    let restored = f.runtime.edit_sources(4,
        json!({"kind":"update","treeId":"","id":"from-file","options":{},"targetTreeId":"include"})).await.unwrap();
    assert_eq!(restored.accepted_revision, 5);
    assert_eq!(restored.result, "include:from-file");
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("from-file")
            .is_none()
    );
    f.host.stop().await.unwrap();

    // The old process is stopped. Reopen the durable owner, allocate a new
    // host generation, and recover the same graph through ordinary intake.
    let recovered = PluginManager::open(f.owner.lock().unwrap().root()).unwrap();
    assert!(recovered.loading_application().is_none());
    assert!(recovered.loading_instance_binding("root-member").is_none());
    let (recovered, host, runtime, _services) =
        Runtime::launch(recovered, &f._group, mpsc::unbounded_channel().0).await;
    let restarted = runtime.reconcile_sources(5, &preparation).await.unwrap();
    assert_eq!(restarted.revision, 5);
    let current = recovered
        .lock()
        .unwrap()
        .loading_instance_binding("root-member")
        .unwrap()
        .clone();
    assert_eq!(current.host_generation, host.generation());
    assert!(current.activation_generation > root_binding.activation_generation);
    assert_eq!(
        current.executable.fingerprint,
        root_binding.executable.fingerprint
    );
    assert_eq!(
        host.snapshot("business", "root-member", current.activation_generation)
            .await
            .unwrap()["state"],
        "running"
    );
    let lines = fs::read_to_string(&receipt).unwrap();
    assert!(lines.contains("root-member:second:5\n"));
    assert!(lines.contains("include:from-file:file:5\n"));

    let without_source = runtime
        .edit_sources(5, json!({"kind":"remove","treeId":"","id":"include"}))
        .await
        .unwrap();
    assert_eq!(without_source.accepted_revision, 6);
    assert_eq!(
        recovered
            .lock()
            .unwrap()
            .loading_instance_binding("root-member")
            .unwrap(),
        &current
    );
    assert!(
        recovered
            .lock()
            .unwrap()
            .loading_instance_binding("include:from-file")
            .is_none()
    );
    let cleared = runtime
        .edit_sources(6, json!({"kind":"reconcile","treeId":"","entries":[]}))
        .await
        .unwrap();
    assert_eq!(cleared.accepted_revision, 7);
    assert_eq!(
        cleared.reconciliation.application.unwrap()["instances"],
        json!([])
    );
    let unchanged = runtime.reconcile_sources(7, &preparation).await.unwrap();
    assert_eq!(unchanged.revision, 7);
    assert!(unchanged.application.is_none());
    assert_eq!(
        recovered
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .graph["root"]["entries"],
        json!([])
    );
    assert_eq!(fs::read_to_string(&receipt).unwrap(), lines);
    assert!(recovered.lock().unwrap().pending_loading_edit().is_none());
    host.stop().await.unwrap();
}

#[tokio::test]
async fn a_memory_root_fiber_saves_through_the_owner_before_the_new_config_executes() {
    let f = Runtime::new(&[("business", r#"
        import { writeFileSync, readFileSync } from 'node:fs';
        export async function apply(ctx, config) {
            if (!config.saved) await ctx.fiber.update({ ...config, saved: true });
            else {
                const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
                writeFileSync(config.receipt, `${ctx.fiber.entry.id}:saved:${revision}`);
            }
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(source_root.join("cordis.yml"), "[]\n").unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    let monitor = f.runtime.monitor_sources(preparation.clone()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 1, .. }
        )
    })
    .await;
    let receipt = f.temp.path().join("memory-root-save.txt");
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let saved = f
        .runtime
        .edit_sources(
            1,
            json!({"kind":"create","treeId":"","options":{"id":"root-member","name":"business",
            "config":{"receipt":receipt,"stateFile":state_file}}}),
        )
        .await
        .unwrap();
    assert_eq!(saved.accepted_revision, 2);
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 3, .. }
        )
    })
    .await;
    assert_eq!(fs::read_to_string(receipt).unwrap(), "root-member:saved:3");
    let graph = f
        .owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .graph
        .clone();
    assert_eq!(
        graph["root"]["entries"][1]["options"]["config"]["saved"],
        true
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        "[]\n"
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap(),
        "[]\n"
    );
    assert!(f.owner.lock().unwrap().pending_loading_edit().is_none());
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn an_interrupted_file_to_root_move_recovers_one_complete_configuration() {
    let f = Runtime::new(&[("business", r#"
        import { appendFileSync } from 'node:fs';
        export function apply(ctx, config) { appendFileSync(config.receipt, ctx.fiber.entry.id + '\n'); }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("interrupted-root-move.log");
    fs::write(
        source_root.join("cordis.yml"),
        json!([
            {"id":"member","name":"business","config":{"receipt":receipt}}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original = f.binding("include:member");
    let (candidate, proposal) = f.propose(&preparation,
        json!({"kind":"update","treeId":"include","id":"member","options":{},"targetTreeId":""})).await;
    assert_eq!(proposal.result, "member");
    assert_eq!(proposal.edits.len(), 1);
    f.owner
        .lock()
        .unwrap()
        .begin_loading_edit(candidate, proposal.clone())
        .unwrap();
    // Simulate interruption after the candidate file rename and before CAS.
    let edit = &proposal.edits[0];
    fs::write(source_root.join(&edit.name), &edit.content).unwrap();
    assert!(f.runtime.reconcile_sources(1, &preparation).await.is_err());
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(f.binding("include:member"), original);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "include:member\n");
    f.host.stop().await.unwrap();

    let mut reopened = PluginManager::open(f.owner.lock().unwrap().root()).unwrap();
    assert!(reopened.pending_loading_edit().is_some());
    assert_eq!(
        reopened.loading_configuration().unwrap().graph["root"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let committed = reopened.finish_loading_edit().unwrap();
    assert_eq!(committed.revision, 2);
    assert_eq!(committed.graph, proposal.plan.graph);
    assert_eq!(committed.executables, proposal.plan.executables);
    assert_eq!(committed.executables[0].instance_id, "member");
    assert!(reopened.pending_loading_edit().is_none());
    assert!(reopened.loading_instance_binding("member").is_none());
    assert!(reopened.loading_application().is_none());
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "include:member\n");
    let recovered_again = PluginManager::open(reopened.root()).unwrap();
    assert_eq!(recovered_again.loading_configuration().unwrap(), &committed);
    assert!(recovered_again.pending_loading_edit().is_none());
}

#[tokio::test]
async fn host_synchronization_joins_saved_intent_and_refreshes_only_when_requested() {
    let f = Runtime::new(&[("observer", "export function apply() {}"), ("business", r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `${config.value}:${revision}\n`);
        }
    "#)]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let receipt = f.temp.path().join("host-synchronization.log");
    let config = |value: &str| json!({"stateFile":state_file,"receipt":receipt,"value":value});
    let document = |value: &str| {
        json!([{"id":"observer","name":"observer"},
        {"id":"business","name":"business","config":config(value)}])
        .to_string()
    };
    fs::write(source_root.join("cordis.yml"), document("initial")).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let binding = f.binding("include:observer");
    let observer_status = f
        .host
        .snapshot(
            "observer",
            "include:observer",
            binding.activation_generation,
        )
        .await
        .unwrap();
    let caller = json!({"hostGeneration":binding.host_generation,"instanceId":binding.executable.instance_id,
        "pluginId":binding.executable.plugin_id,"activationGeneration":binding.activation_generation,
        "version":binding.executable.version,"configurationEpoch":observer_status["configurationEpoch"]});
    let sync = |minimum_revision: u64, refresh: bool| json!({"caller":caller,"minimumRevision":minimum_revision,"refresh":refresh});
    let submitted = f.runtime.submit_source_edit(1,
        json!({"kind":"update","treeId":"include","id":"business","options":{"config":config("saved")}})).await.unwrap();
    assert_eq!(submitted.revision, 2);
    assert!(f.owner.lock().unwrap().loading_application().is_none());
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "initial:1\n");
    let flushed = f
        .services
        .request("loading.sources.synchronize".into(), sync(2, false))
        .await
        .unwrap();
    assert_eq!(flushed, json!({"revision":2}));
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "initial:1\nsaved:2\n"
    );

    let external = document("external");
    fs::write(source_root.join("cordis.yml"), &external).unwrap();
    assert_eq!(
        f.services
            .request("loading.sources.synchronize".into(), sync(2, false))
            .await
            .unwrap(),
        json!({"revision":2})
    );
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "initial:1\nsaved:2\n"
    );
    let refreshed = f
        .services
        .request("loading.sources.synchronize".into(), sync(2, true))
        .await
        .unwrap();
    assert_eq!(refreshed, json!({"revision":3}));
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        external
    );
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "initial:1\nsaved:2\nexternal:3\n"
    );
    fs::write(source_root.join("cordis.yml"), "- name: [editing").unwrap();
    assert!(
        f.services
            .request("loading.sources.synchronize".into(), sync(3, true))
            .await
            .is_err()
    );
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        3
    );
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.yml")).unwrap(),
        "- name: [editing"
    );
    assert!(
        f.services
            .request("loading.sources.synchronize".into(), sync(4, false))
            .await
            .is_err()
    );
    let mut stale = sync(3, false);
    stale["caller"]["configurationEpoch"] = json!(99);
    assert!(
        f.services
            .request("loading.sources.synchronize".into(), stale)
            .await
            .is_err()
    );
    let mut malformed = sync(3, false);
    malformed["extra"] = json!(true);
    assert!(
        f.services
            .request("loading.sources.synchronize".into(), malformed)
            .await
            .is_err()
    );
    fs::write(source_root.join("cordis.yml"), document("restored")).unwrap();
    assert_eq!(
        f.services
            .request("loading.sources.synchronize".into(), sync(3, true))
            .await
            .unwrap(),
        json!({"revision":4})
    );
    let log = fs::read_to_string(&receipt).unwrap();
    assert!(log.ends_with("restored:4\n"));
    f.runtime
        .edit_sources(
            4,
            json!({"kind":"update","treeId":"include","id":"observer","options":{"disabled":true}}),
        )
        .await
        .unwrap();
    assert!(
        f.services
            .request("loading.sources.synchronize".into(), sync(4, true))
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&receipt).unwrap(), log);
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn host_synchronization_does_not_wait_for_business_startup() {
    let f = Runtime::new(&[
        ("observer", "export function apply() {}"),
        (
            "launcher",
            r#"
            import { writeFileSync } from 'node:fs';
            export async function apply(ctx, config) {
                await new Promise(resolve => ctx.inject(['laterService'], resolve));
                writeFileSync(config.receipt, 'ready');
            }
        "#,
        ),
        (
            "provider",
            "export function apply(ctx) { ctx.provide('laterService', {}); }",
        ),
    ])
    .await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("cordis.yml"),
        "[{\"id\":\"observer\",\"name\":\"observer\"}]",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let binding = f.binding("include:observer");
    let observer_status = f
        .host
        .snapshot(
            "observer",
            "include:observer",
            binding.activation_generation,
        )
        .await
        .unwrap();
    let receipt = f.temp.path().join("sync-starting.txt");
    let saved = f.runtime.submit_source_edit(1,
        json!({"kind":"create","treeId":"","options":{"id":"launcher","name":"launcher","config":{"receipt":receipt}}})).await.unwrap();
    assert_eq!(saved.revision, 2);
    let flushed = tokio::time::timeout(Duration::from_secs(5),
        f.services.request("loading.sources.synchronize".into(), json!({"minimumRevision":2,"refresh":false,
            "caller":{"hostGeneration":binding.host_generation,"instanceId":binding.executable.instance_id,
                "pluginId":binding.executable.plugin_id,"activationGeneration":binding.activation_generation,
                "version":binding.executable.version,"configurationEpoch":observer_status["configurationEpoch"]}}))).await.unwrap().unwrap();
    assert_eq!(flushed, json!({"revision":2}));
    let launcher = f.binding("launcher");
    assert_eq!(
        f.host
            .snapshot("launcher", "launcher", launcher.activation_generation)
            .await
            .unwrap()["state"],
        "starting"
    );
    assert!(!receipt.exists());
    f.runtime
        .edit_sources(
            2,
            json!({"kind":"create","treeId":"","options":{"id":"provider","name":"provider"}}),
        )
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(receipt).unwrap(), "ready");
    f.host.stop().await.unwrap();
}

async fn source_observation(
    observations: &mut watch::Receiver<LoadingSourceObservation>,
    matches: impl Fn(&LoadingSourceObservation) -> bool,
) -> LoadingSourceObservation {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = observations.borrow_and_update().clone();
            if matches(&current) {
                return current;
            }
            observations
                .changed()
                .await
                .expect("source monitor closed before the expected observation");
        }
    })
    .await
    .expect("expected a real filesystem notification and intake outcome")
}

#[tokio::test]
async fn a_starting_plugin_can_enable_its_dependency_through_the_real_source_monitor() {
    let f = Runtime::new(&[
        (
            "launcher",
            r#"
            import { writeFileSync } from 'node:fs';
            export async function apply(ctx, config) {
                writeFileSync(config.patch, '- id: provider\n  disabled: false\n');
                await new Promise(resolve => ctx.inject(['businessGate'], () => resolve()));
                writeFileSync(config.receipt, 'dependency available');
            }
        "#,
        ),
        (
            "provider",
            "export function apply(ctx) { ctx.provide('businessGate', {}); }",
        ),
    ])
    .await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let receipt = f.temp.path().join("startup-source-edit.log");
    fs::write(source_root.join("cordis.yml"), json!([
        {"id":"launcher","name":"launcher","config":{"patch":source_root.join("cordis.patch.yml"),"receipt":receipt}},
        {"id":"provider","name":"provider","disabled":true},
    ]).to_string()).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let monitor = f.runtime.monitor_sources(f.preparation()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 2, .. }
        )
    })
    .await;
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "dependency available"
    );
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn source_submission_during_an_update_does_not_wait_for_that_updates_application_receipt() {
    let mut f = Runtime::new(&[
        ("business", r#"
            import { appendFileSync, readFileSync, watch } from 'node:fs';
            export function apply(ctx, config) {
                appendFileSync(config.receipt, 'started\n');
                ctx.on('internal/update', async next => {
                    await new Promise(resolve => {
                        const watcher = watch(next.gate, () => {
                            if (readFileSync(next.gate, 'utf8') !== 'release') return;
                            watcher.close();
                            resolve();
                        });
                        ctx.effect(() => () => watcher.close());
                        ctx.logger.info('update-awaiting-source-submission');
                    });
                    const revision = JSON.parse(readFileSync(next.stateFile, 'utf8')).loading.configuration.revision;
                    appendFileSync(next.receipt, `updated:${revision}\n`);
                });
            }
        "#),
        ("provider", r#"
            import { appendFileSync, readFileSync } from 'node:fs';
            export function apply(ctx, config) {
                const revision = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
                appendFileSync(config.receipt, `provider:${revision}\n`);
            }
        "#),
    ]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let receipt = f.temp.path().join("submission.log");
    let gate = f.temp.path().join("update-gate");
    fs::write(&gate, "waiting").unwrap();
    let config = json!({"receipt": receipt, "stateFile": state_file, "gate": gate, "value": 1});
    fs::write(
        source_root.join("cordis.yml"),
        json!([
            {"id":"business","name":"business","config":config}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original_binding = f.binding("include:business");
    let runtime = f.runtime.clone();
    let mut next = config;
    next["value"] = json!(2);
    let updating = tokio::spawn(async move {
        runtime
            .edit_sources(
                1,
                json!({"kind":"update","treeId":"include","id":"business",
            "options":{"config":next}}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some((method, diagnostic)) = f.events.recv().await {
            if method == "host.diagnostic"
                && diagnostic["message"] == "update-awaiting-source-submission"
            {
                return;
            }
        }
        panic!("host closed before the update callback");
    })
    .await
    .unwrap();
    assert!(!updating.is_finished());
    let submission = tokio::time::timeout(
        Duration::from_secs(5),
        f.runtime.submit_source_edit(
            2,
            json!({"kind":"create","treeId":"include","options":{"id":"provider","name":"provider",
            "config":{"receipt":receipt,"stateFile":state_file}}}),
        ),
    )
    .await
    .expect("admission must not wait for the callback that requested it")
    .unwrap();
    assert_eq!(submission.revision, 3);
    assert_eq!(submission.result, "include:provider");
    {
        let owner = f.owner.lock().unwrap();
        assert_eq!(owner.loading_configuration().unwrap().revision, 3);
        assert!(
            owner.loading_application().is_none(),
            "old application is not the new revision's receipt"
        );
        assert!(owner.loading_instance_binding("include:provider").is_none());
    }
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "started\n");
    assert!(
        f.runtime
            .submit_source_edit(
                2,
                json!({"kind":"remove","treeId":"include","id":"business"})
            )
            .await
            .is_err()
    );
    fs::write(&gate, "release").unwrap();
    let latest = tokio::time::timeout(Duration::from_secs(5), updating)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(latest.accepted_revision, 2);
    assert_eq!(latest.reconciliation.revision, 3);
    assert_eq!(
        latest.reconciliation.application.unwrap()["token"]["revision"],
        3
    );
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "started\nupdated:3\nprovider:3\n"
    );
    assert_eq!(f.binding("include:business"), original_binding);
    assert_eq!(
        f.host
            .snapshot(
                "provider",
                "include:provider",
                f.binding("include:provider").activation_generation
            )
            .await
            .unwrap()["state"],
        "running"
    );
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn real_file_notifications_admit_changes_and_stopping_the_monitor_blocks_late_edits() {
    let f = Runtime::new(&[("business", "export function apply() {}")]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("cordis.yml"),
        "- id: business\n  name: business\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let monitor = f.runtime.monitor_sources(f.preparation()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 1, .. }
        )
    })
    .await;
    let first = f.binding("include:business");

    let broken = "- id: business\n  disabled: [\n";
    fs::write(source_root.join("cordis.patch.yml"), broken).unwrap();
    source_observation(&mut observations, |observation| {
        matches!(observation, LoadingSourceObservation::Rejected { .. })
    })
    .await;
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap(),
        broken
    );
    assert_eq!(f.binding("include:business"), first);
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );

    fs::write(
        source_root.join("cordis.patch.yml"),
        "- id: business\n  disabled: true\n",
    )
    .unwrap();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 2, .. }
        )
    })
    .await;
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:business")
            .is_none()
    );
    magi_core::fs_atomic::write_atomic(
        &source_root.join("cordis.patch.yml"),
        "- id: business\n  disabled: false\n",
    )
    .unwrap();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 3, .. }
        )
    })
    .await;
    assert!(f.binding("include:business").activation_generation > first.activation_generation);
    monitor.stop().await.unwrap();
    // The joined worker may have published another verdict for a queued event before stopping.
    observations.borrow_and_update();
    // There is no sleeping to guess that monitoring stopped: the worker is joined.
    fs::write(
        source_root.join("cordis.patch.yml"),
        "- id: business\n  disabled: true\n",
    )
    .unwrap();
    assert!(observations.changed().await.is_err());
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        3
    );
    assert_eq!(
        f.host
            .snapshot(
                "business",
                "include:business",
                f.binding("include:business").activation_generation
            )
            .await
            .unwrap()["state"],
        "running"
    );
    f.host.stop().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn source_notifications_follow_a_configuration_symlink_inside_the_loading_project() {
    let f = Runtime::new(&[("business", "export function apply() {}")]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let target = source_root.join("actual.yml");
    fs::write(&target, "- id: business\n  name: business\n").unwrap();
    std::os::unix::fs::symlink(&target, source_root.join("cordis.yml")).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let monitor = f.runtime.monitor_sources(f.preparation()).unwrap();
    let mut observations = monitor.observations();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 1, .. }
        )
    })
    .await;
    fs::write(
        &target,
        "- id: business\n  name: business\n  disabled: true\n",
    )
    .unwrap();
    source_observation(&mut observations, |observation| {
        matches!(
            observation,
            LoadingSourceObservation::Reconciled { revision: 2, .. }
        )
    })
    .await;
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:business")
            .is_none()
    );
    monitor.stop().await.unwrap();
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn file_edits_are_admitted_before_execution_and_invalid_edits_leave_saved_intent_and_live_state_distinct()
 {
    let f = Runtime::new(&[(
        "business",
        r#"
        import { appendFileSync, readFileSync } from 'node:fs';
        export function apply(ctx, config) {
            const committed = JSON.parse(readFileSync(config.stateFile, 'utf8')).loading.configuration.revision;
            appendFileSync(config.receipt, `started:${committed}\n`);
            ctx.effect(() => () => appendFileSync(config.receipt, 'stopped\n'));
        }
    "#,
    )])
    .await;
    let receipt = f.temp.path().join("business.log");
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    fs::write(
        source_root.join("cordis.yml"),
        json!([
            {"id":"business","name":"business","config":{"receipt":receipt,"stateFile":state_file}}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    let first = f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(
        first.application.as_ref().unwrap()["instances"][0]["state"],
        "running"
    );
    let binding = f.binding("include:business");
    let unchanged = f.runtime.reconcile_sources(1, &preparation).await.unwrap();
    assert_eq!(unchanged.revision, 1);
    assert!(unchanged.application.is_none());
    assert_eq!(f.binding("include:business"), binding);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "started:1\n");

    let invalid = "- id: business\n  disabled: [\n";
    fs::write(source_root.join("cordis.patch.yml"), invalid).unwrap();
    assert!(f.runtime.reconcile_sources(1, &preparation).await.is_err());
    assert_eq!(
        fs::read_to_string(source_root.join("cordis.patch.yml")).unwrap(),
        invalid
    );
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(f.binding("include:business"), binding);
    assert_eq!(
        f.host
            .snapshot(
                "business",
                "include:business",
                binding.activation_generation
            )
            .await
            .unwrap()["state"],
        "running"
    );
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "started:1\n");

    fs::write(
        source_root.join("cordis.patch.yml"),
        "- id: business\n  disabled: true\n",
    )
    .unwrap();
    assert!(f.runtime.reconcile_sources(0, &preparation).await.is_err());
    assert_eq!(fs::read_to_string(&receipt).unwrap(), "started:1\n");
    let stopped = f.runtime.reconcile_sources(1, &preparation).await.unwrap();
    assert_eq!(stopped.revision, 2);
    assert_eq!(
        stopped.application.as_ref().unwrap()["instances"],
        json!([])
    );
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "started:1\nstopped\n"
    );

    fs::write(
        source_root.join("cordis.patch.yml"),
        "- id: business\n  disabled: false\n",
    )
    .unwrap();
    let restarted = f.runtime.reconcile_sources(2, &preparation).await.unwrap();
    assert_eq!(restarted.revision, 3);
    assert_eq!(
        restarted.application.as_ref().unwrap()["instances"][0]["state"],
        "running"
    );
    assert!(f.binding("include:business").activation_generation > binding.activation_generation);
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        "started:1\nstopped\nstarted:3\n"
    );
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn concurrent_source_intakes_cannot_both_commit_the_same_expected_revision() {
    let f = Runtime::new(&[("business", "export function apply() {}")]).await;
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("cordis.yml"),
        "- id: business\n  name: business\n",
    )
    .unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    let (first, second) = tokio::join!(
        f.runtime.reconcile_sources(0, &preparation),
        f.runtime.reconcile_sources(0, &preparation),
    );
    assert!(first.is_ok());
    assert!(second.is_err());
    assert_eq!(
        f.owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        f.host
            .snapshot("business", "include:business", 1)
            .await
            .unwrap()["state"],
        "running"
    );
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn repeated_source_notifications_do_not_replay_a_failed_startups_side_effects() {
    let f = Runtime::new(&[
        ("observer", "export function apply() {}"),
        (
            "business",
            r#"
        import { appendFileSync, existsSync } from 'node:fs';
        export function apply(ctx, config) {
            appendFileSync(config.attempts, config.id + '\n');
            if (!existsSync(config.ready)) throw new Error('failed after performing a side effect');
            appendFileSync(config.attempts, 'running:' + config.id + '\n');
        }
    "#,
        ),
    ])
    .await;
    let attempts = f.temp.path().join("attempts.log");
    let ready = f.temp.path().join("ready");
    let source_root = f.owner.lock().unwrap().loading_source_root();
    let source_file = source_root.join("cordis.yml");
    let document = json!([
        {"id":"observer","name":"observer"},
        {"id":"left","name":"business","config":{"id":"left","attempts":attempts,"ready":ready}},
        {"id":"right","name":"business","config":{"id":"right","attempts":attempts,"ready":ready}}
    ]);
    fs::write(&source_file, document.to_string()).unwrap();
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    let preparation = f.preparation();
    let first = f.runtime.reconcile_sources(0, &preparation).await.unwrap();
    let first_instances = first.application.unwrap();
    let left = &first_instances["instances"][1];
    let right = &first_instances["instances"][2];
    assert_eq!(left["state"], "failed");
    assert_eq!(right["state"], "failed");
    let duplicate = f.runtime.reconcile_sources(1, &preparation).await.unwrap();
    assert!(duplicate.application.is_none());
    assert_eq!(duplicate.revision, 1);
    // Even admitting a distinct revision cannot rerun a failed generation
    // whose effective entry is unchanged.
    let updated_source = format!("# saved by user\n{document}");
    fs::write(&source_file, &updated_source).unwrap();
    let new_revision = f.runtime.reconcile_sources(1, &preparation).await.unwrap();
    assert_eq!(new_revision.revision, 2);
    assert_eq!(new_revision.application.unwrap()["instances"][1], *left);
    let first_attempts = fs::read_to_string(&attempts).unwrap();
    let mut attempted = first_attempts.lines().collect::<Vec<_>>();
    attempted.sort_unstable();
    assert_eq!(attempted, ["left", "right"]);
    fs::write(&ready, "").unwrap();
    let observer = &first_instances["instances"][0];
    let mut request = json!({"caller":{
        "hostGeneration":observer["hostGeneration"], "pluginId":observer["pluginId"], "instanceId":observer["instanceId"],
        "version":observer["version"], "activationGeneration":observer["activationGeneration"],
        "configurationEpoch":observer["configurationEpoch"],
    }, "expectedRevision":1, "action":"activate",
       "targets":[{"instanceId":left["instanceId"], "activationGeneration":left["activationGeneration"]}]});
    let state_file = f.owner.lock().unwrap().root().join("state.json");
    let before_rejected = fs::read(&state_file).unwrap();
    assert!(
        f.services
            .request("loading.instances.change".into(), request.clone())
            .await
            .is_err()
    );
    request["expectedRevision"] = json!(2);
    let mut stale_target = request.clone();
    stale_target["targets"][0]["activationGeneration"] =
        json!(left["activationGeneration"].as_u64().unwrap() + 1);
    assert!(
        f.services
            .request("loading.instances.change".into(), stale_target)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&state_file).unwrap(), before_rejected);
    assert_eq!(fs::read_to_string(&attempts).unwrap(), first_attempts);

    let receipt = f
        .services
        .request("loading.instances.change".into(), request.clone())
        .await
        .unwrap();
    assert_eq!(receipt, json!({"revision":2}));
    let binding = f.binding("include:left");
    assert!(binding.activation_generation > left["activationGeneration"].as_u64().unwrap());
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:right")
            .is_none()
    );
    assert_eq!(
        f.host
            .snapshot("business", "include:left", binding.activation_generation)
            .await
            .unwrap()["state"],
        "running"
    );
    assert_eq!(
        fs::read_to_string(&attempts).unwrap(),
        format!("{first_attempts}left\nrunning:left\n")
    );
    assert_eq!(fs::read_to_string(&source_file).unwrap(), updated_source);
    assert!(
        f.services
            .request("loading.instances.change".into(), request)
            .await
            .is_err()
    );
    let mut corrected_document = document;
    corrected_document[2]["config"]["value"] = json!("corrected");
    fs::write(&source_file, corrected_document.to_string()).unwrap();
    let corrected = f.runtime.reconcile_sources(2, &preparation).await.unwrap();
    assert_eq!(corrected.revision, 3);
    let corrected = corrected.application.unwrap();
    assert_eq!(corrected["instances"][2]["state"], "running");
    assert_eq!(f.binding("include:left"), binding);
    assert_eq!(
        fs::read_to_string(&attempts).unwrap(),
        format!("{first_attempts}left\nrunning:left\nright\nrunning:right\n")
    );
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn distinct_instances_update_disable_and_reenable_through_committed_revisions() {
    let mut f = Runtime::new(&[(
        "business",
        r#"
        import { writeFileSync, appendFileSync } from 'node:fs';
        export function apply(ctx, config) {
            writeFileSync(config.path, config.value);
            ctx.effect(() => () => appendFileSync(config.path, ':closed'));
        }
    "#,
    )])
    .await;
    let left = f.temp.path().join("left");
    let right = f.temp.path().join("right");
    let document = |value: &str, disabled: bool| {
        json!([
            {"id":"left","name":"business","disabled":disabled,"config":{"path":left,"value":value}},
            {"id":"right","name":"business","config":{"path":right,"value":"right"}},
        ])
    };
    f.accept(&document("first", false)).await;
    let receipt = f.runtime.apply_current().await.unwrap();
    assert_eq!(receipt["instances"][0]["state"], "running");
    f.observe_pending();
    let first = f.binding("include:left");
    let unchanged = f.binding("include:right");
    assert_eq!(first.activation_order, unchanged.activation_order);
    assert_eq!(fs::read_to_string(&left).unwrap(), "first");

    f.accept(&document("second", false)).await;
    f.runtime.apply_current().await.unwrap();
    f.observe_pending();
    assert_eq!(f.binding("include:left"), first);
    assert_eq!(f.binding("include:right"), unchanged);
    assert_eq!(fs::read_to_string(&left).unwrap(), "second");
    assert_eq!(fs::read_to_string(&right).unwrap(), "right");

    f.accept(&document("second", true)).await;
    f.runtime.apply_current().await.unwrap();
    f.observe_pending();
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:left")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&left).unwrap(), "second:closed");
    assert_eq!(f.binding("include:right"), unchanged);

    f.accept(&document("third", false)).await;
    f.runtime.apply_current().await.unwrap();
    f.observe_pending();
    let replacement = f.binding("include:left");
    assert!(replacement.activation_generation > first.activation_generation);
    assert_eq!(replacement.activation_order, first.activation_order);
    assert_eq!(fs::read_to_string(&left).unwrap(), "third");
    f.runtime
        .observe_instance_status(
            &json!({"state":"stopped","hostGeneration":first.host_generation,
        "instanceId":first.executable.instance_id,"pluginId":first.executable.plugin_id,
        "activationGeneration":first.activation_generation}),
        )
        .unwrap();
    assert_eq!(f.binding("include:left"), replacement);
    f.host.stop().await.unwrap();
    assert_eq!(fs::read_to_string(&right).unwrap(), "right:closed");
}

#[tokio::test]
async fn failed_module_does_not_claim_running_or_remove_a_healthy_instance() {
    let mut f = Runtime::new(&[
        ("healthy", "export function apply() {}"),
        ("broken", "throw new Error('original module failure');"),
    ])
    .await;
    f.accept(&json!([
        {"id":"healthy","name":"healthy"}, {"id":"broken","name":"broken"},
    ]))
    .await;
    let receipt = f.runtime.apply_current().await.unwrap();
    assert_eq!(receipt["instances"][0]["state"], "running");
    assert_eq!(receipt["instances"][1]["state"], "failed");
    assert_eq!(receipt["instances"][1]["error"], "original module failure");
    f.observe_pending();
    let healthy = f.binding("include:healthy");
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:broken")
            .is_none()
    );
    f.runtime.apply_current().await.unwrap();
    f.observe_pending();
    assert_eq!(f.binding("include:healthy"), healthy);
    f.host.stop().await.unwrap();
}

#[tokio::test]
async fn abandoning_graph_application_during_import_reclaims_the_process_and_retains_committed_intent()
 {
    use notify::Watcher;

    let f = Runtime::new(&[(
        "slow",
        r#"
        import { writeFileSync } from 'node:fs';
        writeFileSync(new URL('./import-entered', import.meta.url), 'entered');
        await new Promise(() => {});
        export function apply() {}
    "#,
    )])
    .await;
    f.accept(&json!([{"id":"slow","name":"slow"}])).await;
    let (entered, mut imports) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = entered.send(event);
    })
    .unwrap();
    let package_directory = f.project.join("node_modules/slow");
    watcher
        .watch(&package_directory, notify::RecursiveMode::NonRecursive)
        .unwrap();
    let runtime = f.runtime.clone();
    let applying = tokio::spawn(async move { runtime.apply_current().await });
    // Only pending imports belong to graph materialization. An asynchronous
    // business callback now returns a real `starting` projection independently.
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = imports.recv().await {
            if event
                .unwrap()
                .paths
                .contains(&package_directory.join("import-entered"))
            {
                return;
            }
        }
        panic!("source watcher closed before import started");
    })
    .await
    .unwrap();
    applying.abort();
    assert!(applying.await.unwrap_err().is_cancelled());
    {
        let owner = f.owner.lock().unwrap();
        assert!(matches!(
            owner.loading_application(),
            Some(LoadingApplication::Failed(_, _))
        ));
        assert_eq!(owner.loading_configuration().unwrap().revision, 1);
        assert!(owner.loading_instance_binding("include:slow").is_none());
    }
    assert!(f.host.snapshot("slow", "include:slow", 1).await.is_err());
    let _ = f.host.stop().await;
}

#[tokio::test]
async fn host_loss_between_control_calls_revokes_instances_without_changing_desired_configuration()
{
    let f = Runtime::new(&[(
        "business",
        r#"
        import { watch } from 'node:fs';
        export function apply(ctx, config) {
            const watcher = watch(config.trigger, () => process.exit(19));
            ctx.effect(() => () => watcher.close());
        }
    "#,
    )])
    .await;
    let trigger = f.temp.path().join("crash-trigger");
    fs::write(&trigger, "waiting").unwrap();
    f.accept(&json!([{"id":"business","name":"business","config":{"trigger":trigger}}]))
        .await;
    f.runtime.apply_current().await.unwrap();
    let original = f.binding("include:business");
    let configuration = f
        .owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .clone();
    fs::write(&trigger, "crash now").unwrap();
    f.wait_host_invalidated().await;
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:business")
            .is_none()
    );
    assert_eq!(
        f.owner.lock().unwrap().loading_configuration(),
        Some(&configuration)
    );
    let _ = f.host.stop().await;

    let Runtime {
        temp,
        owner,
        runtime,
        host,
        ..
    } = f;
    let state = owner.lock().unwrap().root().to_path_buf();
    drop(runtime);
    drop(owner);
    drop(host);
    let mut reopened = PluginManager::open(&state).unwrap();
    assert_eq!(reopened.loading_configuration(), Some(&configuration));
    assert!(reopened.loading_application().is_none());
    let generation = reopened.allocate_loading_host_generation().unwrap();
    let token = reopened.begin_loading_application(generation).unwrap();
    let replacement = reopened.loading_activations(&token).unwrap().remove(0);
    assert!(replacement.host_generation > original.host_generation);
    assert!(replacement.activation_generation > original.activation_generation);
    assert_eq!(replacement.activation_order, original.activation_order);
    reopened.loading_host_ended(
        original.host_generation,
        "late old host notification".into(),
    );
    assert!(reopened.loading_instance_current(&replacement));
    drop(reopened);
    drop(temp);
}

#[tokio::test]
async fn structural_carrier_forced_updates_preserve_active_leases_and_reactivate_group_descendants()
{
    let mut f = Runtime::new(&[
        (
            "observer",
            r#"
            import { appendFileSync } from 'node:fs';
            export function apply(ctx, config) {
                ctx.on('loader/partial-dispose', entry => appendFileSync(config.receipt, `${entry.id}\n`), { global: true });
            }
        "#,
        ),
        (
            "route",
            r#"
            export function apply(ctx) {
                ctx.inject(['webServer'], ctx => {
                    ctx.webServer.register({ kind: 'exact', path: '/carrier-ok', handler: (_, res) => res.end('ok') });
                });
            }
        "#,
        ),
        (
            "worker",
            r#"
            import { appendFileSync } from 'node:fs';
            export function apply(_, config) {
                appendFileSync(config.starts, `${config.id}\n`);
            }
        "#,
        ),
    ])
    .await;
    let routes = f
        .runtime
        .open_web_routes("http://127.0.0.1:43199")
        .await
        .unwrap();
    let refresh_receipt = f.temp.path().join("carrier-refresh.log");
    let starts_receipt = f.temp.path().join("carrier-starts.log");
    fs::write(&refresh_receipt, "").unwrap();
    fs::write(&starts_receipt, "").unwrap();
    let source_root = f.owner.lock().unwrap().loading_source_root();
    fs::write(
        source_root.join("sub.yml"),
        json!([
            {"id":"sub-worker","name":"worker","config":{"id":"sub","starts":starts_receipt}}
        ])
        .to_string(),
    )
    .unwrap();
    let document = json!([
        {"id":"observer","name":"observer","config":{"receipt":refresh_receipt}},
        {"id":"group","name":"cordis:group","group":true,"config":[
            {"id":"route","name":"route"},
            {"id":"worker","name":"worker","config":{"id":"outer","starts":starts_receipt}},
            {"id":"inner","name":"cordis:group","group":true,"config":[
                {"id":"inner-worker","name":"worker","config":{"id":"inner","starts":starts_receipt}}
            ]},
            {"id":"subfile","name":"cordis:include","config":{"path":"sub.yml"}},
        ]},
    ]);
    fs::write(source_root.join("cordis.patch.yml"), "[]\n").unwrap();
    fs::write(source_root.join("cordis.yml"), document.to_string()).unwrap();
    let reconciled = f
        .runtime
        .reconcile_sources(0, &f.preparation())
        .await
        .unwrap();
    let revision = reconciled.revision;
    let applied = reconciled.application.unwrap();
    f.observe_pending();
    let observer = &applied["instances"][0];
    let route = f.binding("include:route");
    let worker = f.binding("include:worker");
    let inner_worker = f.binding("include:inner-worker");
    let sub_worker = f.binding("include:subfile:sub-worker");
    let page = routes.bind_page(&route).unwrap();
    let lease = routes.admit(&page).unwrap();
    let caller = json!({
        "hostGeneration": observer["hostGeneration"],
        "pluginId": observer["pluginId"],
        "instanceId": observer["instanceId"],
        "version": observer["version"],
        "activationGeneration": observer["activationGeneration"],
        "configurationEpoch": observer["configurationEpoch"],
    });

    // Stop worker, inner-worker, and sub-worker while leaving route active.
    f.services
        .request(
            "loading.instances.change".into(),
            json!({
                "caller": caller,
                "expectedRevision": revision,
                "action": "stop",
                "targets": [
                    {"instanceId":"include:worker","activationGeneration":worker.activation_generation},
                    {"instanceId":"include:inner-worker","activationGeneration":inner_worker.activation_generation},
                    {"instanceId":"include:subfile:sub-worker","activationGeneration":sub_worker.activation_generation},
                ],
            }),
        )
        .await
        .unwrap();
    f.observe_pending();
    lease.assert_current().unwrap();

    // Include carrier forced update requires empty executable target sets and does not restart sub-worker.
    assert!(
        f.services
            .request(
                "loading.instances.change".into(),
                json!({
                    "caller": caller,
                    "expectedRevision": revision,
                    "action": "refresh",
                    "entryId": "include:subfile",
                    "targets": [{"instanceId":"include:route","activationGeneration":route.activation_generation}],
                }),
            )
            .await
            .is_err()
    );
    fs::write(&refresh_receipt, "").unwrap();
    fs::write(&starts_receipt, "").unwrap();
    assert_eq!(
        f.services
            .request(
                "loading.instances.change".into(),
                json!({
                    "caller": caller,
                    "expectedRevision": revision,
                    "action": "refresh",
                    "entryId": "include:subfile",
                    "targets": [],
                }),
            )
            .await
            .unwrap(),
        json!({"revision": revision})
    );
    assert_eq!(
        fs::read_to_string(&refresh_receipt).unwrap(),
        "include:subfile\n"
    );
    assert_eq!(fs::read_to_string(&starts_receipt).unwrap(), "");
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:subfile:sub-worker")
            .is_none()
    );

    // Incomplete or misclassified group target sets are rejected before touching the host.
    assert!(
        f.services
            .request(
                "loading.instances.change".into(),
                json!({
                    "caller": caller,
                    "expectedRevision": revision,
                    "action": "refresh",
                    "entryId": "include:group",
                    "targets": [{"instanceId":"include:route","activationGeneration":route.activation_generation}],
                    "activateTargets": [{"instanceId":"include:worker","activationGeneration":worker.activation_generation}],
                }),
            )
            .await
            .is_err()
    );

    // Complete group forced update keeps route's generation & lease and reactivates stopped group descendants.
    fs::write(&refresh_receipt, "").unwrap();
    assert_eq!(
        f.services
            .request(
                "loading.instances.change".into(),
                json!({
                    "caller": caller,
                    "expectedRevision": revision,
                    "action": "refresh",
                    "entryId": "include:group",
                    "targets": [{"instanceId":"include:route","activationGeneration":route.activation_generation}],
                    "activateTargets": [
                        {"instanceId":"include:worker","activationGeneration":worker.activation_generation},
                        {"instanceId":"include:inner-worker","activationGeneration":inner_worker.activation_generation},
                    ],
                }),
            )
            .await
            .unwrap(),
        json!({"revision": revision})
    );
    f.observe_pending();
    lease.assert_current().unwrap();
    assert_eq!(f.binding("include:route"), route);
    let restarted_worker = f.binding("include:worker");
    let restarted_inner = f.binding("include:inner-worker");
    assert!(restarted_worker.activation_generation > worker.activation_generation);
    assert_eq!(restarted_worker.activation_order, worker.activation_order);
    assert!(restarted_inner.activation_generation > inner_worker.activation_generation);
    assert_eq!(
        restarted_inner.activation_order,
        inner_worker.activation_order
    );
    assert!(
        f.owner
            .lock()
            .unwrap()
            .loading_instance_binding("include:subfile:sub-worker")
            .is_none()
    );
    let started = fs::read_to_string(&starts_receipt).unwrap();
    assert!(
        started.contains("outer\n") && started.contains("inner\n") && !started.contains("sub\n")
    );
    f.host.stop().await.unwrap();
}
