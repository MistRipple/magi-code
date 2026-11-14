//! daemon 在线安装事务：持久请求身份、提交边界及回执；版本事实仍只归 PluginManager。
use crate::ApiError;
use magi_app_server_protocol::generated::{
    PluginBundleInstallApplication as Application, PluginBundleInstallCancelStatus as CancelStatus,
    PluginBundleInstallCancellation, PluginBundleInstallError, PluginBundleInstallResult,
};
use magi_core::{UtcMillis, fs_atomic::write_atomic};
use magi_plugin_system::PluginManager;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::watch;

const MAX_RECORDS: usize = 256;
const MAX_ACTIVE_PER_OWNER: usize = 16;
const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallOwner {
    pub plugin: String,
    pub digest: String,
    pub lease: String,
    pub background: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallCandidate {
    pub plugin_id: String,
    pub digest: String,
    pub version: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    Applying,
    Finished,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    plugin: String,
    digest: String,
    request_id: String,
    fingerprint: String,
    created_at: u64,
    phase: Phase,
    candidate: Option<InstallCandidate>,
    result: Option<PluginBundleInstallResult>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalState {
    version: u32,
    records: BTreeMap<String, Record>,
}
struct Journal {
    path: PathBuf,
    state: Mutex<Result<JournalState, String>>,
}

pub(crate) struct PluginInstallations {
    journal: Arc<Journal>,
    entries: Mutex<BTreeMap<String, Arc<Installation>>>,
}
pub(crate) struct Installation {
    key: String,
    owner: InstallOwner,
    journal: Arc<Journal>,
    phase: Mutex<Phase>,
    cancel: watch::Sender<bool>,
    result: watch::Sender<Option<Result<PluginBundleInstallResult, ApiError>>>,
    waiters: AtomicUsize,
}

fn key(plugin: &str, digest: &str, request: &str) -> String {
    use sha2::{Digest, Sha256};
    magi_core::hex_lower(Sha256::digest(
        serde_json::to_vec(&(plugin, digest, request)).expect("identity serializes"),
    ))
}
fn storage_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Conflict(format!(
        "安装任务日志不可用，未确认的操作不会自动重试：{error}"
    ))
}
impl Journal {
    fn persist(&self, next: &JournalState) -> Result<(), ApiError> {
        let bytes = serde_json::to_vec(next).map_err(storage_error)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(storage_error("安装任务日志超限"));
        }
        write_atomic(&self.path, bytes).map_err(storage_error)
    }
    fn update(&self, id: &str, update: impl FnOnce(&mut Record)) -> Result<(), ApiError> {
        let mut guard = self.state.lock().map_err(storage_error)?;
        let state = guard.as_mut().map_err(|e| storage_error(e.as_str()))?;
        let mut next = state.clone();
        update(
            next.records
                .get_mut(id)
                .ok_or_else(|| storage_error("安装身份已丢失"))?,
        );
        self.persist(&next)?;
        *state = next;
        Ok(())
    }
}

impl PluginInstallations {
    /// 只读取原安装事实判定提交是否存在；恢复绝不重新下载、启用或替换包。
    /// 损坏日志保留原字节并阻止安装写入，不影响已安装插件的读取与运行。
    pub fn open(manager: &PluginManager) -> Self {
        let path = manager.root().join("installations.json");
        let loaded = (|| -> Result<JournalState, String> {
            let mut state: JournalState = match std::fs::read(&path) {
                Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => {
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
                }
                Ok(_) => return Err("安装任务日志超限".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => JournalState {
                    version: 1,
                    records: BTreeMap::new(),
                },
                Err(e) => return Err(e.to_string()),
            };
            if state.version != 1 || state.records.len() > MAX_RECORDS {
                return Err("安装任务日志版本或数量无效".into());
            }
            let mut recovered = false;
            for (id, record) in &mut state.records {
                if id != &key(&record.plugin, &record.digest, &record.request_id)
                    || record.fingerprint.len() > 2048
                    || record.request_id.is_empty()
                    || (record.phase == Phase::Finished) != record.result.is_some()
                    || (record.phase == Phase::Applying && record.candidate.is_none())
                {
                    return Err("安装任务日志身份或阶段无效".into());
                }
                if record.phase == Phase::Finished {
                    continue;
                }
                let committed = record.candidate.as_ref().and_then(|candidate| {
                    manager
                        .state()
                        .plugins
                        .get(&candidate.plugin_id)
                        .filter(|plugin| plugin.digest == candidate.digest)
                });
                let installed = committed.is_some();
                let message = if installed {
                    "应用退出时安装任务尚未结算；候选版本已安装，请在插件管理检查运行状态。再次安装须显式发起新请求。"
                } else {
                    "应用退出时安装任务尚未结算；没有确认候选版本已提交。请检查插件状态后显式重试。"
                };
                record.result = Some(PluginBundleInstallResult {
                    application: Application::Failed,
                    bundle: committed
                        .and_then(|plugin| match &plugin.source {
                            magi_plugin_system::PluginSource::Upstream { package, .. } => {
                                Some(package.clone())
                            }
                            _ => None,
                        })
                        .or_else(|| {
                            serde_json::from_str::<serde_json::Value>(&record.fingerprint)
                                .ok()
                                .and_then(|v| {
                                    v["package"].as_str().map(str::to_owned).or_else(|| {
                                        v["github"]["repository"]
                                            .as_str()
                                            .map(|repo| format!("github:{repo}"))
                                    })
                                })
                        }),
                    version: record.candidate.as_ref().map(|c| c.version.clone()),
                    changed: None,
                    installed: installed.then_some(true),
                    stage: Some(if installed { "enable" } else { "install" }.into()),
                    error: Some(PluginBundleInstallError {
                        code: "installation-interrupted".into(),
                        message: message.into(),
                        diagnostic: message.into(),
                        incompatible: None,
                    }),
                });
                record.phase = Phase::Finished;
                recovered = true;
            }
            if recovered {
                write_atomic(
                    &path,
                    serde_json::to_vec(&state).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(state)
        })();
        let journal = Arc::new(Journal {
            path,
            state: Mutex::new(loaded),
        });
        Self {
            journal,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn start(
        &self,
        owner: InstallOwner,
        id: &str,
        fingerprint: String,
    ) -> Result<(Arc<Installation>, bool), ApiError> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            || fingerprint.len() > 2048
        {
            return Err(ApiError::InvalidInput(
                "安装 requestId 必须为 1–128 个字母、数字或 -_.:".into(),
            ));
        }
        let mut entries = self.entries.lock().map_err(storage_error)?;
        let mut guard = self.journal.state.lock().map_err(storage_error)?;
        let state = guard.as_mut().map_err(|e| storage_error(e.as_str()))?;
        let identity = key(&owner.plugin, &owner.digest, id);
        if let Some(record) = state.records.get(&identity) {
            if record.fingerprint != fingerprint {
                return Err(ApiError::Conflict("安装 requestId 已绑定另一个请求".into()));
            }
            let job = entries
                .entry(identity.clone())
                .or_insert_with(|| {
                    Arc::new(Installation {
                        key: identity,
                        owner,
                        journal: self.journal.clone(),
                        phase: Mutex::new(record.phase),
                        cancel: watch::channel(false).0,
                        result: watch::channel(record.result.clone().map(Ok)).0,
                        waiters: AtomicUsize::new(0),
                    })
                })
                .clone();
            return Ok((job, false));
        }
        let now = UtcMillis::now().0;
        let mut next = state.clone();
        next.records.retain(|_, record| {
            record.phase != Phase::Finished || now.saturating_sub(record.created_at) < RETENTION_MS
        });
        if next.records.len() >= MAX_RECORDS
            || next
                .records
                .values()
                .filter(|record| {
                    record.plugin == owner.plugin
                        && record.digest == owner.digest
                        && record.phase != Phase::Finished
                })
                .count()
                >= MAX_ACTIVE_PER_OWNER
        {
            return Err(ApiError::Conflict(
                "安装任务或保留回执达到上限，请等待当前任务结束或回执到期".into(),
            ));
        }
        next.records.insert(
            identity.clone(),
            Record {
                plugin: owner.plugin.clone(),
                digest: owner.digest.clone(),
                request_id: id.into(),
                fingerprint,
                created_at: now,
                phase: Phase::Preparing,
                candidate: None,
                result: None,
            },
        );
        self.journal.persist(&next)?;
        *state = next;
        entries.retain(|id, _| state.records.contains_key(id));
        let job = Arc::new(Installation {
            key: identity.clone(),
            owner,
            journal: self.journal.clone(),
            phase: Mutex::new(Phase::Preparing),
            cancel: watch::channel(false).0,
            result: watch::channel(None).0,
            waiters: AtomicUsize::new(0),
        });
        entries.insert(identity, job.clone());
        Ok((job, true))
    }

    pub async fn cancel(
        &self,
        owner: &InstallOwner,
        id: &str,
    ) -> Result<PluginBundleInstallCancellation, ApiError> {
        let job = self
            .entries
            .lock()
            .map_err(storage_error)?
            .get(&key(&owner.plugin, &owner.digest, id))
            .cloned();
        let status = if let Some(job) = job {
            if job.owner.lease != owner.lease {
                CancelStatus::NotRunning
            } else if job.request_cancel() {
                if job.wait().await?.application == Application::Cancelled {
                    CancelStatus::Cancelled
                } else {
                    CancelStatus::NotRunning
                }
            } else if job.result.borrow().is_some() {
                CancelStatus::NotRunning
            } else {
                CancelStatus::TooLate
            }
        } else {
            CancelStatus::NotRunning
        };
        Ok(PluginBundleInstallCancellation { status })
    }
    pub fn instance_ended(&self, plugin: &str, lease: &str) {
        if let Ok(entries) = self.entries.lock() {
            for job in entries
                .values()
                .filter(|job| job.owner.plugin == plugin && job.owner.lease == lease)
            {
                job.request_cancel();
            }
        }
    }
}
impl Installation {
    pub fn request_cancel(&self) -> bool {
        let phase = self.phase.lock().expect("installation phase");
        if *phase != Phase::Preparing {
            return false;
        }
        self.cancel.send_replace(true);
        true
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.cancel.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
    pub fn begin_apply(&self, candidate: InstallCandidate) -> Result<bool, ApiError> {
        let mut phase = self.phase.lock().map_err(storage_error)?;
        if *phase != Phase::Preparing || *self.cancel.borrow() {
            return Ok(false);
        }
        self.journal.update(&self.key, |record| {
            record.phase = Phase::Applying;
            record.candidate = Some(candidate);
        })?;
        *phase = Phase::Applying;
        Ok(true)
    }
    pub fn finish(&self, result: PluginBundleInstallResult) {
        let mut phase = self.phase.lock().expect("installation phase");
        let committed = self
            .journal
            .update(&self.key, |record| {
                record.phase = Phase::Finished;
                record.result = Some(result.clone());
            })
            .map(|()| result);
        *phase = Phase::Finished;
        self.result.send_replace(Some(committed));
    }
    pub async fn wait(&self) -> Result<PluginBundleInstallResult, ApiError> {
        let mut receiver = self.result.subscribe();
        receiver
            .wait_for(Option::is_some)
            .await
            .map_err(storage_error)?
            .clone()
            .expect("installation result")
    }
}

/// 等待方消失只取消准备；持久提交阶段仍由 daemon 结算。
pub(crate) struct InstallationWaiter(pub Arc<Installation>);
impl InstallationWaiter {
    pub fn new(job: Arc<Installation>) -> Self {
        job.waiters.fetch_add(1, Ordering::Relaxed);
        Self(job)
    }
}
impl Drop for InstallationWaiter {
    fn drop(&mut self) {
        if self.0.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.request_cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn store(root: &std::path::Path) -> PluginInstallations {
        PluginInstallations::open(&PluginManager::open(root).unwrap())
    }
    fn candidate() -> InstallCandidate {
        InstallCandidate {
            plugin_id: "sample".into(),
            digest: "candidate-digest".into(),
            version: "1.0.0".into(),
        }
    }
    fn owner(lease: &str) -> InstallOwner {
        InstallOwner {
            plugin: "manager".into(),
            digest: "digest".into(),
            lease: lease.into(),
            background: true,
        }
    }
    fn result(application: Application) -> PluginBundleInstallResult {
        PluginBundleInstallResult {
            application,
            bundle: Some("sample".into()),
            version: None,
            changed: None,
            installed: None,
            stage: None,
            error: None,
        }
    }
    #[tokio::test]
    async fn restart_preserves_receipt_and_request_identity_across_instance_generations() {
        let root = tempfile::tempdir().unwrap();
        let first = store(root.path());
        let (job, fresh) = first
            .start(owner("old"), "durable", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(fresh);
        job.finish(result(Application::Applied));
        let before = std::fs::read(root.path().join("state.json")).unwrap_or_default();
        drop(first);
        drop(job);
        let restored = store(root.path());
        let (receipt, fresh) = restored
            .start(owner("new"), "durable", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(!fresh);
        assert_eq!(
            receipt.wait().await.unwrap().application,
            Application::Applied
        );
        assert!(
            restored
                .start(owner("new"), "durable", "different".into())
                .is_err()
        );
        let mut foreign = owner("new");
        foreign.plugin = "another-manager".into();
        assert!(
            restored
                .start(foreign, "durable", "{\"package\":\"sample\"}".into())
                .unwrap()
                .1,
            "另一插件不能领取原请求的成功回执"
        );
        assert_eq!(
            std::fs::read(root.path().join("state.json")).unwrap_or_default(),
            before,
            "读回执不能改变安装事实"
        );
    }

    #[tokio::test]
    async fn restart_settles_preparation_without_replaying_and_new_requests_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        let first = store(root.path());
        first
            .start(owner("old"), "prepare", "{\"package\":\"sample\"}".into())
            .unwrap();
        drop(first);
        let recovered = store(root.path());
        let (job, fresh) = recovered
            .start(owner("new"), "prepare", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(!fresh);
        let result = job.wait().await.unwrap();
        assert_eq!(result.application, Application::Failed);
        assert_eq!(result.error.unwrap().code, "installation-interrupted");
        assert_eq!(result.installed, None);
        assert!(
            recovered
                .start(owner("new"), "explicit-new-request", "input".into())
                .unwrap()
                .1
        );
    }

    #[tokio::test]
    async fn github_preparation_restart_preserves_source_without_resolving_a_moved_branch() {
        let root = tempfile::tempdir().unwrap();
        let fingerprint = serde_json::json!({"ecosystem":"dsh","github":{"repository":"owner/repo","reference":"main"},"requestId":"github"}).to_string();
        let first = store(root.path());
        first
            .start(owner("old"), "github", fingerprint.clone())
            .unwrap();
        drop(first);
        let recovered = store(root.path());
        let (receipt, fresh) = recovered
            .start(owner("new"), "github", fingerprint)
            .unwrap();
        assert!(!fresh);
        let result = receipt.wait().await.unwrap();
        assert_eq!(result.bundle.as_deref(), Some("github:owner/repo"));
        assert_eq!(result.application, Application::Failed);
        assert_eq!(result.error.unwrap().code, "installation-interrupted");
        assert_eq!(result.installed, None);
    }

    #[tokio::test]
    async fn restart_distinguishes_uncommitted_and_committed_candidate_without_authorizing_or_activating()
     {
        use std::io::{Cursor, Write};
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(root.path()).unwrap();
        let manifest = serde_json::json!({"sdkVersion":2,"id":"acme.candidate","name":"Candidate","version":"1.0.0","description":"","dataSchemaVersion":1,"settingsSchema":{"type":"object"},"contributions":{},"permissions":[{"kind":"storage","targets":["cache"]}]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        let package =
            magi_plugin_system::PluginPackage::from_archive(&zip.finish().unwrap().into_inner())
                .unwrap();
        let candidate = InstallCandidate {
            plugin_id: "acme.candidate".into(),
            digest: package.digest().into(),
            version: "1.0.0".into(),
        };
        let first = PluginInstallations::open(&manager);
        let (job, _) = first
            .start(owner("old"), "commit", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(job.begin_apply(candidate.clone()).unwrap());
        drop(first);
        drop(job);
        let missing = PluginInstallations::open(&manager);
        let (job, fresh) = missing
            .start(owner("new"), "commit", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(!fresh);
        assert_eq!(job.wait().await.unwrap().installed, None);
        drop(missing);
        drop(job);
        let applying = PluginInstallations::open(&manager);
        let (job, _) = applying
            .start(owner("old"), "committed", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(job.begin_apply(candidate).unwrap());
        manager
            .install(
                &package,
                magi_plugin_system::PluginSource::Local {
                    name: "candidate.zip".into(),
                },
            )
            .unwrap();
        let before = manager.state().clone();
        drop(applying);
        drop(job);
        let recovered = PluginInstallations::open(&manager);
        let (receipt, fresh) = recovered
            .start(owner("new"), "committed", "{\"package\":\"sample\"}".into())
            .unwrap();
        assert!(!fresh);
        let receipt = receipt.wait().await.unwrap();
        assert_eq!(
            receipt.application,
            Application::Failed,
            "版本存在不能证明生命周期结算成功"
        );
        assert_eq!(receipt.installed, Some(true));
        assert_eq!(
            receipt.changed, None,
            "不能从两次快照猜测提交期间的实际变更"
        );
        assert_eq!(receipt.stage.as_deref(), Some("enable"));
        assert_eq!(manager.state(), &before);
    }

    #[tokio::test]
    async fn journal_write_failure_never_allows_unrecorded_commit_or_success_receipt() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let (job, _) = store
            .start(owner("old"), "durable", "input".into())
            .unwrap();
        let path = root.path().join("installations.json");
        let saved = root.path().join("saved.json");
        std::fs::rename(&path, &saved).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(job.begin_apply(candidate()).is_err());
        job.finish(result(Application::Applied));
        assert!(job.wait().await.is_err(), "未持久化的成功不能返回给调用者");
        let before = std::fs::read(&saved).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(saved, &path).unwrap();
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "失败写入保留原提交阶段"
        );
    }

    #[test]
    fn corrupt_journal_is_preserved_and_blocks_installation_requests() {
        let root = tempfile::tempdir().unwrap();
        let manager = PluginManager::open(root.path()).unwrap();
        let path = root.path().join("installations.json");
        std::fs::write(&path, b"broken").unwrap();
        let blocked = PluginInstallations::open(&manager);
        assert!(blocked.start(owner("new"), "job", "input".into()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }

    #[tokio::test]
    async fn cancellation_waits_for_cleanup_and_does_not_cross_owner_or_instance() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(store(root.path()));
        let who = owner("current");
        let (job, _) = store.start(who.clone(), "same-id", "input".into()).unwrap();
        assert_eq!(
            store
                .cancel(&owner("foreign"), "same-id")
                .await
                .unwrap()
                .status,
            CancelStatus::NotRunning
        );
        let request = {
            let store = store.clone();
            let who = who.clone();
            tokio::spawn(async move { store.cancel(&who, "same-id").await.unwrap() })
        };
        tokio::time::timeout(Duration::from_secs(1), job.cancelled())
            .await
            .unwrap();
        assert!(!request.is_finished(), "进程尚未清理时不能先报告 cancelled");
        assert!(
            !job.begin_apply(candidate()).unwrap(),
            "取消先取得提交屏障后，不能再写入版本"
        );
        job.finish(result(Application::Cancelled));
        assert_eq!(request.await.unwrap().status, CancelStatus::Cancelled);
        assert_eq!(
            store.cancel(&who, "same-id").await.unwrap().status,
            CancelStatus::NotRunning
        );
    }
    #[tokio::test]
    async fn applying_is_not_cancelled_by_waiter_drop_or_instance_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let who = owner("current");
        let (job, _) = store
            .start(who.clone(), "applying", "input".into())
            .unwrap();
        assert!(job.begin_apply(candidate()).unwrap());
        drop(InstallationWaiter::new(job.clone()));
        store.instance_ended("manager", "current");
        assert_eq!(
            store.cancel(&who, "applying").await.unwrap().status,
            CancelStatus::TooLate
        );
        assert!(!*job.cancel.borrow());
        job.finish(result(Application::Applied));
        assert_eq!(job.wait().await.unwrap().application, Application::Applied);
    }
    #[tokio::test]
    async fn dedup_binds_the_complete_request_and_finished_jobs_do_not_block_new_work() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let who = owner("current");
        let (job, fresh) = store.start(who.clone(), "first", "input".into()).unwrap();
        assert!(fresh);
        let (duplicate, fresh) = store.start(who.clone(), "first", "input".into()).unwrap();
        assert!(!fresh);
        assert!(Arc::ptr_eq(&job, &duplicate));
        assert!(
            store
                .start(who.clone(), "first", "different".into())
                .is_err()
        );
        for index in 0..15 {
            store
                .start(who.clone(), &format!("job-{index}"), "input".into())
                .unwrap();
        }
        assert!(
            store
                .start(who.clone(), "overflow", "input".into())
                .is_err()
        );
        job.finish(result(Application::Failed));
        assert!(store.start(who, "next", "input".into()).is_ok());
    }
    #[test]
    fn ending_an_instance_cancels_its_preparation_but_not_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let (old, _) = store.start(owner("old"), "job", "input".into()).unwrap();
        let (duplicate, fresh) = store.start(owner("new"), "job", "input".into()).unwrap();
        assert!(!fresh);
        assert!(
            Arc::ptr_eq(&old, &duplicate),
            "实例重建不能以新租约重放同一请求"
        );
        let (new, _) = store
            .start(owner("new"), "new-job", "input".into())
            .unwrap();
        store.instance_ended("manager", "old");
        assert!(*old.cancel.borrow());
        assert!(!*new.cancel.borrow());
        drop(InstallationWaiter::new(new.clone()));
        assert!(*new.cancel.borrow());
    }

    #[test]
    fn duplicate_waiter_disconnect_does_not_cancel_the_original_request() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let (job, _) = store
            .start(owner("current"), "job", "input".into())
            .unwrap();
        let original = InstallationWaiter::new(job.clone());
        let retry = InstallationWaiter::new(job.clone());
        drop(retry);
        assert!(!*job.cancel.borrow());
        drop(original);
        assert!(*job.cancel.borrow());
        assert!(!job.begin_apply(candidate()).unwrap());
    }
}
