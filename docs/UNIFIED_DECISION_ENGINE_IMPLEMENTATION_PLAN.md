# 编排统一决策引擎实施计划（产品发布级）

## 1. 目标与边界

- 目标：将编排阶段“门禁判定/恢复动作/终止收口”收敛到单一决策线路，消除分散 if-else 分支导致的误停与维护成本。
- 本期只做“必须项”：统一门禁裁决、统一策略承载、统一决策证据链落盘。
- 不做“锦上添花”：不引入新功能入口，不新增复杂 UI 流程，不改既有交互习惯。

## 2. 现状问题（改造前）

- 预算/外部等待/上游错误/stalled 判定逻辑分散在 adapter 内多个分支，迭代容易出现行为漂移。
- Shadow 判定与主判定虽然目标一致，但存在重复实现与一致性维护成本。
- 终止时缺少“本轮为何继续/为何停止”的统一结构化轨迹，线上排障主要依赖日志拼接。

## 3. 改造原则

- 单一决策内核：同一输入快照必须得到同一决策结果。
- 状态显式化：把门禁状态（streak/noProgress/上游错误计数）作为结构化状态传递。
- 可解释优先：每轮关键决策生成可审计 decision trace，并随运行态落盘。
- 兼容现有链路：不破坏现有 reason code、优先级、回归脚本语义。

## 4. 任务分解（实施清单）

### A. 决策内核收敛（必须）

1. 新增 `src/llm/adapters/orchestrator-decision-engine.ts`
   - 统一承载：
     - 门禁阈值策略（stalled/external_wait/error_rate/去抖/硬阈值）
     - streak 更新
     - budget/external_wait/upstream/stalled 候选生成
     - shadow reason 计算
2. `orchestrator-adapter.ts` 改造为“调用决策引擎”
   - 替换分散 gate/streak 判定逻辑
   - 移除重复 helper（预算/外部等待阈值函数）
   - 保留原 reason code 与优先级机制

### B. 决策轨迹贯通（必须）

1. 扩展 `OrchestratorRuntimeState`
   - 新增 `decisionTrace`（轮次、阶段、动作、候选、门禁状态、备注、时间）
2. 运行态上报链路补齐
   - `adapter-factory-interface.ts` 扩展 `orchestratorRuntime.decisionTrace`
   - `mission-driven-engine.ts` 接收并写入终止指标
   - `termination-metrics-repository.ts` 增加 `decision_trace` 字段

### C. 文档与验收（必须）

1. 记录实施方案与完成状态（本文档）
2. 通过编译与门禁主回归链验证
   - 编排门禁、无 Todo 收敛、对话连续性、工具后不重复输出、终止治理基线

## 5. 验收标准（DoD）

- 代码结构
  - 门禁判定主逻辑仅保留一处决策引擎实现。
  - adapter 仅负责组装上下文、调用引擎、执行动作。
- 行为一致性
  - 既有 reason code 与优先级行为不退化。
  - 不复发“工具返回即停”“单轮尖峰误停”“工具轮重复回灌”。
- 可观测性
  - 终止指标中可追踪 `decision_trace`。
  - 可还原每轮关键决策动作与触发条件。

## 6. 实施状态（本轮）

- [x] A1 新增统一决策内核文件。
- [x] A2 adapter 接入决策内核并移除重复门禁 helper。
- [x] B1 运行态新增 decision trace 结构。
- [x] B2 decision trace 贯通到指标落盘。
- [x] C2 全链路回归通过（2026-03-09 01:24 +0800）。

回归命令：
- `npm run -s compile`
- `npm run -s verify:e2e:orchestrator-gate-debounce`
- `npm run -s verify:e2e:no-todo-tool-budget-no-hard-stop`
- `npm run -s verify:e2e:no-todo-post-tool-ambiguous`
- `npm run -s verify:e2e:orchestrator-token-budget-scope`
- `npm run -s verify:e2e:gate-fail-open`
- `npm run -s verify:e2e:conversation-continuity-gate`
- `npm run -s verify:e2e:tool-round-runtime-no-duplicate`
- `npm run -s verify:e2e:tool-round-duplicate-output`
- `npm run -s verify:e2e:tool-termination-resilience`
- `npm run -s verify:e2e:termination-scope`
- `npm run -s verify:e2e:termination-shadow-gate`
- `npm run -s verify:e2e:termination-ab-gate`
- `npm run -s verify:e2e:orchestrator-termination`

## 7. 风险与后续（下一阶段）

- 本期已统一“终止门禁决策”；下一阶段建议继续统一“恢复决策”（retry/switch/degrade/finalize）到同一内核，形成完整 `DecisionAction` 状态机。
- UI 当前未直接展示 decision trace，可在后续设置页增加“高级诊断视图”读取 `.magi/metrics/termination.jsonl`。
