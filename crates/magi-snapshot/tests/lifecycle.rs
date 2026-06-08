//! 端到端测试：session 启动、变更追踪、approve/revert、contentKind 分支。
//!
//! 注意：测试直接调 `session.reconcile()` 把磁盘状态同步进账本。
//! 在生产环境中，watcher 会异步推进同一组 record_upsert/record_removal 路径，
//! 但 macOS sandbox / 一些 CI 环境无法投递 FSEvents，所以测试用 reconcile
//! 显式驱动同一段代码以保证可重现。Watcher 自身在 docs 与生产 wiring 中验证。

use magi_snapshot::{ChangeKind, ContentKind, SnapshotManager, SourceKind, ToolHook, ToolHookCtx};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;

#[cfg(unix)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_scan_records_existing_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("a.txt"), "hello").unwrap();
    fs::write(root.join("b.md"), "world").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s1".into(), root.clone()).await.unwrap();

    let pending = session.pending_changes().unwrap();
    assert!(pending.is_empty(), "fresh baseline should have no pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn start_session_is_idempotent_for_existing_session() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("a.txt"), "hello").unwrap();

    let mgr = SnapshotManager::new();
    let first = mgr
        .start_session("s-idempotent".into(), root.clone())
        .await
        .unwrap();
    let second = mgr
        .start_session("s-idempotent".into(), root.clone())
        .await
        .unwrap();

    assert!(
        Arc::ptr_eq(&first, &second),
        "同一 session 重复启动必须复用内存账本，避免重复 watcher 与账本覆盖"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn start_session_rejects_same_session_id_for_different_workspace() {
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let root_a = dir_a.path().to_path_buf();
    let root_b = dir_b.path().to_path_buf();
    fs::write(root_a.join("a.txt"), "a").unwrap();
    fs::write(root_b.join("b.txt"), "b").unwrap();

    let mgr = SnapshotManager::new();
    let first = mgr
        .start_session("s-workspace-bound".into(), root_a.clone())
        .await
        .unwrap();
    let second = mgr
        .start_session("s-workspace-bound".into(), root_b.clone())
        .await;

    assert!(second.is_err(), "同名 session 不得复用到另一个 workspace");
    assert!(
        mgr.get_session_for_workspace("s-workspace-bound", &root_a)
            .is_some(),
        "原 workspace 的快照账本仍应可按 workspace 命中"
    );
    assert!(
        mgr.get_session_for_workspace("s-workspace-bound", &root_b)
            .is_none(),
        "错误 workspace 不应命中已存在账本"
    );
    assert_eq!(first.workspace_root(), root_a.canonicalize().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn external_write_is_captured() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("foo.txt"), "before").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s2".into(), root.clone()).await.unwrap();

    fs::write(root.join("foo.txt"), "after").unwrap();
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    assert_eq!(pending.len(), 1);
    let change = &pending[0];
    assert_eq!(change.path, "foo.txt");
    assert_eq!(change.change_kind, ChangeKind::Modified);
    assert_eq!(change.content_kind, ContentKind::Text);
    // reconcile 缺失 ToolHookCtx 时归因到 External
    assert_eq!(change.source, SourceKind::External);
    assert!(change.original_content.as_deref() == Some("before"));
    assert!(change.preview_content.as_deref() == Some("after"));
    assert!(change.unified_diff.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_file_creation_records_added() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s3".into(), root.clone()).await.unwrap();

    fs::write(root.join("new.txt"), "shiny").unwrap();
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    let new_change = pending
        .iter()
        .find(|c| c.path == "new.txt")
        .expect("new.txt should appear");
    assert_eq!(new_change.change_kind, ChangeKind::Added);
    assert_eq!(new_change.preview_content.as_deref(), Some("shiny"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_runtime_artifacts_do_not_enter_pending_changes() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join(".gitignore"), "web/dist/\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn original() {}\n").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr
        .start_session("s-ignored-runtime-artifacts".into(), root.clone())
        .await
        .unwrap();

    fs::create_dir_all(root.join("target/debug/.fingerprint/pkg")).unwrap();
    fs::write(
        root.join("target/debug/.fingerprint/pkg/lib-pkg.json"),
        "{}",
    )
    .unwrap();
    fs::create_dir_all(root.join("web/dist/assets")).unwrap();
    fs::write(root.join("web/dist/assets/app.js"), "compiled").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();

    session.reconcile().unwrap();
    let pending = session.pending_changes().unwrap();
    let paths = pending
        .iter()
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        paths,
        vec!["src/lib.rs"],
        "snapshot pending changes 不应暴露 target 或 .gitignore 产物"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deletion_records_deleted() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("dead.txt"), "rip").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s4".into(), root.clone()).await.unwrap();

    fs::remove_file(root.join("dead.txt")).unwrap();
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, "dead.txt");
    assert_eq!(pending[0].change_kind, ChangeKind::Deleted);
    assert_eq!(pending[0].original_content.as_deref(), Some("rip"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_collapses_delete_and_add_by_blob_hash() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("old.txt"), "same content").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr
        .start_session("s4-rename".into(), root.clone())
        .await
        .unwrap();

    fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, "new.txt");
    assert_eq!(pending[0].old_path.as_deref(), Some("old.txt"));
    assert_eq!(pending[0].change_kind, ChangeKind::Renamed);
    assert_eq!(pending[0].preview_content.as_deref(), Some("same content"));
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_rename_advances_both_sides_of_pair() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("old.txt"), "pair").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr
        .start_session("s4-rename-approve".into(), root.clone())
        .await
        .unwrap();

    fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
    session.reconcile().unwrap();
    assert_eq!(session.pending_changes().unwrap().len(), 1);

    // 仅传入 rename 的新路径，session 内部应展开旧路径并一同 approve。
    session.approve(&["new.txt".into()]).unwrap();
    assert!(
        session.pending_changes().unwrap().is_empty(),
        "approve rename should clear both old/new sides"
    );

    // 旧路径不应再出现在 baseline，随后写入旧路径应被识别为新增。
    fs::write(root.join("old.txt"), "pair").unwrap();
    session.reconcile().unwrap();
    let pending = session.pending_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, "old.txt");
    assert_eq!(pending[0].change_kind, ChangeKind::Added);
}

#[tokio::test(flavor = "multi_thread")]
async fn revert_rename_restores_old_path_and_removes_new_path() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("old.txt"), "pair").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr
        .start_session("s4-rename-revert".into(), root.clone())
        .await
        .unwrap();

    fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
    session.reconcile().unwrap();
    assert_eq!(session.pending_changes().unwrap().len(), 1);

    // 仅传入新路径应同时删除 new.txt 并恢复 old.txt。
    session.revert(&["new.txt".into()]).unwrap();
    session.reconcile().unwrap();
    assert!(!root.join("new.txt").exists());
    assert_eq!(fs::read_to_string(root.join("old.txt")).unwrap(), "pair");
    assert!(session.pending_changes().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_advances_baseline() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("x.txt"), "v1").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s5".into(), root.clone()).await.unwrap();

    fs::write(root.join("x.txt"), "v2").unwrap();
    session.reconcile().unwrap();
    assert_eq!(session.pending_changes().unwrap().len(), 1);

    session.approve(&["x.txt".into()]).unwrap();
    let pending = session.pending_changes().unwrap();
    assert!(pending.is_empty(), "approve should clear pending");

    fs::write(root.join("x.txt"), "v3").unwrap();
    session.reconcile().unwrap();
    let pending = session.pending_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].original_content.as_deref(), Some("v2"));
    assert_eq!(pending[0].preview_content.as_deref(), Some("v3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn revert_restores_baseline_content() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("y.txt"), "original").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s6".into(), root.clone()).await.unwrap();

    fs::write(root.join("y.txt"), "edited").unwrap();
    session.reconcile().unwrap();
    assert_eq!(session.pending_changes().unwrap().len(), 1);

    session.revert(&["y.txt".into()]).unwrap();
    session.reconcile().unwrap();
    assert_eq!(fs::read_to_string(root.join("y.txt")).unwrap(), "original");
    assert!(session.pending_changes().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn revert_added_file_removes_it() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s7".into(), root.clone()).await.unwrap();

    fs::write(root.join("brand_new.txt"), "added").unwrap();
    session.reconcile().unwrap();
    assert_eq!(session.pending_changes().unwrap().len(), 1);

    session.revert(&["brand_new.txt".into()]).unwrap();
    session.reconcile().unwrap();
    assert!(!root.join("brand_new.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_file_does_not_inline_content() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("img.png"), [0u8; 16]).unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s8".into(), root.clone()).await.unwrap();

    let mut bin: Vec<u8> = vec![0u8; 16];
    bin.extend_from_slice(&[1, 2, 3, 4, 5]);
    fs::write(root.join("img.png"), &bin).unwrap();
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    let img = pending
        .iter()
        .find(|c| c.path == "img.png")
        .expect("png change should appear");
    assert_eq!(img.content_kind, ContentKind::Binary);
    assert!(img.original_content.is_none());
    assert!(img.preview_content.is_none());
    assert!(img.unified_diff.is_none());
    assert!(img.size > 0);
}

#[cfg(any(unix, windows))]
#[tokio::test(flavor = "multi_thread")]
async fn symlink_recorded_with_target_only() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("real.txt"), "data").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s9".into(), root.clone()).await.unwrap();

    if let Err(error) = create_file_symlink(&root.join("real.txt"), &root.join("link.txt")) {
        #[cfg(windows)]
        {
            eprintln!("skip symlink test on Windows without symlink privilege: {error}");
            return;
        }
        #[cfg(unix)]
        panic!("create file symlink failed: {error}");
    }
    session.reconcile().unwrap();

    let pending = session.pending_changes().unwrap();
    let link = pending
        .iter()
        .find(|c| c.path == "link.txt")
        .expect("link should appear");
    assert_eq!(link.content_kind, ContentKind::Symlink);
    assert!(link.symlink_target.is_some());
    assert!(link.original_content.is_none());
    assert!(link.preview_content.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_hook_attribution_is_recorded() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("z.txt"), "v1").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s10".into(), root.clone()).await.unwrap();

    let ctx = ToolHookCtx {
        tool_call_id: "call-42".into(),
        worker_id: Some("w-7".into()),
        execution_group_id: Some("eg-1".into()),
        declared_paths: vec![PathBuf::from("z.txt")],
    };

    session.before_tool(&ctx);
    fs::write(root.join("z.txt"), "v2").unwrap();
    session.after_tool(&ctx);

    let pending = session.pending_changes().unwrap();
    let z = pending.iter().find(|c| c.path == "z.txt").unwrap();
    assert_eq!(z.source, SourceKind::Tool);
    assert_eq!(z.tool_call_id.as_deref(), Some("call-42"));
    assert_eq!(z.worker_id.as_deref(), Some("w-7"));
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_idempotent() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("a.txt"), "v1").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s11".into(), root.clone()).await.unwrap();

    fs::write(root.join("a.txt"), "v2").unwrap();
    session.reconcile().unwrap();

    let n1 = session.approve(&["a.txt".into()]).unwrap();
    let n2 = session.approve(&["a.txt".into()]).unwrap();
    assert!(n1 >= 1);
    assert_eq!(session.pending_changes().unwrap().len(), 0);
    assert!(n2 <= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_session_cleans_disk_state() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("a.txt"), "v1").unwrap();
    let canon = std::fs::canonicalize(&root).unwrap();

    let mgr = SnapshotManager::new();
    let _session = mgr.start_session("s12".into(), root.clone()).await.unwrap();

    let session_dir = canon.join(".magi/snapshots/index/s12");
    assert!(session_dir.exists());

    mgr.drop_session("s12").await.unwrap();
    assert!(!session_dir.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn reconcile_catches_missed_events() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("init.txt"), "init").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s13".into(), root.clone()).await.unwrap();

    // 关闭 watcher 模拟漏事件场景。
    session.archive().await;
    fs::write(root.join("missed.txt"), "should be reconciled").unwrap();
    fs::write(root.join("init.txt"), "modified").unwrap();

    session.reconcile().unwrap();
    let pending = session.pending_changes().unwrap();
    assert!(pending.iter().any(|c| c.path == "missed.txt"));
    assert!(pending.iter().any(|c| c.path == "init.txt"));
}
