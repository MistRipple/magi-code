//! Mission 维度的生命周期通知队列。
//!
//! 设计要点：
//! - 每个 mission 一个 `MissionNoticeState`，三个 slot 互相独立但拼接顺序固定。
//! - `pending_notice` 把当前 slots 拼成一段文本，按需 drain `MissionResumed`（一次性）。
//! - 其他事件类型走"后来覆盖前者"语义，避免堆积。
//!
//! 线程安全：内部用 `Mutex<HashMap<...>>`。`pending_notice` 在 dispatcher 派发
//! 前同步调用，频率与 mission 数量同阶——锁竞争可忽略。

use std::collections::HashMap;
use std::sync::Mutex;

use magi_core::MissionId;
use magi_event_bus::EventEnvelope;
use magi_event_bus::task_events::{
    MISSION_HUMAN_CHECKPOINT_APPROVED, MISSION_HUMAN_CHECKPOINT_REJECTED,
    MISSION_PLAN_STEP_COMPLETED, MISSION_RESUMED_FROM_RECOVERY,
};

use crate::templates;

/// 单个 mission 的三类通知 slot。空 slot 不会出现在输出里。
#[derive(Debug, Default, Clone)]
pub struct MissionNoticeState {
    pub mission_resumed: Option<String>,
    pub human_checkpoint: Option<String>,
    pub plan_step_completed: Option<String>,
}

impl MissionNoticeState {
    fn is_empty(&self) -> bool {
        self.mission_resumed.is_none()
            && self.human_checkpoint.is_none()
            && self.plan_step_completed.is_none()
    }

    /// 拼接顺序：resume → human_checkpoint → plan_step。每段之间空一行。
    fn render(&self) -> String {
        let mut sections: Vec<&str> = Vec::new();
        if let Some(s) = &self.mission_resumed {
            sections.push(s.as_str());
        }
        if let Some(s) = &self.human_checkpoint {
            sections.push(s.as_str());
        }
        if let Some(s) = &self.plan_step_completed {
            sections.push(s.as_str());
        }
        sections.join("\n\n")
    }
}

/// 全局注册表：按 mission_id 维护通知队列。
///
/// 在 daemon bootstrap 处构造一份 `Arc<LifecycleNoticeRegistry>`：
/// - 一份 clone 交给 subscriber task（写入侧）；
/// - 一份 clone 注入 `TaskExecutionDispatcher`（读取侧）。
pub struct LifecycleNoticeRegistry {
    inner: Mutex<HashMap<MissionId, MissionNoticeState>>,
}

impl LifecycleNoticeRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// dispatcher 在派发每轮 prompt 前调用。返回非空字符串时，
    /// 调用方应把它作为 `lifecycle_notice` 参数传给 `prepend_session_instructions`。
    ///
    /// 调用语义：
    /// - `MissionResumed` slot 一次性消费（读后清空）；
    /// - 其余 slot 保留——模型每轮都被提醒人审/进度的最新状态，
    ///   直到下一个同类事件覆盖或显式 `clear_for_mission`。
    pub fn pending_notice(&self, mission_id: &MissionId) -> Option<String> {
        let mut map = self.inner.lock().expect("lifecycle-notice lock poisoned");
        let state = map.get_mut(mission_id)?;
        if state.is_empty() {
            return None;
        }
        let rendered = state.render();
        // 一次性 slot drain。
        state.mission_resumed = None;
        // 如果 drain 后整个 state 已空，移除条目避免 map 无限膨胀。
        if state.is_empty() {
            map.remove(mission_id);
        }
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    /// subscriber 收到事件后调用；未知 event_type 静默忽略。
    pub fn ingest(&self, env: &EventEnvelope) {
        let Some(mission_id) = env.mission_id.clone() else {
            return;
        };
        let rendered = match env.event_type.as_str() {
            MISSION_RESUMED_FROM_RECOVERY => render_resumed(env),
            MISSION_HUMAN_CHECKPOINT_APPROVED => render_hc(env, true),
            MISSION_HUMAN_CHECKPOINT_REJECTED => render_hc(env, false),
            MISSION_PLAN_STEP_COMPLETED => render_plan_step(env),
            _ => return,
        };
        let Some(rendered) = rendered else { return };

        let mut map = self.inner.lock().expect("lifecycle-notice lock poisoned");
        let entry = map.entry(mission_id).or_default();
        match env.event_type.as_str() {
            MISSION_RESUMED_FROM_RECOVERY => entry.mission_resumed = Some(rendered),
            MISSION_HUMAN_CHECKPOINT_APPROVED | MISSION_HUMAN_CHECKPOINT_REJECTED => {
                entry.human_checkpoint = Some(rendered);
            }
            MISSION_PLAN_STEP_COMPLETED => entry.plan_step_completed = Some(rendered),
            _ => unreachable!("已经在 match 上层过滤"),
        }
    }

    /// 显式清空某个 mission 的所有 slot——用于 mission 终结/归档场景。
    /// 当前未在生产路径调用，留作 read-model / UI 后续接入。
    pub fn clear_for_mission(&self, mission_id: &MissionId) {
        let mut map = self.inner.lock().expect("lifecycle-notice lock poisoned");
        map.remove(mission_id);
    }
}

impl Default for LifecycleNoticeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn render_resumed(env: &EventEnvelope) -> Option<String> {
    let mission_id = env.mission_id.as_ref()?.as_str().to_string();
    let payload = &env.payload;
    let recovery_id = payload
        .get("recovery_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cp_seq = payload
        .get("checkpoint_sequence")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    Some(templates::render_mission_resumed(&[
        ("mission_id", mission_id.as_str()),
        ("recovery_id", recovery_id),
        ("checkpoint_sequence", &cp_seq),
    ]))
}

fn render_hc(env: &EventEnvelope, approved: bool) -> Option<String> {
    let mission_id = env.mission_id.as_ref()?.as_str().to_string();
    let payload = &env.payload;
    let sequence = payload
        .get("sequence")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let plan_step_id = payload
        .get("plan_step_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let decided_by = payload
        .get("decided_by")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let vars = [
        ("mission_id", mission_id.as_str()),
        ("sequence", sequence.as_str()),
        ("plan_step_id", plan_step_id),
        ("decided_by", decided_by),
    ];
    Some(if approved {
        templates::render_human_checkpoint_approved(&vars)
    } else {
        templates::render_human_checkpoint_rejected(&vars)
    })
}

fn render_plan_step(env: &EventEnvelope) -> Option<String> {
    let mission_id = env.mission_id.as_ref()?.as_str().to_string();
    let payload = &env.payload;
    let step_id = payload
        .get("step_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let step_content = payload
        .get("step_content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let total = payload
        .get("total_steps")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let completed = payload
        .get("completed_steps")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    Some(templates::render_plan_step_completed(&[
        ("mission_id", mission_id.as_str()),
        ("step_id", step_id),
        ("step_content", step_content),
        ("total_steps", total.as_str()),
        ("completed_steps", completed.as_str()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::EventContext;
    use magi_event_bus::task_events::{
        mission_human_checkpoint_resolved_event, mission_plan_step_completed_event,
        mission_resumed_from_recovery_event,
    };

    fn with_mission(
        env: magi_event_bus::EventEnvelope,
        mid: &str,
    ) -> magi_event_bus::EventEnvelope {
        env.with_context(EventContext {
            mission_id: Some(MissionId::new(mid)),
            ..EventContext::default()
        })
    }

    #[test]
    fn ingests_mission_resumed_and_drains_after_read() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-1");
        let env = with_mission(
            mission_resumed_from_recovery_event("M-1", "rec-1", 5, Some("chain-1"), Some("abc")),
            "M-1",
        );
        registry.ingest(&env);

        let first = registry.pending_notice(&mid).expect("first read populated");
        assert!(first.contains("rec-1"));
        assert!(first.contains('5'));

        // 一次性 slot：再读应空（除非有其他 slot 被填）。
        assert!(registry.pending_notice(&mid).is_none());
    }

    #[test]
    fn human_checkpoint_rejected_overwrites_approved() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-2");
        registry.ingest(&with_mission(
            mission_human_checkpoint_resolved_event(
                "M-2",
                1,
                "approved",
                "step-a",
                "ops",
                Some("L1"),
            ),
            "M-2",
        ));
        registry.ingest(&with_mission(
            mission_human_checkpoint_resolved_event("M-2", 2, "rejected", "step-b", "qa", None),
            "M-2",
        ));

        let body = registry.pending_notice(&mid).expect("hc populated");
        assert!(body.contains("被驳回"));
        assert!(body.contains("step-b"));
        assert!(!body.contains("step-a"));
    }

    #[test]
    fn human_checkpoint_persists_across_reads_until_overwritten() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-2b");
        registry.ingest(&with_mission(
            mission_human_checkpoint_resolved_event("M-2b", 1, "approved", "step-a", "ops", None),
            "M-2b",
        ));
        let first = registry.pending_notice(&mid).expect("first read");
        assert!(first.contains("step-a"));
        // 与 mission_resumed 不同：人审 slot 不应一次性 drain。
        let second = registry.pending_notice(&mid).expect("second read");
        assert!(second.contains("step-a"));
    }

    #[test]
    fn plan_step_completed_overwrites_previous() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-3");
        registry.ingest(&with_mission(
            mission_plan_step_completed_event("M-3", "s1", "first", 3, 1),
            "M-3",
        ));
        registry.ingest(&with_mission(
            mission_plan_step_completed_event("M-3", "s2", "second", 3, 2),
            "M-3",
        ));
        let body = registry.pending_notice(&mid).expect("plan-step populated");
        assert!(body.contains("s2"));
        assert!(body.contains("second"));
        assert!(!body.contains("s1"));
    }

    #[test]
    fn unknown_event_type_is_silently_ignored() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-4");
        let env = with_mission(
            magi_event_bus::EventEnvelope::domain(
                magi_core::EventId::new("ignored-1"),
                "task.status.changed",
                serde_json::json!({}),
            ),
            "M-4",
        );
        registry.ingest(&env);
        assert!(registry.pending_notice(&mid).is_none());
    }

    #[test]
    fn event_without_mission_id_is_dropped() {
        let registry = LifecycleNoticeRegistry::new();
        let env = mission_resumed_from_recovery_event("M-5", "rec-x", 1, None, None);
        // 注意：未附加 EventContext.mission_id
        registry.ingest(&env);
        // 任何 mission_id 查询都应为 None
        assert!(registry.pending_notice(&MissionId::new("M-5")).is_none());
    }

    #[test]
    fn multiple_slots_concat_in_fixed_order() {
        let registry = LifecycleNoticeRegistry::new();
        let mid = MissionId::new("M-6");
        registry.ingest(&with_mission(
            mission_resumed_from_recovery_event("M-6", "rec-9", 2, None, None),
            "M-6",
        ));
        registry.ingest(&with_mission(
            mission_human_checkpoint_resolved_event("M-6", 3, "approved", "step-z", "ops", None),
            "M-6",
        ));
        registry.ingest(&with_mission(
            mission_plan_step_completed_event("M-6", "sx", "do x", 5, 4),
            "M-6",
        ));
        let body = registry.pending_notice(&mid).expect("populated");
        let pos_resume = body.find("rec-9").expect("resume present");
        let pos_hc = body.find("step-z").expect("hc present");
        let pos_step = body.find("sx").expect("step present");
        assert!(pos_resume < pos_hc, "resume 段应在 hc 段之前");
        assert!(pos_hc < pos_step, "hc 段应在 plan_step 段之前");
    }
}
