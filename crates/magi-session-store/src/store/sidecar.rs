use super::SessionStore;
use crate::models::{
    ActiveExecutionChain, ActiveExecutionTurn, ActiveExecutionTurnItem,
    SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState, SessionRuntimeSidecar,
    SessionSidecarFlushReason,
};
use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, LeaseId, RecoveryResumeInput, SessionId,
    TaskExecutionTarget, TaskId, UtcMillis, WorkerId,
};

fn inherit_current_turn_aliases(turn: &ActiveExecutionTurn, item: &mut ActiveExecutionTurnItem) {
    let Some(alias_source) = turn.items.iter().find(|existing| {
        existing.request_id.is_some()
            || existing.user_message_id.is_some()
            || existing.placeholder_message_id.is_some()
    }) else {
        return;
    };
    if item.request_id.is_none() {
        item.request_id = alias_source.request_id.clone();
    }
    if item.user_message_id.is_none() {
        item.user_message_id = alias_source.user_message_id.clone();
    }
    if item.placeholder_message_id.is_none() {
        item.placeholder_message_id = alias_source.placeholder_message_id.clone();
    }
}

fn current_turn_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "cancelled"
            | "canceled"
    )
}

fn current_turn_item_status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "running"
            | "started"
            | "streaming"
            | "blocked"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

impl SessionStore {
    fn sync_session_workspace_binding(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&magi_core::WorkspaceId>,
    ) {
        let Some(workspace_id) = workspace_id else {
            return;
        };
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
        {
            session.workspace_id = Some(workspace_id.to_string());
            session.updated_at = UtcMillis::now();
        }
    }

    fn ownership_from_active_execution_chain(chain: &ActiveExecutionChain) -> ExecutionOwnership {
        let primary_branch = chain.branches.iter().find(|branch| branch.is_primary);
        ExecutionOwnership {
            session_id: Some(chain.session_id.clone()),
            workspace_id: chain.workspace_id.clone(),
            mission_id: Some(chain.mission_id.clone()),
            task_id: primary_branch
                .map(|branch| branch.task_id.clone())
                .or_else(|| chain.active_branch_task_ids.first().cloned())
                .or_else(|| Some(chain.root_task_id.clone())),
            worker_id: primary_branch
                .map(|branch| branch.worker_id.clone())
                .or_else(|| chain.active_worker_bindings.first().cloned()),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        }
    }

    fn upsert_runtime_sidecar_with_reason(
        &self,
        sidecar: SessionRuntimeSidecar,
        reason: SessionSidecarFlushReason,
    ) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state
            .execution_sidecar_store
            .upsert_runtime_sidecar(sidecar);
        drop(state);
        self.mark_sidecar_dirty(reason);
    }

    fn derive_sidecar_status(
        ownership: &ExecutionOwnership,
        recovery_id: Option<&str>,
        existing_status: Option<&SessionExecutionSidecarStatus>,
    ) -> SessionExecutionSidecarStatus {
        let has_ownership = [
            ownership.session_id.is_some(),
            ownership.workspace_id.is_some(),
            ownership.mission_id.is_some(),
            ownership.task_id.is_some(),
            ownership.worker_id.is_some(),
            ownership.execution_chain_ref.is_some(),
        ]
        .into_iter()
        .any(|field| field);

        if !has_ownership {
            if recovery_id.is_some() {
                SessionExecutionSidecarStatus::RecoveryLinked
            } else {
                SessionExecutionSidecarStatus::Detached
            }
        } else if matches!(
            existing_status,
            Some(SessionExecutionSidecarStatus::Resumed)
        ) {
            SessionExecutionSidecarStatus::Resumed
        } else if recovery_id.is_some() {
            SessionExecutionSidecarStatus::RecoveryLinked
        } else {
            SessionExecutionSidecarStatus::Bound
        }
    }

    pub fn upsert_runtime_sidecar(&self, sidecar: SessionRuntimeSidecar) {
        self.upsert_runtime_sidecar_with_reason(
            sidecar,
            SessionSidecarFlushReason::UpsertRuntimeSidecar,
        );
    }

    pub fn bind_execution_ownership(&self, session_id: SessionId, ownership: ExecutionOwnership) {
        let session_key = session_id.clone();
        let existing = self.runtime_sidecar(&session_id);
        let recovery_id = existing
            .as_ref()
            .and_then(|sidecar| sidecar.recovery_id.clone());
        let current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let requested_workspace_id = ownership.workspace_id.clone().or_else(|| {
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.ownership.workspace_id.clone())
        });
        let mut active_execution_chain = existing
            .as_ref()
            .and_then(|sidecar| sidecar.active_execution_chain.clone());
        if let Some(chain) = active_execution_chain.as_mut()
            && chain.workspace_id.is_none()
        {
            chain.workspace_id = requested_workspace_id;
        }
        let ownership = if let Some(chain) = active_execution_chain.as_ref() {
            Self::ownership_from_active_execution_chain(chain)
        } else {
            ExecutionOwnership {
                execution_chain_ref: ownership.execution_chain_ref.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|sidecar| sidecar.ownership.execution_chain_ref.clone())
                }),
                ..ownership
            }
        };
        let status = Self::derive_sidecar_status(
            &ownership,
            recovery_id.as_deref(),
            existing.as_ref().map(|sidecar| &sidecar.status),
        );
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id,
                ownership: ownership.clone(),
                recovery_id: recovery_id.clone(),
                current_turn,
                active_execution_chain,
                status,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::BindExecutionOwnership,
        );
        self.sync_session_workspace_binding(&session_key, ownership.workspace_id.as_ref());
    }

    pub fn upsert_active_execution_chain(
        &self,
        session_id: SessionId,
        mut active_execution_chain: ActiveExecutionChain,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let existing = self.runtime_sidecar(&session_id);
        if active_execution_chain.session_id != session_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 active_execution_chain.session_id 与 session_id 不一致: {} != {}",
                    active_execution_chain.session_id, session_id
                ),
            });
        }
        active_execution_chain.normalize();
        let recovery_id = active_execution_chain.recovery_ref.clone().or_else(|| {
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.recovery_id.clone())
        });
        let incoming_execution_chain_ref = active_execution_chain.execution_chain_ref.clone();
        let existing_current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let existing_execution_chain_ref = existing.as_ref().and_then(|sidecar| {
            sidecar
                .active_execution_chain
                .as_ref()
                .map(|chain| chain.execution_chain_ref.as_str())
        });
        let current_turn = active_execution_chain.current_turn.clone().or_else(|| {
            (existing_execution_chain_ref == Some(incoming_execution_chain_ref.as_str()))
                .then(|| existing_current_turn.clone())
                .flatten()
        });
        active_execution_chain.current_turn = current_turn.clone();
        let active_execution_chain = Some(active_execution_chain);
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
        let status = Self::derive_sidecar_status(
            &ownership,
            recovery_id.as_deref(),
            existing.as_ref().map(|sidecar| &sidecar.status),
        );
        let updated = SessionRuntimeSidecar {
            session_id,
            ownership,
            recovery_id,
            current_turn,
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::UpsertActiveExecutionChain,
        );
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok(updated)
    }

    pub fn apply_recovery_resume_input(
        &self,
        session_id: SessionId,
        input: RecoveryResumeInput,
    ) -> DomainResult<()> {
        let existing = self.runtime_sidecar(&session_id);
        let execution_chain_ref = if let Some(existing) = existing.as_ref() {
            if let Some(recovery_id) = existing.recovery_id.as_deref()
                && recovery_id != input.recovery_id.as_str()
            {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "session_runtime_sidecar 的 recovery_id 与 recovery input 不一致: {recovery_id} != {}",
                        input.recovery_id
                    ),
                });
            }
            match (
                existing.ownership.execution_chain_ref.clone(),
                input.ownership.execution_chain_ref.clone(),
            ) {
                (Some(left), Some(right)) if left != right => {
                    return Err(DomainError::InvalidState {
                        message: format!(
                            "session_runtime_sidecar 的 execution_chain_ref 与 recovery input 不一致: {left} != {right}"
                        ),
                    });
                }
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
                (Some(left), Some(_)) => Some(left),
            }
        } else {
            input.ownership.execution_chain_ref.clone()
        };
        let active_execution_chain = existing
            .as_ref()
            .and_then(|sidecar| sidecar.active_execution_chain.clone())
            .map(|mut chain| {
                chain.recovery_ref = Some(input.recovery_id.clone());
                chain
            });
        let current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let ownership = if let Some(chain) = active_execution_chain.as_ref() {
            Self::ownership_from_active_execution_chain(chain)
        } else {
            ExecutionOwnership {
                execution_chain_ref,
                ..input.ownership
            }
        };
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership: ownership.clone(),
                recovery_id: Some(input.recovery_id),
                current_turn,
                active_execution_chain,
                status: SessionExecutionSidecarStatus::RecoveryLinked,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::ApplyRecoveryResumeInput,
        );
        self.sync_session_workspace_binding(&session_id, ownership.workspace_id.as_ref());
        Ok(())
    }

    pub fn apply_resume_execution_target(
        &self,
        session_id: &SessionId,
        target: &TaskExecutionTarget,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let recovery_id = target.recovery_id.as_deref();
        if let Some(existing_recovery_id) = existing.recovery_id.as_deref()
            && recovery_id.is_some_and(|value| value != existing_recovery_id)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 recovery_id 与恢复目标不一致: {existing_recovery_id} != {}",
                    recovery_id.unwrap_or_default()
                ),
            });
        }
        if let Some(execution_chain_ref) = existing.ownership.execution_chain_ref.as_deref()
            && target
                .execution_chain_ref
                .as_deref()
                .is_some_and(|value| value != execution_chain_ref)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 execution_chain_ref 与恢复目标不一致: {execution_chain_ref} != {}",
                    target.execution_chain_ref.as_deref().unwrap_or_default()
                ),
            });
        }
        let active_execution_chain = existing.active_execution_chain.clone().map(|mut chain| {
            if let Some(recovery_ref) = target.recovery_id.clone() {
                chain.recovery_ref = Some(recovery_ref);
            }
            chain
        });
        let execution_chain_ref = match (
            existing.ownership.execution_chain_ref.clone(),
            target.execution_chain_ref.clone(),
        ) {
            (Some(left), Some(right)) if left != right => {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "session_runtime_sidecar 的 execution_chain_ref 与恢复目标不一致: {left} != {right}"
                    ),
                });
            }
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
            (Some(left), Some(_)) => Some(left),
        };
        let updated = SessionRuntimeSidecar {
            session_id: session_id.clone(),
            ownership: ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: existing.ownership.workspace_id,
                mission_id: Some(target.mission_id.clone()),
                task_id: Some(target.task_id.clone()),
                worker_id: target.requested_worker_id.clone(),
                execution_chain_ref,
            },
            recovery_id: target.recovery_id.clone(),
            current_turn: existing.current_turn,
            active_execution_chain,
            status: SessionExecutionSidecarStatus::Resumed,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::ApplyResumeExecutionTarget,
        );
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok(updated)
    }

    pub fn attach_recovery_id(
        &self,
        session_id: &SessionId,
        recovery_id: Option<String>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let status = Self::derive_sidecar_status(
            &existing.ownership,
            recovery_id.as_deref(),
            Some(&existing.status),
        );
        let active_execution_chain = existing.active_execution_chain.map(|mut chain| {
            chain.recovery_ref = recovery_id.clone();
            chain
        });
        let updated = SessionRuntimeSidecar {
            session_id: existing.session_id,
            ownership: existing.ownership,
            recovery_id,
            current_turn: existing.current_turn,
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::AttachRecoveryRef,
        );
        Ok(updated)
    }

    pub fn attach_recovery_ref(
        &self,
        session_id: &SessionId,
        recovery_ref: Option<String>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        self.attach_recovery_id(session_id, recovery_ref)
    }

    pub fn update_active_execution_branch_snapshot(
        &self,
        task_id: &TaskId,
        worker_id: WorkerId,
        stage: String,
        lease_id: Option<LeaseId>,
        execution_intent_ref: Option<String>,
        binding_lifecycle: Option<String>,
        checkpoint_stage: Option<String>,
        next_step_index: Option<usize>,
        checkpoint_at: Option<UtcMillis>,
        resume_mode: Option<String>,
        resume_token: Option<String>,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            'updated: {
                for sidecar in &mut state.execution_sidecar_store.runtime_sidecars {
                    let Some(chain) = sidecar.active_execution_chain.as_mut() else {
                        continue;
                    };
                    let Some(branch) = chain
                        .branches
                        .iter_mut()
                        .find(|branch| &branch.task_id == task_id)
                    else {
                        continue;
                    };
                    branch.worker_id = worker_id.clone();
                    branch.stage = stage.clone();
                    branch.lease_id = lease_id.clone();
                    branch.execution_intent_ref = execution_intent_ref.clone();
                    branch.binding_lifecycle = binding_lifecycle.clone();
                    branch.checkpoint_stage = checkpoint_stage.clone();
                    branch.next_step_index = next_step_index;
                    branch.checkpoint_at = checkpoint_at;
                    branch.resume_mode = resume_mode.clone();
                    branch.resume_token = resume_token.clone();
                    if let Some(turn) = sidecar.current_turn.as_mut() {
                        if let Some(lane) = turn
                            .worker_lanes
                            .iter_mut()
                            .find(|lane| lane.task_id == branch.task_id)
                        {
                            lane.worker_id = branch.worker_id.clone();
                        }
                        turn.normalize();
                    }
                    chain.active_branch_task_ids = chain
                        .branches
                        .iter()
                        .map(|entry| entry.task_id.clone())
                        .collect();
                    chain.active_worker_bindings = chain
                        .branches
                        .iter()
                        .map(|entry| entry.worker_id.clone())
                        .collect();
                    chain.normalize();
                    sidecar.ownership = Self::ownership_from_active_execution_chain(chain);
                    let existing_status = sidecar.status.clone();
                    sidecar.status = Self::derive_sidecar_status(
                        &sidecar.ownership,
                        sidecar.recovery_id.as_deref(),
                        Some(&existing_status),
                    );
                    sidecar.updated_at = UtcMillis::now();
                    break 'updated Some(sidecar.clone());
                }
                None
            }
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateActiveExecutionBranchSnapshot);
        }
        Ok(updated)
    }

    pub fn upsert_current_turn(
        &self,
        session_id: SessionId,
        mut turn: ActiveExecutionTurn,
    ) -> DomainResult<SessionRuntimeSidecar> {
        turn.normalize();
        let existing = self.runtime_sidecar(&session_id);
        let (ownership, recovery_id, active_execution_chain, status) =
            if let Some(existing) = existing {
                (
                    existing.ownership,
                    existing.recovery_id,
                    existing.active_execution_chain,
                    existing.status,
                )
            } else {
                (
                    ExecutionOwnership {
                        session_id: Some(session_id.clone()),
                        ..ExecutionOwnership::default()
                    },
                    None,
                    None,
                    SessionExecutionSidecarStatus::Detached,
                )
            };
        let updated = SessionRuntimeSidecar {
            session_id,
            ownership,
            recovery_id,
            current_turn: Some(turn),
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::UpsertCurrentTurn,
        );
        Ok(updated)
    }

    pub fn append_current_turn_item(
        &self,
        session_id: &SessionId,
        mut item: ActiveExecutionTurnItem,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(sidecar) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter_mut()
                .find(|sidecar| &sidecar.session_id == session_id)
            else {
                return Err(DomainError::NotFound {
                    entity: "session_runtime_sidecar",
                });
            };
            let Some(turn) = sidecar.current_turn.as_mut() else {
                return Ok(None);
            };
            let next_item_seq = turn
                .items
                .iter()
                .map(|existing| existing.item_seq)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            if item.item_seq == 0 {
                item.item_seq = next_item_seq;
            }
            inherit_current_turn_aliases(turn, &mut item);
            turn.items.push(item);
            turn.normalize();
            if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                chain.current_turn = sidecar.current_turn.clone();
                chain.normalize();
            }
            sidecar.updated_at = UtcMillis::now();
            Some(sidecar.clone())
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::AppendCurrentTurnItem);
        }
        Ok(updated)
    }

    pub fn upsert_current_turn_item(
        &self,
        session_id: &SessionId,
        mut item: ActiveExecutionTurnItem,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(sidecar) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter_mut()
                .find(|sidecar| &sidecar.session_id == session_id)
            else {
                return Err(DomainError::NotFound {
                    entity: "session_runtime_sidecar",
                });
            };
            let Some(turn) = sidecar.current_turn.as_mut() else {
                return Ok(None);
            };

            if let Some(existing) = turn
                .items
                .iter_mut()
                .find(|existing| existing.item_id == item.item_id)
            {
                if item.item_seq == 0 {
                    item.item_seq = existing.item_seq;
                }
                if item.request_id.is_none() {
                    item.request_id = existing.request_id.clone();
                }
                if item.user_message_id.is_none() {
                    item.user_message_id = existing.user_message_id.clone();
                }
                if item.placeholder_message_id.is_none() {
                    item.placeholder_message_id = existing.placeholder_message_id.clone();
                }
                *existing = item;
            } else {
                let next_item_seq = turn
                    .items
                    .iter()
                    .map(|existing| existing.item_seq)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                if item.item_seq == 0 {
                    item.item_seq = next_item_seq;
                }
                inherit_current_turn_aliases(turn, &mut item);
                turn.items.push(item);
            }

            turn.normalize();
            if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                chain.current_turn = sidecar.current_turn.clone();
                chain.normalize();
            }
            sidecar.updated_at = UtcMillis::now();
            Some(sidecar.clone())
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::AppendCurrentTurnItem);
        }
        Ok(updated)
    }

    pub fn update_current_turn_status(
        &self,
        session_id: &SessionId,
        status: impl Into<String>,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(sidecar) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter_mut()
                .find(|sidecar| &sidecar.session_id == session_id)
            else {
                return Err(DomainError::NotFound {
                    entity: "session_runtime_sidecar",
                });
            };
            let Some(turn) = sidecar.current_turn.as_mut() else {
                return Ok(None);
            };
            turn.status = status.into();
            if turn.completed_at.is_none()
                && matches!(turn.status.as_str(), "completed" | "failed" | "cancelled")
            {
                turn.completed_at = Some(UtcMillis::now());
            }
            turn.normalize();
            if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                chain.current_turn = sidecar.current_turn.clone();
                chain.normalize();
            }
            sidecar.updated_at = UtcMillis::now();
            Some(sidecar.clone())
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
        Ok(updated)
    }

    pub fn cancel_current_turn(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(sidecar) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter_mut()
                .find(|sidecar| &sidecar.session_id == session_id)
            else {
                return Err(DomainError::NotFound {
                    entity: "session_runtime_sidecar",
                });
            };
            let Some(turn) = sidecar.current_turn.as_mut() else {
                return Ok(None);
            };
            if !current_turn_status_is_terminal(&turn.status) {
                let now = UtcMillis::now();
                for item in &mut turn.items {
                    if current_turn_item_status_is_active(&item.status) {
                        item.status = "cancelled".to_string();
                    }
                    if item
                        .tool_status
                        .as_deref()
                        .is_some_and(current_turn_item_status_is_active)
                    {
                        item.tool_status = Some("cancelled".to_string());
                    }
                }
                turn.status = "cancelled".to_string();
                if turn.completed_at.is_none() {
                    turn.completed_at = Some(now);
                }
            }
            turn.normalize();
            if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                chain.current_turn = sidecar.current_turn.clone();
                chain.normalize();
            }
            sidecar.updated_at = UtcMillis::now();
            Some(sidecar.clone())
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
        Ok(updated)
    }

    pub fn clear_execution_ownership(&self, session_id: &SessionId) -> DomainResult<()> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let recovery_id = existing.recovery_id.clone();
        let current_turn = existing.current_turn.clone();
        let active_execution_chain = existing.active_execution_chain.clone();
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
        let status =
            Self::derive_sidecar_status(&ownership, recovery_id.as_deref(), Some(&existing.status));
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership,
                recovery_id,
                current_turn,
                active_execution_chain,
                status,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::ClearExecutionOwnership,
        );
        Ok(())
    }

    pub fn flush_execution_sidecars_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnOnce(&SessionExecutionSidecarStoreState) -> Result<(), E>,
    {
        let version = {
            let flush_state = self
                .sidecar_flush_state
                .read()
                .expect("session sidecar flush state read lock poisoned");
            if flush_state.current_version == flush_state.flushed_version {
                return Ok(false);
            }
            flush_state.current_version
        };
        let snapshot = self.execution_sidecar_store_state();
        persist(&snapshot)?;
        let mut flush_state = self
            .sidecar_flush_state
            .write()
            .expect("session sidecar flush state write lock poisoned");
        flush_state.flushed_version = flush_state.flushed_version.max(version);
        let now = UtcMillis::now();
        flush_state.last_flush_at = Some(now);
        if flush_state.current_version == flush_state.flushed_version {
            flush_state.next_flush_hint = None;
        } else if flush_state.next_flush_hint.is_none() {
            flush_state.next_flush_hint = flush_state.last_dirty_at.or(Some(now));
        }
        Ok(true)
    }
}
