# 模型连接状态扩展 - 添加编排者和压缩模型

## 📋 需求

用户要求：
1. 在模型连接状态中添加编排者模型
2. 在模型连接状态中添加压缩模型
3. 标记编排者和压缩模型为"必需"

## ✅ 实施内容

### 1. 后端扩展（src/ui/webview-provider.ts）

**位置**: 行 2156-2276

**核心改进**:
- ✅ 重构测试逻辑为通用函数 `testModel()`
- ✅ 添加 `isRequired` 参数标记必需模型
- ✅ 扩展测试范围：3 个 Worker + 编排者 + 压缩模型（共 5 个）
- ✅ 并行测试所有 5 个模型

**关键代码**:

```typescript
// 测试模型的通用函数
const testModel = async (name: string, modelConfig: any, isRequired: boolean = false) => {
  // 1. 检查是否启用
  if (!modelConfig.enabled) {
    statuses[name] = { status: 'disabled', version: '已禁用' };
    return;
  }

  // 2. 检查配置完整性
  if (!modelConfig.apiKey || !modelConfig.model) {
    statuses[name] = {
      status: 'not_configured',
      version: isRequired ? '未配置（必需）' : '未配置'  // ✅ 必需标记
    };
    return;
  }

  // 3. 发送真实测试请求
  // ... 测试逻辑
};

// 并行测试所有模型
const testPromises = [
  // 测试 3 个 Worker
  ...(['claude', 'codex', 'gemini'] as WorkerSlot[]).map(worker =>
    testModel(worker, config.workers[worker])
  ),
  // 测试编排者（必需）
  testModel('orchestrator', config.orchestrator, true),
  // 测试压缩模型（必需）
  testModel('compressor', config.compressor, true)
];

await Promise.all(testPromises);
```

### 2. 类型定义扩展（src/llm/types.ts）

**位置**: 行 154-158

**修改前**:
```typescript
export interface FullLLMConfig {
  orchestrator: LLMConfig;
  workers: WorkerLLMConfig;
}
```

**修改后**:
```typescript
export interface FullLLMConfig {
  orchestrator: LLMConfig;
  workers: WorkerLLMConfig;
  compressor: LLMConfig;  // ✅ 新增
}
```

### 3. 配置加载扩展（src/llm/config.ts）

**位置**: 行 32-40

**修改前**:
```typescript
static loadFullConfig(): FullLLMConfig {
  const config = this.loadLLMConfigFile();

  return {
    orchestrator: this.extractOrchestratorConfig(config),
    workers: this.extractWorkersConfig(config),
  };
}
```

**修改后**:
```typescript
static loadFullConfig(): FullLLMConfig {
  const config = this.loadLLMConfigFile();

  return {
    orchestrator: this.extractOrchestratorConfig(config),
    workers: this.extractWorkersConfig(config),
    compressor: this.loadCompressorConfig(),  // ✅ 新增
  };
}
```

### 4. 前端 HTML 扩展（src/ui/webview/index.html）

**位置**: 行 2415-2469

**新增内容**:

```html
<!-- 编排者模型 -->
<div class="cli-connection-item" data-cli="orchestrator">
  <div class="cli-connection-icon orchestrator">
    <svg viewBox="0 0 16 16">...</svg>
  </div>
  <div class="cli-connection-info">
    <div class="cli-connection-name">
      编排者 <span style="color: var(--vscode-errorForeground); font-size: 10px;">(必需)</span>
    </div>
    <div class="cli-connection-status">检测中...</div>
  </div>
  <div class="cli-connection-badge checking">检测中</div>
</div>

<!-- 压缩模型 -->
<div class="cli-connection-item" data-cli="compressor">
  <div class="cli-connection-icon compressor">
    <svg viewBox="0 0 16 16">...</svg>
  </div>
  <div class="cli-connection-info">
    <div class="cli-connection-name">
      压缩模型 <span style="color: var(--vscode-errorForeground); font-size: 10px;">(必需)</span>
    </div>
    <div class="cli-connection-status">检测中...</div>
  </div>
  <div class="cli-connection-badge checking">检测中</div>
</div>
```

### 5. CSS 样式扩展（src/ui/webview/index.html）

**位置**: 行 1883-1887

**新增样式**:
```css
.cli-connection-icon.orchestrator {
  background: rgba(168, 85, 247, 0.15);
  color: #a855f7;
}

.cli-connection-icon.compressor {
  background: rgba(236, 72, 153, 0.15);
  color: #ec4899;
}
```

**颜色方案**:
- Claude: 🟠 橙色 (#d97706)
- Codex: 🟢 绿色 (#22c55e)
- Gemini: 🔵 蓝色 (#3b82f6)
- 编排者: 🟣 紫色 (#a855f7)
- 压缩模型: 🌸 粉色 (#ec4899)

### 6. JavaScript 逻辑扩展（src/ui/webview/index.html）

**位置**: 行 9156-9229

**修改前**:
```javascript
['claude', 'codex', 'gemini'].forEach(cli => {
  // 更新逻辑
});
```

**修改后**:
```javascript
// 更新所有模型状态（Worker + 编排者 + 压缩模型）
['claude', 'codex', 'gemini', 'orchestrator', 'compressor'].forEach(cli => {
  // 更新逻辑
});
```

## 📊 测试流程

```
用户点击"重新检测"
    ↓
后端加载配置
    ↓
并行测试 5 个模型
    ├─ Claude: 发送 "Hello" → 等待响应
    ├─ Codex: 发送 "Hello" → 等待响应
    ├─ Gemini: 发送 "Hello" → 等待响应
    ├─ 编排者: 发送 "Hello" → 等待响应
    └─ 压缩模型: 发送 "Hello" → 等待响应
    ↓
收集所有测试结果
    ↓
分类错误类型
    ↓
发送 cliStatusUpdate 消息到前端
    ↓
前端更新 UI（5 个模型卡片）
```

## 🎯 状态类型

所有 5 个模型支持相同的状态类型：

| 状态 | 含义 | 显示文本 | 颜色 |
|------|------|----------|------|
| `available` | 连接成功 | 已连接 | 🟢 绿色 |
| `disabled` | 已禁用 | 已禁用 | ⚪ 灰色 |
| `not_configured` | 配置不完整 | 未配置 / 未配置（必需） | 🟡 黄色 |
| `auth_failed` | API Key 无效 | 认证失败 | 🔴 红色 |
| `network_error` | 网络错误 | 网络错误 | 🔴 红色 |
| `timeout` | 连接超时 | 连接超时 | 🔴 红色 |
| `invalid_model` | 模型无效 | 模型无效 | 🔴 红色 |
| `unknown` | 未知错误 | 未知错误 | 🔴 红色 |

**特殊处理**:
- 编排者和压缩模型的 `not_configured` 状态显示为"未配置（必需）"
- 名称后面显示红色的"(必需)"标签

## 📝 影响范围

### 修改的文件
1. `src/ui/webview-provider.ts` - 后端测试逻辑
2. `src/llm/types.ts` - 类型定义
3. `src/llm/config.ts` - 配置加载
4. `src/ui/webview/index.html` - 前端 UI、CSS、JavaScript

### 新增内容
- 2 个新的模型卡片（HTML）
- 2 个新的图标样式（CSS）
- 扩展的测试逻辑（TypeScript）
- 扩展的更新逻辑（JavaScript）

## ✅ 验收标准

- [x] 编译通过（0 错误）
- [x] 类型定义完整（FullLLMConfig 包含 compressor）
- [x] 配置加载正确（loadFullConfig 返回 compressor）
- [x] 后端测试逻辑支持 5 个模型
- [x] 前端 HTML 包含 5 个模型卡片
- [x] CSS 样式支持 5 个模型图标
- [x] JavaScript 更新逻辑支持 5 个模型
- [ ] 用户功能测试（待用户测试）

## 🎉 完成状态

**状态**: ✅ 代码实现完成
**编译结果**: ✅ 通过（0 错误）
**下一步**: 用户在 VS Code 中测试

## 📋 用户测试步骤

1. 重启插件（或重新加载窗口）
2. 打开配置面板 → 统计 Tab
3. 点击"重新检测"按钮
4. 查看 5 个模型的连接状态：
   - Claude
   - Codex
   - Gemini
   - 编排者（必需）
   - 压缩模型（必需）

## 🔑 关键特性

1. **统一测试逻辑**: 所有模型使用相同的测试函数
2. **必需标记**: 编排者和压缩模型标记为"必需"
3. **并行测试**: 5 个模型同时测试，提高速度
4. **真实连接**: 发送实际 API 请求验证连接
5. **详细反馈**: 显示具体的错误信息和模型版本

---

**实施时间**: 2024年
**完成时间**: 2024年
