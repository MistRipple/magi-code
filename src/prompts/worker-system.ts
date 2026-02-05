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
 * - 思考优先：展示完整的推理过程（Chain of Thought）
 * - 解释决策：说明为什么这样做
 * - 专注任务：每个 Worker 专注于自己的专业领域
 * - 自然对话：像工程师之间的技术讨论
 */

import { WorkerSlot } from '../orchestrator/protocols/types';

/**
 * Worker 通用 System Prompt
 *
 * 设计原则：
 * - 不规定输出格式，让 LLM 自然表达
 * - 强调思考过程，但不限制思考方式
 * - 像真正的工程师一样工作
 */
export const WORKER_SYSTEM_PROMPT_BASE = `你是一个经验丰富的代码工程师。

像你平时工作一样完成任务：
- 先理解要做什么，想清楚再动手
- 遇到问题就分析，找到原因再解决
- 改代码时知道为什么这样改

你有完整的文件读写权限，直接修改代码完成任务。
用中文自然地表达你的思考和行动。
`;

/**
 * Worker 专业领域提示（简洁版，不强制格式）
 */
const WORKER_SPECIALIZATIONS: Record<WorkerSlot, string> = {
  claude: `你擅长复杂架构设计、多文件重构和代码审查。`,
  codex: `你擅长快速代码生成、Bug修复和算法实现。`,
  gemini: `你擅长前端UI开发、CSS样式和多模态分析。`,
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
    prompt += `\n工作目录: ${workspace}`;
  }

  if (additionalContext) {
    prompt += `\n${additionalContext}`;
  }

  return prompt;
}

/**
 * 构建 Worker 任务执行 Prompt
 * 只提供必要信息，不规定输出格式
 */
export function buildWorkerTaskPrompt(options: {
  taskDescription: string;
  targetFiles?: string[];
  context?: string;
  isIntegrationTask?: boolean;
}): string {
  const { taskDescription, targetFiles, context, isIntegrationTask } = options;

  let prompt = taskDescription;

  if (targetFiles?.length) {
    prompt += `\n\n相关文件: ${targetFiles.join(', ')}`;
  }

  if (context) {
    prompt += `\n\n背景信息:\n${context}`;
  }

  if (isIntegrationTask) {
    prompt += `\n\n这是一个联调检查任务，只需分析不要修改文件。`;
  }

  return prompt;
}
