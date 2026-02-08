# 统一流式输出架构设计（主对话区 + Worker Tab）

## 1. 文档定位

本方案用于统一主对话区与 Worker Tab 的流式消息模型，目标是在多角色并发下保持：

1. 时间轴稳定（不倒序、不跳位）。
2. 卡片实体稳定（不回写已封口卡片）。
3. 内容不丢失（晚到事件可恢复，不静默吞包）。

本方案与以下文档协同：

1. `docs/workflow/ux-flow-specification.md`
2. `docs/architecture/message-response-flow-design.md`
3. `docs/architecture/message-flow-design.md`
4. `docs/workflow/workflow-design.md`

---

## 2. 问题定义（当前实现的结构性风险）

1. 主对话区与 Worker Tab 使用同一套消息渲染组件，但缺少统一"事件级顺序锚点"，并发时可能出现顺序漂移。
2. `requestId/messageId/placeholderId` 语义交叉，前端需要补偿逻辑，增加时序边界的不确定性。
3. 若仅用"封口后丢弃 update"，会引入潜在信息丢失。
4. 角色切换时如果继续写旧卡片，会造成视觉时间轴回跳（用户感知为"卡片被推开/插回"）。

---

## 3. 设计目标与硬约束

### 3.1 目标

1. 主对话区与 Worker Tab 共享同一事件语义，展示效果基本一致（卡片样式、流式行为、完成态规则一致）。
2. 不同区域仅在"路由可见性"上区分，不在"流式机制"上分叉实现。
3. 在中断、补充指令、并发 Worker、重放恢复场景下保持一致性。

### 3.2 硬约束

1. 已封口卡片禁止回写。
2. 晚到事件禁止静默丢弃。
3. 任何卡片更新都必须有可追溯事件（`eventId` + `eventSeq`）。
4. UI 只能按事件投影渲染，不允许本地猜测补全业务状态。
5. 各层只做本层职责范围内的事，禁止越权（详见 §3.3）。
6. 禁止用「回写旧卡片」修复错序。
7. **禁止 Pipeline 层面做 ID 映射或重写**：消息的 ID 由其生产者在创建时确定，传输层不得修改。

### 3.3 六层架构与流式输出职责划分

详见 `docs/architecture/message-flow-design.md` 的「分层架构总览」和「各层详细定义」。

以下是流式输出场景下的关键约束摘要：

| 层级 | 流式输出职责 | 关键禁止行为 |
| ---- | ---- | ---- |
| L1 生产层 | 创建 MESSAGE/UPDATE/COMPLETE，**决定消息 ID** | 不做过滤、不做路由 |
| L2 中枢层 | 状态机管理、节流去重、事件分发 | **不修改消息 ID**（不做 ID 映射/重写） |
| L3 桥接层 | postMessage 透传到前端 | 不做过滤、不修改内容或 ID |
| L4 接收层 | 纯透传 | 不做过滤或转换 |
| L5 路由层 | 路由决策（占位替换/独立卡片/展示区域） | 不修改消息内容 |
| L6 渲染层 | 响应式渲染 | 不做消息过滤、不做路由决策 |

---

## 4. 统一事件模型

### 4.1 ID 职责划分

| ID | 粒度 | 语义 | 分配者（层级） | 接口位置 |
| ---- | ---- | ---- | ---- | ---- |
| `traceId` | 链路级 | 一次任务执行的全链路追踪 | MessageHub.newTrace() (L2) 生成值；消息创建者注入到消息中 | `StandardMessage.traceId`（必填） |
| `requestId` | 请求级 | 一次用户输入及其所有响应 | WebviewProvider (L3) 生成，通过调用链传播 | `metadata.requestId`（optional） |
| `messageId` | 消息级 | 一条独立消息的唯一标识，同时作为该消息对应的 `cardId` | 生产者（见 §4.4） | `StandardMessage.id`（必填） |
| `eventId` | 事件级 | 单条事件唯一标识 | Pipeline (L2) 在 process/processUpdate 时分配 | `StandardMessage.eventId`（Pipeline 填充） |
| `eventSeq` | 事件级 | 会话内全局单调递增序号 | Pipeline (L2) 在 process/processUpdate 时分配 | `StandardMessage.eventSeq`（Pipeline 填充） |

> **注**：`sessionId` 存储在 `metadata.sessionId`（optional），用于跨会话标记，不是核心消息路由字段。

**关键等式**：`messageId ≡ cardId`。每条 StandardMessage 的 `id` 字段同时作为其渲染卡片的唯一标识。不存在独立的 `cardId` 分配机制——谁创建消息，谁决定其 `id`，也就决定了其卡片归属。

### 4.2 requestId 与 messageId 的一对多关系

一个 `requestId`（用户输入）可以触发多条独立消息，每条消息对应一个独立卡片：

```text
requestId: "req-001"
├── messageId: "placeholder-001"  ← 占位符 → 被流式消息替换
│   └── (流式 LLM 输出: thinking + text，使用占位符 ID)
├── messageId: "interaction-001"  ← 独立卡片（INTERACTION：澄清请求）
├── messageId: "placeholder-001"  ← 同一占位符，第二轮流式（tool calling round）
└── messageId: "result-001"       ← 独立卡片（RESULT：最终结果）
```

**规则**：

1. 流式 LLM 输出复用占位符 ID（同一卡片内追加内容）。
2. 非流式独立消息（INTERACTION、RESULT、PROGRESS、ERROR 等）使用自己的 ID，创建独立卡片。
3. Pipeline 不做 ID 映射——不将独立消息的 ID 替换为占位符 ID。

**requestId 传播链**：

```text
WebviewProvider.emitUserAndPlaceholder(requestId)
    ↓ requestId 设置到占位符消息的 metadata.requestId
    ↓ Engine.processRequest(userMessage, requestId)
    ↓ adapter.sendMessage(prompt, requestId)
    ↓ Pipeline.doProcess(message, requestId) → 注入 metadata.requestId
    ↓ 所有该 requestId 下的消息都携带相同的 metadata.requestId
```

### 4.3 顺序字段

1. `eventSeq`：会话内全局单调递增序号，覆盖所有消息类别（CONTENT/CONTROL/NOTIFY/DATA），由 Pipeline 在 `process()` 和 `processUpdate()` 时分配，作为唯一时间轴锚点。
2. `cardStreamSeq`：单卡片内部流式序号，由 Pipeline 的 `ensureMessageEnvelope` / `ensureUpdateEnvelope` 分配，校验单卡片流完整性。

### 4.4 ID 分配职责（生产者负责原则）

消息 ID 由**生产者**在创建时确定，Pipeline 不做二次映射。

| 消息来源 | ID 分配方式 | 说明 |
| --- | --- | --- |
| Normalizer 流式输出 | `adapter.startStreamWithContext()` → `normalizer.startStream(boundId)` | 使用占位符 ID，确保流式 chunk 写入同一卡片 |
| MessageFactory 独立消息（CONTENT） | `createStandardMessage()` / `createInteractionMessage()` 等生成随机 ID | INTERACTION、RESULT、PROGRESS 等独立卡片 |
| MessageFactory 非 CONTENT 消息 | `createControlMessage()` / `createNotifyMessage()` / `createDataMessage()` 生成随机 ID | CONTROL、NOTIFY、DATA 类别消息，不参与卡片渲染 |
| WebviewProvider 用户消息 | 占位符 + 用户消息各自通过 MessageFactory 生成独立 ID | 通过 requestBinding 关联 |

**Pipeline 的职责边界**：

- ✅ 状态管理（messageStates、生命周期跟踪）
- ✅ 节流与去重（cardStreamSeq 校验、封口保护）
- ✅ 事件分发（emitByCategory）
- ✅ 维护 `requestMessageIdMap`（供 adapter 查询占位符 ID）
- ✅ 分配 `eventId` 和 `eventSeq`（所有类别）
- ✅ 分配 `cardStreamSeq`（CONTENT 类别）
- ❌ 不做 ID 映射（不将消息 ID 替换为其他 ID）
- ❌ 不判断消息类型决定归属

### 4.5 事件生命周期映射

设计概念与代码实现的对应关系：

| 设计概念 | 代码实现 | 事件 | 说明 |
| --- | --- | --- | --- |
| card_open | `MESSAGE (lifecycle=STARTED)` | Normalizer.startStream → emit MESSAGE | 创建新卡片或重新激活已完成卡片 |
| card_delta | `StreamUpdate (updateType=append/block_update)` | Normalizer.processTextDelta/processThinking → emit UPDATE | 增量更新同一卡片内容 |
| card_close | 同一 messageId 的 `MESSAGE (lifecycle=COMPLETED/FAILED/CANCELLED)` | Normalizer.endStream → emit COMPLETE | 复用原 messageId，不创建新消息，标记卡片终态 |

---

## 5. 卡片与分段模型

### 5.1 `cardId` 定义

`cardId` 代表"单段连续输出"，不是"角色固定卡片"。在当前实现中，`cardId ≡ messageId`。

### 5.2 分段规则（必须执行）

出现以下任一条件时，必须新开卡片（使用新的 messageId）：

1. 角色切换（例如 orchestrator -> claude）。
2. lane 切换（主对话区 <-> Worker Tab）。
3. 任务阶段切换且语义已变更（规划 -> 执行总结）。
4. 上一段已经 `card_close`（endStream）。
5. **消息类型切换**：流式 LLM 输出 → 交互请求（INTERACTION）、结果通知（RESULT）等非流式消息，必须使用独立 ID 创建独立卡片。

### 5.3 占位符模型

占位符是"流式 LLM 输出首卡片"的预留位置，**不是该 requestId 下所有消息的容器**。

```
用户发送消息 → WebviewProvider 创建占位符（id=P, isPlaceholder=true）
                ↓
    Pipeline 注册 requestMessageIdMap: requestId → P
                ↓
    Adapter.startStreamWithContext() 查询 → boundId = P
                ↓
    Normalizer.startStream(messageIdOverride=P) → 流式消息 id = P
                ↓
    前端：占位符被真实流式内容原地替换
```

**占位符的职责边界**：
- 仅用于流式 LLM 输出的首卡片
- Normalizer 的 MESSAGE/UPDATE/COMPLETE 自动使用占位符 ID
- 独立消息（INTERACTION、RESULT 等）不经过占位符，直接作为独立卡片

### 5.4 多轮流式场景（Tool Calling Round）

当同一 requestId 下有多轮流式输出时（例如 tool calling 递归）：

```
第 1 轮: startStreamWithContext() → boundId=P → 流式输出 → endStream
         Pipeline: state.completed = true
第 2 轮: startStreamWithContext() → boundId=P → 流式输出 → endStream
         Pipeline: 检测到 existingState.completed → 重新激活（reset completed）
         → 新内容追加到同一卡片
```

每一轮的 Normalizer 都通过 `startStreamWithContext` 获取同一个占位符 ID，在同一张卡片内追加内容。Pipeline 在检测到已完成状态收到新的 STARTED 消息时，重新激活该状态。

### 5.5 标准行为示例

当编排者输出一段后插入 Worker 节点卡片，再次编排者输出时：

1. 编排者后续内容必须用新的 `cardId`。
2. 新卡片追加在当前时间轴末尾（即 Worker 卡片下方）。
3. 禁止回到前一个编排者卡片追加内容。

---

## 6. 主对话区与 Worker Tab 的一致渲染设计

### 6.1 一套渲染引擎，两套投影视图

1. 维护单一事件日志（Event Log）。
2. 基于 `audience + lane` 做视图投影：
   1. 主对话区投影：`audience = thread|both`。
   2. Worker Tab 投影：`audience = worker:<id>|both` 且 worker 匹配。
3. 两个区域都使用同一 `card state machine` 与同一卡片组件行为规则。

### 6.2 "显示效果基本一致"的落地定义

1. 相同卡片状态样式：`pending/received/thinking/streaming/completed/failed/cancelled`。
2. 相同流式增量动画策略：增量渲染 + 尾部活动指示。
3. 相同完成态收口策略：完成后禁流、显示终态标识。
4. 不同区域仅差异：
   1. 主对话区可含 Worker 状态卡（任务叙事）。
   2. Worker Tab可含该 Worker 的细节输出（工具调用、思考、结果）。

---

## 7. 卡片状态机

```text
INIT -> OPENED -> RECEIVED -> THINKING -> STREAMING -> COMPLETED
                                          \-> FAILED
                                          \-> CANCELLED
```

状态转换规则：

1. 只能前进，不能回退。
2. `card_close` 后状态固定为终态（`COMPLETED/FAILED/CANCELLED`）。
3. 对 sealed 卡片的 `card_delta` 一律不执行回写。
4. **例外：多轮流式复用**（§5.4）——同一占位符 ID 的已完成状态可被新一轮 STARTED 重新激活。这不违反"不回写 sealed 卡片"原则，因为 sealed 指的是 Pipeline 层面的封口（sealedCards），而多轮复用操作的是 messageStates 的 completed 标志。

---

## 8. 晚到事件与不丢数机制

### 8.1 核心原则

1. 不回写 sealed 卡片。
2. 不静默丢弃业务内容。

### 8.2 处理分支

当收到 sealed 卡片的晚到 `card_delta`：

1. 若 `cardStreamSeq <= finalStreamSeq`：视为重复包，丢弃并计数。
2. 若 `cardStreamSeq > finalStreamSeq`：判定协议违例，进入 `dead-letter queue`。
3. 触发 `replay` 拉取缺失事件窗口（按 `eventSeq` 区间）。
4. 回放后仍无法归并时，生成"补遗卡片"：
   1. 新 `cardId`。
   2. `payload.parentCardId = 原 cardId`。
   3. 追加到时间轴末尾，保证可见且可审计。

---

## 9. 中断与补充指令语义

### 9.1 执行中输入默认语义

执行中输入默认作为补充指令，不打断当前卡片流，在下一决策点生效。

### 9.2 显式打断语义

仅当用户明确发送"停止/中断/取消"时：

1. 当前活动卡片接收 `card_close(cancelled)`。
2. 后续若继续执行，必须新开 `cardId`。

---

## 10. 验收标准（生产门禁）

### 10.1 一致性门禁

1. sealed 卡片回写次数必须为 0。
2. `eventSeq` 单调性违规次数必须为 0。
3. 同一 `cardId` 的 `cardStreamSeq` 逆序次数必须为 0。

### 10.2 可用性门禁

1. `late_delta` 事件可见率 100%（重复包除外）。
2. `replay` 成功率 >= 99.9%。
3. `dead-letter` 未处理积压数为 0（持续窗口内）。

### 10.3 UX 门禁

1. 主对话区与 Worker Tab 在同状态下视觉反馈一致。
2. 多 Worker 并发时主时间轴不回跳、不覆盖历史卡片。
3. 执行中补充指令不会导致无意重启。

---

## 11. 结论

该方案保证：

1. 主对话区与 Worker Tab 使用同一流式内核，展示行为一致。
2. `cardId` 采用"输出段实体"模型，杜绝时间轴回写错位。
3. 晚到事件可恢复、可审计，不以静默丢弃换取表面稳定。
4. 消息 ID 由生产者负责，Pipeline 不越权做 ID 映射，职责边界清晰。
