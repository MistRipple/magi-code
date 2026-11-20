//! Real shared-host source preparation, admission and dependency waiting.
//! These tests do not claim that upstream business tasks passed.
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::{PackageInspection, SharedHost, SharedLaunchConfig},
};
use magi_plugin_system::{
    LoadingPlan, PluginManager,
    loading_runtime::{LoadingPreparation, LoadingRuntime, LoadingSourceObservation},
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

struct NoBusinessServices;
impl Incoming for NoBusinessServices {
    fn request(&self, _: String, _: Value) -> BoxFuture<Result<Value, RpcError>> {
        Box::pin(async {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                "No business port in configuration preparation",
            ))
        })
    }
    fn notification(&self, _: String, _: Value) {}
}

struct MgrSource {
    _temp: tempfile::TempDir,
    _group: ManagedProcessGroup,
    manager: PluginManager,
    host: Arc<SharedHost>,
    package: PathBuf,
    bundle: Vec<u8>,
    manifest: Vec<u8>,
    project: PathBuf,
}

async fn wait_for_source_revision(
    observations: &mut tokio::sync::watch::Receiver<LoadingSourceObservation>,
    revision: u64,
) {
    tokio::time::timeout(Duration::from_secs(10), observations.wait_for(|observation|
        matches!(observation, LoadingSourceObservation::Reconciled { revision: current, .. } if *current == revision)
    )).await.expect("filesystem notification and original graph intake").unwrap();
}

impl MgrSource {
    fn preparation(&self) -> LoadingPreparation {
        LoadingPreparation {
            project_directory: self.project.clone(),
            base: "cordis.yml".into(),
            overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
            packages: vec![PackageInspection {
                project_directory: self.project.clone(),
                directory: self.package.clone(),
                name: "dsh-plugin-mgr".into(),
                version: "0.2.13".into(),
                manifest_sha256: digest(&self.manifest),
            }],
        }
    }
}

async fn mgr_source() -> MgrSource {
    let project = fs::canonicalize(
        std::env::var_os("MAGI_LOADING_PREPARED_PROJECT").expect("set the fixed prepared project"),
    )
    .unwrap();
    let package = project.join("node_modules/dsh-plugin-mgr");
    let bundle = fs::read(package.join("cordis.patch.yml")).unwrap();
    let manifest = fs::read(package.join("package.json")).unwrap();
    let identity: Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(identity["name"], "dsh-plugin-mgr");
    assert_eq!(identity["version"], "0.2.13");
    let baseline: Value = serde_json::from_slice(
        &fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../docs/plugin-validation/phase-1/loading-preparation-2026-11-17/result.json",
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json!(digest(&manifest)), baseline["manifestSha256"]);
    assert_eq!(json!(digest(&bundle)), baseline["bundleSha256"]);
    let temp = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(temp.path()).unwrap();
    fs::create_dir_all(manager.loading_source_root()).unwrap();
    fs::write(manager.loading_source_root().join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        manager.loading_source_root().join("bundle.patch.yml"),
        &bundle,
    )
    .unwrap();
    fs::write(
        manager.loading_source_root().join("cordis.patch.yml"),
        "[]\n",
    )
    .unwrap();
    let group = ManagedProcessGroup::new();
    let host = Arc::new(launch(&mut manager, project.join("runtime/entry.mjs"), &group).await);
    MgrSource {
        _temp: temp,
        _group: group,
        manager,
        host,
        package,
        bundle,
        manifest,
        project,
    }
}

#[tokio::test]
#[ignore = "Requires the fixed original mgr-0.2.13 project with the current staged runtime"]
async fn original_mgr_application_waits_for_real_services_without_claiming_business_success() {
    let MgrSource {
        _temp,
        _group,
        mut manager,
        host,
        package,
        bundle,
        manifest,
        project,
    } = mgr_source().await;
    let candidate = manager
        .prepare_loading_configuration(
            0,
            &[
                "cordis.yml".into(),
                "bundle.patch.yml".into(),
                "cordis.patch.yml".into(),
            ],
        )
        .unwrap();
    let plan = host.prepare_configuration(json!({
        "projectDirectory":project,"sourceDirectory":candidate.source_root(),"sources":candidate.sources(),
        "base":"cordis.yml","overlays":["bundle.patch.yml","cordis.patch.yml"],
        "packages":[{"projectDirectory":project,"directory":package,"name":"dsh-plugin-mgr","version":"0.2.13","manifestSha256":digest(&manifest)}],
    })).await.unwrap();
    let configuration = manager
        .accept_loading_configuration(candidate, serde_json::from_value(plan).unwrap())
        .unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let runtime = LoadingRuntime::new(manager.clone(), host.clone());
    let receipt = runtime.apply_current().await.unwrap();
    assert_eq!(receipt["instances"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["instances"][0]["pluginId"], "dsh-plugin-mgr");
    assert_eq!(receipt["instances"][0]["state"], "waiting");
    assert_eq!(
        receipt["instances"][0]["missingServices"],
        json!(["webServer"])
    );
    let activation = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    let routes = host
        .open_web_routes("http://127.0.0.1:38123")
        .await
        .unwrap();
    let repeated = runtime.apply_current().await.unwrap();
    assert_eq!(
        repeated["instances"][0]["activationGeneration"],
        activation.activation_generation
    );
    assert_eq!(repeated["instances"][0]["state"], "running");
    assert_eq!(repeated["instances"][0]["missingServices"], json!([]));
    assert_eq!(fs::read(package.join("cordis.patch.yml")).unwrap(), bundle);
    assert_eq!(
        manager.lock().unwrap().loading_configuration(),
        Some(&configuration)
    );
    host.stop().await.unwrap();
    routes.closed().await;
    println!(
        "{}",
        json!({"package":"dsh-plugin-mgr@0.2.13", "scope":"committed-graph-execution",
        "bundleSha256":digest(&bundle),"manifestSha256":digest(&manifest),
        "state":"running","missingServices":[],
        "originalBundleUnchanged":true,"business":"not-run","beforeWebRoutes":receipt,"afterWebRoutes":repeated})
    );
}

#[tokio::test]
#[ignore = "Requires both fixed original Mgr projects with one physical SDK installation"]
async fn original_mgr_installation_replacement_survives_existing_source_monitor_and_edits() {
    let source = mgr_source().await;
    let successor = source.preparation();
    let previous_project = fs::canonicalize(
        std::env::var_os("MAGI_LOADING_PREVIOUS_PROJECT").expect("set the fixed 0.2.12 project"),
    )
    .unwrap();
    let previous_package = previous_project.join("node_modules/dsh-plugin-mgr");
    let previous_manifest = fs::read(previous_package.join("package.json")).unwrap();
    assert_eq!(
        digest(&previous_manifest),
        "9e4143f1ff342182a010d9c37a2495f046f30a60365e91d4c99fc633442b56da"
    );
    assert_eq!(
        fs::read(previous_package.join("cordis.patch.yml")).unwrap(),
        source.bundle
    );
    let initial = LoadingPreparation {
        packages: vec![PackageInspection {
            project_directory: previous_project.clone(),
            directory: previous_package.clone(),
            name: "dsh-plugin-mgr".into(),
            version: "0.2.12".into(),
            manifest_sha256: digest(&previous_manifest),
        }],
        project_directory: previous_project,
        ..successor.clone()
    };
    let sources = source.manager.loading_source_root();
    let state_root = source.manager.root().to_path_buf();
    let manager = Arc::new(Mutex::new(source.manager));
    let runtime = LoadingRuntime::new(manager.clone(), source.host.clone());
    let monitor = runtime.monitor_sources(initial.clone()).unwrap();
    let mut observations = monitor.observations();
    let binding = || {
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .unwrap()
            .clone()
    };
    wait_for_source_revision(&mut observations, 1).await;
    let old = binding();
    assert_eq!(old.executable.version, "0.2.12");

    let mut invalid = successor.clone();
    invalid.packages[0].manifest_sha256 = initial.packages[0].manifest_sha256.clone();
    assert!(runtime.reconcile_sources(1, &invalid).await.is_err());
    assert_eq!(binding(), old);
    let accepted = runtime.reconcile_sources(1, &successor).await.unwrap();
    assert_eq!(accepted.revision, 2);
    wait_for_source_revision(&mut observations, 2).await;
    let replacement = binding();
    assert_eq!(replacement.executable.version, "0.2.13");
    assert_eq!(replacement.activation_order, old.activation_order);
    assert!(replacement.activation_generation > old.activation_generation);

    fs::write(
        sources.join("cordis.patch.yml"),
        "- id: dsh-plugin-mgr\n  disabled: true\n",
    )
    .unwrap();
    wait_for_source_revision(&mut observations, 3).await;
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .is_none()
    );
    let enabled = runtime
        .edit_sources(
            3,
            json!({"kind":"update","treeId":"include",
        "id":"dsh-plugin-mgr","options":{"disabled":false}}),
        )
        .await
        .unwrap();
    assert_eq!(enabled.accepted_revision, 4);
    wait_for_source_revision(&mut observations, 4).await;
    assert_eq!(
        fs::read(sources.join("bundle.patch.yml")).unwrap(),
        source.bundle
    );
    assert!(
        fs::read_to_string(sources.join("cordis.patch.yml"))
            .unwrap()
            .contains("disabled: false")
    );
    let current = binding();
    assert_eq!(current.executable.version, "0.2.13");
    assert!(current.activation_generation > replacement.activation_generation);
    let status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            current.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "waiting");
    monitor.stop().await.unwrap();
    source.host.stop().await.unwrap();
    let reopened = PluginManager::open(state_root).unwrap();
    let configuration = reopened.loading_configuration().unwrap();
    assert_eq!(configuration.revision, 4);
    assert_eq!(configuration.preparation, successor);
    assert!(reopened.loading_application().is_none());
    assert_eq!(
        fs::read(previous_package.join("package.json")).unwrap(),
        previous_manifest
    );
    assert_eq!(
        fs::read(source.package.join("package.json")).unwrap(),
        source.manifest
    );
    println!(
        "{}",
        json!({"versions":["0.2.12","0.2.13"],"revisions":[1,2,3,4],
        "invalidManifestRejected":true,"activationOrderPreserved":true,"reopenedVersion":configuration.preparation.packages[0].version,
        "finalStatus":status,"scope":"immutable-installation-input-replacement","business":"not-run"})
    );
}

fn digest(bytes: &[u8]) -> String {
    magi_core::hex_lower(Sha256::digest(bytes))
}

#[tokio::test]
#[ignore = "Requires the fixed original mgr-0.2.13 project with the current staged runtime"]
async fn original_mgr_source_edits_follow_upstream_ownership_and_the_existing_configuration_commit()
{
    let source = mgr_source().await;
    let preparation = source.preparation();
    let manager = Arc::new(Mutex::new(source.manager));
    let runtime = LoadingRuntime::new(manager.clone(), source.host.clone());
    let initial = runtime.reconcile_sources(0, &preparation).await.unwrap();
    assert_eq!(
        initial.application.as_ref().unwrap()["instances"][0]["state"],
        "waiting"
    );
    let original = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    let removed = runtime.edit_sources(1,
        json!({"kind":"update","treeId":"include","id":"dsh-plugin-mgr","options":{"disabled":true}})).await.unwrap();
    assert_eq!(removed.reconciliation.revision, 2);
    assert_eq!(
        removed.reconciliation.application.as_ref().unwrap()["instances"],
        json!([])
    );
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .is_none()
    );
    assert!(
        source
            .host
            .snapshot(
                "dsh-plugin-mgr",
                "include:dsh-plugin-mgr",
                original.activation_generation
            )
            .await
            .is_err()
    );
    let root = manager.lock().unwrap().loading_source_root();
    assert_eq!(fs::read_to_string(root.join("cordis.yml")).unwrap(), "[]\n");
    assert_eq!(
        fs::read_to_string(root.join("cordis.patch.yml")).unwrap(),
        "[]\n"
    );
    assert!(
        fs::read_to_string(root.join("bundle.patch.yml"))
            .unwrap()
            .contains("disabled: true")
    );
    let restarted = runtime.edit_sources(2,
        json!({"kind":"update","treeId":"include","id":"dsh-plugin-mgr","options":{"disabled":false}})).await.unwrap();
    assert_eq!(restarted.reconciliation.revision, 3);
    let final_status = &restarted.reconciliation.application.as_ref().unwrap()["instances"][0];
    assert_eq!(final_status["state"], "waiting");
    assert_eq!(final_status["missingServices"], json!(["webServer"]));
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .unwrap()
            .activation_generation
            > original.activation_generation
    );
    assert!(manager.lock().unwrap().pending_loading_edit().is_none());
    assert_eq!(
        fs::read(source.package.join("cordis.patch.yml")).unwrap(),
        source.bundle
    );
    source.host.stop().await.unwrap();
    println!(
        "{}",
        json!({"package":"dsh-plugin-mgr@0.2.13","scope":"source-edit-journal-and-domain-commit",
        "bundleSha256":digest(&source.bundle),"manifestSha256":digest(&source.manifest),
        "acceptedRevisions":[1,2,3],"owningCandidateSource":"bundle.patch.yml", "unrelatedSourcesUnchanged":true,
        "originalPackageBundleUnchanged":true,"pendingEditCleared":true,"business":"not-run","finalStatus":final_status})
    );
}

#[tokio::test]
#[ignore = "Requires the fixed original mgr-0.2.13 project with the current staged runtime"]
async fn original_mgr_file_changes_are_admitted_by_real_notifications_and_the_shared_owner() {
    let source = mgr_source().await;
    let preparation = source.preparation();
    let manager = Arc::new(Mutex::new(source.manager));
    let runtime = LoadingRuntime::new(manager.clone(), source.host.clone());
    let monitor = runtime.monitor_sources(preparation).unwrap();
    let mut observations = monitor.observations();
    wait_for_source_revision(&mut observations, 1).await;
    let original = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    let first = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            original.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(first["state"], "waiting");
    assert_eq!(first["missingServices"], json!(["webServer"]));
    let patch = manager
        .lock()
        .unwrap()
        .loading_source_root()
        .join("cordis.patch.yml");
    let helper = source.package.join("dist/internal.js");
    let node = std::env::var_os("MAGI_EXTENSION_NODE")
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"))
        .expect("Node required");
    async fn write_with_original_mgr(node: &Path, helper: &Path, patch: &Path, disabled: bool) {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(node)
                .kill_on_drop(true)
                .args([
                    "--input-type=module",
                    "--eval",
                    r#"
                        import { pathToFileURL } from 'node:url';
                        const [helper, patch, disabled] = process.argv.slice(1);
                        const { _internal } = await import(pathToFileURL(helper).href);
                        const write = disabled === 'true' ? _internal.disableRows : _internal.enableRows;
                        console.log(JSON.stringify(await write(patch, ['dsh-plugin-mgr'])));
                    "#,
                    "--",
                ])
                .arg(helper)
                .arg(patch)
                .arg(disabled.to_string())
                .output(),
        )
        .await
        .expect("original Mgr source edit must settle")
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            json!({"ok":true,"reason":null})
        );
    }
    write_with_original_mgr(&node, &helper, &patch, true).await;
    wait_for_source_revision(&mut observations, 2).await;
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .is_none()
    );
    assert!(
        source
            .host
            .snapshot(
                "dsh-plugin-mgr",
                "include:dsh-plugin-mgr",
                original.activation_generation
            )
            .await
            .is_err()
    );
    write_with_original_mgr(&node, &helper, &patch, false).await;
    wait_for_source_revision(&mut observations, 3).await;
    let replacement = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert!(replacement.activation_generation > original.activation_generation);
    let final_status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            replacement.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(final_status["state"], "waiting");
    assert_eq!(final_status["missingServices"], json!(["webServer"]));
    assert_eq!(
        fs::read(source.package.join("cordis.patch.yml")).unwrap(),
        source.bundle
    );
    monitor.stop().await.unwrap();
    source.host.stop().await.unwrap();
    println!(
        "{}",
        json!({"package":"dsh-plugin-mgr@0.2.13", "scope":"source-notification-intake",
        "bundleSha256":digest(&source.bundle),"manifestSha256":digest(&source.manifest),
        "acceptedRevisions":[1,2,3],"states":["waiting","removed","waiting"],
        "activationGenerations":[original.activation_generation,replacement.activation_generation],
        "candidateWriter":"original dist/internal.js disableRows and enableRows",
        "originalBundleUnchanged":true,"business":"not-run","finalStatus":final_status})
    );
}

#[tokio::test]
#[ignore = "Requires the fixed original mgr-0.2.13 project with the current staged runtime"]
async fn original_mgr_group_changes_use_one_accepted_plan_and_keep_the_original_package_unchanged()
{
    let source = mgr_source().await;
    let preparation = source.preparation();
    let manager = Arc::new(Mutex::new(source.manager));
    let runtime = LoadingRuntime::new(manager.clone(), source.host.clone());
    runtime.reconcile_sources(0, &preparation).await.unwrap();
    let original = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    let row = manager
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .graph["root"]["entries"][0]["children"][0]["options"]
        .clone();
    assert_eq!(row["name"], "dsh-plugin-mgr");
    let marker = json!({"id":"marker","name":"cordis:group","group":true,"config":[]});
    let mut disabled = row.clone();
    disabled["disabled"] = json!(true);
    let stopped = runtime
        .edit_sources(
            1,
            json!({"kind":"reconcile","treeId":"include",
        "entries":[marker,disabled]}),
        )
        .await
        .unwrap();
    assert_eq!(stopped.accepted_revision, 2);
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .is_none()
    );
    let restored = runtime
        .edit_sources(
            2,
            json!({"kind":"reconcile","treeId":"include",
        "entries":[row,marker]}),
        )
        .await
        .unwrap();
    assert_eq!(restored.accepted_revision, 3);
    let current = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert!(current.activation_generation > original.activation_generation);
    let status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            current.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["missingServices"], json!(["webServer"]));
    assert!(
        source
            .host
            .snapshot(
                "dsh-plugin-mgr",
                "include:dsh-plugin-mgr",
                original.activation_generation
            )
            .await
            .is_err()
    );
    let unchanged = runtime
        .edit_sources(
            3,
            json!({"kind":"reconcile","treeId":"include",
        "entries":[row,marker]}),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.accepted_revision, 3);
    assert!(unchanged.reconciliation.application.is_none());
    let regrouped = runtime
        .edit_sources(
            3,
            json!({"kind":"reconcile","treeId":"include","entries":[
                {"id":"container","name":"cordis:group","group":true,"config":[row]},marker]}),
        )
        .await
        .unwrap();
    assert_eq!(regrouped.accepted_revision, 4);
    let grouped = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert!(grouped.activation_generation > current.activation_generation);
    let status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            grouped.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["missingServices"], json!(["webServer"]));
    let ungrouped = runtime
        .edit_sources(
            4,
            json!({"kind":"reconcile","treeId":"include","entries":[row,marker]}),
        )
        .await
        .unwrap();
    assert_eq!(ungrouped.accepted_revision, 5);
    let flattened = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    assert!(flattened.activation_generation > grouped.activation_generation);
    let status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            flattened.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["missingServices"], json!(["webServer"]));
    assert!(
        source
            .host
            .snapshot(
                "dsh-plugin-mgr",
                "include:dsh-plugin-mgr",
                grouped.activation_generation
            )
            .await
            .is_err()
    );
    let root_moved = runtime
        .edit_sources(
            5,
            json!({"kind":"update","treeId":"include","id":"dsh-plugin-mgr","options":{},"targetTreeId":""}),
        )
        .await
        .unwrap();
    assert_eq!(root_moved.accepted_revision, 6);
    assert_eq!(root_moved.result, "dsh-plugin-mgr");
    let root_binding = manager
        .lock()
        .unwrap()
        .loading_instance_binding("dsh-plugin-mgr")
        .unwrap()
        .clone();
    let root_status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "dsh-plugin-mgr",
            root_binding.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(root_status["state"], "waiting");
    assert_eq!(root_status["missingServices"], json!(["webServer"]));
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("include:dsh-plugin-mgr")
            .is_none()
    );
    let returned = runtime
        .edit_sources(
            6,
            json!({"kind":"update","treeId":"","id":"dsh-plugin-mgr","options":{},"targetTreeId":"include"}),
        )
        .await
        .unwrap();
    assert_eq!(returned.accepted_revision, 7);
    assert_eq!(returned.result, "include:dsh-plugin-mgr");
    assert!(
        manager
            .lock()
            .unwrap()
            .loading_instance_binding("dsh-plugin-mgr")
            .is_none()
    );
    let last = manager
        .lock()
        .unwrap()
        .loading_instance_binding("include:dsh-plugin-mgr")
        .unwrap()
        .clone();
    let status = source
        .host
        .snapshot(
            "dsh-plugin-mgr",
            "include:dsh-plugin-mgr",
            last.activation_generation,
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["missingServices"], json!(["webServer"]));
    let removed = runtime
        .edit_sources(
            7,
            json!({"kind":"reconcile","treeId":"include","entries":[]}),
        )
        .await
        .unwrap();
    assert_eq!(removed.accepted_revision, 8);
    assert_eq!(
        removed.reconciliation.application.unwrap()["instances"],
        json!([])
    );
    assert!(manager.lock().unwrap().pending_loading_edit().is_none());
    assert_eq!(
        fs::read(source.package.join("cordis.patch.yml")).unwrap(),
        source.bundle
    );
    assert_eq!(
        fs::read(source.package.join("package.json")).unwrap(),
        source.manifest
    );
    source.host.stop().await.unwrap();
    println!(
        "{}",
        json!({"package":"dsh-plugin-mgr@0.2.13","scope":"atomic-group-and-memory-root-edit",
        "acceptedRevisions":[1,2,3,4,5,6,7,8],"states":["waiting","removed","waiting","waiting-in-new-group","waiting-at-source-root","waiting-at-memory-root","waiting-at-source-root","removed"],
        "lastLiveStatus":status,"originalPackageUnchanged":true,"unchangedEditDoesNotApply":true,
        "pendingEditCleared":true,"business":"not-run"})
    );
}

async fn launch(
    manager: &mut PluginManager,
    runtime_entry: PathBuf,
    group: &ManagedProcessGroup,
) -> SharedHost {
    SharedHost::launch(
        group,
        SharedLaunchConfig {
            node: std::env::var_os("MAGI_EXTENSION_NODE")
                .map(PathBuf::from)
                .or_else(|| magi_process::resolve_executable("node"))
                .expect("Node required"),
            runtime_entry,
            endpoint_dir: manager.root().join("socket"),
            host_generation: manager.allocate_loading_host_generation().unwrap(),
            incoming: Arc::new(NoBusinessServices),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn source_bytes_are_prepared_in_a_real_host_then_committed_once_by_the_manager() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(temp.path().join("state")).unwrap();
    fs::create_dir_all(manager.loading_source_root()).unwrap();
    fs::write(
        manager.loading_source_root().join("cordis.yml"),
        "- id: business\n  name: business\n",
    )
    .unwrap();
    let project = fs::canonicalize(temp.path())
        .unwrap()
        .join("immutable-project");
    let package = project.join("node_modules/business");
    fs::create_dir_all(&package).unwrap();
    fs::write(project.join("package.json"), "{\"private\":true}").unwrap();
    let manifest = br#"{"name":"business","version":"1.0.0","main":"index.mjs"}"#;
    fs::write(package.join("package.json"), manifest).unwrap();
    fs::write(
        package.join("index.mjs"),
        "throw new Error('must not execute during preparation');",
    )
    .unwrap();
    let group = ManagedProcessGroup::new();
    let runtime =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host/runtime/dsh/entry.mjs");
    let host = launch(&mut manager, runtime, &group).await;
    let captured = manager
        .prepare_loading_configuration(0, &["cordis.yml".into()])
        .unwrap();
    let prepared = host.prepare_configuration(json!({
        "projectDirectory": project, "sourceDirectory": captured.source_root(),
        "sources": captured.sources(), "base": "cordis.yml",
        "packages": [{"projectDirectory": project, "directory":package, "name":"business", "version":"1.0.0", "manifestSha256":digest(manifest)}],
    })).await.unwrap();
    assert_eq!(prepared["sourcesSha256"], captured.sources_sha256());
    let plan: LoadingPlan = serde_json::from_value(prepared).unwrap();
    let committed = manager
        .accept_loading_configuration(captured, plan)
        .unwrap();
    assert_eq!(committed.revision, 1);
    assert_eq!(
        committed.graph["artifacts"]["business"]["entry"],
        json!(package.join("index.mjs"))
    );
    assert!(manager.loading_application().is_none());
    host.stop().await.unwrap();
    drop(manager);
    let manager = PluginManager::open(temp.path().join("state")).unwrap();
    assert_eq!(manager.loading_configuration(), Some(&committed));
    assert!(manager.loading_application().is_none());
}

#[tokio::test]
#[ignore = "Requires the fixed original mgr-0.2.13 preparation project and its staged runtime"]
async fn fixed_mgr_original_bundle_produces_a_committable_configuration_without_activation() {
    let project = fs::canonicalize(
        std::env::var_os("MAGI_LOADING_PREPARED_PROJECT").expect("set the fixed prepared project"),
    )
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let identities: Value = serde_json::from_slice(
        &fs::read(root.join("target/dsh-phase-1/prepared/results.json")).unwrap(),
    )
    .unwrap();
    let prepared = identities
        .as_array()
        .unwrap()
        .iter()
        .find(|record| {
            record["identity"]["name"] == "dsh-plugin-mgr"
                && record["identity"]["version"] == "0.2.13"
        })
        .unwrap();
    let package = project.join("node_modules/dsh-plugin-mgr");
    let bundle = fs::read(package.join("cordis.patch.yml")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(temp.path()).unwrap();
    fs::create_dir_all(manager.loading_source_root()).unwrap();
    fs::write(manager.loading_source_root().join("cordis.yml"), "[]\n").unwrap();
    fs::write(
        manager.loading_source_root().join("bundle.patch.yml"),
        &bundle,
    )
    .unwrap();
    fs::write(
        manager.loading_source_root().join("cordis.patch.yml"),
        "[]\n",
    )
    .unwrap();
    let group = ManagedProcessGroup::new();
    let host = launch(&mut manager, project.join("runtime/entry.mjs"), &group).await;
    let names = [
        "cordis.yml".into(),
        "bundle.patch.yml".into(),
        "cordis.patch.yml".into(),
    ];
    let captured = manager.prepare_loading_configuration(0, &names).unwrap();
    let input = json!({
        "projectDirectory": project, "sourceDirectory": captured.source_root(),
        "sources": captured.sources(), "base": "cordis.yml", "overlays": ["bundle.patch.yml", "cordis.patch.yml"],
        "packages": [{"projectDirectory":project,"directory":package,"name":"dsh-plugin-mgr","version":"0.2.13","manifestSha256":prepared["manifestSha256"]}],
    });
    let result = host.prepare_configuration(input).await.unwrap();
    let plan: LoadingPlan = serde_json::from_value(result).unwrap();
    let first = manager
        .accept_loading_configuration(captured, plan)
        .unwrap();
    assert_eq!(
        first.graph["root"]["entries"][0]["children"][0]["options"]["name"],
        "dsh-plugin-mgr"
    );
    assert_eq!(
        first.graph["artifacts"]["dsh-plugin-mgr"]["version"],
        "0.2.13"
    );
    assert!(manager.loading_application().is_none());
    fs::write(
        manager.loading_source_root().join("cordis.patch.yml"),
        "- id: dsh-plugin-mgr\n  disabled: true\n",
    )
    .unwrap();
    let captured = manager.prepare_loading_configuration(1, &names).unwrap();
    let result = host.prepare_configuration(json!({
        "projectDirectory":project,"sourceDirectory":captured.source_root(),"sources":captured.sources(),
        "base":"cordis.yml","overlays":["bundle.patch.yml","cordis.patch.yml"],"previous":first.graph,
    })).await.unwrap();
    let stopped = manager
        .accept_loading_configuration(captured, serde_json::from_value(result).unwrap())
        .unwrap();
    assert_eq!(stopped.revision, 2);
    assert_eq!(
        stopped.graph["root"]["entries"][0]["children"][0]["options"]["disabled"],
        true
    );
    assert!(
        stopped.graph["root"]["entries"][0]["children"][0]
            .get("artifact")
            .is_none()
    );
    assert_eq!(fs::read(package.join("cordis.patch.yml")).unwrap(), bundle);
    println!(
        "{}",
        json!({"package":"dsh-plugin-mgr@0.2.13","scope":"configuration-preparation-and-domain-commit","bundleSha256":digest(&bundle),"archiveSha256":prepared["identity"]["archiveSha256"],"acceptedRevisions":[first.revision,stopped.revision],"activation":"not-run","business":"not-run"})
    );
    host.stop().await.unwrap();
}
