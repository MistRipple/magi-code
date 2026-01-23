# 工具权限机制分析与改进建议

## 📅 分析日期：2025-01-22
## 🎯 分析范围：工具权限在不同交互模式下的行为

---

## 🔍 当前实现分析

### 1. 权限定义

**位置**: `src/types.ts`

```typescript
export interface PermissionMatrix {
  allowEdit: boolean;   // 允许文件编辑（Edit, Write, NotebookEdit）
  allowBash: boolean;   // 允许 Bash 命令执行
  allowWeb: boolean;    // 允许 Web 访问（WebFetch, WebSearch）
}
```

### 2. 交互模式配置

**位置**: `src/types.ts`

```typescript
export const INTERACTION_MODE_CONFIGS: Record<InteractionMode, InteractionModeConfig> = {
  ask: {
    mode: 'ask',
    allowFileModification: false,      // ❌ 不允许文件修改
    allowCommandExecution: false,      // ❌ 不允许命令执行
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  },
  agent: {
    mode: 'agent',
    allowFileModification: true,       // ✅ 允许文件修改
    allowCommandExecution: true,       // ✅ 允许命令执行
    requirePlanConfirmation: true,     // ✅ 需要计划确认
    requireRecoveryConfirmation: true, // ✅ 需要恢复确认
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  },
  auto: {
    mode: 'auto',
    allowFileModification: true,       // ✅ 允许文件修改
    allowCommandExecution: true,       // ✅ 允许命令执行
    requirePlanConfirmation: false,    // ❌ 不需要计划确认
    requireRecoveryConfirmation: false,// ❌ 不需要恢复确认
    autoRollbackOnFailure: true,       // ✅ 自动回滚
    maxFilesToModify: 0,
  },
};
```

### 3. 权限解析

**位置**: `src/orchestrator/intelligent-orchestrator.ts`

```typescript
private resolvePermissions(): PermissionMatrix {
  return {
    allowEdit: this.config.permissions?.allowEdit ?? true,
    allowBash: this.config.permissions?.allowBash ?? true,
    allowWeb: this.config.permissions?.allowWeb ?? true,
  };
}
```

**问题**:
- ❌ 权限解析**不考虑交互模式**
- ❌ 默认值都是 `true`，即使在 `ask` 模式下也允许所有操作
- ❌ `InteractionModeConfig` 中的 `allowFileModification` 和 `allowCommandExecution` **没有被使用**

### 4. 工具权限检查

**位置**: `src/tools/tool-manager.ts`

```typescript
private checkPermission(toolName: string): { allowed: boolean; reason?: string } {
  // Bash/Shell 工具需要 allowBash 权限
  if (toolName === 'Bash' || toolName === 'execute_shell') {
    if (!this.permissions.allowBash) {
      return { allowed: false, reason: 'Bash execution is disabled' };
    }
    return { allowed: true };
  }

  // Edit/Write 工具需要 allowEdit 权限
  if (toolName === 'Edit' || toolName === 'Write' || toolName === 'NotebookEdit') {
    if (!this.permissions.allowEdit) {
      return { allowed: false, reason: 'File editing is disabled' };
    }
    return { allowed: true };
  }

  // Web 相关工具需要 allowWeb 权限
  if (toolName === 'WebFetch' || toolName === 'WebSearch' || toolName.toLowerCase().includes('web')) {
    if (!this.permissions.allowWeb) {
      return { allowed: false, reason: 'Web access is disabled' };
    }
    return { allowed: true };
  }

  // 其他工具默认允许（Read, Grep, Glob 等只读工具）
  return { allowed: true };
}
```

**优点**:
- ✅ 权限检查逻辑清晰
- ✅ 只读工具默认允许
- ✅ 错误消息明确

**问题**:
- ❌ 权限矩阵不会根据交互模式自动调整

---

## ⚠️ 当前问题

### 问题 1: Ask 模式下工具权限不受限制

**现象**:
- 用户在 `ask` 模式下询问问题
- LLM 可能会尝试调用 `Edit`、`Bash` 等工具
- 由于默认权限都是 `true`，这些工具会被执行
- 违反了 `ask` 模式"仅对话，不执行"的设计原则

**根本原因**:
- `resolvePermissions()` 不考虑 `interactionMode`
- `InteractionModeConfig` 中的 `allowFileModification` 和 `allowCommandExecution` 没有被映射到 `PermissionMatrix`

### 问题 2: Agent 模式下没有工具授权提示

**现象**:
- 用户在 `agent` 模式下执行任务
- LLM 调用工具时，用户不知道哪些工具被使用
- 没有工具级别的授权确认

**期望行为**:
- 首次使用某个工具时，提示用户授权
- 用户可以选择"允许一次"、"总是允许"、"拒绝"
- 记住用户的授权选择

### 问题 3: Auto 模式下权限控制不明确

**现象**:
- `auto` 模式应该自动执行，但用户可能希望限制某些危险操作
- 例如：允许文件编辑，但不允许执行 Bash 命令

**期望行为**:
- 提供细粒度的权限配置
- 用户可以在设置中配置每个模式的默认权限

---

## 🎯 改进建议

### 改进 1: 根据交互模式自动设置权限 ⭐⭐⭐

**优先级**: 高

**目标**: 让权限矩阵自动反映交互模式的限制

**实现方案**:

```typescript
// src/orchestrator/intelligent-orchestrator.ts

private resolvePermissions(): PermissionMatrix {
  // 获取交互模式配置
  const modeConfig = INTERACTION_MODE_CONFIGS[this.interactionMode];

  // 根据交互模式设置默认权限
  const defaultPermissions: PermissionMatrix = {
    allowEdit: modeConfig.allowFileModification,
    allowBash: modeConfig.allowCommandExecution,
    allowWeb: true, // Web 访问在所有模式下默认允许（用于查询文档等）
  };

  // 用户配置可以覆盖默认值
  return {
    allowEdit: this.config.permissions?.allowEdit ?? defaultPermissions.allowEdit,
    allowBash: this.config.permissions?.allowBash ?? defaultPermissions.allowBash,
    allowWeb: this.config.permissions?.allowWeb ?? defaultPermissions.allowWeb,
  };
}

// 当交互模式改变时，更新权限
setInteractionMode(mode: InteractionMode): void {
  this.interactionMode = mode;
  this.modeConfig = INTERACTION_MODE_CONFIGS[mode];

  // 重新解析权限
  this.permissions = this.resolvePermissions();

  // 更新 MissionDrivenEngine 的权限
  this.missionDrivenEngine.updatePermissions(this.permissions);

  logger.info('编排器.交互_模式.变更', { mode, permissions: this.permissions }, LogCategory.ORCHESTRATOR);
  globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode, permissions: this.permissions } });

  this.syncPlanConfirmationPolicy();
  this.syncRecoveryConfirmationCallback();
}
```

**效果**:
- ✅ `ask` 模式：`allowEdit: false`, `allowBash: false`, `allowWeb: true`
- ✅ `agent` 模式：`allowEdit: true`, `allowBash: true`, `allowWeb: true`
- ✅ `auto` 模式：`allowEdit: true`, `allowBash: true`, `allowWeb: true`

### 改进 2: 添加工具授权提示机制 ⭐⭐

**优先级**: 中

**目标**: 在 Agent 模式下，首次使用工具时提示用户授权

**实现方案**:

```typescript
// src/tools/tool-manager.ts

export class ToolManager extends EventEmitter implements ToolExecutor {
  private permissions: PermissionMatrix;
  private toolAuthorizationCache: Map<string, 'allowed' | 'denied'> = new Map();
  private authorizationCallback?: (toolName: string) => Promise<'allow-once' | 'allow-always' | 'deny'>;

  /**
   * 设置工具授权回调
   */
  setAuthorizationCallback(callback: (toolName: string) => Promise<'allow-once' | 'allow-always' | 'deny'>): void {
    this.authorizationCallback = callback;
  }

  /**
   * 检查工具授权
   */
  private async checkAuthorization(toolName: string): Promise<{ allowed: boolean; reason?: string }> {
    // 1. 先检查基础权限
    const permissionCheck = this.checkPermission(toolName);
    if (!permissionCheck.allowed) {
      return permissionCheck;
    }

    // 2. 检查是否需要用户授权（仅对危险工具）
    const isDangerousTool = ['Bash', 'execute_shell', 'Edit', 'Write', 'NotebookEdit'].includes(toolName);
    if (!isDangerousTool) {
      return { allowed: true };
    }

    // 3. 检查授权缓存
    const cachedAuth = this.toolAuthorizationCache.get(toolName);
    if (cachedAuth === 'allowed') {
      return { allowed: true };
    }
    if (cachedAuth === 'denied') {
      return { allowed: false, reason: 'User denied tool authorization' };
    }

    // 4. 如果没有授权回调，默认允许
    if (!this.authorizationCallback) {
      return { allowed: true };
    }

    // 5. 请求用户授权
    try {
      const decision = await this.authorizationCallback(toolName);

      if (decision === 'allow-always') {
        this.toolAuthorizationCache.set(toolName, 'allowed');
        return { allowed: true };
      } else if (decision === 'allow-once') {
        return { allowed: true };
      } else {
        this.toolAuthorizationCache.set(toolName, 'denied');
        return { allowed: false, reason: 'User denied tool authorization' };
      }
    } catch (error) {
      return { allowed: false, reason: 'Authorization request failed' };
    }
  }

  async execute(toolCall: ToolCall): Promise<ToolResult> {
    logger.debug('Executing tool call', {
      toolName: toolCall.name,
      toolCallId: toolCall.id,
    }, LogCategory.TOOLS);

    try {
      // 检查授权（包括权限和用户授权）
      const authCheck = await this.checkAuthorization(toolCall.name);
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

**前端实现**:

```typescript
// src/ui/webview-provider.ts

// 设置工具授权回调
this.toolManager.setAuthorizationCallback(async (toolName: string) => {
  // 发送授权请求到前端
  this.postMessage({
    type: 'tool_authorization_request',
    toolName,
    description: this.getToolDescription(toolName),
  });

  // 等待用户响应
  return new Promise((resolve) => {
    const handler = (message: any) => {
      if (message.type === 'tool_authorization_response' && message.toolName === toolName) {
        resolve(message.decision);
        // 移除监听器
      }
    };
    // 添加临时监听器
  });
});
```

**效果**:
- ✅ 首次使用危险工具时，弹出授权对话框
- ✅ 用户可以选择"允许一次"、"总是允许"、"拒绝"
- ✅ 授权选择被缓存，避免重复提示

### 改进 3: 提供细粒度权限配置 ⭐

**优先级**: 低

**目标**: 允许用户在设置中配置每个模式的默认权限

**实现方案**:

```typescript
// package.json - 添加配置项

"contributes": {
  "configuration": {
    "title": "MultiCLI",
    "properties": {
      "multiCli.permissions.ask": {
        "type": "object",
        "default": {
          "allowEdit": false,
          "allowBash": false,
          "allowWeb": true
        },
        "description": "Ask 模式的默认权限"
      },
      "multiCli.permissions.agent": {
        "type": "object",
        "default": {
          "allowEdit": true,
          "allowBash": true,
          "allowWeb": true
        },
        "description": "Agent 模式的默认权限"
      },
      "multiCli.permissions.auto": {
        "type": "object",
        "default": {
          "allowEdit": true,
          "allowBash": false,  // 默认不允许 Bash，更安全
          "allowWeb": true
        },
        "description": "Auto 模式的默认权限"
      }
    }
  }
}
```

```typescript
// src/orchestrator/intelligent-orchestrator.ts

private resolvePermissions(): PermissionMatrix {
  // 从 VSCode 配置读取权限
  const config = vscode.workspace.getConfiguration('multiCli');
  const modePermissions = config.get<PermissionMatrix>(`permissions.${this.interactionMode}`);

  // 获取交互模式配置
  const modeConfig = INTERACTION_MODE_CONFIGS[this.interactionMode];

  // 默认权限
  const defaultPermissions: PermissionMatrix = {
    allowEdit: modeConfig.allowFileModification,
    allowBash: modeConfig.allowCommandExecution,
    allowWeb: true,
  };

  // 优先级：用户配置 > VSCode 设置 > 模式默认值
  return {
    allowEdit: this.config.permissions?.allowEdit ?? modePermissions?.allowEdit ?? defaultPermissions.allowEdit,
    allowBash: this.config.permissions?.allowBash ?? modePermissions?.allowBash ?? defaultPermissions.allowBash,
    allowWeb: this.config.permissions?.allowWeb ?? modePermissions?.allowWeb ?? defaultPermissions.allowWeb,
  };
}
```

**效果**:
- ✅ 用户可以在 VSCode 设置中配置每个模式的权限
- ✅ 更灵活的权限控制

---

## 📋 实现优先级

### 立即实现（高优先级）⭐⭐⭐

**改进 1: 根据交互模式自动设置权限**

**原因**:
- 这是一个**严重的安全问题**
- `ask` 模式应该是只读的，但当前可以执行所有操作
- 实现简单，影响大

**工作量**: 1-2 小时

**文件修改**:
- `src/orchestrator/intelligent-orchestrator.ts` - 修改 `resolvePermissions()` 和 `setInteractionMode()`
- `src/orchestrator/core/mission-driven-engine.ts` - 添加 `updatePermissions()` 方法
- 测试验证

### 后续实现（中优先级）⭐⭐

**改进 2: 添加工具授权提示机制**

**原因**:
- 提升用户体验
- 让用户了解哪些工具被使用
- 提供更细粒度的控制

**工作量**: 4-6 小时

**文件修改**:
- `src/tools/tool-manager.ts` - 添加授权机制
- `src/ui/webview-provider.ts` - 添加授权回调
- 前端 UI - 添加授权对话框
- 测试验证

### 可选实现（低优先级）⭐

**改进 3: 提供细粒度权限配置**

**原因**:
- 提供更多灵活性
- 满足高级用户需求

**工作量**: 2-3 小时

**文件修改**:
- `package.json` - 添加配置项
- `src/orchestrator/intelligent-orchestrator.ts` - 读取配置
- 文档更新

---

## 🎯 推荐实现方案

### 第一阶段：修复 Ask 模式权限问题（立即）

1. 修改 `resolvePermissions()` 方法，根据交互模式设置默认权限
2. 修改 `setInteractionMode()` 方法，切换模式时更新权限
3. 添加 `MissionDrivenEngine.updatePermissions()` 方法
4. 更新集成测试，验证不同模式下的权限

### 第二阶段：添加工具授权提示（可选）

1. 在 `ToolManager` 中添加授权机制
2. 在 `WebviewProvider` 中实现授权回调
3. 在前端添加授权对话框 UI
4. 添加授权缓存和持久化

### 第三阶段：细粒度权限配置（可选）

1. 在 `package.json` 中添加配置项
2. 修改权限解析逻辑，读取 VSCode 配置
3. 更新文档，说明权限配置

---

## 📝 总结

### 当前状态

- ❌ **严重问题**: Ask 模式下权限不受限制
- ⚠️ **体验问题**: Agent 模式下没有工具授权提示
- ℹ️ **功能缺失**: 缺少细粒度权限配置

### 建议行动

1. **立即修复**: 实现改进 1，确保 Ask 模式的安全性
2. **后续优化**: 根据用户反馈，考虑实现改进 2 和 3

### 预期效果

实现改进 1 后：
- ✅ Ask 模式：只读，不能执行任何修改操作
- ✅ Agent 模式：可以执行所有操作，但需要计划确认
- ✅ Auto 模式：可以执行所有操作，自动执行

---

**分析人**: AI Assistant
**分析日期**: 2025-01-22
**优先级**: 高（改进 1 应立即实现）
