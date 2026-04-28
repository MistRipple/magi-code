use std::collections::HashMap;

pub struct PendingRecoveryContext {
    pub task_id: String,
    pub prompt: String,
    pub session_id: String,
    pub chain_id: Option<String>,
    pub runtime_reason: Option<String>,
    pub errors: Vec<String>,
    pub can_retry: bool,
    pub can_rollback: bool,
}

pub struct PendingPlanApprovalContext {
    pub plan_id: String,
    pub session_id: String,
    pub prompt: String,
    pub chain_id: Option<String>,
}

pub struct SessionExecutionContextStats {
    pub session_id: String,
    pub has_pending_recovery: bool,
    pub pending_approval_count: usize,
    pub last_touched_at: u64,
}

pub struct SessionExecutionContext {
    session_id: String,
    pending_recovery: Option<PendingRecoveryContext>,
    pending_plan_approvals: HashMap<String, PendingPlanApprovalContext>,
    created_at: u64,
    last_touched_at: u64,
}

impl SessionExecutionContext {
    pub fn new(session_id: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            session_id: session_id.into(),
            pending_recovery: None,
            pending_plan_approvals: HashMap::new(),
            created_at: now,
            last_touched_at: now,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn touch(&mut self) {
        self.last_touched_at = now_millis();
    }

    pub fn set_pending_recovery(&mut self, context: Option<PendingRecoveryContext>) {
        self.pending_recovery = context;
        self.touch();
    }

    pub fn pending_recovery(&self) -> Option<&PendingRecoveryContext> {
        self.pending_recovery.as_ref()
    }

    pub fn clear_pending_recovery(&mut self) {
        self.pending_recovery = None;
        self.touch();
    }

    pub fn set_pending_plan_approval(
        &mut self,
        request_id: String,
        context: PendingPlanApprovalContext,
    ) {
        self.pending_plan_approvals.insert(request_id, context);
        self.touch();
    }

    pub fn get_pending_plan_approval(
        &self,
        request_id: &str,
    ) -> Option<&PendingPlanApprovalContext> {
        self.pending_plan_approvals.get(request_id)
    }

    pub fn delete_pending_plan_approval(&mut self, request_id: &str) -> bool {
        let removed = self.pending_plan_approvals.remove(request_id).is_some();
        if removed {
            self.touch();
        }
        removed
    }

    pub fn clear_transient_state(&mut self) {
        self.pending_recovery = None;
        self.pending_plan_approvals.clear();
        self.touch();
    }

    pub fn is_idle(&self, ttl_ms: u64) -> bool {
        if ttl_ms == 0 {
            return false;
        }
        let now = now_millis();
        now.saturating_sub(self.last_touched_at) >= ttl_ms
            && self.pending_recovery.is_none()
            && self.pending_plan_approvals.is_empty()
    }

    pub fn stats(&self) -> SessionExecutionContextStats {
        SessionExecutionContextStats {
            session_id: self.session_id.clone(),
            has_pending_recovery: self.pending_recovery.is_some(),
            pending_approval_count: self.pending_plan_approvals.len(),
            last_touched_at: self.last_touched_at,
        }
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn last_touched_at(&self) -> u64 {
        self.last_touched_at
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
