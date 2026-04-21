use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    Pending,
    WaitingDeps,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl DispatchStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }

    fn can_reconcile_terminal(previous: Self, next: Self) -> bool {
        previous == Self::Cancelled && (next == Self::Completed || next == Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchPhase {
    Active,
    Summarizing,
    Archived,
}

impl BatchPhase {
    fn allowed_transitions(self) -> &'static [BatchPhase] {
        match self {
            Self::Active => &[Self::Summarizing, Self::Archived],
            Self::Summarizing => &[Self::Archived],
            Self::Archived => &[],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResult {
    pub success: bool,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchCollaborationContracts {
    pub producer_contracts: Vec<String>,
    pub consumer_contracts: Vec<String>,
    pub interface_contracts: Vec<String>,
    pub freeze_files: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchTaskContract {
    pub task_title: String,
    pub ownership: String,
    pub mode: String,
    pub context: Vec<String>,
    pub scope_hint: Vec<String>,
    pub files: Vec<String>,
    pub depends_on: Vec<String>,
    pub collaboration_contracts: DispatchCollaborationContracts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchEntry {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub worker: String,
    pub task_contract: DispatchTaskContract,
    pub status: DispatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<DispatchResult>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenConsumption {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchAuditLevel {
    Normal,
    Watch,
    Intervention,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchAuditIssue {
    pub task_id: String,
    pub level: DispatchAuditLevel,
    pub dimension: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchAuditOutcome {
    pub level: DispatchAuditLevel,
    pub issues: Vec<DispatchAuditIssue>,
    pub task_levels: HashMap<String, DispatchAuditLevel>,
    pub summary: AuditSummaryCount,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuditSummaryCount {
    pub normal: u32,
    pub watch: u32,
    pub intervention: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchBatchSummary {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: usize,
    pub running: usize,
    pub pending: usize,
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: bool,
    reason: Option<String>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: false,
            reason: None,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn cancel(&mut self, reason: impl Into<String>) {
        if self.cancelled {
            return;
        }
        self.reason = Some(reason.into());
        self.cancelled = true;
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct DispatchBatch {
    pub id: String,
    pub request_id: String,
    phase: BatchPhase,
    entries: Vec<DispatchEntry>,
    entry_index: HashMap<String, usize>,
    _created_at: u64,
    last_activity_at: u64,
    user_prompt: String,
    cancellation_token: CancellationToken,
    token_consumption: TokenUsage,
    audit_outcome: Option<DispatchAuditOutcome>,
    events: Vec<DispatchBatchEvent>,
}

#[derive(Clone, Debug)]
pub enum DispatchBatchEvent {
    PhaseChanged {
        batch_id: String,
        phase: BatchPhase,
    },
    TaskStatusChanged {
        task_id: String,
        status: DispatchStatus,
        result: Option<DispatchResult>,
    },
    TaskReady {
        task_id: String,
    },
    AllCompleted {
        batch_id: String,
    },
    BatchCancelled {
        batch_id: String,
        reason: String,
    },
}

impl DispatchBatch {
    pub fn new(batch_id: Option<&str>) -> Self {
        let now = now_millis();
        let id = batch_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("batch-{now}"));
        let request_id = format!("req-{now}");
        Self {
            id,
            request_id,
            phase: BatchPhase::Active,
            entries: Vec::new(),
            entry_index: HashMap::new(),
            _created_at: now,
            last_activity_at: now,
            user_prompt: String::new(),
            cancellation_token: CancellationToken::new(),
            token_consumption: TokenUsage::default(),
            audit_outcome: None,
            events: Vec::new(),
        }
    }

    pub fn phase(&self) -> BatchPhase {
        self.phase
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn set_user_prompt(&mut self, prompt: impl Into<String>) {
        self.user_prompt = prompt.into();
    }

    pub fn user_prompt(&self) -> &str {
        &self.user_prompt
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub fn cancellation_token_mut(&mut self) -> &mut CancellationToken {
        &mut self.cancellation_token
    }

    pub fn last_activity_at(&self) -> u64 {
        self.last_activity_at
    }

    pub fn audit_outcome(&self) -> Option<&DispatchAuditOutcome> {
        self.audit_outcome.as_ref()
    }

    pub fn set_audit_outcome(&mut self, outcome: DispatchAuditOutcome) {
        self.audit_outcome = Some(outcome);
    }

    pub fn drain_events(&mut self) -> Vec<DispatchBatchEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn transition_to(&mut self, next: BatchPhase) -> Result<(), String> {
        if self.phase == next {
            return Ok(());
        }
        let allowed = self.phase.allowed_transitions();
        if !allowed.contains(&next) {
            return Err(format!(
                "非法阶段转换: {:?} -> {:?} (batch {})",
                self.phase, next, self.id
            ));
        }
        self.phase = next;
        self.events.push(DispatchBatchEvent::PhaseChanged {
            batch_id: self.id.clone(),
            phase: next,
        });
        Ok(())
    }

    pub fn register(
        &mut self,
        task_id: impl Into<String>,
        worker: impl Into<String>,
        task_contract: DispatchTaskContract,
    ) -> Result<&DispatchEntry, String> {
        if self.phase == BatchPhase::Archived {
            return Err(format!("batch {} 已归档，无法注册", self.id));
        }
        let task_id = task_id.into();
        if self.entry_index.contains_key(&task_id) {
            return Err(format!("任务 {} 已存在于 batch {}", task_id, self.id));
        }
        for dep_id in &task_contract.depends_on {
            if dep_id == &task_id {
                return Err(format!("任务 {} 不能依赖自身", task_id));
            }
            if !self.entry_index.contains_key(dep_id) {
                return Err(format!(
                    "任务 {} 的依赖 {} 未在 batch 中注册",
                    task_id, dep_id
                ));
            }
        }

        let dep_state = self.evaluate_dependency_state(&task_contract.depends_on);
        let now = now_millis();
        let entry = DispatchEntry {
            task_id: task_id.clone(),
            request_id: Some(self.request_id.clone()),
            worker: worker.into(),
            task_contract,
            status: dep_state.status,
            result: None,
            created_at: now,
            started_at: None,
            completed_at: None,
        };

        let idx = self.entries.len();
        self.entries.push(entry);
        self.entry_index.insert(task_id.clone(), idx);

        if dep_state.status == DispatchStatus::Skipped {
            let reason = dep_state
                .reason
                .unwrap_or_else(|| "依赖未满足，级联跳过".to_string());
            self.update_status_internal(
                idx,
                DispatchStatus::Skipped,
                Some(DispatchResult {
                    success: false,
                    summary: reason,
                    ..Default::default()
                }),
            );
        }

        Ok(&self.entries[idx])
    }

    pub fn update_status(
        &mut self,
        task_id: &str,
        status: DispatchStatus,
        result: Option<DispatchResult>,
    ) {
        let Some(&idx) = self.entry_index.get(task_id) else {
            return;
        };
        let previous = self.entries[idx].status;
        if previous.is_terminal() {
            if DispatchStatus::can_reconcile_terminal(previous, status) {
                // 允许 cancelled → completed/failed 的终态重算
            } else {
                return;
            }
        }
        self.update_status_internal(idx, status, result);
    }

    fn update_status_internal(
        &mut self,
        idx: usize,
        status: DispatchStatus,
        result: Option<DispatchResult>,
    ) {
        let now = now_millis();
        self.last_activity_at = now;
        let entry = &mut self.entries[idx];
        entry.status = status;
        if status == DispatchStatus::Running && entry.started_at.is_none() {
            entry.started_at = Some(now);
        }
        if status.is_terminal() {
            entry.completed_at = Some(now);
            if let Some(ref r) = result {
                if let Some(ref usage) = r.token_usage {
                    self.token_consumption.input_tokens += usage.input_tokens;
                    self.token_consumption.output_tokens += usage.output_tokens;
                }
                entry.result = result.clone();
            }
        }

        let task_id = entry.task_id.clone();
        self.events.push(DispatchBatchEvent::TaskStatusChanged {
            task_id: task_id.clone(),
            status,
            result,
        });

        if status.is_terminal() {
            self.check_dependents(&task_id);
            self.check_all_completed();
        }
    }

    pub fn mark_running(&mut self, task_id: &str) {
        self.update_status(task_id, DispatchStatus::Running, None);
    }

    pub fn mark_completed(&mut self, task_id: &str, result: DispatchResult) {
        self.update_status(task_id, DispatchStatus::Completed, Some(result));
    }

    pub fn mark_failed(&mut self, task_id: &str, result: DispatchResult) {
        self.update_status(task_id, DispatchStatus::Failed, Some(result));
    }

    pub fn can_execute(&self, task_id: &str) -> bool {
        let Some(&idx) = self.entry_index.get(task_id) else {
            return false;
        };
        let entry = &self.entries[idx];
        if entry.task_contract.depends_on.is_empty() {
            return true;
        }
        entry.task_contract.depends_on.iter().all(|dep_id| {
            self.entry_index
                .get(dep_id)
                .map(|&i| self.entries[i].status == DispatchStatus::Completed)
                .unwrap_or(false)
        })
    }

    pub fn is_all_completed(&self) -> bool {
        !self.entries.is_empty() && self.entries.iter().all(|e| e.status.is_terminal())
    }

    pub fn get_entry(&self, task_id: &str) -> Option<&DispatchEntry> {
        self.entry_index.get(task_id).map(|&i| &self.entries[i])
    }

    pub fn entries(&self) -> &[DispatchEntry] {
        &self.entries
    }

    pub fn get_ready_tasks(&self) -> Vec<&DispatchEntry> {
        let mut ready: Vec<&DispatchEntry> = self
            .entries
            .iter()
            .filter(|e| {
                (e.status == DispatchStatus::Pending || e.status == DispatchStatus::WaitingDeps)
                    && self.can_execute(&e.task_id)
            })
            .collect();

        let dependent_count: HashMap<&str, usize> = ready
            .iter()
            .map(|entry| {
                let count = self
                    .entries
                    .iter()
                    .filter(|other| {
                        (other.status == DispatchStatus::Pending
                            || other.status == DispatchStatus::WaitingDeps)
                            && other.task_contract.depends_on.contains(&entry.task_id)
                    })
                    .count();
                (entry.task_id.as_str(), count)
            })
            .collect();

        ready.sort_by(|a, b| {
            let dep_a = dependent_count.get(a.task_id.as_str()).copied().unwrap_or(0);
            let dep_b = dependent_count.get(b.task_id.as_str()).copied().unwrap_or(0);
            dep_b.cmp(&dep_a).then(a.created_at.cmp(&b.created_at))
        });

        ready
    }

    pub fn get_ready_tasks_isolated(&self) -> Vec<&DispatchEntry> {
        let all_ready = self.get_ready_tasks();
        let mut running_workers = HashSet::new();
        for entry in &self.entries {
            if entry.status == DispatchStatus::Running {
                running_workers.insert(entry.worker.as_str());
            }
        }
        let mut selected_workers = HashSet::new();
        let mut result = Vec::new();
        for entry in all_ready {
            if running_workers.contains(entry.worker.as_str()) {
                continue;
            }
            if selected_workers.contains(entry.worker.as_str()) {
                continue;
            }
            selected_workers.insert(entry.worker.as_str());
            result.push(entry);
        }
        result
    }

    pub fn summary(&self) -> DispatchBatchSummary {
        let mut s = DispatchBatchSummary {
            total: self.entries.len(),
            completed: 0,
            failed: 0,
            skipped: 0,
            cancelled: 0,
            running: 0,
            pending: 0,
        };
        for e in &self.entries {
            match e.status {
                DispatchStatus::Completed => s.completed += 1,
                DispatchStatus::Failed => s.failed += 1,
                DispatchStatus::Skipped => s.skipped += 1,
                DispatchStatus::Cancelled => s.cancelled += 1,
                DispatchStatus::Running => s.running += 1,
                DispatchStatus::Pending | DispatchStatus::WaitingDeps => s.pending += 1,
            }
        }
        s
    }

    pub fn token_consumption(&self) -> TokenConsumption {
        let total = self.token_consumption.input_tokens + self.token_consumption.output_tokens;
        TokenConsumption {
            input_tokens: self.token_consumption.input_tokens,
            output_tokens: self.token_consumption.output_tokens,
            total_tokens: total,
        }
    }

    pub fn cancel_all(&mut self, reason: impl Into<String>) {
        if self.phase != BatchPhase::Active {
            return;
        }
        let reason = reason.into();
        self.cancellation_token.cancel(&reason);

        let now = now_millis();
        for entry in &mut self.entries {
            if matches!(
                entry.status,
                DispatchStatus::Pending | DispatchStatus::WaitingDeps | DispatchStatus::Running
            ) {
                entry.status = DispatchStatus::Cancelled;
                entry.completed_at = Some(now);
                entry.result = Some(DispatchResult {
                    success: false,
                    summary: format!("已取消: {reason}"),
                    ..Default::default()
                });
            }
        }

        let _ = self.transition_to(BatchPhase::Archived);
        self.events.push(DispatchBatchEvent::BatchCancelled {
            batch_id: self.id.clone(),
            reason,
        });
    }

    pub fn archive(&mut self) {
        let _ = self.transition_to(BatchPhase::Archived);
    }

    pub fn touch_activity(&mut self) {
        self.last_activity_at = now_millis();
    }

    pub fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj_list: HashMap<&str, Vec<&str>> = HashMap::new();

        for entry in &self.entries {
            in_degree.insert(&entry.task_id, entry.task_contract.depends_on.len());
            for dep_id in &entry.task_contract.depends_on {
                adj_list
                    .entry(dep_id.as_str())
                    .or_default()
                    .push(&entry.task_id);
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut sorted = Vec::new();

        while let Some(current) = queue.pop_front() {
            sorted.push(current.to_string());
            if let Some(dependents) = adj_list.get(current) {
                for &dep in dependents {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push_back(dep);
                        }
                    }
                }
            }
        }

        if sorted.len() != self.entries.len() {
            let sorted_set: HashSet<&str> = sorted.iter().map(|s| s.as_str()).collect();
            let remaining: Vec<&str> = self
                .entries
                .iter()
                .map(|e| e.task_id.as_str())
                .filter(|id| !sorted_set.contains(id))
                .collect();
            return Err(format!("检测到依赖环: {}", remaining.join(", ")));
        }

        Ok(sorted)
    }

    pub fn validate_depth_limit(&self, max_depth: usize) -> Result<(), String> {
        for entry in &self.entries {
            let depth = self.calculate_depth(&entry.task_id, &mut HashSet::new());
            if depth > max_depth {
                return Err(format!(
                    "任务 {} 依赖链深度 {} 超过上限 {}",
                    entry.task_id, depth, max_depth
                ));
            }
        }
        Ok(())
    }

    fn calculate_depth(&self, task_id: &str, visited: &mut HashSet<String>) -> usize {
        if visited.contains(task_id) {
            return 0;
        }
        visited.insert(task_id.to_string());

        let Some(&idx) = self.entry_index.get(task_id) else {
            return 0;
        };
        let entry = &self.entries[idx];
        if entry.task_contract.depends_on.is_empty() {
            return 0;
        }

        let mut max_depth = 0;
        for dep_id in &entry.task_contract.depends_on {
            max_depth = max_depth.max(self.calculate_depth(dep_id, visited) + 1);
        }
        max_depth
    }

    fn check_dependents(&mut self, completed_task_id: &str) {
        let completed_status = self
            .entry_index
            .get(completed_task_id)
            .map(|&i| self.entries[i].status);

        let mut to_skip = Vec::new();
        let mut to_ready = Vec::new();

        for (idx, entry) in self.entries.iter().enumerate() {
            if !entry
                .task_contract
                .depends_on
                .iter()
                .any(|d| d == completed_task_id)
            {
                continue;
            }
            if entry.status != DispatchStatus::WaitingDeps {
                continue;
            }
            if completed_status != Some(DispatchStatus::Completed) {
                to_skip.push((
                    idx,
                    format!("前序任务 {completed_task_id} 未成功，级联跳过"),
                ));
                continue;
            }
            if self.can_execute(&entry.task_id) {
                to_ready.push(idx);
            }
        }

        for (idx, reason) in to_skip {
            self.update_status_internal(
                idx,
                DispatchStatus::Skipped,
                Some(DispatchResult {
                    success: false,
                    summary: reason,
                    ..Default::default()
                }),
            );
        }

        for idx in to_ready {
            self.entries[idx].status = DispatchStatus::Pending;
            let task_id = self.entries[idx].task_id.clone();
            self.events.push(DispatchBatchEvent::TaskReady { task_id });
        }
    }

    fn check_all_completed(&mut self) {
        if self.is_all_completed() && self.phase == BatchPhase::Active {
            let _ = self.transition_to(BatchPhase::Summarizing);
            self.events.push(DispatchBatchEvent::AllCompleted {
                batch_id: self.id.clone(),
            });
        }
    }

    fn evaluate_dependency_state(
        &self,
        depends_on: &[String],
    ) -> DependencyState {
        if depends_on.is_empty() {
            return DependencyState {
                status: DispatchStatus::Pending,
                reason: None,
            };
        }

        let mut has_unfinished = false;
        for dep_id in depends_on {
            if let Some(&idx) = self.entry_index.get(dep_id) {
                let dep = &self.entries[idx];
                if matches!(
                    dep.status,
                    DispatchStatus::Failed | DispatchStatus::Skipped | DispatchStatus::Cancelled
                ) {
                    return DependencyState {
                        status: DispatchStatus::Skipped,
                        reason: Some(format!("依赖 {dep_id} 状态为 {:?}，级联跳过", dep.status)),
                    };
                }
                if dep.status != DispatchStatus::Completed {
                    has_unfinished = true;
                }
            }
        }

        if has_unfinished {
            DependencyState {
                status: DispatchStatus::WaitingDeps,
                reason: None,
            }
        } else {
            DependencyState {
                status: DispatchStatus::Pending,
                reason: Some("所有依赖已完成".to_string()),
            }
        }
    }
}

struct DependencyState {
    status: DispatchStatus,
    reason: Option<String>,
}
