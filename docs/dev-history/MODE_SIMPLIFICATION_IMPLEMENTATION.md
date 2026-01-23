# 交互模式简化实施计划

## 📅 实施日期：2025-01-22
## 🎯 目标：移除 agent 模式，简化为 ask + auto 两种模式

---

## 🎯 最终方案

### Ask 模式
- **可以调用所有工具**
- **每次调用工具都需要用户授权**
- 用途：探索性任务、需要确认的操作

### Auto 模式
- **可以调用所有工具**
- **完全自动执行，不需要任何确认**
- 用途：明确的任务、自动化工作流

---

## 📋 实施步骤

### 阶段 1: 类型定义修改

**文件**: `src/types.ts`

1. 修改 `InteractionMode` 类型
2. 移除 `agent` 相关配置
3. 更新 `INTERACTION_MODE_CONFIGS`

### 阶段 2: 工具授权机制

**文件**: `src/tools/tool-manager.ts`

1. 添加工具授权回调
2. 实现授权检查逻辑
3. Ask 模式：每次调用工具都请求授权
4. Auto 模式：直接执行，不需要授权

### 阶段 3: 编排器更新

**文件**: `src/orchestrator/intelligent-orchestrator.ts`

1. 移除 `agent` 模式相关逻辑
2. 更新模式判断
3. 根据模式设置工具授权回调

### 阶段 4: UI 更新

**文件**: `src/ui/webview-provider.ts` 和前端文件

1. 移除 `agent` 模式选项
2. 添加工具授权对话框
3. 更新模式切换 UI

### 阶段 5: 测试更新

**文件**: `src/test/integration-e2e.test.ts`

1. 移除 `agent` 模式测试
2. 添加工具授权测试

### 阶段 6: 文档更新

1. 更新 README
2. 更新用户文档
3. 创建迁移指南

---

## 🔧 详细实现

### 1. 类型定义 (`src/types.ts`)

```typescript
/**
 * 交互模式
 * - ask: 对话模式，可以调用工具，但每次都需要用户授权
 * - auto: 自动模式，完全自动执行，不需要确认
 */
export type InteractionMode = 'ask' | 'auto';

/**
 * 交互模式配置
 */
export interface InteractionModeConfig {
  mode: InteractionMode;
  /** 是否允许文件修改 */
  allowFileModification: boolean;
  /** 是否允许命令执行 */
  allowCommandExecution: boolean;
  /** 是否需要工具授权 */
  requireToolAuthorization: boolean;
  /** 是否需要计划确认 */
  requirePlanConfirmation: boolean;
  /** 是否需要恢复确认 */
  requireRecoveryConfirmation: boolean;
  /** 失败时是否自动回滚 */
  autoRollbackOnFailure: boolean;
}

/**
 * 预设交互模式配置
 */
export const INTERACTION_MODE_CONFIGS: Record<InteractionMode, InteractionModeConfig> = {
  ask: {
    mode: 'ask',
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: true,   // ✅ 需要工具授权
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
  },
  auto: {
    mode: 'auto',
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: false,  // ❌ 不需要工具授权
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
  },
};
```

### 2. 工具授权机制 (`src/tools/tool-manager.ts`)

```typescript
export class ToolManager extends EventEmitter implements ToolExecutor {
  private permissions: PermissionMatrix;
  private authorizationCallback?: (toolName: string, toolArgs: any) => Promise<boolean>;

  /**
   * 设置工具授权回调
   */
  setAuthorizationCallback(callback: (toolName: string, toolArgs: any) => Promise<boolean>): void {
    this.authorizationCallback = callback;
  }

  /**
   * 检查工具授权
   */
  private async checkAuthorization(toolCall: ToolCall): Promise<{ allowed: boolean; reason?: string }> {
    // 1. 先检查基础权限
    const permissionCheck = this.checkPermission(toolCall.name);
    if (!permissionCheck.allowed) {
      return permissionCheck;
    }

    // 2. 如果没有授权回调，默认允许（Auto 模式）
    if (!this.authorizationCallback) {
      return { allowed: true };
    }

    // 3. 请求用户授权（Ask 模式）
    try {
      const allowed = await this.authorizationCallback(toolCall.name, toolCall.arguments);
      if (!allowed) {
        return { allowed: false, reason: 'User denied tool authorization' };
      }
      return { allowed: true };
    } catch (error) {
      return { allowed: false, reason: 'Authorization request failed' };
    }
  }

  async execute(toolCall: ToolCall): Promise<ToolResult> {
    try {
      // 检查授权（包括权限和用户授权）
      const authCheck = await this.checkAuthorization(toolCall);
      if (!authCheck.allowed) {
        logger.warn('Tool execution blocked', {
          toolName: toolCall.name,
          reason: authCheck.reason,
        }, LogCategory.TOOLS);
        return {
          toolCallId: toolCall.id,
          content: `Tool blocked: ${authCheck.reason}`,
          isError: true,
        };
      }

      // ... 执行工具
    } catch (error: any) {
      // ... 错误处理
    }
  }
}
```

### 3. 编排器更新 (`src/orchestrator/intelligent-orchestrator.ts`)

```typescript
setInteractionMode(mode: InteractionMode): void {
  this.interactionMode = mode;
  this.modeConfig = INTERACTION_MODE_CONFIGS[mode];

  // 根据模式设置工具授权回调
  if (mode === 'ask') {
    // Ask 模式：设置授权回调
    this.toolManager.setAuthorizationCallback(async (toolName, toolArgs) => {
      return await this.requestToolAuthorization(toolName, toolArgs);
    });
  } else {
    // Auto 模式：移除授权回调
    this.toolManager.setAuthorizationCallback(undefined);
  }

  logger.info('编排器.交互_模式.变更', { mode }, LogCategory.ORCHESTRATOR);
  globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode } });

  this.syncPlanConfirmationPolicy();
  this.syncRecoveryConfirmationCallback();
}

private async requestToolAuthorization(toolName: string, toolArgs: any): Promise<boolean> {
  // 发送授权请求到前端
  return new Promise((resolve) => {
    globalEventBus.emitEvent('tool:authorization_request', {
      data: {
        toolName,
        toolArgs,
        callback: (allowed: boolean) => {
          resolve(allowed);
        },
      },
    });
  });
}
```

---

## 📊 需要修改的文件清单

| 文件 | 修改内容 | 优先级 |
|------|---------|--------|
| `src/types.ts` | 移除 agent，更新配置 | 高 |
| `src/tools/tool-manager.ts` | 添加授权机制 | 高 |
| `src/orchestrator/intelligent-orchestrator.ts` | 更新模式逻辑 | 高 |
| `src/orchestrator/core/mission-driven-engine.ts` | 移除 agent 引用 | 中 |
| `src/ui/webview-provider.ts` | 添加授权处理 | 高 |
| 前端 UI 文件 | 更新模式选择器，添加授权对话框 | 高 |
| `src/test/integration-e2e.test.ts` | 更新测试 | 中 |
| 文档 | 更新说明 | 低 |

---

## ⏱️ 预计工作量

- 阶段 1: 类型定义修改 - 30 分钟
- 阶段 2: 工具授权机制 - 2 小时
- 阶段 3: 编排器更新 - 1 小时
- 阶段 4: UI 更新 - 2 小时
- 阶段 5: 测试更新 - 1 小时
- 阶段 6: 文档更新 - 30 分钟

**总计**: 约 7 小时

---

## 🎯 开始实施

准备开始实施，按照以下顺序：
1. 类型定义修改
2. 工具授权机制
3. 编排器更新
4. 编译验证
5. UI 更新（后续）
6. 测试更新（后续）

---

**实施人**: AI Assistant
**实施日期**: 2025-01-22
**状态**: 准备开始
