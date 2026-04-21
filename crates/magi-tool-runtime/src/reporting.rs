use crate::{
    ToolExecutionContext, ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionSummary, ToolInvocationRecord, ToolRegistry,
};
use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope};

impl ToolRegistry {
    pub fn invocations(&self) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .clone()
    }

    pub fn summary(&self) -> ToolExecutionSummary {
        self.summary_for_query(&ToolExecutionContextQuery::default())
    }

    pub fn query_invocations(&self, query: &ToolExecutionContextQuery) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .iter()
            .filter(|record| {
                query
                    .worker_id
                    .as_ref()
                    .is_none_or(|id| record.context.worker_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .task_id
                    .as_ref()
                    .is_none_or(|id| record.context.task_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .session_id
                    .as_ref()
                    .is_none_or(|id| record.context.session_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .workspace_id
                    .as_ref()
                    .is_none_or(|id| record.context.workspace_id.as_ref() == Some(id))
            })
            .cloned()
            .collect()
    }

    pub fn summary_for_query(&self, query: &ToolExecutionContextQuery) -> ToolExecutionSummary {
        let invocations = self.query_invocations(query);
        self.summarize_invocations(&invocations)
    }

    fn summarize_invocations(&self, invocations: &[ToolInvocationRecord]) -> ToolExecutionSummary {
        let total_invocations = invocations.len();
        let successful_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Succeeded)
            .count();
        let blocked_invocations = invocations
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    ExecutionResultStatus::NeedsApproval | ExecutionResultStatus::Rejected
                )
            })
            .count();
        let failed_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Failed)
            .count();
        ToolExecutionSummary {
            total_invocations,
            successful_invocations,
            blocked_invocations,
            failed_invocations,
        }
    }

    pub(crate) fn record_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        let record = ToolInvocationRecord {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            context: context.clone(),
            status: output.status,
            payload: output.payload.clone(),
            created_at: UtcMillis::now(),
        };
        self.invocations
            .write()
            .expect("tool invocation write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "tool.invoked",
            EventCategory::Audit,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                task_id: record.context.task_id.clone(),
                ..EventContext::default()
            },
            EventId::new(format!("tool-call-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
        self.publish_with_category(
            "tool.usage.recorded",
            EventCategory::Usage,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                mission_id: None,
                assignment_id: None,
                task_id: record.context.task_id.clone(),
            },
            EventId::new(format!("tool-usage-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "risk_level": format!("{:?}", input.risk_level),
                "approval_requirement": format!("{:?}", input.approval_requirement),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
    }

    fn publish_with_category(
        &self,
        event_type: &str,
        category: EventCategory,
        context: EventContext,
        event_id: EventId,
        payload: serde_json::Value,
    ) {
        let envelope = match category {
            EventCategory::Domain => EventEnvelope::domain(event_id, event_type, payload),
            EventCategory::Audit => EventEnvelope::audit(event_id, event_type, payload),
            EventCategory::Usage => EventEnvelope::usage(event_id, event_type, payload),
            EventCategory::Projection => EventEnvelope::projection(event_id, event_type, payload),
            EventCategory::System => EventEnvelope::system(event_id, event_type, payload),
        };
        let _ = self.event_bus.publish(envelope.with_context(context));
    }
}
