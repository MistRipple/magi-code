//! Goal/Todo 协议：session 范围内的目标推进清单。
//!
//! - 不是独立项目管理工具，而是单 session 内的执行锚点。
//! - 模型在长任务里通过 `todo_write` 把分解 + 进度写到 ledger；下一轮 Turn 开始
//!   时 ledger 快照自动注入 system prompt，帮助模型保持连贯。
//! - 每次 `todo_write` 用入参整体替换当前列表。
//! - SessionStore 是唯一权威状态；Todo 随 session 持久化并在 daemon 重启后恢复。
//!
//! 状态机：Pending → InProgress → Completed。允许从 InProgress 退回 Pending，
//! 但不允许跨级跳过（由调用方约束，本结构本身不强校验，与 claude-code 一致）。

use magi_core::SessionId;
pub use magi_core::{TodoItem, TodoStatus};
use magi_session_store::SessionStore;
use std::sync::Arc;

fn parse_todo_status(raw: &str) -> Result<TodoStatus, TodoWriteError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        _ => Err(TodoWriteError::InvalidStatus(raw.to_string())),
    }
}

fn prompt_marker(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "[ ]",
        TodoStatus::InProgress => "[~]",
        TodoStatus::Completed => "[x]",
    }
}

// --- TodoLedger（SessionStore 的 session 作用域视图）

/// 单 session 的 todo 列表视图。真实数据只存放在 SessionStore，避免运行期 ledger 与
/// 持久化会话状态形成双轨。
#[derive(Clone, Debug)]
pub struct TodoLedger {
    session_store: Arc<SessionStore>,
    session_id: SessionId,
}

impl TodoLedger {
    pub fn new(session_store: Arc<SessionStore>, session_id: SessionId) -> Self {
        Self {
            session_store,
            session_id,
        }
    }

    /// 整体替换列表。返回写入后的副本，便于调用方回填 tool_call 结果。
    pub fn replace(&self, items: Vec<TodoItem>) -> magi_core::DomainResult<Vec<TodoItem>> {
        self.session_store
            .replace_todo_items(&self.session_id, items)
    }

    pub fn snapshot(&self) -> Vec<TodoItem> {
        self.session_store.todo_items(&self.session_id)
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot().is_empty()
    }

    pub fn pause_in_progress(&self) -> magi_core::DomainResult<bool> {
        let mut items = self.snapshot();
        let mut changed = false;
        for item in &mut items {
            if item.status == TodoStatus::InProgress {
                item.status = TodoStatus::Pending;
                changed = true;
            }
        }
        if changed {
            self.replace(items)?;
        }
        Ok(changed)
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
                prompt_marker(item.status),
                item.content,
            ));
        }
        lines.push("如需更新分解或推进状态，请调用 `todo_write` 工具整体覆盖列表。".to_string());
        Some(lines.join("\n"))
    }
}
// --- 入参解析（`todo_write` 工具的 arguments JSON）

/// `todo_write` 调用入参形如：
/// ```json
/// { "todos": [ { "content": "...", "activeForm": "...", "status": "pending" }, ... ] }
/// ```
/// 空 `todos` 数组等价于"清空 ledger"。坏 entry 直接返回错误，避免模型输出问题被静默写成假进度。
pub fn parse_todo_write_arguments(arguments_json: &str) -> Result<Vec<TodoItem>, TodoWriteError> {
    let value: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|err| TodoWriteError::InvalidJson(err.to_string()))?;
    let todos = value
        .get("todos")
        .ok_or(TodoWriteError::MissingTodosField)?;
    let array = todos.as_array().ok_or(TodoWriteError::TodosNotArray)?;
    let mut items = Vec::with_capacity(array.len());
    let mut in_progress_count = 0usize;
    for (index, entry) in array.iter().enumerate() {
        let object = entry
            .as_object()
            .ok_or(TodoWriteError::TodoEntryNotObject(index))?;
        let content = object
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or(TodoWriteError::MissingTodoField {
                index,
                field: "content",
            })?
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(TodoWriteError::BlankTodoField {
                index,
                field: "content",
            });
        }
        let active_form = object
            .get("activeForm")
            .and_then(serde_json::Value::as_str)
            .ok_or(TodoWriteError::MissingTodoField {
                index,
                field: "activeForm",
            })?
            .trim()
            .to_string();
        if active_form.is_empty() {
            return Err(TodoWriteError::BlankTodoField {
                index,
                field: "activeForm",
            });
        }
        let status = object
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or(TodoWriteError::MissingTodoField {
                index,
                field: "status",
            })
            .and_then(parse_todo_status)?;
        if status == TodoStatus::InProgress {
            in_progress_count += 1;
        }
        items.push(TodoItem {
            content,
            active_form,
            status,
        });
    }
    if in_progress_count > 1 {
        return Err(TodoWriteError::MultipleInProgressTodos);
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
    #[error("todo_write arguments.todos[{0}] 必须是对象")]
    TodoEntryNotObject(usize),
    #[error("todo_write arguments.todos[{index}] 缺少 {field} 字段")]
    MissingTodoField { index: usize, field: &'static str },
    #[error("todo_write arguments.todos[{index}].{field} 不能为空")]
    BlankTodoField { index: usize, field: &'static str },
    #[error("todo_write status 非法: {0}，只允许 pending / in_progress / completed")]
    InvalidStatus(String),
    #[error("todo_write 只允许一个 todo 处于 in_progress")]
    MultipleInProgressTodos,
}
// --- Tool entry：`todo_write` 工具执行体

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
    execute_todo_write_tool_in_scope(
        event_bus,
        ledger,
        session_id,
        workspace_id,
        Some(task_id),
        Some(mission_id),
        arguments,
    )
}

/// 主线 session turn 的 `todo_write` 入口。主线没有独立 Task，但仍属于 session
/// 的 orchestrator mission，因此事件上下文只绑定 session / workspace / mission。
pub fn execute_session_todo_write_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    ledger: &TodoLedger,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    execute_todo_write_tool_in_scope(
        event_bus,
        ledger,
        session_id,
        workspace_id,
        None,
        Some(mission_id),
        arguments,
    )
}

fn execute_todo_write_tool_in_scope(
    event_bus: &magi_event_bus::InMemoryEventBus,
    ledger: &TodoLedger,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    task_id: Option<&magi_core::TaskId>,
    mission_id: Option<&magi_core::MissionId>,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
    use magi_event_bus::{EventContext, EventEnvelope};
    match parse_todo_write_arguments(arguments) {
        Ok(items) => {
            let stored = match ledger.replace(items) {
                Ok(stored) => stored,
                Err(error) => {
                    return (
                        serde_json::json!({
                            "tool": "todo_write",
                            "status": "failed",
                            "error": format!("TodoLedger session 不可用: {error}"),
                        })
                        .to_string(),
                        ExecutionResultStatus::Failed,
                    );
                }
            };
            let snapshot_payload = serde_json::to_value(&stored).unwrap_or(serde_json::Value::Null);
            let _ = event_bus.publish(
                EventEnvelope::domain(
                    EventId::new(format!("event-todo-ledger-updated-{}", UtcMillis::now().0)),
                    "task.todo_ledger.updated",
                    serde_json::json!({
                        "task_id": task_id.map(ToString::to_string),
                        "session_id": session_id.to_string(),
                        "workspace_id": workspace_id.map(ToString::to_string),
                        "count": stored.len(),
                        "todos": snapshot_payload,
                    }),
                )
                .with_context(EventContext {
                    workspace_id: workspace_id.cloned(),
                    session_id: Some(session_id.clone()),
                    mission_id: mission_id.cloned(),
                    task_id: task_id.cloned(),
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
// --- Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ledger(session_name: &str) -> TodoLedger {
        let store = Arc::new(SessionStore::new());
        let session_id = SessionId::new(session_name);
        store
            .create_session(session_id.clone(), session_name)
            .expect("test session should create");
        TodoLedger::new(store, session_id)
    }

    #[test]
    fn replace_round_trip() {
        let ledger = test_ledger("replace-round-trip");
        let items = vec![
            TodoItem::new("写测试", "正在写测试", TodoStatus::Pending),
            TodoItem::new("跑测试", "正在跑测试", TodoStatus::InProgress),
        ];
        let stored = ledger
            .replace(items.clone())
            .expect("replace should succeed");
        assert_eq!(stored, items);
        assert_eq!(ledger.snapshot(), items);
    }

    #[test]
    fn pause_in_progress_returns_active_item_to_pending() {
        let ledger = test_ledger("pause-in-progress");
        ledger
            .replace(vec![
                TodoItem::new("已完成", "已完成", TodoStatus::Completed),
                TodoItem::new("当前步骤", "正在处理当前步骤", TodoStatus::InProgress),
                TodoItem::new("后续步骤", "正在处理后续步骤", TodoStatus::Pending),
            ])
            .expect("todo should write");

        assert!(ledger.pause_in_progress().expect("todo should pause"));

        let items = ledger.snapshot();
        assert_eq!(items[0].status, TodoStatus::Completed);
        assert_eq!(items[1].status, TodoStatus::Pending);
        assert_eq!(items[1].active_form, "正在处理当前步骤");
        assert_eq!(items[2].status, TodoStatus::Pending);
        assert!(
            !ledger
                .pause_in_progress()
                .expect("second pause should no-op")
        );
    }

    #[test]
    fn render_for_prompt_skips_empty_ledger() {
        let ledger = test_ledger("render-empty");
        assert!(ledger.render_for_prompt().is_none());
    }

    #[test]
    fn render_for_prompt_marks_statuses() {
        let ledger = test_ledger("render-statuses");
        ledger
            .replace(vec![
                TodoItem::new("A", "正在 A", TodoStatus::Pending),
                TodoItem::new("B", "正在 B", TodoStatus::InProgress),
                TodoItem::new("C", "正在 C", TodoStatus::Completed),
            ])
            .expect("replace should succeed");
        let prompt = ledger.render_for_prompt().unwrap();
        assert!(prompt.contains("仅作为本轮执行参考"));
        assert!(prompt.contains("不能覆盖本轮用户输入"));
        assert!(prompt.contains("在不冲突时优先推进"));
        assert!(prompt.contains("[ ] A"));
        assert!(prompt.contains("[~] B"));
        assert!(prompt.contains("[x] C"));
    }

    #[test]
    fn session_store_isolates_todo_lists() {
        let store = Arc::new(SessionStore::new());
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        store
            .create_session(session_a.clone(), "session a")
            .expect("session a should create");
        store
            .create_session(session_b.clone(), "session b")
            .expect("session b should create");
        let ledger_a = TodoLedger::new(Arc::clone(&store), session_a.clone());
        let ledger_b = TodoLedger::new(Arc::clone(&store), session_b);
        ledger_a
            .replace(vec![TodoItem::new("A", "A", TodoStatus::Pending)])
            .expect("session a todo should write");
        ledger_b
            .replace(vec![TodoItem::new("B", "B", TodoStatus::Pending)])
            .expect("session b todo should write");
        assert_eq!(ledger_a.snapshot()[0].content, "A");
        assert_eq!(ledger_b.snapshot()[0].content, "B");
        assert_eq!(store.todo_items(&session_a)[0].content, "A");
    }

    #[test]
    fn deleting_session_clears_todo_state() {
        let store = Arc::new(SessionStore::new());
        let session = SessionId::new("session-drop");
        store
            .create_session(session.clone(), "session drop")
            .expect("session should create");
        let ledger = TodoLedger::new(Arc::clone(&store), session.clone());
        ledger
            .replace(vec![TodoItem::new("X", "X", TodoStatus::Pending)])
            .expect("todo should write");
        store
            .delete_session(&session)
            .expect("session should delete");
        assert!(store.todo_items(&session).is_empty());
    }

    #[test]
    fn parse_arguments_accepts_schema_shape() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "step 1", "activeForm": "doing step 1", "status": "pending" },
                { "content": "step 2", "activeForm": "doing step 2", "status": "completed" },
            ]
        })
        .to_string();
        let items = parse_todo_write_arguments(&raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, TodoStatus::Pending);
        assert_eq!(items[1].status, TodoStatus::Completed);
        assert_eq!(items[1].active_form, "doing step 2");
    }

    #[test]
    fn parse_arguments_rejects_blank_content() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "  ", "status": "pending" },
            ]
        })
        .to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(
            err,
            TodoWriteError::BlankTodoField {
                index: 0,
                field: "content"
            }
        );
    }

    #[test]
    fn parse_arguments_rejects_invalid_status() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "ok", "activeForm": "doing ok", "status": "garbage" },
            ]
        })
        .to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(err, TodoWriteError::InvalidStatus("garbage".to_string()));
    }

    #[test]
    fn parse_arguments_rejects_non_object_entry() {
        let raw = serde_json::json!({ "todos": ["not-an-object"] }).to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(err, TodoWriteError::TodoEntryNotObject(0));
    }

    #[test]
    fn parse_arguments_rejects_missing_active_form() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "ok", "status": "pending" },
            ]
        })
        .to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(
            err,
            TodoWriteError::MissingTodoField {
                index: 0,
                field: "activeForm"
            }
        );
    }

    #[test]
    fn parse_arguments_rejects_multiple_in_progress_items() {
        let raw = serde_json::json!({
            "todos": [
                { "content": "step 1", "activeForm": "doing step 1", "status": "in_progress" },
                { "content": "step 2", "activeForm": "doing step 2", "status": "in_progress" },
            ]
        })
        .to_string();
        let err = parse_todo_write_arguments(&raw).unwrap_err();
        assert_eq!(err, TodoWriteError::MultipleInProgressTodos);
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
        let ledger = test_ledger("empty-clears");
        ledger
            .replace(vec![TodoItem::new("X", "X", TodoStatus::Pending)])
            .expect("todo should write");
        let raw = serde_json::json!({ "todos": [] }).to_string();
        let items = parse_todo_write_arguments(&raw).unwrap();
        ledger.replace(items).expect("todo should clear");
        assert!(ledger.is_empty());
    }
}
