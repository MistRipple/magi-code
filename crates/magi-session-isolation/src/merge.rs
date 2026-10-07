//! 把隔离副本里的改动合并回主工作区。
//!
//! 隔离会话的变更账本（`SnapshotSession`）以副本建立时的内容为基线，`pending_changes`
//! 就是这个会话相对基线做过的全部改动。合并按文件做三方比较：
//!
//! | 比较 | 结论 |
//! |---|---|
//! | 主工作区内容 == 副本内容 | 已经一致，只需推进基线 |
//! | 主工作区内容 == 基线 | 主工作区没动过，直接应用副本的改动 |
//! | 其它 | 两边都改过，作为冲突交给用户 |
//!
//! 计划（[`plan_merge`]）只读；应用（[`apply_merge`]）会重新计算计划，以应用时刻的磁盘为准。
//! 应用成功的文件会在副本账本里 `approve`，基线随之推进到合并后的内容，后续合并从这里继续。
use crate::clone::join_relative;
use crate::error::{IsolationError, IsolationResult};
use magi_snapshot::{ChangeKind, ContentKind, FileMeta, PendingChange, SnapshotSession};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeAction {
    Add,
    Modify,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// 主工作区没动过这个文件，可以直接应用。
    Clean,
    /// 主工作区已经是副本里的内容。
    AlreadyApplied,
    /// 两边都改过，需要用户决定。
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// 两边都新建了同名文件，内容不同。
    BothAdded,
    /// 主工作区也修改了这个文件。
    BothModified,
    /// 副本修改了这个文件，主工作区却把它删掉了。
    DeletedInSource,
    /// 副本删除了这个文件，主工作区却修改过它。
    ModifiedInSource,
    /// 特殊文件，或内容无法比较。
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeEntry {
    pub path: String,
    pub action: MergeAction,
    pub state: MergeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<ConflictKind>,
    pub content_kind: ContentKind,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePlan {
    pub entries: Vec<MergeEntry>,
}

impl MergePlan {
    pub fn count(&self, state: MergeState) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == state)
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// 用副本里的版本覆盖主工作区。
    UseSession,
    /// 保留主工作区的版本；这个改动继续留在副本里待处理。
    KeepSource,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSelection {
    /// 只合并这些路径；缺省表示全部。
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// 冲突文件的处理方式；没有给出处理方式的冲突不会被合并。
    #[serde(default)]
    pub resolutions: HashMap<String, ConflictResolution>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeOutcome {
    /// 写入（或删除）了主工作区文件的路径。
    pub applied: Vec<String>,
    /// 主工作区本来就是副本里的内容，只推进了基线的路径。
    pub already_applied: Vec<String>,
    /// 有冲突且没有给出处理方式（或选择保留主工作区）的路径，仍留在副本里。
    pub unresolved: Vec<String>,
    /// 应用失败的路径及原因，仍留在副本里。
    pub failed: Vec<MergeFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeFailure {
    pub path: String,
    pub error: String,
}

/// 一个路径在某一侧的内容状态，用于三方比较。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Side {
    Missing,
    File(String),
    Symlink(String),
    /// 目录、特殊文件，或无法得到内容摘要。
    Opaque,
}

fn side_of_meta(meta: Option<&FileMeta>) -> Side {
    let Some(meta) = meta else {
        return Side::Missing;
    };
    match meta.content_kind {
        ContentKind::Symlink => meta
            .symlink
            .as_ref()
            .map(|link| Side::Symlink(link.target.clone()))
            .unwrap_or(Side::Opaque),
        ContentKind::Special => Side::Opaque,
        _ => meta
            .content_hash
            .clone()
            .map(Side::File)
            .unwrap_or(Side::Opaque),
    }
}

fn side_of_disk(path: &Path) -> Side {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Side::Missing;
    };
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(|target| Side::Symlink(target.to_string_lossy().into_owned()))
            .unwrap_or(Side::Opaque);
    }
    if metadata.is_file() {
        return magi_snapshot::path_content_hash(path)
            .map(Side::File)
            .unwrap_or(Side::Opaque);
    }
    Side::Opaque
}

/// 一次改动展开成「对某个路径做什么」；重命名拆成删除旧路径加新增新路径。
struct Step<'a> {
    path: String,
    action: MergeAction,
    change: &'a PendingChange,
}

fn expand_changes(changes: &[PendingChange]) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    for change in changes {
        match change.change_kind {
            ChangeKind::Added => steps.push(Step {
                path: change.path.clone(),
                action: MergeAction::Add,
                change,
            }),
            ChangeKind::Modified => steps.push(Step {
                path: change.path.clone(),
                action: MergeAction::Modify,
                change,
            }),
            ChangeKind::Deleted => steps.push(Step {
                path: change.path.clone(),
                action: MergeAction::Delete,
                change,
            }),
            ChangeKind::Renamed => {
                if let Some(old_path) = change.old_path.as_ref() {
                    steps.push(Step {
                        path: old_path.clone(),
                        action: MergeAction::Delete,
                        change,
                    });
                }
                steps.push(Step {
                    path: change.path.clone(),
                    action: MergeAction::Add,
                    change,
                });
            }
        }
    }
    steps
}

fn classify(
    action: MergeAction,
    base: &Side,
    source: &Side,
    session: &Side,
) -> (MergeState, Option<ConflictKind>) {
    let comparable = |side: &Side| !matches!(side, Side::Opaque);
    match action {
        MergeAction::Add | MergeAction::Modify => {
            if !comparable(base) || !comparable(source) || !comparable(session) {
                // 三方里任何一方无法比较：只有「副本新增、主工作区没有」这种明确情形才放行。
                if matches!(base, Side::Missing)
                    && matches!(source, Side::Missing)
                    && comparable(session)
                {
                    return (MergeState::Clean, None);
                }
                return (MergeState::Conflict, Some(ConflictKind::Unsupported));
            }
            if source == session {
                (MergeState::AlreadyApplied, None)
            } else if source == base {
                (MergeState::Clean, None)
            } else if matches!(base, Side::Missing) {
                (MergeState::Conflict, Some(ConflictKind::BothAdded))
            } else if matches!(source, Side::Missing) {
                (MergeState::Conflict, Some(ConflictKind::DeletedInSource))
            } else {
                (MergeState::Conflict, Some(ConflictKind::BothModified))
            }
        }
        MergeAction::Delete => {
            if matches!(source, Side::Missing) {
                (MergeState::AlreadyApplied, None)
            } else if !comparable(base) || !comparable(source) {
                (MergeState::Conflict, Some(ConflictKind::Unsupported))
            } else if source == base {
                (MergeState::Clean, None)
            } else {
                (MergeState::Conflict, Some(ConflictKind::ModifiedInSource))
            }
        }
    }
}

/// 计算把隔离会话的全部改动合并回 `source_root` 会发生什么。只读，不改任何文件。
pub fn plan_merge(isolated: &SnapshotSession, source_root: &Path) -> IsolationResult<MergePlan> {
    let changes = isolated.pending_changes()?;
    let mut entries = Vec::new();
    for step in expand_changes(&changes) {
        let source_path = join_relative(source_root, &step.path)?;
        let session_path = join_relative(isolated.workspace_root(), &step.path)?;
        let base = side_of_meta(isolated.baseline_meta(&step.path).as_ref());
        let source = side_of_disk(&source_path);
        let session = match step.action {
            MergeAction::Delete => Side::Missing,
            _ => side_of_disk(&session_path),
        };
        let (state, conflict) = classify(step.action, &base, &source, &session);
        entries.push(MergeEntry {
            path: step.path,
            action: step.action,
            state,
            conflict,
            content_kind: step.change.content_kind,
            size: step.change.size,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(MergePlan { entries })
}

/// 应用合并。以应用时刻的磁盘重新计算计划，按 `selection` 选择要合并的路径和冲突处理方式。
pub fn apply_merge(
    isolated: &SnapshotSession,
    source_root: &Path,
    selection: &MergeSelection,
) -> IsolationResult<MergeOutcome> {
    let plan = plan_merge(isolated, source_root)?;
    let selected: Option<BTreeSet<&str>> = selection
        .paths
        .as_ref()
        .map(|paths| paths.iter().map(String::as_str).collect());
    let mut outcome = MergeOutcome::default();
    let mut settled = Vec::new();

    for entry in &plan.entries {
        if selected
            .as_ref()
            .is_some_and(|selected| !selected.contains(entry.path.as_str()))
        {
            continue;
        }
        let force = match entry.state {
            MergeState::Clean => false,
            MergeState::AlreadyApplied => {
                outcome.already_applied.push(entry.path.clone());
                settled.push(entry.path.clone());
                continue;
            }
            MergeState::Conflict => match selection.resolutions.get(&entry.path) {
                Some(ConflictResolution::UseSession) => true,
                Some(ConflictResolution::KeepSource) | None => {
                    outcome.unresolved.push(entry.path.clone());
                    continue;
                }
            },
        };
        match apply_entry(isolated, source_root, entry, force) {
            Ok(()) => {
                outcome.applied.push(entry.path.clone());
                settled.push(entry.path.clone());
            }
            Err(error) => outcome.failed.push(MergeFailure {
                path: entry.path.clone(),
                error: error.to_string(),
            }),
        }
    }

    if !settled.is_empty() {
        isolated.approve(&settled)?;
    }
    Ok(outcome)
}

fn apply_entry(
    isolated: &SnapshotSession,
    source_root: &Path,
    entry: &MergeEntry,
    force: bool,
) -> IsolationResult<()> {
    if entry.conflict == Some(ConflictKind::Unsupported) && force {
        return Err(IsolationError::UnsafePath(format!(
            "{}：不支持合并这种类型的文件",
            entry.path
        )));
    }
    let target = join_relative(source_root, &entry.path)?;
    match entry.action {
        MergeAction::Delete => remove_path(&target),
        MergeAction::Add | MergeAction::Modify => {
            let origin = join_relative(isolated.workspace_root(), &entry.path)?;
            write_from(&origin, &target)
        }
    }
}

fn remove_path(target: &Path) -> IsolationResult<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() => Err(IsolationError::io(
            "删除文件",
            target,
            std::io::Error::other("目标是目录"),
        )),
        Ok(_) => {
            fs::remove_file(target).map_err(|error| IsolationError::io("删除文件", target, error))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IsolationError::io("读取文件信息", target, error)),
    }
}

/// 以「写临时文件再改名」的方式把 `origin` 的内容写到 `target`，读者不会看到写到一半的文件。
fn write_from(origin: &Path, target: &Path) -> IsolationResult<()> {
    let metadata = fs::symlink_metadata(origin)
        .map_err(|error| IsolationError::io("读取副本文件", origin, error))?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| IsolationError::io("创建目录", parent, error))?;
    }
    if let Ok(existing) = fs::symlink_metadata(target)
        && existing.is_dir()
    {
        return Err(IsolationError::io(
            "写入文件",
            target,
            std::io::Error::other("目标是目录"),
        ));
    }
    let staging = staging_path(target);
    let result = if metadata.file_type().is_symlink() {
        let link = fs::read_link(origin)
            .map_err(|error| IsolationError::io("读取符号链接", origin, error))?;
        let _ = fs::remove_file(&staging);
        symlink(&link, &staging)
            .map_err(|error| IsolationError::io("创建符号链接", &staging, error))
    } else {
        fs::copy(origin, &staging)
            .map(|_| ())
            .map_err(|error| IsolationError::io("复制文件", origin, error))
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    fs::rename(&staging, target).map_err(|error| {
        let _ = fs::remove_file(&staging);
        IsolationError::io("替换文件", target, error)
    })
}

fn staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.magi-merge-{}", std::process::id()))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clone::create_isolated_copy;
    use magi_snapshot::SnapshotManager;
    use std::sync::Arc;

    struct Fixture {
        _source: tempfile::TempDir,
        _parent: tempfile::TempDir,
        source: PathBuf,
        copy: PathBuf,
        session: Arc<SnapshotSession>,
    }

    impl Fixture {
        async fn new(files: &[(&str, &str)]) -> Self {
            let source_dir = tempfile::tempdir().unwrap();
            for (path, content) in files {
                let full = source_dir.path().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, content).unwrap();
            }
            let parent = tempfile::tempdir().unwrap();
            let copy = parent.path().join("copy");
            create_isolated_copy(source_dir.path(), &copy).unwrap();
            let manager = SnapshotManager::new();
            let session = manager
                .start_session("isolated-session".to_string(), copy.clone())
                .await
                .unwrap();
            let source = fs::canonicalize(source_dir.path()).unwrap();
            let copy = fs::canonicalize(copy).unwrap();
            Self {
                _source: source_dir,
                _parent: parent,
                source,
                copy,
                session,
            }
        }

        fn edit_copy(&self, path: &str, content: &str) {
            let full = self.copy.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
            self.session.reconcile().unwrap();
        }

        fn delete_in_copy(&self, path: &str) {
            fs::remove_file(self.copy.join(path)).unwrap();
            self.session.reconcile().unwrap();
        }

        fn edit_source(&self, path: &str, content: &str) {
            let full = self.source.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }

        fn source_text(&self, path: &str) -> Option<String> {
            fs::read_to_string(self.source.join(path)).ok()
        }

        fn state_of(&self, plan: &MergePlan, path: &str) -> MergeState {
            plan.entries
                .iter()
                .find(|entry| entry.path == path)
                .unwrap_or_else(|| panic!("plan has no entry for {path}: {plan:?}"))
                .state
        }
    }

    #[tokio::test]
    async fn untouched_source_takes_session_changes_and_the_baseline_advances() {
        let fixture =
            Fixture::new(&[("a.txt", "a0\n"), ("b.txt", "b0\n"), ("src/c.txt", "c0\n")]).await;
        fixture.edit_copy("a.txt", "a1\n");
        fixture.edit_copy("new/d.txt", "d1\n");
        fixture.delete_in_copy("b.txt");

        let plan = plan_merge(&fixture.session, &fixture.source).unwrap();
        assert_eq!(plan.entries.len(), 3);
        assert_eq!(plan.count(MergeState::Clean), 3);
        // 计划是只读的。
        assert_eq!(fixture.source_text("a.txt").as_deref(), Some("a0\n"));

        let outcome = apply_merge(
            &fixture.session,
            &fixture.source,
            &MergeSelection::default(),
        )
        .unwrap();
        assert_eq!(outcome.applied.len(), 3);
        assert!(outcome.unresolved.is_empty() && outcome.failed.is_empty());
        assert_eq!(fixture.source_text("a.txt").as_deref(), Some("a1\n"));
        assert_eq!(fixture.source_text("new/d.txt").as_deref(), Some("d1\n"));
        assert_eq!(fixture.source_text("b.txt"), None);
        assert_eq!(fixture.source_text("src/c.txt").as_deref(), Some("c0\n"));

        // 基线推进：再计划一次没有任何待合并改动。
        assert!(
            plan_merge(&fixture.session, &fixture.source)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[tokio::test]
    async fn files_changed_on_both_sides_are_conflicts_and_stay_pending_until_resolved() {
        let fixture =
            Fixture::new(&[("a.txt", "a0\n"), ("b.txt", "b0\n"), ("c.txt", "c0\n")]).await;
        fixture.edit_copy("a.txt", "session-a\n");
        fixture.edit_copy("b.txt", "session-b\n");
        fixture.edit_copy("c.txt", "session-c\n");
        fixture.edit_source("a.txt", "source-a\n");
        fixture.edit_source("b.txt", "source-b\n");

        let plan = plan_merge(&fixture.session, &fixture.source).unwrap();
        assert_eq!(fixture.state_of(&plan, "a.txt"), MergeState::Conflict);
        assert_eq!(fixture.state_of(&plan, "b.txt"), MergeState::Conflict);
        assert_eq!(fixture.state_of(&plan, "c.txt"), MergeState::Clean);
        let conflict = plan
            .entries
            .iter()
            .find(|entry| entry.path == "a.txt")
            .unwrap();
        assert_eq!(conflict.conflict, Some(ConflictKind::BothModified));

        // 默认只应用干净的文件；冲突没有给出处理方式就不动。
        let outcome = apply_merge(
            &fixture.session,
            &fixture.source,
            &MergeSelection::default(),
        )
        .unwrap();
        assert_eq!(outcome.applied, vec!["c.txt"]);
        assert_eq!(outcome.unresolved, vec!["a.txt", "b.txt"]);
        assert_eq!(fixture.source_text("a.txt").as_deref(), Some("source-a\n"));
        assert_eq!(fixture.source_text("c.txt").as_deref(), Some("session-c\n"));

        // a 用副本版本覆盖，b 保留主工作区版本。
        let selection = MergeSelection {
            paths: None,
            resolutions: HashMap::from([
                ("a.txt".to_string(), ConflictResolution::UseSession),
                ("b.txt".to_string(), ConflictResolution::KeepSource),
            ]),
        };
        let outcome = apply_merge(&fixture.session, &fixture.source, &selection).unwrap();
        assert_eq!(outcome.applied, vec!["a.txt"]);
        assert_eq!(outcome.unresolved, vec!["b.txt"]);
        assert_eq!(fixture.source_text("a.txt").as_deref(), Some("session-a\n"));
        assert_eq!(fixture.source_text("b.txt").as_deref(), Some("source-b\n"));
        // 保留主工作区的改动仍在副本里待处理。
        let remaining = plan_merge(&fixture.session, &fixture.source).unwrap();
        assert_eq!(remaining.entries.len(), 1);
        assert_eq!(remaining.entries[0].path, "b.txt");
    }

    #[tokio::test]
    async fn identical_content_on_both_sides_only_advances_the_baseline() {
        let fixture = Fixture::new(&[("a.txt", "a0\n")]).await;
        fixture.edit_copy("a.txt", "same\n");
        fixture.edit_source("a.txt", "same\n");

        let plan = plan_merge(&fixture.session, &fixture.source).unwrap();
        assert_eq!(fixture.state_of(&plan, "a.txt"), MergeState::AlreadyApplied);
        let outcome = apply_merge(
            &fixture.session,
            &fixture.source,
            &MergeSelection::default(),
        )
        .unwrap();
        assert_eq!(outcome.already_applied, vec!["a.txt"]);
        assert!(outcome.applied.is_empty());
        assert!(
            plan_merge(&fixture.session, &fixture.source)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_conflicts_and_both_added_are_reported_with_their_kind() {
        let fixture = Fixture::new(&[("gone.txt", "g0\n"), ("kept.txt", "k0\n")]).await;
        fixture.delete_in_copy("gone.txt");
        fixture.edit_source("gone.txt", "edited-in-source\n");
        fixture.edit_copy("both.txt", "session\n");
        fixture.edit_source("both.txt", "source\n");
        fixture.edit_copy("kept.txt", "session-k\n");
        fs::remove_file(fixture.source.join("kept.txt")).unwrap();

        let plan = plan_merge(&fixture.session, &fixture.source).unwrap();
        let kind = |path: &str| {
            plan.entries
                .iter()
                .find(|entry| entry.path == path)
                .unwrap()
                .conflict
        };
        assert_eq!(kind("gone.txt"), Some(ConflictKind::ModifiedInSource));
        assert_eq!(kind("both.txt"), Some(ConflictKind::BothAdded));
        assert_eq!(kind("kept.txt"), Some(ConflictKind::DeletedInSource));
    }

    #[tokio::test]
    async fn selection_limits_the_merge_to_chosen_paths() {
        let fixture = Fixture::new(&[("a.txt", "a0\n"), ("b.txt", "b0\n")]).await;
        fixture.edit_copy("a.txt", "a1\n");
        fixture.edit_copy("b.txt", "b1\n");

        let selection = MergeSelection {
            paths: Some(vec!["a.txt".to_string()]),
            resolutions: HashMap::new(),
        };
        let outcome = apply_merge(&fixture.session, &fixture.source, &selection).unwrap();
        assert_eq!(outcome.applied, vec!["a.txt"]);
        assert_eq!(fixture.source_text("a.txt").as_deref(), Some("a1\n"));
        assert_eq!(fixture.source_text("b.txt").as_deref(), Some("b0\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn executable_bit_survives_the_merge() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new(&[("run.sh", "#!/bin/sh\necho 0\n")]).await;
        let script = fixture.copy.join("run.sh");
        fs::write(&script, "#!/bin/sh\necho 1\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fixture.session.reconcile().unwrap();

        apply_merge(
            &fixture.session,
            &fixture.source,
            &MergeSelection::default(),
        )
        .unwrap();
        let mode = fs::metadata(fixture.source.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        assert_eq!(
            fixture.source_text("run.sh").as_deref(),
            Some("#!/bin/sh\necho 1\n")
        );
    }

    #[tokio::test]
    async fn renamed_files_are_merged_as_delete_plus_add() {
        let fixture = Fixture::new(&[(
            "old.txt",
            "payload that is long enough to be recognised as the same file\n",
        )])
        .await;
        fs::rename(
            fixture.copy.join("old.txt"),
            fixture.copy.join("renamed.txt"),
        )
        .unwrap();
        fixture.session.reconcile().unwrap();

        let outcome = apply_merge(
            &fixture.session,
            &fixture.source,
            &MergeSelection::default(),
        )
        .unwrap();
        assert!(outcome.failed.is_empty(), "{outcome:?}");
        assert_eq!(fixture.source_text("old.txt"), None);
        assert!(fixture.source_text("renamed.txt").is_some());
        assert!(
            plan_merge(&fixture.session, &fixture.source)
                .unwrap()
                .entries
                .is_empty()
        );
    }
}
