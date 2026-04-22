use super::SessionStore;
use crate::models::{
    ActiveExecutionChain, SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState,
    SessionRuntimeSidecar, SessionSidecarFlushReason,
};
use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, RecoveryResumeInput, SessionId,
    TaskExecutionTarget, UtcMillis,
};

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
        state.execution_sidecar_store.upsert_runtime_sidecar(sidecar);
        drop(state);
        self.mark_sidecar_dirty(reason);
    }

    fn derive_sidecar_status(
        ownership: &ExecutionOwnership,
        recovery_id: Option<&str>,
    ) -> SessionExecutionSidecarStatus {
        if matches!(
            (
                ownership.session_id.is_some(),
                ownership.workspace_id.is_some(),
                ownership.mission_id.is_some(),
                ownership.task_id.is_some(),
                ownership.worker_id.is_some(),
                ownership.execution_chain_ref.is_some()
            ),
            (false, false, false, false, false, false)
        ) {
            if recovery_id.is_some() {
                SessionExecutionSidecarStatus::RecoveryLinked
            } else {
                SessionExecutionSidecarStatus::Detached
            }
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
        let recovery_id = existing.as_ref().and_then(|sidecar| sidecar.recovery_id.clone());
        let active_execution_chain = existing
            .as_ref()
            .and_then(|sidecar| sidecar.active_execution_chain.clone());
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
        let status = Self::derive_sidecar_status(&ownership, recovery_id.as_deref());
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id,
                ownership: ownership.clone(),
                recovery_id: recovery_id.clone(),
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
        let recovery_id = active_execution_chain
            .recovery_ref
            .clone()
            .or_else(|| existing.as_ref().and_then(|sidecar| sidecar.recovery_id.clone()));
        let active_execution_chain = Some(active_execution_chain);
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
        let status = Self::derive_sidecar_status(&ownership, recovery_id.as_deref());
        let updated = SessionRuntimeSidecar {
            session_id,
            ownership,
            recovery_id,
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::UpsertActiveExecutionChain,
        );
        self.sync_session_workspace_binding(&updated.session_id, updated.ownership.workspace_id.as_ref());
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
            active_execution_chain,
            status: SessionExecutionSidecarStatus::Resumed,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::ApplyResumeExecutionTarget,
        );
        self.sync_session_workspace_binding(&updated.session_id, updated.ownership.workspace_id.as_ref());
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
        let status = Self::derive_sidecar_status(&existing.ownership, recovery_id.as_deref());
        let active_execution_chain = existing.active_execution_chain.map(|mut chain| {
            chain.recovery_ref = recovery_id.clone();
            chain
        });
        let updated = SessionRuntimeSidecar {
            session_id: existing.session_id,
            ownership: existing.ownership,
            recovery_id,
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

    pub fn clear_execution_ownership(&self, session_id: &SessionId) -> DomainResult<()> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let recovery_id = existing.recovery_id.clone();
        let active_execution_chain = existing.active_execution_chain.clone();
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
        let status = Self::derive_sidecar_status(&ownership, recovery_id.as_deref());
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership,
                recovery_id,
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
