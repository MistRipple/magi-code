# 执行链中断 / 恢复架构设计

更新时间：2026-03-23

## 1. 文档定位

本文档定义当前项目在“停止 / 继续 / 恢复 / 放弃 / 补充”场景下的标准产品语义、运行时状态机、持久化模型与前后端链路契约。

本文档的用途只有一个：作为后续重构的唯一基线。

明确约束如下：

- 不接受“停止到底算取消还是暂停”这种模糊语义。
- 不接受前后端各自猜测恢复逻辑。
- 不接受补丁式兼容分支。
- 不接受 live 展示和 restore 恢复两套不同规则。

## 2. 问题背景

用户的真实目标不是“让模型把一句话重新说一遍”，而是：

- 当主线正在工作时，用户可以主动停止。
- 停止后，用户发送“继续”或点击继续，系统应尽量从原执行现场续上。
- 这个“续上”既要覆盖主线，也要覆盖 Worker 分支。
- 不要求 token 级原地续流，但要尽量避免重新立项、重新规划、重新全量读代码。
- 主线、Worker、工具卡片、thinking 卡片都应服从统一时间轴与统一恢复规则。

因此，正确目标不是“把取消包装成恢复”，而是建立一套真正可恢复的执行链架构。

## 3. 当前实现现状

### 3.1 已确认的代码事实

1. 当前“停止”是实际取消，不是暂停。
   - `src/ui/webview-provider.ts` 的 `interruptCurrentTask()` 会中断 orchestrator、adapter 和运行中的任务。
   - `src/orchestrator/core/mission-driven-engine.ts` 的 `cancel()` 会取消 active batch，并把未完成 Worker 任务标记为 `cancelled`。

2. 当前普通“继续”不是恢复，而是新开一轮执行。
   - `src/ui/webview-provider.ts` 中 `continueTask` 直接调用 `executeTask(message.prompt)`。

3. 当前只有显式恢复入口才会进入恢复链。
   - `src/ui/webview-provider.ts` 中 `resumeInterruptedTask()` 会构造新的 resume prompt，再调用 `executeTask(...)`。

4. 当前启动恢复会把执行中任务收敛为 `cancelled`，不会保留 recoverable 语义。
   - `src/services/task-view-service.ts` 的 `recoverRunningState()` 会把 `executing` mission 转成 `cancelled`，并取消 `running` todo。

5. 当前 recoverable 上下文是局部内存态，不是稳定持久化态。
   - `pendingRecoveryContext` 只有在 `result.recoverable` 时才会被写入。
   - Worker 侧已有 session resume / dispatch resume context，但本质上仍偏运行时内存能力。

6. 当前前端恢复仅恢复历史展示，不恢复运行态。
   - 会话 restore 主要恢复历史消息、timeline projection 与展示所需状态，不继承正在运行中的执行现场。

### 3.2 当前实现与目标之间的核心差距

当前系统的真实语义是：

- 停止 = 取消
- 普通继续 = 新请求
- 显式恢复 = 基于旧上下文重新补跑

而目标语义应该是：

- 停止 = 进入可恢复中断态
- 继续 = 恢复最近一次被中断的执行链
- 放弃 = 显式终止且不可恢复

也就是说，当前差距不是“少写了一个按钮”，而是执行链真相源缺失。

## 4. 设计原则

### 4.1 原则一：以执行链为第一实体

恢复能力不能继续挂靠在零散的 `mission`、`todo`、`pendingRecoveryContext` 或某个按钮动作上。

系统必须引入明确的“执行链（Execution Chain）”概念：

- 一次用户输入触发的一整轮主线执行，是一条执行链。
- 该执行链内部可以派生多个 Worker 分支。
- 该执行链可以跨多次“停止 / 继续”持续存在。
- 恢复的对象是执行链，不是某条文案消息。

### 4.2 原则二：停止不等于放弃

用户主动停止的核心诉求通常是：

- 暂停风险
- 修正方向
- 等待补充信息
- 临时打断当前输出

这不应被系统直接解释成“放弃整个任务”。

### 4.3 原则三：继续是恢复，不是再开一轮

“继续”必须绑定最近一次可恢复的执行链，并沿用该执行链的：

- chainId
- 主线上下文
- Worker branch
- assignment group
- todo / dispatch / session 信息
- 时间轴归属

### 4.4 原则四：恢复基于安全检查点，而不是 token 级续流

目标必须明确为：

- 从最后一个安全检查点继续

而不是：

- 从上次 LLM token 中断位置继续吐同一条流

原因很简单：

- 大模型 API 天然不保证可原位续流。
- 工具调用和外部副作用可能已部分发生。
- Shell、浏览器、远程工具、子进程的运行态并不天然可恢复。

因此，正确做法是持久化“可恢复执行现场”，而不是幻想恢复“网络流”本身。

### 4.5 原则五：live 与 restore 必须同构

后端持久化、后端恢复投影、前端 live 投影、前端渲染必须共享一套模型：

- 同一类节点
- 同一套 id
- 同一套排序
- 同一套主线 / Worker 归属

不能一套逻辑给 live 用，另一套逻辑给 restore 猜。

### 4.6 原则六：不做旧版兼容式回退

后续重构应直接切到新模型：

- 不再保留“停止即取消”的旧语义分支
- 不再保留“普通继续直接 executeTask”的旧语义
- 不再依赖临时的 `pendingRecoveryContext` 作为恢复真相源

## 5. 外部参考调研：Augment

本次额外解包并分析了 `augment.vscode-augment-0.789.1.vsix`，结论如下。

### 5.1 Augment 的真实处理方式

1. 它的对话主模型是“单次 assistant exchange 容器”。
   - `AugmentMessage.svelte` 中，单条 assistant turn 内部依次渲染：
     - thinking
     - markdown 文本
     - tool uses
     - stop hook messages

2. 它的 tool / subagent 不是独立全局时间轴节点，而是挂在当前 assistant turn 下面的结构化子块。
   - `ToolUses.svelte` / `ToolUsesStaging.svelte` 从 `structured_output_nodes` 中筛出 `TOOL_USE` 节点后渲染。

3. 它的 subagent 展示是嵌套式的，而不是主线 / 分支统一时间轴。
   - `SubAgentToolComponent.svelte` 在 tool 卡片内部读取 subagent conversation：
     - 运行中显示最近两条 exchange 预览
     - 完成后显示最后一条 exchange 详情

4. 它有独立的持久化切面：
   - `HistoryWebviewMessageHandler` 持久化 conversation history
   - `ToolUseStateWebviewMessageHandler` 持久化 tool use state

5. 它有 checkpoint 能力，但重点是文件 / blob / edit context，不是执行链恢复。
   - `AggregateCheckpointManager` 主要管理 conversation 维度的文件检查点
   - `StreamManager` 只负责取消 stream，没有完整的“停止后继续”执行链恢复模型

### 5.2 对我们的可借鉴点

可以借鉴的点：

- thinking 作为用户可见结构化块展示
- 同一 turn 下对多个工具做分组展示
- Worker / Subagent 卡片内部使用可折叠内容和“查看更多”交互
- 历史与工具状态分层持久化的思想

不能直接照搬的点：

- 它不是主线 / Worker 统一时间轴架构
- 它的工具与 subagent 都依附在 assistant exchange 内部
- 它没有提供我们目标中的“停止后继续原执行链”完整模型

### 5.3 结论

Augment 可以作为局部交互与状态切分的参考，但不能作为本项目目标架构的模板。

本项目要坚持：

- 时间轴优先
- 执行链优先
- 主线 / Worker 分支并存

而不是退回到“把所有东西都塞进一条 assistant 消息里”。

## 6. 核心对象模型

### 6.1 Execution Chain

执行链是恢复能力的第一真相源。

建议新增持久化实体：

```ts
type ExecutionChainStatus =
  | 'running'
  | 'paused'
  | 'interrupted'
  | 'resuming'
  | 'completed'
  | 'failed'
  | 'cancelled';

interface ExecutionChainRecord {
  id: string;
  sessionId: string;
  userMessageId: string;
  requestId: string;
  status: ExecutionChainStatus;
  attempt: number;
  currentMissionId?: string;
  currentPlanId?: string;
  activeAssignmentGroupId?: string;
  latestSnapshotId?: string;
  interruptedReason?: 'user_stop' | 'process_exit' | 'extension_reload' | 'external_abort';
  recoverable: boolean;
  createdAt: number;
  updatedAt: number;
}
```

### 6.2 Branch

执行链内部允许多个分支：

- `mainline`
- `worker`

```ts
type BranchKind = 'mainline' | 'worker';

interface BranchRecord {
  id: string;
  chainId: string;
  kind: BranchKind;
  parentBranchId?: string;
  workerSlot?: string;
  assignmentGroupId?: string;
  status: ExecutionChainStatus;
  createdAt: number;
  updatedAt: number;
}
```

### 6.3 Assignment Group

这是 Worker 卡片分组的核心单位。

定义：

- 一次主线分配 Worker 的动作，形成一个 `assignmentGroup`
- 同一 `assignmentGroup` 内，同一 `workerSlot` 的串行任务合并到一个 Worker 卡片
- 一旦该 `assignmentGroup` 完成，下一次新的 Worker 分配即使仍然使用同一 Worker，也必须生成新卡片

这正是用户要求的分组语义：

- 分组单位不是“整个对话轮次”
- 也不是“整个 session”
- 而是“某一次 Worker 分配轮次”

### 6.4 Resume Snapshot

恢复不应直接读取散落状态，而应读取标准快照。

```ts
interface ResumeSnapshot {
  id: string;
  chainId: string;
  attempt: number;
  checkpointSeq: number;
  mainline: {
    currentMissionId?: string;
    currentPlanId?: string;
    runtimePhase?: string;
    pendingSupplementaryInputs: string[];
    contextDigest: string[];
  };
  dispatch: {
    assignmentGroupId?: string;
    pendingTaskIds: string[];
    runningTaskIds: string[];
    completedTaskIds: string[];
  };
  workerBranches: Array<{
    branchId: string;
    workerSlot: string;
    assignmentGroupId: string;
    workerSessionId?: string;
    currentTodoId?: string;
    completedTodoIds: string[];
    pendingTodoIds: string[];
    contextDigest: string[];
    latestSummary?: string;
  }>;
  workspace: {
    dirtyFiles: string[];
    pendingChangesSummary: string[];
  };
  timelineCursor: {
    lastVisibleNodeSeq: number;
  };
  createdAt: number;
}
```

## 7. 标准产品语义

### 7.1 补充

定义：

- 当执行链处于 `running` 或 `paused` 时，用户追加一条指令，不新开执行链，而是追加到当前执行链中。

效果：

- 不改变 chainId
- 不新建主线 turn
- 进入主线补充队列，或按目标 Worker 路由到对应分支

### 7.2 停止

定义：

- 用户主动要求当前执行链停止继续推进，并保留后续恢复资格。

效果：

- 中断主线与 Worker 的 live 执行
- 生成最终可恢复快照
- 执行链状态进入 `interrupted`
- 保留 `recoverable = true`

注意：

- 停止后不应把任务写成 `cancelled`
- 停止后不应丢失 branch / assignment group / timeline 归属

### 7.3 继续

定义：

- 用户要求从最近一次可恢复中断点继续当前执行链。

效果：

- 定位最近一条 `interrupted && recoverable` 的执行链
- 读取 `latestSnapshotId`
- 以同一 chainId 恢复主线与 Worker 分支
- `attempt + 1`
- 状态进入 `resuming`，之后回到 `running`

### 7.4 恢复

恢复是系统能力，不一定是单独暴露给用户的产品词。

本文档中：

- 用户动作叫“继续”
- 系统过程叫“恢复（resume）”

### 7.5 放弃

定义：

- 用户明确放弃当前执行链及其恢复资格。

效果：

- 执行链状态进入 `cancelled`
- `recoverable = false`
- 后续任何“继续”都不应再命中该链

## 8. 标准状态机

### 8.1 状态定义

建议执行链标准状态如下：

- `running`
- `paused`
- `interrupted`
- `resuming`
- `completed`
- `failed`
- `cancelled`

### 8.2 状态语义边界

`paused`：

- 系统或治理要求暂不推进
- 运行逻辑仍可视为同一轮内部等待
- 不等于任务被拆散

`interrupted`：

- live 执行已被切断
- 需要借助快照重新进入执行链
- 但仍然 recoverable

`cancelled`：

- 人工放弃或明确不可恢复
- 不允许再继续

### 8.3 标准流转

```text
running -> paused -> running
running -> interrupted -> resuming -> running
running -> completed
running -> failed
running -> cancelled

paused -> interrupted
paused -> cancelled

interrupted -> resuming
interrupted -> cancelled

resuming -> running
resuming -> failed
resuming -> interrupted
```

### 8.4 启动恢复 / 进程崩溃后的收敛规则

若插件重启、后端重启或进程异常退出时存在 `running` / `resuming` 链：

- 若存在完整且可用的 `ResumeSnapshot`
  - 收敛为 `interrupted`
  - `recoverable = true`
- 若不存在可用快照
  - 收敛为 `failed`
  - `recoverable = false`

绝不能无条件收敛为 `cancelled`。

## 9. 主线恢复语义

主线恢复的标准目标是：

- 不重新立项
- 不重新生成新的 chainId
- 不重新丢失 mission / plan / runtime phase
- 不重新全量读一遍上下文

主线恢复至少应复用：

- 原用户请求
- 已完成的计划与阶段状态
- 已生成的主线总结
- 已完成的 Worker 结果
- 未消费的补充指令
- 主线上下文摘要

主线恢复不应复用：

- 已断开的模型网络流
- 未持久化的临时 token
- 无法保证正确性的半完成外部副作用

## 10. Worker 恢复语义

### 10.1 Worker 恢复目标

Worker 恢复必须做到：

- 继续沿用原 branchId
- 继续沿用原 `assignmentGroupId`
- 尽量沿用原 `workerSessionId`
- 恢复已完成 / 未完成 todo 状态
- 尽量复用已读文件、上下文摘要和阶段结论

### 10.2 Worker 卡片分组规则

必须固定为：

- `workerCardKey = assignmentGroupId + workerSlot`

这意味着：

1. 同一轮 Worker 分配里，同一 Worker 多次串行处理任务：
   - 合并到同一 Worker 卡片

2. 同一对话轮内，如果主线后来又发起了新一轮 Worker 分配：
   - 即使还是同一 Worker，也必须创建新 Worker 卡片

3. Worker 卡片完成后，不允许下一轮新分配继续往旧卡片里塞

### 10.3 主线与 Worker 的展示边界

必须明确：

- Worker 内部输出只属于对应 Worker branch
- 主线总结只属于主线
- 若 Worker 结果需要提升到主线，只能通过“主线汇总节点”显式落位
- 不允许直接把 Worker 输出流拿去填充主线消息

## 11. 快照策略

### 11.1 快照写入时机

建议在以下安全边界持久化快照：

- 主线进入新阶段前后
- dispatch batch 创建后
- assignment group 状态变化后
- Worker 任务完成后
- 工具调用完成后
- 用户点击停止并完成 quiesce 后

### 11.2 停止时的标准流程

```text
用户点击停止
-> 控制层将 chain 标记为 interrupting（瞬态）
-> 中断主线与 Worker live 执行
-> 等待 quiesce / 超时收口
-> 写入最终 ResumeSnapshot
-> 将 chain 状态写为 interrupted
-> 写入用户可见时间轴节点：已停止，可继续 / 放弃
```

### 11.3 继续时的标准流程

```text
用户点击继续 / 系统识别继续意图
-> 解析最近一条 recoverable interrupted chain
-> 读取 latestSnapshotId
-> 恢复 mainline runtime
-> 恢复 worker branch runtime
-> 生成新的 attempt
-> chain 状态写为 resuming
-> 时间轴写入“恢复中”节点
-> 进入 running
```

## 12. 持久化设计

### 12.1 session.json 必须是完整恢复基座

`.magi/sessions/session-xxxxx/session.json` 必须不仅能恢复“历史展示”，还要能恢复“可恢复执行态描述”。

建议结构：

```ts
interface PersistedSessionRecord {
  id: string;
  messages: unknown[];
  timelineProjection: unknown;
  executionChains: ExecutionChainRecord[];
  resumeSnapshots: ResumeSnapshot[];
  notifications: unknown;
  updatedAt: number;
}
```

### 12.2 持久化职责划分

`timelineProjection` 负责：

- 用户可见时间轴节点
- 稳定排序
- 稳定归属

`executionChains` 负责：

- 哪条执行链正在运行 / 被中断 / 已完成
- 哪条链可恢复
- 链与用户 turn 的绑定关系

`resumeSnapshots` 负责：

- 恢复现场
- 主线 / Worker 分支恢复所需的最小充分信息

### 12.3 不应持久化的内容

不应试图持久化：

- LLM 隐式思维原始 token 流
- 未完成的网络连接
- 不可验证恢复的外部进程原始句柄

应持久化的是：

- 结构化上下文摘要
- todo / dispatch / branch 状态
- 工具结果摘要
- 文件变更摘要
- 关键运行阶段信息

## 13. 后端持久化 -> restore 投影 -> live 投影 -> 前端渲染

这是整个架构的核心闭环。

### 13.1 后端持久化

后端负责写入：

- timeline event / projection
- execution chain state
- resume snapshot

后端是唯一真相源。

### 13.2 后端 restore 投影

会话加载时：

- 从 `session.json` 读取 timeline + executionChains + resumeSnapshots
- 得到“恢复后的用户可见时间轴”
- 得到“当前是否存在可继续链”

restore 投影不应自己猜测主线 / Worker 关系。

### 13.3 前端 live 投影

运行时前端接收增量事件时：

- 使用与 restore 相同的投影规则
- 使用相同 nodeId / branchId / assignmentGroupId
- 首次落位即固定位置
- 后续只允许原位更新

### 13.4 前端渲染

渲染层只消费已经投影完成的用户可见节点。

渲染层不负责：

- 推断恢复资格
- 推断节点排序
- 推断主线 / Worker 归属

## 14. 时间轴与展示规则

### 14.1 统一时间轴节点

以下都属于用户可见时间轴节点：

- 模型文本
- thinking 卡片
- 工具卡片
- Worker 卡片
- 主线汇总卡片

以下不属于用户可见时间轴节点：

- toast
- 诊断消息
- 内部控制消息
- 纯技术态提示

### 14.2 稳定落位规则

必须满足：

- 每个节点第一次落位后，位置固定
- 只允许原位更新，不允许先挂底部再挪动
- 排序依据是语义发生顺序，不是完成时间

### 14.3 主线 / Worker branch 规则

必须满足：

- 主线在主线 lane 上连续输出
- Worker 在各自 branch lane 上连续输出
- Worker 预览、Worker 总结、Worker 任务列表都属于该 Worker branch
- 主线汇总若需要引用 Worker 结果，必须新增主线节点，不得挪用 Worker 节点

### 14.4 Worker 总结展示规则

建议规则：

- 卡片内默认展示固定最大长度摘要
- 超出部分以 `...` 结尾
- 提供“查看更多”或等效交互
- 点击后在当前上下文以悬浮层展示完整内容
- 不跳转到 Worker 会话

## 15. 自然语言“继续”的判定规则

系统应区分三种情况：

1. 明确 UI 点击“继续”
   - 直接恢复最近可恢复执行链

2. 用户文本只说“继续 / 接着做 / 继续刚才的任务”
   - 若当前 session 只有一条可恢复执行链，则直接命中该链

3. 用户文本有新目标且无法明确判定为恢复
   - 视为新执行链

当存在多条可恢复链且自然语言不明确时：

- 不应静默猜测跨链恢复
- 应以明确的当前链 / 最近链规则约束，或由 UI 明确提供恢复入口

## 16. 不可恢复边界

以下场景应视为不可恢复或仅部分恢复：

- 用户显式放弃
- 恢复快照缺失或损坏
- 关键 Worker session 无法重建且没有足够摘要
- 外部副作用已进入不一致状态且无法校验

处理原则：

- 优先进入 `failed`
- 明确说明不可恢复原因
- 不要伪装成成功继续

## 17. 重构落地建议

### 17.1 第一阶段：建立真相源

- 新增 `ExecutionChainRecord`
- 新增 `ResumeSnapshot`
- 停止 / 继续 / 放弃 改为以 execution chain 为中心

### 17.2 第二阶段：重做停止 / 继续链路

- 停止改写为 `interrupted`
- 继续改为读取快照恢复
- 移除普通 continue 直接 `executeTask(prompt)` 的旧语义

### 17.3 第三阶段：Worker 恢复与分组

- 引入 `assignmentGroupId`
- 固化 Worker 卡片分组规则
- 恢复 branch / session / todo 归属

### 17.4 第四阶段：统一投影与渲染

- restore 与 live 共用同一投影器
- 渲染层只消费投影结果
- 清除底部挂载、刷新丢失、归属漂移等问题

### 17.5 第五阶段：端到端验证

至少覆盖：

- 主线执行中停止后继续
- 主线 + 多 Worker 执行中停止后继续
- 同一轮多次 Worker 串行任务合并
- 同一对话轮内第二次 Worker 分配生成新卡片
- 页面刷新后继续
- session 切换后继续
- 后端重启后恢复为 interrupted 再继续
- 显式放弃后不可继续

## 18. 验收标准

必须全部满足：

1. 停止后，当前执行链进入 `interrupted`，而不是 `cancelled`。
2. 点击继续或明确继续意图时，恢复的是原 execution chain，而不是新开一条链。
3. 主线与 Worker 的输出归属稳定，不串台。
4. 同一 Worker 在同一 `assignmentGroupId` 内的串行任务合并到同一卡片。
5. 同一 Worker 在后续新 `assignmentGroupId` 中必须生成新卡片。
6. 所有用户可见节点首次落位后位置稳定。
7. 刷新页面、切换 session 后，时间轴节点数量、顺序、内容与归属保持一致。
8. `session.json` 足以恢复时间轴与可恢复执行态描述。
9. 放弃后的执行链不可继续。
10. 不再保留旧版“停止即取消、继续即新开”的兼容分支。

## 19. 结论

这次改造的本质不是“给停止加一个恢复按钮”，而是把系统从“消息驱动的局部恢复”升级为“执行链驱动的标准恢复”。

真正需要重构的不是某一个函数，而是四层职责边界：

- 执行链真相源
- 恢复快照真相源
- 时间轴投影真相源
- 前端纯渲染层

只有这四层职责清楚，后续的停止 / 继续、主线 / Worker 分支、工具卡片落位、刷新恢复一致性，才会真正稳定。
