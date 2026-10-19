//! 后台进程的持久事实合同。进程句柄仍由 process runtime 持有；journal 由 daemon 注入。
use crate::{ToolCallerScope, ToolExecutionContext};
use magi_core::{SessionId, TaskId, UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const BACKGROUND_JOB_METADATA_KEY: &str = "backgroundJob";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
    OutcomeUnknown,
}

impl BackgroundJobStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundJob {
    pub job_id: String,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub task_id: TaskId,
    pub turn_id: String,
    pub item_id: String,
    pub tool_call_id: String,
    pub revision: u64,
    pub command: String,
    pub cwd: String,
    pub status: BackgroundJobStatus,
    pub started_at: UtcMillis,
    pub settled_at: Option<UtcMillis>,
    pub exit_code: Option<i32>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

impl BackgroundJob {
    pub fn belongs_to(&self, context: &ToolExecutionContext) -> bool {
        context.session_id.as_ref() == Some(&self.session_id)
            && context.workspace_id == self.workspace_id
            && (context.caller_scope == ToolCallerScope::SessionMainline
                || context.task_id.as_ref() == Some(&self.task_id))
    }

    pub fn validate_snapshot(&self) -> Result<(), String> {
        if self.job_id.trim().is_empty()
            || !self
                .job_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || self.turn_id.trim().is_empty()
            || self.item_id.trim().is_empty()
            || self.tool_call_id.trim().is_empty()
            || self.session_id.as_str().trim().is_empty()
            || self.task_id.as_str().trim().is_empty()
            || self.revision == 0
            || !self.stdout_path.is_absolute()
            || !self.stderr_path.is_absolute()
            || self.status.is_terminal() != self.settled_at.is_some()
            || (self.status == BackgroundJobStatus::Completed && self.exit_code != Some(0))
            || (!self.status.is_terminal() && self.exit_code.is_some())
            || self.settled_at.is_some_and(|at| at.0 < self.started_at.0)
        {
            return Err("后台 Job 身份或状态无效".into());
        }
        Ok(())
    }

    pub fn validate_update_from(&self, old: Option<&Self>) -> Result<(), String> {
        self.validate_snapshot()?;
        let Some(old) = old else {
            return if self.revision == 1 && self.status == BackgroundJobStatus::Starting {
                Ok(())
            } else {
                Err("后台 Job 必须从 Starting revision 1 开始".into())
            };
        };
        let mut expected = old.clone();
        expected.revision = self.revision;
        expected.status = self.status;
        expected.settled_at = self.settled_at;
        expected.exit_code = self.exit_code;
        if expected != *self
            || old.revision.checked_add(1) != Some(self.revision)
            || old.status.is_terminal()
            || (old.status == BackgroundJobStatus::Running
                && self.status == BackgroundJobStatus::Starting)
        {
            return Err("后台 Job 来源、revision 或状态冲突".into());
        }
        Ok(())
    }
}

pub trait BackgroundJobJournal: Send + Sync {
    /// 在启动操作系统进程前提交 Starting 和输出位置；失败时不得执行命令。
    fn begin(
        &self,
        context: &ToolExecutionContext,
        tool_call_id: &str,
        command: &str,
        cwd: &str,
    ) -> Result<BackgroundJob, String>;
    fn commit(&self, job: &BackgroundJob) -> Result<(), String>;
    fn list(&self, context: &ToolExecutionContext) -> Result<Vec<BackgroundJob>, String>;
    /// 等待已提交的 Job 终态；超时返回当前事实，取消返回 None，不调用模型或重启进程。
    fn wait_for_terminal(
        &self,
        context: &ToolExecutionContext,
        job_id: &str,
        timeout: std::time::Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<BackgroundJob>, String>;
}

pub(crate) struct DurableProcessJob {
    cancelling: AtomicBool,
    state: Mutex<ProcessJobState>,
    journal: Arc<dyn BackgroundJobJournal>,
}

struct ProcessJobState {
    record: BackgroundJob,
    /// 未提交的终态仅供原句柄补交，不能覆盖已提交事实或改写成另一个结果。
    pending_terminal: Option<BackgroundJob>,
}

impl DurableProcessJob {
    pub(crate) fn new(record: BackgroundJob, journal: Arc<dyn BackgroundJobJournal>) -> Self {
        Self {
            cancelling: AtomicBool::new(false),
            state: Mutex::new(ProcessJobState {
                record,
                pending_terminal: None,
            }),
            journal,
        }
    }
    pub(crate) fn request_cancel(&self) {
        self.cancelling.store(true, Ordering::SeqCst);
    }
    pub(crate) fn is_cancelling(&self) -> bool {
        self.cancelling.load(Ordering::SeqCst)
    }
    pub(crate) fn snapshot(&self) -> BackgroundJob {
        self.state.lock().expect("job lock").record.clone()
    }
    pub(crate) fn is_unsettled(&self) -> bool {
        // journal IO 期间也保持屏障；查询不能持有进程表锁等待 writer 回调。
        self.state
            .try_lock()
            .map_or(true, |state| !state.record.status.is_terminal())
    }
    pub(crate) fn transition(
        &self,
        status: BackgroundJobStatus,
        exit_code: Option<i32>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("job lock");
        if state.record.status.is_terminal() {
            return Ok(());
        }
        let next = match &state.pending_terminal {
            Some(pending) => pending.clone(),
            None => {
                let mut next = state.record.clone();
                next.revision = next.revision.checked_add(1).ok_or("Job revision 溢出")?;
                next.status = status;
                next.exit_code = exit_code;
                next.settled_at = status
                    .is_terminal()
                    .then(|| UtcMillis(UtcMillis::now().0.max(next.started_at.0)));
                next.validate_update_from(Some(&state.record))?;
                if status.is_terminal() {
                    state.pending_terminal = Some(next.clone());
                }
                next
            }
        };
        self.journal.commit(&next)?;
        state.record = next;
        state.pending_terminal = None;
        Ok(())
    }
}
