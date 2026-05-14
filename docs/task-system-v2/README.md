# Task System v2 设计文档

更新时间：2026-05-14
状态：设计稿（落地基线）
取代：[`../task-orchestration-upgrade/`](../task-orchestration-upgrade/) 中 Mission/Task/Worker 三元结构

## 为什么有 v2

v1 设计基于 Mission + Task Graph + Worker Binding 三元模型，落地后暴露三个结构问题：

1. **Task 对象压成三种角色**：规划单元、执行单元、对话载体被压在同一个 `Task` 上，导致 `TaskKind` 膨胀到 7 种、`TaskStatus` 膨胀到 11 种，状态机不收敛。
2. **缺少运行时输入通道**：对话进行中无法把用户消息、子代理结果、决策回执注入到正在跑的 turn，"guide" 按钮写入 `root_task.context_refs` 但 `task_llm_loop` 从未消费，等于一个 dead path。
3. **单一任务粒度无法覆盖三档负载**：A 档（codex 式一次对话一个 task）、B 档（多代理 5-30 min loop）、C 档（数日 Java→Python 重构）共用一套 schema，导致短任务被长任务的元数据拖累、长任务又缺少 Charter/Plan/Checkpoint 等关键抽象。

v2 不是 v1 的增量补丁，是把任务系统重新分层：

- **Runtime 层**（来自 codex）：Conversation / Turn / Mailbox / AgentRole / SpawnGraph，覆盖所有任务的执行基础。
- **Orchestration 层**（来自 claude-code）：Coordinator Prompt / Task Polymorphism / Memory / Scratchpad，覆盖 B/C 档的多代理协作。
- **Long-Mission 层**（C 档新增）：Charter / Plan / Workspace / KnowledgeGraph / ValidationRunner / Checkpoint / HumanCheckpoint。

## 阅读顺序

1. [`01-architecture.md`](./01-architecture.md) — 整体架构、4 Tier × 21 Layer、任务分档、核心模型 ER 与关键时序
2. [`02-migration-plan.md`](./02-migration-plan.md) — **单次彻底切换**路径（分支隔离 + Cutover Day），不留新旧双轨
3. [`03-open-questions.md`](./03-open-questions.md) — 未决问题与风险评估

## 与现有文档关系

| 文档 | 关系 |
|------|------|
| [`rust-backend-rewrite/`](../rust-backend-rewrite/) | 协议冻结与契约边界仍然有效，v2 在其之上重排任务运行时 |
| [`task-orchestration-upgrade/`](../task-orchestration-upgrade/) | v1，**已废弃**，v2 完成后整体移除 |
| [`canonical-turn-log-refactor-plan.md`](../canonical-turn-log-refactor-plan.md) | Turn 日志重构与 v2 的 L2 Turn 层强相关，v2 落地前先完成此重构 |
| [`p7-compliance-collapse.md`](../p7-compliance-collapse.md) | P7 单信号契约是 v2 的前置条件，必须先收敛完成 |

## 设计原则

- **分层激活而非分系统**：A/B/C 档使用同一套底层 primitives，只是激活的层数不同，不分叉为三个独立系统
- **运行时输入通道一等公民**：Mailbox 是 v2 与 v1 最大的结构差异，所有外部信号（用户输入、子代理结果、决策回执、引导）统一从 Mailbox 入栈
- **协调由 Prompt 完成，不由代码**：claude-code 的 Coordinator 不是一个特殊模式，而是给主代理喂一份特定 system prompt，所有协调动作转化为常规工具调用
- **C 档不污染 A 档**：Charter / Plan / Workspace / KG 仅在 C 档激活，A 档代码路径里看不见它们
- **单次彻底切换，不留双轨**：v2 在隔离分支上完整开发，Cutover Day 一次性合并 + 整批删除老代码。主干永远是单实现：切换前 100% v1、切换后 100% v2，中间没有混合状态
