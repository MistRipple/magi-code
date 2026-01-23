# 📚 Phase 2: 项目级知识库实施计划

## 执行摘要

**目标**: 实现项目级知识库，为 AI 助手提供项目结构、架构决策、常见问题等上下文信息

**优先级**: 高

**预计时间**: 2-3 天

**依赖**: Phase 1 会话管理（已完成 ✅）

---

## 背景和动机

### 当前问题

1. **缺乏项目上下文**: AI 助手不了解项目结构、技术栈、架构决策
2. **重复回答问题**: 相同问题在不同会话中重复回答
3. **上下文碎片化**: 项目知识分散在各个会话中，难以复用
4. **新会话冷启动**: 新会话需要重新解释项目背景

### 解决方案

实现三层知识库：

```
Layer 1: 项目结构索引
  - 文件列表和目录结构
  - 技术栈检测
  - 依赖关系

Layer 2: 架构决策记录 (ADR)
  - 关键技术选型
  - 架构模式
  - 设计决策

Layer 3: 常见问题库 (FAQ)
  - 项目特定问题
  - 最佳实践
  - 常见错误
```

---

## 架构设计

### 核心类: ProjectKnowledgeBase

```typescript
interface ProjectKnowledgeBase {
  // 项目元数据
  projectRoot: string;
  projectName: string;
  techStack: TechStack;

  // 知识库内容
  codeIndex: CodeIndex;
  adrs: ADRRecord[];
  faqs: FAQRecord[];

  // 方法
  initialize(): Promise<void>;
  indexProject(): Promise<CodeIndex>;
  addADR(adr: ADRRecord): void;
  addFAQ(faq: FAQRecord): void;
  getProjectContext(maxTokens: number): string;
}
```

### 数据结构

#### 1. CodeIndex (代码索引)

```typescript
interface CodeIndex {
  files: FileEntry[];
  directories: DirectoryEntry[];
  techStack: TechStack;
  dependencies: DependencyInfo;
  entryPoints: string[];
  lastIndexed: number;
}

interface FileEntry {
  path: string;
  type: 'source' | 'config' | 'doc' | 'test';
  language?: string;
  size: number;
  exports?: string[];  // 导出的函数/类
}

interface TechStack {
  languages: string[];
  frameworks: string[];
  buildTools: string[];
  testFrameworks: string[];
}
```

#### 2. ADR (架构决策记录)

```typescript
interface ADRRecord {
  id: string;
  title: string;
  date: number;
  status: 'proposed' | 'accepted' | 'deprecated' | 'superseded';
  context: string;      // 决策背景
  decision: string;     // 决策内容
  consequences: string; // 影响和后果
  alternatives?: string[]; // 考虑过的替代方案
  relatedFiles?: string[]; // 相关文件
}
```

#### 3. FAQ (常见问题)

```typescript
interface FAQRecord {
  id: string;
  question: string;
  answer: string;
  category: string;
  tags: string[];
  relatedFiles?: string[];
  createdAt: number;
  updatedAt: number;
  useCount: number; // 使用次数
}
```

---

## 实施阶段

### Stage 1: 基础架构和代码索引 ⚡ 优先

**目标**: 实现 ProjectKnowledgeBase 类和基础代码索引功能

**任务**:
1. 创建 `src/knowledge/project-knowledge-base.ts`
2. 实现项目文件扫描和索引
3. 检测技术栈（package.json, tsconfig.json 等）
4. 生成文件列表和目录结构
5. 识别入口文件

**成功标准**:
- ✅ 能够扫描项目目录
- ✅ 正确识别文件类型（源码、配置、文档、测试）
- ✅ 检测技术栈（TypeScript, Node.js, VSCode Extension 等）
- ✅ 生成结构化的代码索引

**测试**:
```typescript
test('扫描项目并生成索引', async () => {
  const kb = new ProjectKnowledgeBase('/path/to/project');
  await kb.initialize();

  expect(kb.codeIndex.files.length).toBeGreaterThan(0);
  expect(kb.techStack.languages).toContain('TypeScript');
  expect(kb.techStack.frameworks).toContain('VSCode Extension');
});
```

**预计时间**: 4-6 小时

---

### Stage 2: ADR 存储和管理 📝

**目标**: 实现架构决策记录的存储和检索

**任务**:
1. 设计 ADR 存储格式（JSON）
2. 实现 `addADR()` 方法
3. 实现 `getADRs()` 方法（支持过滤）
4. 实现 ADR 持久化（保存到 `.multicli/adrs/` 目录）
5. 支持从 Markdown 文件导入 ADR

**成功标准**:
- ✅ 能够添加和检索 ADR
- ✅ ADR 持久化到磁盘
- ✅ 支持按状态、日期过滤
- ✅ 支持从 Markdown 导入

**测试**:
```typescript
test('添加和检索 ADR', () => {
  kb.addADR({
    id: 'adr-001',
    title: '使用 TypeScript 开发',
    status: 'accepted',
    context: '需要类型安全和更好的 IDE 支持',
    decision: '采用 TypeScript 作为主要开发语言',
    consequences: '需要编译步骤，但提高了代码质量'
  });

  const adrs = kb.getADRs({ status: 'accepted' });
  expect(adrs).toHaveLength(1);
  expect(adrs[0].title).toBe('使用 TypeScript 开发');
});
```

**预计时间**: 3-4 小时

---

### Stage 3: FAQ 存储和管理 ❓

**目标**: 实现常见问题的存储和检索

**任务**:
1. 设计 FAQ 存储格式（JSON）
2. 实现 `addFAQ()` 方法
3. 实现 `searchFAQs()` 方法（关键词搜索）
4. 实现 FAQ 持久化（保存到 `.multicli/faqs.json`）
5. 支持 FAQ 使用统计

**成功标准**:
- ✅ 能够添加和搜索 FAQ
- ✅ FAQ 持久化到磁盘
- ✅ 支持关键词搜索
- ✅ 记录使用次数

**测试**:
```typescript
test('添加和搜索 FAQ', () => {
  kb.addFAQ({
    id: 'faq-001',
    question: '如何调试 VSCode 扩展？',
    answer: '按 F5 启动调试，在 Extension Development Host 中测试',
    category: 'development',
    tags: ['debug', 'vscode']
  });

  const results = kb.searchFAQs('调试');
  expect(results).toHaveLength(1);
  expect(results[0].question).toContain('调试');
});
```

**预计时间**: 2-3 小时

---

### Stage 4: 上下文生成和注入 🔗

**目标**: 将项目知识库集成到 ContextManager

**任务**:
1. 在 ContextManager 中添加 `projectKnowledgeBase` 属性
2. 实现 `getProjectContext()` 方法生成项目上下文
3. 修改 `getContextSlice()` 注入项目知识
4. 实现智能选择（根据用户问题选择相关知识）
5. Token 预算管理（10% 用于项目知识）

**成功标准**:
- ✅ 项目知识正确注入到上下文
- ✅ Token 预算控制在 10% 以内
- ✅ 根据问题智能选择相关知识
- ✅ 不影响现有会话总结和 Memory

**上下文结构**:
```
## 项目知识 (10% token 预算)
**项目**: MultiCLI
**技术栈**: TypeScript, Node.js, VSCode Extension API
**文件数**: 156 个源文件

**关键架构决策**:
1. [ADR-001] 使用 TypeScript 开发
2. [ADR-002] 采用 Webview 实现 UI

**相关 FAQ**:
Q: 如何调试 VSCode 扩展？
A: 按 F5 启动调试...

---

## 会话总结 (20% token 预算)
...

## 会话上下文 (30% token 预算)
...

## 最近对话 (40% token 预算)
...
```

**测试**:
```typescript
test('项目知识注入到上下文', async () => {
  contextManager.setProjectKnowledgeBase(kb);

  const context = await contextManager.getContextSlice(
    'user',
    '如何调试扩展？',
    8000
  );

  expect(context).toContain('项目知识');
  expect(context).toContain('MultiCLI');
  expect(context).toContain('调试');
});
```

**预计时间**: 4-5 小时

---

### Stage 5: 自动知识提取 🤖

**目标**: 从会话中自动提取 ADR 和 FAQ

**任务**:
1. 实现 `extractADRFromSession()` 方法
2. 实现 `extractFAQFromSession()` 方法
3. 基于关键词识别决策和问题
4. **使用压缩模型进行智能提取** ⚠️
5. 提示用户确认是否保存
6. 自动关联相关文件

**成功标准**:
- ✅ 能够从会话中识别决策
- ✅ 能够从会话中识别常见问题
- ✅ 使用压缩模型（compressor）处理提取任务
- ✅ 提示用户确认
- ✅ 自动保存到知识库

**关键词**:
- ADR: "决定", "选择", "采用", "架构", "方案"
- FAQ: "如何", "怎么", "为什么", "问题", "错误"

**⚠️ 重要**: 使用 `LLMConfigLoader.loadCompressorConfig()` 获取压缩模型配置，用于智能提取任务

**预计时间**: 3-4 小时

---

### Stage 6: UI 集成 🎨

**目标**: 在 Webview 中显示项目知识

**任务**:
1. 添加 "项目知识" 面板
2. 显示项目结构树
3. 显示 ADR 列表
4. 显示 FAQ 列表
5. 支持搜索和过滤

**成功标准**:
- ✅ UI 显示项目知识
- ✅ 可以浏览 ADR 和 FAQ
- ✅ 支持搜索
- ✅ 样式与现有 UI 一致

**预计时间**: 4-5 小时

---

### Stage 7: 测试和文档 ✅

**目标**: 全面测试和文档化

**任务**:
1. 创建测试脚本 `scripts/test-project-knowledge.js`
2. 编写单元测试（30+ 测试用例）
3. 编写集成测试
4. 更新用户文档
5. 创建 Phase 2 完成报告

**成功标准**:
- ✅ 所有测试通过（100%）
- ✅ 文档完整
- ✅ 示例清晰

**预计时间**: 4-5 小时

---

## Token 预算分配（更新）

```
总预算: 8000 tokens

Layer 0: 项目知识 (10%, 800 tokens)
  - 项目结构: 200 tokens
  - 技术栈: 100 tokens
  - ADR: 300 tokens
  - FAQ: 200 tokens

Layer 1: 会话总结 (20%, 1600 tokens)
  - 会话目标、任务、决策、代码变更

Layer 2: 会话 Memory (30%, 2400 tokens)
  - 当前任务、待解决问题

Layer 3: 最近对话 (40%, 3200 tokens)
  - 最近的用户-助手对话
```

---

## 技术选型

### 文件扫描
- **库**: `fast-glob` (已在项目中使用)
- **原因**: 快速、支持 glob 模式、异步

### 技术栈检测
- **方法**: 检查配置文件（package.json, tsconfig.json 等）
- **原因**: 简单、可靠、零成本

### 存储格式
- **格式**: JSON
- **位置**: `.multicli/` 目录
- **原因**: 易于读写、人类可读、版本控制友好

### 搜索
- **方法**: 简单的关键词匹配
- **未来**: 可以升级到语义搜索（embedding）

---

## 风险和挑战

### 1. 大型项目性能 ⚠️
**风险**: 扫描大型项目（10000+ 文件）可能很慢

**缓解**:
- 使用缓存（只在文件变更时重新索引）
- 异步扫描，不阻塞 UI
- 支持增量索引

### 2. 知识质量 ⚠️
**风险**: 自动提取的知识可能不准确

**缓解**:
- 提示用户确认
- 支持手动编辑
- 记录来源和置信度

### 3. Token 预算 ⚠️
**风险**: 项目知识可能占用太多 tokens

**缓解**:
- 严格的 10% 预算限制
- 智能选择相关知识
- 支持摘要和截断

### 4. 存储空间 ⚠️
**风险**: 知识库可能变得很大

**缓解**:
- 定期清理不常用的 FAQ
- 压缩旧的 ADR
- 支持导出和归档

---

## 成功指标

### 功能指标
- ✅ 能够索引项目（100% 文件覆盖）
- ✅ 能够存储和检索 ADR
- ✅ 能够存储和检索 FAQ
- ✅ 能够注入到上下文

### 性能指标
- ✅ 索引时间 < 5 秒（中型项目）
- ✅ 搜索时间 < 100ms
- ✅ Token 使用 < 10% 预算

### 质量指标
- ✅ 测试覆盖率 > 90%
- ✅ 所有测试通过（100%）
- ✅ 文档完整

---

## 实施顺序

**Week 1**:
- Day 1: Stage 1 (代码索引) ⚡
- Day 2: Stage 2 (ADR) + Stage 3 (FAQ)
- Day 3: Stage 4 (上下文注入)

**Week 2**:
- Day 4: Stage 5 (自动提取)
- Day 5: Stage 6 (UI 集成)
- Day 6: Stage 7 (测试和文档)

---

## 下一步行动

### 立即开始（今天）

1. **创建 ProjectKnowledgeBase 类** ⚡
   - 文件: `src/knowledge/project-knowledge-base.ts`
   - 实现基础结构和接口

2. **实现项目扫描** ⚡
   - 使用 `fast-glob` 扫描文件
   - 识别文件类型

3. **检测技术栈** ⚡
   - 读取 package.json
   - 读取 tsconfig.json
   - 识别框架和工具

4. **生成代码索引** ⚡
   - 创建 FileEntry 列表
   - 创建目录结构
   - 识别入口文件

---

## 相关文档

1. **Phase 1 完成报告**: `SESSION_MANAGEMENT_PHASE1_FINAL.md`
2. **架构分析**: `CONTEXT_MEMORY_ARCHITECTURE_ANALYSIS.md`
3. **实施计划**: `SESSION_MANAGEMENT_IMPLEMENTATION_PLAN.md`

---

**文档版本**: 1.0
**创建日期**: 2025-01-22
**作者**: AI Assistant
**状态**: 📝 计划中，准备开始实施
