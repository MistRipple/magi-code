use std::collections::HashMap;

use magi_core::DomainError;

use crate::types::{
    TodoProjectionStatus, TodoQuery, TodoSource, TodoStats, TodoStatus, TodoStatusCounts,
    TodoTypeCounts, UnifiedTodo,
};

pub struct InMemoryTodoRepository {
    entries: HashMap<String, UnifiedTodo>,
}

impl Default for InMemoryTodoRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTodoRepository {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn save(&mut self, todo: UnifiedTodo) {
        let normalized = normalize_todo_record(todo);
        self.entries.insert(normalized.id.clone(), normalized);
    }

    pub fn save_batch(&mut self, todos: Vec<UnifiedTodo>) {
        for todo in todos {
            self.save(todo);
        }
    }

    pub fn get(&self, todo_id: &str) -> Option<&UnifiedTodo> {
        self.entries.get(todo_id)
    }

    pub fn get_cloned(&self, todo_id: &str) -> Option<UnifiedTodo> {
        self.entries.get(todo_id).cloned()
    }

    pub fn delete(&mut self, todo_id: &str) -> Result<(), DomainError> {
        if self.entries.remove(todo_id).is_none() {
            return Err(DomainError::NotFound { entity: "todo" });
        }
        Ok(())
    }

    pub fn delete_batch(&mut self, todo_ids: &[String]) {
        for id in todo_ids {
            self.entries.remove(id);
        }
    }

    pub fn get_by_mission(&self, mission_id: &str) -> Vec<&UnifiedTodo> {
        self.entries
            .values()
            .filter(|t| t.mission_id.as_str() == mission_id)
            .collect()
    }

    pub fn get_by_assignment(&self, assignment_id: &str) -> Vec<&UnifiedTodo> {
        self.entries
            .values()
            .filter(|t| t.assignment_id.as_str() == assignment_id)
            .collect()
    }

    pub fn get_by_status(&self, statuses: &[TodoProjectionStatus]) -> Vec<&UnifiedTodo> {
        let canonical: Vec<TodoStatus> = statuses.iter().map(|s| s.canonicalize()).collect();
        self.entries
            .values()
            .filter(|t| canonical.contains(&t.status))
            .collect()
    }

    pub fn get_by_worker(&self, worker_id: &str) -> Vec<&UnifiedTodo> {
        self.entries
            .values()
            .filter(|t| t.worker_id.as_str() == worker_id)
            .collect()
    }

    pub fn query(&self, query: &TodoQuery) -> Vec<&UnifiedTodo> {
        self.entries
            .values()
            .filter(|t| {
                if let Some(sid) = &query.session_id {
                    if t.session_id.as_str() != sid.as_str() {
                        return false;
                    }
                }
                if let Some(mid) = &query.mission_id {
                    if t.mission_id.as_str() != mid.as_str() {
                        return false;
                    }
                }
                if let Some(aid) = &query.assignment_id {
                    if t.assignment_id.as_str() != aid.as_str() {
                        return false;
                    }
                }
                if let Some(wid) = &query.worker_id {
                    if t.worker_id.as_str() != wid.as_str() {
                        return false;
                    }
                }
                if let Some(statuses) = &query.status {
                    let canonical: Vec<TodoStatus> =
                        statuses.iter().map(|s| s.canonicalize()).collect();
                    if !canonical.contains(&t.status) {
                        return false;
                    }
                }
                if let Some(types) = &query.todo_type {
                    if !types.contains(&t.todo_type) {
                        return false;
                    }
                }
                if let Some(oos) = query.out_of_scope {
                    if t.out_of_scope != oos {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    pub fn cleanup(&mut self, older_than: u64) -> usize {
        let to_delete: Vec<String> = self
            .entries
            .values()
            .filter(|t| {
                matches!(
                    t.status,
                    TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                ) && t.created_at.0 < older_than
            })
            .map(|t| t.id.clone())
            .collect();
        let count = to_delete.len();
        for id in to_delete {
            self.entries.remove(&id);
        }
        count
    }

    pub fn get_stats(&self) -> TodoStats {
        let mut by_status = TodoStatusCounts::default();
        let mut by_type = TodoTypeCounts::default();
        let mut by_worker: HashMap<String, usize> = HashMap::new();
        let mut total_duration: u64 = 0;
        let mut completed_count: usize = 0;

        for todo in self.entries.values() {
            by_status.increment(todo.status);
            by_type.increment(todo.todo_type);
            *by_worker
                .entry(todo.worker_id.as_str().to_string())
                .or_insert(0) += 1;

            if todo.status == TodoStatus::Completed {
                if let (Some(started), Some(completed)) = (todo.started_at, todo.completed_at) {
                    total_duration += completed.0.saturating_sub(started.0);
                    completed_count += 1;
                }
            }
        }

        let total = self.entries.len();
        let completed_and_skipped = by_status.completed + by_status.skipped;
        let completion_rate = if total > 0 {
            completed_and_skipped as f64 / total as f64
        } else {
            0.0
        };
        let average_duration_ms = if completed_count > 0 {
            total_duration as f64 / completed_count as f64
        } else {
            0.0
        };

        let mut worker_vec: Vec<(String, usize)> = by_worker.into_iter().collect();
        worker_vec.sort_by(|a, b| b.1.cmp(&a.1));

        TodoStats {
            total,
            by_status,
            by_type,
            by_worker: worker_vec,
            completion_rate,
            average_duration_ms,
        }
    }

    pub fn all(&self) -> Vec<&UnifiedTodo> {
        self.entries.values().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn normalize_todo_status(status: TodoStatus) -> TodoStatus {
    status
}

fn normalize_todo_source(source: TodoSource) -> TodoSource {
    source
}

fn normalize_todo_record(mut todo: UnifiedTodo) -> UnifiedTodo {
    todo.status = normalize_todo_status(todo.status);
    todo.source = normalize_todo_source(todo.source);
    todo
}
