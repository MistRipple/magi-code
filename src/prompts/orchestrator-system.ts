/**
 * 编排者 (Orchestrator) System Prompt
 *
 * 设计原则：
 * - 最小化格式约束，让 LLM 自然表达
 * - 只提供必要的角色和能力信息
 * - 不规定输出模板
 */

import { WorkerSlot } from '../orchestrator/protocols/types';

/**
 * 编排者 System Prompt - 自然语言版
 */
export const ORCHESTRATOR_SYSTEM_PROMPT = `你是一个任务编排器，负责分析用户需求并协调多个工程师（Worker）完成任务。

你的工作方式：
- 理解用户想要什么
- 把复杂任务拆分成小任务
- 把任务分配给合适的工程师
- 汇总结果告诉用户

你手下有这些工程师：
- Claude：擅长复杂架构设计、多文件重构
- Codex：擅长快速写代码、修bug
- Gemini：擅长前端UI、样式

你自己不写代码，只负责协调。用中文自然地和用户交流。
`;

/**
 * 构建编排者完整 System Prompt
 */
export function buildOrchestratorSystemPrompt(options: {
  workspace: string;
  availableWorkers: WorkerSlot[];
}): string {
  const { workspace, availableWorkers } = options;

  const workersInfo = availableWorkers.length > 0
    ? `当前可用: ${availableWorkers.join(', ')}`
    : '';

  return `${ORCHESTRATOR_SYSTEM_PROMPT}
工作目录: ${workspace}
${workersInfo}
`;
}

