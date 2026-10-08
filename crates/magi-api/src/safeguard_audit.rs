//! 安全防护审计记录的可见集合与删除。
//!
//! 账本是只追加的：删除不改写已落盘的段，而是追加一条 `security.safety.audit.removed`
//! 墓碑事件，读取时把被墓碑覆盖的记录排除（与用量统计重置同一做法）。设置页的计数、
//! 列表和删除都经过这里，保证它们对"哪些记录还在"的判断一致。被删除记录的原始内容
//! 随账本的保留期过期后从磁盘清除。

use magi_core::EventId;
use magi_event_bus::{
    AuditUsageLedgerEntry, AuditUsageLedgerSnapshot, EventEnvelope, InMemoryEventBus,
};
use std::collections::HashSet;

pub(crate) const EVALUATED_EVENT_TYPE: &str = "security.safety.evaluated";
pub(crate) const REMOVED_EVENT_TYPE: &str = "security.safety.audit.removed";

pub(crate) enum AuditSelection {
    /// 当前可见的全部记录；之后新产生的记录不受影响。
    All,
    Ids(Vec<String>),
}

/// 仍可见的审计记录，按 sequence 升序。
pub(crate) fn visible_entries(
    ledger: &AuditUsageLedgerSnapshot,
) -> impl DoubleEndedIterator<Item = &AuditUsageLedgerEntry> {
    let mut removed_through = 0u64;
    let mut removed_ids: HashSet<&str> = HashSet::new();
    for entry in ledger
        .audit_entries
        .iter()
        .filter(|entry| entry.event_type == REMOVED_EVENT_TYPE)
    {
        if let Some(through) = entry
            .payload
            .get("throughSequence")
            .and_then(serde_json::Value::as_u64)
        {
            removed_through = removed_through.max(through);
        }
        if let Some(ids) = entry
            .payload
            .get("eventIds")
            .and_then(|value| value.as_array())
        {
            removed_ids.extend(ids.iter().filter_map(serde_json::Value::as_str));
        }
    }
    ledger.audit_entries.iter().filter(move |entry| {
        entry.event_type == EVALUATED_EVENT_TYPE
            && entry.sequence > removed_through
            && !removed_ids.contains(entry.event_id.as_str())
    })
}

pub(crate) fn visible_count(event_bus: &InMemoryEventBus) -> usize {
    event_bus.with_audit_usage_ledger(|ledger| visible_entries(ledger).count())
}

/// 删除所选记录，返回实际删除的条数（已不可见的 id 不计）。
pub(crate) fn remove(event_bus: &InMemoryEventBus, selection: AuditSelection) -> usize {
    let (removed, payload) = event_bus.with_audit_usage_ledger(|ledger| match &selection {
        AuditSelection::All => {
            let mut count = 0;
            let mut last = 0;
            for entry in visible_entries(ledger) {
                count += 1;
                last = entry.sequence;
            }
            (count, serde_json::json!({ "throughSequence": last }))
        }
        AuditSelection::Ids(ids) => {
            let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let found = visible_entries(ledger)
                .filter(|entry| wanted.contains(entry.event_id.as_str()))
                .map(|entry| entry.event_id.as_str().to_string())
                .collect::<Vec<_>>();
            (found.len(), serde_json::json!({ "eventIds": found }))
        }
    });
    if removed > 0 {
        event_bus.publish(EventEnvelope::audit(
            EventId::unique("event-safeguard-audit-removed"),
            REMOVED_EVENT_TYPE,
            payload,
        ));
    }
    removed
}
