# Agent 任务单：magi-context-runtime 上下文组装内核

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-context-runtime` 上下文组装内核任务单
- 编号：`WP-CONTEXT-001`
- 负责 Agent：Context Agent

## 2. 写域

- 唯一写域：`crates/magi-context-runtime`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - memory store 持久化实现
  - knowledge store 存储实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：context assembly、token budget、shared context、file summary、recent turns
- 当前实现位置：
  - `src/context/context-manager.ts`
  - `src/context/context-assembler.ts`
  - `src/context/file-summary-cache.ts`
  - `src/context/shared-context-pool.ts`
- 当前问题：
  - context runtime 与 memory / knowledge 仍然交织
  - context manager 承担初始化、装配、缓存、记忆桥接等多重职责

## 4. 根本原因

1. context 系统长期承接“所有与提示词相关的东西”
2. budget、assembly、cache、memory bridge 没被完全拆分
3. 如果不独立 context runtime，后续长任务稳定性与结构清晰度都难保证

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-context-runtime`
  - 收口 budget、assembler、shared context、file summary、recent turns
- 本任务不做什么：
  - 不做 memory persistence
  - 不做 knowledge store 存储
  - 不做 LLM provider 调用
- 与其他 Agent 的边界：
  - context runtime 只消费 memory / knowledge / session / workspace 提供的只读数据

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-context-runtime`
  - `budget`
  - `assembler`
  - `shared_context`
  - `file_summary`
  - `recent_turns`
- 新增 schema：
  - 无，当前先做内部运行模型
- 更新文档：
  - 回写 `D-008`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止继续形成 ContextManager 式超级上下文对象

## 7. 语义约束

- 本任务涉及的真相源：
  - token budget
  - context parts
  - shared context / file summary 运行时视图
- 是否涉及协议变化：
  - 否，当前先立内部边界
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-008`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- context runtime 只做组装，不做持久化真相源
- budget / assembler / cache / memory bridge 必须清晰分层

## 9. 验收标准

- 编译：
  - `magi-context-runtime` 可独立编译
- 最小运行验证：
  - budget 计算、context assembly、file summary / shared context 可用
- 协议验证：
  - 无直接外部协议要求
- 清理验证：
  - crate 内无 knowledge store / memory store 持久化混装

## 10. 输出结论

- 已完成内容：
  - 已建立 context budget、assembly input / result
  - 已建立 knowledge / memory 输入到 context 的最小组装骨架
  - knowledge 路径已改为直接消费 `magi-knowledge-store` 的 `governed_output`
  - knowledge 截断统计已改为直接使用 `KnowledgeQueryResult.total_matches`，不再在 context runtime 内重复查询和重复投影
  - 已补 `SharedContextPool` 与 `FileSummaryStore`
  - 已补 `assemble_from_runtime_sources(...)`，shared context / file summary 已可从运行时来源读取
  - 已补 `ProjectRecentTurnStore`
  - 已补 `RecentTurnsSourceQuery`，recent turns 已支持 session timeline 与 project 两级来源
  - 已补 recent turns 的双路限额、去重治理、显式来源优先级（session > project > provided）与结构化 recent turn 结果
- 删除内容：
  - 无
- 未完成边界：
  - 无
- 后续依赖：
  - `magi-memory-store`
  - `magi-knowledge-store`
  - `magi-session-store`
