//! Job 状态附着在原 canonical 工具 item，不另建进程状态文件或业务日志。
use super::{CanonicalTurnMutation, SessionStore};
use magi_core::{EventId, SessionId, UtcMillis};
use magi_tool_runtime::{
    BACKGROUND_JOB_METADATA_KEY, BackgroundJob, BackgroundJobJournal, BackgroundJobStatus,
    ToolExecutionContext,
};
use std::{path::PathBuf, sync::Arc};

struct SessionJobJournal {
    store: Arc<SessionStore>,
    output_root: PathBuf,
}

impl SessionStore {
    pub fn install_canonical_item_commit_observer(
        &self,
        observer: Arc<dyn Fn(super::CanonicalItemCommitNotice) + Send + Sync>,
    ) {
        *self
            .canonical_item_commit_observer
            .write()
            .expect("canonical observer lock") = Some(observer);
    }
    pub fn background_job_journal(
        self: &Arc<Self>,
        output_root: PathBuf,
    ) -> Arc<dyn BackgroundJobJournal> {
        Arc::new(SessionJobJournal {
            store: self.clone(),
            output_root,
        })
    }

    pub fn background_jobs_for_session(
        &self,
        session: &SessionId,
    ) -> Result<Vec<BackgroundJob>, String> {
        let state = self.state.read().expect("session state lock");
        state
            .canonical_turns
            .iter()
            .filter(|turn| &turn.session_id == session)
            .flat_map(|turn| &turn.items)
            .filter_map(|item| match item.background_job() {
                Ok(Some(job)) => Some(Ok(job)),
                Ok(None) => None,
                Err(error) => Some(Err(error.to_string())),
            })
            .collect()
    }

    fn commit_background_job(&self, job: &BackgroundJob) -> Result<(), String> {
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier");
        let lock = self.canonical_commit_lock_for(&job.session_id);
        let _guard = lock.lock().expect("canonical session lock");
        let mutation = {
            let state = self.state.read().expect("session state lock");
            if state
                .sessions
                .iter()
                .find(|session| session.session_id == job.session_id)
                .is_none_or(|session| {
                    session.workspace_id.as_deref()
                        != job
                            .workspace_id
                            .as_ref()
                            .map(|workspace| workspace.as_str())
                })
            {
                return Err("Job 与 Session 的工作区归属不匹配".into());
            }
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| turn.session_id == job.session_id && turn.turn_id == job.turn_id)
                .ok_or_else(|| "Job 来源 Turn 已不存在".to_string())?;
            let mut next = previous.clone();
            let item = next
                .items
                .iter_mut()
                .find(|item| item.item_id == job.item_id)
                .ok_or("Job 来源工具 item 不存在")?;
            if item
                .worker
                .as_ref()
                .and_then(|worker| worker.task_id.as_ref())
                != Some(&job.task_id)
                || item.tool.as_ref().is_none_or(|tool| {
                    tool.call_id != job.tool_call_id || tool.name != "shell_exec"
                })
            {
                return Err("Job 来源工具或任务身份冲突".into());
            }
            let old = item
                .metadata
                .get(BACKGROUND_JOB_METADATA_KEY)
                .map(|value| serde_json::from_value::<BackgroundJob>(value.clone()))
                .transpose()
                .map_err(|error| error.to_string())?;
            if old.as_ref() == Some(job) {
                return Ok(());
            }
            if old.is_none() && (previous.status.is_terminal() || previous.completed_at.is_some()) {
                return Err("已关闭 Turn 不能启动新 Job".into());
            }
            if old.is_none() {
                // 原子提交 Starting 前再次检查父轮；外层执行检查与此处可能存在时间窗。
                // 已存在 Job 的取消、退出和重启结算仍可写回原 item。
                super::agent_turns::validate_agent_turn_admission(self, &state, previous)
                    .map_err(|error| error.to_string())?;
            }
            job.validate_update_from(old.as_ref())?;
            item.metadata.insert(
                BACKGROUND_JOB_METADATA_KEY.into(),
                serde_json::to_value(job).map_err(|error| error.to_string())?,
            );
            item.updated_at =
                UtcMillis(UtcMillis::now().0.max(item.updated_at.0.saturating_add(1)));
            item.item_version = Some(
                item.item_version
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("item version 溢出")?,
            );
            next.validate_update_from(previous)
                .map_err(|error| error.to_string())?;
            CanonicalTurnMutation {
                previous: Some(previous.clone()),
                next,
            }
        };
        self.persist_session_transaction(
            &job.session_id,
            std::slice::from_ref(&mutation),
            &[],
            None,
        )
        .map_err(|error| error.to_string())?;
        let item = mutation
            .next
            .items
            .iter()
            .find(|item| item.item_id == job.item_id)
            .expect("job item")
            .clone();
        let turn = mutation.next.clone();
        super::sidecar::apply_canonical_turn_in_state(
            &mut self.state.write().expect("session state lock"),
            mutation.next,
        );
        drop(_guard);
        drop(_barrier);
        {
            let (version, changed) = &*self.background_job_signal;
            let mut version = version.lock().expect("job signal lock");
            *version = version.wrapping_add(1);
            changed.notify_all();
        }
        let observer = self
            .canonical_item_commit_observer
            .read()
            .expect("canonical observer lock")
            .clone();
        if let Some(observer) = observer {
            let workspace_id = self
                .state
                .read()
                .expect("session state lock")
                .sessions
                .iter()
                .find(|session| session.session_id == job.session_id)
                .and_then(|session| session.workspace_id.clone());
            observer(super::CanonicalItemCommitNotice {
                turn,
                item,
                workspace_id,
            });
        }
        Ok(())
    }

    /// 重启不按 PID 猜测或重放 Job；保留输出和明确的结果未知事实。
    pub fn reconcile_background_jobs_after_restart(&self) -> Result<usize, String> {
        let sessions = self
            .state
            .read()
            .expect("session state lock")
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        let mut count = 0;
        for session in sessions {
            for mut job in self.background_jobs_for_session(&session)? {
                if job.status.is_terminal() {
                    continue;
                }
                job.revision = job.revision.checked_add(1).ok_or("Job revision 溢出")?;
                job.status = BackgroundJobStatus::OutcomeUnknown;
                job.settled_at = Some(UtcMillis(UtcMillis::now().0.max(job.started_at.0)));
                self.commit_background_job(&job)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

impl BackgroundJobJournal for SessionJobJournal {
    fn begin(
        &self,
        context: &ToolExecutionContext,
        tool_call_id: &str,
        command: &str,
        cwd: &str,
    ) -> Result<BackgroundJob, String> {
        let session = context
            .session_id
            .as_ref()
            .ok_or("持久 Job 需要 Session 身份")?;
        let task = context.task_id.as_ref().ok_or("持久 Job 需要 Task 身份")?;
        if !self.output_root.is_absolute() {
            return Err("Job 输出根目录必须为绝对路径".into());
        }
        let job = {
            let state = self.store.state.read().expect("session state lock");
            let (turn, item) = state
                .canonical_turns
                .iter()
                .rev()
                .filter(|turn| &turn.session_id == session && !turn.status.is_terminal())
                .find_map(|turn| {
                    turn.items
                        .iter()
                        .rev()
                        .find(|item| {
                            item.worker
                                .as_ref()
                                .and_then(|worker| worker.task_id.as_ref())
                                == Some(task)
                                && item.tool.as_ref().is_some_and(|tool| {
                                    tool.call_id == tool_call_id && tool.name == "shell_exec"
                                })
                        })
                        .map(|item| (turn, item))
                })
                .ok_or("Job 缺少当前 canonical 工具调用")?;
            if item.metadata.contains_key(BACKGROUND_JOB_METADATA_KEY) {
                return Err("该工具调用已经启动 Job，不能重放".into());
            }
            let id = EventId::unique("job").to_string();
            let output = self.output_root.join(&id);
            BackgroundJob {
                job_id: id,
                session_id: session.clone(),
                workspace_id: context.workspace_id.clone(),
                task_id: task.clone(),
                turn_id: turn.turn_id.clone(),
                item_id: item.item_id.clone(),
                tool_call_id: tool_call_id.into(),
                revision: 1,
                command: command.into(),
                cwd: cwd.into(),
                status: BackgroundJobStatus::Starting,
                started_at: UtcMillis::now(),
                settled_at: None,
                exit_code: None,
                stdout_path: output.join("stdout.log"),
                stderr_path: output.join("stderr.log"),
            }
        };
        self.store.commit_background_job(&job)?;
        Ok(job)
    }
    fn commit(&self, job: &BackgroundJob) -> Result<(), String> {
        self.store.commit_background_job(job)
    }
    fn list(&self, context: &ToolExecutionContext) -> Result<Vec<BackgroundJob>, String> {
        let session = context
            .session_id
            .as_ref()
            .ok_or("Job 查询需要 Session 身份")?;
        let jobs = self
            .store
            .background_jobs_for_session(session)?
            .into_iter()
            .filter(|job| job.belongs_to(context))
            .collect::<Vec<_>>();
        for job in &jobs {
            job.validate_snapshot()?;
            let output = self.output_root.join(&job.job_id);
            if job.stdout_path != output.join("stdout.log")
                || job.stderr_path != output.join("stderr.log")
            {
                return Err("Job 输出位置与 daemon 管理目录不匹配".into());
            }
        }
        Ok(jobs)
    }

    fn wait_for_terminal(
        &self,
        context: &ToolExecutionContext,
        job_id: &str,
        timeout: std::time::Duration,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Option<BackgroundJob>, String> {
        use std::{
            sync::atomic::Ordering,
            time::{Duration, Instant},
        };
        let deadline = Instant::now() + timeout;
        let (version, changed) = &*self.store.background_job_signal;
        loop {
            // 先读取唤醒版本，再读 canonical 事实，避免提交落在读取与睡眠之间丢失唤醒。
            let observed = *version.lock().expect("job signal lock");
            let job = self
                .list(context)?
                .into_iter()
                .find(|job| job.job_id == job_id)
                .ok_or("Job 不存在或不在当前授权范围")?;
            if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Ok(None);
            }
            if job.status.is_terminal() || Instant::now() >= deadline {
                return Ok(Some(job));
            }
            let mut current = version.lock().expect("job signal lock");
            // 只为取消检查设置短唤醒间隔；Job 变化由提交后的通知唤醒，期间不轮询模型。
            while *current == observed && Instant::now() < deadline {
                if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                    return Ok(None);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                let interval = if cancelled.is_some() {
                    remaining.min(Duration::from_millis(100))
                } else {
                    remaining
                };
                current = changed
                    .wait_timeout(current, interval)
                    .expect("job wait lock")
                    .0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalTurn, SessionStoreState};
    use magi_core::{DomainError, TaskId};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Default)]
    struct Writer {
        fail: AtomicBool,
        mutations: Mutex<Vec<CanonicalTurnMutation>>,
    }
    impl super::super::SessionEventWriter for Writer {
        fn append_session_transaction(
            &self,
            _: &SessionId,
            turns: &[CanonicalTurnMutation],
            _: &[super::super::SessionInputMutation],
            _: Option<(&crate::SessionAcceptanceRecord, Option<&magi_core::Task>)>,
        ) -> magi_core::DomainResult<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(DomainError::InvalidState {
                    message: "disk unavailable".into(),
                });
            }
            self.mutations.lock().unwrap().extend_from_slice(turns);
            Ok(())
        }
    }

    fn fixture() -> (
        Arc<SessionStore>,
        Arc<Writer>,
        ToolExecutionContext,
        PathBuf,
    ) {
        let session_id = SessionId::new(EventId::unique("job-session").to_string());
        let turn: CanonicalTurn = serde_json::from_value(serde_json::json!({
            "sessionId":session_id, "turnId":"job-turn", "turnSeq":1,
            "acceptedAt":1,"status":"running","items":[{
                "sessionId":session_id, "turnId":"job-turn", "turnSeq":1,
                "itemId":"job-tool-item", "itemSeq":1,"kind":"tool_call",
                "createdAt":1,"updatedAt":1,"status":"running", "sourceThreadId":"job-thread",
                "tool":{"callId":"job-call", "name":"shell_exec", "arguments":{"command":"sleep 1", "background":true}},
                "worker":{"taskId":"job-task"}
            }]
        })).unwrap();
        let store = Arc::new(SessionStore::from_state(SessionStoreState {
            canonical_turns: vec![turn],
            ..Default::default()
        }));
        store
            .create_session_for_workspace(
                session_id.clone(),
                "Job test",
                Some("job-workspace".into()),
            )
            .unwrap();
        let writer = Arc::new(Writer::default());
        store.install_session_event_writer(writer.clone());
        let context = ToolExecutionContext {
            session_id: Some(session_id),
            task_id: Some(TaskId::new("job-task")),
            workspace_id: Some(magi_core::WorkspaceId::new("job-workspace")),
            working_directory: Some(std::env::temp_dir()),
            ..Default::default()
        };
        (
            store,
            writer,
            context,
            std::env::temp_dir().join(EventId::unique("job-output-test").to_string()),
        )
    }

    #[cfg(unix)]
    #[test]
    fn background_job_start_and_completion_failures_keep_original_settlement_for_retry() {
        use magi_tool_runtime::{
            ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
        };
        use std::time::{Duration, Instant};

        struct FaultWriter {
            cancel_at: Option<BackgroundJobStatus>,
            fail_running: bool,
            reject: AtomicBool,
            pending_terminal: Mutex<Option<BackgroundJob>>,
        }
        impl super::super::SessionEventWriter for FaultWriter {
            fn append_session_transaction(
                &self,
                _: &SessionId,
                turns: &[CanonicalTurnMutation],
                _: &[super::super::SessionInputMutation],
                _: Option<(&crate::SessionAcceptanceRecord, Option<&magi_core::Task>)>,
            ) -> magi_core::DomainResult<()> {
                for item in turns.iter().flat_map(|turn| &turn.next.items) {
                    let Some(job) = item.background_job()? else {
                        continue;
                    };
                    let registry = ToolRegistry::new(
                        Arc::new(magi_governance::GovernanceService::default()),
                        Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
                    );
                    let query = ToolExecutionContextQuery {
                        session_id: Some(job.session_id.clone()),
                        ..Default::default()
                    };
                    if self.cancel_at == Some(job.status) {
                        registry.cancel_active_executions(&query);
                    }
                    if self.reject.load(Ordering::SeqCst)
                        && (job.status.is_terminal()
                            || (self.fail_running && job.status == BackgroundJobStatus::Running))
                    {
                        assert!(
                            registry.has_active_executions(&query),
                            "writer IO 期间不能宣称资源已释放"
                        );
                        if job.status.is_terminal() {
                            self.pending_terminal.lock().unwrap().get_or_insert(job);
                        }
                        return Err(DomainError::InvalidState {
                            message: "Job settlement disk unavailable".into(),
                        });
                    }
                }
                Ok(())
            }
        }

        for (cancel_at, fail_running, expected, delayed) in [
            (
                Some(BackgroundJobStatus::Starting),
                false,
                BackgroundJobStatus::Cancelled,
                false,
            ),
            (
                Some(BackgroundJobStatus::Running),
                false,
                BackgroundJobStatus::Cancelled,
                false,
            ),
            (None, true, BackgroundJobStatus::OutcomeUnknown, false),
            (None, false, BackgroundJobStatus::Completed, false),
            (None, false, BackgroundJobStatus::Completed, true),
            (None, false, BackgroundJobStatus::Failed, false),
            (None, false, BackgroundJobStatus::Failed, true),
        ] {
            let (store, _, mut context, output) = fixture();
            std::fs::create_dir_all(&output).unwrap();
            context.working_directory = Some(output.clone());
            let writer = Arc::new(FaultWriter {
                cancel_at,
                fail_running,
                reject: AtomicBool::new(true),
                pending_terminal: Mutex::new(None),
            });
            store.install_session_event_writer(writer.clone());
            let journal = store.background_job_journal(output.clone());
            let mut registry = ToolRegistry::new(
                Arc::new(magi_governance::GovernanceService::default()),
                Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            )
            .with_background_job_journal(journal.clone());
            registry.register_default_builtins();
            let invoke = |arguments: serde_json::Value| {
                registry.execute_with_policy(
                    ToolExecutionInput::for_builtin_invocation(
                        magi_core::ToolCallId::new("job-call"),
                        "shell_exec",
                        arguments.to_string(),
                    ),
                    context.clone(),
                    &ToolExecutionPolicy {
                        access_profile: magi_core::AccessProfile::FullAccess,
                        ..Default::default()
                    },
                )
            };
            let finishes = matches!(
                expected,
                BackgroundJobStatus::Completed | BackgroundJobStatus::Failed
            );
            let command = if finishes {
                format!(
                    "sleep {}; printf done >> effect.txt; exit {}",
                    if delayed { "0.5" } else { "0" },
                    if expected == BackgroundJobStatus::Failed {
                        7
                    } else {
                        0
                    }
                )
            } else {
                "sleep 30; printf orphan >> effect.txt".into()
            };
            let launch = invoke(serde_json::json!({"command":command, "background":true}));
            let deadline = Instant::now() + Duration::from_secs(5);
            while writer.pending_terminal.lock().unwrap().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "必须尝试提交结算：{}",
                    launch.payload
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let query = ToolExecutionContextQuery {
                session_id: context.session_id.clone(),
                ..Default::default()
            };
            let pending = registry.has_active_executions(&query);
            let before = journal.list(&context).unwrap().remove(0);
            std::thread::sleep(Duration::from_millis(10));
            writer.reject.store(false, Ordering::SeqCst);
            let retry = invoke(serde_json::json!({"action":"kill", "job_id":before.job_id}));
            assert_eq!(
                retry.status,
                magi_core::ExecutionResultStatus::Succeeded,
                "{}",
                retry.payload
            );
            assert!(pending, "写盘失败后仍须阻止冲突的新工作");
            if !finishes || !delayed {
                assert_eq!(
                    launch.status,
                    magi_core::ExecutionResultStatus::Failed,
                    "{}",
                    launch.payload
                );
            }
            assert!(!before.status.is_terminal());
            let settled = journal.list(&context).unwrap().remove(0);
            assert_eq!(
                Some(&settled),
                writer.pending_terminal.lock().unwrap().as_ref(),
                "补交必须使用原终态的完整内容"
            );
            assert_eq!(settled.status, expected, "kill 重试只能补交原结果");
            if finishes {
                assert_eq!(
                    settled.exit_code,
                    Some(if expected == BackgroundJobStatus::Failed {
                        7
                    } else {
                        0
                    })
                );
            }
            assert_eq!(settled.job_id, before.job_id);
            assert_eq!(settled.revision, before.revision + 1);
            let repeated = invoke(serde_json::json!({"action":"kill", "job_id":settled.job_id}));
            if settled.status == BackgroundJobStatus::OutcomeUnknown {
                assert_eq!(
                    repeated.status,
                    magi_core::ExecutionResultStatus::Failed,
                    "{}",
                    repeated.payload
                );
            } else {
                assert_eq!(
                    repeated.status,
                    magi_core::ExecutionResultStatus::Succeeded,
                    "{}",
                    repeated.payload
                );
                let receipt: serde_json::Value = serde_json::from_str(&repeated.payload).unwrap();
                assert_eq!(receipt["resources_settled"], true);
                assert_eq!(
                    receipt["job"],
                    serde_json::to_value(&settled).unwrap(),
                    "停止成功不改写原命令结果"
                );
            }
            assert_eq!(
                journal.list(&context).unwrap()[0],
                settled,
                "重复停止只读原终态，不能改写时间、退出码或 revision"
            );
            assert!(!registry.has_active_executions(&query));
            assert_eq!(
                std::fs::read_to_string(output.join("effect.txt")).ok(),
                finishes.then(|| "done".to_string())
            );
            std::fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn background_job_stop_rechecks_a_snapshot_taken_before_settlement() {
        use magi_tool_runtime::{ToolExecutionInput, ToolExecutionPolicy, ToolRegistry};

        struct SettleAfterSnapshot {
            journal: Arc<dyn BackgroundJobJournal>,
            settle: AtomicBool,
            status: BackgroundJobStatus,
        }
        impl BackgroundJobJournal for SettleAfterSnapshot {
            fn begin(
                &self,
                _: &ToolExecutionContext,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<BackgroundJob, String> {
                panic!("停止不得重放命令")
            }
            fn commit(&self, _: &BackgroundJob) -> Result<(), String> {
                panic!("已结算的停止不得改写事实")
            }
            fn list(&self, context: &ToolExecutionContext) -> Result<Vec<BackgroundJob>, String> {
                let snapshot = self.journal.list(context)?;
                if self.settle.swap(false, Ordering::SeqCst) {
                    let mut settled = snapshot[0].clone();
                    settled.revision += 1;
                    settled.status = self.status;
                    settled.exit_code = match self.status {
                        BackgroundJobStatus::Completed => Some(0),
                        BackgroundJobStatus::Failed => Some(7),
                        _ => None,
                    };
                    settled.settled_at = Some(UtcMillis::now());
                    self.journal.commit(&settled)?;
                }
                Ok(snapshot)
            }
            fn wait_for_terminal(
                &self,
                _: &ToolExecutionContext,
                _: &str,
                _: std::time::Duration,
                _: Option<&AtomicBool>,
            ) -> Result<Option<BackgroundJob>, String> {
                panic!("已结算的停止不得重新等待")
            }
        }

        for status in [
            BackgroundJobStatus::Completed,
            BackgroundJobStatus::Failed,
            BackgroundJobStatus::Cancelled,
            BackgroundJobStatus::OutcomeUnknown,
        ] {
            let (store, _, context, output) = fixture();
            let journal = store.background_job_journal(output);
            let mut job = journal
                .begin(&context, "job-call", "sleep 1", "/workspace")
                .unwrap();
            job.revision += 1;
            job.status = BackgroundJobStatus::Running;
            journal.commit(&job).unwrap();
            let mut registry = ToolRegistry::new(
                Arc::new(magi_governance::GovernanceService::default()),
                Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            )
            .with_background_job_journal(Arc::new(SettleAfterSnapshot {
                journal: journal.clone(),
                settle: AtomicBool::new(true),
                status,
            }));
            registry.register_default_builtins();
            let stopped = registry.execute_with_policy(
                ToolExecutionInput::for_builtin_invocation(
                    magi_core::ToolCallId::new("stop-job"),
                    "shell_exec",
                    serde_json::json!({"action":"kill", "job_id":job.job_id}).to_string(),
                ),
                context.clone(),
                &ToolExecutionPolicy {
                    access_profile: magi_core::AccessProfile::FullAccess,
                    ..Default::default()
                },
            );
            let settled = journal.list(&context).unwrap().remove(0);
            assert_eq!(settled.status, status);
            let receipt: serde_json::Value = serde_json::from_str(&stopped.payload).unwrap();
            if status == BackgroundJobStatus::OutcomeUnknown {
                assert_eq!(
                    stopped.status,
                    magi_core::ExecutionResultStatus::Failed,
                    "{}",
                    stopped.payload
                );
                assert_eq!(receipt["error_code"], "shell_exec_job_not_attached");
                assert_ne!(receipt["resources_settled"], true);
            } else {
                assert_eq!(
                    stopped.status,
                    magi_core::ExecutionResultStatus::Succeeded,
                    "{}",
                    stopped.payload
                );
                assert_eq!(receipt["resources_settled"], true);
                assert_eq!(receipt["job"], serde_json::to_value(&settled).unwrap());
            }
            assert_eq!(journal.list(&context).unwrap()[0], settled);
            assert_eq!(settled.revision, job.revision + 1);
        }
    }

    #[test]
    fn background_job_writes_are_atomic_and_replay_does_not_launch_again() {
        let (store, writer, context, output) = fixture();
        let journal = store.background_job_journal(output);
        writer.fail.store(true, Ordering::SeqCst);
        assert!(
            journal
                .begin(&context, "job-call", "sleep 1", "/workspace")
                .is_err()
        );
        assert!(
            store
                .background_jobs_for_session(context.session_id.as_ref().unwrap())
                .unwrap()
                .is_empty()
        );
        writer.fail.store(false, Ordering::SeqCst);
        let initial = journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        assert!(
            journal
                .begin(&context, "job-call", "sleep 1", "/workspace")
                .is_err()
        );
        let mut next = initial.clone();
        next.revision += 1;
        next.status = BackgroundJobStatus::Running;
        writer.fail.store(true, Ordering::SeqCst);
        assert!(journal.commit(&next).is_err());
        assert_eq!(journal.list(&context).unwrap(), vec![initial]);
        writer.fail.store(false, Ordering::SeqCst);
        journal.commit(&next).unwrap();
        journal.commit(&next).unwrap();
        assert_eq!(
            writer.mutations.lock().unwrap().len(),
            2,
            "幂等重放不重复追加事实"
        );
        next.revision += 1;
        next.command = "different command".into();
        assert!(journal.commit(&next).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn background_job_settlement_releases_the_process_tree_before_reporting_settled() {
        use magi_tool_runtime::{
            ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
        };
        for (delay, ignore_term, inherit_output, exit_code) in
            [("0", false), ("0.5", false), ("0.5", true)]
                .into_iter()
                .flat_map(|(delay, ignore_term)| {
                    [false, true].map(|inherit| (delay, ignore_term, inherit))
                })
                .flat_map(|(delay, ignore_term, inherit)| {
                    [0, 7].map(|code| (delay, ignore_term, inherit, Some(code)))
                })
                .chain([("0", true, true, None), ("0", false, false, None)])
        {
            let (store, _, mut context, output) = fixture();
            std::fs::create_dir_all(&output).unwrap();
            context.working_directory = Some(output.clone());
            let journal = store.background_job_journal(output.clone());
            let mut registry = ToolRegistry::new(
                Arc::new(magi_governance::GovernanceService::default()),
                Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            )
            .with_background_job_journal(journal.clone());
            registry.register_default_builtins();
            let trap = if ignore_term { "trap \"\" TERM; " } else { "" };
            let redirect = if inherit_output {
                ""
            } else {
                " >/dev/null 2>&1"
            };
            let child = format!(
                "sh -c '{trap}while test ! -f release.txt; do sleep 0.01; done; printf orphan > orphan.txt'{redirect} & printf done; printf diagnostic >&2;"
            );
            let command = match exit_code {
                Some(code) => format!("sleep {delay}; {child} exit {code}"),
                None => format!("{child} sleep 30"),
            };
            let launch = registry.execute_with_policy(
                ToolExecutionInput::for_builtin_invocation(
                    magi_core::ToolCallId::new("job-call"),
                    "shell_exec",
                    serde_json::json!({
                        "command":command, "background":true,
                    })
                    .to_string(),
                ),
                context.clone(),
                &ToolExecutionPolicy {
                    access_profile: magi_core::AccessProfile::FullAccess,
                    ..Default::default()
                },
            );
            let launch_payload: serde_json::Value = serde_json::from_str(&launch.payload).unwrap();
            assert_eq!(
                launch.status,
                if launch_payload["exit_code"] == 7 {
                    magi_core::ExecutionResultStatus::Failed
                } else {
                    magi_core::ExecutionResultStatus::Succeeded
                },
                "{}",
                launch.payload
            );
            let job = journal.list(&context).unwrap().remove(0);
            if exit_code.is_none() {
                let stopped = registry.execute_with_policy(
                    ToolExecutionInput::for_builtin_invocation(
                        magi_core::ToolCallId::new("job-stop"),
                        "shell_exec",
                        serde_json::json!({"action":"kill", "job_id":job.job_id}).to_string(),
                    ),
                    context.clone(),
                    &ToolExecutionPolicy {
                        access_profile: magi_core::AccessProfile::FullAccess,
                        ..Default::default()
                    },
                );
                let receipt: serde_json::Value = serde_json::from_str(&stopped.payload).unwrap();
                assert_eq!(receipt["resources_settled"], true, "{}", stopped.payload);
            }
            let settled = journal
                .wait_for_terminal(
                    &context,
                    &job.job_id,
                    std::time::Duration::from_secs(5),
                    None,
                )
                .unwrap()
                .unwrap();
            let query = ToolExecutionContextQuery {
                session_id: context.session_id.clone(),
                ..Default::default()
            };
            // canonical 终态发布后，原 journal 调用仍可能在返回给进程句柄；
            // 资源屏障完成与结果写入分别观察，不能用一次瞬时读取猜测泄漏。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while registry.has_active_executions(&query) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let active = registry.has_active_executions(&query);
            std::fs::write(output.join("release.txt"), "ready").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let orphan = output.join("orphan.txt").exists();
            registry.cancel_active_executions(&ToolExecutionContextQuery {
                session_id: context.session_id.clone(),
                ..Default::default()
            });
            assert_eq!(
                std::fs::read_to_string(&settled.stdout_path).unwrap(),
                "done"
            );
            assert!(
                std::fs::read_to_string(&settled.stderr_path)
                    .unwrap()
                    .contains("diagnostic")
            );
            std::fs::remove_dir_all(output).unwrap();
            assert_eq!(settled.exit_code, exit_code);
            assert_eq!(
                settled.status,
                match exit_code {
                    Some(0) => BackgroundJobStatus::Completed,
                    Some(_) => BackgroundJobStatus::Failed,
                    None => BackgroundJobStatus::Cancelled,
                }
            );
            assert!(
                !active,
                "Job 结算后仍有活动资源: delay={delay}, ignore_term={ignore_term}, inherit_output={inherit_output}, exit_code={exit_code:?}, job={settled:?}"
            );
            assert!(
                !orphan,
                "Job 已回报结算，派生进程却仍可写入工作区: delay={delay}, ignore_term={ignore_term}, inherit_output={inherit_output}, launch={}",
                launch.payload
            );
        }
    }

    #[test]
    fn background_job_restart_preserves_outputs_and_marks_unknown_once() {
        let (store, _, context, output) = fixture();
        let journal = store.background_job_journal(output.clone());
        let job = journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        let restored = Arc::new(SessionStore::from_state(store.export_state()));
        assert_eq!(
            restored.reconcile_background_jobs_after_restart().unwrap(),
            1
        );
        assert_eq!(
            restored.reconcile_background_jobs_after_restart().unwrap(),
            0
        );
        let records = restored
            .background_job_journal(output)
            .list(&context)
            .unwrap();
        assert_eq!(records[0].status, BackgroundJobStatus::OutcomeUnknown);
        assert_eq!(records[0].job_id, job.job_id);
        assert_eq!(records[0].stdout_path, job.stdout_path);
        assert_eq!(records[0].revision, 2);
        let mut item = restored
            .canonical_turn_for_session_turn_id(context.session_id.as_ref().unwrap(), "job-turn")
            .unwrap()
            .items[0]
            .clone();
        let old = item.clone();
        item.metadata.remove(BACKGROUND_JOB_METADATA_KEY);
        assert!(item.validate_update_from(&old).is_err());
    }

    #[test]
    fn background_job_history_is_scoped_to_session_and_activation() {
        let (store, _, context, output) = fixture();
        let journal = store.background_job_journal(output);
        journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        let sibling = ToolExecutionContext {
            caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
            task_id: Some(TaskId::new("sibling-or-new-activation")),
            ..context.clone()
        };
        assert!(journal.list(&sibling).unwrap().is_empty());
        let other = ToolExecutionContext {
            session_id: Some(SessionId::new("other-session")),
            ..context.clone()
        };
        assert!(journal.list(&other).unwrap().is_empty());
        assert_eq!(journal.list(&context).unwrap().len(), 1);
    }

    #[test]
    fn background_job_wait_observes_committed_terminal_without_losing_notifications() {
        use std::{thread, time::Duration};
        let (store, writer, context, output) = fixture();
        let journal = store.background_job_journal(output);
        let mut job = journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        let timeout = Duration::from_secs(2);
        thread::scope(|scope| {
            let waiting =
                scope.spawn(|| journal.wait_for_terminal(&context, &job.job_id, timeout, None));
            let mut terminal = job.clone();
            terminal.revision += 1;
            terminal.status = BackgroundJobStatus::Completed;
            terminal.exit_code = Some(0);
            terminal.settled_at = Some(UtcMillis::now());
            writer.fail.store(true, Ordering::SeqCst);
            assert!(journal.commit(&terminal).is_err());
            assert_eq!(
                journal.list(&context).unwrap()[0].status,
                BackgroundJobStatus::Starting
            );
            writer.fail.store(false, Ordering::SeqCst);
            journal.commit(&terminal).unwrap();
            assert_eq!(waiting.join().unwrap().unwrap(), Some(terminal));
        });
        job = journal.list(&context).unwrap().remove(0);
        // 终态早于订阅也立即返回，不必等待下一次通知。
        assert_eq!(
            journal
                .wait_for_terminal(&context, &job.job_id, timeout, None)
                .unwrap(),
            Some(job)
        );
    }

    #[test]
    fn background_job_wait_timeout_cancellation_and_scope_do_not_change_the_job() {
        use std::{
            thread,
            time::{Duration, Instant},
        };
        let (store, _, context, output) = fixture();
        let journal = store.background_job_journal(output);
        let job = journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        assert_eq!(
            journal
                .wait_for_terminal(&context, &job.job_id, Duration::from_millis(5), None)
                .unwrap(),
            Some(job.clone())
        );
        let sibling = ToolExecutionContext {
            caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
            task_id: Some(TaskId::new("sibling")),
            ..context.clone()
        };
        assert!(
            journal
                .wait_for_terminal(&sibling, &job.job_id, Duration::from_secs(1), None)
                .is_err()
        );
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                journal.wait_for_terminal(
                    &context,
                    &job.job_id,
                    Duration::from_secs(30),
                    Some(&cancelled),
                )
            });
            cancelled.store(true, Ordering::SeqCst);
            assert_eq!(waiting.join().unwrap().unwrap(), None);
        });
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(journal.list(&context).unwrap(), vec![job]);
    }

    #[test]
    fn background_job_wait_cancel_is_independent_of_a_live_process_connection() {
        use std::{
            thread,
            time::{Duration, Instant},
        };
        let (store, _, context, output) = fixture();
        let journal = store.background_job_journal(output);
        let job = journal
            .begin(&context, "job-call", "sleep 1", "/workspace")
            .unwrap();
        let mut registry = magi_tool_runtime::ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_background_job_journal(journal.clone());
        registry.register_default_builtins();
        thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                registry.execute_with_context(
                    magi_tool_runtime::ToolExecutionInput::for_builtin_invocation(
                        magi_core::ToolCallId::new("wait-without-process"),
                        "shell_exec",
                        serde_json::json!({"action":"wait","job_id":job.job_id,"timeout_ms":60000})
                            .to_string(),
                    ),
                    context.clone(),
                )
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if waiting.is_finished() {
                    panic!("等待调用提前返回：{:?}", waiting.join().unwrap());
                }
                if registry.cancel_active_executions(
                    &magi_tool_runtime::ToolExecutionContextQuery {
                        session_id: context.session_id.clone(),
                        task_id: context.task_id.clone(),
                        ..Default::default()
                    },
                ) == 1
                {
                    break;
                }
                assert!(Instant::now() < deadline, "等待调用必须登记自己的取消句柄");
                thread::sleep(Duration::from_millis(1));
            }
            let result = waiting.join().unwrap();
            assert_eq!(
                result.status,
                magi_core::ExecutionResultStatus::Cancelled,
                "{}",
                result.payload
            );
        });
        assert_eq!(journal.list(&context).unwrap(), vec![job]);
    }
}
