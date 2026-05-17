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

工具卡、worker dispatch、task projection、assistant text 现在分别由不同路径投影。主线和 worker tab 不是同一批事实的过滤结果，而是多层映射和合并结果，容易产生卡片漂移。

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
TimelineEntry：仅做索引 / 摘要 / 非聊天运行态辅助数据
projection：仅做 canonical view model，不参与事实修正
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

## 13. 团队评审修订：执行约束与协议细化

团队评审结论：本方案方向正确，但必须避免把 canonical TurnItem 做成旧链路之外的又一层兼容层。重构期间可以存在迁移工具和短期适配器，但运行时事实源必须逐阶段收敛；每完成一个阶段，都要停止对应旧事实的新写入。

### 13.1 不可违反的架构红线

1. `TurnItem` 是唯一可见内容事实源。
2. `itemId`、`itemSeq`、`kind` 创建后不可变。
3. complete 只能更新状态、耗时、usage，不能生成或重排正文事实。
4. bootstrap 与 live 必须进入同一个 reducer。
5. 主线与 worker tab 必须由同一批 TurnItem 过滤得到。
6. timeline、projection、renderEntries 只能是派生视图，不得作为事实输入修正 TurnItem。
7. 新数据禁止继续写入 completed snapshot JSON 到 `AssistantMessage.message`。
8. 新链路上线后，不允许保留长期运行时 fallback 分支。

### 13.2 Canonical TurnItem 字段冻结规则

创建后不可变字段：

```text
sessionId
turnId
turnSeq
itemId
itemSeq
kind
createdAt
```

允许更新字段：

```text
status
content
blocks
tool.arguments
tool.result
tool.error
worker.title
visibility
updatedAt
metadata 中明确声明为运行态的字段
```

禁止更新字段：

```text
itemSeq
kind
turnSeq
worker.laneSeq
worker.laneId 所代表的排序身份
tool.callId
```

如果收到同一 `itemId` 但不可变字段不同的事件，必须拒绝该事件并记录协议错误；不能静默合并，也不能以新事件覆盖旧事实。

### 13.3 TurnItem 状态机

允许状态转换：

```text
pending → running
pending → completed
pending → failed
pending → cancelled
running → completed
running → failed
running → cancelled
```

禁止状态转换：

```text
completed → running
completed → failed
failed → completed
cancelled → running
cancelled → completed
```

失败或取消 turn 时，后端必须封口所有未终态 item：

```text
pending/running → failed 或 cancelled
```

前端 reducer 只能接受终态幂等重复事件，不能接受终态回退。

### 13.4 Upsert 幂等规则

`session.turn.item.upsert` 必须满足：

1. 同一事件重复应用，结果完全一致。
2. 同一 item 的旧版本晚到，不得覆盖较新内容。
3. 内容增量可以更新 `content`，但不能改变排序身份。
4. 工具 result 可以更新同一 `tool_call` item 的 `tool.result` 和 `status`。
5. assistant streaming 可以更新同一 `assistant_text` item 的 `content` 和 `status`。
6. 同一 item 不允许从 `assistant_text` 变成 `tool_call`，也不允许从 `tool_call` 变成 `assistant_text`。

建议引入 item 级版本：

```text
itemVersion 或 latestEventSeq
```

reducer 规则：

```text
if incoming.itemVersion < existing.itemVersion:
  ignore
else:
  validate immutable fields
  apply mutable fields
```

版本字段只用于新鲜度判断，不参与排序。

### 13.5 itemSeq 分配规则

`itemSeq` 只能由后端在 turn 内分配。分配规则：

1. user message 通常是当前 turn 的第一个可见 item。
2. assistant_text 每个模型 round 独立分配一个 itemSeq。
3. 工具调用开始时分配 itemSeq，工具结果复用同一个 itemSeq。
4. 并发工具按模型返回的 tool_calls 顺序分配 itemSeq，不按完成时间分配。
5. worker/task item 进入同一个 turn 序列；worker 内部可附带 `laneSeq`，但 `laneSeq` 不替代 `itemSeq`。
6. late event 不能申请插队 itemSeq；如果确实属于更早事实，必须由创建时已经分配好的 itemId/itemSeq upsert。

工具前文本、工具、工具后文本的目标序列必须自然表达为：

```text
assistant_text itemSeq=N
tool_call itemSeq=N+1
tool_call itemSeq=N+2
assistant_text itemSeq=N+3
```

### 13.6 assistant round 规则

每个模型 round 只能写入当前 round 对应的 assistant_text item。

错误模式：

```text
round 0 assistant_text item A
工具调用
round 1 继续写 item A
```

正确模式：

```text
round 0 assistant_text item A
工具调用
round 1 assistant_text item B
```

如果 round 只返回 tool_calls 且没有可见文本，后端不得创建可渲染的 `assistant_text` item；如果为了协议占位已经创建空 item，只能保持原 item 身份并将 `visibility.renderable=false`，不能删除、重排或复用该 item，也不能让空 item 参与 UI 排序。

### 13.7 tool_call 单 item 规则

工具调用生命周期：

```text
tool_call status=running
tool_call status=completed result=...
```

同一工具调用必须保持：

```text
itemId 不变
itemSeq 不变
kind=tool_call 不变
tool.callId 不变
```

禁止新写入：

```text
tool_call_started 可渲染 item
tool_call_result 可渲染 item
```

如果底层事件仍保留 started/result 语义，只能映射成同一个 canonical `tool_call` item 的状态更新。

### 13.8 durable turn log 设计

当前 `ActiveExecutionTurn` 主要表达 current turn，不能承担完整历史事实源。需要新增 durable turn log 或等价 compact snapshot。

建议结构：

```text
SessionTurnLog {
  sessionId
  turns[]
}

DurableTurn {
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

写入策略：

1. live 期间写 current turn 与 durable append log。
2. turn completed 后 compact 当前 turn 到 durable turns。
3. bootstrap 从 durable turns 返回 canonical turns。
4. timeline 只保留分页索引、摘要、游标，不再承载 completed snapshot facts。
5. 历史上下文构建必须从 durable turns 读取，而不是从 timeline message 反解。

### 13.9 旧数据迁移边界

迁移工具可以读取旧格式，但正常运行时不能长期 fallback。

迁移输入：

```text
普通 UserMessage TimelineEntry
普通 AssistantMessage TimelineEntry
AssistantMessage completed snapshot JSON
旧 executionItems / projection cache
```

迁移输出：

```text
canonical DurableTurn / TurnItem
```

迁移完成后：

1. 新 bootstrap 不再使用旧 snapshot parser。
2. 新消息不再写旧 completed snapshot。
3. 旧 parser 只能存在于一次性 migration 命令或明确的离线导入工具中。

### 13.10 前端落地约束

不直接大爆炸重写 `messages.svelte.ts`。前端先新增独立 canonical 层：

```text
turn-reducer.ts
turn-store.svelte.ts
turn-render-derivation.ts
```

现有 messages store 短期只做 UI 桥接：

```text
TurnItemStore → RenderItem adapter → MessageList / MessageItem
```

迁移顺序：

1. live `session.turn.item.upsert` 接入 TurnReducer。
2. bootstrap turns 接入同一个 TurnReducer。
3. thread view 从 TurnItem 派生。
4. worker tab 从 TurnItem 过滤派生。
5. 删除 projection/executionItems 事实职责。
6. 最后瘦身 MessageItem，只保留渲染职责。

### 13.11 golden replay 验收

必须建立同一组事件的三种 replay 测试：

```text
live SSE apply
bootstrap snapshot replay
persisted durable turn log reload
```

三者输出必须完全一致：

```text
RenderItem key 顺序一致
RenderItem kind 一致
正文一致
工具卡状态一致
worker tab 过滤结果一致
streaming/completed footer 状态一致
```

核心 golden case：

```text
user_message
assistant_text running: 工具前文本
tool_call running A
tool_call running B
tool_call completed B
tool_call completed A
assistant_text running: 工具后回复
assistant_text completed
turn completed
```

期望 UI 顺序始终是：

```text
user_message
assistant_text 工具前文本
tool_call A
tool_call B
assistant_text 工具后回复
```

工具完成顺序不得影响 UI 排序。

### 13.12 调整后的实施顺序

#### 阶段 0：协议冻结

产出：

```text
Canonical Turn schema
Canonical TurnItem schema
Canonical event schema
状态机
upsert 幂等规则
itemSeq 分配规则
visibility 规则
```

不改 UI，不删除旧链路。

#### 阶段 1：后端 canonical event 输出

目标：后端稳定输出 canonical events。

必须完成：

```text
assistant_stream/final → assistant_text status
tool_call_started/result → tool_call status
工具后 assistant 输出新 item
turn completed 独立事件
```

此阶段可以继续保留 timeline，但 canonical event 必须成为 live 的主输入。

#### 阶段 2：前端 TurnReducer 接 live

目标：live SSE 进入 TurnReducer，UI 通过 adapter 复用现有组件。

验收：流式中工具卡顺序正确，complete 不跳位。

#### 阶段 3：bootstrap 接同一个 reducer

目标：bootstrap 返回 canonical turns，并通过同一个 TurnReducer replay。

验收：live、complete、refresh 三者 DOM 顺序一致。

#### 阶段 4：durable turn log 替代 timeline fact

目标：历史上下文、分页、恢复不再依赖 completed snapshot JSON。

完成后禁止新写入 completed snapshot facts。

#### 阶段 5：worker/task 纳入 canonical item

目标：worker dispatch、worker tool、worker final、task status 都进入 TurnItem。

验收：主线与 worker tab 来自同一批 TurnItem。

#### 阶段 6：删除旧事实层

删除正常运行路径中的：

```text
completed snapshot parser
executionItems merge
timeline snapshot projection
complete metadata 修正 live metadata
displayOrder/timestamp/role rank 排序兜底
```

### 13.13 每阶段切断旧链路策略

禁止把删除旧链路全部推到最后。每阶段完成后必须切断对应旧写入：

1. assistant_text canonical 完成后，停止新 assistant 正文写入 timeline snapshot facts。
2. tool_call canonical 完成后，停止新 tool started/result 双 item 写入。
3. bootstrap canonical 完成后，停止正常运行时 snapshot parser 参与新会话恢复。
4. worker canonical 完成后，停止 worker projection 自行生成事实。
5. durable turn log 完成后，timeline 不再作为历史上下文事实源。

### 13.14 风险与控制

主要风险：

1. 新 canonical 层与旧 projection 长期并存，复杂度翻倍。
2. kind 仍然突变，导致 reducer 无法保证稳定渲染。
3. durable turn log 未完成前删除 timeline snapshot，导致刷新和历史上下文断裂。
4. 前端直接大改 messages store，牵连滚动、通知、session、worker 面板状态。
5. worker/task 与普通 chat/tool 同时重构，导致验证面失控。

控制策略：

1. 先冻结协议，再动实现。
2. 先打通普通 chat/tool/complete/bootstrap 最小闭环，再纳入 worker/task。
3. 每阶段都设置 golden replay 测试。
4. 每阶段都停止对应旧事实新写入。
5. 不允许通过 fallback、兼容分支或 complete 修正掩盖 live 错误。

## 14. 改造执行清单与状态

状态定义：

```text
未开始：尚未进入实现。
进行中：正在实现或验证。
已完成：代码、清理、验证均完成。
阻塞：发现真实约束，需要先解决阻塞原因。
```

更新规则：

1. 每开始一个阶段，必须把对应条目标记为“进行中”。
2. 只有完成代码修改、旧链路清理和验证后，才能标记为“已完成”。
3. 如果验证失败，条目保持“进行中”，并补充失败原因与下一步动作。
4. 如果发现必须先处理的架构约束，条目标记为“阻塞”，不能用 fallback 或兼容分支绕过。
5. 每完成一个阶段，必须同步确认对应旧事实写入已经停止。

### 14.1 阶段 0：协议冻结

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | 定义 Canonical Turn schema | 字段、生命周期、turnSeq 规则明确，不依赖 timeline snapshot。 |
| 已完成 | 定义 Canonical TurnItem schema | itemId、itemSeq、kind、createdAt 等不可变字段明确。 |
| 已完成 | 定义 canonical event schema | live、bootstrap、durable reload 使用同一事件语义。 |
| 已完成 | 定义 TurnItem 状态机 | 非法终态回退会被拒绝并记录协议错误。 |
| 已完成 | 定义 upsert 幂等规则 | 重复事件、晚到旧事件、不可变字段冲突都有明确处理。 |
| 已完成 | 定义 itemSeq 分配规则 | 工具完成顺序、worker lane、late event 不影响 UI 顺序。 |
| 已完成 | 定义 visibility 规则 | 主线、worker tab、不可渲染空 item 的过滤语义明确。 |

### 14.2 阶段 1：后端 canonical event 输出

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | 新增 canonical turn/item 类型 | Rust 类型能表达 assistant_text、tool_call、worker/task 等统一 item。 |
| 已完成 | 收敛 assistant_stream/final | 后端 canonical payload 只输出 `assistant_text` 状态更新；旧 item.kind 暂留到前端 TurnReducer 接管后切断。 |
| 已完成 | 收敛 tool_call started/result | 后端 canonical payload 中同一工具调用始终复用同一个 `tool_call` item。 |
| 已完成 | 固化工具后 assistant round | 工具后的模型输出创建新的 `assistant_text` item；普通会话后续 round 不再写独立主线 timeline entry，完成态只写回主 timeline snapshot。 |
| 已完成 | 输出 turn completed 独立事件 | 同一 `session.turn.item` 事件中输出 `canonical_event_kind=turn_completed`，turn completed 只封口状态、耗时、usage，不重排 item。 |
| 已完成 | 拒绝不可变字段冲突 | 同一 itemId 的 canonical kind、itemSeq、laneId、laneSeq、tool.callId 冲突与非法状态回退会在写入路径被拒绝。 |

### 14.3 阶段 2：前端 TurnReducer 接 live

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | 新增 `turn-reducer.ts` | reducer 只消费 canonical events，具备幂等与非法转换拒绝能力。 |
| 已完成 | 新增 `turn-store.svelte.ts` | live 状态以 TurnItem 为事实源，不反向读取 projection 修正事实。 |
| 已完成 | 新增 render 派生层 | 当前以 canonical TurnItem 派生 `SessionTimelineProjection` 作为 UI adapter，projection 不再参与 live 事实写入。 |
| 已完成 | 接入 live SSE | `session.turn.item` 已切到 canonical event data message，旧 live projection data message 已删除。 |
| 已完成 | MessageList/MessageItem 适配 | 组件继续消费派生 projection adapter，组件层不新增工具块 merge 或排序猜测。 |

### 14.4 阶段 3：bootstrap 接同一个 reducer

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | bootstrap 返回 canonical turns | bootstrap DTO 已直接返回 durable `canonical_turns`，前端优先消费 `canonicalTurns`。 |
| 已完成 | bootstrap replay 进入 TurnReducer | bootstrap durable turns 直接替换同一 TurnReducer 状态，live 与 refresh 使用同一个派生层。 |
| 已完成 | 移除新会话 snapshot parser 依赖 | session bootstrap 正常路径已切断 recentEvents replay 与 timeline projection fallback。 |
| 进行中 | 校验 live/complete/refresh 一致 | 已通过类型检查、后端测试和 durable reload 单元验证；浏览器 golden replay 仍待补齐。 |

### 14.5 阶段 4：durable turn log 替代 timeline fact

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | 新增 durable turn log 存储 | `SessionDurableState` / `SessionStoreState` 已持久化 `canonical_turns`，projection/bootstrap 可直接读取。 |
| 已完成 | live 写入 durable append log | current turn、item upsert、状态更新、取消路径同步 upsert durable canonical turn。 |
| 已完成 | turn completed compact | completed 状态写入 durable canonical turn；新 completed snapshot facts 写入点已删除。 |
| 已完成 | 历史上下文改读 durable turns | 普通 session 模型上下文已从 durable canonical turns 构造，不再从 timeline snapshot 反解。 |
| 已完成 | timeline 降级为索引/摘要 | bootstrap、模型上下文和正常渲染不再把 timeline 作为正文事实；前端已停止把 `/messages` legacy timeline page 转成聊天 projection。 |

### 14.6 阶段 5：worker/task 纳入 canonical item

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | worker dispatch 写入 TurnItem | `worker_spawned` 已映射为 canonical `worker_dispatch` 并随 current turn 持久化。 |
| 已完成 | worker tool 写入 TurnItem | task/worker 工具 started/result 复用 `tool_call` canonical item，并携带 worker visibility。 |
| 已完成 | worker final 写入 TurnItem | worker final 作为带 worker ref/visibility 的 `assistant_text` item 持久化。 |
| 已完成 | task status 写入 TurnItem | 真实 TaskStore 状态回调已写入 canonical `task_status` item，并发布同一 `session.turn.item` canonical 事件。 |
| 已完成 | worker tab 由 TurnItem 过滤派生 | 前端 canonical adapter 通过同一批 TurnItem 的 `visibility.workerTabIds` 派生 worker tab。 |

### 14.7 阶段 6：删除旧事实层

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | 删除 completed snapshot parser 正常运行路径 | 后端 builder 与写入点已删除；前端 `tryParseCompletedTurnTimelineSnapshot` 正常 parser 已删除。 |
| 已完成 | 删除 executionItems merge 事实职责 | live/bootstrap canonical adapter 不再生成或接收 executionItems；authoritative projection 会丢弃旧 executionItems，分页只展示普通 timeline artifacts。 |
| 已完成 | 删除本地 executionItems 生成层 | 前端已删除 `TimelineExecutionItem` 类型、本地 fragment executionItems 生成/索引/更新逻辑及 `timeline-execution-item-merge.ts`，latest canonical UI 不再存在第二套 item 层。 |
| 已完成 | 瘦身 UI projection 派生 | `messageProjection` 不再从 `timelineNodes` 临时重建 live projection，worker tab 收集不再扫描 executionItems，分页 merge 不再合并 executionItems；render entries 已取消 `executionItemId` 分支，authoritative projection 已阻断本地旧流式节点反向覆盖。 |
| 已完成 | 删除 unifiedUpdate 本地流式事实层 | `message-handler` 不再消费 `unifiedUpdate` 改写时间线，`unifiedMessage/unifiedComplete` 的 content 路径仅保留本地用户 echo、占位与发送失败错误；`messages.svelte.ts` 已删除 `applyTimelineStreamPatch`、RAF 合并队列和旧 dispatch lane patch 工具。 |
| 已完成 | 删除 timeline snapshot projection | `session.turn.item` live 旧 projection、bootstrap fallback、runtime current/tool/dispatch 投影构建均已切断。 |
| 已完成 | 切断 legacy 历史分页 projection merge | `MessageList` 不再调用 `/messages` legacy timeline page 合并聊天 projection；`/messages` 会话快照已返回 `canonical_turns`，前端会话切换只接入 canonical snapshot，且不再透传 legacy 分页游标；`rust-daemon-contract` 不再把 `TimelineEntry` 解析为 `TimelineProjectionArtifact`，bootstrap payload 中的 `timelineProjection` 仅保留空 adapter 形状。 |
| 已完成 | 删除 complete metadata 修正 live metadata | canonical live/refresh 不再通过 complete metadata 修正正文或排序身份。 |
| 已完成 | 删除 displayOrder/timestamp/role rank 排序兜底 | canonical adapter 顺序只来自 `turnSeq/itemSeq`；displayOrder 仅作为派生 view model 字段。 |

### 14.8 验证清单

| 状态 | 条目 | 验收标准 |
| --- | --- | --- |
| 已完成 | golden replay：live SSE apply | 已新增 `npm --prefix web run test:canonical`，覆盖普通聊天、单工具、多工具乱序完成、工具失败、取消与 late upsert 单调性。 |
| 已完成 | golden replay：bootstrap snapshot replay | 同一 fixture 将 live reducer 结果通过 `replaceCanonicalTurns` replay，校验 thread projection 签名完全一致。 |
| 已完成 | golden replay：durable reload | 同一 fixture 再次以 durable turns reload replay；已补 durable canonical turn log 写入验证，持久 log 可直接驱动 bootstrap。 |
| 已完成 | 浏览器验证：工具前文本、多工具顺序、工具后回复 | 最新会话普通聊天、单工具、多工具顺序均已完成真实浏览器验证：用户消息、工具卡、最终回复按 canonical `turnSeq/itemSeq` 顺序稳定展示；刷新后顺序不变。 |
| 已完成 | 浏览器验证：取消、失败、刷新 | 最新会话工具失败与取消场景已完成真实浏览器验证：失败工具卡保持原位，取消 turn 通过 canonical `session.turn.item` 终态事件原位更新为非 running，持久化为 `cancelled`，刷新后不出现运行态提示残留或终态回退控制台错误。 |
| 已完成 | 前端 check | legacy 历史分页 projection merge 切断后 `npm --prefix web run test:canonical`、`npm --prefix web run check` 通过。 |
| 已完成 | 后端测试/check | `cargo test -p magi-session-store -p magi-api -p magi-daemon --manifest-path Cargo.toml --no-run`、`cargo test -p magi-api deep_session_action_finalizes_turn_when_background_root_completes`、`cargo test -p magi-api append_dispatch_assistant_message_uses_current_turn_assistant_final_as_authoritative_source`、`cargo test -p magi-api completed_plain_turn_summary_does_not_inherit_previous_execution_chain` 通过。 |
| 已完成 | diff 检查 | `git diff --check` 通过。 |

### 14.9 当前状态

当前整体状态：阶段 0、阶段 1、阶段 2、阶段 3、阶段 4、阶段 5、阶段 6 的正常运行事实链路已完成收敛；new data 不再写 completed snapshot，真实 task status 已进入 canonical `task_status` item，live/bootstrap 均由同一个 TurnReducer 接管。UI projection 已进一步瘦身：主渲染派生不再从 `timelineNodes` 临时重建 live projection，authoritative projection 不再接收或分页合并 executionItems，render entries 不再支持 executionItem 子项分支，worker tab 收集只读取 artifact 级 `workerTabs`，本地 fragment executionItems 生成/索引/更新逻辑与 `TimelineExecutionItem` 类型已删除；旧 `unifiedUpdate`/RAF 合并队列/dispatch lane patch 已不再改写时间线，`unifiedMessage/unifiedComplete` 的 content 路径只保留本地用户 echo、占位和发送失败错误，不再作为 assistant/tool 正文事实层。`/messages` legacy timeline 分页已停止进入聊天 projection，并已补充返回 `canonical_turns` 作为会话快照事实；前端会话切换不再透传 legacy 分页游标，`rust-daemon-contract` 也不再把 `TimelineEntry` 解析成 `TimelineProjectionArtifact`；`recentEvents` 仅用于 ops/runtime timeline 与任务跟踪提示，不参与聊天正文事实。浏览器普通聊天、单工具、多工具顺序、工具失败、取消与刷新场景 golden replay 已复跑，并已沉淀为 `npm --prefix web run test:canonical` 自动化 fixture：运行态 `assistant_phase/system_notice` 不再作为主线可见事实残留，工具卡按 canonical 顺序稳定展示，失败工具卡不挤压 assistant final，取消 turn 持久化为 `cancelled`；针对 SSE/刷新后的迟到 item upsert，前端 reducer 已保持终态 turn 与终态 item 单调不回退，不再产生 `completed -> running` 的控制台错误。

阶段 1-5 当前进展：

1. 已在 `magi-session-store` 定义并导出 Rust Canonical Turn 协议类型。
2. 已在前端新增 TypeScript Canonical Turn 协议类型。
3. 已在现有 `session.turn.item` 事件 payload 中输出 `canonical_schema_version`、`canonical_event_kind`、`canonical_turn`、`canonical_item`，作为同事件 canonical 协议骨架。
4. `assistant_stream/final/error` 在 canonical payload 中统一收敛为 `assistant_text`。
5. `tool_call_started/result` 在 canonical payload 中统一收敛为同一个 `tool_call` item。
6. 普通会话多轮工具调用已固化为每个模型 round 独立 assistant item：工具后输出更新工具后 round 的 item，不复用工具前 item；后续 round 不再写独立主线 timeline entry。
7. 已在 session store 写入路径拒绝同一 itemId 的 canonical kind、itemSeq、laneId、laneSeq、tool.callId 冲突和非法终态回退。
8. 后端 runtime item.kind 仍作为内部写回输入存在，但前端 live/bootstrap 只消费 canonical kind；旧 projection 不再解释该字段生成正常事实。
9. 已新增前端 `turn-reducer.ts` 与 `turn-store.svelte.ts`，live 状态以 canonical TurnItem 为事实源。
10. 已删除 `session.turn.item` → `sessionTurnItemProjectionUpdated` → `applyLiveTimelineProjectionUpdate` 旧 live projection 链路。
11. bootstrap 已直接返回 durable `canonical_turns`，前端以 `replaceCanonicalSessionTurns` 接入同一个 TurnReducer，不再从 `recentEvents` replay 或走 timeline projection fallback。
12. 当前 UI 通过 canonical adapter 派生 `SessionTimelineProjection`，projection 只作为现有组件的渲染适配层，不再反向修正 live 事实；主渲染派生已停止从本地 `timelineNodes` 临时构造第二套 live projection，render entries 已停止表达 execution item 子项，authoritative projection 已阻断旧本地节点更新反向覆盖。
13. `SessionDurableState` / `SessionStoreState` 已持久化 canonical turns，并在 current turn/item/status/cancel 写入路径同步 upsert。
14. 普通 session 模型上下文已改读 durable canonical turns，不再从 timeline completed snapshot 反解历史正文。
15. worker dispatch、worker tool、worker final 已随 current turn 转入 canonical item；task status item 已由 TaskStore 状态回调写入 canonical turn。
16. 普通聊天 `assistant_phase` 已从主线可见事实中移除；前端 adapter 同时过滤历史 canonical `system_notice`，避免完成态或刷新后残留运行态提示。
17. 请求绑定终态识别已收敛到 canonical `assistant_text`，不再只识别旧 `assistant_final/assistant_error` kind。
18. 前端 TurnReducer 对终态 turn / item 增加单调保护：刷新或 SSE 重连后的迟到 `turn_item_upsert` 只能补齐内容，不能把已完成状态回退到 running。
19. 多场景浏览器验证已覆盖：普通聊天、单工具调用、连续多工具调用、工具失败、手动取消和刷新恢复；手动取消已确认 live UI 不再停留在 `running`，刷新后也不回退；所有最新会话均无控制台错误与业务网络失败。
20. 前端 golden fixture 已覆盖 live apply、bootstrap replay、durable reload 三种路径，验证同一批 canonical turns 派生出的 thread projection 签名一致。
21. BottomTabs、messageProjection 与 timeline render-items 已停止扫描 executionItems 作为 worker/主线派生输入，render entry 类型也不再暴露 `executionItemId`，避免旧 fragment/executionItems 路径继续影响 canonical UI 位置。
22. 本地 `TimelineExecutionItem` 类型、fragment executionItems 生成/索引/更新逻辑和 `timeline-execution-item-merge.ts` 已删除，前端消息 store 中不再保留第二套 item 容器。
23. 旧 `unifiedUpdate` 本地流式补丁层已停止改写时间线，`applyTimelineStreamPatch`、RAF 合并队列、`dispatch-group-lane-upsert.ts` 与 `timeline-message-fragmentation.ts` 已删除；content unified 消息仅保留本地用户 echo、占位和发送失败错误。
24. legacy `/messages` 历史分页不再合并到当前聊天 projection，`/messages` 会话快照已返回 `canonical_turns`，前端会话切换不再透传 legacy 分页游标，bootstrap adapter 不再从 `TimelineEntry.message` 构造聊天 artifact；最新会话正文、工具卡、worker 可见内容只接受 canonical turns/events。
25. `session/interrupt` 在 `cancel_current_turn` 后发布同一 canonical `session.turn.item` 终态事件，`session.turn.interrupted` 只保留为运行态通知；取消后的工具卡状态由 canonical 事实源驱动，不再依赖前端刷新或 interrupted 事件猜测。

已完成的前置事项：

1. 已完成现有聊天流式输出链路审计。
2. 已完成当前链路中的局部一致性修复。
3. 已完成 Canonical Turn Log 重构方案初版。
4. 已完成团队评审修订，补齐协议冻结、状态机、幂等、durable turn log、golden replay 和旧链路切断策略。
5. 已完成阶段 0 协议冻结：新增 Rust/TypeScript Canonical Turn 协议类型，并实现状态转换与不可变字段更新校验。

下一步可继续按同一原则审计非聊天事实链路（通知、设置、任务投影）中的历史命名和文案；聊天正文、工具卡、worker 可见内容不得恢复旧 completed snapshot、timeline projection、executionItems 或 unifiedUpdate 事实职责。
