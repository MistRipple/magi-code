//! 共享宿主的会话事实面：用户会话目录的只读请求，以及目录变化通知。
//!
//! 目录始终从会话存储读取，不保存镜像。通知只说明“哪个会话变了”，宿主据此重新读取
//! 权威事实；`factsSequence` 在 daemon 进程内单调递增，读取先于目录取值，所以读到的
//! 序号之后发出的通知一定更大，宿主可据此判定通知是否已被读取覆盖。
use magi_api::plugin_session_facts::{SessionFact, session_fact};
use magi_core::SessionId;
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_extension_host::protocol::{ErrorKind, RpcError};
use magi_session_store::SessionStore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::broadcast::error::RecvError;

/// `host.session.list` 一次最多返回的会话数；超出整体失败，不静默截断。
pub(crate) const SESSION_FACTS_LIST_LIMIT: usize = 5000;
const COALESCE_WINDOW: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(crate) struct SessionFacts {
    store: Arc<SessionStore>,
    sequence: Arc<AtomicU64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListRequest {
    #[serde(default)]
    include_archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetRequest {
    session_id: SessionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    Changed,
    Added,
    Removed,
}

impl Change {
    fn name(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }
}

impl SessionFacts {
    pub(crate) fn new(store: Arc<SessionStore>, sequence: Arc<AtomicU64>) -> Self {
        Self { store, sequence }
    }

    fn fact(&self, session: &magi_session_store::SessionRecord) -> Option<SessionFact> {
        session_fact(
            session,
            self.store.runtime_sidecar(&session.session_id).as_ref(),
        )
    }

    pub(crate) fn list(&self, params: Value) -> Result<Value, RpcError> {
        let request: ListRequest = parse(params)?;
        // 序号先于目录读取：此后发出的通知序号必然更大。
        let sequence = self.sequence.load(Ordering::SeqCst);
        let mut sessions = Vec::new();
        for record in self.store.sessions() {
            if !request.include_archived && format!("{:?}", record.status) != "Active" {
                continue;
            }
            let Some(fact) = self.fact(&record) else {
                continue;
            };
            if sessions.len() >= SESSION_FACTS_LIST_LIMIT {
                return Err(RpcError::new(
                    ErrorKind::Conflict,
                    format!("会话目录超过上限 {SESSION_FACTS_LIST_LIMIT}"),
                ));
            }
            sessions.push(fact);
        }
        Ok(json!({ "factsSequence": sequence, "sessions": sessions }))
    }

    pub(crate) fn get(&self, params: Value) -> Result<Value, RpcError> {
        let request: GetRequest = parse(params)?;
        let sequence = self.sequence.load(Ordering::SeqCst);
        let session = self
            .store
            .session(&request.session_id)
            .and_then(|record| self.fact(&record));
        Ok(json!({ "factsSequence": sequence, "session": session }))
    }

    fn is_user_session(&self, session_id: &SessionId) -> bool {
        self.store
            .session(session_id)
            .is_some_and(|record| record.kind.is_user())
    }

    /// 把总线上的会话事件合并为 `host.session.changed` 通知（同一会话在窗口内只发一次）。
    /// 总线落后（广播丢事件）时发 `resync`，宿主重读整个目录。
    pub(crate) fn spawn_forwarder(
        self,
        events: Arc<InMemoryEventBus>,
        notify: impl Fn(Value) + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let mut receiver = events.subscribe();
        tokio::spawn(async move {
            let mut pending: HashMap<SessionId, Change> = HashMap::new();
            let mut resync = false;
            loop {
                let wait = if pending.is_empty() && !resync {
                    None
                } else {
                    Some(COALESCE_WINDOW)
                };
                let received = match wait {
                    None => Some(receiver.recv().await),
                    Some(window) => tokio::time::timeout(window, receiver.recv()).await.ok(),
                };
                match received {
                    Some(Ok(event)) => {
                        if let Some((session_id, change)) = classify(&event)
                            && (change == Change::Removed || self.is_user_session(&session_id))
                        {
                            let entry = pending.entry(session_id).or_insert(change);
                            *entry = (*entry).max(change);
                        }
                    }
                    Some(Err(RecvError::Lagged(_))) => resync = true,
                    Some(Err(RecvError::Closed)) => return,
                    None => {
                        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
                        let changes: Vec<Value> = pending
                            .drain()
                            .map(|(id, change)| json!({"kind": change.name(), "sessionId": id}))
                            .collect();
                        notify(json!({
                            "factsSequence": sequence,
                            "resync": std::mem::take(&mut resync),
                            "changes": changes,
                        }));
                    }
                }
            }
        })
    }
}

fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))
}

fn classify(event: &EventEnvelope) -> Option<(SessionId, Change)> {
    let session_id = event.session_id.clone()?;
    let change = match event.event_type.as_str() {
        "session.created" => Change::Added,
        "session.deleted" => Change::Removed,
        "session.title.updated" | "session.viewed" | "session.closed" => Change::Changed,
        kind if kind.starts_with("session.turn.") => Change::Changed,
        _ => return None,
    };
    Some((session_id, change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::EventId;

    fn event(kind: &str, session: &str) -> EventEnvelope {
        EventEnvelope::domain(EventId::unique("e"), kind, json!({})).with_context(
            magi_event_bus::EventContext {
                session_id: Some(SessionId::new(session)),
                ..Default::default()
            },
        )
    }

    #[test]
    fn session_events_map_to_directory_changes_and_others_are_ignored() {
        let kind = |kind| classify(&event(kind, "s1")).map(|(_, change)| change);
        assert_eq!(kind("session.created"), Some(Change::Added));
        assert_eq!(kind("session.deleted"), Some(Change::Removed));
        assert_eq!(kind("session.title.updated"), Some(Change::Changed));
        assert_eq!(kind("session.turn.completed"), Some(Change::Changed));
        assert_eq!(kind("session.turn.item"), Some(Change::Changed));
        assert_eq!(kind("model.usage.recorded"), None);
        assert_eq!(kind("session.context.pressure.updated"), None);
        // 没有会话身份的事件不属于任何会话。
        assert!(
            classify(&EventEnvelope::domain(
                EventId::unique("e"),
                "session.created",
                json!({})
            ))
            .is_none()
        );
    }

    fn facts() -> (SessionFacts, Arc<SessionStore>) {
        let store = Arc::new(SessionStore::new());
        (SessionFacts::new(store.clone(), Arc::default()), store)
    }

    #[test]
    fn the_directory_lists_active_user_sessions_with_the_facts_of_the_store() {
        let (facts, store) = facts();
        store
            .create_session_with_working_directory_at(
                SessionId::new("a"),
                "甲",
                Some("w1".into()),
                magi_core::UtcMillis(100),
                Some(std::env::temp_dir()),
            )
            .unwrap();
        store.create_session(SessionId::new("b"), "乙").unwrap();
        store.archive_session(&SessionId::new("b")).unwrap();

        let listed = facts.list(json!({})).unwrap();
        assert_eq!(listed["factsSequence"], 0);
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "已归档会话默认不在目录里");
        assert_eq!(sessions[0]["sessionId"], "a");
        assert_eq!(sessions[0]["title"], "甲");
        assert_eq!(sessions[0]["isRunning"], false);
        assert_eq!(sessions[0]["workspaceId"], "w1");
        assert!(sessions[0]["cwd"].is_string());

        let with_archived = facts.list(json!({"includeArchived": true})).unwrap();
        assert_eq!(with_archived["sessions"].as_array().unwrap().len(), 2);

        let one = facts.get(json!({"sessionId": "b"})).unwrap();
        assert_eq!(one["session"]["status"], "Archived");
        assert!(facts.get(json!({"sessionId": "missing"})).unwrap()["session"].is_null());
        // 未知字段整体拒绝，不静默忽略。
        assert!(facts.list(json!({"limit": 1})).is_err());
    }

    #[test]
    fn a_directory_over_the_limit_fails_as_a_whole_instead_of_truncating() {
        let (facts, store) = facts();
        for index in 0..=SESSION_FACTS_LIST_LIMIT {
            store
                .create_session(SessionId::new(format!("s{index}")), "x")
                .unwrap();
        }
        let error = facts.list(json!({})).expect_err("超过上限必须失败");
        assert!(error.message.contains("5000"), "{}", error.message);
        // 单个读取不受目录上限影响。
        assert!(facts.get(json!({"sessionId": "s0"})).unwrap()["session"].is_object());
    }

    #[tokio::test]
    async fn bus_events_coalesce_into_one_notification_per_session_and_removal_wins() {
        let (facts, store) = facts();
        store.create_session(SessionId::new("a"), "甲").unwrap();
        let bus = Arc::new(InMemoryEventBus::new(64));
        let (sender, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let task = facts
            .clone()
            .spawn_forwarder(bus.clone(), move |params| sender.send(params).unwrap());
        tokio::task::yield_now().await;

        bus.publish(event("session.created", "a"));
        bus.publish(event("session.turn.item", "a"));
        bus.publish(event("session.turn.completed", "a"));
        bus.publish(event("session.deleted", "gone"));
        bus.publish(event("model.usage.recorded", "a"));
        let first = sent.recv().await.unwrap();
        assert_eq!(first["factsSequence"], 1);
        assert_eq!(first["resync"], false);
        let mut changes: Vec<(String, String)> = first["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["sessionId"].as_str().unwrap().into(),
                    c["kind"].as_str().unwrap().into(),
                )
            })
            .collect();
        changes.sort();
        assert_eq!(
            changes,
            vec![
                ("a".into(), "added".into()),
                ("gone".into(), "removed".into())
            ],
            "同一会话在窗口内只通知一次，添加覆盖变化；已不存在的会话照常报告删除"
        );

        // 读取先于目录：之后发出的通知序号更大。
        assert_eq!(facts.list(json!({})).unwrap()["factsSequence"], 1);
        bus.publish(event("session.title.updated", "a"));
        let second = sent.recv().await.unwrap();
        assert_eq!(second["factsSequence"], 2);
        task.abort();
    }

    #[tokio::test]
    async fn sessions_unknown_to_the_store_produce_no_notification() {
        let (facts, store) = facts();
        let bus = Arc::new(InMemoryEventBus::new(64));
        let (sender, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let task = facts
            .clone()
            .spawn_forwarder(bus.clone(), move |params| sender.send(params).unwrap());
        tokio::task::yield_now().await;
        bus.publish(event("session.turn.item", "never-created"));
        store.create_session(SessionId::new("u"), "用户").unwrap();
        bus.publish(event("session.turn.item", "u"));
        let notice = sent.recv().await.unwrap();
        assert_eq!(notice["changes"].as_array().unwrap().len(), 1);
        assert_eq!(notice["changes"][0]["sessionId"], "u");
        task.abort();
    }

    #[test]
    fn removal_outranks_addition_and_change_when_changes_coalesce() {
        assert!(Change::Removed > Change::Added && Change::Added > Change::Changed);
    }
}
