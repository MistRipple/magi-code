# Agent 任务单：magi-memory-store 会话记忆与分层记忆

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-memory-store` 会话记忆与分层记忆任务单
- 编号：`WP-MEMORY-001`
- 负责 Agent：Memory Agent

## 2. 写域

- 唯一写域：`crates/magi-memory-store`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - knowledge store
  - context assembly 主流程
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：session memory、layered memory、memory extraction、memory compaction 输出
- 当前实现位置：
  - `src/context/memory-document.ts`
  - `src/context/layered-memory-store.ts`
  - `src/orchestrator/session-memory/session-memory-extraction-service.ts`
- 当前问题：
  - session memory 与 layered memory 边界不够清晰
  - memory persistence 和 context assembly 仍有交织

## 4. 根本原因

1. 记忆能力是在 context 系统中逐步长出来的
2. 存储、抽取、压缩结果、偏好记忆没有彻底分层
3. 如果不先独立 memory store，Rust 侧会继续把记忆逻辑粘在 context runtime 中

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-memory-store`
  - 收口 session memory、layered memory、memory extraction persistence
- 本任务不做什么：
  - 不做 context assembly
  - 不做 PKB 查询
- 与其他 Agent 的边界：
  - memory store 只提供持久化记忆真相
  - context runtime 只读消费

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-memory-store`
  - `session_memory`
  - `layered_memory`
  - `preferences`
  - `extraction_results`
- 新增 schema：
  - 无，当前先做内部模型
- 更新文档：
  - 回写 `D-008`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止把 context assembler 直接塞进 memory crate

## 7. 语义约束

- 本任务涉及的真相源：
  - session memory
  - layered memory
  - preference memory
- 是否涉及协议变化：
  - 否，当前先立内部边界
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-008`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- 记忆存储与上下文组装必须分层
- 不允许把知识库查询和记忆存储继续混装

## 9. 验收标准

- 编译：
  - `magi-memory-store` 可独立编译
- 最小运行验证：
  - session memory / layered memory / preferences 可持久化
- 协议验证：
  - 无直接外部协议要求
- 清理验证：
  - crate 内无 context assembler 与 PKB 查询混装

## 10. 输出结论

- 已完成内容：
  - 已建立 memory layer、memory record 和 session 级查询骨架
  - 已支持基础 append / list_for_session
  - 已补 preference memory，支持按 session 查询偏好项
  - 已补 extraction results，支持记录抽取来源、摘要和产出 memory ids
  - 已补 compaction history，支持保留每次压缩结果的历史记录
  - 已补 `apply_extraction(...) / extraction_linkage(...) / verify_extraction_linkage(...)`，让 extraction 主链与 memory record 建立可校验闭环
  - 已补 orchestrator / runtime read model 级消费验证，证明 extraction provenance 与 extraction refs 可继续进入系统级摘要
  - 已补 `/session/action` shadow 路由级自动 extraction 回写，且同一 session 的后续 dispatch 已能继续消费这条 route 级记忆
  - 已补确定性排序，避免同时间戳与同层级结果出现不稳定输出
- 删除内容：
  - 无
- 未完成边界：
  - extraction 自动回写尚未抽成对所有 execution runtime 调用方统一生效
  - 尚未接入持久化策略
- 后续依赖：
  - `magi-context-runtime`
  - `magi-orchestrator`
