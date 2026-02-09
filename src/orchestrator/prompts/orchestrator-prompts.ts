/**
 * 编排者专用 Prompt 模板
 *
 * 核心理念：
 * - 编排者 Claude 专职编排，不执行任何编码任务
 * - 所有 Prompt 都围绕"分析、规划、监控、汇总"设计
 * - 统一编排模式：单次 LLM 调用 + 工具循环（ReAct 模式）
 */

import { WorkerSlot } from '../protocols/types';
import type { DispatchEntry } from '../core/dispatch-batch';

// ============================================================================
// Phase 2: 需求分析 Prompt（合并目标理解 + 路由决策）
// ============================================================================

/**
 * 构建需求分析 Prompt
 * Phase 2: 一次 LLM 调用，同时输出目标理解和路由决策
 *
 * @see docs/workflow/workflow-design.md - 5 阶段工作流
 */
export function buildRequirementAnalysisPrompt(
  userPrompt: string,
  recommendedMode: string,
  categoryHints: string,
  sessionContext?: string,
  availableToolsSummary?: string
): string {
  const contextSection = sessionContext?.trim()
    ? `## 最近会话上下文（用于解析省略指令）
${sessionContext}

`
    : '';

  const toolsSection = availableToolsSummary?.trim()
    ? `## 当前可用的工具和能力
${availableToolsSummary}

`
    : '';

  return `你是一个任务编排者，请完成需求分析，同时输出目标理解和路由决策。

## 用户请求
${userPrompt}

${contextSection}${toolsSection}## 上游推荐模式
${recommendedMode}

## 画像任务类型（仅用于理解任务分类）
${categoryHints}

## 分析要求

### 1. 目标理解
- 用户想要达成什么（goal）
- 任务的复杂度和关键点（analysis）
- 任何限制条件（constraints）
- 如何判断任务完成（acceptanceCriteria）
- 风险评估（riskLevel: low/medium/high, riskFactors）

### 2. 路由决策
- needsWorker 必须显式输出布尔值 true 或 false（禁止空值、null、字符串、省略）
- needsWorker=false 且 needsTooling=false 时，必须提供 directResponse
- needsWorker=false 且 needsTooling=true 时，directResponse 可选（用于说明将执行的动作）
- needsWorker=true 时，给出 delegationBriefings（任务委派说明）
- **涉及代码/文件修改时，必须 needsWorker=true**
- **工具调用任务判定（积极使用工具）**：
  - 以下场景必须设置 needsWorker=false，needsTooling=true，executionMode=direct：
    - 分析/理解项目结构（使用 codebase_retrieval 语义搜索，禁止逐个读取所有文件）
    - 查看特定文件内容（使用 text_editor view，不要用终端 ls/cat/find）
    - 搜索代码内容（使用 grep_search 精确匹配或 codebase_retrieval 语义搜索，不要用终端 grep/rg）
    - 运行编译/测试/构建命令（npm run build、npm test 等，使用 launch-process）
    - 执行 git 命令（git status、git log、git diff 等，使用 launch-process）
    - 检查进程、端口、环境变量（使用 launch-process）
    - **搜索互联网信息（使用 web_search 工具，而非启动浏览器）**
    - **获取网页内容（使用 web_fetch 工具，而非启动浏览器）**
    - 用户明确要求"执行/运行/查看/检查/列出"某个操作
    - **用户的请求可以通过已安装的 MCP 工具或 Skill 完成**
    - **用户提到的关键词与某个已安装的 MCP/Skill 工具名或描述匹配**
    - 任何可以通过一次或几次工具调用完成、且不涉及代码编写/文件创建修改的任务
  - **优先使用工具**：当用户请求既可以纯文字回答、也可以通过工具获得更准确的结果时，优先选择 needsTooling=true
  - 仅在以下情况才需要 needsWorker=true：需要修改/创建文件、多步复杂工具链、或高风险操作
- **复杂工程任务必须 needsWorker=true**：
  - 包含"搭建"、"开发"、"实现"、"创建"等动词 + 系统/模块/后台/平台等名词
  - 涉及多个功能模块
  - 需要创建多个文件或目录结构
- **若用户输入是"继续/然后/接着/按刚才方案"等省略指令**：
  - 必须优先结合"最近会话上下文"还原真实目标
  - 仅当上下文确实为空或无法判定时，才允许输出"缺乏上下文"
- 根据任务性质选择 executionMode：
  - direct: 简单任务（无需 Todo）
  - sequential: 有依赖或需按序执行
  - parallel: 多模块且无依赖
  - dependency_chain: 明确依赖链（前置产出作为后续输入）

## 输出格式

用自然语言简要说明你的理解，然后输出 JSON。不要使用固定的标题格式。

JSON 结构：
\`\`\`json
{
  "goal": "用户想要达成什么",
  "analysis": "任务的复杂度和关键点",
  "constraints": ["任何限制条件"],
  "acceptanceCriteria": ["如何判断任务完成"],
  "riskLevel": "low|medium|high",
  "riskFactors": ["可能的风险因素"],
  "needsWorker": true/false（必填，且必须是布尔值）,
  "directResponse": "needsWorker=false 且无需工具时必须提供；工具直执场景可选",
  "delegationBriefings": ["给执行者的委托说明（可多条，对应多个 Worker）"],
  "executionMode": "direct|sequential|parallel|dependency_chain",
  "needsTooling": true/false,
  "requiresModification": true/false,
  "reason": "决策理由（用户可见）"
}
\`\`\``;
}

// ============================================================================
// 统一编排：系统提示词构建器
// ============================================================================

/**
 * 统一系统提示词上下文
 */
export interface UnifiedPromptContext {
  /** 可用 Worker 列表 */
  availableWorkers: WorkerSlot[];
  /** Worker 画像（动态来源于 ProfileLoader）。提供时使用画像数据，否则回退硬编码 */
  workerProfiles?: Array<{ worker: WorkerSlot; displayName: string; strengths: string[] }>;
  /** 项目上下文（项目信息、技术栈等） */
  projectContext?: string;
  /** 会话历史摘要 */
  sessionSummary?: string;
  /** 知识库 ADR */
  relevantADRs?: string;
  /** 动态可用工具摘要（内置 + MCP + Skill，由 ToolManager 生成） */
  availableToolsSummary?: string;
}

/**
 * 构建统一系统提示词（ReAct 模式）
 *
 * 取代 IntentGate + analyzeRequirement 的两阶段调用，
 * 将角色定义、Worker 能力、决策原则、项目上下文融合为单一提示词。
 * LLM 在此提示词下通过工具循环自主决策：直接回答 / 工具操作 / 分配 Worker。
 */
export function buildUnifiedSystemPrompt(context: UnifiedPromptContext): string {
  const { availableWorkers, workerProfiles, projectContext, sessionSummary, relevantADRs, availableToolsSummary } = context;

  // Worker 能力描述表（优先使用动态画像，回退硬编码）
  const workerTable = availableWorkers.map(w => {
    const profile = workerProfiles?.find(p => p.worker === w);
    if (profile) {
      return `| ${w} | ${profile.displayName} | ${profile.strengths.join('、')} |`;
    }
    const fallback: Record<WorkerSlot, { model: string; strengths: string }> = {
      claude: { model: 'Claude', strengths: '架构设计、深度分析、代码重构、复杂多文件修改' },
      codex: { model: 'Codex', strengths: '快速代码生成、Bug 修复、测试编写、API 开发' },
      gemini: { model: 'Gemini', strengths: '多模态理解、前端 UI/UX、文档分析、样式优化' },
    };
    const { model, strengths } = fallback[w];
    return `| ${w} | ${model} | ${strengths} |`;
  }).join('\n');

  const sections: string[] = [];

  // 角色定义
  sections.push(`你是 MultiCLI，一个能协调多个专业 AI 协作完成复杂开发任务的编程助手。

## 身份
- 你运行在 VSCode 插件中，拥有完整的文件系统和终端访问能力
- 你可以直接回答问题、使用工具操作代码、或将复杂任务分配给专业 Worker
- 你的回答应当简洁、专业、直接`);

  // Worker 能力
  sections.push(`## 可用 Worker
当任务涉及多步代码操作或需要专业领域知识时，使用 dispatch_task 分配给 Worker：

| Worker | 模型 | 擅长领域 |
|--------|------|----------|
${workerTable}

对于超复杂的多 Worker 协作任务，使用 plan_mission 创建完整的协作计划。`);

  // 决策原则（三层执行模型）— 层级2工具列表从 ToolManager 动态注入
  const toolsListSection = availableToolsSummary?.trim()
    ? `\n${availableToolsSummary}`
    : `
- 文件操作：text_editor、grep_search、remove_files
- 终端命令：launch-process、read-process、write-process、kill-process、list-processes
- 网络工具：web_search、web_fetch
- 代码智能：codebase_retrieval
- 可视化：mermaid_diagram`;

  sections.push(`## 决策原则
根据任务复杂度，选择最经济的执行方式：

**层级 1 - 直接响应**：不调用任何工具
- 问候、知识问答、代码解释、方案建议
- 简短的概念说明或技术对比

**层级 2 - 工具操作**：调用已注册工具自行完成
${toolsListSection}

**工具选择优先级**（当有多个工具可完成同一任务时，选择更专用的工具）：
- 理解项目/分析代码 → codebase_retrieval（语义搜索），而非逐个读取文件
- 搜索代码内容 → grep_search（精确匹配）或 codebase_retrieval（语义搜索），而非 launch-process grep/rg
- 读取特定文件内容 → text_editor(view)，而非 launch-process cat
- 浏览目录结构 → text_editor(view + 目录路径)，而非 launch-process ls/find
- 搜索互联网 → web_search，而非 launch-process curl
- 获取网页内容 → web_fetch，而非 launch-process curl/wget
- launch-process 仅用于需要真正运行进程的场景：构建(npm build)、测试(npm test)、git 操作、启动服务等

**工具协作链**：

分析/理解项目时（禁止逐个读取所有文件）：
1. codebase_retrieval — 语义搜索，快速找到相关代码区域
2. text_editor(view) — 仅读取真正需要细看的关键文件

简单文件修改（改名、typo、改配置等 1-3 个文件内的小改动）：
1. text_editor(view) — 先查看要修改的文件
2. text_editor(str_replace) — 精确修改

**编排者直改规则**：你可以直接修改最多 3 个文件。超过 3 个文件的修改必须通过 dispatch_task 委派给 Worker。
涉及代码逻辑的复杂修改（新功能、重构、多文件联动），即使不超过 3 个文件也应优先委派 Worker。

**层级 3 - 分配 Worker**：使用 dispatch_task 委托
- 涉及代码逻辑的复杂修改（新功能开发、重构、多文件联动）
- 需要专业领域知识的任务（前端 → gemini，后端 → codex，架构 → claude）
- 大规模重构或新功能开发
- 需要多个 Worker 协作时，拆分为多个 dispatch_task 或使用 plan_mission

**原则**：能层级 1 解决的不用层级 2，能层级 2 解决的不用层级 3。`);

  // dispatch_task 使用指南
  sections.push(`## dispatch_task 使用指南
- task 参数应包含清晰的目标、约束和验收标准
- files 参数帮助 Worker 定位关键文件，尽量提供
- Worker 执行是异步的，执行完成后结果会自动返回
- 多个独立的 dispatch_task 可以依次发起，Worker 会并行执行`);

  // 项目上下文
  if (projectContext) {
    sections.push(`## 项目上下文\n${projectContext}`);
  }

  // ADR
  if (relevantADRs) {
    sections.push(`## 相关架构决策\n${relevantADRs}`);
  }

  // 会话上下文
  if (sessionSummary) {
    sections.push(`## 当前会话\n${sessionSummary}`);
  }

  return sections.join('\n\n');
}

// ============================================================================
// Phase C: dispatch_task 汇总提示词
// ============================================================================

/**
 * 构建 dispatch_task Phase C 汇总提示词
 * 基于 DispatchBatch 中所有 Worker 的执行结果，生成面向用户的最终结论
 */
export function buildDispatchSummaryPrompt(
  userPrompt: string,
  entries: DispatchEntry[],
): string {
  const resultsText = entries
    .map(e => {
      const statusLabel = e.status === 'completed' ? '成功' : e.status === 'failed' ? '失败' : '跳过';
      const files = e.result?.modifiedFiles?.join(', ') || '无';
      const summary = e.result?.summary || '无输出';
      const errors = e.result?.errors?.join('; ') || '';
      return `### Worker ${e.worker} [${statusLabel}]
**任务**: ${e.task.substring(0, 120)}
**修改文件**: ${files}
**摘要**: ${summary}${errors ? `\n**错误**: ${errors}` : ''}`;
    })
    .join('\n\n');

  return `请根据以下 Worker 执行结果，为用户生成简洁的任务完成总结。

## 用户原始需求
${userPrompt}

## Worker 执行结果
${resultsText}

## 要求
1. 用 1-3 句话概括完成情况
2. 列出关键修改内容和涉及的文件
3. 如有失败的 Worker，说明原因和建议
4. 不要输出代码块或 diff
5. 保持简洁，控制在 10 行以内
6. 用中文回复，Markdown 格式`;
}
