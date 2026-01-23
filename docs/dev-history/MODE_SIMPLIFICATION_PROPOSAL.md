# 交互模式简化方案

## 📅 提议日期：2025-01-22
## 🎯 目标：简化交互模式，降低用户理解难度

---

## 🤔 当前问题

### 三种模式的复杂性

当前有三种模式：`ask`、`agent`、`auto`

| 模式 | 用途 | 文件修改 | 命令执行 | 计划确认 | 恢复确认 | 自动回滚 |
|------|------|---------|---------|---------|---------|---------|
| **ask** | 仅对话 | ❌ | ❌ | ❌ | ❌ | ❌ |
| **agent** | 任务执行（需确认） | ✅ | ✅ | ✅ | ✅ | ❌ |
| **auto** | 自动执行 | ✅ | ✅ | ❌ | ❌ | ✅ |

**问题**：
1. **用户困惑**：`agent` 和 `auto` 的区别不够明显
2. **选择困难**：用户不知道什么时候用 `agent`，什么时候用 `auto`
3. **维护成本**：三种模式增加了代码复杂度

---

## 💡 简化方案：保留 Ask + Auto

### 方案概述

**只保留两种模式**：
- **Ask 模式**：纯对话，不执行任何操作
- **Auto 模式**：自动执行，包含原 `agent` 的确认机制

### 新的模式定义

| 模式 | 用途 | 文件修改 | 命令执行 | 计划确认 | 恢复确认 | 自动回滚 |
|------|------|---------|---------|---------|---------|---------|
| **ask** | 仅对话咨询 | ❌ | ❌ | ❌ | ❌ | ❌ |
| **auto** | 智能执行 | ✅ | ✅ | ✅ (可配置) | ✅ (可配置) | ✅ (可配置) |

### Auto 模式的增强

**核心思想**：Auto 模式不是"无脑自动"，而是"智能自动"

**配置选项**：
```typescript
export interface AutoModeConfig {
  // 是否需要计划确认（默认：true，更安全）
  requirePlanConfirmation: boolean;

  // 是否需要恢复确认（默认：true）
  requireRecoveryConfirmation: boolean;

  // 失败时是否自动回滚（默认：true）
  autoRollbackOnFailure: boolean;

  // 最大修改文件数（0 = 无限制）
  maxFilesToModify: number;

  // 危险操作需要确认（默认：true）
  confirmDangerousOperations: boolean;
}
```

**默认配置**（安全优先）：
```typescript
const DEFAULT_AUTO_CONFIG: AutoModeConfig = {
  requirePlanConfirmation: true,      // ✅ 需要确认计划
  requireRecoveryConfirmation: true,  // ✅ 需要确认恢复
  autoRollbackOnFailure: true,        // ✅ 自动回滚
  maxFilesToModify: 0,                // 无限制
  confirmDangerousOperations: true,   // ✅ 危险操作需确认
};
```

**高级用户配置**（完全自动）：
```typescript
const FULL_AUTO_CONFIG: AutoModeConfig = {
  requirePlanConfirmation: false,     // ❌ 不需要确认
  requireRecoveryConfirmation: false, // ❌ 不需要确认
  autoRollbackOnFailure: true,        // ✅ 自动回滚
  maxFilesToModify: 10,               // 限制修改文件数
  confirmDangerousOperations: false,  // ❌ 不需要确认
};
```

---

## 🎯 优势分析

### 1. 用户体验改善

**简化决策**：
- ❓ 之前：我应该用 `agent` 还是 `auto`？
- ✅ 现在：想对话用 `ask`，想执行用 `auto`

**清晰的心智模型**：
- `ask` = 只说不做
- `auto` = 智能执行（可配置安全级别）

### 2. 降低学习成本

**之前**：
```
用户需要理解：
1. ask 是什么？
2. agent 是什么？
3. auto 是什么？
4. agent 和 auto 有什么区别？
5. 什么时候用 agent？
6. 什么时候用 auto？
```

**现在**：
```
用户只需理解：
1. ask = 对话咨询
2. auto = 执行任务（可配置安全级别）
```

### 3. 灵活性不减

通过配置选项，Auto 模式可以覆盖原来 `agent` 和 `auto` 的所有场景：

**场景 1：谨慎执行（原 agent 模式）**
```typescript
{
  requirePlanConfirmation: true,
  requireRecoveryConfirmation: true,
  autoRollbackOnFailure: true,
  confirmDangerousOperations: true,
}
```

**场景 2：完全自动（原 auto 模式）**
```typescript
{
  requirePlanConfirmation: false,
  requireRecoveryConfirmation: false,
  autoRollbackOnFailure: true,
  confirmDangerousOperations: false,
}
```

**场景 3：平衡模式（推荐）**
```typescript
{
  requirePlanConfirmation: true,      // 确认计划
  requireRecoveryConfirmation: false, // 不确认恢复
  autoRollbackOnFailure: true,        // 自动回滚
  confirmDangerousOperations: true,   // 确认危险操作
}
```

---

## 🔧 实现方案

### 1. 类型定义修改

```typescript
// src/types.ts

/**
 * 交互模式
 * - ask: 对话模式，仅对话交流，不执行代码编辑
 * - auto: 自动模式，智能执行任务（可配置确认级别）
 */
export type InteractionMode = 'ask' | 'auto';

/**
 * Auto 模式配置
 */
export interface AutoModeConfig {
  /** 是否需要计划确认 */
  requirePlanConfirmation: boolean;
  /** 是否需要恢复确认 */
  requireRecoveryConfirmation: boolean;
  /** 失败时是否自动回滚 */
  autoRollbackOnFailure: boolean;
  /** 最大修改文件数（0 = 无限制） */
  maxFilesToModify: number;
  /** 危险操作需要确认 */
  confirmDangerousOperations: boolean;
}

/**
 * 交互模式配置
 */
export interface InteractionModeConfig {
  mode: InteractionMode;
  /** 是否允许文件修改 */
  allowFileModification: boolean;
  /** 是否允许命令执行 */
  allowCommandExecution: boolean;
  /** Auto 模式配置（仅 auto 模式有效） */
  autoConfig?: AutoModeConfig;
}

/**
 * 预设交互模式配置
 */
export const INTERACTION_MODE_CONFIGS: Record<InteractionMode, InteractionModeConfig> = {
  ask: {
    mode: 'ask',
    allowFileModification: false,
    allowCommandExecution: false,
  },
  auto: {
    mode: 'auto',
    allowFileModification: true,
    allowCommandExecution: true,
    autoConfig: {
      requirePlanConfirmation: true,      // 默认需要确认（安全）
      requireRecoveryConfirmation: true,
      autoRollbackOnFailure: true,
      maxFilesToModify: 0,
      confirmDangerousOperations: true,
    },
  },
};
```

### 2. VSCode 配置

```json
// package.json

"contributes": {
  "configuration": {
    "title": "MultiCLI",
    "properties": {
      "multiCli.interactionMode": {
        "type": "string",
        "enum": ["ask", "auto"],
        "default": "auto",
        "description": "交互模式：ask（仅对话）或 auto（智能执行）"
      },
      "multiCli.autoMode.requirePlanConfirmation": {
        "type": "boolean",
        "default": true,
        "description": "Auto 模式：是否需要确认执行计划"
      },
      "multiCli.autoMode.requireRecoveryConfirmation": {
        "type": "boolean",
        "default": true,
        "description": "Auto 模式：是否需要确认恢复操作"
      },
      "multiCli.autoMode.autoRollbackOnFailure": {
        "type": "boolean",
        "default": true,
        "description": "Auto 模式：失败时是否自动回滚"
      },
      "multiCli.autoMode.maxFilesToModify": {
        "type": "number",
        "default": 0,
        "description": "Auto 模式：最大修改文件数（0 = 无限制）"
      },
      "multiCli.autoMode.confirmDangerousOperations": {
        "type": "boolean",
        "default": true,
        "description": "Auto 模式：危险操作是否需要确认"
      }
    }
  }
}
```

### 3. UI 改进

**模式切换器**：
```
┌─────────────────────────────────┐
│  模式：                          │
│  ○ Ask  - 对话咨询              │
│  ● Auto - 智能执行              │
│                                 │
│  Auto 模式设置：                │
│  ☑ 确认执行计划                │
│  ☑ 确认恢复操作                │
│  ☑ 自动回滚失败                │
│  ☑ 确认危险操作                │
│                                 │
│  [应用] [重置为默认]            │
└─────────────────────────────────┘
```

---

## 📊 迁移影响分析

### 需要修改的文件

| 文件 | 修改内容 | 影响 |
|------|---------|------|
| `src/types.ts` | 移除 `agent` 模式定义 | 中 |
| `src/orchestrator/intelligent-orchestrator.ts` | 更新模式判断逻辑 | 中 |
| `src/ui/webview-provider.ts` | 更新 UI 和配置读取 | 中 |
| `package.json` | 更新配置项 | 小 |
| 前端 UI 文件 | 更新模式选择器 | 中 |
| 文档 | 更新说明 | 小 |

### 向后兼容

**策略**：自动迁移

```typescript
// 如果用户之前使用 agent 模式，自动迁移到 auto 模式（带确认）
function migrateMode(oldMode: string): InteractionMode {
  if (oldMode === 'agent') {
    // 迁移到 auto 模式，并设置为需要确认
    return 'auto';
  }
  return oldMode as InteractionMode;
}
```

---

## 🎯 推荐实施步骤

### 阶段 1：准备（1 小时）
1. 创建迁移计划文档
2. 备份当前配置
3. 通知用户即将变更

### 阶段 2：实现（3-4 小时）
1. 修改类型定义
2. 更新 IntelligentOrchestrator
3. 更新 WebviewProvider
4. 更新前端 UI
5. 添加配置迁移逻辑

### 阶段 3：测试（2 小时）
1. 测试 Ask 模式
2. 测试 Auto 模式（各种配置）
3. 测试配置迁移
4. 更新集成测试

### 阶段 4：文档（1 小时）
1. 更新用户文档
2. 更新 README
3. 创建迁移指南

**总工作量**：6-8 小时

---

## 📝 用户沟通

### 变更公告

```markdown
# MultiCLI 模式简化更新

我们简化了交互模式，让 MultiCLI 更易用！

## 变更内容

**之前**：三种模式（ask、agent、auto）
**现在**：两种模式（ask、auto）

## 新模式说明

### Ask 模式 💬
- 纯对话咨询
- 不执行任何操作
- 适合：询问问题、获取建议

### Auto 模式 🚀
- 智能执行任务
- 可配置安全级别
- 适合：实现功能、修复 Bug、重构代码

## Auto 模式配置

您可以在设置中配置 Auto 模式的行为：
- ✅ 确认执行计划（推荐）
- ✅ 确认恢复操作（推荐）
- ✅ 自动回滚失败（推荐）
- ✅ 确认危险操作（推荐）

## 迁移说明

如果您之前使用 **agent 模式**：
- 自动迁移到 **auto 模式**
- 默认开启所有确认选项（与 agent 行为一致）

如果您之前使用 **auto 模式**：
- 保持不变
- 可以在设置中调整确认选项
```

---

## 🎉 总结

### 优势

1. ✅ **更简单**：从 3 种模式减少到 2 种
2. ✅ **更清晰**：Ask = 对话，Auto = 执行
3. ✅ **更灵活**：Auto 模式可配置，覆盖所有场景
4. ✅ **更安全**：默认配置更安全（需要确认）
5. ✅ **易迁移**：自动迁移，用户无感知

### 风险

1. ⚠️ **用户习惯**：需要适应新的模式名称
2. ⚠️ **配置复杂**：Auto 模式的配置选项较多

### 缓解措施

1. 提供清晰的迁移指南
2. 提供预设配置（安全、平衡、完全自动）
3. 在 UI 中提供配置向导

---

**提议人**: AI Assistant
**提议日期**: 2025-01-22
**建议**: 强烈推荐实施，能显著改善用户体验
