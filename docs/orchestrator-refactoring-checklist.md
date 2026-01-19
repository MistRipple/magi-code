# OrchestratorAgent 重构检查清单

> **关联文档**: [orchestrator-refactoring-plan.md](./orchestrator-refactoring-plan.md)

---

## Phase 1: 基础设施

### 数据模型
- [ ] 定义 `Mission` 接口
- [ ] 定义 `Contract` 接口
- [ ] 定义 `Assignment` 接口
- [ ] 定义 `WorkerTodo` 接口
- [ ] 定义所有状态枚举类型
- [ ] 创建类型文件 `src/orchestrator/mission/types.ts`

### 存储层
- [ ] 实现 `MissionStorage`
  - [ ] `save(mission: Mission): Promise<void>`
  - [ ] `load(id: string): Promise<Mission | null>`
  - [ ] `update(mission: Mission): Promise<void>`
  - [ ] `delete(id: string): Promise<void>`
  - [ ] `listBySession(sessionId: string): Promise<Mission[]>`
- [ ] 实现 `ContractStorage`（可内嵌于 Mission）
- [ ] 实现数据迁移工具

### 管理器基础
- [ ] 实现 `ContractManager` 骨架
  - [ ] `defineContracts()`
  - [ ] `verifyContractConsistency()`
- [ ] 实现 `AssignmentManager` 骨架
  - [ ] `createAssignments()`
  - [ ] `updateAssignment()`

---

## Phase 2: Worker 自主性

### AutonomousWorker
- [ ] 创建 `AutonomousWorker` 类
- [ ] 实现 `planWork()` - 自主规划
  - [ ] 构建规划 Prompt
  - [ ] 解析 Todo 列表
  - [ ] 验证 Todo 合理性
- [ ] 实现 `executeTodo()` - 执行单个 Todo
  - [ ] 整合 GuidanceInjector
  - [ ] 生成 TodoOutput
- [ ] 实现 `addDynamicTodo()` - 动态添加
  - [ ] 超范围检测
  - [ ] 审批流程
- [ ] 实现 `planRecovery()` - 失败恢复

### 规划审查
- [ ] 实现规划审查逻辑
  - [ ] 检查 Todo 覆盖度
  - [ ] 检查契约依赖
  - [ ] 检查超范围项
- [ ] 实现规划修订流程
- [ ] 实现规划批准/拒绝

### 与现有系统整合
- [ ] 复用现有 `GuidanceInjector`
- [ ] 复用现有 `ProfileLoader`
- [ ] 适配现有 `CLIAdapterFactory`

---

## Phase 3: 编排器重构

### MissionOrchestrator 核心
- [ ] 创建 `MissionOrchestrator` 类
- [ ] 实现 `execute()` 主入口
- [ ] 实现 `understandGoal()` - Phase 2
  - [ ] 创建 `GoalParser` 组件
  - [ ] 提取目标、约束、验收标准
- [ ] 实现 `planCollaboration()` - Phase 3
  - [ ] 确定参与者
  - [ ] 定义契约
  - [ ] 分配职责
- [ ] 实现 `letWorkersPlan()` - Phase 4
- [ ] 实现 `reviewPlanning()` - Phase 5
- [ ] 实现 `executeMission()` - Phase 7
- [ ] 实现 `verifyMission()` - Phase 8
- [ ] 实现 `summarizeMission()` - Phase 9

### WorkerCoordinator
- [ ] 创建 `WorkerCoordinator` 类
- [ ] 实现 Worker 实例管理
- [ ] 实现并行执行调度
- [ ] 实现进度汇报
- [ ] 实现阻塞处理

### 整合现有组件
- [ ] 复用 `IntentGate`
- [ ] 复用 `VerificationRunner`
- [ ] 复用 `SnapshotManager`
- [ ] 复用 `ContextManager`

---

## Phase 4: 契约系统

### ContractManager 完善
- [ ] 实现契约类型识别
  - [ ] API 契约
  - [ ] 数据契约
  - [ ] 文件契约
- [ ] 实现契约生成
- [ ] 实现契约模板

### 契约验证
- [ ] 实现 `verifyContractConsistency()`
- [ ] 实现冲突检测
- [ ] 实现违反处理

### 契约状态管理
- [ ] 实现状态转换（draft → agreed → implemented → verified）
- [ ] 实现变更通知

---

## Phase 5: 集成与迁移

### UI 层适配
- [ ] 更新 `WebviewProvider` 事件处理
- [ ] 更新进度展示（显示 Todo 级别）
- [ ] 更新计划展示（显示 Assignment + Contract）
- [ ] 更新状态展示

### 兼容层
- [ ] 实现 `LegacyOrchestratorAdapter`
  - [ ] `execute()` 兼容
  - [ ] `createPlan()` 兼容
  - [ ] `executePlan()` 兼容
- [ ] 实现功能开关

### 测试
- [ ] 单元测试
  - [ ] Mission 创建/保存/加载
  - [ ] Contract 定义/验证
  - [ ] Worker 规划
  - [ ] Todo 执行
- [ ] 集成测试
  - [ ] 单 Worker 任务
  - [ ] 多 Worker 协作任务
  - [ ] 契约冲突场景
  - [ ] 动态 Todo 场景
- [ ] 端到端测试
  - [ ] 完整流程测试
  - [ ] 与 UI 集成测试

---

## Phase 6: 优化与稳定

### 性能优化
- [ ] 规划结果缓存
- [ ] 并行规划优化
- [ ] Prompt 优化（减少 Token）

### 错误处理
- [ ] 规划失败恢复
- [ ] 执行失败恢复
- [ ] 契约违反恢复
- [ ] 超时处理

### 可观测性
- [ ] 日志完善
- [ ] 事件发射完善
- [ ] 进度追踪完善

### 文档
- [ ] API 文档
- [ ] 使用指南
- [ ] 迁移指南

---

## 验收标准

### M1: 数据模型可用
- [ ] 能创建 Mission
- [ ] 能创建 Assignment
- [ ] 能创建 WorkerTodo
- [ ] 能保存/加载所有数据

### M2: Worker 自主规划
- [ ] Worker 能生成 Todo 列表
- [ ] Todo 列表通过审查
- [ ] 能检测超范围 Todo

### M3: 端到端流程
- [ ] 单 Worker 任务完成
- [ ] 多 Worker 协作任务完成
- [ ] 动态 Todo 添加工作正常

### M4: 契约系统
- [ ] 契约自动生成
- [ ] 契约验证通过
- [ ] 契约冲突检测有效

### M5: 生产就绪
- [ ] 所有测试通过
- [ ] 性能达标（单任务 < 2min）
- [ ] 无阻塞性 Bug
- [ ] 文档完备

---

## 注意事项

### 保留的组件
以下组件应保留并复用，不需重写：
- `IntentGate` - 意图门控
- `ProfileLoader` - 画像加载
- `GuidanceInjector` - 引导注入
- `VerificationRunner` - 验证执行
- `SnapshotManager` - 快照管理
- `ContextManager` - 上下文管理
- `MessageBus` - 消息总线
- `CLIAdapterFactory` - CLI 适配器工厂

### 需要重写的组件
- `OrchestratorAgent` → `MissionOrchestrator`
- `WorkerAgent` → `AutonomousWorker`
- `WorkerPool` → `WorkerCoordinator`
- `ExecutionPlan` → `Mission`
- `SubTask` → `Assignment` + `WorkerTodo`

### 需要增强的组件
- `PolicyEngine` → 拆分为 `ContractManager` + `AssignmentManager`
- `PlanStorage` → `MissionStorage`

---

## 变更日志

| 日期 | 变更 |
|------|------|
| 2026-01-19 | 创建检查清单 |
