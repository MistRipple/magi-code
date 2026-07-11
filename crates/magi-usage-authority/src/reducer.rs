use std::collections::{HashMap, HashSet};

use crate::costing::normalize_usage_delta;
use crate::types::{
    SessionSummary, SessionUsageSnapshot, UsageBindingSnapshot, UsageCallStatus, UsageEvent,
    UsageEventType, UsageModelSnapshot, UsageTotals, WorkspaceUsageSnapshot,
};

fn apply_delta(
    totals: &UsageTotals,
    delta: &crate::costing::NormalizedUsageTotals,
    status: Option<UsageCallStatus>,
) -> UsageTotals {
    let mut next = totals.clone();
    next.llm_call_count += 1;
    next.raw_input_tokens += delta.raw_input_tokens;
    next.raw_output_tokens += delta.raw_output_tokens;
    next.cache_read_tokens += delta.cache_read_tokens;
    next.cache_write_tokens += delta.cache_write_tokens;
    next.net_input_tokens += delta.net_input_tokens;
    next.net_output_tokens += delta.net_output_tokens;
    next.total_tokens += delta.total_tokens;
    if status == Some(UsageCallStatus::Success) {
        next.success_count += 1;
    } else {
        next.failure_count += 1;
    }
    next
}

fn upsert_binding_snapshot(
    current: &[UsageBindingSnapshot],
    input: UsageBindingSnapshot,
) -> Vec<UsageBindingSnapshot> {
    let mut next: Vec<UsageBindingSnapshot> = current.to_vec();
    if let Some(pos) = next.iter().position(|item| {
        item.template_id == input.template_id
            && item.engine_id == input.engine_id
            && item.binding_revision == input.binding_revision
            && item.role == input.role
    }) {
        next[pos] = input;
    } else {
        next.push(input);
    }
    next
}

fn upsert_model_snapshot(
    current: &[UsageModelSnapshot],
    input: UsageModelSnapshot,
) -> Vec<UsageModelSnapshot> {
    let mut next: Vec<UsageModelSnapshot> = current.to_vec();
    if let Some(pos) = next
        .iter()
        .position(|item| item.model_identity_key == input.model_identity_key)
    {
        next[pos] = input;
    } else {
        next.push(input);
    }
    next
}

pub fn rebuild_session_snapshot_from_events(
    workspace_id: &str,
    session_id: &str,
    events: &[UsageEvent],
) -> SessionUsageSnapshot {
    let mut sorted: Vec<&UsageEvent> = events.iter().collect();
    sorted.sort_by_key(|e| e.ledger_seq);

    let mut snapshot = SessionUsageSnapshot::empty(workspace_id, session_id);
    let mut seen_event_ids: HashSet<String> = HashSet::new();
    let mut seen_assignments: HashSet<String> = HashSet::new();
    let mut seen_turns: HashSet<String> = HashSet::new();
    let mut seen_binding_assignments: HashSet<String> = HashSet::new();

    for event in sorted {
        if !event.event_id.is_empty() && seen_event_ids.contains(&event.event_id) {
            continue;
        }
        seen_event_ids.insert(event.event_id.clone());
        snapshot.version += 1;
        snapshot.last_applied_ledger_seq = event.ledger_seq;
        snapshot.updated_at = snapshot.updated_at.max(event.timestamp);

        if event.event_type == UsageEventType::SessionReset {
            let version = snapshot.version;
            snapshot = SessionUsageSnapshot::empty(workspace_id, session_id);
            snapshot.version = version;
            snapshot.last_applied_ledger_seq = event.ledger_seq;
            snapshot.updated_at = event.timestamp;
            seen_assignments.clear();
            seen_turns.clear();
            seen_binding_assignments.clear();
            continue;
        }

        let (usage_delta, exec_binding, model_id) = match (
            &event.usage_delta,
            &event.execution_binding,
            &event.model_identity,
        ) {
            (Some(ud), Some(eb), Some(mi)) => (ud, eb, mi),
            _ => continue,
        };

        let normalized = normalize_usage_delta(usage_delta);
        snapshot.totals = apply_delta(&snapshot.totals, &normalized, event.status);

        if let Some(aid) = &event.assignment_id
            && seen_assignments.insert(aid.clone())
        {
            snapshot.totals.assignment_count += 1;
        }
        if let Some(tid) = &event.turn_id
            && seen_turns.insert(tid.clone())
        {
            snapshot.totals.turn_count += 1;
        }

        let binding_existing = snapshot.by_execution_binding.iter().find(|item| {
            item.template_id == exec_binding.template_id
                && item.engine_id == exec_binding.engine_id
                && item.binding_revision == exec_binding.binding_revision
                && item.role == exec_binding.role
        });
        let mut binding_totals = apply_delta(
            binding_existing
                .map(|b| &b.totals)
                .unwrap_or(&UsageTotals::default()),
            &normalized,
            event.status,
        );
        if let Some(aid) = &event.assignment_id {
            let key = format!("{}:{}", exec_binding.template_id, aid);
            if seen_binding_assignments.insert(key) {
                binding_totals.assignment_count += 1;
            }
        }
        snapshot.by_execution_binding = upsert_binding_snapshot(
            &snapshot.by_execution_binding,
            UsageBindingSnapshot {
                template_id: exec_binding.template_id.clone(),
                engine_id: exec_binding.engine_id.clone(),
                binding_revision: exec_binding.binding_revision,
                role: exec_binding.role,
                provider: Some(model_id.provider.clone()),
                declared_model_spec: Some(model_id.declared_model_spec.clone()),
                resolved_model: Some(model_id.resolved_model.clone()),
                model_identity_key: Some(model_id.model_identity_key.clone()),
                totals: binding_totals,
            },
        );

        let model_existing = snapshot
            .by_model_identity
            .iter()
            .find(|item| item.model_identity_key == model_id.model_identity_key);
        let model_totals = apply_delta(
            model_existing
                .map(|m| &m.totals)
                .unwrap_or(&UsageTotals::default()),
            &normalized,
            event.status,
        );
        snapshot.by_model_identity = upsert_model_snapshot(
            &snapshot.by_model_identity,
            UsageModelSnapshot {
                model_identity_key: model_id.model_identity_key.clone(),
                provider: model_id.provider.clone(),
                declared_model_spec: model_id.declared_model_spec.clone(),
                resolved_model: model_id.resolved_model.clone(),
                base_url_fingerprint: model_id.base_url_fingerprint.clone(),
                reasoning_effort: model_id.reasoning_effort,
                totals: model_totals,
            },
        );
    }

    snapshot
}

pub fn rebuild_workspace_snapshot_from_sessions(
    workspace_id: &str,
    session_snapshots: &[SessionUsageSnapshot],
) -> WorkspaceUsageSnapshot {
    let mut totals = UsageTotals::default();
    let mut binding_map: HashMap<String, UsageBindingSnapshot> = HashMap::new();
    let mut model_map: HashMap<String, UsageModelSnapshot> = HashMap::new();

    let by_session: Vec<SessionSummary> = session_snapshots
        .iter()
        .map(|s| SessionSummary {
            session_id: s.session_id.clone(),
            version: s.version,
            updated_at: s.updated_at,
            totals: s.totals.clone(),
        })
        .collect();

    for session in session_snapshots {
        totals = totals.add(&session.totals);

        for binding in &session.by_execution_binding {
            let key = format!(
                "{}:{}:{}:{:?}",
                binding.template_id, binding.engine_id, binding.binding_revision, binding.role
            );
            binding_map
                .entry(key)
                .and_modify(|existing| {
                    existing.totals = existing.totals.add(&binding.totals);
                })
                .or_insert_with(|| binding.clone());
        }

        for model in &session.by_model_identity {
            model_map
                .entry(model.model_identity_key.clone())
                .and_modify(|existing| {
                    existing.totals = existing.totals.add(&model.totals);
                })
                .or_insert_with(|| model.clone());
        }
    }

    let version = session_snapshots
        .iter()
        .map(|s| s.version)
        .max()
        .unwrap_or(0);
    let updated_at = session_snapshots
        .iter()
        .map(|s| s.updated_at)
        .max()
        .unwrap_or(0);
    let last_applied = session_snapshots
        .iter()
        .map(|s| (s.session_id.clone(), s.version))
        .collect();

    WorkspaceUsageSnapshot {
        workspace_id: workspace_id.to_string(),
        version,
        last_applied_session_snapshot_versions: last_applied,
        updated_at,
        totals,
        by_session,
        by_execution_binding: binding_map.into_values().collect(),
        by_model_identity: model_map.into_values().collect(),
    }
}
