# 模型调用记录持久化设计（流记录、系统提示、请求头）

状态：设计，待确认后实施。2026-11-19 决定：持久化（用户确认）。
目的：补齐 DSH 会话日志里 Magi 目前没有的三类事实，使 `page`/`follow` 能给出助手流式回放、`assistant/attempt`、请求检视与系统提示视图。设计不新增第二个事实源：拥有者是现有的 `ModelStep` 与会话事件日志。

## 1. 现状

- 每个 `ModelStep` 已持久化：请求指纹、提示帧哈希（`promptFrame`）、各次尝试（`attempts`）及其传输回执（HTTP 状态、用量、结果、响应计时）。
- 没有持久化：提供方的流（文本、思考、工具调用的增量块及时间）、系统提示正文、模型请求头（配置与工具表）。`promptFrame` 只有哈希，原文只存在于请求装配的那一刻。
- DSH 对应：`assistant/message.stream`、`assistant/attempt`、`system/message`、`request/header`、`request/context`。当前观察对前两者发空或不发，后三者不发。

## 2. 原则

1. 不改请求内容，不改前缀，不影响提示缓存：只记录已经发送的内容。
2. 事实只记一次、归一个拥有者：流记录与请求记录是会话事件日志里的新事件，由 `ModelStep` 的提交事务同一入口写入；观察、`page`、`follow` 都从它们派生。
3. 不补造：缺记录的旧会话不回填，观察对应字段保持缺席（流为空数组、不发 `system/message`），与现状一致。
4. 凭据不入库：见第 5 节。
5. 体积有界：见第 6 节。

## 3. 数据与事件

新增两个会话事件类型（`CanonicalTurnEventKind`），与 `ModelStepUpdated` 一样挂在回合上、带 `model_step` 身份，但载荷独立，避免每次步骤更新都重写大对象：

- `ModelAttemptStreamRecorded`：`{stepId, attemptId, stream, complete}`。`stream` 是 DSH 的紧凑流记录（`text-chunks`、`reasoning-chunks`、`tool-call-chunks`，其余为原样 `chunk`，相邻同类增量合并但保留每个增量的边界与相对时间），由 Magi 的 `LlmStreamChunk`（`contentDelta`、`thinking`、`toolCall*`、`usage`、`stopReason`）与到达时间转换而来。每次尝试结算时写一次，包括失败、取消、被重试的尝试（它们没有对应的助手消息，`follow`/`page` 以 `assistant/attempt` 呈现）。`complete=false` 表示超过上限而被放弃，此时不写 `stream` 正文。
- `ModelRequestRecorded`：`{stepId, systemPrompt, header}`，每个步骤首次提交时写一次。`systemPrompt` 是发送的系统提示正文，`header` 是请求头（模型配置与工具表）。同一会话内正文按内容摘要去重：与上一个记录的摘要相同则只写摘要引用，不重复正文（工具表很大，变更才写）。`request/header` 的 `reason`（`initial`、`resume`、`change`、`series`）由相邻记录比较得出，不另存。

投影：检查点只保留"当前系统提示与请求头"的最新值与摘要，用于继续写与去重；流与历史正文保存在被封存的事件范围里（沿用 `durable-log-compaction-design.md` 的 gzip 封存），不进内存投影。

线协议：这两个事件不进入 App Server 的 wire 模式（现有 `CanonicalTurnEvent` 种类本来就不在 schema 里），所以不触发协议生成。`ModelStep` 的 schema 不变。

## 4. 观察映射（DSH 日志）

- `assistant/message.stream`：取成功尝试的流记录；`usage` 已有。
- `assistant/attempt`：每个没有产生助手消息的尝试一条，`stream` 取它的流记录；先于它之后的下一次尝试。
- `system/message`：首个步骤之前，取 `systemPrompt`，系统提示变化时追加（沿用 DSH 的"主线系统节点"规则，变化以日志中的新节点表达）。
- `request/header`、`request/context`：由 `ModelRequestRecorded` 与上一条比较生成，`reason` 如第 3 节。
- 一个 canonical 事件仍可产生多条观察（已支持），这些新事件的观察排序固定为：`request/header`、`system/message`、`step/start`、…、`assistant/attempt`（失败的）、`assistant/message`、`step/end`。顺序规则写进测试，在线与重放共用同一推导。

## 5. 脱敏

- 请求体、系统提示、工具表中可能出现用户自己的内容，这是会话内容，按会话数据保护（与消息正文一致）。
- 凭据：API key、授权头、令牌不是请求体的一部分（在传输层添加），不进入这两类记录。写入前对 `header` 与 `systemPrompt` 运行现有的凭据脱敏器（`public_mcp_error_detail` 使用的同一套规则），命中则整条按脱敏文本记录并带 `redacted: true`，不静默丢弃。
- 外部执行器步骤（`executor_capabilities` 非空）的请求体由执行器持有，不记录系统提示与请求头，只记录它回传的流。

## 6. 体积与封存

- 单次尝试流记录上限 8 MiB（序列化后），请求记录上限 4 MiB；超限写 `complete=false`/拒绝写入并给出可见诊断，不截断（DSH 要求流"精确"）。
- 流记录的主要成本是增量块数；紧凑记录按 `dt` 数组与文本数组存放，合并相邻增量，实测体积以第一阶段的基准为准后再定上限。
- 检查点封存沿用现有间隔；因为大对象不进投影，检查点体积不随流记录增长。

## 7. 兼容与迁移

- 旧会话没有这些事件：观察缺席，行为与今天一致，不回填。
- 新事件由新版本写入；旧版本读到未知事件种类会拒绝加载该会话日志（`deny_unknown_fields`/枚举未知）。这是单向升级，随发布说明标注，不做降级兼容。

## 8. 分期与验收

- **P1 流记录**：捕获并持久化尝试流，`assistant/message.stream`、`assistant/attempt` 观察，`page` 返回；重启重放一致；超限与取消用例。
- **P2 系统提示与请求头**：`ModelRequestRecorded`，`system/message`、`request/header`、`request/context`；去重与变化用例；脱敏用例。
- **P3 实时助手流**（设计见 [live-assistant-stream-design.md](live-assistant-stream-design.md)）：`follow` 的 `assistant-stream` 增量帧需要 daemon 向宿主推送进行中的流块，帧高频，需要有界队列与修订连续性；不与 P1、P2 合并。
- 验收：每条事件通过客户端 `assertSessionWireEvent` 与 `foldSurface`；真实 daemon 上一次真实回合（含工具调用与一次重试）的日志重放与在线一致；性能上记录一次长回合的体积与写入延迟。

## 9. 待你确认的点

1. 上限（8 MiB/4 MiB）与超限处理（放弃记录并告警，而不是截断）是否接受。
2. 外部执行器步骤不记录系统提示与请求头是否接受。
3. 单向升级（旧版本不能读新日志）是否接受。

## 10. 实施前核对（2026-11-19）：捕获点必须在原始块层

核对 `conversation_loop.rs` 的流式回调与 `magi-bridge-client` 后，第 3 节的"由 `LlmStreamChunk` 转换"需要落实到具体捕获点，且有三处保真度限制：

1. **不能用 `ModelStreamingDelta`**。`conversation_loop` 的 `on_delta` 收到的是给界面用的增量：积压时后一帧并入前一帧（`ModelStreamingDelta::merge`），丢掉了增量边界；部分上游整段改写已给出文字，用 `replace` 帧表达；工具调用只带"出现过"的事实，没有 id 与参数增量。DSH 要求流"精确、不合并增量边界"，从这条路径无法还原。
2. **捕获点是协议层的原始块**。HTTP 提供方统一经 `protocol/streaming.rs::parse_stream_event(family, event) -> Vec<LlmStreamChunk>` 产生块（`ContentStart/Delta/End`、`ToolCallStart/Delta/End`、`Thinking`、`Usage`、`stop_reason`）。记录器应挂在 `http_model_client/streaming.rs` 消费这些块的位置，在合并之前按到达时间记录，再转换成 DSH 的紧凑流记录。
3. **转换不是一一对应**：`LlmStreamChunk` 没有块序号，DSH 的 `block-start/text-delta/...` 带 `index`，需要按块的开始与结束顺序分配序号；这是对结构的归一，不是还原提供方的原文。使用 `replace` 语义的提供方（整段改写正文）没有"增量流"可言，对它们记录 `complete=false`，不伪造。

对第 9 节的影响：
- 上限与超限处理不变；新增确认点 4：**`complete=false` 的提供方（replace 语义）不提供流式回放与 `assistant/attempt` 的流，是否接受**。
- 工作量比第 8 节估计的大：P1 要改 `magi-bridge-client` 的流式消费路径（每个提供方族一份测试）、把记录器经模型调用链交给步骤结算、再接事件写入与观察。建议 P1 先只覆盖已有统一块层的 HTTP 提供方，其余声明不支持。

## 11. 实施进度

2026-11-19 用户回复"按建议"：四个确认点按文档默认值执行，P1 先只覆盖有统一块层的 HTTP 提供方。

- **P1 步骤 1（已完成）**：`magi-bridge-client::AttemptStreamRecorder`（`attempt_stream.rs`）——纯转换，把带到达时间的原始块归一成 DSH 紧凑流记录；7 个单元测试；用 DSH 自己的 `expandAssistantStream` 与 `assembleAssistantStream` 交叉验证（`test/attempt-stream.test.mjs`，固定装订自 Rust 输出）：展开出的块序列与时间边界一致，DSH 从增量重建的块与记录的 `block-end` 块完全相同。终止原因无法归一时返回 `None`，不编造 `finish`。
- **步骤 2（已完成，仅成功尝试）**：记录器挂在 `http_model_client/streaming.rs::apply_provider_stream_event`，在界面增量合并之前按 HTTP 读取的到达时间（绝对毫秒）逐块记录；成功结束时经新的传输事件 `ModelTransportEvent::StreamRecorded` 交出（工具名先从线上名字还原成规范名，终止原因无法归一则不交）。经真实 HTTP 流式路径的测试：增量边界保留、线上工具名被还原、没有可归一终止原因时不产生记录。
- **步骤 3（已完成）**：会话事件 `ModelAttemptStreamRecorded`（载荷 `ModelAttemptStream{stepId, attemptId, stream}`），写入入口 `SessionStore::record_model_attempt_stream`（只给进行中 Step 的最近一次尝试；无写入器或超过 8 MiB 时不记录；失败只记警告、不让模型调用失败）；日志层校验它指向真实存在的尝试且回合常驻，不进内存投影；重放与检查点后水位与回合不变。
- **步骤 4（已完成）**：观察端用"待配对流记录"（按 (回合, 步骤)）在在线与重放两条路径上给 `assistant/message` 带上 `stream`；工作线程的步骤不记，回合结束丢弃未领走的；Node 端严格校验后原样放进 `data.stream`。带 Rust 记录器真实输出的事件通过客户端的 `assertSessionWireEvent` 校验。
- **步骤 5（已完成，失败与取消尝试的流记录与 `assistant/attempt`）**：`AttemptStreamRecorder` 新增 `fail`/`fail_with` 与 `AttemptStreamFailure::from_bridge_error`（HTTP 状态用 `provider_http_{status}` 并带 `status`，取消用 `aborted`+`CANCELLED`，其余按 `BridgeErrorLayer` 给稳定码；未关闭的块不写 `block-end`，只写 `finish` 块，并还原已录制的线上工具名）。`http_model_client/streaming.rs::streaming_http_io` 拆为外层包装 + 内层实现，外层持有 `StreamRecording`（`StreamDeltaProgress` 持有 `&mut StreamRecording`），任何 `Err` 路径均补发失败/取消流记录，`replace` 改写帧放弃流记录。`persistence.rs::append_model_attempt_stream_locked` 同步通知在线 `plugin_session_observer`；观察端（`session_observation.rs`）按 `finish.reason.kind` 区分成功流与 `error`/`aborted` 流，后者不进待配对，直接发 `assistantAttempt`（先于同一步骤之后的下一次尝试）；Node 端映射成非 surface 的 DSH `'assistant/attempt'`（无 `surfaceOp`）。已用 DSH 的 `expandAssistantStream`/`assembleAssistantStream`/`assertSessionWireEvent`/`foldSurface`、本地 HTTP 失败/重试/取消测试与检查点重放测试交叉验证。
- **P2（已完成，系统提示与请求头）**：
  - `magi-core::public_text` 复用底层凭据规则导出 `redact_credentials_text` / `redact_credentials_json_strings`（脱敏 Bearer、`sk-`、具名凭据与 URL userinfo，保留正常路径与 JSON Schema 键结构）。
  - `magi-session-store` 新增 `CanonicalTurnEventKind::ModelRequestRecorded`、`ModelRequestHeader` / `ModelRequestCallConfig` / `ModelRequestToolSchema` / `ModelRequestSnapshot` / `ModelRequestRecord` 与 `MAX_MODEL_REQUEST_RECORD_BYTES`（4 MiB），入口为 `SessionStore::record_model_request`。
  - `magi-conversation-runtime::model_step` 在主线 `Execution` 步骤首次提交（`begin_model_step`）后复用 `cache_prompt_ir` 采集脱敏后的系统提示、模型配置与规范工具表并写入；外部执行器步骤（`ExecutorOwned`）跳过不记；写入失败仅告警。
  - `magi-daemon::session_event_log` 与 `persistence` 实现按 SHA-256 摘要去重（检查点仅保留最新一条 `latest_model_request` 供跨重启/检查点继续去重与在线观察播种）、超 4 MiB 放弃并告警（不截断）以及落盘后同步通知在线观察；`session_observation` 与 Node `session-observer.mjs` 将其映射为 `request/header`（`initial`/`change`）、`request/context` 与 `system/message`（`surfaceOp: 'append'`、`source.kind: 'system-prompt'`）。
  - 验证：已通过 `magi-core` / `magi-session-store` / `magi-bridge-client` / `magi-conversation-runtime` / `magi-daemon` 单元测试（含去重、检查点重开、重启播种在线/重放一致性、4 MiB 超限放弃、外部执行器跳过、凭据脱敏）以及 Node 测试（110/110，经 DSH 原包 `foldRequestHeader`、`headerEquals`、`deriveEventMessage`、`assertSessionWireEvent`、`foldSurface` 校验）。
- **P3（已完成，实时助手流 `follow` `assistant-stream`）**：
  - 详细设计与不变量见 [live-assistant-stream-design.md](live-assistant-stream-design.md)。
  - `magi-bridge-client::AttemptStreamRecorder` 新增 `LiveStreamChunk` 与 `push_emitting` / `finish_with_emitting` / `fail_with_emitting`（与 P1 紧凑流同源生成，工具名实时解码为规范名）；`ModelTransportEvent` 新增非持久化 `LiveStreamStart` / `LiveStreamChunk` / `LiveStreamAbandon`；`http_model_client/streaming.rs` 边读 SSE 边实时发出，`replace` 改写或不可归一终止原因时发 `LiveStreamAbandon`。
  - `magi-session-store` 新增 `LiveAssistantStreamEvent` 与 `SessionEventWriter::publish_live_assistant_stream`；`magi-conversation-runtime::model_step` 拦截实时事件转发，写流失败或超 8 MiB 时补发 `Abandon`。
  - `magi-daemon` 新增 `main_execution_step_position`、`LiveAttemptTracker`（过滤子代理/非 `Execution` 步骤、保证 `chunk` 连续 `index`、队列满/超帧标记降级并尽力发 `overflow`）、`PluginServices::notify_assistant_stream` 经共享连接推送 `host.session.assistantStream`。
  - Node 宿主 `entry.mjs` / `services.mjs` / `session-reader.mjs`（并行记录 `sourceSequences`，不污染 DSH 线事件）/ `session-gateway.mjs` 维护单会话唯一 `revision` 与 `AssistantStreamAccumulator` 重连基线，按 `start.afterEventSeq` 还原中途连入的 `startedAfterSeq`，并在匹配持久事件（`assistant/message` 或 `assistant/attempt`）送入 `follow` 分配稠密 `seq` 后紧接发出 `end(committed, eventType, seq)`；断链或 `overflow` 以 `gateway/overflow` 结束流而不跳号。
  - 验证：`magi-bridge-client` / `magi-daemon` 单元测试、`cargo clippy --workspace --all-targets -- -D warnings` 全过；Node 测试 114/114（含真实挂载锁定包 `@deepseek-ai/dsh-api-session-controller/client` 端到端验证实时块渲染、持久结算、`step/end` 回收临时行与中途重连，全程零 `rebaseline`）。
- **外部执行器流记录（2026-11-20 补齐）**：`magi-conversation-runtime::model_step::build_external_executor_attempt_stream` 将外部执行器实际回传的响应（文本、思考、工具调用与终止原因）经 `AttemptStreamRecorder` 归一并落盘 `ModelAttemptStreamRecorded`（仍跳过系统提示与请求头，符合 2026-11-19 用户确认）；空响应不伪造流记录。
- **尚未覆盖**：尚未对真实外网上游模型 API 跑一次完整长回合（含工具调用、重试与体积/延迟实测）。
- 子代理只读映射（`subagent/catalog`、`subagent/descriptor`、父/子投影与只读 `page`/`follow`）、`commands` 持久化生命周期事件（`CommandEventRecorded` → `command/run`/`command/done` 与只读 `agent` lookup）、`session/attachment`、`session/title` 观察与 `title` 投影（`control` 帧严格采用 `changes: [{ key, val }]` 数组）、正式后端 `ctx.sessions` / 只读 `ctx.sessionPersistence` / 官方 `ctx.sessionQuery`（合并清退原路由专用版）、页面宿主迁移（`/api/plugins/{id}/mux` WebSocket 隧道 + `createWebSocket` 桥接 + 挂载锁定 `@deepseek-ai/dsh-client-file-upload/client`、`@deepseek-ai/dsh-api-session-controller/client`、`@deepseek-ai/dsh-client-ui-session/client` + `<SessionProvider>` 严格会话作用域绑定 + 彻底删除 `client-host/session-facts.mjs` 与手写双链路）：**均已交付并通过 Rust/Node/Web 检查、真实 `magi-daemon-app` 8 步端到端与真实 Electron 45 右侧栏沙箱 iframe 7 步端到端验收**；打包 `.app` 发布态下的固定样本回归待后续重新打包时执行。
