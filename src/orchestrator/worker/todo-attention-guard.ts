import type { DecisionHookEvent } from '../../llm/types';

export interface TodoAttentionGuardContext {
  todoContent: string;
  expectedOutput?: string;
  targetPaths?: string[];
  allowSplitTodo?: boolean;
}

const SOFT_REMINDER_THRESHOLD = 4;
const HARD_REMINDER_THRESHOLD = 6;
const BOUNDARY_TOOL_NAMES = new Set(['todo_split', 'todo_claim_next', 'todo_update']);

function clipText(value: string | undefined, fallback: string): string {
  const normalized = typeof value === 'string' ? value.trim().replace(/\s+/g, ' ') : '';
  if (!normalized) {
    return fallback;
  }
  return normalized.length > 160 ? `${normalized.slice(0, 157)}...` : normalized;
}

function buildTargetPathsText(targetPaths: string[] | undefined): string {
  if (!Array.isArray(targetPaths) || targetPaths.length === 0) {
    return '';
  }
  const normalized = targetPaths
    .filter((item): item is string => typeof item === 'string')
    .map((item) => item.trim())
    .filter(Boolean)
    .slice(0, 4);
  if (normalized.length === 0) {
    return '';
  }
  return `优先收敛这些目标文件：${normalized.join('、')}。`;
}

export function createTodoAttentionGuard(context: TodoAttentionGuardContext): (event: DecisionHookEvent) => string[] {
  let staleRounds = 0;
  let reminderLevel = 0;
  const todoText = clipText(context.todoContent, '完成当前 Todo');
  const expectedOutput = clipText(context.expectedOutput, '交付当前 Todo 的明确结果');
  const targetPathsText = buildTargetPathsText(context.targetPaths);

  const buildSoftReminder = (): string => (
    `[System] 当前仍在执行同一 Todo，请先收敛到当前任务：${todoText}。`
    + `预期输出：${expectedOutput}。`
    + `${targetPathsText}`
    + '如果信息已经足够，请开始实现、修改或直接给出结论，不要继续扩展范围。'
  );

  const buildHardReminder = (): string => (
    `[System] 你已连续多轮未实质推进当前 Todo。`
    + `当前 Todo：${todoText}。`
    + `预期输出：${expectedOutput}。`
    + `${targetPathsText}`
    + (context.allowSplitTodo
      ? '如果发现它实际包含多个可独立验证的子目标，请先调用 todo_split 重新收敛；'
      : '')
    + '如果当前 Todo 已完成，请直接输出完成结论；仅在确认完成当前 Todo 后，再进入 todo_claim_next。'
  );

  return (event: DecisionHookEvent): string[] => {
    if (event.type !== 'tool_result') {
      return [];
    }

    const toolNames = Array.isArray(event.toolNames)
      ? event.toolNames
        .filter((item): item is string => typeof item === 'string')
        .map((item) => item.trim())
        .filter(Boolean)
      : [];

    if (toolNames.length === 0) {
      return [];
    }

    const hasBoundaryTool = toolNames.some((name) => BOUNDARY_TOOL_NAMES.has(name));
    if (hasBoundaryTool || event.hadWriteTool) {
      staleRounds = 0;
      reminderLevel = 0;
      return [];
    }

    if (event.allReadOnly || event.noSubstantiveOutput) {
      staleRounds += 1;
    } else {
      staleRounds = 0;
      reminderLevel = 0;
      return [];
    }

    if (staleRounds >= HARD_REMINDER_THRESHOLD && reminderLevel < 2) {
      reminderLevel = 2;
      return [buildHardReminder()];
    }

    if (staleRounds >= SOFT_REMINDER_THRESHOLD && reminderLevel < 1) {
      reminderLevel = 1;
      return [buildSoftReminder()];
    }

    return [];
  };
}
