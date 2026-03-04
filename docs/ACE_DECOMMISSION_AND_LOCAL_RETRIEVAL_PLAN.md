# ACE 全链路下线与本地代码检索基础设施升级方案

版本: v1.0  
日期: 2026-03-03  
状态: 已完成（代码链路已切换到本地检索单路径）  
适用范围: `magi` 扩展后端 + Webview 前端 + 提示词与文档体系

---

## 1. 目标与决策

本次改造目标不是“让 ACE 成为可选项”，而是：

1. 清理所有 ACE 相关实现、配置入口、文案与运行链路。
2. 将原“本地兜底检索”升级为正式基础设施，成为 `codebase_retrieval` 的唯一实现。
3. 保持外部工具契约稳定（工具名继续使用 `codebase_retrieval`），避免对模型能力与历史行为造成破坏性变更。

核心决策:

1. 不保留 ACE 回退分支，不做兼容双实现，不加 feature flag。
2. 不删除 `codebase_retrieval` 工具名，仅替换其执行内核与文案语义。
3. 本地检索必须具备可观测、可扩展、可验证的基础设施属性，而非“失败兜底”。

---

## 2. 五步分析（按工程规范）

### 2.1 表象分析

1. `codebase_retrieval` 在工具列表中可见，但在部分时序下不可用或结果不稳定。
2. 用户侧认知是“ACE 工具”，而系统内已有本地三级检索能力，职责边界混乱。
3. 设置面板仍暴露 ACE 配置，造成产品心智和行为不一致。

### 2.2 机理溯源

1. 工具注册是静态的，运行可用性依赖异步注入与配置状态。
2. ACE 与本地检索耦合在同一执行器中，导致职责混杂。
3. 提示词、适配器、前端卡片、配置处理均写入了 ACE 概念。

### 2.3 差距诊断

1. 现状是“远程优先 + 本地兜底”，目标是“本地唯一主路径”。
2. 现状的配置与文案强调 ACE，目标是产品层完全去 ACE 化。
3. 现状本地检索是降级策略，目标是具备独立 SLA 的基础设施。

### 2.4 根本原因

1. 工具语义（代码检索）与实现语义（ACE 服务）被绑定。
2. 产品配置复用了 `promptEnhance` 作为 ACE 配置载体，造成跨功能耦合。
3. 缺少“单一实现”治理，导致双路径长期并存。

### 2.5 彻底修复策略

1. 架构层: 移除 `ace` 目录与 `AceExecutor`，建立 `CodebaseRetrievalExecutor + CodebaseRetrievalService` 单路径实现。
2. 产品层: 删除前端 ACE 配置入口与后端配置写入逻辑中的 ACE 分支。
3. 认知层: 更新提示词、工具说明、README，统一为“本地语义检索基础设施”。

---

## 3. 改造范围

### 3.1 后端范围

1. 工具执行层与工具管理层。
2. 检索基础设施层（PKB/Grep/LSP 协作）。
3. 配置读写与运行时注入链路。
4. 模型适配器提示与检索策略相关逻辑。

### 3.2 前端范围

1. 设置面板中的 ACE 配置页与交互事件。
2. 工具卡片中 ACE 命名映射。
3. 与 ACE 配置相关的消息协议绑定。

### 3.3 文档范围

1. `README.md`、`magi-docs/README.md` 中 ACE 表述。
2. 工具能力说明中的“语义搜索”实现来源。

---

## 4. 目标架构（下线后）

```text
codebase_retrieval (工具契约不变)
        |
        v
CodebaseRetrievalExecutor  (唯一执行器)
        |
        v
CodebaseRetrievalService   (本地基础设施，唯一实现)
   |         |          |
   v         v          v
 PKB      grep_search  lsp_query
```

约束:

1. 不允许出现远程 ACE 请求路径。
2. 不允许出现“ACE 不可用时回退”文案。
3. 本地检索失败只允许返回本地链路错误，不再提及 ACE 配置。

---

## 5. 实施方案（工作流拆分）

## 5.1 工作流 A: 删除 ACE 实现与接线

动作:

1. 删除 `src/ace/index-manager.ts`。
2. 删除 `src/tools/ace-executor.ts`。
3. 新增 `src/tools/codebase-retrieval-executor.ts`，直接依赖本地检索服务。
4. `ToolManager` 删除 `aceExecutor` 字段、初始化、`configureAce/getAceExecutor/isAceConfigured`。
5. `ToolManager.executeBuiltinTool` 中 `codebase_retrieval` 改为走新执行器。
6. `WebviewProvider.injectCodebaseRetrievalService()` 中直接注入新执行器接口。

验收:

1. 全仓无 `AceExecutor`、`ACE configured`、`/agents/codebase-retrieval` 相关调用。
2. `codebase_retrieval` 仍可被模型调用且能稳定返回结果。

## 5.2 工作流 B: 本地检索基础设施升级

升级目标:

1. 角色升级: `LocalCodeSearchService` 从“fallback”重命名为“基础设施服务”，建议名 `CodebaseRetrievalService`。
2. 协议升级: 输出统一结构，包含 `sources`、`snippets`、`scores`、`timings`。
3. 查询升级:
- 强化 query 规范化（中英混合、符号抽取、路径线索抽取）。
- 增加 `scope_paths` 可选参数，支持目录/文件范围检索。
4. 排序升级:
- PKB/grep/LSP 结果统一重排（去重 + 相关度融合 + 长度预算）。
5. 可观测升级:
- 增加命中率、耗时、失败率、空结果率日志指标。
6. 运行升级:
- 启动预热索引，文件事件增量刷新。
- 缓存策略升级为可调 TTL/LRU 上限。

验收:

1. 常见检索场景 P95 响应时间与命中率达到目标阈值（在仓库内定义指标基线）。
2. 无远程依赖即可完成语义检索主流程。

## 5.3 工作流 C: 前端与配置清理

动作:

1. `SettingsPanel.svelte` 删除 ACE 配置 Tab 与相关状态字段。
2. `config-handler.ts` 删除 ACE 配置保存、测试与 toast 文案。
3. Webview 通信中删除 `promptEnhanceConfigLoaded/promptEnhanceResult` 的 ACE 用途分支。
4. `ToolCall.svelte` 将 `codebase_retrieval` 展示文案改为“本地检索”语义。

验收:

1. UI 不再出现 ACE 术语与 ACE 配置入口。
2. 设置页交互不再触发 ACE 配置消息。

## 5.4 工作流 D: 提示词与适配器统一

动作:

1. `orchestrator-prompts.ts` 中将 ACE 相关描述替换为本地语义检索基础设施描述。
2. `orchestrator-adapter.ts`、`worker-adapter.ts` 保留工具名，但移除 ACE 语义注释。
3. `tool-manager.ts` 内置工具说明改为“本地代码检索”。

验收:

1. 系统提示词中不再出现 ACE 字样。
2. 模型仍优先使用 `codebase_retrieval` 做项目理解。

## 5.5 工作流 E: 文档与发布清理

动作:

1. 更新 `README.md`、`magi-docs/README.md` 能力说明，去 ACE 化。
2. 变更日志新增“检索架构重构”章节。
3. 输出迁移说明: 配置层面无需 ACE 参数。

验收:

1. 对外文档无 ACE 概念残留。
2. 发布说明可解释检索行为变化与预期收益。

---

## 6. 文件级清理清单（初版）

### 6.1 删除文件

1. `src/ace/index-manager.ts`
2. `src/tools/ace-executor.ts`

### 6.2 核心修改文件

1. `src/tools/tool-manager.ts`
2. `src/tools/types.ts`
3. `src/services/codebase-retrieval-service.ts`
4. `src/ui/webview-provider.ts`
5. `src/ui/handlers/config-handler.ts`
6. `src/ui/webview-svelte/src/components/SettingsPanel.svelte`
7. `src/ui/webview-svelte/src/components/ToolCall.svelte`
8. `src/orchestrator/prompts/orchestrator-prompts.ts`
9. `src/llm/adapters/orchestrator-adapter.ts`
10. `src/llm/adapters/worker-adapter.ts`
11. `src/services/prompt-enhancer-service.ts`（ACE 相关注释更新：第 113、121 行引用了 AceExecutor 语义）
12. `README.md`
13. `magi-docs/README.md`

### 6.3 可能新增文件

1. `src/tools/codebase-retrieval-executor.ts`
2. `src/services/codebase-retrieval-service.ts`
3. `scripts/e2e-codebase-retrieval-regression.cjs`

---

## 7. 兼容与迁移策略

1. 保持工具名 `codebase_retrieval` 不变，避免模型调用面断裂。
2. 删除 ACE 配置后，历史配置项可忽略读取，不再在任何逻辑中生效。
3. 不新增兼容分支，不保留旧实现开关，避免双实现长期共存。

---

## 8. 验收标准

### 8.1 功能验收

1. `codebase_retrieval` 在无任何外部服务配置情况下可稳定工作。
2. 返回结果覆盖 PKB + grep + LSP 三类来源，且输出格式一致。
3. 编排者与 Worker 在深度模式/常规模式下都能正常调用该工具。

### 8.2 清理验收

1. 全仓检索无 `ACE`、`AceExecutor`、`/agents/codebase-retrieval` 运行时依赖。
2. 前端设置中无 ACE 配置入口与文案。
3. 提示词中无 ACE 概念残留。

### 8.3 构建与回归

1. `npm run compile`
2. `npm run build:extension`
3. `npm run build:webview`
4. `npm run verify:e2e:real-dispatch`
5. `npm run verify:e2e:codebase-retrieval`

---

## 9. 风险与控制

1. 风险: 本地检索在超大仓库下性能下降。  
控制: 增加索引预热、查询预算、分段裁剪、缓存和可观测指标。

2. 风险: 清理 ACE 时误删 prompt enhance 配置链。  
控制: `promptEnhance` 字段（`~/.magi/config.json`）是 ACE 专用配置（存储远程 baseUrl + apiKey），与 `PromptEnhancerService` 的 LLM 调用无关（后者使用 compressor/orchestrator 配置）。清理时直接删除 `promptEnhance` 读写逻辑即可，`PromptEnhancerService` 无需修改——其 `collectCodeContext()` 通过 `ToolManager.execute('codebase_retrieval')` 调用，会自动走新的本地检索路径。单独回归提示词增强功能确认不受影响。

3. 风险: 工具名变化导致模型行为漂移。  
控制: 工具名保持不变，仅替换实现内核与描述文本。

---

## 10. 执行顺序建议

1. 第 1 阶段: 新建本地检索执行器与服务升级（先并行可运行）。
2. 第 2 阶段: ToolManager 切换 `codebase_retrieval` 到新执行器。
3. 第 3 阶段: 清理 ACE 旧实现与配置接口。
4. 第 4 阶段: 清理前端设置与提示词文案。
5. 第 5 阶段: 文档与回归收口。

完成定义:

1. 代码中无 ACE 运行链路。
2. 产品界面无 ACE 入口。
3. `codebase_retrieval` 仅由本地基础设施驱动。

---

## 11. 实施结果（2026-03-03）

1. 已删除 ACE 运行时实现：`src/tools/ace-executor.ts`、`src/ace/index-manager.ts`。
2. 已删除旧本地兜底服务：`src/services/local-code-search-service.ts`。
3. 已上线新单路径基础设施：`CodebaseRetrievalExecutor + CodebaseRetrievalService`。
4. 已完成 ToolManager、Webview 注入、SettingsPanel、ConfigHandler、协议类型的全链路去 ACE 化。
5. 已新增 `codebase_retrieval` 专项回归脚本：`scripts/e2e-codebase-retrieval-regression.cjs`。
6. 全仓扫描结果：除本方案文档外，无 ACE 运行链路残留关键字。
