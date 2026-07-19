use magi_git::{
    BranchCreateOptions, BranchDeleteOptions, BranchSwitchOptions, GitError, GitPrecondition,
    GitService, MergeOptions, WorktreeCreateOptions, WorktreeRemoveOptions,
};
use std::{fs, path::Path, process::Command};

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn precondition(observation: &magi_git::GitObservation) -> GitPrecondition {
    GitPrecondition {
        expected_branch: observation.branch.clone(),
        expected_head: observation.head.clone(),
        expected_worktree_path: Some(observation.worktree_path.clone()),
    }
}

/// 使用真实开源仓库拓扑验证 Magi 结构化 Git 工作流。
///
/// 测试始终先克隆到临时目录，绝不修改 `MAGI_GIT_REAL_REPO` 指向的基准 fixture。
#[tokio::test]
async fn rg_retry_supports_full_local_branch_and_worktree_flow() {
    let Ok(source) = std::env::var("MAGI_GIT_REAL_REPO") else {
        eprintln!("skip real-repository fixture: MAGI_GIT_REAL_REPO is not set");
        return;
    };
    let fixture = tempfile::tempdir().expect("integration fixture");
    let repository = fixture.path().join("rg-retry");
    let clone = Command::new("git")
        .args(["clone", "--no-local", source.as_str()])
        .arg(&repository)
        .output()
        .expect("clone fixture");
    assert!(
        clone.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    git(&repository, &["config", "user.name", "Magi Integration"]);
    git(&repository, &["config", "user.email", "magi@example.test"]);

    let service = GitService::new();
    let initial = service.observe(&repository).await.expect("observe");
    assert_eq!(initial.branch.as_deref(), Some("main"));
    assert!(initial.origin_url.is_some());
    let branches = service
        .branch_list(&repository, true)
        .await
        .expect("branch list");
    assert!(branches.branches.iter().any(|branch| branch.name == "main"));
    assert!(
        branches
            .branches
            .iter()
            .any(|branch| branch.is_remote && branch.name == "origin/main")
    );

    let feature = service
        .branch_create(
            &repository,
            BranchCreateOptions {
                branch: "magi/test/rg-retry".to_string(),
                start_point: None,
                switch: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("create feature");
    fs::write(
        repository.join("magi-git-integration.txt"),
        "structured git integration\n",
    )
    .expect("write integration fixture");
    git(&repository, &["add", "magi-git-integration.txt"]);
    git(&repository, &["commit", "-m", "test structured git flow"]);
    let feature_head = service.observe(&repository).await.expect("feature head");
    let main = service
        .branch_switch(
            &repository,
            BranchSwitchOptions {
                branch: "main".to_string(),
                precondition: precondition(&feature_head),
            },
        )
        .await
        .expect("switch main");
    let preview = service
        .merge_preview(&repository, "magi/test/rg-retry", &precondition(&main))
        .await
        .expect("merge preview");
    assert!(preview.fast_forward);
    assert_eq!(preview.incoming_commit_count, 1);
    assert!(
        preview
            .changed_paths
            .contains(&"magi-git-integration.txt".to_string())
    );
    let merged = service
        .merge(
            &repository,
            MergeOptions {
                target: "magi/test/rg-retry".to_string(),
                ff_only: false,
                precondition: precondition(&main),
            },
        )
        .await
        .expect("merge");
    assert_eq!(merged.head, feature_head.head);
    let after_delete = service
        .branch_delete(
            &repository,
            BranchDeleteOptions {
                branch: "magi/test/rg-retry".to_string(),
                remote: None,
                force: false,
                confirm_force: false,
                confirm_remote: false,
                precondition: precondition(&merged),
            },
        )
        .await
        .expect("delete merged feature");

    let disposable = service
        .branch_create(
            &repository,
            BranchCreateOptions {
                branch: "magi/test/disposable".to_string(),
                start_point: None,
                switch: false,
                precondition: precondition(&after_delete),
            },
        )
        .await
        .expect("create disposable");
    let confirmation = service
        .branch_delete(
            &repository,
            BranchDeleteOptions {
                branch: "magi/test/disposable".to_string(),
                remote: None,
                force: true,
                confirm_force: false,
                confirm_remote: false,
                precondition: precondition(&disposable),
            },
        )
        .await
        .expect_err("force delete needs confirmation");
    assert!(matches!(
        confirmation,
        GitError::ConfirmationRequired { .. }
    ));
    service
        .branch_delete(
            &repository,
            BranchDeleteOptions {
                branch: "magi/test/disposable".to_string(),
                remote: None,
                force: true,
                confirm_force: true,
                confirm_remote: false,
                precondition: precondition(&disposable),
            },
        )
        .await
        .expect("confirmed force delete");

    let detached_path = fixture.path().join("rg-retry-readonly-agent");
    let detached = service
        .worktree_create(
            &repository,
            WorktreeCreateOptions {
                path: detached_path.clone(),
                base: merged.head.clone().expect("merged head"),
                branch: None,
                create_branch: false,
                detached: true,
                precondition: precondition(&merged),
            },
        )
        .await
        .expect("detached agent worktree");
    assert!(detached.detached);
    assert_eq!(detached.head, merged.head);
    service
        .worktree_remove(
            &repository,
            WorktreeRemoveOptions {
                path: detached_path,
                force: false,
                confirm_force: false,
                precondition: precondition(&merged),
            },
        )
        .await
        .expect("remove detached worktree");

    assert_eq!(feature.branch.as_deref(), Some("magi/test/rg-retry"));
}
