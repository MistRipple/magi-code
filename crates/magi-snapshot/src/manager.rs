use crate::blob_store::BlobStore;
use crate::error::{SnapshotError, SnapshotResult};
use crate::scan::SnapshotPathFilter;
use crate::session::SnapshotSession;
use crate::watcher::{DebouncedEvent, FsWatcher};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock, broadcast, mpsc};

const WORKSPACE_WATCH_EVENT_CAPACITY: usize = 2_048;

struct WorkspaceWatcher {
    _watcher: FsWatcher,
    events: broadcast::Sender<DebouncedEvent>,
    forwarder: tokio::task::JoinHandle<()>,
}

impl WorkspaceWatcher {
    fn start(workspace_root: &Path, respect_gitignore: bool) -> SnapshotResult<Self> {
        let path_filter = SnapshotPathFilter::new(workspace_root, respect_gitignore);
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
        let watcher = FsWatcher::start(
            workspace_root,
            Arc::new(path_filter.excluded_prefixes()),
            raw_tx,
        )?;
        let (events, _) = broadcast::channel(WORKSPACE_WATCH_EVENT_CAPACITY);
        let forward_events = events.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                let _ = forward_events.send(event);
            }
        });
        Ok(Self {
            _watcher: watcher,
            events,
            forwarder,
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<DebouncedEvent> {
        self.events.subscribe()
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

/// 顶层管理：跨 session 共享 BlobStore，按 session_id + 实际工作根维持 SnapshotSession 实例。
/// session_id 保持原会话身份；实际根分别拥有基线与变更账本，同名相对路径不会混淆。
/// 项目/源目录授权由调用方验证，本层只拥有账本与实际目录的关联。
///
/// 每个 workspace 有自己的 `.magi/snapshots/blobs` 目录，blob 在 workspace 内跨 session 共享。
///
/// `blob_stores` 因为构造涉及磁盘 I/O，使用 async RwLock。
/// `sessions` 仅做 HashMap 查找，使用 std::sync::RwLock 让查询路径不需要 async 上下文，
/// 便于 sync 投影（bootstrap、HTTP 同步处理器）按需取出 SnapshotSession。
pub struct SnapshotManager {
    blob_stores: AsyncRwLock<HashMap<PathBuf, Arc<BlobStore>>>,
    workspace_watchers: StdRwLock<HashMap<PathBuf, Arc<WorkspaceWatcher>>>,
    sessions: StdRwLock<HashMap<SnapshotKey, Arc<SnapshotSession>>>,
    start_lock: AsyncMutex<()>,
    storage_root: Option<Arc<SnapshotStorageRootResolver>>,
}

pub type SnapshotStorageRootResolver = dyn Fn(&str, &Path) -> SnapshotResult<PathBuf> + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SnapshotKey {
    session_id: String,
    workspace_root: PathBuf,
}

impl SnapshotKey {
    fn new(session_id: impl Into<String>, workspace_root: &Path) -> Self {
        Self {
            session_id: session_id.into(),
            workspace_root: workspace_root.to_path_buf(),
        }
    }
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            blob_stores: AsyncRwLock::new(HashMap::new()),
            workspace_watchers: StdRwLock::new(HashMap::new()),
            sessions: StdRwLock::new(HashMap::new()),
            start_lock: AsyncMutex::new(()),
            storage_root: None,
        }
    }

    /// 宿主拥有项目/会话身份与固定状态布局；本层只消费已解析的存储目录。
    pub fn with_storage_root_resolver(
        mut self,
        resolver: Arc<SnapshotStorageRootResolver>,
    ) -> Self {
        self.storage_root = Some(resolver);
        self
    }

    /// 启动新 session（或在已有 session 上恢复）。
    pub async fn start_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(&workspace_root)?;
        let _start_guard = self.start_lock.lock().await;
        tracing::info!(%session_id, stage = "start_lock_acquired", "snapshot preparation");
        if let Some(session) = self.get_session_for_workspace(&session_id, &workspace_root) {
            self.resume_session(&session).await?;
            return Ok(session);
        }
        self.create_session(session_id, workspace_root).await
    }

    async fn resume_session(&self, session: &Arc<SnapshotSession>) -> SnapshotResult<()> {
        let respect_gitignore = session.workspace_root().join(".git").is_dir();
        let resumed = session
            .resume(
                self.workspace_watcher(session.workspace_root(), respect_gitignore)?
                    .subscribe(),
            )
            .await;
        if resumed {
            session.reconcile_async().await?;
        }
        Ok(())
    }

    /// Git branch/HEAD 被结构化操作切换后，以新的代码树重建该 session 的快照基线。
    ///
    /// 调用方必须已经持有 workspace Git mutation/turn 排他权。这里继续使用 start_lock，
    /// 避免与 lazy start 并发创建两个 watcher 或恢复旧 baseline。
    pub async fn rebase_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(&workspace_root)?;
        let _start_guard = self.start_lock.lock().await;
        if let Some(session) = self.get_session_for_workspace(&session_id, &workspace_root) {
            session.drop_session().await?;
            self.sessions
                .write()
                .expect("snapshot sessions registry poisoned")
                .remove(&SnapshotKey::new(&session_id, &workspace_root));
        }
        self.create_session(session_id, workspace_root).await
    }

    async fn create_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        let legacy_root = snapshots_root_for(&workspace_root);
        let snapshots_root = match self.storage_root.as_ref() {
            Some(resolve) => resolve(&session_id, &workspace_root)?,
            None => legacy_root.clone(),
        };
        if snapshots_root == legacy_root {
            ensure_workspace_state_git_excluded(&workspace_root)?;
        } else {
            if legacy_root.exists() {
                let canonical = std::fs::canonicalize(&legacy_root)
                    .map_err(|error| SnapshotError::io(&legacy_root, error))?;
                if !canonical.starts_with(&workspace_root)
                    || std::fs::symlink_metadata(&legacy_root)
                        .map_err(|error| SnapshotError::io(&legacy_root, error))?
                        .file_type()
                        .is_symlink()
                {
                    return Err(SnapshotError::InvalidRoot(
                        "旧快照存储越出源目录或包含根符号链接".into(),
                    ));
                }
            }
            let from = legacy_root;
            let to = snapshots_root.clone();
            tokio::task::spawn_blocking(move || migrate_snapshot_storage(&from, &to))
                .await
                .map_err(|error| {
                    SnapshotError::Internal(format!("snapshot storage migration failed: {error}"))
                })??;
        }
        let blobs_dir = snapshots_root.join("blobs");

        let blobs = {
            let read = self.blob_stores.read().await;
            if let Some(s) = read.get(&snapshots_root) {
                s.clone()
            } else {
                drop(read);
                let mut write = self.blob_stores.write().await;
                if let Some(s) = write.get(&snapshots_root) {
                    s.clone()
                } else {
                    let store = Arc::new(BlobStore::new(&blobs_dir)?);
                    write.insert(snapshots_root.clone(), store.clone());
                    store
                }
            }
        };

        let respect_gitignore = workspace_root.join(".git").is_dir();
        let watcher_events = self
            .workspace_watcher(&workspace_root, respect_gitignore)?
            .subscribe();
        tracing::info!(%session_id, stage = "watcher_ready", "snapshot preparation");
        let session = SnapshotSession::start(
            session_id.clone(),
            workspace_root.clone(),
            blobs,
            snapshots_root,
            respect_gitignore,
            watcher_events,
        )
        .await?;

        self.sessions
            .write()
            .expect("snapshot sessions registry poisoned")
            .insert(
                SnapshotKey::new(&session_id, &workspace_root),
                session.clone(),
            );
        Ok(session)
    }

    fn workspace_watcher(
        &self,
        workspace_root: &Path,
        respect_gitignore: bool,
    ) -> SnapshotResult<Arc<WorkspaceWatcher>> {
        if let Some(watcher) = self
            .workspace_watchers
            .read()
            .expect("workspace watchers registry poisoned")
            .get(workspace_root)
            .cloned()
        {
            return Ok(watcher);
        }
        let watcher = Arc::new(WorkspaceWatcher::start(workspace_root, respect_gitignore)?);
        self.workspace_watchers
            .write()
            .expect("workspace watchers registry poisoned")
            .insert(workspace_root.to_path_buf(), watcher.clone());
        Ok(watcher)
    }

    /// 旧单根读入口仅在结果唯一时可用；多根会话必须显式指定实际目录。
    pub fn get_session(&self, session_id: &str) -> Option<Arc<SnapshotSession>> {
        let sessions = self
            .sessions
            .read()
            .expect("snapshot sessions registry poisoned");
        let mut matches = sessions
            .iter()
            .filter(|(key, _)| key.session_id == session_id);
        let (_, session) = matches.next()?;
        matches.next().is_none().then(|| Arc::clone(session))
    }

    pub fn sessions_for_session(&self, session_id: &str) -> Vec<Arc<SnapshotSession>> {
        let mut sessions = self
            .sessions
            .read()
            .expect("snapshot sessions registry poisoned")
            .iter()
            .filter(|(key, _)| key.session_id == session_id)
            .map(|(_, session)| Arc::clone(session))
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| a.workspace_root().cmp(b.workspace_root()));
        sessions
    }

    pub fn get_session_for_workspace(
        &self,
        session_id: &str,
        workspace_root: &Path,
    ) -> Option<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(workspace_root).ok()?;
        self.sessions
            .read()
            .expect("snapshot sessions registry poisoned")
            .get(&SnapshotKey::new(session_id, &workspace_root))
            .cloned()
    }

    /// 关闭该会话全部目录的 watcher，保留磁盘账本及 blob 所有权。
    /// 已归档账本仍是持久数据的拥有方，不能释放其最后一份 baseline blob。
    pub async fn archive_session(&self, session_id: &str) -> SnapshotResult<()> {
        let _start_guard = self.start_lock.lock().await;
        let sessions = self.sessions_for_session(session_id);
        if sessions.is_empty() {
            return Err(SnapshotError::SessionNotFound(session_id.to_string()));
        }
        for session in sessions {
            session.archive().await;
        }
        Ok(())
    }

    /// 只删除一个实际目录的账本。切换/丢弃副本不能删除同会话其它目录成果。
    pub async fn drop_session_for_workspace(
        &self,
        session_id: &str,
        workspace_root: &Path,
    ) -> SnapshotResult<()> {
        let _start_guard = self.start_lock.lock().await;
        let workspace_root = canonical_workspace_root(workspace_root)?;
        let session = self
            .get_session_for_workspace(session_id, &workspace_root)
            .ok_or_else(|| SnapshotError::SessionNotFound(session_id.to_string()))?;
        session.drop_session().await?;
        self.sessions
            .write()
            .expect("snapshot sessions registry poisoned")
            .remove(&SnapshotKey::new(session_id, &workspace_root));
        Ok(())
    }

    /// 删除该会话全部已装载目录的账本，并释放 blob 引用。
    pub async fn drop_session(&self, session_id: &str) -> SnapshotResult<()> {
        let _start_guard = self.start_lock.lock().await;
        let sessions = self.sessions_for_session(session_id);
        if sessions.is_empty() {
            return Err(SnapshotError::SessionNotFound(session_id.to_string()));
        }
        for session in sessions {
            session.drop_session().await?;
            self.sessions
                .write()
                .expect("snapshot sessions registry poisoned")
                .remove(&SnapshotKey::new(session_id, session.workspace_root()));
        }
        Ok(())
    }
}

/// `.magi` 下的会话、快照和索引都属于 Magi 本地运行态，
/// 不能反过来把用户仓库标成 dirty。
///
/// 只写 repository-local 的 `info/exclude`，不改用户受版本控制的 `.gitignore`；
/// linked worktree 则沿 `commondir` 定位共享 Git 目录。若这些路径中的文件
/// 已经被显式跟踪，Git 仍会正常报告改动，避免隐藏用户真实文件。
fn ensure_workspace_state_git_excluded(workspace_root: &Path) -> SnapshotResult<()> {
    let dot_git = workspace_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let marker = std::fs::read_to_string(&dot_git)
            .map_err(|error| SnapshotError::io(&dot_git, error))?;
        let raw = marker
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SnapshotError::InvalidRoot(format!(
                    "invalid linked-worktree .git marker: {}",
                    dot_git.display()
                ))
            })?;
        resolve_relative_path(workspace_root, Path::new(raw))
    } else {
        return Ok(());
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(raw) => resolve_relative_path(&git_dir, Path::new(raw.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir,
        Err(error) => return Err(SnapshotError::io(git_dir.join("commondir"), error)),
    };
    let exclude_path = common_dir.join("info").join("exclude");
    let existing = match std::fs::read_to_string(&exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SnapshotError::io(&exclude_path, error)),
    };
    const PATTERNS: [&str; 1] = ["/.magi/"];
    let missing = PATTERNS
        .into_iter()
        .filter(|pattern| !existing.lines().any(|line| line.trim() == *pattern))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SnapshotError::io(parent, error))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")
            .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    }
    for pattern in missing {
        file.write_all(format!("{pattern}\n").as_bytes())
            .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    }
    file.sync_all()
        .map_err(|error| SnapshotError::io(&exclude_path, error))
}

fn resolve_relative_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// 旧目录仅作为迁移输入。先完整复制到本次独占的临时目录，再原子发布；
/// 失败不留下半迁移目标，不删除或覆盖旧数据。
fn migrate_snapshot_storage(from: &Path, to: &Path) -> SnapshotResult<()> {
    use sha2::{Digest, Sha256};
    const MANIFEST: &str = "storage-migration-v1.json";
    if to.starts_with(from) || from.starts_with(to) {
        return Err(SnapshotError::InvalidRoot(
            "新旧快照存储目录不能重叠".into(),
        ));
    }
    let source_identity = Sha256::digest(from.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let manifest = serde_json::json!({"version": 1, "sourceIdentity": source_identity});
    if !to.is_absolute() {
        return Err(SnapshotError::InvalidRoot(
            "snapshot storage root must be absolute".into(),
        ));
    }
    if to.is_dir() {
        let path = to.join(MANIFEST);
        let actual: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| SnapshotError::io(&path, error))?,
        )?;
        if actual != manifest {
            return Err(SnapshotError::InvalidRoot("快照存储迁移身份不匹配".into()));
        }
        return Ok(());
    }
    if to.exists() {
        return Err(SnapshotError::InvalidRoot(
            "snapshot storage root is not a directory".into(),
        ));
    }
    let parent = to
        .parent()
        .ok_or_else(|| SnapshotError::InvalidRoot("snapshot storage parent missing".into()))?;
    std::fs::create_dir_all(parent).map_err(|error| SnapshotError::io(parent, error))?;
    static NEXT_MIGRATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = NEXT_MIGRATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(
        ".snapshot-migration-{}-{time}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&staging).map_err(|error| SnapshotError::io(&staging, error))?;
    let result = (|| {
        if from.exists() {
            copy_snapshot_directory(from, &staging)?;
        }
        let manifest_path = staging.join(MANIFEST);
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&manifest_path)
            .map_err(|error| SnapshotError::io(&manifest_path, error))?;
        file.write_all(&serde_json::to_vec(&manifest)?)
            .and_then(|_| file.sync_all())
            .map_err(|error| SnapshotError::io(&manifest_path, error))?;
        #[cfg(unix)]
        std::fs::File::open(&staging)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| SnapshotError::io(&staging, error))?;
        std::fs::rename(&staging, to).map_err(|error| SnapshotError::io(to, error))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| SnapshotError::io(parent, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn copy_snapshot_directory(from: &Path, to: &Path) -> SnapshotResult<()> {
    for entry in std::fs::read_dir(from).map_err(|error| SnapshotError::io(from, error))? {
        let entry = entry.map_err(|error| SnapshotError::io(from, error))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| SnapshotError::io(&source, error))?;
        if kind.is_dir() {
            std::fs::create_dir(&target).map_err(|error| SnapshotError::io(&target, error))?;
            copy_snapshot_directory(&source, &target)?;
            #[cfg(unix)]
            std::fs::File::open(&target)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| SnapshotError::io(&target, error))?;
        } else if kind.is_file() {
            std::fs::copy(&source, &target).map_err(|error| SnapshotError::io(&target, error))?;
            std::fs::File::open(&target)
                .and_then(|file| file.sync_all())
                .map_err(|error| SnapshotError::io(&target, error))?;
        } else {
            return Err(SnapshotError::InvalidRoot(
                "旧快照目录包含符号链接或特殊文件，无法安全迁移".into(),
            ));
        }
    }
    Ok(())
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn snapshots_root_for(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".magi").join("snapshots")
}

fn canonical_workspace_root(workspace_root: &Path) -> SnapshotResult<PathBuf> {
    if !workspace_root.is_absolute() {
        return Err(SnapshotError::InvalidRoot(format!(
            "workspace_root must be absolute: {}",
            workspace_root.display()
        )));
    }
    if !workspace_root.is_dir() {
        return Err(SnapshotError::InvalidRoot(format!(
            "workspace_root not a directory: {}",
            workspace_root.display()
        )));
    }
    std::fs::canonicalize(workspace_root).map_err(|error| SnapshotError::io(workspace_root, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, args: &[&str]) -> std::process::Output {
        magi_process::std_command("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command should run")
    }

    #[tokio::test]
    async fn managed_storage_migrates_original_baseline_once_and_preserves_source_data() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("same.txt"), "original").unwrap();
        let legacy = SnapshotManager::new();
        let old = legacy
            .start_session("migrated".into(), source.path().into())
            .await
            .unwrap();
        std::fs::write(source.path().join("same.txt"), "changed").unwrap();
        old.reconcile().unwrap();
        old.archive().await;
        let old_baseline = std::fs::read(
            source
                .path()
                .join(".magi/snapshots/index/migrated/baseline.json"),
        )
        .unwrap();
        let target = state.path().join("project/snapshots");
        let manager = || {
            let target = target.clone();
            SnapshotManager::new()
                .with_storage_root_resolver(Arc::new(move |_, _| Ok(target.clone())))
        };
        let current = manager();
        let snapshot = current
            .start_session("migrated".into(), source.path().into())
            .await
            .unwrap();
        let pending = snapshot.pending_changes().unwrap();
        assert_eq!(pending[0].original_content.as_deref(), Some("original"));
        assert_eq!(pending[0].preview_content.as_deref(), Some("changed"));
        assert!(target.join("index/migrated/baseline.json").exists());
        snapshot.approve(&["same.txt".into()]).unwrap();
        current.archive_session("migrated").await.unwrap();
        let restarted = manager();
        let restored = restarted
            .start_session("migrated".into(), source.path().into())
            .await
            .unwrap();
        assert!(
            restored.pending_changes().unwrap().is_empty(),
            "已提交目标不能再次从旧数据覆盖"
        );
        assert_eq!(
            std::fs::read(
                source
                    .path()
                    .join(".magi/snapshots/index/migrated/baseline.json")
            )
            .unwrap(),
            old_baseline
        );
        assert_eq!(old.pending_changes().unwrap().len(), 1);
        restored.archive().await;
        std::fs::write(target.join("storage-migration-v1.json"), "{}").unwrap();
        assert!(
            manager()
                .start_session("migrated".into(), source.path().into())
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_external_link_without_publishing_partial_target() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), source.path().join("blobs")).unwrap();
        let destination = target.path().join("snapshots");
        assert!(migrate_snapshot_storage(source.path(), &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read_dir(target.path()).unwrap().count(),
            0,
            "自有 staging 应清理"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret")).unwrap(),
            "keep"
        );
        std::fs::remove_file(source.path().join("blobs")).unwrap();
        migrate_snapshot_storage(source.path(), &destination).unwrap();
        assert!(destination.join("storage-migration-v1.json").exists());
    }

    #[test]
    fn workspace_state_is_excluded_from_repository_status() {
        let workspace = tempfile::tempdir().expect("workspace");
        assert!(
            run_git(workspace.path(), &["init", "--quiet"])
                .status
                .success()
        );
        std::fs::create_dir_all(workspace.path().join(".magi/cache")).expect("state dir");
        std::fs::write(workspace.path().join(".magi/sessions.json"), "{}\n")
            .expect("session state");
        std::fs::write(
            workspace.path().join(".magi/cache/search-index.json.gz"),
            b"cache",
        )
        .expect("cache state");

        ensure_workspace_state_git_excluded(workspace.path()).expect("exclude workspace state");

        let status = run_git(workspace.path(), &["status", "--porcelain", "--", ".magi"]);
        assert!(status.status.success());
        assert!(
            status.stdout.is_empty(),
            "Magi runtime state must not dirty the repository: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    #[test]
    fn workspace_state_exclusion_is_idempotent_and_preserves_existing_rules() {
        let workspace = tempfile::tempdir().expect("workspace");
        assert!(
            run_git(workspace.path(), &["init", "--quiet"])
                .status
                .success()
        );
        let exclude_path = workspace.path().join(".git/info/exclude");
        std::fs::write(&exclude_path, "existing-rule\n").expect("existing exclude");

        ensure_workspace_state_git_excluded(workspace.path()).expect("first exclude");
        ensure_workspace_state_git_excluded(workspace.path()).expect("second exclude");

        let exclude = std::fs::read_to_string(exclude_path).expect("read exclude");
        assert!(exclude.lines().any(|line| line == "existing-rule"));
        assert_eq!(exclude.lines().filter(|line| *line == "/.magi/").count(), 1);
    }

    #[tokio::test]
    async fn rebase_session_replaces_old_branch_baseline_with_current_tree() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("branch.txt"), "main\n").expect("main fixture");
        let manager = SnapshotManager::new();
        let original = manager
            .start_session(
                "session-git-rebase".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("start snapshot");

        std::fs::write(workspace.path().join("branch.txt"), "feature\n").expect("feature fixture");
        let rebased = manager
            .rebase_session(
                "session-git-rebase".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("rebase snapshot");

        assert!(!Arc::ptr_eq(&original, &rebased));
        assert!(
            rebased
                .pending_changes()
                .expect("pending changes")
                .is_empty(),
            "新 branch 当前文件树必须成为新 baseline"
        );
    }

    #[tokio::test]
    async fn sessions_in_same_workspace_share_one_recursive_watcher() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("shared.txt"), "base\n").expect("fixture");
        let manager = SnapshotManager::new();

        manager
            .start_session(
                "session-shared-watcher-a".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("start first snapshot");
        manager
            .start_session(
                "session-shared-watcher-b".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("start second snapshot");

        assert_eq!(
            manager
                .workspace_watchers
                .read()
                .expect("workspace watchers should lock")
                .len(),
            1
        );
    }
}
