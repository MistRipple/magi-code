/**
 * 编排者专用 Prompt 模板
 * 
 * 核心理念：
 * - 编排者 Claude 专职编排，不执行任何编码任务
 * - 所有 Prompt 都围绕"分析、规划、监控、汇总"设计
 */

import { WorkerType, ExecutionResult, SubTask, ExecutionPlan } from '../protocols/types';

// ============================================================================
// CLI 能力描述
// ============================================================================

/** 获取 Worker 能力描述 */
export function getWorkerDescription(worker: WorkerType): string {
  const descriptions: Record<WorkerType, string> = {
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
  availableWorkers: WorkerType[],
  projectContext?: string
): string {
  const workersDesc = availableWorkers
    .map(w => `- ${w}: ${getWorkerDescription(w)}`)
    .join('\n');

  return `你是一个智能任务编排器（Orchestrator）。你的职责是分析用户需求并制定执行计划。

**重要**：你只负责分析和规划，不执行任何编码任务。所有编码工作将由 Worker 执行。

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

## 强制规则
1. 若任务涉及前后端协作（或同时需要 Codex 与 Gemini），必须包含 **Claude 架构/契约任务**，并让其他子任务依赖该任务。
2. 架构任务需明确：目录结构、接口契约、前后端对接约束。

## 输出格式
请以 JSON 格式输出执行计划：
\`\`\`json
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
      "description": "子任务描述",
      "assignedWorker": "claude/codex/gemini",
      "reason": "选择该 Worker 的原因",
      "targetFiles": ["预计修改的文件列表"],
      "dependencies": [],
      "prompt": "发送给该 Worker 的具体指令（英文，详细明确）"
    }
  ],
  "executionMode": "parallel/sequential",
  "summary": "执行计划总结"
}
\`\`\`

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
  const resultsText = executionResults
    .map(r => `### ${r.workerType} (${r.workerId}) 执行结果 (${r.success ? '✅ 成功' : '❌ 失败'})
**子任务 ID**: ${r.subTaskId}
**耗时**: ${r.duration}ms
**修改文件**: ${r.modifiedFiles?.join(', ') || '无'}
**输出**:
${r.result}
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
4. 给出后续建议（如需要）

请用简洁清晰的中文回复，使用 Markdown 格式。`;
}

// ============================================================================
// 格式化函数
// ============================================================================

/**
 * 格式化执行计划为用户可读的文本
 */
export function formatPlanForUser(plan: ExecutionPlan): string {
  if (plan.isSimpleTask) {
    return `## 📋 任务分析结果

**分析**: ${plan.analysis}

⚠️ **这是一个简单任务，无需多 Worker 协作。**
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
    ? '🔗 依赖图调度（含并行批次）'
    : (plan.executionMode === 'parallel' ? '⚡ 并行执行' : '🔗 串行执行');

  return `## 📋 执行计划

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
⚠️ **注意**: 各 Worker 将直接修改文件。

**确认执行此计划？**`;
}

/**
 * 构建进度更新消息
 */
export function buildProgressMessage(
  completedTasks: number,
  totalTasks: number,
  currentWorker?: WorkerType,
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
// 导出
// ============================================================================

export const OrchestratorPrompts = {
  getWorkerDescription,
  buildOrchestratorAnalysisPrompt,
  buildOrchestratorSummaryPrompt,
  formatPlanForUser,
  buildProgressMessage,
};
