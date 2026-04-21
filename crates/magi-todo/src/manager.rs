use std::collections::{BinaryHeap, HashSet};
use std::cmp::Reverse;

use magi_core::UtcMillis;

use crate::governance::{derive_todo_execution_gate, TodoExecutionChecks, TodoExecutionGate};
use crate::repository::InMemoryTodoRepository;
use crate::types::{
    ApprovalSeverity, ApprovalStatus, CreateTodoParams, PlanReviewFeedback,
    TodoExecutionBlocker, TodoOutput, TodoProjectionStatus, TodoQuery, TodoSource,
    TodoStats, TodoStatus, UnifiedTodo, UpdateTodoParams,
};

fn now_millis() -> UtcMillis {
    UtcMillis::now()
}

fn generate_todo_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("todo_{}_{:x}", ts, rand_u32())
}

fn rand_u32() -> u32 {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    tick.wrapping_add(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PriorityEntry {
    priority: u8,
    todo_id: String,
}

impl PartialOrd for PriorityEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoEvent {
    Created { todo_id: String },
    Ready { todo_id: String },
    Started { todo_id: String },
    Progress { todo_id: String, progress: u8 },
    Completed { todo_id: String },
    Failed { todo_id: String, error: String },
    Blocked { todo_id: String, reason: String },
    Unblocked { todo_id: String },
    Skipped { todo_id: String },
    Cancelled { todo_id: String, reason: Option<String> },
    Timeout { todo_id: String },
    Retrying { todo_id: String },
    ApprovalRequested { todo_id: String },
    Approved { todo_id: String },
    Rejected { todo_id: String },
}

pub struct TodoManager {
    repository: InMemoryTodoRepository,
    queue: BinaryHeap<Reverse<PriorityEntry>>,
    queue_ids: HashSet<String>,
    available_contracts: HashSet<String>,
    events: Vec<TodoEvent>,
}

impl Default for TodoManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TodoManager {
    pub fn new() -> Self {
        Self {
            repository: InMemoryTodoRepository::new(),
            queue: BinaryHeap::new(),
            queue_ids: HashSet::new(),
            available_contracts: HashSet::new(),
            events: Vec::new(),
        }
    }

    pub fn drain_events(&mut self) -> Vec<TodoEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn initialize(&mut self) {
        self.queue.clear();
        self.queue_ids.clear();
        self.available_contracts.clear();
        self.events.clear();

        let todos: Vec<UnifiedTodo> = self.repository.all().into_iter().cloned().collect();

        for todo in &todos {
            if todo.status == TodoStatus::Completed {
                for contract in &todo.produces_contracts {
                    self.available_contracts.insert(contract.clone());
                }
            }
        }

        for todo in &todos {
            if TodoProjectionStatus::from(todo.status).is_execution_candidate() {
                self.check_and_update_status(&todo.id.clone());
            }
        }
    }

    // ========================================================================
    // CRUD
    // ========================================================================

    pub fn create(&mut self, params: CreateTodoParams) -> Result<UnifiedTodo, String> {
        let now = now_millis();
        let session_id = params
            .session_id
            .ok_or_else(|| "session_id is required".to_string())?;

        let todo_id = generate_todo_id();
        let timeout_at = params.timeout_ms.map(|t| UtcMillis(now.0 + t));

        let todo = UnifiedTodo {
            id: todo_id.clone(),
            session_id,
            mission_id: params.mission_id,
            assignment_id: params.assignment_id,
            parent_id: params.parent_id,
            source: params.source.unwrap_or(TodoSource::PlannerMacro),
            content: params.content,
            reasoning: params.reasoning,
            expected_output: params.expected_output,
            prompt: params.prompt,
            todo_type: params.todo_type,
            worker_id: params.worker_id,
            required: params.required.unwrap_or(true),
            effort_weight: params.effort_weight.unwrap_or(1.0),
            waiver_approved: false,
            priority: params.priority.unwrap_or(3),
            depends_on: params.depends_on.unwrap_or_default(),
            required_contracts: params.required_contracts.unwrap_or_default(),
            produces_contracts: params.produces_contracts.unwrap_or_default(),
            execution_blocker: None,
            blocked_reason: None,
            out_of_scope: false,
            approval_status: None,
            approval_severity: None,
            approval_note: None,
            review_status: None,
            review_feedback: None,
            status: TodoStatus::Pending,
            progress: 0,
            timeout_ms: params.timeout_ms,
            timeout_at,
            retry_count: 0,
            max_retries: params.max_retries.unwrap_or(3),
            output: None,
            error: None,
            modified_files: None,
            target_files: params.target_files,
            created_at: now,
            started_at: None,
            completed_at: None,
        };

        self.repository.save(todo.clone());
        self.check_and_update_status(&todo_id);
        self.events.push(TodoEvent::Created { todo_id });
        Ok(todo)
    }

    pub fn create_batch(&mut self, params_list: Vec<CreateTodoParams>) -> Result<Vec<UnifiedTodo>, String> {
        let mut todos = Vec::with_capacity(params_list.len());
        for params in params_list {
            todos.push(self.create(params)?);
        }
        Ok(todos)
    }

    pub fn get(&self, todo_id: &str) -> Option<&UnifiedTodo> {
        self.repository.get(todo_id)
    }

    pub fn update(&mut self, todo_id: &str, updates: UpdateTodoParams) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?
            .clone();

        let next = apply_updates(todo, updates);
        self.repository.save(next);
        Ok(())
    }

    pub fn delete(&mut self, todo_id: &str) {
        self.remove_from_queue(todo_id);
        let _ = self.repository.delete(todo_id);
    }

    // ========================================================================
    // Query
    // ========================================================================

    pub fn get_by_mission(&self, mission_id: &str) -> Vec<&UnifiedTodo> {
        self.repository.get_by_mission(mission_id)
    }

    pub fn get_by_assignment(&self, assignment_id: &str) -> Vec<&UnifiedTodo> {
        self.repository.get_by_assignment(assignment_id)
    }

    pub fn query(&self, query: &TodoQuery) -> Vec<&UnifiedTodo> {
        self.repository.query(query)
    }

    pub fn cancel_by_query(&mut self, query: &TodoQuery, reason: Option<&str>) -> Vec<String> {
        let ids: Vec<String> = self
            .repository
            .query(query)
            .iter()
            .filter(|t| can_cancel(t.status))
            .map(|t| t.id.clone())
            .collect();
        let mut cancelled = Vec::new();
        for id in ids {
            if self.cancel(&id, reason.map(|s| s.to_string())).is_ok() {
                cancelled.push(id);
            }
        }
        cancelled
    }

    // ========================================================================
    // State transitions
    // ========================================================================

    pub fn start(&mut self, todo_id: &str) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        let gate = self.evaluate_execution_gate(todo);
        if !gate.executable {
            return Err(format!("Cannot start todo: {:?}", gate.reason));
        }
        if todo.status != TodoStatus::Pending {
            return Err(format!("Cannot start todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Running;
        next.execution_blocker = None;
        next.blocked_reason = None;
        next.started_at = Some(now_millis());
        self.repository.save(next);
        self.remove_from_queue(todo_id);
        self.events.push(TodoEvent::Started {
            todo_id: todo_id.to_string(),
        });
        Ok(())
    }

    pub fn update_progress(&mut self, todo_id: &str, progress: u8) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        let mut next = todo.clone();
        next.progress = progress.min(100);
        self.repository.save(next);
        self.events.push(TodoEvent::Progress {
            todo_id: todo_id.to_string(),
            progress,
        });
        Ok(())
    }

    pub fn complete(&mut self, todo_id: &str, output: Option<TodoOutput>) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status == TodoStatus::Completed {
            return Ok(());
        }
        if todo.status != TodoStatus::Running {
            return Err(format!("Cannot complete todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Completed;
        next.progress = 100;
        next.completed_at = Some(now_millis());
        if let Some(ref out) = output {
            next.modified_files = Some(out.modified_files.clone());
        }
        next.output = output;
        self.repository.save(next.clone());

        for contract in &next.produces_contracts {
            self.available_contracts.insert(contract.clone());
        }

        self.events.push(TodoEvent::Completed {
            todo_id: todo_id.to_string(),
        });

        self.trigger_dependent_todos(todo_id);

        if let Some(parent_id) = &next.parent_id {
            self.try_complete_parent(&parent_id.clone());
        }
        Ok(())
    }

    pub fn fail(&mut self, todo_id: &str, error: String) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status == TodoStatus::Failed {
            return Ok(());
        }
        if todo.status.is_terminal() {
            return Ok(());
        }
        if todo.status != TodoStatus::Running {
            return Err(format!("Cannot fail todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Failed;
        next.completed_at = Some(now_millis());
        next.error = Some(error.clone());
        self.repository.save(next);
        self.events.push(TodoEvent::Failed {
            todo_id: todo_id.to_string(),
            error,
        });
        Ok(())
    }

    pub fn skip(&mut self, todo_id: &str, reason: Option<String>) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status == TodoStatus::Skipped {
            return Ok(());
        }
        let projection: TodoProjectionStatus = todo.status.into();
        if !projection.is_skippable() {
            return Err(format!("Cannot skip todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Skipped;
        next.completed_at = Some(now_millis());
        if let Some(r) = &reason {
            next.blocked_reason = Some(r.clone());
        }
        self.repository.save(next.clone());
        self.remove_from_queue(todo_id);
        self.events.push(TodoEvent::Skipped {
            todo_id: todo_id.to_string(),
        });

        self.trigger_dependent_todos(todo_id);
        if let Some(parent_id) = &next.parent_id {
            self.try_complete_parent(&parent_id.clone());
        }
        Ok(())
    }

    pub fn block(
        &mut self,
        todo_id: &str,
        blocker: TodoExecutionBlocker,
        reason: String,
    ) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status.is_terminal() {
            return Err(format!("Cannot block todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Pending;
        next.execution_blocker = Some(blocker);
        next.blocked_reason = Some(reason.clone());
        next.started_at = None;
        self.repository.save(next);
        self.remove_from_queue(todo_id);
        self.events.push(TodoEvent::Blocked {
            todo_id: todo_id.to_string(),
            reason,
        });
        Ok(())
    }

    pub fn cancel(&mut self, todo_id: &str, reason: Option<String>) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status == TodoStatus::Cancelled {
            return Ok(());
        }
        if !can_cancel(todo.status) {
            return Err(format!("Cannot cancel todo in status: {:?}", todo.status));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Cancelled;
        next.completed_at = Some(now_millis());
        if let Some(r) = &reason {
            next.blocked_reason = Some(r.clone());
        }
        self.repository.save(next.clone());
        self.remove_from_queue(todo_id);
        self.events.push(TodoEvent::Cancelled {
            todo_id: todo_id.to_string(),
            reason,
        });

        self.trigger_dependent_todos(todo_id);
        if let Some(parent_id) = &next.parent_id {
            self.try_complete_parent(&parent_id.clone());
        }
        Ok(())
    }

    pub fn retry(&mut self, todo_id: &str) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status != TodoStatus::Failed {
            return Err(format!("Cannot retry todo in status: {:?}", todo.status));
        }
        if todo.retry_count >= todo.max_retries {
            return Err(format!(
                "Todo has reached max retries: {}",
                todo.max_retries
            ));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Pending;
        next.retry_count += 1;
        next.execution_blocker = None;
        next.error = None;
        next.progress = 0;
        next.completed_at = None;
        self.repository.save(next);
        self.events.push(TodoEvent::Retrying {
            todo_id: todo_id.to_string(),
        });
        self.check_and_update_status(todo_id);
        Ok(())
    }

    pub fn reset_to_pending(&mut self, todo_id: &str, force: bool) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        if todo.status == TodoStatus::Pending {
            return Ok(());
        }

        let allowed = matches!(
            todo.status,
            TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
        ) || (force && todo.status == TodoStatus::Running);

        if !allowed {
            return Err(format!(
                "Cannot reset todo to pending from status: {:?}",
                todo.status
            ));
        }

        let mut next = todo.clone();
        next.status = TodoStatus::Pending;
        next.completed_at = None;
        next.output = None;
        next.error = None;
        next.execution_blocker = None;
        next.blocked_reason = None;
        next.progress = 0;
        self.repository.save(next);
        self.check_and_update_status(todo_id);
        Ok(())
    }

    // ========================================================================
    // Claim (semi-autonomous scheduling)
    // ========================================================================

    pub fn find_claimable(
        &self,
        mission_id: &str,
        worker_id: Option<&str>,
    ) -> Vec<&UnifiedTodo> {
        let todos = self.repository.get_by_mission(mission_id);
        let mut claimable: Vec<&UnifiedTodo> = todos
            .into_iter()
            .filter(|t| {
                if t.status != TodoStatus::Pending {
                    return false;
                }
                if let Some(wid) = worker_id {
                    if t.worker_id.as_str() != wid {
                        return false;
                    }
                }
                let gate = self.evaluate_execution_gate(t);
                gate.executable
            })
            .collect();
        claimable.sort_by_key(|t| t.priority);
        claimable
    }

    pub fn try_claim(&mut self, todo_id: &str) -> Result<Option<UnifiedTodo>, String> {
        let todo = match self.repository.get(todo_id) {
            Some(t) => t,
            None => return Ok(None),
        };

        let gate = self.evaluate_execution_gate(todo);
        if !gate.executable {
            return Ok(None);
        }

        self.start(todo_id)?;
        Ok(self.repository.get_cloned(todo_id))
    }

    // ========================================================================
    // Queue
    // ========================================================================

    pub fn peek(&self) -> Option<&UnifiedTodo> {
        let entry = self.queue.peek()?;
        self.repository.get(&entry.0.todo_id)
    }

    pub fn dequeue(&mut self) -> Option<UnifiedTodo> {
        let entry = self.queue.pop()?;
        self.queue_ids.remove(&entry.0.todo_id);
        self.repository.get_cloned(&entry.0.todo_id)
    }

    pub fn dequeue_batch(&mut self, count: usize) -> Vec<UnifiedTodo> {
        let mut results = Vec::with_capacity(count);
        for _ in 0..count {
            match self.dequeue() {
                Some(todo) => results.push(todo),
                None => break,
            }
        }
        results
    }

    // ========================================================================
    // Approval
    // ========================================================================

    pub fn request_approval(
        &mut self,
        todo_id: &str,
        note: Option<String>,
        severity: Option<ApprovalSeverity>,
    ) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        let mut next = todo.clone();
        next.out_of_scope = true;
        next.approval_status = Some(ApprovalStatus::Pending);
        if let Some(s) = severity {
            next.approval_severity = Some(s);
        }
        next.approval_note = note;
        self.repository.save(next);
        self.check_and_update_status(todo_id);
        self.events.push(TodoEvent::ApprovalRequested {
            todo_id: todo_id.to_string(),
        });
        Ok(())
    }

    pub fn approve(&mut self, todo_id: &str, note: Option<String>) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        let mut next = todo.clone();
        next.approval_status = Some(ApprovalStatus::Approved);
        if let Some(n) = note {
            next.approval_note = Some(n);
        }
        self.repository.save(next);
        self.check_and_update_status(todo_id);
        self.events.push(TodoEvent::Approved {
            todo_id: todo_id.to_string(),
        });
        Ok(())
    }

    pub fn reject(&mut self, todo_id: &str, reason: String) -> Result<(), String> {
        let todo = self
            .repository
            .get(todo_id)
            .ok_or_else(|| format!("Todo not found: {}", todo_id))?;

        let mut next = todo.clone();
        next.approval_status = Some(ApprovalStatus::Rejected);
        next.approval_note = Some(reason);
        self.repository.save(next);
        self.skip(todo_id, None)?;
        self.events.push(TodoEvent::Rejected {
            todo_id: todo_id.to_string(),
        });
        Ok(())
    }

    // ========================================================================
    // Plan revision
    // ========================================================================

    pub fn revise_plan(
        &mut self,
        mission_id: &str,
        feedback: PlanReviewFeedback,
    ) -> Result<PlanRevisionResult, String> {
        let mut result = PlanRevisionResult::default();

        for todo_id in &feedback.todos_to_remove {
            self.delete(todo_id);
            result.todos_removed += 1;
        }

        for modification in &feedback.todos_to_modify {
            self.update(&modification.todo_id, modification.updates.clone())?;
            result.todos_modified += 1;
        }

        for params in feedback.todos_to_add {
            let mut p = params;
            p.mission_id = magi_core::MissionId::new(mission_id);
            self.create(p)?;
            result.todos_added += 1;
        }

        Ok(result)
    }

    // ========================================================================
    // Stats & maintenance
    // ========================================================================

    pub fn get_stats(&self) -> TodoStats {
        self.repository.get_stats()
    }

    pub fn cleanup(&mut self, older_than: u64) -> usize {
        self.repository.cleanup(older_than)
    }

    pub fn check_mission_completion(&self, mission_id: &str) -> MissionCompletionCheck {
        let todos = self.repository.get_by_mission(mission_id);
        let mut completed = 0usize;
        let mut failed = 0usize;
        let mut pending = 0usize;

        for todo in &todos {
            match todo.status {
                TodoStatus::Completed | TodoStatus::Skipped => completed += 1,
                TodoStatus::Failed => failed += 1,
                _ => pending += 1,
            }
        }

        MissionCompletionCheck {
            all_done: pending == 0 && failed == 0,
            any_failed: failed > 0,
            completed,
            failed,
            pending,
            total: todos.len(),
        }
    }

    pub fn get_execution_gate(&self, todo_id: &str) -> Option<TodoExecutionGate> {
        let todo = self.repository.get(todo_id)?;
        Some(self.evaluate_execution_gate(todo))
    }

    pub fn register_contract(&mut self, contract_id: &str) {
        self.available_contracts.insert(contract_id.to_string());
        self.recheck_blocked_todos();
    }

    pub fn handle_timeout(&mut self, todo_id: &str) {
        let status = self.repository.get(todo_id).map(|t| t.status);
        if status != Some(TodoStatus::Running) {
            return;
        }
        let _ = self.fail(todo_id, "Todo timeout".to_string());
        self.events.push(TodoEvent::Timeout {
            todo_id: todo_id.to_string(),
        });
    }

    // ========================================================================
    // Internal helpers
    // ========================================================================

    fn evaluate_execution_gate(&self, todo: &UnifiedTodo) -> TodoExecutionGate {
        let dependencies_met = self.check_dependencies(todo);
        let contracts_met = self.check_contracts(todo);
        let approval_met = !todo.out_of_scope || todo.approval_status == Some(ApprovalStatus::Approved);
        derive_todo_execution_gate(
            todo,
            &TodoExecutionChecks {
                dependencies_met,
                contracts_met,
                approval_met,
            },
            true,
        )
    }

    fn check_dependencies(&self, todo: &UnifiedTodo) -> bool {
        todo.depends_on.iter().all(|dep_id| {
            self.repository
                .get(dep_id)
                .is_some_and(|dep| dep.status == TodoStatus::Completed)
        })
    }

    fn check_contracts(&self, todo: &UnifiedTodo) -> bool {
        todo.required_contracts
            .iter()
            .all(|c| self.available_contracts.contains(c))
    }

    fn check_and_update_status(&mut self, todo_id: &str) {
        let todo = match self.repository.get(todo_id) {
            Some(t) => t.clone(),
            None => return,
        };

        if todo.status != TodoStatus::Pending {
            return;
        }

        let gate = {
            let dependencies_met = self.check_dependencies(&todo);
            let contracts_met = self.check_contracts(&todo);
            let approval_met =
                !todo.out_of_scope || todo.approval_status == Some(ApprovalStatus::Approved);
            derive_todo_execution_gate(
                &todo,
                &TodoExecutionChecks {
                    dependencies_met,
                    contracts_met,
                    approval_met,
                },
                false,
            )
        };

        if gate.executable {
            self.sync_executable_projection(todo_id);
        } else if let Some(blocker) = gate.blocked_by {
            use crate::governance::TodoExecutionGateBlocker;
            match blocker {
                TodoExecutionGateBlocker::Status => {
                    self.remove_from_queue(todo_id);
                }
                TodoExecutionGateBlocker::Dependencies => {
                    self.sync_blocked_projection(
                        todo_id,
                        TodoExecutionBlocker::Dependencies,
                        gate.reason.unwrap_or_default(),
                    );
                }
                TodoExecutionGateBlocker::Contracts => {
                    self.sync_blocked_projection(
                        todo_id,
                        TodoExecutionBlocker::Contracts,
                        gate.reason.unwrap_or_default(),
                    );
                }
                TodoExecutionGateBlocker::Approval => {
                    self.sync_blocked_projection(
                        todo_id,
                        TodoExecutionBlocker::Approval,
                        gate.reason.unwrap_or_default(),
                    );
                }
            }
        }
    }

    fn sync_blocked_projection(
        &mut self,
        todo_id: &str,
        blocker: TodoExecutionBlocker,
        reason: String,
    ) {
        let todo = match self.repository.get(todo_id) {
            Some(t) => t,
            None => return,
        };
        if todo.status != TodoStatus::Pending {
            return;
        }
        if todo.execution_blocker == Some(blocker) && todo.blocked_reason.as_deref() == Some(&reason)
        {
            self.remove_from_queue(todo_id);
            return;
        }

        let mut next = todo.clone();
        next.execution_blocker = Some(blocker);
        next.blocked_reason = Some(reason.clone());
        self.repository.save(next);
        self.remove_from_queue(todo_id);
        self.events.push(TodoEvent::Blocked {
            todo_id: todo_id.to_string(),
            reason,
        });
    }

    fn sync_executable_projection(&mut self, todo_id: &str) {
        let todo = match self.repository.get(todo_id) {
            Some(t) => t,
            None => return,
        };
        if todo.status != TodoStatus::Pending {
            return;
        }
        let was_blocked = todo.execution_blocker.is_some();

        let mut next = todo.clone();
        next.execution_blocker = None;
        next.blocked_reason = None;
        let priority = next.priority;
        self.repository.save(next);

        self.remove_from_queue(todo_id);
        self.enqueue(todo_id, priority);

        if was_blocked {
            self.events.push(TodoEvent::Ready {
                todo_id: todo_id.to_string(),
            });
            self.events.push(TodoEvent::Unblocked {
                todo_id: todo_id.to_string(),
            });
        }
    }

    fn trigger_dependent_todos(&mut self, completed_todo_id: &str) {
        let all_ids: Vec<String> = self
            .repository
            .all()
            .iter()
            .filter(|t| t.depends_on.iter().any(|d| d == completed_todo_id))
            .map(|t| t.id.clone())
            .collect();
        for id in all_ids {
            self.check_and_update_status(&id);
        }
    }

    fn try_complete_parent(&mut self, parent_id: &str) {
        let parent = match self.repository.get(parent_id) {
            Some(p) => p,
            None => return,
        };
        if parent.status == TodoStatus::Completed {
            return;
        }

        let assignment_id = parent.assignment_id.as_str().to_string();

        let (all_done, child_count, modified) = {
            let children: Vec<&UnifiedTodo> = self
                .repository
                .get_by_assignment(&assignment_id)
                .into_iter()
                .filter(|t| t.parent_id.as_deref() == Some(parent_id))
                .collect();

            if children.is_empty() {
                return;
            }

            let all_done = children
                .iter()
                .all(|c| matches!(c.status, TodoStatus::Completed | TodoStatus::Skipped));
            let count = children.len();
            let modified: Vec<String> = children
                .iter()
                .flat_map(|c| {
                    c.output
                        .as_ref()
                        .map(|o| o.modified_files.clone())
                        .unwrap_or_default()
                })
                .collect();
            (all_done, count, modified)
        };

        if !all_done {
            return;
        }

        if self.repository.get(parent_id).is_some_and(|p| p.status != TodoStatus::Running) {
            let mut p = self.repository.get(parent_id).unwrap().clone();
            p.status = TodoStatus::Running;
            p.started_at = Some(now_millis());
            self.repository.save(p);
        }

        let started = self.repository.get(parent_id).and_then(|p| p.started_at);
        let duration = started
            .map(|s| now_millis().0.saturating_sub(s.0))
            .unwrap_or(0);

        let _ = self.complete(
            parent_id,
            Some(TodoOutput {
                success: true,
                summary: format!("所有 {} 个子步骤已完成", child_count),
                modified_files: modified,
                new_contracts: None,
                issues: None,
                error: None,
                duration_ms: duration,
                token_usage: None,
            }),
        );
    }

    fn recheck_blocked_todos(&mut self) {
        let blocked_ids: Vec<String> = self
            .repository
            .all()
            .iter()
            .filter(|t| t.status == TodoStatus::Pending && t.execution_blocker.is_some())
            .map(|t| t.id.clone())
            .collect();
        for id in blocked_ids {
            self.check_and_update_status(&id);
        }
    }

    fn enqueue(&mut self, todo_id: &str, priority: u8) {
        if self.queue_ids.contains(todo_id) {
            return;
        }
        self.queue_ids.insert(todo_id.to_string());
        self.queue.push(Reverse(PriorityEntry {
            priority,
            todo_id: todo_id.to_string(),
        }));
    }

    fn remove_from_queue(&mut self, todo_id: &str) {
        if self.queue_ids.remove(todo_id) {
            let items: Vec<_> = self
                .queue
                .drain()
                .filter(|e| e.0.todo_id != todo_id)
                .collect();
            for item in items {
                self.queue.push(item);
            }
        }
    }
}

fn can_cancel(status: TodoStatus) -> bool {
    let projection: TodoProjectionStatus = status.into();
    projection.is_execution_candidate() || status == TodoStatus::Running
}

fn apply_updates(mut todo: UnifiedTodo, updates: UpdateTodoParams) -> UnifiedTodo {
    if let Some(v) = updates.content {
        todo.content = v;
    }
    if let Some(v) = updates.reasoning {
        todo.reasoning = v;
    }
    if let Some(v) = updates.expected_output {
        todo.expected_output = Some(v);
    }
    if let Some(v) = updates.priority {
        todo.priority = v;
    }
    if let Some(v) = updates.depends_on {
        todo.depends_on = v;
    }
    if let Some(v) = updates.required_contracts {
        todo.required_contracts = v;
    }
    if let Some(v) = updates.produces_contracts {
        todo.produces_contracts = v;
    }
    if let Some(v) = updates.required {
        todo.required = v;
    }
    if let Some(v) = updates.effort_weight {
        todo.effort_weight = v;
    }
    if let Some(v) = updates.waiver_approved {
        todo.waiver_approved = v;
    }
    if let Some(v) = updates.review_status {
        todo.review_status = Some(v);
    }
    if let Some(v) = updates.review_feedback {
        todo.review_feedback = Some(v);
    }
    todo
}

#[derive(Clone, Debug, Default)]
pub struct PlanRevisionResult {
    pub todos_added: usize,
    pub todos_removed: usize,
    pub todos_modified: usize,
}

#[derive(Clone, Debug)]
pub struct MissionCompletionCheck {
    pub all_done: bool,
    pub any_failed: bool,
    pub completed: usize,
    pub failed: usize,
    pub pending: usize,
    pub total: usize,
}
