//! `magi-web-harness`：T3 的 MCP 桥接入口（设计基线 §5.7.3）。
//!
//! 组件边界：
//! - **同一套桥接工具、turn 令牌与去重账本支持两种传输**：Streamable HTTP
//!   （`harness::http`，只监听 `127.0.0.1` 随机高端口）与 stdio
//!   （`harness::stdio`，由 `openai/tunnel-client` 拉起）；
//! - stdio 进程只是**轻量中继**：不自己持有令牌与挂起调用，经仅当前 OS 用户可
//!   访问的本地 socket 连到 daemon 内的会话表（本模块）；
//! - 令牌校验、挂起调用与去重账本都在 daemon 侧完成；两条路径都是独立入口，
//!   不挂主 app、不共享鉴权中间件、不暴露任何 `/api/*` 路由。
//!
//! 关键不变量（逐条对应设计基线）：
//! 1. 只有 Magi 发出的那个回复能调用工具（turn 令牌 + 归属绑定）；
//! 2. 工具结果由 Magi 的 conversation loop 执行，harness 只转换与挂起；
//! 3. 同一回复续接是唯一交付形态，超时走「同一对话下一轮回填」；
//! 4. 去重只针对同一次调用的传输层重试，禁止按内容哈希去重。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Notify, watch};

use crate::protocol::{ToolResultStatus, harness_tool_call_id, sha256_hex};

pub mod http;
pub mod mcp;
pub mod stdio;
pub mod tunnel;

pub use http::{HarnessHttpServer, serve_http};
pub use mcp::{BRIDGE_TOOL_CALL, BRIDGE_TOOL_INVENTORY, JsonRpcRequest, MCP_PROTOCOL_VERSION};
pub use stdio::{StdioRelay, serve_local_socket};
pub use tunnel::{TUNNEL_CLIENT_VERSION, TunnelClientStatus, TunnelManager, TunnelRuntimeConfig};

/// harness 的桥接 schema 版本。桥接工具形状变化即发布新的连接器名。
pub const HARNESS_REVISION: &str = "1";

/// 同一条回复内允许同时挂起的工具调用上限（防止单条回复打爆 daemon）。
pub const MAX_PENDING_CALLS: usize = 32;

/// 默认挂起时限：必须**早于** ChatGPT 侧与通道侧的超时（设计基线 §5.7.3）。
pub const DEFAULT_SUSPENSION_TIMEOUT: Duration = Duration::from_secs(90);

/// 一个可调用工具在本次回复内的形状（来自 `request.tools`）。
#[derive(Clone, Debug, PartialEq)]
pub struct HarnessToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// 开启一次 harness 会话的输入。
///
/// `turn_id` 就是本轮重锚块里的 `turn_nonce`；`token` 是写进发送上下文、
/// 由模型原样回带的 turn 令牌，**只驻内存**，不落盘、不写日志 / UI / 诊断。
#[derive(Clone)]
pub struct HarnessTurnSpec {
    pub session_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub epoch: u64,
    pub turn_id: String,
    pub token: String,
    pub tools: Vec<HarnessToolSchema>,
    pub suspension_timeout: Duration,
}

impl std::fmt::Debug for HarnessTurnSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarnessTurnSpec")
            .field("session_id", &self.session_id)
            .field("thread_id", &self.thread_id)
            .field("engine_id", &self.engine_id)
            .field("epoch", &self.epoch)
            .field("turn_id", &self.turn_id)
            .field("token", &"<redacted>")
            .field("tools", &self.tools.len())
            .field("suspension_timeout", &self.suspension_timeout)
            .finish()
    }
}

/// 一个已到达、正在等待 Magi 结果的工具调用。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolCall {
    pub tool_call_id: String,
    pub name: String,
    pub arguments: Value,
}

/// harness 交回给推理通道的一个批次（顺序即批次顺序）。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolBatch {
    pub turn_id: String,
    pub calls: Vec<PendingToolCall>,
}

/// 工具执行结果（由 Magi 的 loop 交回）。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallOutcome {
    pub status: ToolResultStatus,
    pub body: String,
}

/// harness 的显式错误分类（daemon 内部类型，不进入对外 schema）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessError {
    /// 令牌不存在 / 不属于本会话。
    TokenInvalid,
    /// 令牌已被撤销（超时、turn 终态、失效）。
    TokenRevoked,
    /// 工具名不在本次允许的清单里。
    UnknownTool,
    /// 结果数量或顺序与批次不一致（§5.7.3 第 3 条）。
    BatchMismatch,
    /// 批次里的某个调用找不到（已被超时撤销）。
    CallNotFound,
    /// 挂起调用数超过上限。
    TooManyCalls,
    /// 同一 JSON-RPC id 被复用于不同的工具请求。
    RequestMismatch,
}

impl HarnessError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TokenInvalid => "invalid_token",
            Self::TokenRevoked => "token_revoked",
            Self::UnknownTool => "unknown_tool",
            Self::BatchMismatch => "batch_mismatch",
            Self::CallNotFound => "call_not_found",
            Self::TooManyCalls => "too_many_calls",
            Self::RequestMismatch => "request_mismatch",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::TokenInvalid => "turn 令牌无效",
            Self::TokenRevoked => "turn 令牌已撤销",
            Self::UnknownTool => "工具名不在本次允许的清单中",
            Self::BatchMismatch => "工具结果数量或顺序与批次不一致",
            Self::CallNotFound => "该工具调用已不在挂起批次中",
            Self::TooManyCalls => "同一回复内挂起的工具调用过多",
            Self::RequestMismatch => "同一 JSON-RPC id 不能代表不同的工具调用",
        }
    }
}

/// 一个工具调用的挂起槽。
struct AwaitingCall {
    tool_call_id: String,
    name: String,
    arguments: Value,
    /// The latest outcome is a level-triggered value rather than a bounded
    /// event stream.  MCP transports are allowed to retry the same JSON-RPC
    /// request while the first request is still suspended; a bounded
    /// `broadcast` channel could make the fifth (or later) retry observe
    /// `Lagged` even though the original call was delivered successfully.
    /// `watch` keeps the exact same result available to every retry without
    /// making the deduplication guarantee depend on an arbitrary retry count.
    sink: watch::Sender<Option<ToolCallOutcome>>,
    delivered: bool,
}

/// JSON-RPC id 是传输层重试的身份。若同一 id 携带不同请求体，必须拒绝，
/// 不能把新的副作用调用误认成旧调用的重试。
#[derive(Clone, Debug, PartialEq)]
struct CallRequestIdentity {
    index: usize,
    name: String,
    arguments: Value,
}

#[derive(Default)]
struct SessionState {
    revoked: bool,
    /// JSON-RPC id → 到达序号。重试复用同一序号 ⇒ 同一 `tool_call_id`。
    id_index: HashMap<String, CallRequestIdentity>,
    next_index: usize,
    awaiting: Vec<AwaitingCall>,
    /// 已经交付给推理通道、等待结果回带的当前批次。
    batch: Option<Vec<String>>,
    /// 已完成的结果，按 `tool_call_id` 去重（只对同一次调用）。
    completed: HashMap<String, ToolCallOutcome>,
}

/// 一次 harness 会话（一条临时对话里的一轮）。
///
/// `Debug` 手写：**绝不打印令牌**（令牌不写日志 / UI / 诊断，§5.7.3）。
pub struct HarnessSession {
    session_id: String,
    thread_id: String,
    engine_id: String,
    epoch: u64,
    turn_id: String,
    token: String,
    token_ref: String,
    tools: Vec<HarnessToolSchema>,
    suspension_timeout: Duration,
    state: Mutex<SessionState>,
    changed: Notify,
}

impl std::fmt::Debug for HarnessSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarnessSession")
            .field("session_id", &self.session_id)
            .field("thread_id", &self.thread_id)
            .field("engine_id", &self.engine_id)
            .field("epoch", &self.epoch)
            .field("turn_id", &self.turn_id)
            .field("token_ref", &self.token_ref)
            .field("suspension_timeout", &self.suspension_timeout)
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

impl HarnessSession {
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// 令牌的不可逆引用：进入 `provider_context.turnTokenRef` 的就是它。
    pub fn token_ref(&self) -> &str {
        &self.token_ref
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn engine_id(&self) -> &str {
        &self.engine_id
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Check the binding carried by a T3 continuation before handing results
    /// back to the suspended MCP request.
    ///
    /// The token reference is deliberately the only value persisted in
    /// `provider_context`, so a continuation must also prove that the live
    /// in-memory session still belongs to the same Magi identity and epoch.
    /// Keeping this check here avoids making the client reach into the
    /// session's private token-bearing fields.
    pub(crate) fn matches_binding(
        &self,
        session_id: &str,
        thread_id: &str,
        engine_id: &str,
        epoch: u64,
    ) -> bool {
        self.session_id == session_id
            && self.thread_id == thread_id
            && self.engine_id == engine_id
            && self.epoch == epoch
    }

    pub fn tools(&self) -> &[HarnessToolSchema] {
        &self.tools
    }

    pub fn suspension_timeout(&self) -> Duration {
        self.suspension_timeout
    }

    pub fn is_revoked(&self) -> bool {
        self.lock().revoked
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state.lock().expect("harness state lock")
    }

    /// 恒定时比较校验令牌：不接受长度不等或任一字节不同。
    fn token_matches(&self, candidate: &str) -> bool {
        let expected = self.token.as_bytes();
        // The model must return the exact token from the anchor.  Trimming
        // would silently accept a token with extra bytes and would make the
        // protocol's "原样回带" guarantee false.
        let candidate = candidate.as_bytes();
        if expected.len() != candidate.len() {
            return false;
        }
        let mut diff = 0u8;
        for (left, right) in expected.iter().zip(candidate.iter()) {
            diff |= left ^ right;
        }
        diff == 0
    }

    /// 当前挂起批次的调用（诊断与投影用，只读）。
    pub fn pending_calls(&self) -> Vec<PendingToolCall> {
        self.lock()
            .awaiting
            .iter()
            .map(|call| PendingToolCall {
                tool_call_id: call.tool_call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect()
    }

    /// 等待一个工具批次。返回 `None` 表示会话已撤销（超时 / turn 终态 / 失效）。
    ///
    /// `debounce` 用于把**同一时刻**到达的多个调用收成一个批次（§5.7.3 第 3 条）：
    /// 首个调用到达后等待一个很短的收集窗口，再快照当前挂起集合。
    pub async fn next_batch(&self, debounce: Duration) -> Option<PendingToolBatch> {
        'outer: loop {
            loop {
                // Register the waiter before inspecting the state.  Otherwise
                // a tool call can be inserted between the empty check and
                // `notified().await`; `notify_waiters` would then have no
                // waiter to wake and the T3 reply would hang until the outer
                // timeout.
                let notified = self.changed.notified();
                {
                    let state = self.lock();
                    if state.revoked {
                        return None;
                    }
                    if !state.awaiting.is_empty() && state.batch.is_none() {
                        break;
                    }
                }
                notified.await;
            }
            if !debounce.is_zero() {
                tokio::time::sleep(debounce).await;
            }
            let mut state = self.lock();
            if state.revoked {
                return None;
            }
            // Only one conversation-loop consumer may own a batch.  A
            // duplicate consumer (for example after a transport retry) waits
            // for delivery instead of re-emitting the same calls and
            // overwriting `state.batch`.
            if state.batch.is_some() {
                drop(state);
                continue 'outer;
            }
            let calls = state
                .awaiting
                .iter()
                .map(|call| PendingToolCall {
                    tool_call_id: call.tool_call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect::<Vec<_>>();
            if calls.is_empty() {
                continue 'outer;
            }
            state.batch = Some(calls.iter().map(|call| call.tool_call_id.clone()).collect());
            return Some(PendingToolBatch {
                turn_id: self.turn_id.clone(),
                calls,
            });
        }
    }

    /// 把工具结果交回挂起中的调用；顺序与数量必须与批次一致。
    pub fn deliver_results(
        &self,
        results: &[(String, ToolCallOutcome)],
    ) -> Result<(), HarnessError> {
        let mut state = self.lock();
        if state.revoked {
            return Err(HarnessError::TokenRevoked);
        }
        let Some(batch) = state.batch.clone() else {
            // The MCP transport and the conversation loop can race: the
            // connector may already have received a result while the loop is
            // retrying the continuation request.  Result delivery is a
            // transport operation, so the exact same `(call_id, outcome)` is
            // idempotent; a conflicting replay must never overwrite it.
            if results.is_empty() {
                return Err(HarnessError::BatchMismatch);
            }
            for (call_id, outcome) in results {
                match state.completed.get(call_id) {
                    Some(existing) if existing == outcome => {}
                    Some(_) => return Err(HarnessError::BatchMismatch),
                    None => return Err(HarnessError::CallNotFound),
                }
            }
            return Ok(());
        };
        if batch.len() != results.len()
            || batch
                .iter()
                .zip(results.iter())
                .any(|(expected, (call_id, _))| expected != call_id)
        {
            return Err(HarnessError::BatchMismatch);
        }
        let indices = results
            .iter()
            .map(|(call_id, outcome)| {
                if let Some(existing) = state.completed.get(call_id) {
                    return if existing == outcome {
                        Ok(None)
                    } else {
                        Err(HarnessError::BatchMismatch)
                    };
                }
                state
                    .awaiting
                    .iter()
                    .position(|call| &call.tool_call_id == call_id)
                    .map(Some)
                    .ok_or(HarnessError::CallNotFound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (index, (_, outcome)) in indices.iter().zip(results.iter()) {
            let Some(index) = index else {
                continue;
            };
            let call = &mut state.awaiting[*index];
            call.delivered = true;
            let _ = call.sink.send(Some(outcome.clone()));
        }
        for (call_id, outcome) in results {
            state.completed.insert(call_id.clone(), outcome.clone());
        }
        state.awaiting.retain(|call| !call.delivered);
        state.batch = None;
        self.changed.notify_waiters();
        Ok(())
    }

    /// 撤销令牌：挂起调用立即失败，后续任何调用都被拒绝。
    pub fn revoke(&self) {
        self.revoke_with_message("token_revoked");
    }

    /// The timeout is a normal T3 boundary, not a generic token revocation:
    /// the conversation loop must be able to recognize it and send the tool
    /// result in the next same-dialogue turn.  Wake duplicate transport
    /// waiters with the same stable marker, but do not put that marker in the
    /// completed-result map (the real tool result may still arrive later).
    fn revoke_for_timeout(&self) {
        self.revoke_with_message("magi_tool_timeout");
    }

    fn revoke_with_message(&self, message: &'static str) {
        let mut state = self.lock();
        if state.revoked {
            return;
        }
        let outcome = ToolCallOutcome {
            status: ToolResultStatus::Error,
            body: message.to_string(),
        };
        for call in &state.awaiting {
            let _ = call.sink.send(Some(outcome.clone()));
        }
        state.revoked = true;
        state.awaiting.clear();
        state.batch = None;
        drop(state);
        self.changed.notify_waiters();
    }

    /// 处理一条 MCP JSON-RPC 请求；通知返回 `None`。
    pub async fn handle(self: &Arc<Self>, request: JsonRpcRequest) -> Option<Value> {
        if !request.is_well_formed() {
            return Some(mcp::error(
                request.response_id(),
                mcp::JSONRPC_INVALID_REQUEST,
                "请求不是合法的 JSON-RPC 2.0",
            ));
        }
        let id = request.response_id();
        match request.method.as_str() {
            "initialize" => Some(mcp::success(id, mcp::initialize_result())),
            "notifications/initialized" | "notifications/cancelled" => None,
            "ping" => Some(mcp::success(id, mcp::ping_result())),
            "tools/list" => Some(mcp::success(id, mcp::tools_list_result())),
            "tools/call" => {
                if request.is_notification() {
                    return None;
                }
                let params = request.params.clone().unwrap_or(Value::Null);
                Some(mcp::success(id.clone(), self.call_tool(params, &id).await))
            }
            other => Some(mcp::error(
                id,
                mcp::JSONRPC_METHOD_NOT_FOUND,
                format!("不支持的方法：{other}"),
            )),
        }
    }

    /// `tools/call`：两个桥接工具的唯一入口。
    async fn call_tool(&self, params: Value, rpc_id: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `arguments` 允许被 MCP 客户端包成字符串（部分实现这么做）。
        let arguments = match params.get("arguments") {
            Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
            Some(value) => value.clone(),
            None => Value::Null,
        };
        let token = arguments
            .get("turn_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !self.token_matches(&token) {
            return mcp::tool_failure(format!(
                "{}：{}",
                HarnessError::TokenInvalid.code(),
                HarnessError::TokenInvalid.message()
            ));
        }
        if self.is_revoked() {
            return mcp::tool_failure(format!(
                "{}：{}",
                HarnessError::TokenRevoked.code(),
                HarnessError::TokenRevoked.message()
            ));
        }
        match name.as_str() {
            mcp::BRIDGE_TOOL_INVENTORY => mcp::tool_success(self.render_inventory(&arguments)),
            mcp::BRIDGE_TOOL_CALL => {
                let rpc_key = if rpc_id.is_null() {
                    String::new()
                } else {
                    rpc_id.to_string()
                };
                self.bridge_tool_call(&arguments, &rpc_key).await
            }
            other => mcp::tool_failure(format!("未知桥接工具：{other}")),
        }
    }

    fn render_inventory(&self, arguments: &Value) -> String {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 200) as usize;
        let mut text = String::new();
        let filtered = self.tools.iter().filter(|tool| {
            query.is_empty()
                || tool.name.to_ascii_lowercase().contains(&query)
                || tool.description.to_ascii_lowercase().contains(&query)
        });
        let mut shown = 0usize;
        for tool in filtered.skip(offset).take(limit) {
            shown += 1;
            text.push_str(&format!(
                "- {}\n  说明：{}\n  参数 schema：{}\n",
                tool.name,
                tool.description,
                serde_json::to_string(&tool.parameters).unwrap_or_else(|_| "{}".to_string())
            ));
        }
        if shown == 0 {
            text.push_str("（没有匹配的工具）\n");
        }
        text.push_str(
            "\n调用方式：用 magi_tool_call，参数 name 取上面的工具名，arguments 传参数对象。",
        );
        text
    }

    /// `magi_tool_call`：转成 `ChatToolCall` 并**挂起等待**结果（同回复续接）。
    async fn bridge_tool_call(&self, arguments: &Value, rpc_key: &str) -> Value {
        let requested = arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !self.tools.iter().any(|tool| tool.name == requested) {
            return mcp::tool_failure(format!(
                "{}：{}",
                HarnessError::UnknownTool.code(),
                HarnessError::UnknownTool.message()
            ));
        }
        let payload = arguments.get("arguments").cloned().unwrap_or(Value::Null);
        // 同一 JSON-RPC id 的重试共享同一个槽与同一个 `tool_call_id`（传输层去重），
        // **绝不用内容哈希**——连续两次相同的合法调用是两次真实调用（R58）。
        let dedupe_key = if rpc_key.is_empty() {
            // 没有 JSON-RPC id 时不做跨请求去重：每次到达都是一次新调用。
            // Keep this in a separate namespace from caller-controlled string
            // ids; otherwise an id such as `anon-0` could alias the first
            // anonymous request and change the transport retry semantics.
            format!("anon:{}", self.lock().next_index)
        } else {
            // The JSON-RPC id is the transport retry identity, not the
            // canonical tool-call identity.  Namespace it for the same
            // collision reason as anonymous requests.
            format!("rpc:{rpc_key}")
        };
        let receiver = {
            let mut state = self.lock();
            if state.revoked {
                return mcp::tool_failure(format!(
                    "{}：{}",
                    HarnessError::TokenRevoked.code(),
                    HarnessError::TokenRevoked.message()
                ));
            }
            let index = match state.id_index.get(&dedupe_key) {
                Some(identity) => {
                    if identity.name != requested || identity.arguments != payload {
                        return mcp::tool_failure(format!(
                            "{}：{}",
                            HarnessError::RequestMismatch.code(),
                            HarnessError::RequestMismatch.message()
                        ));
                    }
                    identity.index
                }
                None => {
                    if state.awaiting.len() >= MAX_PENDING_CALLS {
                        return mcp::tool_failure(format!(
                            "{}：{}",
                            HarnessError::TooManyCalls.code(),
                            HarnessError::TooManyCalls.message()
                        ));
                    }
                    let index = state.next_index;
                    state.next_index += 1;
                    state.id_index.insert(
                        dedupe_key.clone(),
                        CallRequestIdentity {
                            index,
                            name: requested.to_string(),
                            arguments: payload.clone(),
                        },
                    );
                    index
                }
            };
            let tool_call_id = harness_tool_call_id(&self.turn_id, index);
            if let Some(done) = state.completed.get(&tool_call_id).cloned() {
                return mcp::tool_outcome(&done);
            }
            if let Some(existing) = state
                .awaiting
                .iter()
                .find(|call| call.tool_call_id == tool_call_id)
            {
                existing.sink.subscribe()
            } else {
                let (sink, receiver) = watch::channel(None);
                state.awaiting.push(AwaitingCall {
                    tool_call_id: tool_call_id.clone(),
                    name: requested.to_string(),
                    arguments: payload,
                    sink,
                    delivered: false,
                });
                receiver
            }
        };
        self.changed.notify_waiters();
        self.await_slot(receiver).await
    }

    /// 挂起等待：时限内拿到结果走同回复续接，超时撤销令牌走下一轮回填。
    async fn await_slot(&self, mut receiver: watch::Receiver<Option<ToolCallOutcome>>) -> Value {
        let wait = async {
            loop {
                if let Some(outcome) = receiver.borrow().clone() {
                    return Ok(outcome);
                }
                if receiver.changed().await.is_err() {
                    return Err(());
                }
            }
        };
        match tokio::time::timeout(self.suspension_timeout, wait).await {
            Ok(Ok(outcome)) => mcp::tool_outcome(&outcome),
            Ok(Err(_)) => mcp::tool_failure("token_revoked"),
            Err(_) => {
                // 超时 / 通道关闭：返回 magi_tool_timeout 并撤销令牌。
                // 对话保留，loop 下一次调用用 magi-tool-result 块在同一对话的下一轮回填。
                self.revoke_for_timeout();
                mcp::tool_failure("magi_tool_timeout")
            }
        }
    }
}

/// 会话表：daemon 进程内、按令牌不可逆引用索引。
///
/// **不落盘、不新增文件、不走 `RuntimeStatePersistence::save_json`**（设计基线 §5.12）；
/// 进程重启即按「无会话」重建。
#[derive(Default)]
pub struct HarnessRegistry {
    sessions: Mutex<HashMap<String, Arc<HarnessSession>>>,
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开启一次会话并登记令牌引用。
    pub fn open(&self, spec: HarnessTurnSpec) -> Arc<HarnessSession> {
        let token_ref = token_ref(&spec.token);
        let session = Arc::new(HarnessSession {
            session_id: spec.session_id,
            thread_id: spec.thread_id,
            engine_id: spec.engine_id,
            epoch: spec.epoch,
            turn_id: spec.turn_id,
            token: spec.token,
            token_ref: token_ref.clone(),
            tools: spec.tools,
            suspension_timeout: spec.suspension_timeout,
            state: Mutex::new(SessionState::default()),
            changed: Notify::new(),
        });
        let previous = self
            .sessions
            .lock()
            .expect("harness registry lock")
            .insert(token_ref, Arc::clone(&session));
        // A reused token is a caller bug, but replacing the old entry without
        // revoking it would leave an independently suspended MCP response
        // alive.  Fail closed on the collision.
        if let Some(previous) = previous {
            previous.revoke();
        }
        session
    }

    /// 按令牌不可逆引用查找会话。
    pub fn lookup(&self, token_ref: &str) -> Option<Arc<HarnessSession>> {
        let mut sessions = self.sessions.lock().expect("harness registry lock");
        let session = sessions.get(token_ref).cloned();
        if session.as_ref().is_some_and(|session| session.is_revoked()) {
            sessions.remove(token_ref);
            return None;
        }
        session
    }

    /// 撤销并移除一个会话。
    pub fn revoke(&self, token_ref: &str) {
        if let Some(session) = self
            .sessions
            .lock()
            .expect("harness registry lock")
            .remove(token_ref)
        {
            session.revoke();
        }
    }

    /// 会话删除 / 空闲释放时按 Magi 会话 id 清理。
    pub fn forget_session(&self, session_id: &str) -> usize {
        let mut sessions = self.sessions.lock().expect("harness registry lock");
        let doomed = sessions
            .iter()
            .filter(|(_, session)| session.session_id == session_id)
            .map(|(token_ref, _)| token_ref.clone())
            .collect::<Vec<_>>();
        for token_ref in &doomed {
            if let Some(session) = sessions.remove(token_ref) {
                session.revoke();
            }
        }
        doomed.len()
    }

    /// 撤销某一轮的会话（turn 进入终态）。
    pub fn revoke_turn(&self, turn_id: &str) -> usize {
        let mut sessions = self.sessions.lock().expect("harness registry lock");
        let doomed = sessions
            .iter()
            .filter(|(_, session)| session.turn_id == turn_id)
            .map(|(token_ref, _)| token_ref.clone())
            .collect::<Vec<_>>();
        for token_ref in &doomed {
            if let Some(session) = sessions.remove(token_ref) {
                session.revoke();
            }
        }
        doomed.len()
    }

    /// 应用退出 / 清除数据：撤销全部会话（挂起的 `tools/call` 以明确错误应答）。
    pub fn revoke_all(&self) -> usize {
        let mut sessions = self.sessions.lock().expect("harness registry lock");
        let doomed = sessions.keys().cloned().collect::<Vec<_>>();
        for token_ref in &doomed {
            if let Some(session) = sessions.remove(token_ref) {
                session.revoke();
            }
        }
        doomed.len()
    }

    pub fn len(&self) -> usize {
        let mut sessions = self.sessions.lock().expect("harness registry lock");
        sessions.retain(|_, session| !session.is_revoked());
        sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 令牌的不可逆引用（sha256 hex）。`provider_context` 里只放这个值。
pub fn token_ref(token: &str) -> String {
    sha256_hex(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> HarnessTurnSpec {
        HarnessTurnSpec {
            session_id: "s1".into(),
            thread_id: "orchestrator".into(),
            engine_id: "chatgpt-web/gpt-5".into(),
            epoch: 1,
            turn_id: "0123456789abcdef0123456789abcdef".into(),
            token: "feedfacefeedfacefeedfacefeedface".into(),
            tools: vec![HarnessToolSchema {
                name: "read_file".into(),
                description: "读取文件".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            suspension_timeout: Duration::from_millis(150),
        }
    }

    fn call_request(id: u64, name: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(id)),
            method: "tools/call".into(),
            params: Some(serde_json::json!({
                "name": mcp::BRIDGE_TOOL_CALL,
                "arguments": {
                    "turn_token": "feedfacefeedfacefeedfacefeedface",
                    "name": name,
                    "arguments": { "path": "src/lib.rs" }
                }
            })),
        }
    }

    #[test]
    fn registry_indexes_sessions_by_token_ref() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        assert_eq!(registry.len(), 1);
        assert!(registry.lookup(&session.token_ref()).is_some());
        assert!(registry.lookup("nope").is_none());
        registry.forget_session("s1");
        assert!(registry.is_empty());
        assert!(session.is_revoked());
    }

    #[tokio::test]
    async fn inventory_lists_only_this_turns_tools() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let response = session
            .handle(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(1)),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": mcp::BRIDGE_TOOL_INVENTORY,
                    "arguments": { "turn_token": "feedfacefeedfacefeedfacefeedface" }
                })),
            })
            .await
            .expect("response");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("read_file"));
        let _ = registry;
    }

    #[tokio::test]
    async fn a_wrong_token_is_rejected_as_a_tool_failure() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let response = session
            .handle(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(2)),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": mcp::BRIDGE_TOOL_INVENTORY,
                    "arguments": { "turn_token": "deadbeefdeadbeefdeadbeefdeadbeef" }
                })),
            })
            .await
            .expect("response");
        assert_eq!(response["result"]["isError"], true);
        let _ = registry;
    }

    #[tokio::test]
    async fn a_suspended_call_is_delivered_back_into_the_same_reply() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let caller = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(7, "read_file")).await })
        };
        let batch = session
            .next_batch(Duration::from_millis(20))
            .await
            .expect("batch");
        assert_eq!(batch.calls.len(), 1);
        let calls = batch.calls.clone();
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "src/lib.rs");
        session
            .deliver_results(&[(
                calls[0].tool_call_id.clone(),
                ToolCallOutcome {
                    status: ToolResultStatus::Ok,
                    body: "fn main() {}".into(),
                },
            )])
            .expect("deliver");
        let response = caller.await.expect("join").expect("response");
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["content"][0]["text"], "fn main() {}");
    }

    #[tokio::test]
    async fn a_retry_of_the_same_jsonrpc_id_shares_one_tool_call_id() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let first = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(9, "read_file")).await })
        };
        let batch = session
            .next_batch(Duration::from_millis(20))
            .await
            .expect("batch");
        let first_id = batch.calls[0].tool_call_id.clone();
        let retry = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(9, "read_file")).await })
        };
        session
            .deliver_results(&[(
                first_id.clone(),
                ToolCallOutcome {
                    status: ToolResultStatus::Ok,
                    body: "ok".into(),
                },
            )])
            .expect("deliver");
        let first_response = first.await.expect("join").expect("response");
        let retry_response = retry.await.expect("join").expect("response");
        assert_eq!(first_response["result"]["content"][0]["text"], "ok");
        assert_eq!(retry_response["result"]["content"][0]["text"], "ok");
        assert_eq!(first_id, harness_tool_call_id(&session.turn_id, 0));
    }

    #[tokio::test]
    async fn many_transport_retries_all_receive_the_same_settled_result() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let mut callers = Vec::new();
        for _ in 0..8 {
            let session = Arc::clone(&session);
            callers.push(tokio::spawn(async move {
                session.handle(call_request(10, "read_file")).await
            }));
        }
        let batch = session
            .next_batch(Duration::from_millis(1))
            .await
            .expect("batch");
        session
            .deliver_results(&[(
                batch.calls[0].tool_call_id.clone(),
                ToolCallOutcome {
                    status: ToolResultStatus::Ok,
                    body: "same result".into(),
                },
            )])
            .expect("deliver");

        for caller in callers {
            let response = caller.await.expect("join").expect("response");
            assert_eq!(response["result"]["isError"], false);
            assert_eq!(response["result"]["content"][0]["text"], "same result");
        }
    }

    #[tokio::test]
    async fn a_conflicting_jsonrpc_retry_cannot_reuse_a_side_effect_slot() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let first = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(11, "read_file")).await })
        };
        let batch = session
            .next_batch(Duration::from_millis(1))
            .await
            .expect("batch");
        let mut conflicting_request = call_request(11, "read_file");
        conflicting_request.params = Some(serde_json::json!({
            "name": mcp::BRIDGE_TOOL_CALL,
            "arguments": {
                "turn_token": "feedfacefeedfacefeedfacefeedface",
                "name": "read_file",
                "arguments": { "path": "different.rs" }
            }
        }));
        let conflicting = session.handle(conflicting_request).await.expect("response");
        assert_eq!(
            conflicting["result"]["content"][0]["text"],
            "request_mismatch：同一 JSON-RPC id 不能代表不同的工具调用"
        );
        session
            .deliver_results(&[(
                batch.calls[0].tool_call_id.clone(),
                ToolCallOutcome {
                    status: ToolResultStatus::Ok,
                    body: "ok".into(),
                },
            )])
            .expect("deliver");
        assert_eq!(
            first.await.expect("join").expect("response")["result"]["isError"],
            false
        );
    }

    #[tokio::test]
    async fn explicit_revocation_wakes_a_hanging_call_without_mislabeling_it_as_timeout() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let caller = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(12, "read_file")).await })
        };
        let _ = session
            .next_batch(Duration::from_millis(1))
            .await
            .expect("batch");
        session.revoke();
        let response = caller.await.expect("join").expect("response");
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["result"]["content"][0]["text"], "token_revoked");
    }

    #[tokio::test]
    async fn duplicate_result_delivery_is_idempotent_but_conflicting_delivery_is_rejected() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let caller = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.handle(call_request(13, "read_file")).await })
        };
        let batch = session
            .next_batch(Duration::from_millis(1))
            .await
            .expect("batch");
        let call_id = batch.calls[0].tool_call_id.clone();
        let result = (
            call_id.clone(),
            ToolCallOutcome {
                status: ToolResultStatus::Ok,
                body: "one".into(),
            },
        );
        session
            .deliver_results(std::slice::from_ref(&result))
            .expect("deliver");
        assert!(
            session
                .deliver_results(std::slice::from_ref(&result))
                .is_ok()
        );
        let conflicting = (
            call_id,
            ToolCallOutcome {
                status: ToolResultStatus::Ok,
                body: "two".into(),
            },
        );
        assert_eq!(
            session.deliver_results(std::slice::from_ref(&conflicting)),
            Err(HarnessError::BatchMismatch)
        );
        assert_eq!(
            caller.await.expect("join").expect("response")["result"]["content"][0]["text"],
            "one"
        );
    }

    #[tokio::test]
    async fn a_timeout_revokes_the_token_and_the_registry_reports_it() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let response = session
            .handle(call_request(3, "read_file"))
            .await
            .expect("response");
        assert_eq!(response["result"]["isError"], true);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("magi_tool_timeout")
        );
        assert!(session.is_revoked());
        assert_eq!(registry.revoke_turn(&session.turn_id()), 1);
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_rejected_before_suspending() {
        let registry = HarnessRegistry::new();
        let session = registry.open(spec());
        let response = session
            .handle(call_request(4, "write_file"))
            .await
            .expect("response");
        assert_eq!(response["result"]["isError"], true);
        assert!(!session.is_revoked());
        assert_eq!(registry.len(), 1);
    }
}
