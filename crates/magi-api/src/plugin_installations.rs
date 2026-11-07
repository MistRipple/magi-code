//! daemon 拥有的在线安装任务。只记录当前操作与短期去重回执，安装事实仍在 PluginManager。
use crate::ApiError;
use magi_app_server_protocol::generated::{
    PluginBundleInstallApplication as Application, PluginBundleInstallCancelStatus as CancelStatus,
    PluginBundleInstallCancellation, PluginBundleInstallResult,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct InstallOwner {
    pub plugin: String,
    pub digest: String,
    pub lease: String,
    pub background: bool,
}

#[derive(Default)]
pub(crate) struct PluginInstallations {
    entries: Mutex<HashMap<(InstallOwner, String), Arc<Installation>>>,
}

#[derive(PartialEq)]
enum Phase {
    Preparing,
    Applying,
    Finished,
}
pub(crate) struct Installation {
    phase: Mutex<Phase>,
    cancel: watch::Sender<bool>,
    result: watch::Sender<Option<PluginBundleInstallResult>>,
    fingerprint: String,
    created: Instant,
    waiters: AtomicUsize,
}

impl PluginInstallations {
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
        {
            return Err(ApiError::InvalidInput(
                "安装 requestId 必须为 1–128 个字母、数字或 -_.:".into(),
            ));
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApiError::Conflict("安装任务不可用".into()))?;
        entries.retain(|_, job| {
            job.result.borrow().is_none() || job.created.elapsed() < Duration::from_secs(1200)
        });
        let key = (owner.clone(), id.to_owned());
        if let Some(job) = entries.get(&key) {
            return if job.fingerprint == fingerprint {
                Ok((job.clone(), false))
            } else {
                Err(ApiError::Conflict("安装 requestId 已绑定另一个请求".into()))
            };
        }
        while entries.len() >= 256 || entries.keys().filter(|(who, _)| who == &owner).count() >= 16
        {
            let restricted = entries.keys().filter(|(who, _)| who == &owner).count() >= 16;
            let oldest = entries
                .iter()
                .filter(|((who, _), job)| {
                    (!restricted || who == &owner) && job.result.borrow().is_some()
                })
                .min_by_key(|(_, job)| job.created)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                entries.remove(&key);
            } else {
                return Err(ApiError::Conflict(
                    "安装任务过多，请等待当前任务结束".into(),
                ));
            }
        }
        let job = Arc::new(Installation {
            phase: Mutex::new(Phase::Preparing),
            cancel: watch::channel(false).0,
            result: watch::channel(None).0,
            fingerprint,
            created: Instant::now(),
            waiters: AtomicUsize::new(0),
        });
        entries.insert(key, job.clone());
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
            .map_err(|_| ApiError::Conflict("安装任务不可用".into()))?
            .get(&(owner.clone(), id.to_owned()))
            .cloned();
        let status = if let Some(job) = job {
            if job.request_cancel() {
                // cancelled 表示准备 future 与受控子进程已释放，不只表示收到按钮点击。
                if job.wait().await.application == Application::Cancelled {
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
            for ((owner, _), job) in entries.iter() {
                if owner.plugin == plugin && owner.lease == lease {
                    job.request_cancel();
                }
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
    pub fn begin_apply(&self) -> bool {
        let mut phase = self.phase.lock().expect("installation phase");
        if *phase != Phase::Preparing || *self.cancel.borrow() {
            return false;
        }
        *phase = Phase::Applying;
        true
    }
    pub fn finish(&self, result: PluginBundleInstallResult) {
        let mut phase = self.phase.lock().expect("installation phase");
        *phase = Phase::Finished;
        self.result.send_replace(Some(result));
    }
    pub async fn wait(&self) -> PluginBundleInstallResult {
        let mut receiver = self.result.subscribe();
        receiver
            .wait_for(Option::is_some)
            .await
            .expect("installation result")
            .clone()
            .unwrap()
    }
}

/// 调用断开只取消尚未进入提交的准备；应用中的任务由 daemon 继续结算。
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
    async fn cancellation_waits_for_cleanup_and_does_not_cross_owner_or_instance() {
        let store = Arc::new(PluginInstallations::default());
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
        assert!(!job.begin_apply(), "取消先取得提交屏障后，不能再写入版本");
        job.finish(result(Application::Cancelled));
        assert_eq!(request.await.unwrap().status, CancelStatus::Cancelled);
        assert_eq!(
            store.cancel(&who, "same-id").await.unwrap().status,
            CancelStatus::NotRunning
        );
    }
    #[tokio::test]
    async fn applying_is_not_cancelled_by_waiter_drop_or_instance_replacement() {
        let store = PluginInstallations::default();
        let who = owner("current");
        let (job, _) = store
            .start(who.clone(), "applying", "input".into())
            .unwrap();
        assert!(job.begin_apply());
        drop(InstallationWaiter::new(job.clone()));
        store.instance_ended("manager", "current");
        assert_eq!(
            store.cancel(&who, "applying").await.unwrap().status,
            CancelStatus::TooLate
        );
        assert!(!*job.cancel.borrow());
        job.finish(result(Application::Applied));
        assert_eq!(job.wait().await.application, Application::Applied);
    }
    #[tokio::test]
    async fn dedup_binds_the_complete_request_and_finished_jobs_do_not_block_new_work() {
        let store = PluginInstallations::default();
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
        let store = PluginInstallations::default();
        let (old, _) = store.start(owner("old"), "job", "input".into()).unwrap();
        let (new, _) = store.start(owner("new"), "job", "input".into()).unwrap();
        store.instance_ended("manager", "old");
        assert!(*old.cancel.borrow());
        assert!(!*new.cancel.borrow());
        drop(InstallationWaiter::new(new.clone()));
        assert!(*new.cancel.borrow());
    }

    #[test]
    fn duplicate_waiter_disconnect_does_not_cancel_the_original_request() {
        let store = PluginInstallations::default();
        let (job, _) = store
            .start(owner("current"), "job", "input".into())
            .unwrap();
        let original = InstallationWaiter::new(job.clone());
        let retry = InstallationWaiter::new(job.clone());
        drop(retry);
        assert!(!*job.cancel.borrow());
        drop(original);
        assert!(*job.cancel.borrow());
        assert!(!job.begin_apply());
    }
}
