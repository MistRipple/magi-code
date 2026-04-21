# Agent 任务单：magi-knowledge-store 项目知识库

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-knowledge-store` 项目知识库任务单
- 编号：`WP-KNOWLEDGE-001`
- 负责 Agent：Knowledge Agent

## 2. 写域

- 唯一写域：`crates/magi-knowledge-store`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - context runtime
  - memory store
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：代码索引、ADR、FAQ、learning、governed knowledge query
- 当前实现位置：
  - `src/knowledge/project-knowledge-base.ts`
  - `src/knowledge/governed-knowledge-context-service.ts`
- 当前问题：
  - 知识索引、存储、查询、输出格式混装
  - 项目知识库职责过重

## 4. 根本原因

1. PKB 在历史演进中承担了太多不同层级职责
2. index / store / query / governed output 没被拆开
3. 如果不重画边界，Rust 侧会复制同样的超大知识对象

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-knowledge-store`
  - 拆分 indexer、store、query service、governed output
- 本任务不做什么：
  - 不做 context budget
  - 不做 memory persistence
- 与其他 Agent 的边界：
  - knowledge store 只负责知识真相
  - context runtime 只读消费

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-knowledge-store`
  - `indexer`
  - `storage`
  - `query`
  - `governed_output`
- 新增 schema：
  - 如 knowledge API 需要冻结，先补 `schema/api`
- 更新文档：
  - 回写 `D-008`、`D-009`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止在 Rust 侧继续形成全能 PKB 对象

## 7. 语义约束

- 本任务涉及的真相源：
  - ADR / FAQ / learning
  - code index
  - governed knowledge output
- 是否涉及协议变化：
  - 可能影响 knowledge API
- 是否涉及语义偏差台账登记：
  - 是，必须对齐 `D-008`、`D-009`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- index / store / query / output 分层必须清晰
- 不允许把 context / memory 逻辑重新塞回 knowledge crate

## 9. 验收标准

- 编译：
  - `magi-knowledge-store` 可独立编译
- 最小运行验证：
  - ADR / FAQ / learning 存取与查询可用
  - code index 可建立
- 协议验证：
  - knowledge 输出结构可稳定描述
- 清理验证：
  - crate 内无 context budget 与 memory persistence 混装

## 10. 输出结论

- 已完成内容：
  - 已建立 knowledge record、kind 和基础 store
  - 已支持基础 upsert / list 骨架
  - 已补 `KnowledgeIndexer`，支持 title/content/tags 的索引词项构建
  - 已补 `KnowledgeQueryService`，支持 kind/text/tags 查询、匹配评分与 `matched_terms`
  - 已补 `GovernedKnowledgeService`，支持 excerpt、score、matched_terms、source_ref 的治理后输出
  - 已补 `CodeIndexIngestion / CodeIndexSource / KnowledgeAuditLink / KnowledgeGovernanceLink`，可让 code index sidecar 与治理审计直接进入 query / governed output
  - 已补 orchestrator / runtime read model 级消费验证，证明 `code_source / audit_link / governance_link` 不只停在 context assembly
  - 已补确定性排序，保证列表输出稳定
- 删除内容：
  - 无
- 未完成边界：
  - 尚未接入真实仓库扫描与更广消费者
- 后续依赖：
  - `magi-context-runtime`
  - `magi-orchestrator`
  - `magi-api`
