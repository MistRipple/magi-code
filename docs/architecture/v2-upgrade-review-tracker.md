# 统一编排架构 -- 审核与升级任务追踪

> 审核日期：2026-02-07
> 审核方法：多代理联合审核（架构师 + 工程师 + 文档审核 + SRE）
> 设计文档：`docs/architecture/unified-orchestration-design.md`

---

## 一、审核总结

| 维度 | 审核方 | 结论 |
|:---|:---|:---|
| 架构模式合理性 | 架构师 | **通过** -- 三层模型设计合理，动态升级可行 |
| 通信设计 | 架构师 | **需修补** -- Phase B+ 中间调用机制需明确 |
| 非阻塞执行模型 | 架构师 | **需修补** -- DispatchBatch 和 Phase C 需实现 |
| 依赖链管理 | 架构师 | **有缺陷** -- depends_on/task_id/环检测缺失 |
| 扩展性 | 架构师 | **需注意** -- Worker 硬编码需动态化 |
| 代码匹配度 | 工程师 | **差距显著** -- 6 个核心组件需改造 |
| 文档一致性 | 文档审核 | **11 个问题** -- 3 高 / 6 中 / 2 低 |
| 风险与边界 | SRE | **14 个遗漏场景** -- 3 P0 / 5 P1 / 4 P2 / 2 P3 |

---

## 二、设计层面问题（已修复到设计文档）

以下问题已在审核后直接更新到 `unified-orchestration-design.md`：

### 已修复

| ID | 问题 | 修复内容 | 影响章节 |
|:---|:---|:---|:---|
| D-01 | LLM 调用次数统计不含中间调用和用户确认轮次 | 修正三层模型描述为"+ 可能的中间调用"；场景示例表精确计算每个场景的调用次数；增加"LLM 调用次数说明"注脚 | 三章 |
| D-02 | Worker→orchestrator 上报只有功能描述，无具体 API 机制 | 定义 `workerReport` 事件 + `reportToOrchestrator(type, content)` API；type 分 blocker/info/request 三种 | 六章 通道1 |
| D-03 | "通过结果摘要或错误状态"与"运行时主动上报"语义矛盾 | 统一为"通过 workerReport 事件向 orchestrator 上报问题（blocker 类型），触发 Phase B+" | 六章 通道3 |
| D-04 | 中间 LLM 调用未纳入 Phase 模型 | 引入 Phase B+ 概念，明确不属于 A 也不属于 C，是 Phase B 期间的附加响应步骤 | 六章 通道1 |
| D-05 | 中间调用无频率限制和并发处理策略 | 增加最小间隔 30 秒、仅 blocker/request 触发 LLM、并发上报排队串行 | 六章 通道1 |
| D-06 | 中间调用 Token 估算缺失 | 增加精简上下文 ~600-800 tokens 估算 | 六章 通道1 |
| D-07 | DispatchBatch 生命周期不完整 | 补充完整 6 步生命周期（创建→注册→追加→完成检测→汇总→归档）；明确 Layer 1 不创建 | 五章 |
| D-08 | 前序 Worker 失败处理仅在风险表，未在主流程定义 | 在 DispatchBatch 主流程中增加"依赖链与失败处理"段落：级联跳过、部分失败、全部失败 | 五章 |
| D-09 | SharedContextPool 数据流未定义写入主体/内容/筛选 | 补充完整数据流定义：写入主体（Worker 自动）、写入内容（文件列表/接口/摘要）、读取筛选（按 task_id）、并发安全 | 六章 通道3 |
| D-10 | Worker 取消信号链缺失 | 新增约束4：CancellationToken + AbortController + 全局超时 10 分钟 | 八章 |
| D-11 | Worker 实例隔离缺失 | 新增约束5：每个 dispatch 独立执行上下文 | 八章 |
| D-12 | 并行 Worker 文件冲突无检测 | 新增约束6：DispatchBatch 基于 files 参数自动检测冲突转串行 | 八章 |
| D-13 | 依赖链无深度上限和环检测 | 新增约束7：深度上限 5 层 + 拓扑排序环检测 | 八章 |
| D-14 | "6 层架构"概念突现无定义 | 替换为实际组件列表：MessageHub + MessagePipeline + MessageBus + MessageFactory | 九章 |
| D-15 | RecoveryHandler 命名不一致 | 统一为 ProfileAwareRecoveryHandler | 九章+十章 |
| D-16 | 功能清单缺失多项 | 补充：取消与超时、Worker 执行隔离、文件冲突检测、依赖链安全 | 九章 |
| D-17 | 风险表不完整（8项→16项） | 从 8 项扩充到 16 项，覆盖取消信号、超时、状态覆盖、汇总失败、文件冲突、用户并发、环形依赖、plan_mission 拒绝 | 十章 |

---

## 三、代码实现层面问题（升级任务表）

以下问题需要在代码升级阶段实现，按实现优先级排序：

### P0 -- 核心基础设施（阻塞后续所有功能）

| ID | 任务 | 涉及文件 | 难度 | 状态 | 前置依赖 |
|:---|:---|:---|:---|:---|:---|
| C-01 | **实现 DispatchBatch 类** -- 追踪所有 dispatch_task 的状态、依赖关系、生命周期管理 | `dispatch-batch.ts` | 高 | ✅ 已完成 | 无 |
| C-02 | **dispatch_task 返回 task_id** -- 工具定义增加 depends_on 参数，返回值增加 task_id/status/worker | `orchestration-executor.ts` | 低 | ✅ 已完成 | 无 |
| C-03 | **dispatch_task 改为真正非阻塞** -- Handler 签名改为立即返回 task_id，Worker 异步执行 | `orchestration-executor.ts` + `mission-driven-engine.ts` | 中 | ✅ 已完成 | C-01, C-02 |

### P1 -- 核心功能实现

| ID | 任务 | 涉及文件 | 难度 | 状态 | 前置依赖 |
|:---|:---|:---|:---|:---|:---|
| C-04 | **实现 Phase C 汇总 LLM 调用** -- DispatchBatch 全部完成后自动触发 orchestrator 汇总 | `mission-driven-engine.ts` + `orchestrator-prompts.ts` | 中 | ✅ 已完成 | C-01 |
| C-05 | **实现 Phase B+ 中间 LLM 调用** -- Worker 上报事件触发 orchestrator 中间响应 | `mission-driven-engine.ts` | 高 | ✅ 已完成 | C-01 |
| C-06 | **Worker 上报机制增强** -- AutonomousWorker 增加 reportToOrchestrator(type, content) 方法 | `autonomous-worker.ts` | 中 | ✅ 已完成（复用 onReport 回调） | C-05 |
| C-07 | **Worker 能力描述动态化** -- 系统提示词和工具定义从 ProfileLoader 动态读取 | `orchestrator-prompts.ts` + `orchestration-executor.ts` + `mission-driven-engine.ts` | 低 | ✅ 已完成 | 无 |
| C-08 | **依赖链编排** -- DispatchBatch 中实现 depends_on 等待和 SharedContextPool 注入 | `mission-driven-engine.ts` + `dispatch-batch.ts` | 中 | ✅ 已完成 | C-01, C-02 |

### P2 -- 可靠性增强

| ID | 任务 | 涉及文件 | 难度 | 状态 | 前置依赖 |
|:---|:---|:---|:---|:---|:---|
| C-09 | **CancellationToken 信号链** -- cancel 信号传递到 Worker + LLM 请求 AbortController | `dispatch-batch.ts` + `mission-driven-engine.ts` + `autonomous-worker.ts` | 中 | ✅ 已完成 | C-01 |
| C-10 | **Worker 级超时** -- Promise.race 包裹，默认 10 分钟 | `mission-driven-engine.ts` | 低 | ✅ 已完成 | C-01 |
| C-11 | **Worker 执行上下文隔离** -- 每个 dispatch 使用独立 batchId 作为 missionId | `mission-driven-engine.ts` | 中 | ✅ 已完成 | 无 |
| C-12 | **环形依赖检测 + 深度上限** -- DispatchBatch 注册时拓扑排序 + 深度校验 | `dispatch-batch.ts` + `mission-driven-engine.ts` | 低 | ✅ 已完成 | C-01 |
| C-13 | **文件冲突检测** -- DispatchBatch 检测 files 参数重叠 | `dispatch-batch.ts` + `mission-driven-engine.ts` | 低 | ✅ 已完成 | C-01, C-08 |
| C-14 | **Phase C 降级展示** -- 汇总 LLM 失败时直接展示 Worker 原始结果 | `mission-driven-engine.ts` | 低 | ✅ 已完成 | C-04 |
| C-15 | **Worker 崩溃后状态清理** -- catch 块中调用 clearAllSessions + 通知 DispatchBatch | `mission-driven-engine.ts` | 低 | ✅ 已完成 | C-01 |

---

## 四、可直接复用的现有能力

| 能力 | 现有实现 | 复用方式 |
|:---|:---|:---|
| Mission 完整流程 | MissionOrchestrator.planMission | plan_mission 工具直接调用 |
| Worker 执行引擎 | AutonomousWorker.executeAssignment | dispatch_task Worker 执行核心 |
| UI 消息管道 | MessageHub 全套 API（subTaskCard、workerOutput 等） | 无需改造 |
| Worker 进度回传 | subTaskCard 更新机制 | 直接复用 |
| 配置解析 | ProfileLoader + WorkerAssignmentLoader | 动态描述的数据源 |
| 上下文共享 | SharedContextPool + insightGenerated | 串行依赖的数据传递通道 |
| 失败恢复 | ProfileAwareRecoveryHandler | Worker 内部复用 |
| Todo 管理 | AutonomousWorker 的 Todo 循环 | Worker 内部复用 |

---

## 五、建议实施顺序

```
阶段 1（基础设施）: C-01 → C-02 → C-03
  ↓
阶段 2（核心功能）: C-04 + C-07（并行） → C-08 → C-05 → C-06
  ↓
阶段 3（可靠性）:   C-09 + C-10 + C-11 + C-12（并行） → C-13 → C-14 → C-15
```

阶段 1 完成后即可进行基本的 dispatch_task 非阻塞执行。
阶段 2 完成后具备完整的三层执行模型能力。
阶段 3 完成后达到生产级可靠性。
