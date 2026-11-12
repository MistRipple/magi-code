fn command_failed(stderr: &str) -> GitError {
    GitError::CommandFailed {
        operation: "push".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

#[test]
fn command_failures_are_classified_by_unambiguous_stderr_markers() {
    let cases = [
        (
            "fatal: unable to access 'x': Could not resolve host: github.com",
            "git_network_unreachable",
        ),
        (
            "fatal: Authentication failed for 'https://example/x.git'",
            "git_authentication_failed",
        ),
        (
            "git@host: Permission denied (publickey).",
            "git_authentication_failed",
        ),
        (
            "! [rejected] main -> main (non-fast-forward)\nerror: failed to push some refs",
            "git_push_rejected",
        ),
        (
            "fatal: repository 'https://example/x.git/' not found\nRepository not found.",
            "git_remote_unavailable",
        ),
        (
            "error: The branch 'topic' is not fully merged.",
            "git_command_failed",
        ),
    ];
    for (stderr, expected) in cases {
        assert_eq!(command_failed(stderr).code(), expected, "{stderr}");
    }
}

#[test]
fn every_error_code_is_namespaced_and_only_command_errors_are_execution_failures() {
    let invalid = GitError::InvalidInput {
        message: "x".to_string(),
    };
    assert!(invalid.code().starts_with("git_"));
    assert!(!invalid.is_execution_failure());
    assert!(command_failed("boom").is_execution_failure());
    assert!(GitError::Io("x".to_string()).is_execution_failure());
}

use super::*;
use std::{collections::BTreeSet, fs};
use tempfile::TempDir;

#[test]
fn git_commands_do_not_inherit_parent_stdin() {
    const CHILD: &str = "MAGI_GIT_STDIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let repo = repository();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let args = ["hash-object", "--stdin"];
                let empty = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
                assert_eq!(run_git(repo.path(), &args).await.unwrap().stdout, empty);
                assert_eq!(
                    run_git_with_env(repo.path(), &args, &[])
                        .await
                        .unwrap()
                        .stdout,
                    empty
                );
                assert_eq!(
                    run_git_bytes(repo.path(), &args).await.unwrap().stdout,
                    format!("{empty}\n").as_bytes()
                );
            });
        return;
    }
    // 模拟 Desktop 保持开放的 owner pipe；Git 不能读取它或等待其 EOF。
    let mut command = magi_process::std_command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::git_commands_do_not_inherit_parent_stdin",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null());
    let mut child = magi_process::spawn_managed(&mut command).unwrap();
    let _owner_pipe = child.take_stdin().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "Git stdin 隔离子进程失败：{status}");
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.terminate().unwrap();
            panic!("Git 等待宿主 stdin 关闭，没有独立的空输入");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = magi_process::std_command("git")
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

fn repository() -> TempDir {
    let temp = tempfile::tempdir().expect("temp repo");
    git(temp.path(), &["init", "-b", "main"]);
    git(temp.path(), &["config", "user.name", "Magi Test"]);
    git(temp.path(), &["config", "user.email", "magi@example.test"]);
    fs::write(temp.path().join("README.md"), "initial\n").expect("write fixture");
    git(temp.path(), &["add", "README.md"]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

fn precondition(observation: &GitObservation) -> GitPrecondition {
    GitPrecondition {
        expected_branch: observation.branch.clone(),
        expected_head: observation.head.clone(),
        expected_worktree_path: Some(observation.worktree_path.clone()),
    }
}

#[tokio::test]
async fn snapshot_includes_uncommitted_work_without_touching_index_or_refs() {
    let repo = repository();
    let service = GitService::new();
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        service
            .snapshot_worktree(repo.path(), &head)
            .await
            .expect("clean snapshot"),
        head,
        "干净工作树的快照就是 base 本身"
    );

    fs::write(repo.path().join("README.md"), "edited\n").expect("edit tracked");
    fs::write(repo.path().join("new.txt"), "untracked\n").expect("write untracked");
    fs::write(repo.path().join(".gitignore"), "ignored.log\n").expect("write ignore");
    fs::write(repo.path().join("ignored.log"), "noise\n").expect("write ignored");
    let status_before = git(repo.path(), &["status", "--porcelain"]);

    let snapshot = service
        .snapshot_worktree(repo.path(), &head)
        .await
        .expect("dirty snapshot");

    assert_ne!(snapshot, head);
    assert_eq!(
        git(repo.path(), &["rev-parse", &format!("{snapshot}^")]),
        head
    );
    assert_eq!(
        git(repo.path(), &["show", &format!("{snapshot}:README.md")]),
        "edited"
    );
    assert_eq!(
        git(repo.path(), &["show", &format!("{snapshot}:new.txt")]),
        "untracked"
    );
    assert!(
        magi_process::std_command("git")
            .arg("-C")
            .arg(repo.path())
            .args(["cat-file", "-e", &format!("{snapshot}:ignored.log")])
            .status()
            .is_ok_and(|status| !status.success()),
        "忽略的文件不进入快照"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head, "HEAD 不变");
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        status_before,
        "用户的 index 和工作树不变"
    );
}

#[tokio::test]
async fn agent_changes_are_committed_and_applied_atomically() {
    let repo = repository();
    let service = GitService::new();
    // 主线有未提交改动；代理从包含它的快照出发。
    fs::write(repo.path().join("README.md"), "main edit\n").expect("main edit");
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let base = service
        .snapshot_worktree(repo.path(), &head)
        .await
        .expect("snapshot");
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let agent_path = agent_dir.path().join("agent");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "magi/agent/test",
            agent_path.to_str().unwrap(),
            &base,
        ],
    );
    assert_eq!(
        fs::read_to_string(agent_path.join("README.md")).unwrap(),
        "main edit\n",
        "代理看得到主线未提交的改动"
    );
    assert_eq!(
        service
            .commit_all(&agent_path, "nothing")
            .await
            .expect("noop commit"),
        None
    );
    fs::write(agent_path.join("feature.rs"), "fn feature() {}\n").expect("agent write");
    let agent_head = service
        .commit_all(&agent_path, "magi agent: feature")
        .await
        .expect("commit")
        .expect("should commit");
    assert_eq!(
        service
            .changed_paths(&agent_path, &base, &agent_head)
            .await
            .unwrap(),
        vec!["feature.rs".to_string()]
    );

    // 主线同时修改了同一文件的同一行 → 冲突时主线保持原样。
    fs::write(agent_path.join("README.md"), "agent edit\n").expect("agent conflict edit");
    let conflicting_head = service
        .commit_all(&agent_path, "magi agent: conflict")
        .await
        .unwrap()
        .unwrap();
    fs::write(repo.path().join("README.md"), "main later edit\n").expect("main later edit");
    let error = service
        .apply_revision_diff(repo.path(), &base, &conflicting_head)
        .await
        .expect_err("同一行冲突必须拒绝");
    assert!(
        matches!(error, GitError::ApplyConflict { ref conflicted_paths, .. } if conflicted_paths.contains(&"README.md".to_string())),
        "{error:?}"
    );
    assert!(!repo.path().join("feature.rs").exists(), "冲突时不能半应用");
    assert_eq!(
        fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "main later edit\n"
    );

    // 无冲突的改动应用到主线工作树，不产生提交、不改 index。
    let applied = service
        .apply_revision_diff(repo.path(), &base, &agent_head)
        .await
        .expect("clean apply");
    assert_eq!(applied, vec!["feature.rs".to_string()]);
    assert_eq!(
        fs::read_to_string(repo.path().join("feature.rs")).unwrap(),
        "fn feature() {}\n"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn restore_after_restart_releases_stale_agent_worktree_occupancy() {
    let repo = repository();
    let service = GitService::new();
    let observation = service.observe(repo.path()).await.expect("observe");
    let registry = SessionCodeContextRegistry::default();
    registry.accept("session-restart", "workspace-restart", vec![], &observation);
    let agent_path = repo.path().join("agent-restart");
    registry
        .register_agent_worktree(
            "session-restart",
            AgentWorktreeContext {
                task_id: "task-running-before-restart".to_string(),
                lease_id: Some("lease-old-process".to_string()),
                worker_id: "worker-executor".to_string(),
                path: agent_path.clone(),
                mode: AgentWorktreeMode::Writable,
                base_head: observation.head.clone().expect("base head"),
                branch: Some("magi/agent/restart".to_string()),
                active: true,
                result_head: None,
                changed_paths: Vec::new(),
                applied: false,
            },
        )
        .expect("register worktree");
    let persisted = registry.all();
    assert!(persisted[0].agent_worktrees[0].active);

    let restored = SessionCodeContextRegistry::default();
    assert_eq!(restored.restore_after_restart(persisted), 1);
    let context = restored.get("session-restart").expect("context restored");
    assert!(
        !context.agent_worktrees[0].active,
        "重启后不能再有占用中的 worktree"
    );
    assert!(
        !context
            .runtime_workspace_roots
            .iter()
            .any(|root| same_path(root, &agent_path))
    );
    assert_eq!(context.agent_worktrees[0].path, agent_path, "分配记录保留");
}

#[tokio::test]
async fn project_repository_contexts_keep_independent_revisions_outputs_and_restart_state() {
    let repo = repository();
    let second = repository();
    let service = GitService::new();
    let linked_parent = tempfile::tempdir().unwrap();
    let linked = linked_parent.path().join("linked");
    service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: linked.clone(),
                base: "HEAD".into(),
                branch: Some("linked-project".into()),
                create_branch: true,
                detached: false,
                precondition: GitPrecondition::default(),
            },
        )
        .await
        .unwrap();
    let observations = [
        service.observe(repo.path()).await.unwrap(),
        service.observe(second.path()).await.unwrap(),
        service.observe(&linked).await.unwrap(),
    ];
    assert_eq!(
        observations[0].git_common_dir,
        observations[2].git_common_dir
    );
    let registry = SessionCodeContextRegistry::default();
    for (index, observation) in observations.iter().enumerate() {
        registry.accept("project-session", "project", vec![], observation);
        registry
            .register_agent_worktree_for_root(
                "project-session",
                &observation.worktree_path,
                AgentWorktreeContext {
                    task_id: "same-task".into(),
                    lease_id: Some("same-lease".into()),
                    worker_id: "worker".into(),
                    path: linked_parent.path().join(format!("agent-{index}")),
                    mode: AgentWorktreeMode::Writable,
                    base_head: observation.head.clone().unwrap(),
                    branch: None,
                    active: true,
                    result_head: None,
                    changed_paths: vec![],
                    applied: false,
                },
            )
            .unwrap();
    }
    assert!(registry.get("project-session").is_none());
    assert!(
        registry
            .register_agent_worktree(
                "project-session",
                registry.all()[0].agent_worktrees[0].clone()
            )
            .is_err()
    );
    let untouched = registry
        .get_repository("project-session", &observations[1].worktree_path)
        .unwrap();
    git(repo.path(), &["switch", "-c", "external-change"]);
    let current = service.observe(repo.path()).await.unwrap();
    let drifted = registry.observe("project-session", "project", vec![], &current);
    assert!(drifted.has_external_drift());
    assert_eq!(
        registry
            .get_repository("project-session", &observations[1].worktree_path)
            .unwrap(),
        untouched
    );
    assert_eq!(
        registry
            .get_for_path(
                "project-session",
                &linked_parent.path().join("agent-0/src/index.ts")
            )
            .unwrap()
            .execution_root,
        current.worktree_path
    );
    assert!(
        registry
            .validate_revision_for_root(
                "project-session",
                &current.worktree_path,
                Some(untouched.context_revision)
            )
            .is_err()
    );
    registry
        .validate_revision_for_root(
            "project-session",
            &untouched.execution_root,
            Some(untouched.context_revision),
        )
        .unwrap();
    registry
        .record_agent_worktree_result_for_root(
            "project-session",
            &current.worktree_path,
            "same-task",
            "same-lease",
            "result-a".into(),
            vec!["src/index.ts".into()],
        )
        .unwrap();
    assert!(
        registry
            .get_repository("project-session", &untouched.execution_root)
            .unwrap()
            .agent_worktrees[0]
            .result_head
            .is_none()
    );
    let persisted = serde_json::to_vec(&registry.all()).unwrap();
    let restored = SessionCodeContextRegistry::default();
    assert_eq!(
        restored.restore_after_restart(serde_json::from_slice(&persisted).unwrap()),
        3
    );
    assert_eq!(restored.for_session("project-session").len(), 3);
    for context in restored.all() {
        assert!(!context.agent_worktrees[0].active);
        assert_eq!(
            context.runtime_workspace_roots,
            vec![context.execution_root.clone()]
        );
    }
    assert_eq!(
        restored
            .get_repository("project-session", &current.worktree_path)
            .unwrap()
            .agent_worktrees[0]
            .result_head
            .as_deref(),
        Some("result-a")
    );
    restored
        .remove_for_root("project-session", &current.worktree_path)
        .unwrap();
    assert_eq!(restored.for_session("project-session").len(), 2);
    restored.remove("project-session");
    assert!(restored.for_session("project-session").is_empty());
}

#[tokio::test]
async fn agent_worktree_recovery_preserves_dirty_files_and_rebinds_only_the_old_lease() {
    let repo = repository();
    let service = GitService::new();
    let observation = service.observe(repo.path()).await.unwrap();
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("agent");
    service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: path.clone(),
                base: observation.head.clone().unwrap(),
                branch: Some("magi/agent/recovery".into()),
                create_branch: true,
                detached: false,
                precondition: GitPrecondition::default(),
            },
        )
        .await
        .unwrap();
    std::fs::write(path.join("staged.txt"), "staged before restart").unwrap();
    ensure_success(
        "stage_fixture",
        run_git(&path, &["add", "staged.txt"]).await.unwrap(),
    )
    .unwrap();
    std::fs::write(path.join("staged.txt"), "unstaged after staging").unwrap();
    std::fs::write(path.join("untracked.txt"), "untracked before restart").unwrap();
    let before_index = run_git(&path, &["diff", "--cached", "--binary"])
        .await
        .unwrap()
        .stdout;
    let before_worktree = run_git(&path, &["diff", "--binary"]).await.unwrap().stdout;
    let registry = SessionCodeContextRegistry::default();
    registry.accept("recovery", "workspace", vec![], &observation);
    registry
        .register_agent_worktree(
            "recovery",
            AgentWorktreeContext {
                task_id: "agent".into(),
                lease_id: Some("old-lease".into()),
                worker_id: "old-worker".into(),
                path: path.clone(),
                mode: AgentWorktreeMode::Writable,
                base_head: observation.head.clone().unwrap(),
                branch: Some("magi/agent/recovery".into()),
                active: true,
                result_head: None,
                changed_paths: vec![],
                applied: false,
            },
        )
        .unwrap();
    registry.restore_after_restart(registry.all());
    let previous = registry.get("recovery").unwrap().agent_worktrees[0].clone();
    service
        .recover_agent_worktree(repo.path(), &observation.git_common_dir, &previous)
        .await
        .unwrap();
    registry
        .reattach_agent_worktree("recovery", &previous, "new-lease", "new-worker")
        .unwrap();
    assert_eq!(
        run_git(&path, &["diff", "--cached", "--binary"])
            .await
            .unwrap()
            .stdout,
        before_index
    );
    assert_eq!(
        run_git(&path, &["diff", "--binary"]).await.unwrap().stdout,
        before_worktree
    );
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "untracked before restart"
    );
    assert!(
        registry
            .reattach_agent_worktree("recovery", &previous, "another-lease", "another-worker")
            .is_err()
    );
    assert!(
        registry
            .release_agent_worktree_for_lease("recovery", "agent", "old-lease")
            .is_err()
    );
    let current = registry.get("recovery").unwrap();
    assert_eq!(current.agent_worktrees.len(), 1);
    assert_eq!(
        current.agent_worktrees[0].lease_id.as_deref(),
        Some("new-lease")
    );
    assert!(current.agent_worktrees[0].active);
    assert!(current.runtime_workspace_roots.contains(&path));
    assert!(!repo.path().join("untracked.txt").exists());
}

#[tokio::test]
async fn agent_worktree_recovery_rejects_changed_repository_branch_and_applied_results() {
    let repo = repository();
    let other = repository();
    let service = GitService::new();
    let observation = service.observe(repo.path()).await.unwrap();
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("agent");
    service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: path.clone(),
                base: observation.head.clone().unwrap(),
                branch: Some("magi/agent/original".into()),
                create_branch: true,
                detached: false,
                precondition: GitPrecondition::default(),
            },
        )
        .await
        .unwrap();
    let allocation = AgentWorktreeContext {
        task_id: "agent".into(),
        lease_id: Some("old".into()),
        worker_id: "worker".into(),
        path: path.clone(),
        mode: AgentWorktreeMode::Writable,
        base_head: observation.head.clone().unwrap(),
        branch: Some("magi/agent/original".into()),
        active: false,
        result_head: None,
        changed_paths: vec![],
        applied: false,
    };
    for mutate in [
        "active",
        "applied",
        "foreign_repo",
        "branch",
        "subdirectory",
    ] {
        let mut invalid = allocation.clone();
        match mutate {
            "active" => invalid.active = true,
            "applied" => invalid.applied = true,
            "foreign_repo" => invalid.path = other.path().to_path_buf(),
            "branch" => invalid.branch = Some("another-branch".into()),
            "subdirectory" => {
                invalid.path = path.join("subdir");
                std::fs::create_dir(&invalid.path).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            service
                .recover_agent_worktree(repo.path(), &observation.git_common_dir, &invalid)
                .await
                .is_err(),
            "{mutate}"
        );
    }
    service
        .recover_agent_worktree(repo.path(), &observation.git_common_dir, &allocation)
        .await
        .unwrap();
}

#[tokio::test]
async fn agent_worktree_recovery_rebuilds_only_the_recorded_branch_result() {
    let repo = repository();
    let service = GitService::new();
    let observation = service.observe(repo.path()).await.unwrap();
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("agent");
    service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: path.clone(),
                base: observation.head.clone().unwrap(),
                branch: Some("magi/agent/result".into()),
                create_branch: true,
                detached: false,
                precondition: GitPrecondition::default(),
            },
        )
        .await
        .unwrap();
    std::fs::write(path.join("result.txt"), "agent result").unwrap();
    service.commit_all(&path, "agent result").await.unwrap();
    let result = service.observe_ref(&path).await.unwrap().head.unwrap();
    service
        .worktree_remove(
            repo.path(),
            WorktreeRemoveOptions {
                path: path.clone(),
                force: false,
                confirm_force: false,
                precondition: GitPrecondition::default(),
            },
        )
        .await
        .unwrap();
    let mut allocation = AgentWorktreeContext {
        task_id: "agent".into(),
        lease_id: Some("old".into()),
        worker_id: "worker".into(),
        path: path.clone(),
        mode: AgentWorktreeMode::Writable,
        base_head: observation.head.clone().unwrap(),
        branch: Some("magi/agent/result".into()),
        active: false,
        result_head: None,
        changed_paths: vec!["result.txt".into()],
        applied: false,
    };
    assert!(
        service
            .recover_agent_worktree(repo.path(), &observation.git_common_dir, &allocation)
            .await
            .is_err()
    );
    assert!(
        !path.exists(),
        "缺少匹配的持久结果，不能静默改用分支当前 HEAD"
    );
    allocation.result_head = Some(result.clone());
    service
        .recover_agent_worktree(repo.path(), &observation.git_common_dir, &allocation)
        .await
        .unwrap();
    assert_eq!(
        service.observe_ref(&path).await.unwrap().head.as_deref(),
        Some(result.as_str())
    );
    assert_eq!(
        std::fs::read_to_string(path.join("result.txt")).unwrap(),
        "agent result"
    );
}

#[tokio::test]
async fn agent_worktrees_are_scoped_by_execution_lease() {
    let repo = repository();
    let service = GitService::new();
    let observation = service.observe(repo.path()).await.expect("observe");
    let registry = SessionCodeContextRegistry::default();
    registry.accept(
        "session-lease-scope",
        "workspace-lease-scope",
        vec![],
        &observation,
    );

    for (lease_id, path_suffix) in [("lease-old", "old"), ("lease-current", "current")] {
        registry
            .register_agent_worktree(
                "session-lease-scope",
                AgentWorktreeContext {
                    task_id: "task-recovered".to_string(),
                    lease_id: Some(lease_id.to_string()),
                    worker_id: "worker-executor".to_string(),
                    path: repo.path().join(path_suffix),
                    mode: AgentWorktreeMode::ReadOnly,
                    base_head: observation.head.clone().expect("base head"),
                    branch: None,
                    active: true,
                    result_head: None,
                    changed_paths: Vec::new(),
                    applied: false,
                },
            )
            .expect("register worktree");
    }

    let before = registry.get("session-lease-scope").expect("context");
    assert_eq!(before.agent_worktrees.len(), 2);
    registry
        .release_agent_worktree_for_lease("session-lease-scope", "task-recovered", "lease-old")
        .expect("release stale lease");
    let after = registry.get("session-lease-scope").expect("context");
    assert!(
        !after
            .agent_worktrees
            .iter()
            .find(|worktree| worktree.lease_id.as_deref() == Some("lease-old"))
            .expect("stale worktree")
            .active
    );
    assert!(
        after
            .agent_worktrees
            .iter()
            .find(|worktree| worktree.lease_id.as_deref() == Some("lease-current"))
            .expect("current worktree")
            .active
    );
}

#[tokio::test]
async fn create_switch_and_delete_branch_with_cas() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let created = service
        .branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "feature/test".to_string(),
                start_point: None,
                switch: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("create branch");
    assert_eq!(created.branch.as_deref(), Some("feature/test"));

    let switched = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "main".to_string(),
                precondition: precondition(&created),
            },
        )
        .await
        .expect("switch branch");
    service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "feature/test".to_string(),
                remote: None,
                force: false,
                confirm_force: false,
                confirm_remote: false,
                precondition: precondition(&switched),
            },
        )
        .await
        .expect("delete branch");
    let list = service
        .branch_list(repo.path(), false)
        .await
        .expect("list branches");
    assert_eq!(list.branches.len(), 1);
    assert_eq!(list.branches[0].name, "main");
}

#[tokio::test]
async fn preserves_dirty_changes_during_switch_and_rejects_stale_head() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    service
        .branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "other".to_string(),
                start_point: None,
                switch: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("create branch");
    fs::write(repo.path().join("README.md"), "dirty\n").expect("dirty fixture");
    let switched_with_changes = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "other".to_string(),
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("compatible dirty changes should follow branch switch");
    assert_eq!(switched_with_changes.branch.as_deref(), Some("other"));
    assert!(switched_with_changes.dirty.has_uncommitted);
    assert_eq!(
        fs::read_to_string(repo.path().join("README.md")).expect("read dirty fixture"),
        "dirty\n"
    );

    git(repo.path(), &["restore", "README.md"]);
    let back_on_main = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "main".to_string(),
                precondition: precondition(&switched_with_changes),
            },
        )
        .await
        .expect("switch back to main");
    fs::write(repo.path().join("next.txt"), "next\n").expect("next fixture");
    git(repo.path(), &["add", "next.txt"]);
    git(repo.path(), &["commit", "-m", "external commit"]);
    let stale = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "other".to_string(),
                precondition: precondition(&back_on_main),
            },
        )
        .await
        .expect_err("stale head must fail");
    assert!(matches!(stale, GitError::StaleContext { .. }));
}

#[tokio::test]
async fn create_and_switch_branch_preserves_dirty_changes() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    fs::write(repo.path().join("README.md"), "work in progress\n").expect("dirty fixture");

    let created = service
        .branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "feature/from-dirty".to_string(),
                start_point: None,
                switch: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("create branch with compatible dirty changes");

    assert_eq!(created.branch.as_deref(), Some("feature/from-dirty"));
    assert!(created.dirty.has_uncommitted);
    assert_eq!(
        fs::read_to_string(repo.path().join("README.md")).expect("read dirty fixture"),
        "work in progress\n"
    );
}

#[tokio::test]
async fn concurrent_branch_mutations_allow_exactly_one_cas_winner() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let first_service = service.clone();
    let second_service = service.clone();
    let first_precondition = precondition(&initial);
    let second_precondition = first_precondition.clone();
    let (first, second) = tokio::join!(
        first_service.branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "concurrent/first".to_string(),
                start_point: None,
                switch: true,
                precondition: first_precondition,
            }
        ),
        second_service.branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "concurrent/second".to_string(),
                start_point: None,
                switch: true,
                precondition: second_precondition,
            }
        )
    );
    let results = [first, second];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(GitError::StaleContext { .. })))
            .count(),
        1
    );
}

#[tokio::test]
async fn creates_detached_and_writable_worktrees() {
    let repo = repository();
    let parent = repo.path().parent().expect("parent");
    let detached_path = parent.join(format!(
        "{}-detached",
        repo.path().file_name().unwrap().to_string_lossy()
    ));
    let writable_path = parent.join(format!(
        "{}-writable",
        repo.path().file_name().unwrap().to_string_lossy()
    ));
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let detached = service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: detached_path.clone(),
                base: initial.head.clone().expect("head"),
                branch: None,
                create_branch: false,
                detached: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("detached worktree");
    assert!(detached.detached);
    let writable = service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: writable_path.clone(),
                base: initial.head.clone().expect("head"),
                branch: Some("magi/worker-test".to_string()),
                create_branch: true,
                detached: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("writable worktree");
    assert_eq!(writable.branch.as_deref(), Some("magi/worker-test"));
    service
        .worktree_remove(
            repo.path(),
            WorktreeRemoveOptions {
                path: detached_path,
                force: false,
                confirm_force: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("remove detached");
    service
        .worktree_remove(
            repo.path(),
            WorktreeRemoveOptions {
                path: writable_path,
                force: false,
                confirm_force: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("remove writable");
}

#[tokio::test]
async fn merge_preview_and_merge_report_revision_changes() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let feature = service
        .branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "feature".to_string(),
                start_point: None,
                switch: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("feature");
    fs::write(repo.path().join("feature.txt"), "feature\n").expect("feature fixture");
    git(repo.path(), &["add", "feature.txt"]);
    git(repo.path(), &["commit", "-m", "feature"]);
    let feature_head = service.observe(repo.path()).await.expect("feature head");
    let main = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "main".to_string(),
                precondition: precondition(&feature_head),
            },
        )
        .await
        .expect("main");
    let preview = service
        .merge_preview(repo.path(), "feature", &precondition(&main))
        .await
        .expect("preview");
    assert!(preview.fast_forward);
    assert_eq!(preview.incoming_commit_count, 1);
    assert_eq!(preview.changed_paths, vec!["feature.txt"]);
    let merged = service
        .merge(
            repo.path(),
            MergeOptions {
                target: "feature".to_string(),
                ff_only: false,
                precondition: precondition(&main),
            },
        )
        .await
        .expect("merge");
    assert_eq!(merged.head, feature_head.head);
    assert_ne!(feature.head, merged.head);
}

#[tokio::test]
async fn merge_conflict_returns_paths_and_keeps_git_conflict_state() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let feature = service
        .branch_create(
            repo.path(),
            BranchCreateOptions {
                branch: "conflicting".to_string(),
                start_point: None,
                switch: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("feature");
    fs::write(repo.path().join("README.md"), "feature\n").expect("feature content");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "feature conflict"]);
    let feature_head = service.observe(repo.path()).await.expect("feature head");
    let _main = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "main".to_string(),
                precondition: precondition(&feature_head),
            },
        )
        .await
        .expect("main");
    fs::write(repo.path().join("README.md"), "main\n").expect("main content");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "main conflict"]);
    let main_head = service.observe(repo.path()).await.expect("main head");
    let conflict = service
        .merge(
            repo.path(),
            MergeOptions {
                target: "conflicting".to_string(),
                ff_only: false,
                precondition: precondition(&main_head),
            },
        )
        .await
        .expect_err("merge should conflict");
    match conflict {
        GitError::MergeConflict {
            conflicted_paths, ..
        } => assert_eq!(conflicted_paths, vec!["README.md"]),
        other => panic!("unexpected error: {other}"),
    }
    let conflicted = service
        .observe(repo.path())
        .await
        .expect("conflicted status");
    assert_eq!(conflicted.dirty.conflicted_paths, vec!["README.md"]);
    assert_eq!(feature.branch.as_deref(), Some("conflicting"));
}

#[tokio::test]
async fn branch_delete_blocks_current_remote_and_checked_out_branch() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let current = service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "main".to_string(),
                remote: None,
                force: false,
                confirm_force: false,
                confirm_remote: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("current branch deletion must fail");
    assert!(matches!(current, GitError::CurrentBranch { .. }));

    let remote = service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "main".to_string(),
                remote: Some("origin".to_string()),
                force: false,
                confirm_force: false,
                confirm_remote: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("remote delete needs confirmation");
    assert!(matches!(remote, GitError::ConfirmationRequired { .. }));

    let worktree_path = repo.path().parent().unwrap().join(format!(
        "{}-checked-out",
        repo.path().file_name().unwrap().to_string_lossy()
    ));
    service
        .worktree_create(
            repo.path(),
            WorktreeCreateOptions {
                path: worktree_path.clone(),
                base: initial.head.clone().expect("head"),
                branch: Some("checked-out".to_string()),
                create_branch: true,
                detached: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("worktree");
    let in_use = service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "checked-out".to_string(),
                remote: None,
                force: true,
                confirm_force: true,
                confirm_remote: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("checked out branch deletion must fail");
    assert!(matches!(in_use, GitError::BranchInUse { .. }));
    service
        .worktree_remove(
            repo.path(),
            WorktreeRemoveOptions {
                path: worktree_path,
                force: false,
                confirm_force: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("cleanup worktree");
}

#[tokio::test]
async fn pull_and_push_use_configured_upstream() {
    let repo = repository();
    let remote = tempfile::tempdir().expect("bare remote");
    git(remote.path(), &["init", "--bare"]);
    let remote_path = remote.path().to_string_lossy().to_string();
    git(
        repo.path(),
        &["remote", "add", "origin", remote_path.as_str()],
    );
    git(repo.path(), &["push", "--set-upstream", "origin", "main"]);
    git(remote.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let peer_parent = tempfile::tempdir().expect("peer parent");
    let peer = peer_parent.path().join("peer");
    let peer_path = peer.to_string_lossy().to_string();
    git(
        peer_parent.path(),
        &[
            "clone",
            "--branch",
            "main",
            remote_path.as_str(),
            &peer_path,
        ],
    );
    git(&peer, &["config", "user.name", "Magi Peer"]);
    git(&peer, &["config", "user.email", "peer@example.test"]);
    fs::write(peer.join("remote.txt"), "from remote\n").expect("remote fixture");
    git(&peer, &["add", "remote.txt"]);
    git(&peer, &["commit", "-m", "remote update"]);
    git(&peer, &["push", "origin", "main"]);

    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    let pulled = service
        .pull(
            repo.path(),
            PullOptions {
                remote: None,
                branch: None,
                ff_only: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("pull configured upstream");
    assert_eq!(
        fs::read_to_string(repo.path().join("remote.txt")).unwrap(),
        "from remote\n"
    );

    fs::write(repo.path().join("local.txt"), "from local\n").expect("local fixture");
    git(repo.path(), &["add", "local.txt"]);
    git(repo.path(), &["commit", "-m", "local update"]);
    let local = service.observe(repo.path()).await.expect("local head");
    let confirmation = service
        .push(
            repo.path(),
            PushOptions {
                remote: None,
                branch: None,
                set_upstream: false,
                force_with_lease: true,
                confirm_force: false,
                precondition: precondition(&local),
            },
        )
        .await
        .expect_err("force-with-lease needs confirmation");
    assert!(matches!(
        confirmation,
        GitError::ConfirmationRequired { .. }
    ));

    let pushed = service
        .push(
            repo.path(),
            PushOptions {
                remote: None,
                branch: None,
                set_upstream: false,
                force_with_lease: false,
                confirm_force: false,
                precondition: precondition(&local),
            },
        )
        .await
        .expect("push configured upstream");
    assert_eq!(
        git(remote.path(), &["rev-parse", "refs/heads/main"]),
        pushed.head.expect("pushed head")
    );
    assert_eq!(pulled.branch.as_deref(), Some("main"));
}

#[tokio::test]
async fn confirmed_remote_delete_uses_exact_configured_remote() {
    let repo = repository();
    let remote = tempfile::tempdir().expect("bare remote");
    git(remote.path(), &["init", "--bare"]);
    let remote_path = remote.path().to_string_lossy().to_string();
    git(
        repo.path(),
        &["remote", "add", "fixture", remote_path.as_str()],
    );
    git(
        repo.path(),
        &["push", "fixture", "main:refs/heads/delete-me"],
    );
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");
    service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "delete-me".to_string(),
                remote: Some("fixture".to_string()),
                force: false,
                confirm_force: false,
                confirm_remote: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect("confirmed remote delete");
    let deleted = run_git(
        remote.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/delete-me"],
    )
    .await
    .expect("inspect remote");
    assert!(!deleted.success, "remote branch must be deleted");
}

#[tokio::test]
async fn structured_operations_reject_option_like_refs_and_remotes() {
    let repo = repository();
    let service = GitService::new();
    let initial = service.observe(repo.path()).await.expect("observe");

    let switch = service
        .branch_switch(
            repo.path(),
            BranchSwitchOptions {
                branch: "--detach".to_string(),
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("option-like branch must be rejected");
    assert!(matches!(switch, GitError::InvalidInput { .. }));

    let merge = service
        .merge(
            repo.path(),
            MergeOptions {
                target: "--abort".to_string(),
                ff_only: false,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("option-like revision must be rejected");
    assert!(matches!(merge, GitError::InvalidInput { .. }));

    let remote = service
        .branch_delete(
            repo.path(),
            BranchDeleteOptions {
                branch: "topic".to_string(),
                remote: Some("--force".to_string()),
                force: false,
                confirm_force: false,
                confirm_remote: true,
                precondition: precondition(&initial),
            },
        )
        .await
        .expect_err("option-like remote must be rejected");
    assert!(matches!(remote, GitError::InvalidInput { .. }));
}

#[test]
fn conflict_status_matrix_is_explicit() {
    let statuses = BTreeSet::from([
        ('D', 'D'),
        ('A', 'U'),
        ('U', 'D'),
        ('U', 'A'),
        ('D', 'U'),
        ('A', 'A'),
        ('U', 'U'),
    ]);
    for x in [' ', 'A', 'D', 'M', 'R', 'U'] {
        for y in [' ', 'A', 'D', 'M', 'R', 'U'] {
            assert_eq!(is_conflicted_status(x, y), statuses.contains(&(x, y)));
        }
    }
}

#[tokio::test]
async fn session_context_reports_external_branch_or_head_drift() {
    let repo = repository();
    let service = GitService::new();
    let registry = SessionCodeContextRegistry::default();
    let initial = service.observe(repo.path()).await.expect("observe");
    let bound = registry.accept("session-1", "workspace-1", vec![], &initial);
    assert!(!bound.has_external_drift());

    fs::write(repo.path().join("external.txt"), "external\n").expect("external fixture");
    git(repo.path(), &["add", "external.txt"]);
    git(repo.path(), &["commit", "-m", "external"]);
    let external = service
        .observe(repo.path())
        .await
        .expect("external observe");
    assert!(
        service
            .is_session_context_fast_forward(&bound, &external)
            .await
            .expect("classify fast-forward")
    );
    let drifted = registry.observe("session-1", "workspace-1", vec![], &external);
    assert!(drifted.has_external_drift());
    assert_eq!(drifted.git.base_head, initial.head);
    assert_eq!(drifted.git.observed_head, external.head);

    let accepted = registry.accept("session-1", "workspace-1", vec![], &external);
    assert!(!accepted.has_external_drift());
    assert!(accepted.context_revision > bound.context_revision);

    git(repo.path(), &["switch", "-c", "external/branch"]);
    let switched = service.observe(repo.path()).await.expect("branch observe");
    assert!(
        !service
            .is_session_context_fast_forward(&accepted, &switched)
            .await
            .expect("classify branch switch")
    );
}

#[tokio::test]
async fn ref_observation_and_tree_patch_preserve_git_tree_content() {
    let repo = repository();
    fs::write(repo.path().join("deleted.txt"), "delete me\n").expect("deleted fixture");
    git(repo.path(), &["add", "deleted.txt"]);
    git(repo.path(), &["commit", "-m", "add delete fixture"]);
    let service = GitService::new();
    let before = service.observe(repo.path()).await.expect("observe before");
    let before_head = before.head.expect("before HEAD");

    fs::write(repo.path().join("README.md"), b"updated\n\0binary").expect("modify fixture");
    fs::write(repo.path().join("added.txt"), "added\n").expect("add fixture");
    fs::remove_file(repo.path().join("deleted.txt")).expect("delete fixture");
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-m", "tree patch fixture"]);

    let after = service.observe(repo.path()).await.expect("observe after");
    let after_head = after.head.clone().expect("after HEAD");
    let reference = service.observe_ref(repo.path()).await.expect("observe ref");
    assert_eq!(reference.branch.as_deref(), Some("main"));
    assert_eq!(reference.head.as_deref(), Some(after_head.as_str()));

    let patch = service
        .tree_baseline_patch(repo.path(), &before_head, &after_head)
        .await
        .expect("tree patch");
    assert!(patch.iter().any(|entry| matches!(
        entry,
        GitTreeBaselineEntry::RegularFile { path, content }
            if path == "README.md" && content == b"updated\n\0binary"
    )));
    assert!(patch.iter().any(|entry| matches!(
        entry,
        GitTreeBaselineEntry::RegularFile { path, content }
            if path == "added.txt" && content == b"added\n"
    )));
    assert!(patch.iter().any(|entry| matches!(
        entry,
        GitTreeBaselineEntry::Deleted { path } if path == "deleted.txt"
    )));
}

#[test]
fn workspace_coordinator_hands_the_repository_to_waiters_in_order() {
    let coordinator = WorkspaceGitOperationCoordinator::default();
    let repository = PathBuf::from("/tmp/magi-git-coordinator-fairness/.git");
    coordinator
        .begin_execution("session-a", &repository)
        .expect("a runs first");
    // b、c 依次因仓库被占用而排队。
    coordinator
        .begin_execution("session-b", &repository)
        .expect_err("b waits for a");
    coordinator.register_waiter("session-b", &repository);
    coordinator
        .begin_execution("session-c", &repository)
        .expect_err("c waits for a");
    coordinator.register_waiter("session-c", &repository);

    // a 结束后立刻想再占用（目标续跑）：必须排在 b、c 后面，不能抢先。
    coordinator.end_execution("session-a");
    coordinator
        .begin_execution("session-a", &repository)
        .expect_err("a must queue behind the waiters");
    coordinator.register_waiter("session-a", &repository);
    coordinator
        .begin_execution("session-c", &repository)
        .expect_err("c is not the head of the queue");
    coordinator
        .begin_execution("session-b", &repository)
        .expect("b is the head and gets the repository");

    coordinator.end_execution("session-b");
    coordinator
        .begin_execution("session-a", &repository)
        .expect_err("c is still ahead of a");
    coordinator
        .begin_execution("session-c", &repository)
        .expect("c is next");
    coordinator.end_execution("session-c");
    coordinator
        .begin_execution("session-a", &repository)
        .expect("a is last and gets it now");
}

#[test]
fn workspace_coordinator_repository_sets_release_partial_ownership_and_share_queue_order() {
    let coordinator = WorkspaceGitOperationCoordinator::default();
    let a = PathBuf::from("/tmp/magi-multi-lease-a/.git");
    let b = PathBuf::from("/tmp/magi-multi-lease-b/.git");
    coordinator.begin_execution("first", &a).unwrap();
    coordinator.begin_execution("second", &b).unwrap();
    assert!(
        coordinator
            .begin_execution_set("first", &[b.clone(), a.clone()])
            .is_err()
    );
    assert!(coordinator.execution_roots("first").is_empty());
    // 刻意交叉登记；队列次序必须以会话为单位，不能形成两个仓库的不同队首。
    coordinator.register_waiter("first", &a);
    coordinator.register_waiter("second", &b);
    coordinator.register_waiter("first", &b);
    coordinator.register_waiter("second", &a);
    assert!(
        coordinator
            .begin_execution_set("second", &[a.clone(), b.clone()])
            .is_err()
    );
    assert!(coordinator.execution_roots("second").is_empty());
    coordinator
        .begin_execution_set("first", &[b.clone(), a.clone(), b.clone()])
        .unwrap();
    assert_eq!(
        coordinator.execution_roots("first"),
        vec![a.clone(), b.clone()]
    );
    coordinator.suspend_execution("first");
    coordinator.suspend_execution("first");
    coordinator
        .begin_execution_set("second", &[a.clone(), b.clone()])
        .unwrap();
    assert!(coordinator.try_resume_execution("first").is_err());
    assert!(coordinator.execution_roots("first").is_empty());
    coordinator.end_execution("second");
    coordinator.try_resume_execution("first").unwrap();
    assert_eq!(
        coordinator.execution_roots("first"),
        vec![a.clone(), b.clone()]
    );
    coordinator.end_execution_for_repository("first", &a);
    assert_eq!(coordinator.execution_roots("first"), vec![b]);
    coordinator.suspend_execution("first");
    coordinator.end_execution("first");
    coordinator.try_resume_execution("first").unwrap();
    assert!(
        coordinator.execution_roots("first").is_empty(),
        "取消后不能重新取得旧租约"
    );
}

#[test]
fn workspace_coordinator_drops_waiters_that_stopped_retrying() {
    let coordinator = WorkspaceGitOperationCoordinator::default();
    let repository = PathBuf::from("/tmp/magi-git-coordinator-stale/.git");
    coordinator.register_waiter("session-gone", &repository);
    coordinator.cancel_waiter("session-gone");
    coordinator
        .begin_execution("session-live", &repository)
        .expect("cancelled waiter must not block");
    coordinator.end_execution("session-live");
    coordinator.register_waiter("session-gone", &repository);
    coordinator
        .state
        .lock()
        .expect("lock")
        .waiters
        .iter_mut()
        .for_each(|waiter| {
            waiter.refreshed_at = std::time::Instant::now()
                - EXECUTION_WAITER_STALE_AFTER
                - std::time::Duration::from_secs(1);
        });
    coordinator
        .begin_execution("session-live", &repository)
        .expect("stale waiter must not block");
}

#[test]
fn workspace_coordinator_closes_turn_mutation_race() {
    let coordinator = WorkspaceGitOperationCoordinator::default();
    let repository = PathBuf::from("/tmp/magi-git-coordinator-repo/.git");
    coordinator
        .begin_execution("session-1", &repository)
        .expect("execution lease");
    let parallel_session = coordinator
        .begin_execution("session-parallel", &repository)
        .expect_err("sessions sharing one worktree must execute serially");
    assert!(matches!(
        parallel_session,
        GitCoordinationError::ExecutionActive { .. }
    ));
    let blocked = match coordinator.begin_mutation(&repository) {
        Ok(_) => panic!("mutation must not overlap execution"),
        Err(error) => error,
    };
    assert!(matches!(
        blocked,
        GitCoordinationError::ExecutionActive { .. }
    ));

    coordinator.end_execution("session-1");
    let mutation = coordinator
        .begin_mutation(&repository)
        .expect("mutation lease");
    let blocked_execution = coordinator
        .begin_execution("session-2", &repository)
        .expect_err("execution must not overlap mutation");
    assert!(matches!(
        blocked_execution,
        GitCoordinationError::MutationActive { .. }
    ));
    drop(mutation);
    coordinator
        .begin_execution("session-2", &repository)
        .expect("execution after mutation");
}
