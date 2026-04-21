# Agent 任务单：magi-governance 治理与审批边界

更新时间：2026-04-15

---

## 1. 任务名称

- 名称：`magi-governance` 治理与审批边界任务单
- 编号：`WP-GOV-001`
- 负责 Agent：Governance Agent

## 2. 写域

- 唯一写域：`crates/magi-governance`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - tool runtime 实际执行实现
  - orchestrator / worker 状态机实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：审批、风险、tool policy、sandbox、治理阈值
- 当前实现位置：
  - `src/orchestrator/core/governance-engine.ts`
  - `src/governance/**`
  - `src/tools/tool-policy.ts`
  - `src/tools/shell/sandbox-policy.ts`
- 当前问题：
  - 治理规则散落在 orchestrator、tool manager、shell executor 等多处
  - 风险决策、审批决策、工具策略没有统一边界

## 4. 根本原因

1. 当前治理能力是跟随执行主链逐步挂上的
2. 工具策略与 orchestrator 风险控制没有统一层
3. 如果不单独收口，Rust 侧会继续把策略逻辑塞进 tool / orchestrator 内部

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-governance`
  - 统一风险阈值、approval policy、tool policy、sandbox policy
- 本任务不做什么：
  - 不执行工具
  - 不承载任务状态机
  - 不承载审计账本
- 与其他 Agent 的边界：
  - 只输出策略决策和治理接口
  - 不直接拥有 tool runtime 或 orchestrator 状态

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-governance`
  - `approval`
  - `risk`
  - `tool_policy`
  - `sandbox`
  - `thresholds`
- 新增 schema：
  - 若审批或策略需跨边界暴露，先更新 `schema/`
- 更新文档：
  - 如治理语义变化，回写 `D-006`、`D-010`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止在 Rust 侧继续让 tool runtime 和 orchestrator 各自维护一套策略

## 7. 语义约束

- 本任务涉及的真相源：
  - 风险阈值
  - 审批动作语义
  - tool allow / deny / ask
  - sandbox 语义
- 是否涉及协议变化：
  - 可能影响 approval / policy 对外表达
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-006`、`D-010`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- 不得把治理逻辑重新塞回 tool runtime 或 orchestrator
- 禁止同时保留两套风险决策路径

## 9. 验收标准

- 编译：
  - `magi-governance` 可独立编译
- 最小运行验证：
  - approval / tool policy / sandbox policy 可独立判定
- 协议验证：
  - 需要对外暴露的策略结构可被 schema 描述
- 清理验证：
  - crate 内不混入实际工具执行和调度逻辑

## 10. 输出结论

- 已完成内容：
  - 已建立 tool request、sandbox request、path access request 的统一治理入口
  - 已统一 `DecisionPhase` 与 `GovernanceDecision`
  - 已支持 tool / approval / sandbox 三类基础判定
  - 已补 worker control request 与 `GovernanceOutcome`，可覆盖 allow / needs approval / rejected / blocked / repair retry 路径
  - 已补 `DecisionPhase::WorkerControl`，供 worker / orchestrator 闭环消费统一治理决策
  - 已补 `GovernanceDecisionTrace` 与 `GovernanceTarget`，可把 tool / sandbox / path / worker control 的治理结果统一导出为可序列化决策轨迹，显式表达 action / outcome / summary
- 删除内容：
  - 无
- 未完成边界：
  - 尚未建立更细粒度阈值配置
  - 尚未把决策轨迹接入统一审计账本与更完整审批动作状态机
- 后续依赖：
  - `magi-tool-runtime`
  - `magi-orchestrator`
  - `magi-worker-runtime`
