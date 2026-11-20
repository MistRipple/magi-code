# P3 实时助手流设计（`follow` `assistant-stream`）

状态：已实施并完成单元与真实 DSH 客户端交叉验证（2026-11-20）。
关联：[模型调用记录持久化设计](model-call-record-persistence-design.md)（P3 节）、[DSH 会话网关设计](dsh-session-gateway-design.md)（§2.5、§5、§6）、[共享 Cordis 宿主内部控制协议](plugin-host-protocol.md)。

## 1. 目标与边界

为 `session.follow({ assistantStream: true })` 提供 DSH 协议的进程内实时助手流：
- 开场 `snapshot` 帧携带 `assistantStream: { revision, activeAttempt? }` 基线；
- 模型流式推理期间持续发送 `{ type: 'assistant-stream', frame }`（`start`、`chunk`、`end`），让页面实时渲染文本、思考与工具调用增量；
- 尝试结束并与持久日志对齐后，发出 `end` 帧（`committed` 指向刚提交的 `assistant/message` 或 `assistant/attempt` 稠密序号，或 `abandoned` 放弃临时行）。

**边界与非目标**：
- 实时流是**进程内非持久化**传输，不写事件日志、不影响 `page` 与历史重放；持久化历史回放仍由 P1 的 `ModelAttemptStreamRecorded` 提供。
- 仅覆盖主线 `Execution` 模型步骤的 HTTP 流式尝试（与 P1 同一原始块来源）；工作线程（子代理）、压缩辅助步骤、`replace` 语义改写提供方与无统一块层的外部执行器不发实时流。
- 绝不阻塞模型推理或 UI `ModelStreamingDelta`；发送队列满或载荷超限时降级为 `overflow`，由受影响的 `follow` 流以 `gateway/overflow` 结束让客户端重开，**绝不跳号**。

## 2. DSH 协议不变量（核对锁定包 `@deepseek-ai/dsh-api-session-controller@0.2.2-rc.3`）

依据原包 `src/types.ts`、`src/assistant-stream.ts`、`src/history/assistant-stream.ts` 与 `src/client/sessions/assistant-stream.ts`：

1. **帧形状**：
   - `start`: `{ type: 'start', attemptId, revision, startedAfterSeq, turn, step }`
   - `chunk`: `{ type: 'chunk', attemptId, revision, index, time, chunk }`（`chunk` 为单条 DSH `StreamChunk`：`block-start`、`text-delta`、`reasoning-delta`、`tool-call-delta`、`block-end`、`usage`、`finish`）
   - `end`: `{ type: 'end', attemptId, revision, index, outcome }`
     - `outcome = { kind: 'committed', eventType: 'assistant/message' | 'assistant/attempt', seq }`
     - 或 `outcome = { kind: 'abandoned' }`
2. **`revision` 严格连续**：同一会话上从 `1` 起严格连续递增（每条 `start` / `chunk` / `end` 递增 1）。
3. **`index` 严格连续**：同一次尝试内 `chunk.index` 从 `0` 连续递增，`end.index === nextIndex`。
4. **先持久、后 `end(committed)`**：客户端 `ClientAssistantStream.acceptDurable` 在收到匹配当前 `activeAttempt` 的持久事件（`seq > startedAfterSeq` 且 `(turn, step)` 一致）时将其暂存在 `pending`，随后 `acceptFrame(end)` 凭 `outcome.seq` 与 `outcome.eventType` 释放它。若 `end(committed)` 早于对应持久事件到达或 `seq` 不匹配，客户端会触发 `rebaseline`。因此 **`end(committed)` 必须在对应持久 `event` 帧送入同一条 `follow` 流之后立即发出**。
5. **开场基线 `snapshot.assistantStream`**：若打开 `follow({ assistantStream: true })` 时该会话存在进行中的尝试，基线携带 `{ revision, activeAttempt: { attemptId, startedAfterSeq, turn, step, nextIndex, stream } }`（`stream` 为紧凑 `AssistantStreamRecord[]`）；否则仅携带 `{ revision }`（从未发过时为 `{ revision: 0 }`）。

## 3. 单一所有权与整条链路

```
magi-bridge-client::AttemptStreamRecorder
  └─ 协议原始块 → 同步产出紧凑记录(P1) + 展开后的 LiveStreamChunk(规范工具名)
       │ ModelTransportEvent::{LiveStreamStart, LiveStreamChunk, LiveStreamAbandon}
       ▼
magi-conversation-runtime::model_step::on_transport
  └─ 不落盘、不进 ModelTransportRecorder；调用 SessionStore::publish_live_assistant_stream
       │ SessionEventWriter::publish_live_assistant_stream
       ▼
magi-daemon::StateRepository + NativePluginCoordinator
  ├─ 校验主线 Execution 步骤，计算 (turn, step, afterEventSeq)
  ├─ 追踪单会话活跃 attempt 与 chunk 连续 index；队列满/超帧标记 degraded 并尝试发 overflow
  └─ 经同一共享控制连接发送单向通知 host.session.assistantStream
       │
       ▼
Node 宿主 session-gateway.mjs（每会话唯一 LiveAssistantStreamState）
  ├─ 维护会话级严格连续 revision 与 SessionAssistantStreamAccumulator 基线
  ├─ 接收 host.session.assistantStream（start / chunk / abandon / overflow）
  ├─ 接收 host.session.committed 时，在匹配 (turn, step) 的 assistant/message 或 assistant/attempt
  │  分配完稠密 seq 并送入 follow 队列后，紧接发出 end(committed, eventType, seq)
  ├─ 中途连入 follow 时用 start.afterEventSeq 对照 snapshot.events.sourceSequence 精确求 startedAfterSeq
  └─ 发生 overflow、index 跳号或 FollowFeed 满 4096 帧时，以 gateway/overflow 结束流而不跳号
```

### 3.1 `magi-bridge-client`：与 P1 同源展开

- 新增公开结构体 `LiveStreamChunk { time: u64, chunk: Value }`。
- `AttemptStreamRecorder` 内部写块逻辑保持单一路径：新增 `push_emitting`、`finish_with_emitting`、`fail_with_emitting`，在写入紧凑 `records` 的同时按到达顺序同步收集每条展开的 `LiveStreamChunk`（`tool-call` 的 `block-start`/`block-end` 实时通过 `decode_name` 还原为规范工具名，不等到 `finish`）。这样实时块序列与持久化流经 `expandAssistantStream` 展开后的序列完全一致。
- `ModelTransportEvent` 新增三个非持久化变体：
  - `LiveStreamStart`
  - `LiveStreamChunk(LiveStreamChunk)`
  - `LiveStreamAbandon`
- `ModelTransportRecorder::apply` 忽略这三个实时事件，不影响传输回执。
- `http_model_client/streaming.rs`：
  - `StreamRecording::start(wire_name_map)` 在尝试开始时即持有工具名解码表；
  - 进入流式读取循环前发 `LiveStreamStart`；
  - 每次 `apply_provider_stream_event` 消费原始块时同步发出产生的 `LiveStreamChunk`；
  - 遇到 `replace` 语义改写帧时调用 `invalidate()` 并立刻发出 `LiveStreamAbandon`；
  - 正常结束：若有可归一终止原因，发出最终关闭块 + `finish` 的 `LiveStreamChunk`，再发 `StreamRecorded`；若终止原因不可归一无法持久化，发出 `LiveStreamAbandon`；
  - 失败/取消结束：若记录器仍有效，发出 `finish(error|aborted)` 的 `LiveStreamChunk`，再发 `StreamRecorded`。

### 3.2 `magi-session-store` & `magi-conversation-runtime`

- `magi-session-store` 定义 `LiveAssistantStreamEvent`（`Start`、`Chunk { time, chunk }`、`Abandon`），在 `SessionEventWriter` 上新增默认空实现方法：
  `fn publish_live_assistant_stream(&self, _session_id: &SessionId, _turn_id: &str, _step_id: &str, _attempt_id: &str, _event: LiveAssistantStreamEvent) {}`
- `SessionStore::publish_live_assistant_stream` 在存在 writer 时查询当前 `step` 的最近一次 `attempt_id` 并转发给 writer。
- `magi-conversation-runtime::model_step::on_transport`：
  - 拦截 `LiveStreamStart` / `LiveStreamChunk` / `LiveStreamAbandon`，转发给 `SessionStore::publish_live_assistant_stream`；
  - 若 `record_model_attempt_stream` 返回 `Err`（写入失败）或因超 8 MiB 返回 `Ok(None)`（未持久化流记录），立刻补发 `LiveAssistantStreamEvent::Abandon`，避免前端永远停留在未结算的临时流；
  - 注意：`SessionStore::record_model_attempt_stream` 同步返回 `DomainResult<Option<u64>>` 以区分"已提交"和"因超 8 MiB 或无 writer 跳过"（或检查超限日志并补发 `Abandon`）。

### 3.3 `magi-daemon`：主线过滤、水位锚点与有界背压

- `StateRepository::publish_live_assistant_stream`：
  1. 仅当共享宿主已连接且 `plugin_session_observer` 已安装时工作，否则直接返回；
  2. 在 `Start` 时读常驻投影（或已加载投影），确认该 `(turn_id, step_id)` 属于主线 `Execution` 步骤，计算 1-based `(turn, step)` 与当前会话 `last_event_seq` 作为 `afterEventSeq`；非主线/非 Execution 步骤记为忽略，不发任何通知；
  3. 通知协议 `host.session.assistantStream`（单向，与 `host.session.committed` 共用同一有界 `Peer::notify` 队列以保证严格因果序）：
     - `start`: `{ hostGeneration, sessionId, frame: { kind: "start", attemptId, turn, step, afterEventSeq } }`
     - `chunk`: `{ hostGeneration, sessionId, frame: { kind: "chunk", attemptId, index, time, chunk } }`
     - `abandon`: `{ hostGeneration, sessionId, frame: { kind: "abandon", attemptId, index } }`
     - `overflow`: `{ hostGeneration, sessionId, frame: { kind: "overflow", attemptId } }`
  4. **背压与连续性保护**：`NativePluginCoordinator` 按会话维护轻量跟踪态 `{ attempt_id, next_index, degraded }`。若任一 `notify_service("host.session.assistantStream", ...)` 返回错误（例如 `Backpressure` 队列满或超帧字节上限），立即置 `degraded = true`，丢弃该 attempt 后续全部 `chunk`，并尝试发送轻量 `overflow` 帧通知宿主中止当前订阅者的实时流，绝不继续发送跳号的 `chunk`。当 `committed` 事务发出匹配的 `assistantMessage`、`assistantAttempt` 或 `turnEnded` 时，清理对应跟踪态。

### 3.4 Node 宿主 `session-gateway.mjs`：`revision`、基线与 `end(committed)`

在共享宿主中，为每个有实时活动的会话维护一个进程内状态 `SessionLiveStream`（内含锁定包的 `SessionAssistantStreamAccumulator` 语义、`revision`、`ordinal`、当前 `activeAttempt: { attemptId, turn, step, afterEventSeq, nextIndex }`）：

1. **收到 `host.session.assistantStream`**：
   - 核对 `hostGeneration` 与 `sessionId`；
   - `overflow`：重置 `activeAttempt`，对所有正在 `follow({ assistantStream: true })` 该会话的 `FollowFeed` 触发 `gateway/overflow` 错误；
   - `start`：若仍有未结束的旧 `activeAttempt`，先递增 `revision` 发出 `end({ kind: 'abandoned' })`；再递增 `revision` 启动新 `activeAttempt`，写入累加器与全部 `assistantStream: true` 的 `FollowFeed`（每条 `follow` 流按自己当前稠密游标填入 `startedAfterSeq`）；
   - `chunk`：校验 `attemptId` 与 `index === activeAttempt.nextIndex`；若不匹配则视同断链，重置状态并对 `assistantStream: true` 的 `FollowFeed` 触发 `gateway/overflow`；匹配则递增 `revision`、推入累加器并广播给 `assistantStream: true` 的 `FollowFeed`；
   - `abandon`：若匹配当前 `activeAttempt`，递增 `revision` 发出 `end({ kind: 'abandoned' })` 并清空 `activeAttempt`。
2. **收到 `host.session.committed`**：
   - 将持久观察推入全部 `FollowFeed`；
   - 若该观察映射为 `assistant/message`（`surfaceOp === 'append'`）或 `assistant/attempt`，且与 `activeAttempt` 的 `(turn, step)` 一致且 `eventSeq > activeAttempt.afterEventSeq`：
     - 递增 `revision` 并清空 `activeAttempt`，向每个 `assistantStream: true` 的 `FollowFeed` **紧接在该持久观察之后**入队一条 `committed-end` 标记 `{ attemptId, revision, index, eventType, sourceEventSeq: eventSeq }`；
     - 在 `follow` 消费循环中，每当那条持久事件刚刚分配完稠密 `seq` 并 `yield entryFor(event)` 之后，紧接的 `committed-end` 即刻以该精确稠密 `seq` 发出 `{ type: 'assistant-stream', frame: { type: 'end', attemptId, revision, index, outcome: { kind: 'committed', eventType, seq } } }`！
   - 若该观察映射为 `turn/end` 且当前仍有同 `turn` 的未结算 `activeAttempt`：在 `turn/end` 之前（或立即）以 `revision += 1` 发出 `end({ kind: 'abandoned' })` 并清空 `activeAttempt`。
3. **中途连入 `follow({ assistantStream: true })`**：
   - 先注册 `FollowFeed`，再异步执行 `openLog(sessionId)` 得到 `snapshot`（`through`、`events`、`cursor = events.length - 1`）；
   - 在 `openLog` 返回后的同一同步帧里拍下该会话的 `SessionLiveStream` 快照 `(baseline, ordinalCut)`：
     - 若存在 `activeAttempt` 且 `activeAttempt.afterEventSeq <= snapshot.through`：
       - 检查 `snapshot.events` 中是否已有 `sourceSequence > activeAttempt.afterEventSeq` 且匹配的 `assistant/message` 或 `assistant/attempt`；若有，说明历史快照已包含该尝试结算，开场 `assistantStream` 仅发 `{ revision: baseline.revision }`，且过滤掉该已结算尝试的旧帧；
       - 否则，在 `snapshot.events` 中找到最后一个 `sourceSequence <= activeAttempt.afterEventSeq` 的事件序号作为精确 `startedAfterSeq`（找不到则为 `-1`），将其填入 `baseline.activeAttempt.startedAfterSeq` 并随开场 `snapshot` 发出！
     - 若 `activeAttempt.afterEventSeq > snapshot.through`：开场 `snapshot` 仅发 `{ revision: startBeforeRevision }`，并将 `ordinalCut` 留在 `start` 之前，让 `feed` 按因果序依次吐出 `through` 之后的持久事件、`start`、`chunk`、`end`！
   - 消费循环跳过 `ordinal <= ordinalCut` 的实时帧，确保开场基线与后续增量无缝衔接、无重复、无跳号。
4. **有界队列**：每条 `FollowFeed` 继续沿用 4096 帧硬上限，溢出时以 `gateway/overflow` 结束该流。

## 4. 验证计划

1. **`magi-bridge-client` 单元测试**：验证 `AttemptStreamRecorder` 同步产出的 `LiveStreamChunk` 序列与持久化紧凑流经展开后逐条完全一致；验证本地 HTTP 流式请求发出 `LiveStreamStart` → `LiveStreamChunk`（工具名已实时解码）→ `StreamRecorded` / `LiveStreamAbandon`（含 `replace` 放弃与失败/取消）。
2. **`magi-daemon` 单元测试**：验证主线步骤发射 `start`/`chunk`/`abandon`，子代理工作线程与非 `Execution` 步骤不发射，发送失败触发 `overflow` 降级且不续发跳号 chunk。
3. **Node 宿主 + 真实 DSH `ClientAssistantStream` 交叉测试**：
   - 完整成功回合：`start` → 多段 `chunk` → 持久 `assistant/message` → `end(committed, 'assistant/message', seq)` → `step/end`，全程经锁定包的 `ClientAssistantStream` 驱动，**零 `rebaseline`**；
   - 失败重试回合：第 1 次 `start` → `chunk` → `assistant/attempt` → `end(committed, 'assistant/attempt', seq)` → 第 2 次 `start` → `chunk` → `assistant/message` → `end(committed, ...)`，**零 `rebaseline`**；
   - `abandon`（`replace`/超限/中止）回合：`start` → `chunk` → `end(abandoned)`，客户端返回 `abandonment`；
   - 中途重连 `follow({ assistantStream: true })`：在尝试进行一半时新连入，验证 `snapshot.assistantStream.activeAttempt` 携带准确 `startedAfterSeq` 和已收到的紧凑流，后续增量无缝续接并精准结算，**零 `rebaseline`**；
   - 队列溢出与跳号：`overflow` 帧或 `index` 跳号或队列超 4096 时，流以 `gateway/overflow` 结束，绝不产出跳号 `revision`。
