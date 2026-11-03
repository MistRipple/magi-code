//! 原生插件计划的持久事实。执行事实只引用 canonical Turn，不复制正文、任务或用量。
use magi_core::fs_atomic::write_atomic;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub use magi_app_server_protocol::generated::{
    PluginJobBudget as JobBudget, PluginJobContext as JobContext, PluginJobRequest as JobRequest,
    PluginJobSchedule as JobSchedule,
};

pub fn validate_request(request: &JobRequest) -> Result<(), String> {
    let budget = &request.budget;
    if budget.max_model_calls > 100
        || !(1..=32768).contains(&budget.max_output_tokens)
        || !(1000..=120000).contains(&budget.timeout_ms)
    {
        return Err(
            "作业预算超出范围：调用 0..100，单次输出 1..32768 token，期限 1000..120000ms".into(),
        );
    }
    if request.text.len() > 65536
        || request.session_id.is_empty()
        || request.session_id.len() > 256
        || request.command_id.is_empty()
        || request.command_id.len() > 64
        || !(0..=9_007_199_254_740_991).contains(&request.run_at)
        || request
            .interval_ms
            .is_some_and(|ms| !(60000..=31_536_000_000).contains(&ms))
    {
        return Err("作业参数无效：文字最多 64KiB，重复间隔至少一分钟".into());
    }
    Ok(())
}
pub fn context(schedule: &JobSchedule) -> JobContext {
    JobContext {
        job_id: schedule.pending_request_id.clone().expect("claimed job"),
        schedule_id: schedule.id.clone(),
        budget: schedule.request.budget.clone(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u16,
    schedules: BTreeMap<String, JobSchedule>,
}
pub struct JobStore {
    path: PathBuf,
    state: State,
}
impl JobStore {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                version: 1,
                schedules: BTreeMap::new(),
            },
            Err(e) => return Err(e.to_string()),
        };
        if state.version != 1 || state.schedules.len() > 512 {
            return Err("插件计划状态版本或数量无效".into());
        }
        for (id, schedule) in &state.schedules {
            validate_request(&schedule.request)?;
            if id != &schedule.id || schedule.digest.is_empty() {
                return Err("插件计划状态无效".into());
            }
        }
        Ok(Self { path, state })
    }
    fn commit(&mut self, state: State) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        write_atomic(
            &self.path,
            serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.state = state;
        Ok(())
    }
    pub fn list(&self) -> Vec<JobSchedule> {
        self.state.schedules.values().cloned().collect()
    }
    pub fn insert(
        &mut self,
        plugin: &str,
        digest: &str,
        request: JobRequest,
    ) -> Result<JobSchedule, String> {
        validate_request(&request)?;
        if self.state.schedules.len() >= 512
            || self
                .state
                .schedules
                .values()
                .filter(|s| s.plugin_id == plugin)
                .count()
                >= 32
        {
            return Err("插件计划数量已达上限".into());
        }
        let schedule = JobSchedule {
            id: magi_core::EventId::unique("plugin-schedule").to_string(),
            plugin_id: plugin.into(),
            digest: digest.into(),
            next_run_at: request.run_at,
            request,
            enabled: true,
            run_number: 0,
            pending_request_id: None,
            last_turn_id: None,
            last_outcome: None,
        };
        let mut state = self.state.clone();
        state
            .schedules
            .insert(schedule.id.clone(), schedule.clone());
        self.commit(state)?;
        Ok(schedule)
    }
    /// 在产生任何执行副作用前持久化幂等身份；重启后继续使用同一身份，绝不重复启动。
    pub fn claim(&mut self, id: &str, now: i64) -> Result<Option<JobSchedule>, String> {
        let mut state = self.state.clone();
        let schedule = state.schedules.get_mut(id).ok_or("计划不存在")?;
        if !schedule.enabled || schedule.next_run_at > now {
            return Ok(None);
        }
        if schedule.pending_request_id.is_none() {
            schedule.run_number = schedule.run_number.checked_add(1).ok_or("作业计数耗尽")?;
            schedule.pending_request_id = Some(format!("{}-{}", schedule.id, schedule.run_number));
            let result = schedule.clone();
            self.commit(state)?;
            return Ok(Some(result));
        }
        Ok(Some(schedule.clone()))
    }
    pub fn finish(
        &mut self,
        id: &str,
        request: &str,
        turn: Option<String>,
        outcome: String,
        now: i64,
    ) -> Result<(), String> {
        let mut state = self.state.clone();
        let schedule = state.schedules.get_mut(id).ok_or("计划不存在")?;
        if schedule.pending_request_id.as_deref() != Some(request) {
            return Err("过期作业回执".into());
        }
        schedule.pending_request_id = None;
        schedule.last_turn_id = turn;
        schedule.last_outcome = Some(outcome.clone());
        // 失败、取消和结果不确定不自动重试；恢复需用户重新创建计划。
        if outcome != "completed" || schedule.request.interval_ms.is_none() {
            schedule.enabled = false;
        }
        if let Some(interval) = schedule.request.interval_ms {
            schedule.next_run_at = now.saturating_add(interval);
        }
        self.commit(state)
    }
    pub fn disable(&mut self, plugin: &str, id: &str) -> Result<JobSchedule, String> {
        let mut state = self.state.clone();
        let schedule = state
            .schedules
            .get_mut(id)
            .filter(|s| s.plugin_id == plugin)
            .ok_or("计划不存在")?;
        schedule.enabled = false;
        let result = schedule.clone();
        self.commit(state)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> JobRequest {
        JobRequest {
            command_id: "status".into(),
            session_id: "session".into(),
            text: "hello".into(),
            run_at: 100,
            interval_ms: Some(60000),
            budget: JobBudget {
                max_model_calls: 2,
                max_output_tokens: 512,
                timeout_ms: 5000,
            },
        }
    }
    #[test]
    fn claiming_is_durable_idempotent_and_failed_runs_do_not_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        let mut store = JobStore::open(path.clone()).unwrap();
        let schedule = store.insert("acme.test", "digest", request()).unwrap();
        assert!(store.claim(&schedule.id, 99).unwrap().is_none());
        let claimed = store.claim(&schedule.id, 100).unwrap().unwrap();
        drop(store);
        let mut store = JobStore::open(path).unwrap();
        assert_eq!(store.claim(&schedule.id, 101).unwrap().unwrap(), claimed);
        let request = claimed.pending_request_id.unwrap();
        assert!(
            store
                .finish(&schedule.id, "stale", None, "completed".into(), 101)
                .is_err()
        );
        store
            .finish(
                &schedule.id,
                &request,
                Some("turn".into()),
                "failed".into(),
                102,
            )
            .unwrap();
        assert!(store.claim(&schedule.id, 999999).unwrap().is_none());
    }
    #[test]
    fn corrupt_state_is_never_overwritten_and_persistence_failure_does_not_claim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        std::fs::write(&path, "broken").unwrap();
        assert!(JobStore::open(path.clone()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_file(&path).unwrap();
        let mut store = JobStore::open(path.clone()).unwrap();
        let schedule = store.insert("acme.test", "digest", request()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(store.claim(&schedule.id, 100).is_err());
        assert!(store.list()[0].pending_request_id.is_none());
    }
}
