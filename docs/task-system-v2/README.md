# Task System v2 设计文档

更新时间：2026-05-17
状态：单方案设计基线

## 目标

Task System v2 的目标不是把所有对话都包装成任务，也不是为了多代理而多代理。v2 要把 Magi 从“一轮模型对话 + 工具调用”升级为“可持续推进复杂工作的执行系统”。

一句话定义：

> 将用户的复杂目标稳定推进为有状态、有拓扑、有恢复点、有验收证据的交付闭环。

因此，v2 的核心产出是：

- 简单任务低摩擦完成，不被任务系统拖慢。
- 中等任务进入结构化执行链，能展示进度、拆分子任务、回收结果并完成验证。
- 复杂任务进入 Mission，能跨 turn、跨 context、跨进程恢复，并在关键节点强制验证与人审。

## 唯一设计

v2 只采用一套业务模型：

```text
Session
  用户交互入口与 UI 容器

ExecutionChain
  当前可恢复执行链，连接 UI、任务树、worker lane 与恢复信息

Mission
  复杂目标的长期业务边界，持有 Charter / Plan / KG / Validation / Checkpoint / HumanCheckpoint

Task
  可调度、可展示、可取消、可验收的业务工作单元

Conversation
  单个 Task 的运行时容器，负责 Turn / Mailbox / 模型调用边界

Thread
  单个 worker/task 的持久化消息历史，防止不同 worker 上下文串线
```

关键约定：

- `Task` 是业务调度节点，`SpawnGraph` 使用 `TaskId` 建图。
- `Conversation` 是运行时容器，不作为业务拓扑根。
- `ExecutionChain` 是一次任务化执行的恢复入口，不替代 `Task`。
- `Mission` 只在复杂任务中激活，不污染简单任务和普通工具执行。

## 阅读顺序

1. [`01-architecture.md`](./01-architecture.md) — 唯一业务模型、简单/中等/复杂任务推进方式、核心对象与不变式。
2. [`02-migration-plan.md`](./02-migration-plan.md) — 按当前唯一设计推进实现收敛的落地顺序。
3. [`03-open-questions.md`](./03-open-questions.md) — 已定规则、剩余实现风险与验收标准。

## 设计原则

- **任务越简单，系统越隐形**：普通问答和一次性工具调用不创建 TaskGraph。
- **任务越复杂，约束越硬**：复杂任务必须有 Plan、Validation、Checkpoint 和 HumanCheckpoint。
- **Task 是业务事实源**：状态、进度、父子关系、取消、回执、验收都落在 Task/TaskGraph 上。
- **Conversation 只承载运行时**：Turn/Mailbox/模型工具循环属于 Conversation，不承担业务身份。
- **Prompt 负责协调，Runtime 负责边界**：Coordinator 可以决定怎么拆解，但不能绕过权限、验证、人审和恢复约束。
- **一个设计，一个主线**：文档与实现只保留这一套推进模型，不保留兼容路线或双实现叙事。
