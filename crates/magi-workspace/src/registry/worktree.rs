use super::WorkspaceStore;
use crate::models::{WorkspaceRecord, WorkspaceStoreState, WorktreeAllocation};
use magi_core::{
    AbsolutePath, DomainError, DomainResult, ExecutionOwnership, SessionId, TaskId, UtcMillis,
    WorkerId, WorkspaceId,
};

#[derive(Clone, Copy)]
struct OwnershipFilter<'a> {
    session_id: Option<&'a SessionId>,
    task_id: Option<&'a TaskId>,
    worker_id: Option<&'a WorkerId>,
}

impl OwnershipFilter<'_> {
    fn matches(self, allocation: &WorktreeAllocation) -> bool {
        allocation.active
            && self.session_id.is_none_or(|session_id| {
                allocation.ownership.session_id.as_ref() == Some(session_id)
            })
            && self
                .task_id
                .is_none_or(|task_id| allocation.ownership.task_id.as_ref() == Some(task_id))
            && self
                .worker_id
                .is_none_or(|worker_id| allocation.ownership.worker_id.as_ref() == Some(worker_id))
    }
}

fn active_ownership_conflict_index(
    state: &WorkspaceStoreState,
    workspace_id: &WorkspaceId,
    ownership: &ExecutionOwnership,
) -> Option<usize> {
    let ownership_scoped = ownership.session_id.is_some()
        || ownership.task_id.is_some()
        || ownership.worker_id.is_some();
    if !ownership_scoped {
        return None;
    }
    state.worktree_allocations.iter().position(|allocation| {
        allocation.active
            && &allocation.workspace_id == workspace_id
            && allocation.ownership.session_id == ownership.session_id
            && allocation.ownership.task_id == ownership.task_id
            && allocation.ownership.worker_id == ownership.worker_id
    })
}

fn release_workspace_allocations(
    state: &mut WorkspaceStoreState,
    workspace_id: &WorkspaceId,
    released_at: UtcMillis,
) {
    for allocation in state
        .worktree_allocations
        .iter_mut()
        .filter(|allocation| &allocation.workspace_id == workspace_id)
    {
        allocation.active = false;
        allocation.released_at = Some(released_at);
    }
}

fn clear_workspace_root_if_inactive(state: &mut WorkspaceStoreState, workspace_id: &WorkspaceId) {
    let has_active = state
        .worktree_allocations
        .iter()
        .any(|allocation| &allocation.workspace_id == workspace_id && allocation.active);
    if has_active {
        return;
    }

    if let Some(workspace) = state
        .workspaces
        .iter_mut()
        .find(|workspace| &workspace.workspace_id == workspace_id)
    {
        workspace.worktree_root = None;
        workspace.updated_at = UtcMillis::now();
    }
}

impl WorkspaceStore {
    pub fn assign_worktree_root(
        &self,
        workspace_id: &WorkspaceId,
        worktree_root: AbsolutePath,
    ) -> DomainResult<WorkspaceRecord> {
        self.assign_worktree_root_for_execution(
            workspace_id,
            ExecutionOwnership::default(),
            worktree_root,
        )
    }

    pub fn assign_worktree_root_for_execution(
        &self,
        workspace_id: &WorkspaceId,
        ownership: ExecutionOwnership,
        worktree_root: AbsolutePath,
    ) -> DomainResult<WorkspaceRecord> {
        let mut state = self.write_state();
        if let Some(index) = active_ownership_conflict_index(&state, workspace_id, &ownership) {
            let existing_root = state.worktree_allocations[index].worktree_root.clone();
            if existing_root != worktree_root {
                return Err(DomainError::AlreadyExists {
                    entity: "worktree_allocation",
                });
            }
            let workspace = state
                .workspaces
                .iter_mut()
                .find(|workspace| &workspace.workspace_id == workspace_id)
                .ok_or(DomainError::NotFound {
                    entity: "workspace",
                })?;
            workspace.worktree_root = Some(existing_root);
            workspace.updated_at = UtcMillis::now();
            return Ok(workspace.clone());
        }

        let now = UtcMillis::now();
        let updated_workspace = {
            let workspace = state
                .workspaces
                .iter_mut()
                .find(|workspace| &workspace.workspace_id == workspace_id)
                .ok_or(DomainError::NotFound {
                    entity: "workspace",
                })?;
            workspace.worktree_root = Some(worktree_root.clone());
            workspace.updated_at = now;
            workspace.clone()
        };

        state.worktree_allocations.push(WorktreeAllocation {
            allocation_id: format!("worktree-allocation-{}", now.0),
            workspace_id: workspace_id.clone(),
            ownership,
            worktree_root,
            active: true,
            created_at: now,
            released_at: None,
        });
        Self::sort_worktree_allocations(&mut state.worktree_allocations);
        Ok(updated_workspace)
    }

    pub fn release_worktree_root(
        &self,
        workspace_id: &WorkspaceId,
    ) -> DomainResult<WorkspaceRecord> {
        let mut state = self.write_state();
        let released_at = UtcMillis::now();
        let updated_workspace = {
            let workspace = state
                .workspaces
                .iter_mut()
                .find(|workspace| &workspace.workspace_id == workspace_id)
                .ok_or(DomainError::NotFound {
                    entity: "workspace",
                })?;
            workspace.worktree_root = None;
            workspace.updated_at = released_at;
            workspace.clone()
        };
        release_workspace_allocations(&mut state, workspace_id, released_at);
        Self::sort_worktree_allocations(&mut state.worktree_allocations);
        Ok(updated_workspace)
    }

    pub fn release_worktree_allocation(
        &self,
        allocation_id: &str,
    ) -> DomainResult<WorktreeAllocation> {
        let mut state = self.write_state();
        let released_at = UtcMillis::now();
        let allocation = state
            .worktree_allocations
            .iter_mut()
            .find(|allocation| allocation.allocation_id == allocation_id)
            .ok_or(DomainError::NotFound {
                entity: "worktree_allocation",
            })?;
        if !allocation.active {
            return Err(DomainError::InvalidState {
                message: format!("worktree_allocation {allocation_id} 已被释放"),
            });
        }

        allocation.active = false;
        allocation.released_at = Some(released_at);
        let released = allocation.clone();
        clear_workspace_root_if_inactive(&mut state, &released.workspace_id);
        Self::sort_worktree_allocations(&mut state.worktree_allocations);
        Ok(released)
    }

    pub fn worktree_allocations_by_ownership(
        &self,
        session_id: Option<&SessionId>,
        task_id: Option<&TaskId>,
        worker_id: Option<&WorkerId>,
    ) -> Vec<WorktreeAllocation> {
        let filter = OwnershipFilter {
            session_id,
            task_id,
            worker_id,
        };
        let mut allocations = self
            .read_state()
            .worktree_allocations
            .iter()
            .filter(|allocation| filter.matches(allocation))
            .cloned()
            .collect::<Vec<_>>();
        Self::sort_worktree_allocations(&mut allocations);
        allocations
    }

    pub fn active_worktree_allocations(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Vec<WorktreeAllocation> {
        let mut allocations = self
            .read_state()
            .worktree_allocations
            .iter()
            .filter(|allocation| &allocation.workspace_id == workspace_id && allocation.active)
            .cloned()
            .collect::<Vec<_>>();
        Self::sort_worktree_allocations(&mut allocations);
        allocations
    }

    pub fn worktree_allocations(&self) -> Vec<WorktreeAllocation> {
        let mut allocations = self.read_state().worktree_allocations.clone();
        Self::sort_worktree_allocations(&mut allocations);
        allocations
    }
}
