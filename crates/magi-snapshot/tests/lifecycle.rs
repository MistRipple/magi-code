//! 端到端测试：session 启动、变更追踪、approve/revert、contentKind 分支。
//!
//! 注意：测试直接调 `session.reconcile()` 把磁盘状态同步进账本。
//! 在生产环境中，watcher 会异步推进同一组 record_upsert/record_removal 路径，
//! 但 macOS sandbox / 一些 CI 环境无法投递 FSEvents，所以测试用 reconcile
//! 显式驱动同一段代码以保证可重现。Watcher 自身在 docs 与生产 wiring 中验证。

use magi_snapshot::{ChangeKind, ContentKind, SnapshotManager, SourceKind, ToolHook, ToolHookCtx};
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

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

#[tokio::test(flavor = "multi_thread")]
async fn symlink_recorded_with_target_only() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(root.join("real.txt"), "data").unwrap();

    let mgr = SnapshotManager::new();
    let session = mgr.start_session("s9".into(), root.clone()).await.unwrap();

    std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
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
