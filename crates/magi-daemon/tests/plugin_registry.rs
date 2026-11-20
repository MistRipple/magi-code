//! Online archive acquisition and native loading of the two fixed Mgr releases.
//! The test uses a preinstalled shared SDK and resolves ordinary dependencies online.

use magi_api::fetch_npm_archive;
use magi_daemon::{
    plugin_dependencies::{PluginDependencyToolchain, prepare_plugin_dependencies},
    plugin_runtime::{PluginRuntime, PluginRuntimeConfig},
};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
};
use magi_plugin_system::{
    LoadingSourceEdit, PluginManager,
    loading_runtime::{LoadingPreparation, LoadingSourceObservation},
    npm::NativePackageIdentity,
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
use tokio::{io::AsyncReadExt, net::TcpListener, sync::mpsc};

struct CoreOnly(mpsc::Sender<Value>);

impl Incoming for CoreOnly {
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
            let _ = self.0.try_send(params);
        }
    }
}

async fn mgr_running(statuses: &mut mpsc::Receiver<Value>, version: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = statuses.recv().await.expect("shared host disconnected");
            if status["version"] == version && status["state"] == "running" {
                assert_eq!(status["missingServices"], json!([]));
                return status;
            }
        }
    })
    .await
    .expect("Mgr reached running state with public loader and webServer")
}

async fn stage_mgr(
    processes: &ManagedProcessGroup,
    toolchain: &PluginDependencyToolchain<'_>,
    root: &Path,
    version: &str,
) -> (LoadingPreparation, NativePackageIdentity) {
    let archive = fetch_npm_archive("dsh-plugin-mgr", version).await.unwrap();
    let project_directory = root.join(version);
    fs::create_dir(&project_directory).unwrap();
    let mut prepared =
        prepare_plugin_dependencies(processes, toolchain, &project_directory, &[archive])
            .await
            .unwrap();
    let (descriptor, identity) = prepared.pop().unwrap();
    let dependency = project_directory.join("node_modules/js-yaml");
    assert!(
        !fs::symlink_metadata(&dependency)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    for name in ["cordis", "dsh-tools"] {
        let relative = Path::new("node_modules/@deepseek-ai").join(name);
        assert_eq!(
            fs::canonicalize(project_directory.join(&relative)).unwrap(),
            fs::canonicalize(toolchain.sdk_project.join(relative)).unwrap()
        );
    }
    let lock = fs::read_to_string(project_directory.join("package-lock.json")).unwrap();
    let lock_sha256 = magi_core::hex_lower(sha2::Sha256::digest(lock.as_bytes()));
    println!(
        "{}",
        json!({"version":version,"dependencyLockSha256":lock_sha256,"dependencyLock":lock})
    );
    (
        LoadingPreparation {
            project_directory,
            base: "cordis.yml".into(),
            overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
            packages: vec![descriptor],
        },
        identity,
    )
}

async fn cancel_dependency_preparation(
    processes: &ManagedProcessGroup,
    toolchain: &PluginDependencyToolchain<'_>,
    parent: &Path,
) {
    let archive = fetch_npm_archive("dsh-plugin-mgr", "0.2.13").await.unwrap();
    let directory = tempfile::tempdir_in(parent).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let registry = format!("http://{}", listener.local_addr().unwrap());
    let delayed = PluginDependencyToolchain {
        registry: &registry,
        ..*toolchain
    };
    let archives = [archive];
    let mut stream = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::select! {
            outcome = prepare_plugin_dependencies(processes, &delayed, directory.path(), &archives) => {
                assert!(outcome.is_err(), "stalled registry must not return a prepared project");
                panic!("preparation stopped before the cancellation point: {}", outcome.err().unwrap().message());
            }
            incoming = listener.accept() => incoming.unwrap().0,
        }
    }).await.expect("npm reached the delayed registry");
    let mut request = Vec::new();
    let response =
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut request)).await;
    match response.expect("cancelled npm subprocess closed its connection") {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("unexpected cancellation socket error: {error}"),
    }
    let staging_path = directory.path().to_owned();
    directory.close().unwrap();
    assert!(!staging_path.exists());
}

#[tokio::test]
#[ignore = "Requires npm registry and the fixed, dependency-locked shared SDK project"]
async fn registry_mgr_releases_enter_the_same_native_loading_commit() {
    let shared = fs::canonicalize(
        std::env::var_os("MAGI_DSH_SHARED_SDK_PROJECT").expect("set the fixed SDK project"),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let processes = ManagedProcessGroup::new();
    let node = std::env::var_os("MAGI_EXTENSION_NODE")
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"))
        .expect("Node required");
    let npm_cli =
        PathBuf::from(std::env::var_os("MAGI_DSH_NPM_CLI").expect("set the npm CLI module"));
    let toolchain = PluginDependencyToolchain {
        node: &node,
        npm_cli: &npm_cli,
        sdk_project: &shared,
        registry: "https://registry.npmjs.org",
    };
    let (old, old_identity) = stage_mgr(&processes, &toolchain, &root, "0.2.12").await;
    let (new, new_identity) = stage_mgr(&processes, &toolchain, &root, "0.2.13").await;
    assert_eq!(
        old_identity.archive_sha256,
        "694b301bdd9c116e8fb01b1820e13932b9ad398bda5d1e6139db0de35372be9c"
    );
    assert_eq!(
        new_identity.archive_sha256,
        "d4022f212060921d40d74ed18ac2dfc3cafc5be529e297b49e125b7294b32228"
    );
    let bundle = fs::read_to_string(old.packages[0].directory.join("cordis.patch.yml")).unwrap();
    let next_bundle =
        fs::read_to_string(new.packages[0].directory.join("cordis.patch.yml")).unwrap();
    assert_eq!(next_bundle, bundle);

    let state = root.join("state");
    let manager = PluginManager::open(&state).unwrap();
    let sources = manager.loading_source_root();
    fs::create_dir_all(&sources).unwrap();
    fs::write(sources.join("cordis.yml"), "[]\n").unwrap();
    let prior_bundle = sources.join("bundle.patch.yml");
    let patch = sources.join("cordis.patch.yml");
    fs::write(&prior_bundle, &bundle).unwrap();
    fs::write(&patch, "[]\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = PluginRuntimeConfig {
        node: node.clone(),
        sdk_project: shared.clone(),
        endpoint_dir: root.join("socket"),
        public_origin: format!("http://{}", listener.local_addr().unwrap()),
        bundle_management: None,
    };
    let owner = Arc::new(Mutex::new(manager));
    let (events, mut statuses) = mpsc::channel(64);
    let runtime = PluginRuntime::launch(
        owner.clone(),
        &processes,
        config.clone(),
        old.clone(),
        Arc::new(CoreOnly(events)),
    )
    .await
    .unwrap();

    let first_instance = mgr_running(&mut statuses, "0.2.12").await;
    cancel_dependency_preparation(&processes, &toolchain, &root).await;
    assert_eq!(
        owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        1
    );
    // Transfer the effective bundle layer into the candidate editable patch
    // while replacing its immutable package project. Each half alone would
    // leave either no active instance or ambiguous duplicate entry ids.
    let edits = vec![
        LoadingSourceEdit {
            name: "bundle.patch.yml".into(),
            expected_sha256: Some(magi_core::hex_lower(sha2::Sha256::digest(
                bundle.as_bytes(),
            ))),
            content: "[]\n".into(),
        },
        LoadingSourceEdit {
            name: "cordis.patch.yml".into(),
            expected_sha256: Some(magi_core::hex_lower(sha2::Sha256::digest(b"[]\n"))),
            content: next_bundle.clone(),
        },
    ];
    let mut invalid = edits.clone();
    invalid[1].content = "[{\n".into();
    assert!(runtime.loading().reconcile_sources(1, &new).await.is_err());
    assert!(
        runtime
            .loading()
            .submit_installation(1, &new, invalid)
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&prior_bundle).unwrap(), bundle);
    assert_eq!(fs::read_to_string(&patch).unwrap(), "[]\n");
    assert_eq!(
        owner
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .preparation,
        old
    );
    assert!(owner.lock().unwrap().pending_loading_edit().is_none());
    let mut stale = edits.clone();
    stale[1].expected_sha256 = Some("0".repeat(64));
    assert!(
        runtime
            .loading()
            .submit_installation(1, &new, stale)
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&prior_bundle).unwrap(), bundle);
    assert_eq!(fs::read_to_string(&patch).unwrap(), "[]\n");
    let updated = runtime
        .loading()
        .submit_installation(1, &new, edits)
        .await
        .unwrap();
    let replacement = mgr_running(&mut statuses, "0.2.13").await;
    assert_eq!(updated, 2);
    assert_eq!(fs::read_to_string(&prior_bundle).unwrap(), "[]\n");
    assert_eq!(fs::read_to_string(&patch).unwrap(), next_bundle);
    assert!(owner.lock().unwrap().pending_loading_edit().is_none());
    assert!(
        replacement["activationGeneration"].as_u64().unwrap()
            > first_instance["activationGeneration"].as_u64().unwrap()
    );
    runtime.stop().await.unwrap();
    // A broken external edit made while the app was closed must be rejected
    // while the accepted version is still restored in a new generation.
    let invalid = "[{\n";
    fs::write(&patch, invalid).unwrap();
    let reopened = PluginManager::open(state).unwrap();
    assert_eq!(reopened.loading_configuration().unwrap().preparation, new);
    let reopened = Arc::new(Mutex::new(reopened));
    let (events, mut statuses) = mpsc::channel(64);
    let restarted = PluginRuntime::launch(
        reopened.clone(),
        &processes,
        config,
        old,
        Arc::new(CoreOnly(events)),
    )
    .await
    .unwrap();
    let recovered = mgr_running(&mut statuses, "0.2.13").await;
    assert!(recovered["hostGeneration"].as_u64() > replacement["hostGeneration"].as_u64());
    assert!(
        recovered["activationGeneration"].as_u64() > replacement["activationGeneration"].as_u64()
    );
    let mut observations = restarted.source_observations();
    let rejection = tokio::time::timeout(
        Duration::from_secs(10),
        observations.wait_for(|value| matches!(value, LoadingSourceObservation::Rejected { .. })),
    )
    .await
    .expect("invalid draft reported")
    .unwrap()
    .clone();
    assert_eq!(fs::read_to_string(&patch).unwrap(), invalid);
    assert_eq!(
        reopened
            .lock()
            .unwrap()
            .loading_configuration()
            .unwrap()
            .revision,
        2
    );
    fs::write(&patch, next_bundle).unwrap();
    let reconciled = tokio::time::timeout(
        Duration::from_secs(10),
        observations.wait_for(|value| {
            matches!(
                value,
                LoadingSourceObservation::Reconciled {
                    revision: 2,
                    application: None
                }
            )
        }),
    )
    .await
    .expect("corrected draft revalidated")
    .is_ok();
    assert!(reconciled);
    restarted.stop().await.unwrap();
    println!(
        "{}",
        json!({"first": first_instance, "replacement": replacement,
        "recovered": recovered, "invalidDraftPreserved": true, "draftRecoveryObservation": format!("{rejection:?}"),
        "oldArchive": old_identity, "newArchive": new_identity, "acceptedRevisions": [1, 2],
        "scope": "online-original-archive-and-native-loading-commit", "business": "not-run",
        "installationAndSourcesCommittedTogether": true, "invalidMixedCandidatePreservedOldRevision": true,
        "cancelledDependencyPreparationPreservedRevision": true})
    );
}
