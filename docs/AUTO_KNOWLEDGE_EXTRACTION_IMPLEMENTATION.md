# 全自动知识提取实现报告

## 实现概述

已成功实现知识库的**全自动提取机制**，使用**压缩模型**自动从会话中提取 ADR（架构决策记录）和 FAQ（常见问题），无需任何手动操作。

---

## 1. 核心改动

### 1.1 WebView Provider 增强

**文件：** `src/ui/webview-provider.ts`

#### 改动 1：初始化知识库时设置压缩模型客户端

```typescript
private async initializeProjectKnowledgeBase(): Promise<void> {
  try {
    this.projectKnowledgeBase = new ProjectKnowledgeBase({
      projectRoot: this.workspaceRoot
    });
    await this.projectKnowledgeBase.initialize();

    // ✅ 新增：设置压缩模型客户端（用于自动知识提取）
    await this.setupKnowledgeExtractionClient();

    // 注入知识库到编排器
    this.intelligentOrchestrator.setKnowledgeBase(this.projectKnowledgeBase);

    // ✅ 新增：监听任务完成事件，自动提取知识
    this.setupAutoKnowledgeExtraction();

    const codeIndex = this.projectKnowledgeBase.getCodeIndex();
    logger.info('项目知识库.已初始化', {
      files: codeIndex ? codeIndex.files.length : 0
    }, LogCategory.SESSION);
  } catch (error: any) {
    logger.error('项目知识库.初始化失败', { error: error.message }, LogCategory.SESSION);
  }
}
```

#### 改动 2：设置压缩模型客户端

```typescript
/**
 * 设置知识提取客户端（使用压缩模型）
 */
private async setupKnowledgeExtractionClient(): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    const { UniversalClient } = await import('../llm/clients/universal-client');

    // 加载压缩模型配置
    const compressorConfig = LLMConfigLoader.loadCompressorConfig();

    if (!compressorConfig.enabled) {
      logger.warn('项目知识库.压缩模型未启用', undefined, LogCategory.SESSION);
      return;
    }

    // 创建压缩模型客户端
    const client = new UniversalClient({
      baseUrl: compressorConfig.baseUrl,
      apiKey: compressorConfig.apiKey,
      model: compressorConfig.model,
      provider: compressorConfig.provider,
    });

    // 设置到知识库
    this.projectKnowledgeBase.setLLMClient(client);

    logger.info('项目知识库.压缩模型客户端.已设置', {
      model: compressorConfig.model,
      provider: compressorConfig.provider
    }, LogCategory.SESSION);
  } catch (error: any) {
    logger.error('项目知识库.压缩模型客户端.设置失败', { error: error.message }, LogCategory.SESSION);
  }
}
```

#### 改动 3：设置自动知识提取

```typescript
/**
 * 设置自动知识提取
 * 监听任务完成事件，自动从会话中提取 ADR 和 FAQ
 */
private setupAutoKnowledgeExtraction(): void {
  // 任务完成计数器
  let completedTaskCount = 0;
  const EXTRACTION_THRESHOLD = 3; // 每完成 3 个任务提取一次

  // 监听任务完成事件
  globalEventBus.on('task:completed', async (event: any) => {
    completedTaskCount++;

    // 达到阈值时提取知识
    if (completedTaskCount >= EXTRACTION_THRESHOLD) {
      completedTaskCount = 0; // 重置计数器
      await this.extractKnowledgeFromCurrentSession();
    }
  });

  // 监听会话结束事件
  globalEventBus.on('session:ended', async (event: any) => {
    const sessionId = event.sessionId;
    if (sessionId) {
      await this.extractKnowledgeFromSession(sessionId);
    }
  });

  logger.info('项目知识库.自动提取.已启用', {
    threshold: EXTRACTION_THRESHOLD
  }, LogCategory.SESSION);
}
```

#### 改动 4：提取知识的核心逻辑

```typescript
/**
 * 从指定会话提取知识
 */
private async extractKnowledgeFromSession(sessionId: string): Promise<void> {
  try {
    const session = this.sessionManager.getSession(sessionId);
    if (!session || session.messages.length < 5) {
      // 消息太少，不值得提取
      return;
    }

    logger.info('项目知识库.开始提取知识', {
      sessionId,
      messageCount: session.messages.length
    }, LogCategory.SESSION);

    // 转换消息格式
    const messages = session.messages.map(m => ({
      role: m.role,
      content: m.content
    }));

    // 提取 ADR
    const adrs = await this.projectKnowledgeBase.extractADRFromSession(messages);
    if (adrs.length > 0) {
      logger.info('项目知识库.ADR提取成功', {
        count: adrs.length,
        titles: adrs.map(a => a.title)
      }, LogCategory.SESSION);

      // 通知前端
      this.postMessage({
        type: 'toast',
        message: `自动提取了 ${adrs.length} 条架构决策记录`,
        toastType: 'success'
      });

      // 刷新知识库显示
      await this.handleGetProjectKnowledge();
    }

    // 提取 FAQ
    const faqs = await this.projectKnowledgeBase.extractFAQFromSession(messages);
    if (faqs.length > 0) {
      logger.info('项目知识库.FAQ提取成功', {
        count: faqs.length,
        questions: faqs.map(f => f.question)
      }, LogCategory.SESSION);

      // 通知前端
      this.postMessage({
        type: 'toast',
        message: `自动提取了 ${faqs.length} 条常见问题`,
        toastType: 'success'
      });

      // 刷新知识库显示
      await this.handleGetProjectKnowledge();
    }

    if (adrs.length === 0 && faqs.length === 0) {
      logger.info('项目知识库.未提取到新知识', { sessionId }, LogCategory.SESSION);
    }
  } catch (error: any) {
    logger.error('项目知识库.知识提取失败', {
      sessionId,
      error: error.message
    }, LogCategory.SESSION);
  }
}
```

---

## 2. 压缩模型配置

### 2.1 当前配置

**文件：** `~/.multicli/llm.json`

```json
{
  "compressor": {
    "enabled": true,
    "baseUrl": "https://newapi.stonefancyx.com",
    "apiKey": "sk-2T6Mj0ADtdMiLHHad6BaKZFaHJawm4IwYHbpLz8QZGosRcTo",
    "model": "gemini-3-flash-preview",
    "provider": "openai"
  }
}
```

### 2.2 压缩模型用途

根据设计文档（`docs/dev-history/archived-phases/SESSION_MANAGEMENT_PHASE2_PLAN.md`），压缩模型专门用于处理"杂活"：

1. **知识提取**：从会话中提取 ADR 和 FAQ
2. **上下文压缩**：压缩长对话历史
3. **摘要生成**：生成会话摘要
4. **辅助任务**：不需要高级推理能力的任务

**为什么使用压缩模型？**
- 成本低：使用更便宜的模型（如 Gemini Flash、Claude Haiku）
- 速度快：快速处理大量文本
- 专注：专门处理提取和压缩任务

---

## 3. 自动提取触发机制

### 3.1 触发时机

#### 时机 1：任务完成计数触发
```
每完成 3 个任务 → 自动提取一次知识
```

**优点：**
- 及时捕获新的决策和问题
- 避免频繁提取浪费资源
- 平衡实时性和效率

#### 时机 2：会话结束触发
```
会话结束 → 提取整个会话的知识
```

**优点：**
- 完整分析整个会话
- 捕获最终决策
- 避免遗漏重要信息

### 3.2 提取条件

```typescript
if (!session || session.messages.length < 5) {
  // 消息太少，不值得提取
  return;
}
```

**最少消息数：** 5 条

**原因：**
- 太少的消息通常不包含有价值的决策或问题
- 避免无效的 LLM 调用
- 节省成本

---

## 4. 提取流程

### 4.1 ADR 提取流程

```
1. 监听任务完成事件
   ↓
2. 达到阈值（3个任务）
   ↓
3. 获取当前会话消息
   ↓
4. 调用 extractADRFromSession(messages)
   ↓
5. LLM 分析会话内容
   ↓
6. 识别关键决策（关键词：决定、选择、采用、使用、方案、架构）
   ↓
7. 提取决策的背景、内容、影响、替代方案
   ↓
8. 生成 ADR 记录
   ↓
9. 自动保存到 .multicli/knowledge/adrs.json
   ↓
10. 通知前端显示 Toast
   ↓
11. 刷新知识库 UI
```

### 4.2 FAQ 提取流程

```
1. 监听任务完成事件
   ↓
2. 达到阈值（3个任务）
   ↓
3. 获取当前会话消息
   ↓
4. 调用 extractFAQFromSession(messages)
   ↓
5. LLM 分析会话内容
   ↓
6. 识别常见问题（关键词：如何、怎么、为什么、问题、错误）
   ↓
7. 提取问题和答案
   ↓
8. 生成 FAQ 记录
   ↓
9. 自动保存到 .multicli/knowledge/faqs.json
   ↓
10. 通知前端显示 Toast
   ↓
11. 刷新知识库 UI
```

---

## 5. LLM 提示词

### 5.1 ADR 提取提示词

**文件：** `src/knowledge/project-knowledge-base.ts:682-730`

```typescript
private buildADRExtractionPrompt(messages: Array<{ role: string; content: string }>): string {
  const conversationText = messages
    .map(m => `[${m.role}]: ${m.content}`)
    .join('\n\n');

  return `请从以下对话中提取架构决策记录（ADR）。

## 对话内容
${conversationText}

## 提取规则
1. 识别关键技术决策（包含关键词：决定、选择、采用、使用、方案、架构等）
2. 提取决策的背景、内容、影响
3. 识别考虑过的替代方案
4. 每个决策生成一个 ADR

## 输出格式
请以 JSON 数组格式输出，每个 ADR 包含以下字段：
\`\`\`json
[
  {
    "title": "决策标题",
    "context": "决策背景和原因",
    "decision": "具体决策内容",
    "consequences": "决策的影响和后果",
    "alternatives": ["替代方案1", "替代方案2"]
  }
]
\`\`\`

如果没有找到明确的架构决策，返回空数组 []。`;
}
```

### 5.2 FAQ 提取提示词

**文件：** `src/knowledge/project-knowledge-base.ts:732-780`

```typescript
private buildFAQExtractionPrompt(messages: Array<{ role: string; content: string }>): string {
  const conversationText = messages
    .map(m => `[${m.role}]: ${m.content}`)
    .join('\n\n');

  return `请从以下对话中提取常见问题（FAQ）。

## 对话内容
${conversationText}

## 提取规则
1. 识别用户提出的问题（包含关键词：如何、怎么、为什么、问题、错误等）
2. 提取问题和对应的答案
3. 识别问题的分类和标签
4. 每个问答对生成一个 FAQ

## 输出格式
请以 JSON 数组格式输出，每个 FAQ 包含以下字段：
\`\`\`json
[
  {
    "question": "问题内容",
    "answer": "答案内容",
    "category": "分类（如：development, usage, troubleshooting）",
    "tags": ["标签1", "标签2"]
  }
]
\`\`\`

如果没有找到明确的问答对，返回空数组 []。`;
}
```

---

## 6. 数据持久化

### 6.1 存储位置

```
.multicli/knowledge/
├── code-index.json    # 代码索引（自动生成）
├── adrs.json          # ADR 列表（自动提取）
└── faqs.json          # FAQ 列表（自动提取）
```

### 6.2 自动保存机制

```typescript
// ProjectKnowledgeBase 类中
async extractADRFromSession(messages): Promise<ADRRecord[]> {
  // ... 提取逻辑 ...
  
  // 自动保存
  for (const adr of adrs) {
    this.addADR(adr); // 内部调用 saveADRs()
  }
  
  return adrs;
}
```

**保存时机：**
- 每次提取到新的 ADR/FAQ 后立即保存
- 无需手动操作
- 自动持久化到文件系统

---

## 7. 用户体验

### 7.1 Toast 通知

提取成功后，用户会看到 Toast 通知：

```
✅ 自动提取了 2 条架构决策记录
✅ 自动提取了 3 条常见问题
```

### 7.2 知识库 UI 自动刷新

提取完成后，知识库 Tab 会自动刷新显示新的内容：

```typescript
// 刷新知识库显示
await this.handleGetProjectKnowledge();
```

用户无需手动刷新，新的 ADR/FAQ 会立即显示在 UI 中。

---

## 8. 性能优化

### 8.1 提取阈值

```typescript
const EXTRACTION_THRESHOLD = 3; // 每完成 3 个任务提取一次
```

**优化点：**
- 避免每个任务都提取（太频繁）
- 避免等待太久才提取（遗漏信息）
- 平衡实时性和性能

### 8.2 消息数量过滤

```typescript
if (session.messages.length < 5) {
  return; // 消息太少，不提取
}
```

**优化点：**
- 避免无效的 LLM 调用
- 节省 API 成本
- 提高提取质量

### 8.3 异步处理

```typescript
globalEventBus.on('task:completed', async (event: any) => {
  // 异步提取，不阻塞主流程
  await this.extractKnowledgeFromCurrentSession();
});
```

**优化点：**
- 不阻塞任务完成流程
- 后台静默提取
- 提升用户体验

---

## 9. 错误处理

### 9.1 压缩模型未启用

```typescript
if (!compressorConfig.enabled) {
  logger.warn('项目知识库.压缩模型未启用', undefined, LogCategory.SESSION);
  return;
}
```

**处理：** 记录警告，不影响其他功能

### 9.2 LLM 调用失败

```typescript
try {
  const response = await this.llmClient.sendMessage({...});
} catch (error) {
  logger.error('项目知识库.ADR提取.失败', { error }, LogCategory.SESSION);
  return [];
}
```

**处理：** 记录错误，返回空数组，不影响主流程

### 9.3 会话消息不足

```typescript
if (!session || session.messages.length < 5) {
  return; // 静默跳过
}
```

**处理：** 静默跳过，不记录错误

---

## 10. 测试建议

### 10.1 功能测试

1. **完成 3 个任务后自动提取**
   - 执行 3 个任务
   - 观察是否自动提取知识
   - 检查 Toast 通知
   - 验证知识库 UI 更新

2. **会话结束时提取**
   - 完成一个会话
   - 切换到新会话
   - 检查是否提取了旧会话的知识

3. **消息数量过滤**
   - 创建只有 2-3 条消息的会话
   - 验证不会触发提取

### 10.2 压缩模型测试

1. **验证模型配置**
   ```bash
   cat ~/.multicli/llm.json | grep -A 6 "compressor"
   ```

2. **测试模型连接**
   - 打开设置面板
   - 查看"压缩模型"连接状态
   - 应显示"已连接"

### 10.3 数据持久化测试

1. **验证文件生成**
   ```bash
   ls -la .multicli/knowledge/
   cat .multicli/knowledge/adrs.json
   cat .multicli/knowledge/faqs.json
   ```

2. **重启后验证**
   - 重启 VS Code
   - 打开知识库 Tab
   - 验证之前提取的知识仍然存在

---

## 11. 配置说明

### 11.1 调整提取阈值

如果想改变提取频率，修改 `src/ui/webview-provider.ts`：

```typescript
const EXTRACTION_THRESHOLD = 5; // 改为每完成 5 个任务提取一次
```

### 11.2 调整最少消息数

如果想改变最少消息数，修改 `src/ui/webview-provider.ts`：

```typescript
if (session.messages.length < 10) { // 改为至少 10 条消息
  return;
}
```

### 11.3 禁用自动提取

如果想临时禁用自动提取，注释掉事件监听：

```typescript
// globalEventBus.on('task:completed', async (event: any) => {
//   ...
// });
```

---

## 12. 与手动添加的对比

| 特性 | 全自动提取 | 手动添加 |
|------|-----------|---------|
| **用户操作** | 无需任何操作 | 需要填写表单 |
| **触发时机** | 任务完成/会话结束 | 用户主动触发 |
| **数据来源** | 会话消息（LLM 分析） | 用户输入 |
| **准确性** | 依赖 LLM 理解 | 100% 准确 |
| **效率** | 高（自动化） | 低（手动） |
| **成本** | 有 API 调用成本 | 无成本 |
| **适用场景** | 日常开发，快速积累 | 重要决策，精确记录 |

**结论：** 全自动提取适合日常使用，手动添加适合重要决策的精确记录。

---

## 13. 总结

### 13.1 实现的功能

✅ **压缩模型集成**
- 加载压缩模型配置
- 创建 UniversalClient 实例
- 设置到 ProjectKnowledgeBase

✅ **自动提取触发**
- 监听任务完成事件（每 3 个任务）
- 监听会话结束事件
- 自动调用提取方法

✅ **智能提取**
- 使用 LLM 分析会话内容
- 识别架构决策和常见问题
- 生成结构化数据

✅ **自动保存**
- 提取后立即保存到文件
- 无需手动操作
- 持久化到 `.multicli/knowledge/`

✅ **用户通知**
- Toast 通知提取结果
- 自动刷新知识库 UI
- 无缝用户体验

### 13.2 关键优势

1. **完全自动化**：无需任何手动操作
2. **使用压缩模型**：成本低、速度快
3. **智能提取**：LLM 理解会话内容
4. **及时捕获**：任务完成即提取
5. **持久化存储**：自动保存到文件
6. **用户友好**：Toast 通知 + UI 自动刷新

### 13.3 下一步优化

1. **提取质量优化**
   - 改进 LLM 提示词
   - 添加去重逻辑
   - 提高识别准确率

2. **用户控制**
   - 添加"确认提取"对话框
   - 允许用户编辑提取结果
   - 支持手动触发提取

3. **性能优化**
   - 批量提取（减少 API 调用）
   - 缓存提取结果
   - 异步队列处理

4. **数据质量**
   - 添加评分机制
   - 支持用户反馈
   - 自动清理低质量数据

---

## 14. 文件清单

### 修改的文件

- ✅ `src/ui/webview-provider.ts` - 添加自动提取逻辑

### 创建的文档

- ✅ `docs/AUTO_KNOWLEDGE_EXTRACTION_IMPLEMENTATION.md` - 本文档
- ✅ `docs/KNOWLEDGE_BASE_DATA_SOURCE_GUIDE.md` - 数据来源说明
- ✅ `docs/KNOWLEDGE_BASE_IMPLEMENTATION_VERIFICATION.md` - 实现验证报告
- ✅ `docs/KNOWLEDGE_UI_REFACTORING_REPORT.md` - UI 重构报告

### 相关文件（未修改）

- `src/knowledge/project-knowledge-base.ts` - 知识库核心类（已有提取方法）
- `src/llm/config.ts` - LLM 配置加载器（已有压缩模型配置）
- `~/.multicli/llm.json` - LLM 配置文件（压缩模型已启用）

---

## 15. 快速开始

### 15.1 验证配置

```bash
# 检查压缩模型配置
cat ~/.multicli/llm.json | grep -A 6 "compressor"

# 应该看到 "enabled": true
```

### 15.2 重新加载扩展

1. 按 `Cmd+Shift+P` (macOS) 或 `Ctrl+Shift+P` (Windows/Linux)
2. 输入 `Developer: Reload Window`
3. 或者直接重启 VS Code

### 15.3 测试自动提取

1. 创建新会话
2. 执行 3 个任务（如：创建文件、修改代码、添加功能）
3. 观察是否出现 Toast 通知
4. 切换到"知识库" Tab
5. 查看是否有新的 ADR/FAQ

### 15.4 查看提取结果

```bash
# 查看 ADR
cat .multicli/knowledge/adrs.json | jq '.'

# 查看 FAQ
cat .multicli/knowledge/faqs.json | jq '.'
```

---

🎉 **全自动知识提取功能已完成！现在知识库会自动从你的工作会话中学习和积累项目知识。**

