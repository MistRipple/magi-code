# MultiCLI 功能点核对清单

> 版本: 0.5.1 | 创建时间: 2026-01-13 | 状态: 进行中

## 状态说明

- `[ ]` 未开始
- `[/]` 进行中
- `[x]` 已完成（代码实现符合设计）
- `[!]` 存在缺口（需要修复/重构）
- `[-]` 不适用/已废弃

---

## 一、核心架构层（P0 - 必须优先核对）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| A1 | 独立编排者架构（Orchestrator 仅调度/验证，不编辑代码） | 2.2, 2.3 | orchestrator-agent.ts | [x] | 独立编排者存在，未发现直接写文件逻辑 |
| A2 | 三子代理执行者（Claude/Codex/Gemini Worker） | 2.2, 2.3 | worker-agent.ts, workers/*.ts | [x] | Worker 基类与三种实现已落地 |
| A3 | 消息总线（MessageBus）编排者与 Worker 通信 | 2.1, 2.2 | message-bus.ts | [x] | MessageBus 已实现并被编排/执行层使用 |
| A4 | Worker Pool 管理器 | 2.1 | worker-pool.ts | [x] | 队列/重试/降级/锁均已实现 |
| A5 | **RiskPolicy（策略内核）** — 重构新增 | 2.8 | risk-policy.ts | [x] | 已按设计评分维度与映射完善 |

---

## 二、工作流层（P0）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| B1 | 7 阶段工作流（P1→P7） | 7.2 | intelligent-orchestrator.ts | [x] | 阶段门控与确认策略已统一 |
| B2 | Phase 1: 任务分析 | 7.2 | task-analyzer.ts, ai-task-decomposer.ts | [x] | AI/规则分析具备有效性校验与兜底 |
| B3 | Phase 2: Hard Stop（执行计划确认） | 7.2 | intelligent-orchestrator.ts | [x] | 确认回调 + UI 内联卡片已落地 |
| B4 | Phase 3: 并行/串行执行 | 7.2 | execution-scheduler.ts, worker-pool.ts | [x] | WorkerPool/ExecutionScheduler 支持并行与依赖 |
| B5 | Phase 4: 集成联调 | 7.2 | intelligent-orchestrator.ts | [x] | 门控/联调审查/修复回派与进度一致性已收口 |
| B6 | Phase 5: 验证检查 | 7.2, 7.4 | verification-runner.ts | [x] | VerificationRunner 已集成 |
| B7 | Phase 6: 失败治理（分型恢复） | 7.2, 7.5 | recovery-handler.ts | [x] | 分型策略与恢复触发已接入 |
| B8 | Phase 7: 汇总交付 | 7.2 | result-aggregator.ts | [x] | 汇总去重/过滤联调结果并做输出净化 |
| B9 | **RiskPolicy 路径分级（轻量/标准/完整）** — 重构新增 | 2.8 | risk-policy.ts | [x] | 按评分映射完成 |
| B10 | **RiskPolicy 风险评分规则与映射** — 重构新增 | 2.8 | risk-policy.ts | [x] | 维度/权重规则已落地 |
| B11 | **轻量跨层合并规则（小改动不拆分）** — 新增 | 2.4 | orchestrator-agent.ts | [x] | 合并规则已接入并补全分发逻辑 |

---

## 三、任务调度层（P0）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| C1 | 任务拆分与路由（Skills-based CLI 选择） | 3.1-3.4 | task-splitter.ts, cli-selector.ts | [x] | 补齐 backend 类别并与技能映射一致 |
| C2 | 任务依赖图（DAG 调度） | 2.4, 4.4 | task-dependency-graph.ts | [x] | 已在 WorkerPool 依赖调度中使用 |
| C3 | 并行/串行决策 | 2.4, 4.1-4.3 | execution-scheduler.ts | [x] | 执行调度器具备并行/串行 |
| C4 | 文件锁策略 | 2.4, 4.4 | execution-scheduler.ts / worker-pool.ts | [x] | FileLockManager 已接入队列 |
| C5 | **冲突域与依赖调度** — 重构新增 | 2.4 | orchestrator-agent.ts / worker-pool.ts | [x] | 冲突域标注 + 调度锁已落地 |
| C6 | 任务队列与优先级调度 | 4.4 | worker-pool.ts | [x] | 多队列 + 优先级 + 饥饿提升 |
| C7 | 子任务自检与互检 | 4.5 | intelligent-orchestrator.ts | [x] | 自检/互检在 OrchestratorAgent 中实现 |

---

## 四、快照与变更层（P0）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| D1 | Snapshot Manager（创建/通过/还原） | 2.6, 16, 附录B | snapshot-manager.ts | [x] | 创建/还原/通过均已实现 |
| D2 | Diff 生成器（本地生成，0 Token） | 2.6, 16 | diff-generator.ts | [x] | 本地 diff 生成存在 |
| D3 | 快照元数据更新（lastModifiedBy/At） | 9.1 | snapshot-manager.ts | [x] | lastModifiedBy/At 已写入 |
| D4 | 待处理变更列表 | 10.1 | snapshot-manager.ts | [x] | pendingChanges 已输出到 UI |

---

## 五、验证与恢复层（P1）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| E1 | 验证执行器（编译/IDE诊断/Lint/Test） | 7.4 | verification-runner.ts | [x] | VerificationRunner 已集成 |
| E2 | **验证梯度（由 RiskPolicy 驱动）** — 重构新增 | 2.8 | orchestrator-agent.ts | [x] | 低/高风险分别切换验证强度 |
| E3 | **失败治理分型（替代 3-Strike 盲重试）** — 重构新增 | 7.5 | recovery-handler.ts | [x] | 已按失败类型策略执行 |

---

## 六、任务状态管理层（P1）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| F1 | TaskStateManager（状态追踪） | 8.1 | task-state-manager.ts | [x] | 状态流转验证与持久化校验已完善 |
| F2 | 状态同步机制（事件发布 + UI 订阅） | 8.2 | task-state-manager.ts | [x] | 载入即同步并广播状态 |
| F3 | 持久化存储（.multicli/tasks/） | 8.3 | task-state-manager.ts | [x] | 版本化持久化与数据校验完成 |
| F4 | 长任务连续执行（批量 Prompt） | 8.4 | orchestrator-agent.ts | [x] | 同 CLI 多任务批量执行已接入 |

---

## 七、上下文管理层（P1）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| G1 | 三层上下文管理（Memory + 对话 + 切片） | 2.5 | context-manager.ts | [x] | ContextManager 已集成 |
| G2 | 智能压缩代理 | 2.5 | context-compressor.ts | [x] | ContextCompressor 已集成 |
| G3 | Memory 文档读写 | 2.5 | memory-document.ts | [x] | MemoryDocument 已集成 |
| G4 | Worker 上下文注入（精简） | 2.5 | context-manager.ts | [x] | 依据任务规模动态裁剪上下文 |

---

## 八、消息与会话层（P1）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| H1 | 统一消息入口（streamEvent / orchestratorMessage） | 2.7 | message-bus.ts / UI | [x] | 已收口到两类入口 |
| H2 | 事件结构约束（sessionId / target / source） | 2.7 | types.ts | [x] | streamEvent/orchestratorMessage 均带约束 |
| H3 | 角色隔离（Thread 只允许 orchestrator） | 2.7 | UI 层 | [x] | UI 路由已严格过滤 |
| H4 | 会话一致性（切换会话静默中断） | 2.7 | session-manager.ts | [x] | 切换会话前静默中断 |
| H5 | 渲染节奏（增量更新） | 2.7 | UI 层 | [x] | 主对话与 CLI 流式增量更新 |

---

## 九、稳定性防护层（P1）— 重构新增

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| I1 | 空响应检测与自动恢复 | 2.9 | cli/session/session-manager.ts | [x] | 空响应统一判定并触发恢复 |
| I2 | 输出截断与上下文预压缩 | 2.9 | context-compressor.ts | [x] | 预压缩与截断策略已接入上下文切片 |
| I3 | 会话恢复与断点续跑 | 2.9 | cli/session/session-manager.ts | [x] | 任务上下文快照提前注入并可恢复 |
| I4 | 任务状态持久化 | 2.9 | task-state-manager.ts | [x] | 载入校验 + 状态广播已完成 |

---

## 十、交互模式层（P2）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| J1 | Ask/Agent/Auto 三种模式 | 6.1-6.3 | types.ts, intelligent-orchestrator.ts | [x] | 交互模式已接入 UI/编排 |
| J2 | 模式行为差异（文件修改/命令执行/确认/回滚） | 6.2 | intelligent-orchestrator.ts | [x] | 模式策略驱动确认与恢复 |
| J3 | **风险阈值驱动 Hard Stop** — 重构新增 | 2.8 | orchestrator-agent.ts | [x] | 风险驱动确认策略已落地 |

---

## 十一、UI 层（P2）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| K1 | Webview 主面板（对话/任务/变更 Tab） | 11.1-11.3 | webview-provider.ts | [x] | 主面板结构完整 |
| K2 | CLI 输出面板（Claude/Codex/Gemini） | 11.3 | webview | [x] | CLI Tab 已实现 |
| K3 | 消息路由与展示规则 | 11.6 | webview | [x] | 已按设计收口 |
| K4 | Hard Stop 确认卡片 | 11.6 | webview | [x] | 内联卡片已实现 |
| K5 | Session 选择器 | 11.4 | webview | [x] | 会话切换/重命名/删除已实现 |
| K6 | Diff 预览面板 | 11.4 | webview | [x] | VSCode 原生 diff 已接入 |

---

## 十二、用户交互流程（P2）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| L1 | 打断/继续/回滚流程 | 17.0 | intelligent-orchestrator.ts | [x] | 中断标记与回滚/继续路径已统一 |
| L2 | 状态流转（pending→running→completed/failed） | 17.1 | task-state-manager.ts / task-manager.ts | [x] | 子任务驱动任务状态流转已统一 |
| L3 | 快捷键支持 | 17.2 | extension.ts | [x] | 已补齐快捷键映射并补全命令贡献 |

---

## 十三、CLI 探测与降级（P3）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| M1 | CLI Detector（检测可用性） | 2.6, 5.1 | cli-detector.ts | [x] | 可用性检测已实现 |
| M2 | CLI 状态类型（AVAILABLE/NOT_INSTALLED/AUTH_FAILED 等） | 5.1 | types.ts | [x] | CLIStatusCode 已定义 |
| M3 | 降级逻辑（备选 CLI 选择） | 5.2 | cli-selector.ts | [x] | CLISelector + WorkerPool 降级路径已统一 |
| M4 | 能力总结表 | 5.3 | cli-selector.ts | [x] | CLI_CAPABILITIES 已提供 |

---

## 十四、执行统计与健康度（P3）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| N1 | 执行统计模块 | 13 Phase 8 | execution-stats.ts | [x] | ExecutionStats 已实现 |
| N2 | CLI 健康状态监控 | 1.1 | execution-stats.ts | [x] | 健康评分与阈值纳入统计与选择策略 |

---

## 十五、配置与权限（P3）— 重构新增

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| O1 | 基础配置项（CLI 路径/skills/timeout） | 14 | package.json, types.ts | [x] | 统一入口读取并传递到工厂与编排器 |
| O2 | **权限与安全阀（edit/bash/web 权限矩阵）** — 重构新增 | 14.x | package.json, orchestrator | [x] | 权限矩阵已落地并注入 Worker Prompt |
| O3 | **策略开关（自动验证/恢复/回滚）** — 重构新增 | 14.x | package.json, orchestrator | [x] | 统一策略开关已接入执行与恢复流程 |

---

## 十六、事件系统（P3）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| P1 | 事件定义（task:*/worker:*/snapshot:*） | 10.2 | events.ts | [x] | 事件枚举与总线已实现 |

---

## 十七、流式输出与打断（P3）

| 编号 | 功能点 | 设计章节 | 预期实现模块 | 状态 | 备注 |
| --- | --- | --- | --- | --- | --- |
| Q1 | 流式输出（spawn + stdout/stderr） | 附录A | workers/*.ts | [x] | CLI 流式输出已接入 |
| Q2 | 打断机制（SIGTERM/SIGKILL） | 附录A | workers/*.ts | [x] | interrupt/kill 已实现 |
