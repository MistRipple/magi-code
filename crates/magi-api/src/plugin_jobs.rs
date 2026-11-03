//! daemon 原生调度：计划归 JobStore；每次执行沿 TurnService/Coordinator 生成唯一 canonical Turn。
use crate::{ApiError, ApiState};
use magi_core::{SessionId, UtcMillis};
use magi_plugin_system::jobs::{JobRequest, JobSchedule, JobStore};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) struct PluginJobs {
    store: Mutex<Result<JobStore, String>>,
    started: AtomicBool,
    pub gate: tokio::sync::Mutex<()>,
}
impl PluginJobs {
    pub fn open(path: PathBuf) -> Self {
        Self {
            store: Mutex::new(JobStore::open(path)),
            started: AtomicBool::new(false),
            gate: tokio::sync::Mutex::new(()),
        }
    }
    fn with_store<T>(
        &self,
        f: impl FnOnce(&mut JobStore) -> Result<T, String>,
    ) -> Result<T, ApiError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| ApiError::Conflict("插件计划锁已损坏".into()))?;
        let store = store
            .as_mut()
            .map_err(|e| ApiError::Conflict(format!("插件计划状态不可用，原文件保留：{e}")))?;
        f(store).map_err(ApiError::Conflict)
    }
    pub fn list(&self, plugin: &str) -> Result<Vec<JobSchedule>, ApiError> {
        self.with_store(|s| {
            Ok(s.list()
                .into_iter()
                .filter(|j| j.plugin_id == plugin)
                .collect())
        })
    }
    pub fn create(
        &self,
        state: &ApiState,
        plugin: &str,
        request: JobRequest,
    ) -> Result<JobSchedule, ApiError> {
        magi_plugin_system::jobs::validate_request(&request).map_err(ApiError::InvalidInput)?;
        let session = state
            .session_store
            .session_record(&SessionId::new(&request.session_id))
            .ok_or_else(|| ApiError::InvalidInput("会话不存在".into()))?;
        if session.kind != magi_session_store::SessionKind::User {
            return Err(ApiError::Forbidden("计划只能使用普通用户会话".into()));
        }
        let manager = state
            .plugin_manager
            .lock()
            .map_err(|_| ApiError::Conflict("插件管理器不可用".into()))?;
        if !manager
            .command_contributions()
            .map_err(|e| ApiError::Conflict(e.to_string()))?
            .iter()
            .any(|(m, c)| m.id == plugin && c.id == request.command_id)
        {
            return Err(ApiError::Forbidden(
                "计划必须引用当前已激活插件的公开命令".into(),
            ));
        }
        let digest = manager
            .package(plugin)
            .map_err(|e| ApiError::Conflict(e.to_string()))?
            .digest()
            .to_string();
        self.with_store(|store| store.insert(plugin, &digest, request))
    }
    pub async fn cancel(
        &self,
        state: &ApiState,
        plugin: &str,
        id: &str,
    ) -> Result<JobSchedule, ApiError> {
        let _gate = self.gate.lock().await;
        let schedule = self.with_store(|store| store.disable(plugin, id))?;
        if let Some(turn) = find_turn(state, &schedule)
            && state.session_store.turn_execution_is_active(
                &SessionId::new(&schedule.request.session_id),
                &turn.turn_id,
            )
        {
            let workspace = state
                .session_store
                .session_record(&SessionId::new(&schedule.request.session_id))
                .and_then(|s| s.workspace_id);
            crate::turn_service::interrupt_session_turn_with_expected(
                state,
                &SessionId::new(&schedule.request.session_id),
                workspace.as_deref(),
                None,
                Some(&turn.turn_id),
            )
            .await?;
        }
        Ok(schedule)
    }
    pub fn start(self: &Arc<Self>, state: ApiState) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let jobs = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = jobs.tick(&state).await {
                    tracing::error!(?error, "插件调度失败");
                }
            }
        });
    }
    pub(crate) async fn tick(&self, state: &ApiState) -> Result<(), ApiError> {
        let _gate = self.gate.lock().await;
        let schedules = self.with_store(|store| Ok(store.list()))?;
        let now = UtcMillis::now().0 as i64;
        for previous in schedules {
            // 即使用户已禁用，也要从 canonical 收口已接纳的作业。
            if previous.pending_request_id.is_some() {
                if let Some(turn) = find_turn(state, &previous) {
                    if turn.status.is_terminal() {
                        self.with_store(|s| {
                            s.finish(
                                &previous.id,
                                previous.pending_request_id.as_deref().unwrap(),
                                Some(turn.turn_id),
                                serde_json::to_value(turn.status)
                                    .expect("status serializes")
                                    .as_str()
                                    .expect("status string")
                                    .to_string(),
                                now,
                            )
                        })?;
                    }
                    continue;
                }
                // 取消、停用或包摘要变化可能发生在 Turn 接纳前。此时不会再有
                // canonical Turn 可供轮询，必须结算这个 pending 身份，否则它会
                // 永久占住计划并在重启后一直显示“运行中”。
                if !previous.enabled {
                    self.with_store(|s| {
                        s.finish(
                            &previous.id,
                            previous.pending_request_id.as_deref().unwrap(),
                            None,
                            "cancelled_before_turn".into(),
                            now,
                        )
                    })?;
                    continue;
                }
            }
            if !previous.enabled || previous.next_run_at > now {
                continue;
            }
            let valid = {
                let manager = state
                    .plugin_manager
                    .lock()
                    .map_err(|_| ApiError::Conflict("插件管理器不可用".into()))?;
                manager.is_active(&previous.plugin_id).unwrap_or(false)
                    && manager
                        .package(&previous.plugin_id)
                        .is_ok_and(|p| p.digest() == previous.digest)
            };
            if !valid {
                self.with_store(|s| s.disable(&previous.plugin_id, &previous.id))?;
                continue;
            }
            // 宿主尚在启动时不消耗本次计划；失败状态可在设置页诊断。
            if !state
                .extension_status
                .as_ref()
                .and_then(|s| s.status(&previous.plugin_id))
                .is_some_and(|s| {
                    s.state == magi_app_server_protocol::generated::PluginRuntimeState::Running
                })
            {
                continue;
            }
            let Some(schedule) = self.with_store(|s| s.claim(&previous.id, now))? else {
                continue;
            };
            let request_id = schedule.pending_request_id.as_ref().unwrap();
            let session = SessionId::new(&schedule.request.session_id);
            let Some(record) = state.session_store.session_record(&session) else {
                self.with_store(|s| {
                    s.finish(
                        &schedule.id,
                        request_id,
                        None,
                        "session_deleted".into(),
                        now,
                    )
                })?;
                continue;
            };
            let request=serde_json::from_value::<crate::dto::SessionTurnRequestDto>(json!({
                "sessionId":session,"scope":if record.workspace_id.is_some(){"workspace"}else{"personal"},
                "workspaceId":record.workspace_id,"text":schedule.request.text,
                "pluginCommandId":format!("plugin/{}/{}",schedule.plugin_id,schedule.request.command_id),
                "requestId":request_id,"userMessageId":format!("user-{request_id}")
            })).map_err(|e|ApiError::InvalidInput(e.to_string()))?;
            match crate::turn_service::submit_plugin_job(
                state.clone(),
                request,
                magi_plugin_system::jobs::context(&schedule),
            )
            .await
            {
                Ok(_) => {}
                // 活动会话/队列冲突保留同一幂等身份，不打断用户正在做的工作。
                Err(ApiError::Conflict(_)) => {}
                Err(error) => {
                    // 接纳可能已经落盘：一律先从唯一 canonical 事实核对，不能贸然重跑。
                    if find_turn(state, &schedule).is_none() {
                        self.with_store(|s| {
                            s.finish(
                                &schedule.id,
                                request_id,
                                None,
                                format!("rejected: {error:?}"),
                                now,
                            )
                        })?;
                    }
                }
            }
        }
        Ok(())
    }
}
fn find_turn(
    state: &ApiState,
    schedule: &JobSchedule,
) -> Option<magi_session_store::CanonicalTurn> {
    let request = schedule.pending_request_id.as_deref()?;
    state
        .session_store
        .canonical_turns_for_session(&SessionId::new(&schedule.request.session_id))
        .into_iter()
        .find(|turn| {
            turn.items.iter().any(|item| {
                item.metadata.get("requestId").and_then(|v| v.as_str()) == Some(request)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_before_turn_admission_settles_the_pending_identity() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = PluginJobs::open(dir.path().join("jobs.json"));
        let request = JobRequest {
            command_id: "run".into(),
            session_id: "s".into(),
            text: "".into(),
            run_at: 0,
            interval_ms: None,
            budget: magi_plugin_system::jobs::JobBudget {
                max_model_calls: 0,
                max_output_tokens: 128,
                timeout_ms: 1000,
            },
        };
        let schedule = jobs
            .with_store(|s| s.insert("acme.job", "digest", request))
            .unwrap();
        jobs.with_store(|s| s.claim(&schedule.id, 1)).unwrap();
        jobs.with_store(|s| s.disable("acme.job", &schedule.id))
            .unwrap();
        let state = ApiState::new(
            "test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            Arc::new(magi_session_store::SessionStore::default()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        );
        jobs.tick(&state).await.unwrap();
        let actual = jobs.list("acme.job").unwrap().pop().unwrap();
        assert!(actual.pending_request_id.is_none());
        assert!(!actual.enabled);
        assert_eq!(
            actual.last_outcome.as_deref(),
            Some("cancelled_before_turn")
        );
        assert!(state.session_store.sessions().is_empty());
    }
}
