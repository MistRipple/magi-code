# 执行链中断 / 恢复重构实施方案

更新时间：2026-03-24

## 1. 文档定位

本文档是 [execution-resume-architecture.md](./execution-resume-architecture.md) 的工程落地版。

目标不是讨论语义，而是明确：

- 这次改造到底算多大
- 哪些模块复用，哪些模块替换
- 从哪里先动手，如何避免补丁式返工
- 每一阶段完成后，系统应该进入什么稳定状态

## 2. 结论先行

### 2.1 改造量判断

这是一次中大型架构重构，不是局部修补。

更准确的量级判断：

- 核心重构面：约 15 到 25 个核心文件
- 联动适配面：约 30 到 50 个文件
- 涉及层级：后端执行链、Worker 调度、session 持久化、restore 投影、前端 live 投影、前端渲染、e2e 验证

### 2.2 但不是推倒重来

当前代码已经具备几块可以复用的基础设施：

- `timelineProjection`
- `session timeline replay`
- `runtime-truth-contract`
- `dispatch-presentation-adapter`
- `worker-session`
- `worker-lifecycle-card`

所以这次正确姿势不是重写项目，而是：

- 保留已有可复用的时间轴基础设施
- 用新的执行链真相源接管停止 / 继续 / 恢复语义
- 删除旧的取消式恢复路径

### 2.3 核心原则

整个实施过程中必须遵守：

- 只保留一套业务语义
- 只保留一套恢复真相源
- 只保留一套 live / restore 投影规则
- 每阶段结束必须删掉对应旧路径，不允许长期双轨并存

### 2.4 状态语义单一真相源

这次改造必须额外遵守一条工程铁律：同一种状态语义，只允许一个主字段承担真相源职责。

具体约束如下：

1. `type` 只负责消息渲染形态。
   - 例如：`text` / `instruction` / `task_card` / `system-notice` / `error`
   - 不允许再用 `type` 偷带“是否失败”“是否可恢复”“是否只是状态提示”之类运行态语义。

2. 消息执行终态只由 `lifecycle` 承载。
   - 是否 streaming / completed / failed，统一看 `lifecycle`
   - 不允许再通过 `type=error`、`noticeType=error`、`metadata.isStatusMessage` 去旁路表达失败

3. 工具执行终态只由 `tool_call.status` 承载。
   - `tool_call.error` 只负责详情
   - `standardized.status` 只作为标准化结果投影，不得反向覆盖主状态模型

4. 错误详情只由 `metadata.error` / `tool_call.error` / `failureCode` 承载。
   - 这些字段只负责“为什么失败”
   - 不负责“是否失败”本身

5. `isStatusMessage` 只能是行为控制标记。
   - 仅允许用于“主回答排除”“placeholder 绑定排除”等控制逻辑
   - 不允许再参与消息类型分类、通知分类、system role 判定、时间轴展示语义判定

6. Worker 卡片状态必须拆成两层，禁止多源抢占：
   - 持久化结果态：只由卡片自身结构化字段承载（例如 `subTaskCard.status`，后续演进为标准化 assignment result status）
   - 运行时覆盖态：只由 `workerRuntime.status` 承载
   - `waitResult`、`laneTasks`、`laneTaskCards` 只能作为派生输入或报告数据，不得与卡片主状态并列抢占最终展示状态

7. 停止 / 暂停 / 取消 / 中断必须统一词典。
   - 业务执行终态与底层中断原因必须拆开
   - `stopped` 已从代码词典中彻底删除，统一使用 `cancelled`
   - 不允许 `cancelled` / `paused` / `interrupted` / `aborted` / `killed` 在不同层各说各话

## 3. 当前结构评估

## 3.1 已有可复用基础

### A. 时间轴投影基础已经存在

可复用文件：

- `src/session/session-timeline-projection.ts`
- `src/session/session-timeline-recovery.ts`
- `src/shared/timeline-ordering.ts`
- `src/shared/timeline-presentation.ts`
- `src/shared/timeline-worker-lifecycle.ts`
- `src/ui/webview-svelte/src/lib/timeline-render-items.ts`

这些模块已经提供了：

- 稳定排序
- 线程 / Worker 面板视图拆分
- artifact / execution item 结构
- timeline replay 能力

这意味着：

- 我们不需要重做“时间轴引擎”
- 需要重做的是“谁来给时间轴提供权威执行链数据”

### B. Worker 卡片聚合基础已经存在

可复用文件：

- `src/orchestrator/core/dispatch-presentation-adapter.ts`
- `src/ui/webview-svelte/src/lib/worker-lifecycle-card.ts`

当前已经有：

- `dispatchWaveId`
- `laneId`
- `workerCardId`
- `laneTaskIds`
- `laneTaskCards`

这意味着：

- Worker 卡片分组并非从零开始
- 但当前分组主语还是 dispatch batch / lane，而不是标准化 `assignmentGroup`

### C. 恢复相关基础已经存在，但语义不对

相关文件：

- `src/ui/webview-provider.ts`
- `src/services/task-view-service.ts`
- `src/orchestrator/core/dispatch-resume-context-store.ts`
- `src/orchestrator/worker/worker-session.ts`

当前问题不是完全没有恢复能力，而是：

- 主线恢复挂在 `pendingRecoveryContext`
- Worker 恢复偏内存态
- session reload 之后不继承执行态
- 停止动作直接把 recoverable 链路切断

### D. 真相源契约基础已经存在

可复用文件：

- `src/orchestrator/runtime/runtime-truth-contract.ts`

这里已经明确了：

- `timeline_rendering` 的 authority 是 `timeline_projection`
- `recovery_resume` 的 authority 是 `recovery_projection`

这很好，说明我们不是从概念上推翻，而是把这份契约真正做实。

## 3.2 当前已识别的状态语义混用点

这部分不是边角问题，而是后续恢复改造中的硬阻塞项。

### A. 消息类型、通知语义、失败语义存在历史串用

当前系统已经暴露过以下混用风险：

- `type` 被拿去暗示失败或通知语义
- `noticeType` 被拿去暗示消息主类型
- `metadata.isStatusMessage` 曾参与前端通知 / 分类语义判断

整改目标必须是：

- `type` 只负责渲染形态
- `lifecycle` 承担消息执行终态
- `metadata.error` 承担错误详情
- `isStatusMessage` 退回行为控制字段

### B. Worker 卡片状态存在多源并列仲裁

当前链路里，以下字段都曾参与“这张卡当前是什么状态”的判断：

- `subTaskCard.status`
- `subTaskCard.wait_status`
- `waitResult.results[].status`
- `laneTasks[].status`
- `laneTaskCards[].status`
- `workerRuntime.status`

这会直接导致：

- live 与 restore 不一致
- 已终态卡片被运行态反向覆盖
- 等待结果与任务终态混成一层

后续必须收敛为：

- 卡片持久化状态一条线
- 运行时覆盖状态一条线
- 报告型字段不再参与主状态仲裁

### C. 停止 / 暂停 / 取消 / 中断词汇漂移

**`stopped` 已被彻底删除**，统一为 `cancelled`。当前项目不同层仍存在以下词汇需要继续拆层：

- `cancelled`（业务终态 — 已统一）
- `paused`
- `interrupted`
- `aborted`
- `killed`

这些词有的表达业务终态，有的表达底层中断原因，但目前没有统一词典。

后续必须拆开：

- 业务执行终态
- 底层中断原因
- UI 展示映射

否则恢复链路一定会继续出现双语义问题。

## 3.3 当前必须替换的错误路径

以下逻辑必须被视为旧路径，最终删除：

1. `continueTask -> executeTask(message.prompt)`
   - 文件：`src/ui/webview-provider.ts`

2. `recoverRunningState() -> mission.executing => cancelled`
   - 文件：`src/services/task-view-service.ts`

3. 停止时把未完成 worker / todo 直接写成 `cancelled`
   - 文件：`src/orchestrator/core/mission-driven-engine.ts`
   - 文件：`src/orchestrator/core/dispatch-batch.ts`
   - 文件：`src/agent/service/agent-runtime-service.ts`

4. `DispatchResumeContextStore` 只做 mission -> workerSession 的内存映射
   - 文件：`src/orchestrator/core/dispatch-resume-context-store.ts`

5. 前端 restore 明确清空运行态，不承接 recoverable 执行链
   - 文件：`src/ui/webview-svelte/src/stores/messages.svelte.ts`

这些逻辑如果不删，后续一定会继续形成双语义冲突。

## 4. 重构总蓝图

建议拆成 6 个阶段，每个阶段结束都必须达到“可稳定提交”的状态。

---

## 5. 阶段一：建立执行链真相源

### 5.1 目标

把恢复能力从 `mission + pendingRecoveryContext + memory map` 提升为正式的执行链模型。

### 5.2 推荐新增模块

建议新增到 `src/orchestrator/runtime/`：

- `execution-chain-types.ts`
- `execution-chain-store.ts`
- `execution-chain-query-service.ts`
- `resume-snapshot-types.ts`
- `resume-snapshot-store.ts`
- `resume-snapshot-builder.ts`

### 5.3 需要调整的现有模块

- `src/orchestrator/runtime/index.ts`
- `src/orchestrator/runtime/runtime-truth-contract.ts`
- `src/orchestrator/runtime/orchestration-runtime-query-service.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/ui/webview-provider.ts`

### 5.4 这一阶段要做成什么

必须落地：

- `ExecutionChainRecord`
- `BranchRecord`
- `AssignmentGroupRecord`
- `ResumeSnapshot`

并完成以下绑定：

- 用户一次执行请求创建一个 `ExecutionChainRecord`
- 当前 mission / plan / requestId 绑定到 chain
- Worker 分支绑定到 chain
- chain 成为停止 / 继续 / 放弃的目标对象

### 5.5 阶段一完成标准

- 新执行请求已经有 `chainId`
- 不依赖 `pendingRecoveryContext` 才能找到“当前执行对象”
- session 级查询可以拿到当前 chain 概况

---

## 6. 阶段二：重写停止 / 继续 / 放弃语义

### 6.1 目标

让用户动作语义和后端状态机一致。

### 6.2 重点改造文件

- `src/ui/webview-provider.ts`
- `src/ui/shared/bridges/web-client-bridge.ts`
- `src/agent/service/agent-runtime-service.ts`
- `src/agent/service/local-agent-service.ts`
- `src/services/task-view-service.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/mission/types.ts`

### 6.3 关键改法

#### A. 停止

把当前“停止即取消”改为：

- chain 进入 `interrupted`
- 主线与 Worker 执行被 quiesce
- 写入最终 `ResumeSnapshot`
- UI 展示“已停止，可继续 / 放弃”

#### B. 继续

把当前 `continueTask -> executeTask(prompt)` 改为：

- 根据当前 session 查找最近一条 `interrupted && recoverable` chain
- 读取 `latestSnapshotId`
- 调用统一 resume 流程

#### C. 放弃

新增明确的 abandon / cancel 语义：

- chain -> `cancelled`
- `recoverable = false`

### 6.4 这一阶段额外必须完成的语义收敛

除了 stop / continue / abandon 主链改造，还必须同步完成：

- 停止 / 暂停 / 取消 / 中断的统一状态词典
- 业务终态与底层中断原因拆层
- UI / runtime / persistence / resume 查询层统一映射

最低要求是：

- `stopped` 已从词典中删除（已完成），用户主动停止后统一写 `cancelled`
- 不允许不同层对同一终止动作使用 `cancelled` / `interrupted` 等不同词汇
- `aborted` / `killed` / `timeout` 这类底层原因，不再直接冒充业务主状态

### 6.5 需要删除的旧路径

- `continueTask` 直接新开执行
- `pendingRecoveryContext` 作为恢复唯一真相源
- 启动恢复把执行态直接清成 `cancelled`
- 用词汇映射补丁在 UI 层临时兜底 stop / cancel / interrupted 分歧

### 6.6 阶段二完成标准

- `停止` 后状态为 `interrupted`
- `继续` 不再新开 chain
- `放弃` 后不可恢复
- 全链路对停止 / 取消 / 暂停 / 中断有统一词典，不再出现同层并行语义

---

## 7. 阶段三：Worker 分支与 assignmentGroup 正式化

### 7.1 目标

把当前基于 dispatch batch/lane 的展示逻辑，升级为正式的 Worker 分支模型。

### 7.2 重点改造文件

- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/core/dispatch-batch.ts`
- `src/orchestrator/core/dispatch-presentation-adapter.ts`
- `src/orchestrator/core/dispatch-resume-context-store.ts`
- `src/orchestrator/worker/autonomous-worker.ts`
- `src/orchestrator/worker/worker-session.ts`
- `src/shared/timeline-worker-lifecycle.ts`
- `src/ui/webview-svelte/src/lib/worker-lifecycle-card.ts`

### 7.3 必须建立的新主语

不能再只靠：

- batch.id
- laneId
- workerCardId

必须新增：

- `assignmentGroupId`
- `branchId`
- `workerCardKey = assignmentGroupId + workerSlot`

### 7.4 关键规则

1. 同一 `assignmentGroupId` 内，同一 Worker 的串行任务合并到一个卡片
2. 新一轮 Worker 分配必须生成新的 `assignmentGroupId`
3. Worker 恢复时沿用原 `branchId` 与 `assignmentGroupId`
4. Worker 总结、任务列表、状态更新只更新本卡片，不允许串到主线

### 7.5 `DispatchResumeContextStore` 的调整方向

当前它只记录：

- `missionId -> workerSessionBySlot`

需要升级为：

- `chainId + assignmentGroupId + branchId -> worker resume snapshot`

### 7.6 阶段三完成标准

- 同一 Worker 在同一分配轮次中的多次串行任务合并显示
- 新分配轮次不会复用旧 Worker 卡片
- Worker 恢复时不会丢卡片身份

---

## 8. 阶段四：session 持久化与 restore 投影升级

### 8.1 目标

让 `.magi/sessions/session-xxxxx/session.json` 成为完整恢复基座，而不只是历史展示基座。

### 8.2 重点改造文件

- `src/session/unified-session-manager.ts`
- `src/session/session-timeline-projection.ts`
- `src/session/session-timeline-recovery.ts`
- `src/session/timeline-record-adapter.ts`
- `src/agent/service/agent-runtime-service.ts`
- `src/agent/service/local-agent-service.ts`

### 8.3 持久化结构升级

当前 `PersistedUnifiedSessionRecord` 只有：

- `messages`
- `timeline`
- `notifications`
- `snapshots`
- `timelineProjection`

需要扩成：

- `executionChains`
- `resumeSnapshots`
- 如有必要，再补 `branches` 或从 snapshots 中反解

### 8.4 restore 规则

session 加载时必须做到：

- 还原用户可见时间轴
- 还原当前可恢复执行链
- 若进程曾异常退出，带快照的 running/resuming 链收敛为 `interrupted`

不允许继续保留：

- “只恢复历史展示，不继承运行态”的旧结论

### 8.5 阶段四完成标准

- 刷新后仍能准确知道哪条链可继续
- refresh / session switch 前后时间轴与可恢复状态一致

---

## 9. 阶段五：统一 live 投影与前端渲染

### 9.1 目标

彻底解决：

- 顺序错乱
- 卡片挂底部
- refresh 后内容丢失
- 主线 / Worker 串台

### 9.2 重点改造文件

- `src/ui/webview-svelte/src/stores/messages.svelte.ts`
- `src/ui/webview-svelte/src/lib/message-handler.ts`
- `src/ui/webview-svelte/src/lib/timeline-render-items.ts`
- `src/ui/webview-svelte/src/lib/worker-lifecycle-card.ts`
- `src/ui/webview-svelte/src/components/MessageList.svelte`
- `src/ui/webview-svelte/src/components/MessageItem.svelte`
- `src/ui/webview-svelte/src/components/SubTaskSummaryCard.svelte`

### 9.3 核心改法

#### A. 前端只消费投影结果

前端不再自己推断：

- 该消息是不是应该挂到底部
- 该 Worker 卡片是不是该并到旧卡片
- 该节点应该归主线还是归 Worker

这些都必须在后端 / 投影层确定好。

#### B. live 和 restore 共用同一模型

要求：

- nodeId 相同
- branchId 相同
- assignmentGroupId 相同
- render order 相同

#### C. thinking / tool / worker 节点全部统一进时间轴

线程面板只渲染“用户可见时间轴节点”：

- 模型文本
- thinking
- 工具
- Worker
- 主线汇总

toast、诊断、控制消息不进入主时间轴。

#### D. 消息状态模型必须同步收敛

在统一 live / restore 投影时，必须同步把消息状态模型收敛到单一语义源：

- `type` 只负责渲染形态
- `lifecycle` 只负责消息执行终态
- `tool_call.status` 只负责工具执行终态
- `metadata.error` / `tool_call.error` 只负责错误详情
- `isStatusMessage` 不再参与消息类型分类、通知分类、system role 判定

不允许继续保留“同一条消息是否失败 / 是否通知 / 是否只是状态消息，需要多个字段联合猜测”的实现。

### 9.4 阶段五完成标准

- 每个节点首次落位后位置稳定
- 后续只原位更新
- live 与 restore 一致
- live / restore 使用同一套消息状态判定规则，不再分别猜测 notice / error / status 语义

---

## 10. 阶段六：清理旧实现与补齐真实 e2e

### 10.1 目标

从架构上完成收口，而不是留下“新实现跑大部分、旧实现兜小概率”的技术债。

### 10.2 必须删除 / 收敛的旧逻辑

- `pendingRecoveryContext` 临时恢复路径
- `continueTask` 直接 `executeTask`
- `recoverRunningState()` 中“执行态收敛为 cancelled”的旧语义
- 任何仅存在于前端 store 的恢复判断
- 任何仅存在于内存的 Worker 恢复唯一真相源
- `type` / `noticeType` / `metadata.isStatusMessage` 共同分摊消息语义的旧实现
- `subTaskCard.status` / `wait_status` / `waitResult.results[].status` / `workerRuntime.status` 并列抢占卡片主状态的旧实现
- `cancelled` / `paused` / `interrupted` / `aborted` / `killed` 在不同层并行表达主状态的旧实现（`stopped` 已删除）

### 10.3 e2e 验证矩阵

必须覆盖真实 LLM 端到端：

1. 主线执行中，用户停止，再继续
2. 主线 + 多 Worker 并行执行中，用户停止，再继续
3. 同一 Worker 在一个 assignmentGroup 内连续串行多个任务
4. 同一轮对话内，新的 Worker 分配生成新卡片
5. 停止后刷新页面，再继续
6. 停止后切换 session，再切回继续
7. 后端重启后，链路收敛为 `interrupted` 再继续
8. 用户选择放弃后，再输入“继续”，不得命中旧链
9. Worker 任务总结超长时：
   - 卡片固定最大展示长度
   - 超出显示 `...`
   - 点击查看更多弹出完整内容
10. 主线总结不得显示到 Worker 卡片中

### 10.4 阶段六完成标准

- 旧路径已删除
- 真实 e2e 全通过
- 不再需要“补一个前端排序判断”这类临时修复

## 11. 推荐开工顺序

为了避免返工，顺序必须这样：

1. 先做执行链真相源
2. 再做停止 / 继续状态机切换
3. 再做 Worker 分支与 assignmentGroup
4. 再升级 session 持久化与 restore
5. 最后收口前端 live / restore 同构渲染
6. 收尾时删除旧路径并做真实 e2e

不能倒过来先修前端展示。

原因很简单：

- 如果真相源没改，前端修得再漂亮，也是在消费错误语义
- 如果 session 持久化没改，刷新后仍会丢恢复能力
- 如果 Worker 分组主语没改，卡片仍会串台

## 12. 预计风险点

### 12.1 风险一：Mission / Plan / ExecutionChain 三层状态打架

控制方式：

- 明确 `execution_chain` 才是停止 / 继续的唯一真相源
- `mission` 保留业务任务投影职责
- `plan_ledger` 保留细粒度执行态职责

### 12.2 风险二：Worker 恢复快照粒度不足

控制方式：

- 不要求恢复原 token 流
- 但必须至少恢复：
  - 当前 todo
  - 已完成 todo
  - worker session 摘要
  - 最近文件上下文摘要

### 12.3 风险三：live / restore 再次出现两套投影规则

控制方式：

- 抽出统一投影契约
- 渲染层只吃投影结果

### 12.4 风险四：阶段推进中产生临时双轨

控制方式：

- 每阶段结束必须删旧路径
- 不允许加长生命周期 feature flag
- 不允许“先兼容旧 session，再慢慢迁移”的长期分支

## 13. 推荐最终交付物

本次重构结束时，至少应交付：

1. 新的执行链 / 恢复快照模型
2. session.json 新持久化结构
3. 停止 / 继续 / 放弃唯一语义路径
4. Worker assignmentGroup 分组模型
5. live / restore 同构时间轴投影
6. 完整真实 e2e 场景集
7. 删除旧恢复路径后的干净代码树

## 14. 最终判断

这次改造的工作量是偏大的，但它是必要的大，而且边界已经很清楚。

如果按这份方案推进，收益是一次性解决：

- 停止 / 继续语义混乱
- 主线 / Worker 串台
- Worker 卡片分组错误
- 刷新 / 切换 session 后恢复不一致
- live 与 restore 两套规则

如果不按这份方案推进，而是继续局部补丁，那么这些问题会继续交替复发，且每修一处都会伤到另一处。

因此，推荐结论只有一个：

- 以执行链为核心，按 6 个阶段做一次架构收敛式重构
- 每阶段完成即删除对应旧路径
- 最后用真实 LLM e2e 做最终验收
