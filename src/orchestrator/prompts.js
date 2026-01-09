"use strict";
/**
 * Prompt 模板系统
 * 用于 Claude 驱动的智能编排
 * 架构理念：各 CLI 各司其职、独立执行、最后汇总
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.PromptTemplates = void 0;
exports.buildTaskAnalysisPrompt = buildTaskAnalysisPrompt;
exports.buildExecutionPrompt = buildExecutionPrompt;
exports.buildSummaryPrompt = buildSummaryPrompt;
exports.buildCodeReviewPrompt = buildCodeReviewPrompt;
exports.formatPlanForUser = formatPlanForUser;
/**
 * Phase 1: 任务分析 Prompt - 让 Claude 分析任务并输出执行计划
 * 分析完成后需要 Hard Stop，等待用户确认
 */
function buildTaskAnalysisPrompt(userPrompt, availableCLIs, projectContext) {
    return `你是一个智能任务编排器（Orchestrator）。请分析以下用户需求，并制定执行计划。

## 用户需求
${userPrompt}

## 可用的 CLI 工具
${availableCLIs.map(cli => `- ${cli}: ${getCliDescription(cli)}`).join('\n')}

${projectContext ? `## 项目上下文\n${projectContext}\n` : ''}

## 分析任务
请分析用户需求，判断：
1. 这个任务是否需要多 CLI 协作？
2. 如果需要协作，应该如何分配任务？
3. 任务之间是否有依赖关系（需要串行）还是可以并行？
4. 是否为简单任务（无需多模型协作）？

## 输出格式
请以 JSON 格式输出执行计划：
\`\`\`json
{
  "analysis": "对任务的简要分析",
  "isSimpleTask": true/false,
  "skipReason": "如果是简单任务，说明跳过协作的原因",
  "needsCollaboration": true/false,
  "subTasks": [
    {
      "id": "1",
      "description": "子任务描述",
      "assignedCli": "claude/codex/gemini",
      "reason": "选择该 CLI 的原因",
      "targetFiles": ["预计修改的文件列表"],
      "dependencies": [],
      "prompt": "发送给该 CLI 的具体指令（英文）"
    }
  ],
  "executionMode": "parallel/sequential",
  "summary": "执行计划总结"
}
\`\`\`

## 重要约束
- **前端/UI/样式任务** → 分配给 Gemini
- **后端/逻辑/算法/Bug修复任务** → 分配给 Codex
- **架构设计/复杂分析/文档任务** → 分配给 Claude
- **各 CLI 独立执行**：每个 CLI 直接修改文件，拥有完整写入权限
- **避免文件冲突**：不同 CLI 负责不同文件，有冲突时改为串行执行
- 如果任务简单，设置 isSimpleTask=true，并说明原因`;
}
/**
 * Phase 3: 执行指令 Prompt - 让各 CLI 直接执行修改
 * 架构理念：各 CLI 拥有完整写入权限，直接修改文件
 */
function buildExecutionPrompt(taskDescription, cli, targetFiles) {
    const filesHint = targetFiles && targetFiles.length > 0
        ? `\n\n**Target Files**: ${targetFiles.join(', ')}`
        : '';
    return `${taskDescription}${filesHint}

**EXECUTION MODE**: Direct modification
- You have FULL write permission to modify files directly
- Make the necessary changes to complete the task
- Provide a brief summary of what you changed after completion`;
}
/**
 * Phase 4: 汇总报告 Prompt - 让 Claude 汇总各 CLI 的执行结果
 * 注意：Claude 只汇总结果，不重新执行代码
 */
function buildSummaryPrompt(originalPrompt, executionResults) {
    const resultsText = executionResults
        .map(r => `### ${r.cli} 执行结果 (${r.success ? '✅ 成功' : '❌ 失败'})
**任务**: ${r.task}
**输出**:
${r.result}
`)
        .join('\n');
    return `请根据以下执行结果，为用户生成一份简洁的总结报告。

## 原始需求
${originalPrompt}

## 各 CLI 执行结果
${resultsText}

## 要求
1. 总结完成了哪些工作
2. 如果有失败的任务，说明原因
3. 列出修改的文件（如果有）
4. 给出后续建议（如果需要）

请用简洁清晰的中文回复。`;
}
/**
 * Code Review Prompt - 可选的代码审查功能
 */
function buildCodeReviewPrompt(originalPrompt, changedFiles, diff) {
    const filesText = changedFiles
        .map(f => `### ${f.path}\n\`\`\`\n${f.content.substring(0, 2000)}${f.content.length > 2000 ? '\n... (truncated)' : ''}\n\`\`\``)
        .join('\n\n');
    return `Please perform a thorough code review of the following changes.

## Original Requirement
${originalPrompt}

## Changes Made (Unified Diff)
\`\`\`diff
${diff}
\`\`\`

## Changed Files
${filesText}

## Review Focus
1. **Logic Errors**: Check for bugs, edge cases, and logical flaws
2. **Security Issues**: Identify potential security vulnerabilities
3. **Performance**: Spot performance bottlenecks or inefficiencies
4. **Best Practices**: Verify adherence to coding standards

## Output Format
Please provide your review in JSON format:
\`\`\`json
{
  "overallScore": 1-10,
  "issues": [
    {
      "severity": "critical/major/minor/suggestion",
      "file": "path/to/file",
      "line": 123,
      "description": "Issue description",
      "suggestion": "How to fix"
    }
  ],
  "summary": "Overall assessment"
}
\`\`\`

OUTPUT: Review comments ONLY. Do NOT make any actual modifications.`;
}
/**
 * 获取 CLI 描述 - 增强版，包含详细能力说明
 */
function getCliDescription(cli) {
    const descriptions = {
        claude: `主控模型/架构师
      - 擅长: 复杂推理、架构设计、长上下文理解、代码审查、多步骤任务分解
      - 最适合: 架构设计、重构规划、复杂问题分析、文档编写、代码审查
      - 特点: 推理能力强，适合需要深度思考的任务`,
        codex: `后端专家/修复专家
      - 擅长: 快速代码生成、Bug 修复、代码补全、测试编写、算法实现
      - 最适合: 简单 Bug 修复、功能实现、单元测试、代码调试
      - 特点: 执行速度快，适合明确的编码任务`,
        gemini: `前端专家/多模态专家
      - 擅长: 多模态理解、前端 UI/UX、CSS 样式、React/Vue 组件、创意任务
      - 最适合: 前端开发、UI 组件、样式优化、图片理解、创意设计
      - 特点: 多模态能力强，适合视觉相关任务`,
    };
    return descriptions[cli] || '通用编程助手';
}
/**
 * 格式化执行计划为用户可读的文本（用于 Hard Stop 展示）
 */
function formatPlanForUser(plan) {
    if (plan.isSimpleTask) {
        return `## 📋 任务分析结果

**分析**: ${plan.analysis}

⚠️ **这是一个简单任务，无需多模型协作。**
原因: ${plan.skipReason || '任务复杂度较低'}

---
**是否同意跳过多模型协作，直接执行？**
请回复 **Y** 继续执行，或 **N** 强制使用多模型协作。`;
    }
    const tasksText = plan.subTasks
        .map((t, i) => {
        const filesInfo = t.targetFiles && t.targetFiles.length > 0
            ? `\n   - 目标文件: ${t.targetFiles.join(', ')}`
            : '';
        return `${i + 1}. **${t.description}**
   - 分配给: \`${t.assignedCli}\` (直接执行，有写入权限)
   - 原因: ${t.reason}${filesInfo}`;
    })
        .join('\n');
    return `## 📋 执行计划

**分析**: ${plan.analysis}

### 子任务列表
${tasksText}

**执行模式**: ${plan.executionMode === 'parallel' ? '⚡ 并行执行（各 CLI 同时工作）' : '🔗 串行执行（按顺序依次执行）'}

**总结**: ${plan.summary}

---
⚠️ **注意**: 各 CLI 将直接修改文件，拥有完整写入权限。

**Shall I proceed with this plan? (Y/N)**
请回复 **Y** 确认执行，或 **N** 取消/修改计划。`;
}
exports.PromptTemplates = {
    buildTaskAnalysisPrompt,
    buildExecutionPrompt,
    buildSummaryPrompt,
    buildCodeReviewPrompt,
    formatPlanForUser,
};
//# sourceMappingURL=prompts.js.map