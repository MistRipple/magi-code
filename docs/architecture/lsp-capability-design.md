# LSP 能力设计方案 — 从 AI 工具到系统基础设施

> **版本**: v2.0
> **日期**: 2025-07-12
> **状态**: 待实施
> **相关文件**: `src/tools/lsp-executor.ts`, `src/tools/tool-manager.ts`, `src/orchestrator/prompts/orchestrator-prompts.ts`, `src/orchestrator/lsp/lsp-enforcer.ts`, `src/orchestrator/core/mission-driven-engine.ts`, `src/ui/webview-provider.ts`

---

## 一、背景与动因

### 1.1 LSP 协议的设计本质

根据 [LSP 3.17 规范](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)，Language Server Protocol 的核心前提是：

- **为人机交互设计**：假设消费者是人类使用的编辑器
- **服务 UI 元素**：hover 返回 Markdown tooltip、signatureHelp 提供打字时参数提示
- **逐步导航模型**：人类通过"跳转定义 → 查看引用 → 跳转实现"逐步建立心智模型

### 1.2 业界 AI 编码工具的实践

| 产品 | 代码智能方案 | 是否直接暴露 LSP 工具给 AI |
|:-----|:-----------|:-------------------------|
| Aider | tree-sitter 符号提取 + 图排序 → repo map | 否 |
| Sourcegraph Cody | Code Graph (SCIP) + 搜索 | 否 |
| Continue.dev | Embeddings 语义检索 | 否 |
| Claude Code | grep + 文件读取 | 否 |
| Cursor | 内部代码索引 + 上下文引擎 | 否 |

**共同特征**：在系统层（非 AI 工具层）使用代码智能构建结构化上下文注入 prompt，AI 看到的是分析结果而非调用分析工具。

### 1.3 Magi 当前状态与核心问题

当前 `lsp_query` 作为 AI 工具暴露 9 个 action，存在**定位错配**：

1. **LLM 使用率极低**：编排者拥有 16+ 个工具，倾向于用 `codebase_retrieval` 或 `grep_search`，几乎不主动调用 `lsp_query`
2. **精度前提不成立**：LSP 导航类 action（definition/references）需要精确的文件路径+行列号，但 LLM 在达到这个精度前已经用其他工具解决了问题
3. **提示词噪声**：LSP 相关提示词占 ~17 行，增加 LLM 的决策复杂度
4. **功能未被利用**：VS Code 的 TypeScript Server 已为整个项目构建了完整的语义图（类型推断、引用关系、调用链），但这些数据仅在 LLM 偶尔调用时才被访问
5. **UI 零呈现**：LSP 数据在 webview 中没有任何呈现，用户感知不到它的存在

**核心结论**：问题不是 LSP 能力不足，而是定位错误 — LSP 不应该是"给 AI 用的工具"，而应该是"替 AI 准备好上下文的基础设施"。

---

## 二、设计原则

1. **基础设施化**：LSP 从 AI 工具层降级为系统基础设施，不再出现在 LLM 的工具列表中
2. **自动利用**：系统层自动使用 LSP 数据增强 AI 上下文，无需 LLM 主动调用
3. **不删实现**：所有 handler 代码保留，`LspExecutor.execute()` 接口不变，供系统内部调用
4. **提示词净化**：移除所有 `lsp_query` 相关的提示词内容，减少 LLM 决策噪声

---

## 三、架构转型

### 3.1 Before：AI 工具模型

```
LLM 决策层:  "需不需要调 lsp_query?" → 大概率不调
                                         ↓ (偶尔)
             lsp_query(definition, file, line, col)
                                         ↓
             LspExecutor.execute() → 返回结果给 LLM
```

### 3.2 After：系统基础设施模型

```
系统基础设施层:
  ┌─ LspEnforcer (预检/后检)
  │    Worker 执行前: diagnostics + documentSymbols → 注入 guidancePrompt
  │    Worker 执行后: diagnostics 对比 → 检测新增编译错误（本期新增）
  │
  ├─ LocalSearchEngine 数据源 (已规划)
  │    workspaceSymbols → 符号搜索策略
  │
  └─ webview-provider 本地搜索回退 (已有)
       workspaceSymbols → ACE 不可用时的符号搜索

LLM 层: 不感知 LSP 的存在，只看到系统注入的结构化上下文
```

### 3.3 LSP 的三个系统消费者

| 消费者 | 调用方式 | 使用的 Action | 变更 |
|:-------|:---------|:-------------|:-----|
| **LspEnforcer** | `LspExecutor.execute()` 内部调用 | diagnostics, documentSymbols | 保留，增强后检 |
| **webview-provider** | `toolManager.getLspExecutor()` | workspaceSymbols | 保留不变 |
| **LocalSearchEngine** | 未来通过 `LspExecutor.execute()` | workspaceSymbols | 待实施 |

---

## 四、变更清单

### 4.1 tool-manager.ts — 移除 lsp_query 的 AI 工具注册

```typescript
// ===== BUILTIN_TOOL_NAMES =====
// Before (line 332):
'lsp_query',

// After: 删除此行

// ===== execute() switch =====
// Before (line 455-456):
case 'lsp_query':
  return await this.lspExecutor.execute(toolCall);

// After: 删除这两行（AI 不再能调用，系统内部直接调用 LspExecutor）

// ===== getBuiltinToolDefinitions() =====
// Before (line 743-744):
// 13. lsp_query (LSP 代码智能)
tools.push(this.lspExecutor.getToolDefinition());

// After: 删除这两行

// ===== 保留 =====
// getLspExecutor() (line 1038-1040): 保留，供系统内部使用
// LspExecutor 实例化 (line 118/149/179): 保留
// import LspExecutor (line 39): 保留
```

### 4.2 orchestrator-prompts.ts — 移除所有 lsp_query 提示词

#### 4.2.1 buildRequirementAnalysisPrompt

```
// Before (line 72): 分析/理解项目结构（使用 codebase_retrieval 语义搜索 + lsp_query 符号分析，...）
// After:            分析/理解项目结构（使用 codebase_retrieval 语义搜索，禁止逐个读取所有文件）

// Before (line 75): 查找符号定义/引用/实现/类型/调用链（使用 lsp_query ...）
// After: 删除整行

// Before (line 76): 检查代码诊断/编译错误（使用 lsp_query diagnostics）
// After: 删除整行
```

#### 4.2.2 buildUnifiedSystemPrompt — 回退工具列表

```
// Before (line 199): 代码智能：codebase_retrieval、lsp_query
// After:             代码智能：codebase_retrieval
```

#### 4.2.3 buildUnifiedSystemPrompt — 工具选择优先级

```
// Before (line 213-216): 4 条 LSP 映射
- 理解项目/分析代码 → codebase_retrieval（语义搜索）或 lsp_query（结构化分析），而非逐个读取文件
- 查找符号定义/引用/类型/实现 → lsp_query(...)，而非 grep_search
- 追溯调用链/影响分析 → lsp_query(callHierarchy)，而非手动追踪
- 检查编译错误 → lsp_query(diagnostics)，而非 launch-process tsc

// After: 1 条（仅保留 codebase_retrieval）
- 理解项目/分析代码 → codebase_retrieval（语义搜索），而非逐个读取文件
```

#### 4.2.4 buildUnifiedSystemPrompt — 工具协作链

```
// Before (3 条链 10 步，其中 8 步涉及 lsp_query):

分析/理解项目时（禁止逐个读取所有文件）：
1. codebase_retrieval — 语义搜索
2. lsp_query(workspaceSymbols/documentSymbols) — 理解模块结构
3. text_editor(view) — 仅读取关键文件

编辑文件前必须先理解代码：
1. codebase_retrieval 或 lsp_query(definition/typeDefinition/references)
2. lsp_query(implementation) — 查找实现
3. lsp_query(hover) — 确认类型签名
4. grep_search + text_editor(str_replace)

重构/修复影响分析：
1. lsp_query(callHierarchy, direction=incoming)
2. lsp_query(references) — 查找引用点
3. lsp_query(diagnostics) — 验证无新增错误


// After (2 条链 5 步，无 lsp_query):

分析/理解项目时（禁止逐个读取所有文件）：
1. codebase_retrieval — 语义搜索，快速找到相关代码区域
2. text_editor(view) — 仅读取真正需要细看的关键文件

编辑文件前必须先理解代码：
1. codebase_retrieval — 定位相关代码
2. grep_search — 精确匹配具体修改点
3. text_editor(str_replace) — 精确修改
```

### 4.3 mission-driven-engine.ts — 移除 lsp_query 的工具描述

```typescript
// Before (line 2312):
'lsp_query': { category: '代码智能', desc: 'LSP 代码智能查询（定义、引用、符号）' },

// After: 删除此行
```

### 4.4 lsp-enforcer.ts — 移除引导 Worker 调用 lsp_query 的文本

```typescript
// Before (line 274):
sections.push('要求: 在修改前先确认诊断与符号信息；如需精确定位引用/定义/类型/调用链，
必须调用 lsp_query 进行进一步查询（支持 definition、typeDefinition、implementation、
references、hover、callHierarchy 等操作）。');

// After:
sections.push('要求: 在修改前先确认以上诊断与符号信息，确保修改不会引入新的编译错误。');
```

**理由**：lsp_query 已从工具列表移除，Worker 无法再调用它。引导文本应改为告知 Worker 利用已注入的预检信息，而非引导其调用不存在的工具。

### 4.5 webview-provider.ts — 保留但清理注释

```typescript
// line 2802 注释：
// Before: // 2. ACE 不可用，使用本地多策略搜索（grep + LSP + 知识库索引）
// After:  // 2. ACE 不可用，使用本地多策略搜索（grep + 符号搜索 + 知识库索引）

// lspSymbolSearchForContext 方法 (line 3036-3095): 保留不变
// 该方法通过 toolManager.getLspExecutor() 直接调用，不依赖工具注册
```

### 4.6 lsp-executor.ts — 保留全部代码

```
不做任何修改。
- getToolDefinition(): 保留（LspEnforcer/webview-provider 可能使用工具名构造 ToolCall）
- getTools(): 保留
- execute(): 保留
- 所有 13 个 handler: 保留
- canHandle(): 保留
```

**理由**：LspExecutor 作为内部组件继续服务 LspEnforcer 和 webview-provider，其代码不需要任何改动。移除的只是它在 ToolManager 中的"AI 工具注册"。

---

## 五、LspEnforcer 增强：执行后诊断验证（本期新增）

### 5.1 动机

当前 LspEnforcer 只做执行前预检。增加执行后诊断对比，可自动检测 Worker 是否引入了新的编译错误。

### 5.2 新增接口

```typescript
// lsp-enforcer.ts 新增方法
async postCheck(
  assignment: Assignment,
  preflightDiagnostics: string[]  // 执行前的诊断快照
): Promise<{ newErrors: string[] } | null>
```

### 5.3 工作流

```
AssignmentExecutor.execute():
  1. lspEnforcer.applyIfNeeded(assignment)  ← 已有：注入预检信息
  2. 保存预检诊断快照
  3. worker.executeAssignment(assignment)   ← Worker 执行
  4. lspEnforcer.postCheck(assignment, savedDiagnostics)  ← 新增：后检
  5. 如果 newErrors.length > 0，将错误信息追加到执行结果中
```

### 5.4 实现要点

- postCheck 对相同目标文件再次调用 `diagnostics`
- 对比执行前后的诊断列表，找出新增项
- 不触发重试（避免无限循环），仅将新增错误附加到 Worker 的执行结果摘要中
- 如果 LSP 查询失败，静默跳过（不影响正常流程）

---

## 六、不做什么

1. **不删除 LspExecutor 的任何代码** — 所有 handler、类型定义、接口完整保留
2. **不修改 LspExecutor 的 execute() 签名** — 内部调用方式不变
3. **不修改 webview-provider 的 lspSymbolSearchForContext** — 它直接调用 LspExecutor，不依赖工具注册
4. **不在 webview UI 中添加 LSP 相关面板** — webview 定位是编排工作流界面，LSP UI 由 VS Code 原生提供

---

## 七、影响面分析

### 7.1 完整引用清单与变更判定

| 文件 | 行号 | 引用内容 | 变更 |
|:-----|:-----|:--------|:-----|
| `tool-manager.ts` | 14 | 注释 `lsp_query: LSP 代码智能查询` | 删除 |
| `tool-manager.ts` | 332 | `'lsp_query'` in BUILTIN_TOOL_NAMES | 删除 |
| `tool-manager.ts` | 455-456 | `case 'lsp_query':` switch 分支 | 删除 |
| `tool-manager.ts` | 743-744 | `tools.push(this.lspExecutor.getToolDefinition())` | 删除 |
| `tool-manager.ts` | 39 | `import { LspExecutor }` | **保留** |
| `tool-manager.ts` | 118/149/179 | LspExecutor 实例化和 workspace 更新 | **保留** |
| `tool-manager.ts` | 1038-1040 | `getLspExecutor()` 公开方法 | **保留** |
| `orchestrator-prompts.ts` | 72 | `lsp_query 符号分析` | 修改：移除 lsp_query 部分 |
| `orchestrator-prompts.ts` | 75 | `查找符号定义/引用...lsp_query` | 删除整行 |
| `orchestrator-prompts.ts` | 76 | `检查代码诊断...lsp_query diagnostics` | 删除整行 |
| `orchestrator-prompts.ts` | 199 | `代码智能：codebase_retrieval、lsp_query` | 修改：移除 lsp_query |
| `orchestrator-prompts.ts` | 213-216 | 工具选择优先级 4 条 LSP 映射 | 替换为 1 条 |
| `orchestrator-prompts.ts` | 226-240 | 工具协作链 3 条含 lsp_query | 替换为 2 条无 lsp_query |
| `mission-driven-engine.ts` | 2312 | `'lsp_query': { category, desc }` | 删除 |
| `lsp-enforcer.ts` | 185 | `name: 'lsp_query'` (queryLsp 内部构造) | **保留**（内部调用） |
| `lsp-enforcer.ts` | 274 | 引导 Worker 调用 lsp_query 的文本 | 修改：移除工具引导 |
| `webview-provider.ts` | 2802 | 注释 `grep + LSP + 知识库索引` | 修改注释 |
| `webview-provider.ts` | 3058 | `name: 'lsp_query'` (lspSymbolSearchForContext) | **保留**（内部调用） |
| `lsp-executor.ts` | 全部 | LspExecutor 完整实现 | **保留** |

### 7.2 风险评估

| 风险 | 概率 | 影响 | 缓解措施 |
|:-----|:-----|:-----|:---------|
| Worker 在 LspEnforcer 引导下仍尝试调用 lsp_query | 低 | LLM 收到 unknown tool 错误 | 修改引导文本（4.4），不再提及 lsp_query |
| webview-provider 的 lspSymbolSearchForContext 受影响 | 无 | — | 该方法通过 `getLspExecutor()` 直接调用，不经过工具注册 |
| 编排者需要 LSP 信息但无法获取 | 低 | 编排者用 codebase_retrieval/grep 替代 | LspEnforcer 自动注入预检上下文；codebase_retrieval 覆盖大部分场景 |

---

## 八、验收标准

| # | 标准 | 要求 |
|:--|:-----|:-----|
| 1 | TypeScript 编译 | `npx tsc --noEmit` 零错误 |
| 2 | AI 工具列表 | `ToolManager.getTools()` 返回的工具列表中不包含 `lsp_query` |
| 3 | BUILTIN_TOOL_NAMES | 数组中不包含 `'lsp_query'` |
| 4 | execute() switch | 不包含 `case 'lsp_query':` 分支 |
| 5 | 提示词 | `buildRequirementAnalysisPrompt` 和 `buildUnifiedSystemPrompt` 中不包含 `lsp_query` 字符串 |
| 6 | 工具摘要 | `mission-driven-engine.ts` 的工具描述映射中不包含 `lsp_query` |
| 7 | LspEnforcer 引导文本 | 不再引导 Worker 调用 `lsp_query`，改为利用预检信息 |
| 8 | LspEnforcer 预检 | `applyIfNeeded()` 正常工作（diagnostics + documentSymbols） |
| 9 | 内部调用兼容 | `toolManager.getLspExecutor().execute(...)` 正常返回结果 |
| 10 | webview 本地搜索 | `lspSymbolSearchForContext()` 正常工作 |
| 11 | LspEnforcer 后检 | `postCheck()` 方法存在，能检测新增诊断 |
| 12 | LspExecutor 代码完整 | 所有 13 个 handler 方法保留，无删除 |

---

## 九、提示词量化对比

| 维度 | v1.0（9 action 精简） | v2.0（基础设施化） | 缩减 |
|:-----|:---------------------|:------------------|:-----|
| AI 工具列表中的 LSP 工具 | 1 个（lsp_query） | 0 个 | -100% |
| 工具判定列表 LSP 条目 | 2 条 | 0 条 | -100% |
| 工具选择优先级 LSP 条目 | 4 条 | 0 条 | -100% |
| 工具协作链步数 | 10 步（含 8 步 LSP） | 5 步（0 步 LSP） | -50% |
| LSP 相关提示词总行数 | ~17 行 | 0 行 | -100% |


---

## 十、ACE 回退替代方案 — 本地搜索引擎降级（本期新增）

> **版本**: v2.1
> **日期**: 2025-07-14
> **依赖**: LocalSearchEngine (Sprint 1-3) + IndexPersistence

### 10.1 背景

ACE（远程语义搜索服务）是系统的主搜索通道。当 ACE 不可用（未配置、网络故障、服务下线）时，系统需要有完整的本地替代能力，而非直接报错。

### 10.2 ACE 消费场景清单

| # | 消费者 | 调用路径 | ACE 不可用时的行为 |
|:--|:-------|:---------|:-----------------|
| 1 | **Worker/编排器工具调用** | `ToolManager.execute('codebase_retrieval')` → `AceExecutor.execute()` | **Before**: 直接报错 `ACE API not configured`<br>**After**: 自动回退到 LocalSearchEngine |
| 2 | **webview 提示词增强** | `collectCodeContext()` → `tryAceSemanticSearch()` | 回退到 `performLocalContextSearch()` (已接入 LocalSearchEngine) |
| 3 | **编排提示词引用** | `orchestrator-prompts.ts` 中 `codebase_retrieval` | 不变（工具名不变，后端透明降级） |

### 10.3 核心设计：AceExecutor 透明降级

```
codebase_retrieval 工具调用
  ↓
AceExecutor.execute(query)
  ↓
┌─ ACE 已配置且可用?
│    YES → 远程语义搜索 (AceIndexManager.search)
│    │      ↓ 搜索失败?
│    │        YES → 回退到本地搜索
│    │        NO  → 返回结果
│    NO  → 本地搜索回退 (LocalSearchFallback)
│            ↓
│         PKB.search(query)
│            ↓
│         LocalSearchEngine.search()
│            TF-IDF + SymbolIndex + DependencyGraph + QueryExpander
│            ↓
│         格式化为与 ACE 兼容的输出
│            ↓
│         返回（LLM 无感知差异）
```

### 10.4 实现清单

#### 10.4.1 ace-executor.ts — 增加回退机制

```typescript
// 新增类型
export type LocalSearchFallback = (query: string, maxResults?: number) => Promise<string | null>;

// AceExecutor 新增
private localSearchFallback: LocalSearchFallback | null = null;

setLocalSearchFallback(fallback: LocalSearchFallback): void;

// execute() 修改
// ACE 可用 → 远程搜索 → 失败时回退本地
// ACE 不可用 → 直接本地搜索
// 本地搜索也不可用 → 才报错

// 新增私有方法
private async executeLocalFallback(toolCallId, query, aceError?): Promise<ToolResult>
```

#### 10.4.2 webview-provider.ts — 注入回退回调

```typescript
// initializeProjectKnowledgeBase() 中新增
this.injectLocalSearchFallback();

// 新增方法
private injectLocalSearchFallback(): void {
  const aceExecutor = toolManager.getAceExecutor();
  aceExecutor.setLocalSearchFallback(async (query, maxResults) => {
    return pkb.search(query, { maxResults, maxContextLength: 6000 });
  });
}
```

### 10.5 降级层级

```
Level 0: ACE 远程语义搜索（最佳质量）
  ↓ ACE 不可用或搜索失败
Level 1: LocalSearchEngine 本地索引搜索（TF-IDF + 符号 + 依赖图）
  ↓ LocalSearchEngine 未初始化
Level 2: 报错，提示用户配置 ACE 或使用 grep_search
```

### 10.6 LLM 无感知原则

| 维度 | 说明 |
|:-----|:-----|
| 工具名 | 不变：`codebase_retrieval` |
| 工具描述 | 不变（已描述为"代码库搜索"，不强调 ACE） |
| 输出格式 | 兼容：本地搜索结果格式化为类似 ACE 的 `Query + 代码片段` 格式 |
| 错误处理 | 改善：从"直接报错"到"静默降级 + 搜索成功" |
| 提示词 | 不需要修改：`codebase_retrieval` 引用全部保留 |

### 10.7 与 LSP 基础设施化的协同

```
ACE 可用:
  LspEnforcer (预检/后检)  +  ACE 语义搜索  =  最佳体验

ACE 不可用:
  LspEnforcer (预检/后检)  +  LocalSearchEngine 本地索引  =  优雅降级
  │                            │
  │ diagnostics + symbols      │ TF-IDF + SymbolIndex + DependencyGraph
  │ 自动注入 guidancePrompt    │ 透明替代 codebase_retrieval
  │ 100% 触发率                │ 持久化 + 增量更新 + 查询扩展
  ↓                            ↓
  LLM 不感知 LSP 存在          LLM 不感知 ACE 是否在线
```

### 10.8 验收标准

| # | 标准 | 要求 |
|:--|:-----|:-----|
| 1 | ACE 未配置时 `codebase_retrieval` 不报错 | LocalSearchEngine 可用时返回本地搜索结果 |
| 2 | ACE 搜索失败时自动回退 | 返回本地搜索结果，不中断 Worker 执行 |
| 3 | 本地搜索结果格式 | 与 ACE 输出格式兼容（Query + 代码片段） |
| 4 | 注入链路完整 | PKB 初始化 → setLocalSearchFallback → AceExecutor 可用 |
| 5 | 编译通过 | `npm run compile` 零错误 |