# 统一编排架构升级方案

> 基于 `unified-orchestration-design.md` 的架构审查结果，本文档仅包含升级内容。

---

## 升级 1: dispatch_task 安全护栏 [P0]

### 问题
dispatch_task 不需要 Mission，没有用户确认、没有快照回滚。orchestrator 可通过多次 dispatch_task 绕过 plan_mission 的所有安全机制。

### 方案
在 `DispatchBatch.register()` 中增加规模检测，触发阈值时返回升级建议而非静默继续。

**阈值规则**：
- 同一 batch 内 dispatch 数量 ≥ 3 → 返回 `escalation_suggested` 标记
- 同一 batch 内涉及的不重复文件总数 ≥ 5 → 返回 `escalation_suggested` 标记

**影响文件**：
- `src/orchestrator/core/dispatch-batch.ts` — 新增 `checkEscalation()` 方法
- `src/tools/orchestration-executor.ts` — `executeDispatchTask` 中读取标记，将建议注入工具结果

---

## 升级 2: Worker 执行隔离 [P0]

### 问题
设计文档约束 5 中"独立上下文"和"任务队列"用"或"连接，方案未定。

### 方案
确定为 **同类型串行 + 不同类型并行**。在 DispatchBatch 中增加 `WorkerQueue`，同一 WorkerSlot 的任务排队串行执行。

**实现**：
- `src/orchestrator/core/dispatch-batch.ts` — 新增 `getReadyTasksByWorkerIsolation()` 方法
  - 每个 WorkerSlot 最多返回 1 个 ready 任务
  - 不同 WorkerSlot 可同时各返回 1 个

---

## 升级 3: Worker enum 动态化 [P1]

### 问题
`OrchestrationExecutor` 中 worker enum 硬编码为 `['claude', 'codex', 'gemini']`，与设计文档"从 ProfileLoader 动态获取"不符。

### 方案
注入 `ProfileLoader`，工具定义和验证逻辑从 `ProfileLoader.getAllProfiles().keys()` 动态获取。

**影响文件**：
- `src/tools/orchestration-executor.ts` — 构造函数接收 ProfileLoader，4 处硬编码替换

---

## 升级 4: Phase B+ 频率限制 [P1]

### 问题
`OrchestratorResponder.handleProgress()` 直接返回 `continue`，无频率限制。设计文档要求 30s 最小间隔。

### 方案
在 `OrchestratorResponder` 中增加频率限制状态：

- `lastReportTimestamps: Map<WorkerSlot, number>` — 记录每个 Worker 上次触发 LLM 调用的时间
- 进度汇报（progress）：直接 continue，不触发 LLM
- 阻塞/请求汇报（question）：检查间隔 ≥ 30s 才触发 LLM 调用，否则排队

**影响文件**：
- `src/orchestrator/core/executors/orchestrator-responder.ts` — 新增频率限制逻辑

---

## 升级 5: Phase A 决策摘要持久化 [P1]

### 问题
Phase A 结束后 orchestrator LLM 退出。Phase B+ 中间调用丢失了 Phase A 的推理上下文。

### 方案
MissionDrivenEngine 在 Phase A 完成后（`understandGoalWithLLM` + `planCollaborationWithLLM`），将关键决策摘要写入 SharedContextPool。Phase B+ 调用时从池中读取。

**写入内容**：
```typescript
{
  type: 'decision',
  source: 'orchestrator',
  content: `目标: ${goal}\n分析: ${analysis}\n策略: ${strategy}`,
  importance: 'critical',
  tags: ['phase-a-decision'],
}
```

**影响文件**：
- `src/orchestrator/core/mission-driven-engine.ts` — `execute()` 中规划完成后写入

---

## 升级 6: 成本控制 [P2]

### 问题
无预算/成本上限机制，用户可能产生不可控 API 费用。

### 方案
在 DispatchBatch 中增加 token 消耗追踪：

- `tokenBudget: { limit: number; consumed: number }` — 可配置的 token 预算
- Worker 完成时累加 `consumed`
- 超过预算 80% 时发出警告事件
- 超过预算 100% 时阻止新任务 dispatch

**影响文件**：
- `src/orchestrator/core/dispatch-batch.ts` — 新增预算追踪
- `src/tools/orchestration-executor.ts` — dispatch 前检查预算

