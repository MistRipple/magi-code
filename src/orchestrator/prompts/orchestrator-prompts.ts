/**
 * 编排者专用 Prompt 模板
 * 
 * 核心理念：
 * - 编排者 Claude 专职编排，不执行任何编码任务
 * - 所有 Prompt 都围绕"分析、规划、监控、汇总"设计
 */

import { WorkerSlot, ExecutionResult, SubTask, ExecutionPlan } from '../protocols/types';

// ============================================================================
// 模型能力描述
// ============================================================================

/** 获取 Worker 能力描述 */
export function getWorkerDescription(worker: WorkerSlot): string {
  const descriptions: Record<WorkerSlot, string> = {
    claude: `Worker Claude (代码执行者)
      - 擅长: 复杂代码实现、架构重构、多文件修改、代码审查
      - 最适合: 需要深度理解的编码任务、复杂重构、架构调整
      - 特点: 推理能力强，适合需要深度思考的编码任务`,
    codex: `Worker Codex (后端专家)
      - 擅长: 快速代码生成、Bug 修复、代码补全、测试编写、算法实现
      - 最适合: 简单 Bug 修复、功能实现、单元测试、代码调试
      - 特点: 执行速度快，适合明确的编码任务`,
    gemini: `Worker Gemini (前端专家)
      - 擅长: 多模态理解、前端 UI/UX、CSS 样式、React/Vue 组件
      - 最适合: 前端开发、UI 组件、样式优化、图片理解
      - 特点: 多模态能力强，适合视觉相关任务`,
  };
  return descriptions[worker] || '通用编程助手';
}

// ============================================================================
// Phase 1: 任务分析 Prompt
// ============================================================================

/**
 * 构建任务分析 Prompt
 * 编排者分析用户需求，生成执行计划
 */
export function buildOrchestratorAnalysisPrompt(
  userPrompt: string,
  availableWorkers: WorkerSlot[],
  projectContext?: string
): string {
  const workersDesc = availableWorkers
    .map(w => `- ${w}: ${getWorkerDescription(w)}`)
    .join('\n');

  return `你是一个智能任务编排器（Orchestrator）。你的职责是分析用户需求并制定执行计划。

**重要**：你只负责分析和规划，不执行任何编码任务。所有编码工作将由 Worker 执行。
**严禁输出计划之外的说明性文本**（例如“我将先探索项目”、“我处于计划模式”等），这些都会导致计划被丢弃。

## 用户需求
${userPrompt}

## 可用的 Worker
${workersDesc}

${projectContext ? `## 项目上下文\n${projectContext}\n` : ''}

## 分析任务
请分析用户需求，判断：
1. 这个任务是否需要多 Worker 协作？
2. 如果需要协作，应该如何分配任务？
3. 任务之间是否有依赖关系（需要串行）还是可以并行？
4. 是否为简单任务（单个 Worker 即可完成）？
5. 如果是咨询/解释/方案建议等不需要执行的内容，直接给出回答

## 工具限制
本阶段禁止调用任何工具（包括文件读取、命令执行等）。只根据当前提示生成计划。
**禁止产生 tool_use/tool_call 相关内容或任何结构化工具调用。**

## 强制规则
1. 若任务涉及前后端协作（或同时需要 Codex 与 Gemini），必须包含 **Claude 架构/契约任务**，并让其他子任务依赖该任务。
2. 架构任务需明确：目录结构、接口契约、前后端对接约束。
3. **禁止返回解释性文本**。不要说"我现在..."、"让我..."、"首先需要..."等。

## 输出格式 - 严格要求
**关键：你的回复必须是纯 JSON，第一个字符是 {，最后一个字符是 }**
**不要输出任何解释、前缀、后缀或代码块标记（如 \`\`\`json）**
**不要输出除 JSON 之外的任何字符**
**直接输出以下 JSON 对象：**
{
  "analysis": "对任务的简要分析",
  "isSimpleTask": true/false,
  "needsWorker": true/false,
  "directResponse": "如果不需要 Worker，直接在此回答用户问题（可选）",
  "needsUserInput": true/false,
  "questions": ["需要用户补充的关键问题 1", "问题 2"],
  "skipReason": "如果不需要 Worker，说明原因",
  "needsCollaboration": true/false,
  "featureContract": "功能契约（接口、数据结构、交互约束的统一描述）",
  "acceptanceCriteria": [
    "验收标准 1",
    "验收标准 2"
  ],
  "subTasks": [
    {
      "id": "1",
      "shortTitle": "简短标题（≤20字，用于 Worker 卡片显示，如：分析依赖、重构模块）",
      "description": "子任务描述",
      "assignedWorker": "claude/codex/gemini",
      "reason": "选择该 Worker 的原因",
      "targetFiles": ["预计修改的文件列表"],
      "dependencies": [],
      "delegationBriefing": "用自然语言向 Worker 说明任务背景、你的理解、重点关注什么、期望产出是什么。像同事间的工作委托，而非机械指令。",
      "background": false
    }
  ],
  "executionMode": "parallel/sequential",
  "summary": "执行计划总结"
}

## 重要判断
- **不需要 Worker 的情况**（设置 needsWorker: false）：
  - 用户只是问问题、请求解释、咨询建议
  - 不涉及代码修改、文件创建、功能实现
  - 例如："这段代码什么意思？"、"如何实现 X？"、"帮我解释一下"
  - 此时直接在 directResponse 中回答，subTasks 留空

- **需要用户补充信息的情况**（设置 needsUserInput: true）：
  - 关键信息缺失，无法合理决策技术栈、范围或约束
  - 必须输出 questions 列表，等待用户补充后再生成计划
  - **禁止默认假设**或直接选定技术栈

- **需要 Worker 的情况**（设置 needsWorker: true）：
  - 需要修改代码、创建文件、实现功能
  - 例如："帮我重构这个函数"、"实现 X 功能"、"修复这个 bug"

## 重要约束
- **前端/UI/样式任务** → 分配给 Gemini
- **后端/逻辑/算法/Bug修复任务** → 分配给 Codex
- **复杂架构/重构/多文件修改** → 分配给 Claude
- **避免文件冲突**：不同 Worker 负责不同文件，有冲突时改为串行执行
- **Prompt 要详细**：每个子任务的 prompt 必须足够详细，Worker 能独立完成`;
}

// ============================================================================
// Phase 6: 汇总报告 Prompt
// ============================================================================

/**
 * 构建汇总报告 Prompt
 * 编排者整合所有 Worker 的执行结果
 */
export function buildOrchestratorSummaryPrompt(
  originalPrompt: string,
  executionResults: ExecutionResult[]
): string {
  const sanitizeOutput = (content: string): string => {
    const withoutFences = content.replace(/```[\s\S]*?```/g, '[代码块已省略]');
    const trimmed = withoutFences.trim();
    if (!trimmed) return '无';
    if (trimmed.length <= 400) return trimmed;
    return `${trimmed.slice(0, 400)}...(已截断)`;
  };

  const resultsText = executionResults
    .map(r => `### ${r.workerType} (${r.workerId}) 执行结果 (${r.success ? '[成功]' : '[失败]'})
**子任务 ID**: ${r.subTaskId}
**耗时**: ${r.duration}ms
**修改文件**: ${r.modifiedFiles?.join(', ') || '无'}
**输出**:
${sanitizeOutput(r.result || '')}
${r.error ? `**错误**: ${r.error}` : ''}
`)
    .join('\n');

  return `请根据以下执行结果，为用户生成一份简洁的总结报告。

## 原始需求
${originalPrompt}

## 各 Worker 执行结果
${resultsText}

## 要求
1. 总结完成了哪些工作
2. 如果有失败的任务，说明原因和建议
3. 不要输出代码块、diff 或文件清单（这些已在 Worker 面板中展示）
4. 避免重复叙述，同一内容只出现一次
5. 控制在 12-15 行以内，保持简洁
6. 给出后续建议（如需要）

请用简洁清晰的中文回复，使用 Markdown 格式。`;
}

// ============================================================================
// Phase 2: 需求分析 Prompt（合并目标理解 + 路由决策）
// ============================================================================

/**
 * 构建需求分析 Prompt
 * Phase 2: 一次 LLM 调用，同时输出目标理解和路由决策
 *
 * @see docs/workflow-design.md - 5 阶段工作流
 */
export function buildRequirementAnalysisPrompt(
  userPrompt: string,
  recommendedMode: string,
  categoryHints: string
): string {
  return `你是一个任务编排者，请完成需求分析，同时输出目标理解和路由决策。

## 用户请求
${userPrompt}

## 上游推荐模式
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
- needsWorker=false 时，必须提供 directResponse
- needsWorker=true 时，给出 delegationBriefings（任务委派说明）
- **涉及代码/文件修改、执行工具或工程性产出时，必须 needsWorker=true**
- **复杂工程任务必须 needsWorker=true**：
  - 包含"搭建"、"开发"、"实现"、"创建"等动词 + 系统/模块/后台/平台等名词
  - 涉及多个功能模块
  - 需要创建多个文件或目录结构
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
  "needsWorker": true/false,
  "directResponse": "当不需要 Worker 时必须提供",
  "delegationBriefings": ["给执行者的委托说明（可多条，对应多个 Worker）"],
  "executionMode": "direct|sequential|parallel|dependency_chain",
  "needsTooling": true/false,
  "requiresModification": true/false,
  "reason": "决策理由（用户可见）"
}
\`\`\``;
}

// ============================================================================
// Phase 0: 轻量路由判断 Prompt（已废弃，保留向后兼容）
// ============================================================================

/**
 * 构建 Worker 需求判断 Prompt
 * 由编排者 LLM 决策是否需要 Worker 执行
 *
 * @deprecated 请使用 buildRequirementAnalysisPrompt，它合并了目标理解和路由决策
 */
export function buildWorkerNeedDecisionPrompt(
  userPrompt: string,
  recommendedMode: string,
  categoryHints: string
): string {
  return `你是一个任务编排者，请完成意图到分配的统一决策。

## 用户请求
${userPrompt}

## 上游推荐模式
${recommendedMode}

## 画像任务类型（仅用于理解任务分类）
${categoryHints}

## 判断要求
1. needsWorker=false 时，必须提供 directResponse
2. needsWorker=true 时，只需给出 delegationBriefing（单条说明），不需要选择 Worker 或分类
3. **涉及代码/文件修改、执行工具或工程性产出（diff/代码/配置）时，必须 needsWorker=true**
4. **【关键】复杂工程任务必须 needsWorker=true**：
   - 包含"搭建"、"开发"、"实现"、"创建"等动词 + 系统/模块/后台/平台等名词 → **必须 needsWorker=true**
   - 涉及多个功能模块（如"商品管理、订单系统、数据看板"）→ **必须 needsWorker=true**
   - 需要创建多个文件或目录结构 → **必须 needsWorker=true**
5. 编排者可使用工具，但不应因此强制派发 Worker
6. requiresModification=true 仅在需要对文件产生实际修改（增删改文件）时设置；仅分析/阅读则为 false
7. **delegationBriefing**: 当 needsWorker=true 时，生成一段自然语言的任务委托说明，像同事间的工作交接，包含任务背景、重点关注点、期望产出

## 输出格式

用自然语言简要说明你的判断，然后输出 JSON。不要使用固定的标题格式。

JSON 结构：
\`\`\`json
{
  "needsWorker": true/false,
  "delegationBriefing": "给执行者的委托说明",
  "needsTooling": true/false,
  "requiresModification": true/false,
  "directResponse": "当不需要 Worker 时必须提供",
  "reason": "简短判断理由"
}
\`\`\``;
}

// ============================================================================
// 格式化函数
// ============================================================================

/**
 * 格式化执行计划为用户可读的文本
 */
export function formatPlanForUser(plan: ExecutionPlan): string {
  if (plan.isSimpleTask) {
    return `## 任务分析结果

**分析**: ${plan.analysis}

**注意**: 这是一个简单任务，无需多 Worker 协作。
原因: ${plan.skipReason || '任务复杂度较低'}

---
**是否同意直接执行？**`;
  }

  const tasksText = plan.subTasks
    .map((t, i) => {
      const filesInfo = t.targetFiles?.length
        ? `\n   - 目标文件: ${t.targetFiles.join(', ')}`
        : '';
      const depsInfo = t.dependencies.length
        ? `\n   - 依赖: ${t.dependencies.join(', ')}`
        : '';
      return `${i + 1}. **${t.description}**
   - 分配给: \`${t.assignedWorker}\`
   - 原因: ${t.reason}${filesInfo}${depsInfo}`;
    })
    .join('\n');

  const hasDependencies = plan.subTasks.some(task => task.dependencies && task.dependencies.length > 0);
  const executionModeText = hasDependencies
    ? '依赖图调度（含并行批次）'
    : (plan.executionMode === 'parallel' ? '并行执行' : '串行执行');

  return `## 执行计划

**分析**: ${plan.analysis}

### 功能契约
${plan.featureContract}

### 验收清单
${(plan.acceptanceCriteria || []).map(item => `- ${item}`).join('\n') || '- 未提供'}

### 子任务列表
${tasksText}

**执行模式**: ${executionModeText}

**总结**: ${plan.summary}

---
**注意**: 各 Worker 将直接修改文件。

**确认执行此计划？**`;
}

/**
 * 构建进度更新消息
 */
export function buildProgressMessage(
  completedTasks: number,
  totalTasks: number,
  currentWorker?: WorkerSlot,
  currentTask?: string
): string {
  const safeTotal = totalTasks > 0 ? totalTasks : 0;
  const rawProgress = safeTotal > 0 ? (completedTasks / safeTotal) * 100 : 0;
  const progress = Math.min(100, Math.max(0, Math.round(rawProgress)));
  const filled = Math.min(10, Math.max(0, Math.floor(progress / 10)));
  const progressBar = '█'.repeat(filled) + '░'.repeat(10 - filled);

  let message = `**进度**: [${progressBar}] ${progress}% (${completedTasks}/${totalTasks})`;

  if (currentWorker && currentTask) {
    message += `\n**当前**: ${currentWorker} 正在执行 "${currentTask}"`;
  }

  return message;
}

// ============================================================================
// Prompt 优化工具
// ============================================================================

/**
 * Prompt 压缩选项
 */
export interface PromptCompressionOptions {
  /** 移除多余空白行 */
  removeExtraWhitespace?: boolean;
  /** 移除注释块 */
  removeComments?: boolean;
  /** 截断上下文长度 */
  maxContextLength?: number;
  /** 使用紧凑格式 */
  compact?: boolean;
}

/**
 * 压缩 Prompt 以减少 Token 使用
 */
export function compressPrompt(prompt: string, options: PromptCompressionOptions = {}): string {
  let result = prompt;

  // 移除多余空白行
  if (options.removeExtraWhitespace !== false) {
    result = result.replace(/\n{3,}/g, '\n\n');
    result = result.replace(/[ \t]+$/gm, '');
  }

  // 移除注释块（可选）
  if (options.removeComments) {
    result = result.replace(/\/\*[\s\S]*?\*\//g, '');
    result = result.replace(/\/\/.*$/gm, '');
  }

  // 截断上下文
  if (options.maxContextLength && result.length > options.maxContextLength) {
    result = result.slice(0, options.maxContextLength) + '\n...[已截断]';
  }

  return result.trim();
}

/**
 * 估算 Token 数量（粗略估计：中文约 1.5 token/字，英文约 0.25 token/字）
 */
export function estimateTokenCount(text: string): number {
  const chineseChars = (text.match(/[\u4e00-\u9fff]/g) || []).length;
  const otherChars = text.length - chineseChars;
  return Math.ceil(chineseChars * 1.5 + otherChars * 0.25);
}

/**
 * 构建紧凑版任务分析 Prompt（减少约 30% Token）
 */
export function buildCompactAnalysisPrompt(
  userPrompt: string,
  availableWorkers: WorkerSlot[],
  projectContext?: string
): string {
  const workers = availableWorkers.map(w => {
    const short: Record<WorkerSlot, string> = {
      claude: 'Claude: 架构/重构/复杂任务',
      codex: 'Codex: 后端/算法/Bug修复',
      gemini: 'Gemini: 前端/UI/样式',
    };
    return short[w];
  }).join(' | ');

  const context = projectContext
    ? compressPrompt(projectContext, { maxContextLength: 2000 })
    : '';

  return `任务编排器。分析需求，制定计划。禁止执行代码。

需求: ${userPrompt}

Workers: ${workers}
${context ? `上下文: ${context}\n` : ''}
输出纯JSON:
{"analysis":"分析","isSimpleTask":bool,"needsWorker":bool,"directResponse":"直接回答(可选)","needsUserInput":bool,"questions":[],"needsCollaboration":bool,"subTasks":[{"id":"1","shortTitle":"≤10字标题","description":"描述","assignedWorker":"worker","targetFiles":[],"dependencies":[],"delegationBriefing":"自然语言委托说明"}],"executionMode":"parallel/sequential","summary":"总结"}

规则: 前端→Gemini, 后端→Codex, 复杂→Claude. 禁止tool_use.`;
}

// ============================================================================
// 导出
// ============================================================================

export const OrchestratorPrompts = {
  getWorkerDescription,
  buildOrchestratorAnalysisPrompt,
  buildOrchestratorSummaryPrompt,
  formatPlanForUser,
  buildProgressMessage,
  // 新增优化工具
  compressPrompt,
  estimateTokenCount,
  buildCompactAnalysisPrompt,
};
