use std::collections::{HashMap, HashSet};

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAction {
    Resume,
    Rerun,
    Skip,
}

#[derive(Clone, Debug)]
pub struct ResumeWorkerDispatchContext {
    pub worker_slot: String,
    pub action: ResumeAction,
    pub assignment_id: Option<String>,
    pub worker_session_id: Option<String>,
    pub resume_prompt: Option<String>,
}

struct ResumeExecutionContext {
    _session_id: String,
    _source_mission_id: String,
    worker_contexts: HashMap<String, ResumeWorkerDispatchContext>,
    consumed_workers: HashSet<String>,
    _created_at: u64,
}

pub struct DispatchResumeContextStore {
    mission_worker_sessions: HashMap<String, HashMap<String, String>>,
    active_resume_contexts: HashMap<String, ResumeExecutionContext>,
    max_mission_session_records: usize,
}

impl DispatchResumeContextStore {
    pub fn new(max_mission_session_records: usize) -> Self {
        Self {
            mission_worker_sessions: HashMap::new(),
            active_resume_contexts: HashMap::new(),
            max_mission_session_records,
        }
    }

    pub fn activate(
        &mut self,
        current_session_id: &str,
        source_mission_id: &str,
        worker_actions: &[WorkerActionInput],
        resume_prompt: Option<&str>,
    ) -> Result<usize, ()> {
        let worker_sessions = self.mission_worker_sessions.get(source_mission_id);
        let mut worker_contexts = HashMap::new();

        for wa in worker_actions {
            let worker_session_id = wa
                .worker_session_id
                .clone()
                .or_else(|| {
                    worker_sessions
                        .and_then(|ws| ws.get(&wa.worker_slot))
                        .cloned()
                });

            worker_contexts.insert(
                wa.worker_slot.clone(),
                ResumeWorkerDispatchContext {
                    worker_slot: wa.worker_slot.clone(),
                    action: wa.action,
                    assignment_id: wa.assignment_id.clone(),
                    worker_session_id,
                    resume_prompt: resume_prompt.map(|s| s.to_string()),
                },
            );
        }

        if worker_contexts.is_empty() {
            return Err(());
        }

        let count = worker_contexts.len();
        self.active_resume_contexts.insert(
            current_session_id.to_string(),
            ResumeExecutionContext {
                _session_id: current_session_id.to_string(),
                _source_mission_id: source_mission_id.to_string(),
                worker_contexts,
                consumed_workers: HashSet::new(),
                _created_at: now_millis(),
            },
        );

        Ok(count)
    }

    pub fn clear(&mut self, current_session_id: Option<&str>) {
        match current_session_id {
            Some(id) => {
                self.active_resume_contexts.remove(id);
            }
            None => {
                self.active_resume_contexts.clear();
            }
        }
    }

    pub fn list(&self, current_session_id: &str) -> Vec<ResumeWorkerDispatchContext> {
        self.active_resume_contexts
            .get(current_session_id)
            .map(|ctx| ctx.worker_contexts.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn consume_for_worker(
        &mut self,
        current_session_id: &str,
        worker: &str,
    ) -> ConsumeResult {
        let Some(ctx) = self.active_resume_contexts.get_mut(current_session_id) else {
            return ConsumeResult {
                already_consumed: false,
                context: None,
            };
        };
        let Some(worker_ctx) = ctx.worker_contexts.get(worker) else {
            return ConsumeResult {
                already_consumed: false,
                context: None,
            };
        };
        if ctx.consumed_workers.contains(worker) {
            return ConsumeResult {
                already_consumed: true,
                context: Some(worker_ctx.clone()),
            };
        }
        ctx.consumed_workers.insert(worker.to_string());
        ConsumeResult {
            already_consumed: false,
            context: Some(worker_ctx.clone()),
        }
    }

    pub fn record_worker_session(
        &mut self,
        mission_id: &str,
        worker: &str,
        worker_session_id: &str,
    ) {
        if mission_id.is_empty() || worker_session_id.is_empty() {
            return;
        }
        let entry = self
            .mission_worker_sessions
            .entry(mission_id.to_string())
            .or_default();
        entry.insert(worker.to_string(), worker_session_id.to_string());

        if self.mission_worker_sessions.len() > self.max_mission_session_records {
            if let Some(oldest_key) = self.mission_worker_sessions.keys().next().cloned() {
                self.mission_worker_sessions.remove(&oldest_key);
            }
        }
    }

    pub fn dispose(&mut self) {
        self.active_resume_contexts.clear();
        self.mission_worker_sessions.clear();
    }
}

#[derive(Clone, Debug)]
pub struct WorkerActionInput {
    pub worker_slot: String,
    pub action: ResumeAction,
    pub assignment_id: Option<String>,
    pub worker_session_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ConsumeResult {
    pub already_consumed: bool,
    pub context: Option<ResumeWorkerDispatchContext>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_and_list() {
        let mut store = DispatchResumeContextStore::new(10);
        let actions = vec![
            WorkerActionInput {
                worker_slot: "worker-a".to_string(),
                action: ResumeAction::Resume,
                assignment_id: Some("a-1".to_string()),
                worker_session_id: None,
            },
            WorkerActionInput {
                worker_slot: "worker-b".to_string(),
                action: ResumeAction::Rerun,
                assignment_id: None,
                worker_session_id: None,
            },
        ];
        let result = store.activate("sess-1", "mission-1", &actions, Some("继续"));
        assert_eq!(result.unwrap(), 2);
        let listed = store.list("sess-1");
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn empty_actions_fails() {
        let mut store = DispatchResumeContextStore::new(10);
        let result = store.activate("sess-1", "mission-1", &[], None);
        assert!(result.is_err());
    }

    #[test]
    fn consume_once() {
        let mut store = DispatchResumeContextStore::new(10);
        let actions = vec![WorkerActionInput {
            worker_slot: "worker-a".to_string(),
            action: ResumeAction::Resume,
            assignment_id: None,
            worker_session_id: None,
        }];
        store.activate("sess-1", "m-1", &actions, None).unwrap();

        let first = store.consume_for_worker("sess-1", "worker-a");
        assert!(!first.already_consumed);
        assert!(first.context.is_some());

        let second = store.consume_for_worker("sess-1", "worker-a");
        assert!(second.already_consumed);
    }

    #[test]
    fn consume_unknown_worker() {
        let mut store = DispatchResumeContextStore::new(10);
        let actions = vec![WorkerActionInput {
            worker_slot: "worker-a".to_string(),
            action: ResumeAction::Resume,
            assignment_id: None,
            worker_session_id: None,
        }];
        store.activate("sess-1", "m-1", &actions, None).unwrap();
        let result = store.consume_for_worker("sess-1", "worker-x");
        assert!(!result.already_consumed);
        assert!(result.context.is_none());
    }

    #[test]
    fn clear_session() {
        let mut store = DispatchResumeContextStore::new(10);
        let actions = vec![WorkerActionInput {
            worker_slot: "worker-a".to_string(),
            action: ResumeAction::Resume,
            assignment_id: None,
            worker_session_id: None,
        }];
        store.activate("sess-1", "m-1", &actions, None).unwrap();
        store.clear(Some("sess-1"));
        assert!(store.list("sess-1").is_empty());
    }

    #[test]
    fn record_worker_session_and_reuse() {
        let mut store = DispatchResumeContextStore::new(10);
        store.record_worker_session("m-1", "worker-a", "ws-123");
        let actions = vec![WorkerActionInput {
            worker_slot: "worker-a".to_string(),
            action: ResumeAction::Resume,
            assignment_id: None,
            worker_session_id: None,
        }];
        store.activate("sess-2", "m-1", &actions, None).unwrap();
        let listed = store.list("sess-2");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].worker_session_id.as_deref(), Some("ws-123"));
    }

    #[test]
    fn max_mission_records_eviction() {
        let mut store = DispatchResumeContextStore::new(2);
        store.record_worker_session("m-1", "w", "ws-1");
        store.record_worker_session("m-2", "w", "ws-2");
        store.record_worker_session("m-3", "w", "ws-3");
        assert!(store.mission_worker_sessions.len() <= 2);
    }
}
