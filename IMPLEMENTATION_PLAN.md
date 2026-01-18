# 实施计划：统一任务状态管理系统

**开始时间**: 2026-01-18 10:20
**预计完成**: 2-3 天
**优先级**: P0 (必须立即解决)

---

## 目标

将 TaskStateManager 的功能合并到 UnifiedTaskManager，消除双重状态管理，解决状态不一致风险。

---

## Stage 1: 扩展 UnifiedTaskManager 功能

**Goal**: 为 UnifiedTaskManager 添加 TaskStateManager 的核心功能

**Success Criteria**:
- ✅ SubTask 添加 attempts/maxAttempts 字段
- ✅ 添加 resetForRetry() 方法
- ✅ 添加状态变更回调机制
- ✅ 所有测试通过

**Tasks**:
1. [x] 扩展 SubTask 接口（types.ts）
   - ✅ retryCount 字段已存在
   - ✅ maxRetries 字段已存在

2. [x] 在 UnifiedTaskManager 中添加重试方法
   - ✅ `canRetrySubTask(taskId, subTaskId): boolean`
   - ✅ `resetSubTaskForRetry(taskId, subTaskId): Promise<void>`

3. [x] 添加状态变更回调机制
   - ✅ 添加 'subtask:retrying' 事件
   - ✅ 在状态变更时触发事件

4. [x] 更新状态转换逻辑
   - ✅ startSubTask 支持 'retrying' 状态
   - ✅ resetForRetry 时增加 retryCount 计数

**Tests**:
- [x] 测试 resetSubTaskForRetry 正确增加 retryCount ✅
- [x] 测试 canRetrySubTask 正确判断是否可重试 ✅
- [x] 测试达到最大重试次数时抛出错误 ✅
- [x] 测试 retryCount 递增 ✅
- [x] 测试完整重试流程 ✅

**Status**: ✅ Complete (2026-01-18 10:30)

---

## Stage 2: 统一 TaskStatus 类型（已完成）

**Goal**: 确保所有地方使用统一的 TaskStatus 定义

**Success Criteria**:
- ✅ TaskStatus 定义在 src/task/types.ts
- ✅ 所有模块导入统一的类型
- ✅ 编译无错误

**Status**: ✅ Complete

---

## Stage 3: 迁移 OrchestratorAgent

**Goal**: 修改 OrchestratorAgent 使用 UnifiedTaskManager 替代 TaskStateManager

**Success Criteria**:
- ✅ 移除 TaskStateManager 实例
- ✅ 使用 UnifiedTaskManager 的新方法
- ✅ 移除状态映射逻辑
- ✅ 所有功能正常工作

**Tasks**:
1. [x] 分析 OrchestratorAgent 中 TaskStateManager 的使用 ✅
   - ✅ 找出所有调用点（25 处）
   - ✅ 确定替代方案
   - ✅ 创建详细分析文档

2. [x] 添加 UnifiedTaskManager 支持 ✅
   - ✅ 添加导入和属性
   - ✅ 在 ensureContext 中初始化
   - ✅ 添加事件处理方法

3. [x] 替换方法调用（25 处）✅
   - ✅ 创建任务 (3 处)
   - ✅ 更新状态 (9 处)
   - ✅ 重试逻辑 (1 处)
   - ✅ 更新进度 (1 处)
   - ✅ 获取任务 (4 处)
   - ✅ 取消任务 (1 处)

4. [x] 移除状态映射逻辑 ✅
   - ✅ 删除 `applyTaskStateToTaskManager()`
   - ✅ 删除 `mapTaskStateStatus()`
   - ✅ 删除 `replayTaskStatesToTaskManager()`

5. [x] 删除 TaskStateManager ✅
   - ✅ 删除导入
   - ✅ 删除属性声明
   - ✅ 删除初始化代码
   - ✅ 删除所有调用（184 行）

**Tests**:
- [x] TypeScript 编译通过 ✅
- [ ] 测试任务创建流程 (Stage 5)
- [ ] 测试状态更新流程 (Stage 5)
- [ ] 测试重试流程 (Stage 5)
- [ ] 测试恢复流程 (Stage 4)

**Documents Created**:
- ✅ docs/Stage3-TaskStateManager使用分析.md
- ✅ docs/Stage3-迁移策略.md
- ✅ docs/Stage3-实施方案-最终版.md
- ✅ docs/Stage3.2-完成报告.md
- ✅ docs/Stage3.3-完成报告.md
- ✅ docs/Stage3.4-完成报告.md

**Status**: ✅ Complete (2026-01-18 11:00)

---

## Stage 4: 更新 RecoveryHandler

**Goal**: 修改 RecoveryHandler 使用 UnifiedTaskManager

**Success Criteria**:
- ✅ RecoveryHandler 使用 UnifiedTaskManager
- ✅ 恢复流程正常工作
- ✅ 所有测试通过

**Tasks**:
1. [x] 修改 RecoveryHandler 构造函数 ✅
   - ✅ 接受 UnifiedTaskManager 而不是 TaskStateManager

2. [x] 更新恢复方法 ✅
   - ✅ 使用 UnifiedTaskManager 的方法
   - ✅ 更新接口定义
   - ✅ 更新 16 个方法调用

3. [x] 更新类型定义 ✅
   - ✅ 将 TaskState 替换为 SubTask (9 处)
   - ✅ 更新 RecoveryConfirmationCallback

4. [x] 更新 OrchestratorAgent ✅
   - ✅ 删除 TaskState 导入
   - ✅ 更新方法签名

**Tests**:
- [x] TypeScript 编译通过 ✅
- [ ] 测试恢复策略选择 (Stage 5)
- [ ] 测试原 CLI 重试 (Stage 5)
- [ ] 测试升级到 Claude (Stage 5)
- [ ] 测试回滚 (Stage 5)

**Documents Created**:
- ✅ docs/Stage4-RecoveryHandler分析.md
- ✅ docs/Stage4-完成报告.md

**Status**: ✅ Complete (2026-01-18 11:30)

---

## Stage 5: 清理和测试

**Goal**: 清理旧代码，确保所有功能正常

**Success Criteria**:
- ✅ TaskStateManager 相关代码已移除或标记为废弃
- ✅ 所有测试通过
- ✅ 文档已更新

**Tasks**:
1. [ ] 标记 TaskStateManager 为废弃
   - 添加 @deprecated 注释
   - 保留代码作为备份

2. [ ] 运行完整测试套件
   - 单元测试
   - 集成测试
   - E2E 测试

3. [ ] 更新文档
   - 更新架构文档
   - 更新 API 文档
   - 创建迁移指南

**Tests**:
- [ ] 所有单元测试通过
- [ ] 所有集成测试通过
- [ ] E2E 测试通过

**Status**: Not Started

---

## 风险评估

### 高风险 🔴
- **状态同步逻辑错误** → 缓解：充分的集成测试
- **恢复机制失效** → 缓解：保留 TaskStateManager 代码作为备份

### 中风险 🟡
- **重构过程中引入新 bug** → 缓解：分阶段迁移，每阶段测试
- **测试覆盖不足** → 缓解：增加测试用例

### 低风险 🟢
- **性能影响** → 缓解：性能测试

---

## 回滚计划

如果出现严重问题，可以快速回滚：

1. 恢复 TaskStateManager 的使用
2. 恢复 OrchestratorAgent 的状态映射逻辑
3. 回滚代码到上一个稳定版本

---

## 进度跟踪

- [x] Stage 1: 扩展 UnifiedTaskManager 功能 ✅
- [x] Stage 2: 统一 TaskStatus 类型 ✅
- [x] Stage 3: 迁移 OrchestratorAgent ✅
- [x] Stage 4: 更新 RecoveryHandler ✅
- [ ] Stage 5: 清理和测试

**当前阶段**: Stage 5
**完成度**: 80% (4/5)

---

**最后更新**: 2026-01-18 11:30
