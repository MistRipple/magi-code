use super::{DaemonError, EventHistoryRead, SessionConversationProjection};
use crate::native_plugin_platform::session_observation::{
    ObservationScope, PendingAttemptStreams, committed_observations_for_scope,
};
use magi_core::{SessionId, UtcMillis};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommittedObservation {
    seq: u64,
    source_sequence: u64,
    occurred_at: UtcMillis,
    observation: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::daemon) struct CommittedObservationPage {
    through: u64,
    next_after: Option<u64>,
    events: Vec<CommittedObservation>,
}

impl CommittedObservationPage {
    pub(in crate::daemon) fn read_for_scope(
        session: &SessionId,
        after: u64,
        through: Option<u64>,
        limit: usize,
        byte_limit: usize,
        scope: ObservationScope<'_>,
        mut open_source: impl FnMut(u64, Option<u64>) -> Result<EventHistoryRead, DaemonError>,
    ) -> Result<Self, DaemonError> {
        let mut source = open_source(0, through)?;
        let through = source.through;
        if after > through {
            return Err(DaemonError::internal("会话观察游标超出已提交水位"));
        }
        if source.first_retained != 1 {
            return Err(DaemonError::internal(format!(
                "会话的原始历史前缀已被截断，最早保留序号为 {}",
                source.first_retained,
            )));
        }
        let mut page = Self {
            through,
            next_after: None,
            events: Vec::new(),
        };
        if after == through {
            return Ok(page);
        }
        let mut projection = SessionConversationProjection {
            session_id: Some(session.clone()),
            ..Default::default()
        };
        let mut seq = 0;
        let mut bytes = 0;
        let mut pending = PendingAttemptStreams::default();
        loop {
            let (range, transactions) = source.transactions()?;
            for transaction in transactions {
                let transaction = transaction?;
                super::history::validate_history_transaction(
                    &transaction,
                    session,
                    projection.last_event_seq + 1,
                    range.last_event_seq,
                )?;

                // Keep only the old facts of records touched by this transaction.
                // Replay uses the same projection and validation as daemon startup.
                let before_turns = projection
                    .canonical_turns
                    .iter()
                    .filter(|turn| {
                        transaction
                            .events
                            .iter()
                            .any(|event| event.turn_id == turn.turn_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let before_inputs = projection
                    .inputs
                    .iter()
                    .filter(|input| {
                        transaction.events.iter().any(|event| {
                            event
                                .input
                                .as_ref()
                                .is_some_and(|next| next.input_id == input.input_id)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for event in &transaction.events {
                    projection.apply_event(event.clone())?;
                }
                for (event, observation) in committed_observations_for_scope(
                    &transaction.events,
                    &before_turns,
                    &before_inputs,
                    &projection.canonical_turns,
                    &mut pending,
                    scope,
                ) {
                    let ordinal = seq;
                    seq += 1;
                    if event.event_seq <= after || event.event_seq > through {
                        continue;
                    }
                    let record = CommittedObservation {
                        seq: ordinal,
                        source_sequence: event.event_seq,
                        occurred_at: event.occurred_at,
                        observation,
                    };
                    let size = serde_json::to_vec(&record)?.len() + 1;
                    // 同一个 canonical 事件的观察不拆页：游标是已完整返回的最后一个事件序号。
                    let starts_new_event = page
                        .events
                        .last()
                        .is_none_or(|previous| previous.source_sequence != record.source_sequence);
                    if starts_new_event && (page.events.len() >= limit || bytes + size > byte_limit)
                    {
                        let Some(previous) = page.events.last() else {
                            return Err(DaemonError::internal("会话观察单条记录超过读取上限"));
                        };
                        page.next_after = Some(previous.source_sequence);
                        return Ok(page);
                    }
                    bytes += size;
                    page.events.push(record);
                }
            }
            if projection.last_event_seq != range.last_event_seq {
                return Err(DaemonError::internal("会话历史事务未覆盖已提交范围"));
            }
            if projection.last_event_seq >= through {
                return Ok(page);
            }
            source = open_source(projection.last_event_seq, Some(through))?;
        }
    }
}
