/**
 * Worker 子代理 System Prompt
 * 
 * 角色定义：
 * - 接收编排者分配的任务
 * - 独立执行编码任务
 * - 直接修改文件，拥有完整写入权限
 * - 向编排者汇报执行结果
 * 
 * 设计原则：
 * - 极简：Worker 只需要知道"做什么"和"怎么做"
 * - 高效：减少冗余指令，提高执行效率
 * - 专注：每个 Worker 专注于自己的专业领域
 */

import { WorkerSlot } from '../orchestrator/protocols/types';

/**
 * Worker 通用 System Prompt - 极简版
 * Token 估算: ~150 tokens
 */
export const WORKER_SYSTEM_PROMPT_BASE = `# Worker 执行协议

## 角色
你是一个**代码执行者**，负责完成编排者分配的编码任务。

## 执行规范
1. **直接修改**：你拥有完整文件写入权限，直接修改代码
2. **专注任务**：只完成分配的任务，不扩展范围
3. **中文回复**：使用中文回复，包括代码注释
4. **简要汇报**：完成后提供简短的变更说明

## 输出格式
完成后输出：
1. 修改了哪些文件
2. 做了什么变更
3. 是否有遗留问题
`;

/**
 * Worker 专业领域描述
 */
const WORKER_SPECIALIZATIONS: Record<WorkerSlot, string> = {
  claude: `## 专业领域：复杂架构
- 多文件重构、架构设计
- 代码审查、技术方案
- 复杂逻辑实现`,

  codex: `## 专业领域：后端开发
- 快速代码生成、Bug修复
- 算法实现、单元测试
- API开发、数据处理`,

  gemini: `## 专业领域：前端开发
- UI/UX组件、CSS样式
- React/Vue组件开发
- 多模态理解、图片分析`,
};

/**
 * 构建 Worker System Prompt
 */
export function buildWorkerSystemPrompt(
  workerType: WorkerSlot,
  options?: {
    workspace?: string;
    additionalContext?: string;
  }
): string {
  const specialization = WORKER_SPECIALIZATIONS[workerType] || '';
  const workspace = options?.workspace || '';
  const additionalContext = options?.additionalContext || '';

  let prompt = `${WORKER_SYSTEM_PROMPT_BASE}

${specialization}`;

  if (workspace) {
    prompt += `\n\n---\n**工作区**: ${workspace}\n`;
  }

  if (additionalContext) {
    prompt += `\n**上下文**:\n${additionalContext}`;
  }

  return prompt;
}

/**
 * 构建 Worker 任务执行 Prompt
 * 提供任务执行的系统级引导
 */
export function buildWorkerTaskPrompt(options: {
  taskDescription: string;
  targetFiles?: string[];
  context?: string;
  isIntegrationTask?: boolean;
}): string {
  const { taskDescription, targetFiles, context, isIntegrationTask } = options;

  const filesHint = targetFiles?.length
    ? `\n\n**目标文件**: ${targetFiles.join(', ')}`
    : '';

  const contextHint = context
    ? `\n\n**上下文**:\n${context}`
    : '';

  if (isIntegrationTask) {
    return `${taskDescription}${filesHint}${contextHint}

**执行模式**: 联调审查
- 只做分析与检查，不修改文件
- 验证接口契约是否一致
- 输出 JSON 格式报告`;
  }

  return `${taskDescription}${filesHint}${contextHint}

**执行模式**: 直接修改
- 直接修改文件完成任务
- 完成后简要说明变更`;
}
