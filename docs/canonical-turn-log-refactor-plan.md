# Canonical Turn Log 重构方案

## 1. 背景与结论

当前聊天、工具卡、worker 卡、任务输出链路已经暴露出结构性问题：流式中间态与完成态不是同一条事实链路。live 阶段依赖 turn item、timeline entry、SSE event、前端 projection 和 message store 的组合；complete 阶段又通过 completed snapshot 重新解释 turn items。结果是同一条 assistant 输出在不同阶段可能被不同模型重排、合并或覆盖。

对比 Claude Code 与 Codex 后，正确方向不是继续修补排序、merge 或 complete 逻辑，而是彻底收敛为一条 canonical TurnItem stream：所有可见内容只由 TurnItem 表达，live、complete、bootstrap、history、主线 UI、worker tab 都消费同一批 TurnItem。

最终目标：

- 一个事实源：Canonical TurnItem。
- 一个排序源：turnSeq + itemSeq + laneSeq + blockSeq。
- 一个 reducer：live 与 bootstrap 共用。
- 一个渲染派生：thread / worker 都从同一批 TurnItem 过滤得到。
- complete 只封口，不重写正文、blocks 或排序事实。
- timeline、projection、render entries 只能是派生视图，不能拥有事实解释权。

## 2. 当前架构问题

### 2.1 多事实源并存

当前链路中至少存在以下事实层：

1. 后端 ActiveExecutionTurn / ActiveExecutionTurnItem。
2. 后端 TimelineEntry。
3. completed turn snapshot JSON。
4. 前端 TimelineProjectionArtifact。
5. 前端 executionItems。
6. 前端 message nodes。
7. 前端 threadRenderEntries / workerRenderEntries。
8. UI 层 MessageItem / ToolCard 派生状态。

这些层都可能参与内容、状态、排序、身份和生命周期判断，导致同一条输出被多次解释。

### 2.2 live 与 complete 双链路

当前 live 路径大致为：

```text
模型增量 / 工具事件
→ ActiveExecutionTurnItem
→ session.turn.item event
→ SSE
→ buildRustTurnTimelineProjectionFromEventPayload
→ applyLiveTimelineProjectionUpdate
→ message store / projection
→ UI
```

当前 complete / bootstrap 路径大致为：

```text
turn completed
→ build_completed_turn_timeline_snapshot
→ TimelineEntry AssistantMessage.message 写入 JSON snapshot
→ bootstrap / messages API
→ tryParseCompletedTurnTimelineSnapshot
→ buildTurnArtifactsFromSummary
→ canonicalizeTimelineProjection
→ UI
```

这两条链路不完全等价，所以会出现流式中间态和完成态顺序不同。

### 2.3 TimelineEntry 语义双态

`TimelineEntryKind::AssistantMessage` 当前既可能表示普通 assistant 文本，也可能表示 completed turn snapshot JSON。这让前端必须猜测 message 字段的协议类型，导致 timeline 成为事实源和恢复源，而不是单纯索引。

### 2.4 complete 仍具备修正 live 的能力

complete 当前会生成 assistant_final、completed snapshot、timeline upsert，并触发前端重建 projection。这等于允许 complete 重新组织正文、工具卡和 worker 输出。正确做法应该是：live 如果错，complete 也暴露同样错误，而不是由 snapshot 掩盖。

### 2.5 工具、worker、chat 未统一为同一种 item

工具卡、worker dispatch、task graph、assistant text 现在分别由不同路径投影。主线和 worker tab 不是同一批事实的过滤结果，而是多层映射和合并结果，容易产生卡片漂移。

## 3. 目标架构

目标架构为 Canonical Turn Log：

```text
Session
  Turn[]
    TurnItem[]
```

所有可渲染内容都是 TurnItem。

### 3.1 Canonical Turn

```text
Turn {
  sessionId
  turnId
  turnSeq
  status
  acceptedAt
  completedAt
  responseDurationMs
  usage
  items[]
}
```

Turn 只负责承载一次用户请求与对应执行链的生命周期。

### 3.2 Canonical TurnItem

```text
TurnItem {
  sessionId
  turnId
  turnSeq
  itemId
  itemSeq
  kind
  status
  content
  blocks
  tool
  worker
  visibility
  createdAt
  updatedAt
}
```

建议 item kind 收敛为：

```text
user_message
assistant_text
assistant_thinking
tool_call
worker_dispatch
worker_update
worker_result
task_status
error
system_notice
```

工具 started/result 不再是两种可渲染 item，而是同一个 `tool_call` item 的状态更新。

### 3.3 唯一排序规则

排序只允许：

```text
turnSeq → itemSeq → laneSeq → blockSeq
```

禁止以下字段参与事实排序：

```text
timestamp
timeline index
displayOrder
role rank
cardStreamSeq fallback
content length
terminality heuristic
```

这些字段最多作为展示信息或调试信息。

## 4. 后端重构方案

### 4.1 建立 canonical turn 协议

新增或收敛以下事件：

```text
session.turn.started
session.turn.item.upsert
session.turn.completed
session.turn.failed
session.turn.cancelled
```

核心事件是 `session.turn.item.upsert`：

```json
{
  "type": "session.turn.item.upsert",
  "sessionId": "...",
  "turnId": "...",
  "turnSeq": 1,
  "item": {
    "itemId": "...",
    "itemSeq": 4,
    "kind": "assistant_text",
    "status": "running",
    "content": "..."
  }
}
```

所有事件必须幂等。重复应用同一 itemId 的事件只能更新内容、状态和结果，不能改变 itemSeq。

### 4.2 模型轮次生成独立 assistant item

正确顺序：

```text
round 0 assistant_text itemSeq=2
tool_call itemSeq=3
tool_call itemSeq=4
round 1 assistant_text itemSeq=5
```

工具后的 assistant 输出必须是新的 assistant_text item，不能复用工具前的 assistant item。

### 4.3 工具 started/result 收敛为同一 item

错误模式：

```text
tool_call_started itemSeq=3
tool_call_result itemSeq=8
```

目标模式：

```text
tool_call itemSeq=3 status=running
tool_call itemSeq=3 status=completed result=...
```

同一个工具调用必须保持同一个 itemId、itemSeq、tool.callId。

### 4.4 worker/task 统一进入 TurnItem

worker 输出也必须是 TurnItem，只是带 worker/lane 归属：

```text
worker_dispatch itemSeq=3 worker=reviewer
tool_call itemSeq=4 worker=reviewer laneSeq=1
assistant_text itemSeq=5 worker=reviewer laneSeq=1
worker_result itemSeq=6 worker=reviewer
```

主线和 worker tab 只是同一批 item 的不同过滤结果。

### 4.5 complete 降级为封口事件

complete 只能做：

```text
assistant_text.status = completed
turn.status = completed
turn.completedAt = now
turn.responseDurationMs = ...
turn.usage = ...
```

complete 禁止做：

```text
生成 completed snapshot
重写 final_text
重写 blocks
重新 build artifacts
重新 merge executionItems
重新生成 renderEntries
用 complete 修正 live 排序
```

## 5. 持久化与 bootstrap 重构

### 5.1 timeline 降级

timeline 不再是事实源，只作为索引、分页或展示缓存。

禁止新数据继续写入：

```text
TimelineEntryKind::AssistantMessage.message = completed snapshot JSON
```

### 5.2 持久化 canonical turn log

建议采用 append-only log + compact snapshot：

```json
{ "type": "turn.started", "turn": { } }
{ "type": "turn.item.upsert", "item": { } }
{ "type": "turn.item.upsert", "item": { } }
{ "type": "turn.completed", "turnId": "..." }
```

定期 compact 为：

```json
{
  "sessionId": "...",
  "turns": [
    {
      "turnId": "...",
      "turnSeq": 1,
      "status": "completed",
      "items": []
    }
  ]
}
```

### 5.3 bootstrap 只返回 canonical turns

bootstrap 返回：

```text
sessions
currentSession
turns
notifications
runtimeState
```

前端 bootstrap 不再解析 TimelineEntry.message，不再区分普通 assistant 文本和 completed snapshot JSON，而是把 turns/items 批量喂给同一个 reducer。

## 6. 前端重构方案

### 6.1 新建 TurnStore

替代当前 message store / projection 混合事实。

```text
TurnStore {
  sessions: Map<SessionId, SessionState>
}

SessionState {
  turns: Map<TurnId, TurnState>
}

TurnState {
  turnId
  turnSeq
  status
  items: Map<ItemId, TurnItem>
}
```

### 6.2 唯一 reducer

所有入口共用：

```text
applyTurnStarted
applyTurnItemUpsert
applyTurnCompleted
applyTurnFailed
applyTurnCancelled
```

live SSE、bootstrap、history replay 都只能调用这套 reducer。

### 6.3 渲染纯派生

主线：

```text
allItems
→ filter visibility.thread
→ sort turnSeq/itemSeq/laneSeq/blockSeq
→ toRenderItem
```

worker tab：

```text
allItems
→ filter visibility.workers includes workerId
→ sort turnSeq/itemSeq/laneSeq/blockSeq
→ toRenderItem
```

### 6.4 UI 组件映射

```text
user_message        → UserMessageItem
assistant_text      → AssistantMessageItem
assistant_thinking  → ThinkingBlock
tool_call           → ToolCard
worker_dispatch     → DispatchGroupCard
worker_update       → WorkerStatusCard
error               → ErrorCard
```

MessageItem 不再承担工具块拼接、executionItems merge、complete 修正、worker fragment 合并等职责。

## 7. 必须删除或降级的旧链路

### 7.1 后端删除或废弃

```text
completed turn snapshot 作为 AssistantMessage.message
build_completed_turn_timeline_snapshot 的事实职责
timeline entry 承载 turn facts
assistant_stream/final 复用 streaming_entry_id 的排序语义
tool_call_started/tool_call_result 双 item
```

### 7.2 前端删除或废弃

```text
tryParseCompletedTurnTimelineSnapshot
buildTurnArtifactsFromSummary 作为事实构建器
TimelineProjectionArtifact 作为事实层
executionItems 作为第二套 item
timeline-execution-item-merge
streaming-complete-merge
complete metadata 修正 live metadata
MessageItem 内部工具块拼接
displayOrder / timestamp / role rank 排序兜底
```

### 7.3 可保留但必须降级

```text
TimelineEntry：仅做索引 / 分页 / 展示缓存
projection：仅做 view model，不参与事实修正
renderEntries：仅做 UI 派生结果，不参与排序事实生成
```

## 8. 旧数据迁移

不保留长期运行时 fallback。需要一次性迁移旧数据。

迁移规则：

1. completed snapshot JSON：读取 turn_items，转成 canonical Turn / TurnItem。
2. 普通 UserMessage：转成 user_message completed item。
3. 普通 AssistantMessage：转成 assistant_text completed item。
4. NotificationPublished：保留在 notification store，不进入聊天 TurnItem，除非明确要作为 system_notice 展示。

迁移完成后，新运行时只认 canonical turn log。

## 9. 验收标准

### 9.1 普通流式文本

要求：

```text
live 顺序 = complete 顺序 = refresh 后顺序
```

### 9.2 工具前文本 + 工具 + 工具后回复

要求顺序固定：

```text
assistant_text
tool_call
assistant_text
```

流式中、完成后、刷新后必须一致。

### 9.3 多工具并发

工具结果返回顺序可以不同，但 UI 顺序必须按 itemSeq。

### 9.4 工具失败

失败工具卡不改变排序，不挤压 assistant final。

### 9.5 worker tab

主线和 worker tab 渲染同一批 TurnItem 的不同过滤结果。

### 9.6 complete

complete 只改变 status、duration、usage，不新增正文、不改 blocks、不改 itemSeq。

### 9.7 bootstrap

刷新页面后 DOM 顺序必须和 live 结束瞬间一致。

## 10. 实施顺序

### 阶段一：协议与模型

- 定义 Canonical Turn / TurnItem schema。
- 定义 turn event schema。
- 明确 itemSeq 分配策略。
- 补齐后端模型单元测试。

### 阶段二：后端写入链路

重点改造：

```text
session_turn_execution.rs
task_llm_loop.rs
session_turn_writeback.rs
sidecar.rs
```

目标：所有可见内容只写 TurnItem。

### 阶段三：持久化

重点改造：

```text
magi-session-store
```

目标：持久化 TurnLog / TurnSnapshot，timeline 降级。

### 阶段四：SSE / API / bootstrap

目标：live 和 bootstrap 都发送 canonical turn events/items。

### 阶段五：前端 store

新增：

```text
turn-store.svelte.ts
turn-reducer.ts
turn-render-derivation.ts
```

目标：live 与 bootstrap 共用同一个 reducer。

### 阶段六：UI 接入

改造：

```text
MessageList
MessageItem
ToolCallRenderer
Worker tabs
```

目标：RenderItem 直接来自 TurnItem。

### 阶段七：删除旧链路

删除 completed snapshot parser、executionItems merge、complete merge、timeline snapshot projection、timestamp/displayOrder 排序 fallback。

## 11. 最终链路图

```text
LLM delta
  ↓
TurnItemUpsert(assistant_text)
  ↓
SessionStore TurnLog
  ↓
SSE session.turn.item.upsert
  ↓
Frontend TurnReducer
  ↓
TurnStore
  ↓
RenderDerivation
  ↓
UI

Tool start/result
  ↓
TurnItemUpsert(tool_call)
  ↓
同一条链路

Worker update
  ↓
TurnItemUpsert(worker_update)
  ↓
同一条链路

Complete
  ↓
TurnCompleted + item status completed
  ↓
同一条链路

Bootstrap
  ↓
TurnSnapshot / TurnLog replay
  ↓
同一个 TurnReducer
  ↓
同一个 UI
```

## 12. 最终完成定义

本重构只有在以下条件全部满足时才算完成：

1. 新数据不再写 completed snapshot JSON 到 AssistantMessage.message。
2. live 和 bootstrap 只使用同一个 TurnReducer。
3. 主线和 worker tab 只从同一批 TurnItem 派生。
4. complete 不再重写正文、blocks、metadata、排序身份。
5. 工具 started/result 是同一个 tool_call item 的状态更新。
6. 工具后 assistant 输出是新的 assistant_text item。
7. 删除 executionItems 作为事实层。
8. 删除 timeline/projection/message 多事实 merge。
9. 删除 timestamp/displayOrder/role rank 排序兜底。
10. 所有验收场景通过自动化或浏览器验证。
