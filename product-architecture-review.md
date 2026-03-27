# Magi 产品架构审查报告

更新时间：2025-07-20

## 1. 文档定位

本文档基于对 magi 全项目（263 个 TypeScript 文件，~108K 行代码）的深度审查，以产品架构视角识别系统性不足，按优先级分类并给出改进方案。

本文档遵循 `$cn-engineering-standard`，每项问题必须给出数据支撑、影响分析和具体方案。

## 2. 项目概况

### 2.1 模块规模分布

| 模块 | 文件数 | 代码行数 | 占比 |
|------|--------|---------|------|
| `orchestrator/` | 104 | 37,966 | 35.2% |
| `llm/` | 22 | 13,186 | 12.2% |
| `tools/` | 22 | 12,404 | 11.5% |
| `knowledge/` | 15 | 8,311 | 7.7% |
| `agent/` | 11 | 6,425 | 6.0% |
| `context/` | 10 | 5,974 | 5.5% |
| `ui/` | 8 | 4,748 | 4.4% |
| `session/` | 8 | 4,248 | 3.9% |
| 其他 | 63 | ~15,738 | 14.6% |

### 2.2 `orchestrator/` 内部子模块

| 子目录 | 文件数 | 说明 |
|--------|--------|------|
| `core/` | 41 | 核心引擎、调度、消息、恢复 |
| `runtime/` | 13 | 执行链、时间线、诊断 |
| `profile/` | 13 | 画像、分类、编译器 |
| `mission/` | 7 | Mission 存储与状态 |
| `worker/` | 4 | 自治 Worker |
| `plan-ledger/` | 3 | 计划账本 |
| `protocols/` | 3 | 协议类型 |
| 其他 | 20 | prompts, review, wisdom, recovery, lsp, trace |

## 3. 问题清单

### P0 — 严重设计缺陷

#### P0-1: God-Class 问题

三个文件超过 3,000 行，承载过多职责：

| 文件 | 行数 | 字节 | constructor |
|------|------|------|-------------|
| `mission-driven-engine.ts` | 4,462 | 170KB | 213 行，20+ 私有字段 |
| `autonomous-worker.ts` | 3,571 | — | 内含完整 Todo 执行状态机 |
| `dispatch-manager.ts` | 3,375 | 136KB | DispatchManagerDeps 20+ 回调 |

合计 11,408 行，仅 3 个文件占整个项目的 10.6%。

影响：
- 可维护性差：单文件修改影响半径极大
- 测试困难：构造器依赖注入链过长，mock 成本极高
- 协作冲突：多人开发时合并冲突概率高

#### P0-2: 同名类型三重定义 — `OrchestratorConfig`

| 位置 | 语义 | 核心字段 |
|------|------|---------|
| `types/agent-types.ts:116` | LLM 连接配置 | `llm`, `maxTokens`, `temperature` |
| `config/index.ts:72` | 治理阈值配置 | `maxRetries`, `defaultTimeout`, `governanceThresholds` |
| `protocols/types.ts:81` | 执行运行时配置 | `timeout`, `review`, `verification`, `integration` |

三个完全不同的语义用同一个名字。`import { OrchestratorConfig }` 必须看 path 才知道是哪个。

影响：
- 新开发者极易用错，造成运行时 bug
- IDE 自动补全提示多个候选，增加认知负担
- 代码审查时极易忽略引用错误

### P1 — 架构债务

#### P1-1: `src/types.ts` 遗留 Barrel 文件职责混乱

数据：
- 118 个文件通过它引用类型（全项目扇入最高）
- 43 个文件同时直接引用 `agent-types.ts`（绕过 barrel）
- 7 个文件直接引用 `task/types.ts`

混合了 4 种不同关注点：
- Agent 类型 re-export（`AgentType`, `WorkerSlot`）
- Task 类型 re-export（`Task`, `SubTask`, `TaskView`）
- UI 通信类型（`WebviewToExtensionMessage`, `UIState` 等 ~200 行）
- Domain 类型（`FileSnapshot`, `PendingChange`, `InteractionMode` 等）
- 事件系统（`EventType`, `AppEvent`）

#### P1-2: DispatchManager 回调注入反模式

`DispatchManagerDeps` 接口有 20+ 个回调/getter 字段。
MissionDrivenEngine 的 constructor 花了 213 行构建 deps 对象。

本质问题：
- 违反接口隔离原则（ISP）
- 大量 getter/callback 穿透封装、共享可变状态
- 添加新功能必须修改 deps 接口 → MDE constructor → DM → 级联变更

#### P1-3: `orchestrator/core/` 目录膨胀

41 个文件平铺在同一目录：
- 11 个 `dispatch-*` 文件
- 6 个 `orchestration-*` 文件
- 4 个 `message-*` 文件
- 其余 20 个杂项文件

#### P1-4: 配置系统碎片化

配置散布在 4 个物理位置 + 多种加载机制：

| 位置 | 内容 | 加载器 |
|------|------|--------|
| `~/.magi/llm.json` | LLM 连接 | `LLMConfigLoader` |
| `~/.magi/config.json` | 全局配置 | `ConfigManager` |
| `~/.magi/worker-assignments.json` | Worker 分配 | `WorkerAssignmentStorage` |
| `~/.magi/*.json` | 各类子配置 | 各自 loader |
| `.magi/` | 工作区运行时 | `FileBasedMissionStorage` |
| VS Code settings | 扩展配置 | `vscode.workspace.getConfiguration` |

没有统一的配置层次模型和变更通知机制。

### P2 — 改进建议

#### P2-1: 顶层散落文件归属不明

| 文件 | 外部引用 | 建议 |
|------|---------|------|
| `src/types.ts` | 118 | 拆分（见 P1-1） |
| `src/events.ts` | 4 | 移入 infrastructure |
| `src/snapshot-manager.ts` | 10+ | 移入独立目录 |
| `src/diff-generator.ts` | 1 | 移入 agent/service/ |

#### P2-2: 次级大文件

| 文件 | 行数 | 建议 |
|------|------|------|
| `local-agent-service.ts` | 3,485 | 拆分路由/生命周期 |
| `orchestrator-adapter.ts` | 2,815 | 拆分 prompt 构建/决策/终止 |
| `plan-ledger-service.ts` | 2,675 | 拆分状态机逻辑 |
| `tool-manager.ts` | 2,609 | 拆分注册/执行/schema |

#### P2-3: 缺少架构文档

仅有 1 份设计文档（`worker-dispatch-auto-assignment-rebuild-plan.md`）。
缺少整体架构图、模块交互图、数据流图、配置系统文档、部署架构文档。

## 4. 架构质量评分

| 维度 | 评分 | 说明 |
|------|------|------|
| 功能完整性 | ⭐⭐⭐⭐⭐ | 编排、调度、执行、恢复、知识管理完备 |
| 模块划分 | ⭐⭐⭐☆☆ | 顶层合理，core/ 内部缺乏分层 |
| 类型系统 | ⭐⭐☆☆☆ | 同名冲突、barrel 混乱、碎片化 |
| 可维护性 | ⭐⭐☆☆☆ | 3 个 God-class 是主要风险 |
| 可测试性 | ⭐⭐☆☆☆ | 构造器过重、回调链过长 |
| 代码质量 | ⭐⭐⭐⭐☆ | 无死代码，命名规范一致 |
| 文档完备性 | ⭐⭐☆☆☆ | 仅 1 份设计文档 |

## 5. 改进路线图

### Phase 1 — 高 ROI，低风险（文件移动与重命名，不改逻辑）

| 编号 | 任务 | 预估影响 |
|------|------|---------|
| R-1 | OrchestratorConfig 三处重命名消除同名冲突 | 修改 ~13 个文件 import |
| R-2 | orchestrator/core/ 按子域拆分子目录 | 移动 41 个文件，修改 import |

### Phase 2 — 中等投入

| 编号 | 任务 | 预估影响 |
|------|------|---------|
| R-3 | src/types.ts 拆分：UI 通信 → protocol/，Domain → 各模块 | 需迁移 118 个消费者 |
| R-4 | DispatchManagerDeps 接口瘦身 | 影响 MDE + DM 两个核心文件 |

### Phase 3 — 重构性投入

| 编号 | 任务 | 预估影响 |
|------|------|---------|
| R-5 | MissionDrivenEngine 职责拆分 | 4,462 行 → 5+ 个文件 |
| R-6 | AutonomousWorker 职责拆分 | 3,571 行 → 3+ 个文件 |
| R-7 | DispatchManager 职责拆分 | 3,375 行 → 3+ 个文件 |

## 6. 约束与原则

- 所有重构必须通过 `npx tsc --noEmit` + `npm run vscode:prepublish` 验证
- 每一步重构后立即构建验证，禁止累积多步再验证
- 文件移动类重构（Phase 1）优先，逻辑拆分类重构（Phase 3）后置
- 同名类型重命名采用全量查找替换 + 逐文件确认策略


