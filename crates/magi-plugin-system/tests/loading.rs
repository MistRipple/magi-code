//! Domain transactions only. These tests do not claim Loader or upstream business acceptance.
use magi_plugin_system::loading_runtime::LoadingPreparation;
use magi_plugin_system::{LoadingApplication, LoadingCandidate, LoadingPlan, PluginManager};
use serde_json::json;
use std::{fs, path::Path};

fn source(manager: &PluginManager, content: &str) {
    fs::create_dir_all(manager.loading_source_root()).unwrap();
    fs::write(manager.loading_source_root().join("cordis.yml"), content).unwrap();
}

fn candidate(manager: &PluginManager, revision: u64) -> LoadingCandidate {
    manager
        .prepare_loading_configuration(revision, &["cordis.yml".into()])
        .unwrap()
}

fn plan(candidate: &LoadingCandidate, value: bool) -> LoadingPlan {
    // Stand-in for the compiler, whose semantic validation is outside this domain test.
    LoadingPlan {
        source_root: candidate.source_root().into(),
        source_names: candidate
            .sources()
            .iter()
            .map(|source| source.name().into())
            .collect(),
        sources_sha256: candidate.sources_sha256().into(),
        preparation: LoadingPreparation {
            project_directory: candidate.source_root().into(),
            base: "cordis.yml".into(),
            overlays: Vec::new(),
            packages: Vec::new(),
        },
        graph: json!({"enabled":value}),
        executables: Vec::new(),
    }
}

#[test]
fn configuration_commit_is_revisioned_durable_and_distinct_from_runtime_application() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    let configuration = manager
        .accept_loading_configuration(captured, compiled)
        .unwrap();
    assert_eq!(configuration.revision, 1);
    assert!(manager.loading_application().is_none());
    let generation = manager.allocate_loading_host_generation().unwrap();
    let application = manager.begin_loading_application(generation).unwrap();
    assert!(manager.begin_loading_application(generation).is_err());
    assert!(matches!(
        manager.loading_application(),
        Some(LoadingApplication::Applying(_))
    ));
    manager
        .finish_loading_application(&application, Ok(()))
        .unwrap();
    assert!(matches!(
        manager.loading_application(),
        Some(LoadingApplication::Applied(_))
    ));
    assert!(
        manager
            .finish_loading_application(&application, Ok(()))
            .is_err()
    );
    drop(manager);

    let mut reopened = PluginManager::open(directory.path()).unwrap();
    assert_eq!(reopened.loading_configuration(), Some(&configuration));
    assert!(
        reopened.loading_application().is_none(),
        "a previous process cannot be reported as applied after restart"
    );
    let replacement = reopened.allocate_loading_host_generation().unwrap();
    assert!(replacement > generation);
    assert!(reopened.begin_loading_application(generation).is_err());
    let second = reopened.begin_loading_application(replacement).unwrap();
    assert!(second.sequence > application.sequence);
    assert!(
        reopened
            .finish_loading_application(&application, Ok(()))
            .is_err()
    );
    reopened
        .finish_loading_application(&second, Err("required service unavailable".into()))
        .unwrap();
    assert!(
        matches!(reopened.loading_application(), Some(LoadingApplication::Failed(_, message)) if message == "required service unavailable")
    );
    assert_eq!(reopened.loading_configuration(), Some(&configuration));
}

#[test]
fn external_source_changes_and_other_editors_cannot_commit_a_stale_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    source(&manager, "- id: user-edit\n  disabled: true\n");
    assert!(
        manager
            .accept_loading_configuration(captured, compiled)
            .unwrap_err()
            .to_string()
            .contains("来源 cordis.yml 已变化")
    );
    assert!(manager.loading_configuration().is_none());
    assert_eq!(
        fs::read_to_string(manager.loading_source_root().join("cordis.yml")).unwrap(),
        "- id: user-edit\n  disabled: true\n"
    );

    let editor_a = candidate(&manager, 0);
    let editor_b = candidate(&manager, 0);
    let compiled_a = plan(&editor_a, true);
    let compiled_b = plan(&editor_b, false);
    manager
        .accept_loading_configuration(editor_a, compiled_a)
        .unwrap();
    assert!(
        manager
            .accept_loading_configuration(editor_b, compiled_b)
            .is_err()
    );
    assert!(
        manager
            .prepare_loading_configuration(0, &["cordis.yml".into()])
            .is_err()
    );
    assert_eq!(
        manager.loading_configuration().unwrap().graph,
        json!({"enabled":true})
    );
}

#[test]
fn repeated_notifications_do_not_advance_the_revision_or_invalidate_the_current_application() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    manager
        .accept_loading_configuration(captured, compiled)
        .unwrap();
    let generation = manager.allocate_loading_host_generation().unwrap();
    let application = manager.begin_loading_application(generation).unwrap();
    manager
        .finish_loading_application(&application, Ok(()))
        .unwrap();
    let before = fs::read(directory.path().join("state.json")).unwrap();

    let duplicate = candidate(&manager, 1);
    let compiled = plan(&duplicate, true);
    assert_eq!(
        manager
            .accept_loading_configuration(duplicate, compiled)
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        manager.loading_application(),
        Some(&LoadingApplication::Applied(application))
    );
    assert_eq!(
        fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
}

#[test]
fn changed_revision_host_and_attempt_reject_late_receipts_without_hiding_failures() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    manager
        .accept_loading_configuration(captured, compiled)
        .unwrap();
    let generation = manager.allocate_loading_host_generation().unwrap();
    let first = manager.begin_loading_application(generation).unwrap();
    source(&manager, "# new source revision\n[]\n");
    let captured = candidate(&manager, 1);
    let compiled = plan(&captured, false);
    assert_eq!(
        manager
            .accept_loading_configuration(captured, compiled)
            .unwrap()
            .revision,
        2
    );
    assert!(manager.finish_loading_application(&first, Ok(())).is_err());
    let second = manager.begin_loading_application(generation).unwrap();
    manager
        .finish_loading_application(&second, Err("dependency failed".into()))
        .unwrap();
    let third = manager.begin_loading_application(generation).unwrap();
    assert!(manager.finish_loading_application(&second, Ok(())).is_err());
    assert!(
        manager
            .finish_loading_application(&third, Err(String::new()))
            .is_err()
    );
    assert_eq!(
        manager.loading_application(),
        Some(&LoadingApplication::Applying(third.clone()))
    );
    let next_host = manager.allocate_loading_host_generation().unwrap();
    assert!(manager.finish_loading_application(&third, Ok(())).is_err());
    assert!(manager.begin_loading_application(next_host).is_ok());
}

#[test]
fn wrong_source_binding_invalid_graph_and_persistence_failure_leave_the_old_revision_intact() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("state");
    let mut manager = PluginManager::open(&root).unwrap();
    source(&manager, "[]\n");
    let captured = candidate(&manager, 0);
    let mut compiled = plan(&captured, true);
    compiled.sources_sha256 = "wrong".into();
    assert!(
        manager
            .accept_loading_configuration(captured, compiled)
            .is_err()
    );
    let captured = candidate(&manager, 0);
    let mut compiled = plan(&captured, true);
    compiled.source_root = directory.path().join("different-source-project");
    assert!(
        manager
            .accept_loading_configuration(captured, compiled)
            .is_err()
    );
    let captured = candidate(&manager, 0);
    let mut compiled = plan(&captured, true);
    compiled.graph = json!([]);
    assert!(
        manager
            .accept_loading_configuration(captured, compiled)
            .is_err()
    );

    fs::create_dir(root.join("state.json")).unwrap();
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    assert!(
        manager
            .accept_loading_configuration(captured, compiled)
            .is_err()
    );
    assert!(manager.loading_configuration().is_none());
    fs::remove_dir(root.join("state.json")).unwrap();
    let captured = candidate(&manager, 0);
    let compiled = plan(&captured, true);
    assert_eq!(
        manager
            .accept_loading_configuration(captured, compiled)
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn sources_are_bounded_confined_and_captured_in_stable_order() {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    fs::write(
        manager.loading_source_root().join("cordis.patch.yml"),
        "[]\n",
    )
    .unwrap();
    let capture = |names: &[&str]| {
        manager.prepare_loading_configuration(
            0,
            &names.iter().map(|name| (*name).into()).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        capture(&["cordis.yml", "cordis.patch.yml"])
            .unwrap()
            .sources_sha256(),
        capture(&["cordis.patch.yml", "cordis.yml"])
            .unwrap()
            .sources_sha256()
    );
    for names in [
        vec![],
        vec!["cordis.yml", "cordis.yml"],
        vec!["../state.json"],
        vec!["/etc/passwd"],
        vec!["a/../cordis.yml"],
        vec!["a\\cordis.yml"],
    ] {
        assert!(capture(&names).is_err(), "{names:?}");
    }
    fs::write(
        manager.loading_source_root().join("large.yml"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .unwrap();
    assert!(capture(&["large.yml"]).is_err());
    fs::create_dir(manager.loading_source_root().join("directory.yml")).unwrap();
    assert!(capture(&["directory.yml"]).is_err());
    assert!(Path::new(&manager.loading_source_root()).exists());
}

#[cfg(unix)]
#[test]
fn a_source_symlink_cannot_leave_the_loading_project() {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(directory.path()).unwrap();
    source(&manager, "[]\n");
    let outside = directory.path().join("outside.yml");
    fs::write(&outside, "[]\n").unwrap();
    std::os::unix::fs::symlink(&outside, manager.loading_source_root().join("escape.yml")).unwrap();
    assert!(
        manager
            .prepare_loading_configuration(0, &["escape.yml".into()])
            .is_err()
    );
}
