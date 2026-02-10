/**
 * Prompts 模块 - 统一导出
 * 
 * 架构概览：
 * ┌─────────────────────────────────────────────────────────────┐
 * │                    Magi Prompt 体系                      │
 * ├─────────────────────────────────────────────────────────────┤
 * │  编排者 (Orchestrator)                                       │
 * │  ├── System Prompt: orchestrator-system.ts (~300 tokens)   │
 * │  └── Task Prompts: orchestrator-prompts.ts (动态生成)        │
 * ├─────────────────────────────────────────────────────────────┤
 * │  Worker (子代理)                                             │
 * │  ├── System Prompt: worker-system.ts (~150 tokens)          │
 * │  └── Task Prompts: 动态生成 (buildWorkerTaskPrompt)          │
 * └─────────────────────────────────────────────────────────────┘
 */

// 编排者 Prompt
export {
  ORCHESTRATOR_SYSTEM_PROMPT,
  buildOrchestratorSystemPrompt,
} from './orchestrator-system';

// Worker Prompt
export {
  WORKER_SYSTEM_PROMPT_BASE,
  buildWorkerSystemPrompt,
  buildWorkerTaskPrompt,
} from './worker-system';

/**
 * Prompt Token 估算
 */
export const PROMPT_TOKEN_ESTIMATES = {
  orchestratorSystem: 300,
  workerSystem: 150,
} as const;

/**
 * 快速获取 System Prompt
 */
export function getSystemPrompt(
  role: 'orchestrator' | 'worker',
  options: {
    workspace: string;
    workerType?: 'claude' | 'codex' | 'gemini';
    availableWorkers?: ('claude' | 'codex' | 'gemini')[];
  }
): string {
  if (role === 'orchestrator') {
    const { buildOrchestratorSystemPrompt } = require('./orchestrator-system');
    return buildOrchestratorSystemPrompt({
      workspace: options.workspace,
      availableWorkers: options.availableWorkers || [],
    });
  }

  if (role === 'worker' && options.workerType) {
    const { buildWorkerSystemPrompt } = require('./worker-system');
    return buildWorkerSystemPrompt(options.workerType, {
      workspace: options.workspace,
    });
  }

  throw new Error(`Invalid role or missing workerType: ${role}`);
}
