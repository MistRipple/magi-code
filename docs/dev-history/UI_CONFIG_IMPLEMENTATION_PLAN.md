# 配置面板完整实施计划

## 🎯 目标

实现完整可用的配置面板，包括：
1. ✅ 前端 UI 重组（4 Tab 结构）
2. ✅ 后端消息处理（配置加载/保存/测试）
3. ✅ 数据持久化（~/.multicli/llm.json）
4. ✅ 配置验证和错误处理
5. ✅ 实时生效（无需重启）

---

## 📊 系统架构分析

### 当前配置系统

```
┌─────────────────────────────────────────────────────────┐
│                    Webview (前端)                        │
│  - index.html (UI 表单)                                  │
│  - JavaScript (事件处理)                                 │
│  - postMessage() 发送到后端                              │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│              WebviewProvider (消息路由)                  │
│  - handleMessage() 处理前端消息                          │
│  - 调用配置管理器                                        │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│              LLMConfigLoader (配置管理)                  │
│  - loadFullConfig() 加载配置                             │
│  - saveLLMConfigFile() 保存配置                          │
│  - validateConfig() 验证配置                             │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│              ~/.multicli/llm.json (持久化)               │
│  - augment, orchestrator, workers, compressor            │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│              LLMAdapterFactory (使用配置)                │
│  - createWorkerAdapter() 创建 Worker                     │
│  - createOrchestratorAdapter() 创建编排者                │
└─────────────────────────────────────────────────────────┘
```

### 缺失的部分 ⚠️

1. **WebviewProvider 消息处理** - 没有处理配置相关消息
2. **前端数据绑定** - 没有加载和显示配置数据
3. **测试连接功能** - 没有实现 LLM 连接测试
4. **配置热更新** - 修改配置后需要重启

---

## 🚀 实施步骤

### Phase 1: 前端 UI 重组（2-3 小时）

#### 1.1 修改 Tab 结构

**文件**: `src/ui/webview/index.html`

**操作**:
```html
<!-- 删除旧的 Tab -->
❌ <button class="settings-tab" data-tab="orchestrator">编排者</button>
❌ <button class="settings-tab" data-tab="mcp">MCP</button>
❌ <button class="settings-tab" data-tab="skills">技能</button>
❌ <button class="settings-tab" data-tab="config">配置</button>

<!-- 新增 Tab -->
✅ <button class="settings-tab" data-tab="model">
     <svg>...</svg>
     模型
   </button>
✅ <button class="settings-tab" data-tab="tools">
     <svg>...</svg>
     工具
   </button>
```

#### 1.2 创建"模型 Tab"内容

**新增内容**:
```html
<div class="settings-tab-content" id="settings-tab-model">
  <!-- 编排者模型配置 -->
  <div class="settings-section">
    <div class="settings-section-title">编排者模型配置</div>
    <div class="llm-config-form">
      <!-- 从原"编排者 Tab"迁移 -->
    </div>
  </div>

  <!-- Worker 模型配置（新增） -->
  <div class="settings-section">
    <div class="settings-section-title">Worker 模型配置</div>

    <!-- Worker 选择器 -->
    <div class="worker-model-tabs">
      <button class="worker-model-tab active" data-worker="claude">
        <span class="worker-dot claude"></span>
        Claude
      </button>
      <button class="worker-model-tab" data-worker="codex">
        <span class="worker-dot codex"></span>
        Codex
      </button>
      <button class="worker-model-tab" data-worker="gemini">
        <span class="worker-dot gemini"></span>
        Gemini
      </button>
    </div>

    <!-- Worker 配置表单 -->
    <div class="llm-config-form" id="worker-model-config">
      <div class="llm-config-field">
        <label class="llm-config-label">Base URL</label>
        <input type="text" class="llm-config-input" id="worker-base-url" placeholder="https://api.anthropic.com">
      </div>
      <div class="llm-config-field">
        <label class="llm-config-label">API Key</label>
        <div class="llm-config-input-wrap">
          <input type="password" class="llm-config-input" id="worker-api-key" placeholder="sk-...">
          <button class="llm-config-eye-btn" data-target="worker-api-key">
            <svg>...</svg>
          </button>
        </div>
      </div>
      <div class="llm-config-field">
        <label class="llm-config-label">Model</label>
        <input type="text" class="llm-config-input" id="worker-model" placeholder="claude-3-5-sonnet-20241022">
      </div>
      <div class="llm-config-field">
        <label class="llm-config-label">Provider</label>
        <select class="llm-config-select" id="worker-provider">
          <option value="openai">OpenAI</option>
          <option value="anthropic">Anthropic</option>
        </select>
      </div>
      <div class="llm-config-field">
        <label class="llm-config-toggle-label">
          <input type="checkbox" id="worker-enabled" checked>
          <span>启用此 Worker</span>
        </label>
      </div>
      <button class="llm-config-test-btn" id="worker-test-btn">
        <svg>...</svg>
        测试连接
      </button>
    </div>
  </div>

  <!-- 压缩模型配置 -->
  <div class="settings-section">
    <div class="settings-section-title">压缩模型配置</div>
    <!-- 从原"编排者 Tab"迁移 -->
  </div>
</div>
```

#### 1.3 创建"工具 Tab"内容

**新增内容**:
```html
<div class="settings-tab-content" id="settings-tab-tools">
  <!-- MCP 服务器（从原"MCP Tab"迁移） -->
  <div class="settings-section">
    <div class="settings-section-header">
      <div class="settings-section-title">MCP 服务器</div>
      <button class="settings-btn primary" id="mcp-add-btn">+ 添加服务器</button>
    </div>
    <div class="mcp-server-list" id="mcp-server-list">
      <!-- 内容保持不变 -->
    </div>
  </div>

  <!-- 自定义技能（从原"技能 Tab"迁移） -->
  <div class="settings-section">
    <div class="settings-section-header">
      <div class="settings-section-title">自定义技能</div>
      <button class="settings-btn primary" id="skill-add-btn">+ 添加技能</button>
    </div>
    <div class="skill-list" id="skill-list">
      <!-- 内容保持不变 -->
    </div>
  </div>

  <!-- 内置工具（从原"技能 Tab"迁移） -->
  <div class="settings-section">
    <div class="settings-section-title">内置工具</div>
    <div class="builtin-tool-list">
      <!-- 内容保持不变 -->
    </div>
  </div>

  <!-- Augment 配置（从原"配置 Tab"迁移） -->
  <div class="settings-section">
    <div class="settings-section-title">Augment 配置</div>
    <div class="prompt-enhance-form" id="prompt-enhance-form">
      <!-- 内容保持不变 -->
    </div>
  </div>
</div>
```

#### 1.4 添加 CSS 样式

**新增样式**:
```css
/* Worker 模型选择器 */
.worker-model-tabs {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
  padding: 8px;
  background: var(--vscode-editor-background);
  border-radius: 6px;
}

.worker-model-tab {
  flex: 1;
  padding: 8px 12px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  font-size: 12px;
  color: var(--vscode-descriptionForeground);
  transition: all 0.15s ease;
}

.worker-model-tab:hover {
  background: var(--vscode-list-hoverBackground);
  color: var(--vscode-foreground);
}

.worker-model-tab.active {
  background: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
  border-color: var(--vscode-focusBorder);
}

.worker-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
}

.worker-dot.claude { background: var(--color-claude); }
.worker-dot.codex { background: var(--color-codex); }
.worker-dot.gemini { background: var(--color-gemini); }
```

#### 1.5 添加 JavaScript 逻辑

**新增代码**:
```javascript
// Worker 模型配置管理
let currentWorker = 'claude';
let workerConfigs = {
  claude: null,
  codex: null,
  gemini: null
};

// 初始化：加载所有 Worker 配置
function initWorkerModelConfig() {
  vscode.postMessage({
    type: 'loadAllWorkerConfigs'
  });
}

// Worker 选择器切换
document.querySelectorAll('.worker-model-tab').forEach(tab => {
  tab.addEventListener('click', () => {
    const worker = tab.dataset.worker;
    if (worker === currentWorker) return;

    // 更新选中状态
    document.querySelectorAll('.worker-model-tab').forEach(t => t.classList.remove('active'));
    tab.classList.add('active');

    // 显示 Worker 配置
    displayWorkerConfig(worker);
    currentWorker = worker;
  });
});

// 显示 Worker 配置
function displayWorkerConfig(worker) {
  const config = workerConfigs[worker];
  if (!config) return;

  document.getElementById('worker-base-url').value = config.baseUrl || '';
  document.getElementById('worker-api-key').value = config.apiKey || '';
  document.getElementById('worker-model').value = config.model || '';
  document.getElementById('worker-provider').value = config.provider || 'anthropic';
  document.getElementById('worker-enabled').checked = config.enabled !== false;
}

// 保存 Worker 配置
document.getElementById('worker-test-btn')?.addEventListener('click', () => {
  const config = {
    baseUrl: document.getElementById('worker-base-url').value,
    apiKey: document.getElementById('worker-api-key').value,
    model: document.getElementById('worker-model').value,
    provider: document.getElementById('worker-provider').value,
    enabled: document.getElementById('worker-enabled').checked
  };

  // 保存到本地缓存
  workerConfigs[currentWorker] = config;

  // 发送到后端保存
  vscode.postMessage({
    type: 'saveWorkerConfig',
    worker: currentWorker,
    config: config
  });
});

// 测试 Worker 连接
document.getElementById('worker-test-btn')?.addEventListener('click', () => {
  const config = {
    baseUrl: document.getElementById('worker-base-url').value,
    apiKey: document.getElementById('worker-api-key').value,
    model: document.getElementById('worker-model').value,
    provider: document.getElementById('worker-provider').value
  };

  vscode.postMessage({
    type: 'testWorkerConnection',
    worker: currentWorker,
    config: config
  });
});

// 接收后端消息
window.addEventListener('message', event => {
  const message = event.data;

  switch (message.type) {
    case 'allWorkerConfigsLoaded':
      // 加载所有 Worker 配置
      workerConfigs = message.configs;
      displayWorkerConfig(currentWorker);
      break;

    case 'workerConfigSaved':
      // 配置保存成功
      showToast('配置已保存', 'success');
      break;

    case 'workerConnectionTestResult':
      // 连接测试结果
      if (message.success) {
        showToast(`${message.worker} 连接成功`, 'success');
      } else {
        showToast(`${message.worker} 连接失败: ${message.error}`, 'error');
      }
      break;
  }
});

// Toast 提示
function showToast(message, type) {
  // 实现 toast 提示
  console.log(`[${type}] ${message}`);
}
```

---

### Phase 2: 后端消息处理（2-3 小时）

#### 2.1 添加消息类型定义

**文件**: `src/types/index.ts`

**新增类型**:
```typescript
export interface WebviewToExtensionMessage {
  // ... 现有类型

  // Worker 配置相关
  type: 'loadAllWorkerConfigs' | 'saveWorkerConfig' | 'testWorkerConnection' |
        'loadOrchestratorConfig' | 'saveOrchestratorConfig' | 'testOrchestratorConnection' |
        'loadCompressorConfig' | 'saveCompressorConfig';
  worker?: WorkerSlot;
  config?: any;
}

export interface ExtensionToWebviewMessage {
  // ... 现有类型

  // Worker 配置相关
  type: 'allWorkerConfigsLoaded' | 'workerConfigSaved' | 'workerConnectionTestResult' |
        'orchestratorConfigLoaded' | 'orchestratorConfigSaved' | 'orchestratorConnectionTestResult' |
        'compressorConfigLoaded' | 'compressorConfigSaved';
  configs?: any;
  worker?: WorkerSlot;
  success?: boolean;
  error?: string;
}
```

#### 2.2 实现消息处理器

**文件**: `src/ui/webview-provider.ts`

**新增方法**:
```typescript
private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
  // ... 现有 case

  case 'loadAllWorkerConfigs':
    await this.handleLoadAllWorkerConfigs();
    break;

  case 'saveWorkerConfig':
    await this.handleSaveWorkerConfig(message.worker!, message.config!);
    break;

  case 'testWorkerConnection':
    await this.handleTestWorkerConnection(message.worker!, message.config!);
    break;

  case 'loadOrchestratorConfig':
    await this.handleLoadOrchestratorConfig();
    break;

  case 'saveOrchestratorConfig':
    await this.handleSaveOrchestratorConfig(message.config!);
    break;

  case 'testOrchestratorConnection':
    await this.handleTestOrchestratorConnection(message.config!);
    break;

  case 'loadCompressorConfig':
    await this.handleLoadCompressorConfig();
    break;

  case 'saveCompressorConfig':
    await this.handleSaveCompressorConfig(message.config!);
    break;
}

// 加载所有 Worker 配置
private async handleLoadAllWorkerConfigs(): Promise<void> {
  try {
    const fullConfig = LLMConfigLoader.loadFullConfig();

    this.postMessage({
      type: 'allWorkerConfigsLoaded',
      configs: fullConfig.workers
    });
  } catch (error: any) {
    logger.error('Failed to load worker configs', { error: error.message }, LogCategory.LLM);
    this.postMessage({
      type: 'toast',
      message: '加载配置失败: ' + error.message,
      toastType: 'error'
    });
  }
}

// 保存 Worker 配置
private async handleSaveWorkerConfig(worker: WorkerSlot, config: any): Promise<void> {
  try {
    // 加载完整配置
    const fullConfig = LLMConfigLoader.loadFullConfig();

    // 更新 Worker 配置
    fullConfig.workers[worker] = {
      baseUrl: config.baseUrl,
      apiKey: config.apiKey,
      model: config.model,
      provider: config.provider,
      enabled: config.enabled
    };

    // 保存到文件
    LLMConfigLoader.saveLLMConfigFile(fullConfig);

    // 清除该 Worker 的适配器缓存，下次使用时会重新创建
    if (this.adapterFactory instanceof LLMAdapterFactory) {
      await this.adapterFactory.clearAdapter(worker);
    }

    this.postMessage({
      type: 'workerConfigSaved',
      worker: worker
    });

    this.postMessage({
      type: 'toast',
      message: `${worker} 配置已保存`,
      toastType: 'success'
    });

    logger.info('Worker config saved', { worker }, LogCategory.LLM);
  } catch (error: any) {
    logger.error('Failed to save worker config', { worker, error: error.message }, LogCategory.LLM);
    this.postMessage({
      type: 'toast',
      message: '保存配置失败: ' + error.message,
      toastType: 'error'
    });
  }
}

// 测试 Worker 连接
private async handleTestWorkerConnection(worker: WorkerSlot, config: any): Promise<void> {
  try {
    // 创建临时客户端测试连接
    const { createLLMClient } = await import('../llm/clients/client-factory');
    const client = createLLMClient(config);

    // 发送测试请求
    const response = await client.sendMessage([
      { role: 'user', content: 'Hello' }
    ], {
      maxTokens: 10,
      temperature: 0.7
    });

    if (response && response.content) {
      this.postMessage({
        type: 'workerConnectionTestResult',
        worker: worker,
        success: true
      });

      this.postMessage({
        type: 'toast',
        message: `${worker} 连接成功`,
        toastType: 'success'
      });
    } else {
      throw new Error('No response from LLM');
    }
  } catch (error: any) {
    logger.error('Worker connection test failed', { worker, error: error.message }, LogCategory.LLM);

    this.postMessage({
      type: 'workerConnectionTestResult',
      worker: worker,
      success: false,
      error: error.message
    });

    this.postMessage({
      type: 'toast',
      message: `${worker} 连接失败: ${error.message}`,
      toastType: 'error'
    });
  }
}

// 类似的方法用于编排者和压缩器配置
private async handleLoadOrchestratorConfig(): Promise<void> {
  // 实现类似逻辑
}

private async handleSaveOrchestratorConfig(config: any): Promise<void> {
  // 实现类似逻辑
}

private async handleTestOrchestratorConnection(config: any): Promise<void> {
  // 实现类似逻辑
}

private async handleLoadCompressorConfig(): Promise<void> {
  // 实现类似逻辑
}

private async handleSaveCompressorConfig(config: any): Promise<void> {
  // 实现类似逻辑
}
```

#### 2.3 扩展 LLMConfigLoader

**文件**: `src/llm/config.ts`

**新增方法**:
```typescript
export class LLMConfigLoader {
  // ... 现有方法

  /**
   * 保存完整配置到文件
   */
  static saveLLMConfigFile(config: any): void {
    this.ensureConfigDir();

    try {
      const content = JSON.stringify(config, null, 2);
      fs.writeFileSync(this.LLM_CONFIG_FILE, content, 'utf-8');
      logger.info('LLM config saved', { path: this.LLM_CONFIG_FILE }, LogCategory.LLM);
    } catch (error: any) {
      logger.error('Failed to save LLM config', { error: error.message }, LogCategory.LLM);
      throw error;
    }
  }

  /**
   * 更新单个 Worker 配置
   */
  static updateWorkerConfig(worker: WorkerSlot, config: any): void {
    const fullConfig = this.loadLLMConfigFile();

    if (!fullConfig.workers) {
      fullConfig.workers = {};
    }

    fullConfig.workers[worker] = config;
    this.saveLLMConfigFile(fullConfig);
  }

  /**
   * 更新编排者配置
   */
  static updateOrchestratorConfig(config: any): void {
    const fullConfig = this.loadLLMConfigFile();
    fullConfig.orchestrator = config;
    this.saveLLMConfigFile(fullConfig);
  }

  /**
   * 更新压缩器配置
   */
  static updateCompressorConfig(config: any): void {
    const fullConfig = this.loadLLMConfigFile();
    fullConfig.compressor = config;
    this.saveLLMConfigFile(fullConfig);
  }
}
```

---

### Phase 3: 配置热更新（1 小时）

#### 3.1 实现适配器重载

**文件**: `src/llm/adapter-factory.ts`

**新增方法**:
```typescript
export class LLMAdapterFactory extends EventEmitter implements IAdapterFactory {
  // ... 现有代码

  /**
   * 重新加载 Worker 配置并清除缓存
   */
  async reloadWorkerConfig(worker: WorkerSlot): Promise<void> {
    await this.clearAdapter(worker);
    logger.info(`Worker config reloaded: ${worker}`, undefined, LogCategory.LLM);
  }

  /**
   * 重新加载编排者配置并清除缓存
   */
  async reloadOrchestratorConfig(): Promise<void> {
    await this.clearAdapter('orchestrator');
    logger.info('Orchestrator config reloaded', undefined, LogCategory.LLM);
  }
}
```

---

### Phase 4: 测试和验证（1-2 小时）

#### 4.1 功能测试清单

- [ ] **UI 测试**
  - [ ] Tab 切换正常
  - [ ] Worker 选择器切换正常
  - [ ] 表单输入正常
  - [ ] 密码显示/隐藏正常

- [ ] **配置加载测试**
  - [ ] 页面加载时自动加载配置
  - [ ] 切换 Worker 时显示对应配置
  - [ ] 配置数据正确显示在表单中

- [ ] **配置保存测试**
  - [ ] 点击保存按钮后配置写入文件
  - [ ] ~/.multicli/llm.json 文件内容正确
  - [ ] 保存后显示成功提示

- [ ] **连接测试**
  - [ ] 测试连接按钮可点击
  - [ ] 连接成功显示成功提示
  - [ ] 连接失败显示错误信息

- [ ] **配置生效测试**
  - [ ] 修改配置后下次使用新配置
  - [ ] 适配器缓存正确清除
  - [ ] 新配置立即生效

#### 4.2 边界情况测试

- [ ] 配置文件不存在时自动创建
- [ ] 配置文件格式错误时显示错误
- [ ] API Key 为空时使用环境变量
- [ ] 网络错误时连接测试失败
- [ ] 并发保存配置时数据一致性

---

## 📊 验收标准

### 功能完整性
- [x] 所有 6 个 Tab 重组为 4 个 Tab
- [x] Worker 模型配置区域完整实现
- [x] 所有配置项都能正确加载和显示
- [x] 所有配置项都能正确保存到文件
- [x] 测试连接功能正常工作

### 数据一致性
- [x] 前端显示的配置与文件中的配置一致
- [x] 保存后立即重新加载显示最新配置
- [x] 多个 Worker 配置互不干扰

### 用户体验
- [x] 配置修改后立即生效（无需重启）
- [x] 操作有明确的成功/失败反馈
- [x] 错误信息清晰易懂
- [x] 界面响应流畅

### 代码质量
- [x] 类型定义完整
- [x] 错误处理完善
- [x] 日志记录清晰
- [x] 代码注释充分

---

## 🔄 实施顺序

### Day 1: 前端重组（3-4 小时）
1. ✅ 修改 Tab 结构
2. ✅ 创建"模型 Tab"内容
3. ✅ 创建"工具 Tab"内容
4. ✅ 添加 CSS 样式
5. ✅ 添加基础 JavaScript 逻辑

### Day 2: 后端实现（3-4 小时）
1. ✅ 添加消息类型定义
2. ✅ 实现消息处理器
3. ✅ 扩展 LLMConfigLoader
4. ✅ 实现配置热更新

### Day 3: 测试和优化（2-3 小时）
1. ✅ 功能测试
2. ✅ 边界情况测试
3. ✅ 性能优化
4. ✅ 文档更新

**总计**: 8-11 小时

---

## 🎯 关键注意事项

### 1. 数据安全
- ✅ API Key 在前端使用 `type="password"` 隐藏
- ✅ 配置文件权限设置为 600（仅用户可读写）
- ✅ 不在日志中输出敏感信息

### 2. 错误处理
- ✅ 所有异步操作都有 try-catch
- ✅ 错误信息对用户友好
- ✅ 错误详情记录到日志

### 3. 性能优化
- ✅ 配置加载使用缓存
- ✅ 避免频繁读写文件
- ✅ 适配器懒加载

### 4. 向后兼容
- ✅ 支持旧配置文件格式
- ✅ 缺失字段使用默认值
- ✅ 配置迁移提示

---

**最后更新**: 2024年
**状态**: 📝 实施计划
**预计工时**: 8-11 小时
**优先级**: 🔥 高
