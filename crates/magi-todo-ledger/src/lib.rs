//! 任务系统 — L13 TodoLedger：session 范围内的 todo 列表。
//!
//! 参考 claude-code 的 TodoWrite：
//! - 不是项目管理工具，而是单 session 内的"思维锚点"。
//! - 模型在长任务里通过 `todo_write` 把分解 + 进度写到 ledger；下一轮 Turn 开始
//!   时 ledger 快照自动注入 system prompt，帮助模型保持连贯。
//! - 每次 `todo_write` 用入参整体替换当前列表（claude-code 语义）。
//! - 不持久化：session 结束即丢弃。Mission / Project 维度的持久任务有
//!   独立的 Plan 文档（S12）。
//!
//! 状态机：Pending → InProgress → Completed。允许从 InProgress 退回 Pending，
//! 但不允许跨级跳过（由调用方约束，本结构本身不强校验，与 claude-code 一致）。

use magi_core::SessionId;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

// ---------------------------------------------------------------------------
// TodoStatus / TodoItem
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "in_progress" | "in-progress" | "inprogress" => Self::InProgress,
            "completed" | "complete" | "done" => Self::Completed,
            _ => Self::Pending,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    fn prompt_marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[~]",
            Self::Completed => "[x]",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    /// 进行中状态在 UI 上展示的现在分词（参考 claude-code TodoWrite 协议）。
    pub active_form: String,
    pub status: TodoStatus,
}

impl TodoItem {
    pub fn new(
        content: impl Into<String>,
        active_form: impl Into<String>,
        status: TodoStatus,
    ) -> Self {
        Self {
            content: content.into(),
            active_form: active_form.into(),
            status,
        }
    }
}

// ---------------------------------------------------------------------------
// TodoLedger（per-session 实例）
// ---------------------------------------------------------------------------

/// 单 session 的 todo 列表。`todo_write` 用入参整体替换；外部读者通过 `snapshot`
/// 拿到一份只读复制。线程安全：内部 `RwLock`，并发场景下读多写少。
#[derive(Debug, Default)]
pub struct TodoLedger {
    items: RwLock<Vec<TodoItem>>,
}

impl TodoLedger {
    pub fn new() -> Self {
        Self {
            items: RwLock::new(Vec::new()),
        }
    }

    /// 整体替换列表。返回写入后的副本，便于调用方回填 tool_call 结果。
    pub fn replace(&self, items: Vec<TodoItem>) -> Vec<TodoItem> {
        let mut guard = self
            .items
            .write()
            .expect("TodoLedger.items RwLock poisoned (write)");
        *guard = items;
        guard.clone()
    }

    pub fn snapshot(&self) -> Vec<TodoItem> {
        self.items
            .read()
            .expect("TodoLedger.items RwLock poisoned (read)")
            .clone()
    }

    pub fn is_empty(&self) -> bool {
        self.items
            .read()
            .expect("TodoLedger.items RwLock poisoned (read)")
            .is_empty()
    }

    /// 渲染成注入下一轮 Turn 用的 system prompt 片段。空列表返回 `None`，
    /// 避免给模型添加无意义的"todo 为空"提示。
    pub fn render_for_prompt(&self) -> Option<String> {
        let snapshot = self.snapshot();
        if snapshot.is_empty() {
            return None;
        }
        let mut lines = Vec::with_capacity(snapshot.len() + 2);
        lines.push(
            "当前 TodoLedger（本 session 内的任务分解，仅作为本轮执行参考；不能覆盖本轮用户输入、当前会话事实或当前 task 目标；在不冲突时优先推进 in_progress 与 pending 项）："
                .to_string(),
        );
        for (idx, item) in snapshot.iter().enumerate() {
            lines.push(format!(
                "{}. {} {}",
                idx + 1,
                item.status.prompt_marker(),
                item.content,
            ));
        }
        lines.push("如需更新分解或推进状态，请调用 `todo_write` 工具整体覆盖列表。".to_string());
        Some(lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// TodoLedgerRegistry（session → ledger）
// ---------------------------------------------------------------------------

/// 进程内 session→ledger 索引。`LlmTaskDispatcher` 注入一份 `Arc<TodoLedgerRegistry>`，
/// 每个 conversation_loop 在自己 session 下取 ledger。
#[derive(Debug, Default)]
pub struct TodoLedgerRegistry {
    map: RwLock<HashMap<SessionId, Arc<TodoLedger>>>,
}

impl TodoLedgerRegistry {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    /// 拿到 session 对应的 ledger；不存在则按需创建。
    pub fn get_or_create(&self, session_id: &SessionId) -> Arc<TodoLedger> {
        if let Some(existing) = self
            .map
            .read()
            .expect("TodoLedgerRegistry.map RwLock poisoned (read)")
            .get(session_id)
            .cloned()
        {
            return existing;
        }
        let mut guard = self
            .map
            .write()
            .expect("TodoLedgerRegistry.map RwLock poisoned (write)");
        guard
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(TodoLedger::new()))
            .clone()
    }

    /// 只读访问：当前 session 没有 ledger 时返回 `None`，避免无端创建。
    pub fn get(&self, session_id: &SessionId) -> Option<Arc<TodoLedger>> {
        self.map
            .read()
            .expect("TodoLedgerRegistry.map RwLock poisoned (read)")
            .get(session_id)
            .cloned()
    }

    /// session 结束时清除 ledger。
    pub fn drop_session(&self, session_id: &SessionId) {
        let mut guard = self
            .map
            .write()
            .expect("TodoLedgerRegistry.map RwLock poisoned (write)");
        guard.remove(session_id);
    }
}

// ---------------------------------------------------------------------------
// 入参解析（`todo_write` 工具的 arguments JSON）
// ---------------------------------------------------------------------------

/// `todo_write` 调用入参形如：
/// ```json
/// { "todos": [ { "content": "...", "activeForm": "...", "status": "pending" }, ... ] }
/// ```
/// 容错：忽略缺字段或非法 status；空 `todos` 数组等价于"清空 ledger"。
pub fn parse_todo_write_arguments(arguments_json: &str) -> Result<Vec<TodoItem>, TodoWriteError> {
    let value: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|err| TodoWriteError::InvalidJson(err.to_string()))?;
    let todos = value
        .get("todos")
        .ok_or(TodoWriteError::MissingTodosField)?;
    let array = todos.as_array().ok_or(TodoWriteError::TodosNotArray)?;
    let mut items = Vec::with_capacity(array.len());
    for entry in array {
        let object = match entry.as_object() {
            Some(obj) => obj,
            None => continue,
        };
        let content = object
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            continue;
        }
        // 兼容 camelCase 与 snake_case 两种写法。
        let active_form = object
            .get("activeForm")
            .or_else(|| object.get("active_form"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let status = object
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(TodoStatus::parse)
            .unwrap_or(TodoStatus::Pending);
        items.push(TodoItem {
            content,
            active_form,
            status,
        });
    }
    Ok(items)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TodoWriteError {
    #[error("todo_write arguments 不是合法 JSON: {0}")]
    InvalidJson(String),
    #[error("todo_write arguments 缺少 todos 字段")]
    MissingTodosField,
    #[error("todo_write arguments.todos 必须是数组")]
    TodosNotArray,
}

// ---------------------------------------------------------------------------
// Tool entry：`todo_write` 工具执行体
// ---------------------------------------------------------------------------

/// S9 工具下沉：把 `todo_write` 的完整执行体收口在本 crate，conversation_loop
/// 不再持有这段业务，由 crate 自身承担实现。
///
/// 入参与 `&magi_core::Task` 解耦：只透传 `task_id` / `mission_id`，避免
/// TodoLedger 反向依赖完整 Task 结构。
pub fn execute_todo_write_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    ledger: &TodoLedger,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    task_id: &magi_core::TaskId,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
    use magi_event_bus::{EventContext, EventEnvelope};
    match parse_todo_write_arguments(arguments) {
        Ok(items) => {
            let stored = ledger.replace(items);
            let snapshot_payload = serde_json::to_value(&stored).unwrap_or(serde_json::Value::Null);
            let _ = event_bus.publish(
                EventEnvelope::domain(
                    EventId::new(format!("event-todo-ledger-updated-{}", UtcMillis::now().0)),
                    "task.todo_ledger.updated",
                    serde_json::json!({
                        "task_id": task_id.to_string(),
                        "session_id": session_id.to_string(),
                        "workspace_id": workspace_id.map(ToString::to_string),
                        "count": stored.len(),
                        "todos": snapshot_payload,
                    }),
                )
                .with_context(EventContext {
                    workspace_id: workspace_id.cloned(),
                    session_id: Some(session_id.clone()),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(task_id.clone()),
                    ..EventContext::default()
                }),
            );
            (
                serde_json::json!({
                    "tool": "todo_write",
                    "status": "succeeded",
                    "count": stored.len(),
                    "todos": stored,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        Err(err) => (
            serde_json::json!({
                "tool": "todo_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_round_trip() {
        let ledger = TodoLedger::new();
        let items = vec![
            TodoItem::new("写测试", "正在写测试", TodoStatus::Pending),
            TodoItem::new("跑测试", "正在跑测试", TodoStatus::InProgress),
        ];
        let stored = ledger.replace(items.clone());
        assert_eq!(stored, items);
        assert_eq!(ledger.snapshot(), items);
    }

    #[test]
    fn render_for_prompt_skips_empty_ledger() {
        let ledger = TodoLedger::new();
        assert!(ledger.render_for_prompt().is_none());
    }

    #[test]
    fn render_for_prompt_marks_statuses() {
        let ledger = TodoLedger::new();
        ledger.replace(vec![
            TodoItem::new("A", "正在 A", TodoStatus::Pending),
            TodoItem::new("B", "正在 B", TodoStatus::InProgress),
            TodoItem::new("C", "正在 C", TodoStatus::Completed),
        ]);
        let prompt = ledger.render_for_prompt().unwrap();
        assert!(prompt.contains("仅作为本轮执行参考"));
        assert!(prompt.contains("不能覆盖本轮用户输入"));
        assert!(prompt.contains("在不冲突时优先推进"));
        assert!(prompt.contains("[ ] A"));
        assert!(prompt.contains("[~] B"));
        assert!(prompt.contains("[x] C"));
    }

    #[test]
    fn registry_isolates_sessions() {
        let registry = TodoLedgerRegistry::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        let ledger_a = registry.get_or_create(&session_a);
        let ledger_b = registry.get_or_create(&session_b);
        ledger_a.replace(vec![TodoItem::new("A", "A", TodoStatus::Pending)]);
        ledger_b.replace(vec![TodoItem::new("B", "B", TodoStatus::Pending)]);
        assert_eq!(ledger_a.snapshot()[0].content, "A");
        assert_eq!(ledger_b.snapshot()[0].content, "B");
        assert!(Arc::ptr_eq(&ledger_a, &registry.get_or_create(&session_a)));
    }

    #[test]
    fn registry_drop_session_clears_state() {
        let registry = TodoLedgerRegistry::new();
        let session = SessionId::new("session-drop");
        let ledger = registry.get_or_create(&session);
        ledger.replace(vec![TodoItem::new("X", "X", TodoStatus::Pending)]);
        registry.drop_session(&session);
        assert!(registry.get(&session).is_none());
        let fresh = registry.get_or_create(&session);
        assert!(fresh.is_empty());
    }

    #[test]
    fn parse_arguments_handles_camel_case() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "step 1", "activeForm": "doing step 1", "status": "pending" },
                { "content": "step 2", "activeForm": "doing step 2", "status": "in_progress" },
            ]
        })
        .to_string();
        let items = parse_todo_write_arguments(&raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, TodoStatus::Pending);
        assert_eq!(items[1].status, TodoStatus::InProgress);
        assert_eq!(items[1].active_form, "doing step 2");
    }

    #[test]
    fn parse_arguments_skips_blank_and_invalid_status() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "  ", "status": "pending" },
                { "content": "ok", "status": "garbage" },
                "not-an-object",
            ]
        })
        .to_string();
        let items = parse_todo_write_arguments(&raw).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].content, "ok");
        assert_eq!(items[0].status, TodoStatus::Pending); // 非法 status → fallback
    }

    #[test]
    fn parse_arguments_rejects_non_object_todos_field() {
        let raw = serde_json::json!({ "todos": "nope" }).to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(err, TodoWriteError::TodosNotArray);
    }

    #[test]
    fn parse_arguments_rejects_missing_field() {
        let raw = serde_json::json!({}).to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(err, TodoWriteError::MissingTodosField);
    }

    #[test]
    fn empty_todos_clears_ledger() {
        let ledger = TodoLedger::new();
        ledger.replace(vec![TodoItem::new("X", "X", TodoStatus::Pending)]);
        let raw = serde_json::json!({ "todos": [] }).to_string();
        let items = parse_todo_write_arguments(&raw).unwrap();
        ledger.replace(items);
        assert!(ledger.is_empty());
    }
}
