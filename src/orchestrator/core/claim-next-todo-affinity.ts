import type { UnifiedTodo } from '../../todo';

export type ClaimAffinityLevel =
  | 'same_assignment'
  | 'shared_target_files'
  | 'none';

export interface ClaimNextTodoAffinityContext {
  currentAssignmentId: string;
  currentTodoId: string;
  currentTargetFiles?: string[];
}

export interface ClaimNextTodoAffinity {
  level: ClaimAffinityLevel;
  score: number;
  reason: string;
}

export interface ClaimNextTodoSelection {
  selected: UnifiedTodo | null;
  affinity: ClaimNextTodoAffinity;
}

// 产品约束：todo_claim_next 只用于保持当前执行上下文连续性，
// 不是 Worker 在 Mission 内跨 Assignment 自由找活的入口。
// 当候选缺少足够亲和度时，必须 fail-closed 并交回编排层重新调度。
function normalizePaths(paths: string[] | undefined): string[] {
  if (!Array.isArray(paths)) {
    return [];
  }
  return paths
    .filter((item): item is string => typeof item === 'string')
    .map((item) => item.trim())
    .filter(Boolean);
}

function countSharedTargetFiles(a: string[] | undefined, b: string[] | undefined): number {
  const left = normalizePaths(a);
  const right = new Set(normalizePaths(b));
  if (left.length === 0 || right.size === 0) {
    return 0;
  }
  let shared = 0;
  for (const filePath of left) {
    if (right.has(filePath)) {
      shared += 1;
    }
  }
  return shared;
}

export function evaluateClaimNextTodoAffinity(
  candidate: UnifiedTodo,
  context: ClaimNextTodoAffinityContext,
): ClaimNextTodoAffinity {
  if (candidate.id === context.currentTodoId) {
    return {
      level: 'none',
      score: 0,
      reason: '候选 Todo 与当前 Todo 相同，不应重复认领',
    };
  }

  if (candidate.assignmentId === context.currentAssignmentId) {
    return {
      level: 'same_assignment',
      score: 300,
      reason: '候选 Todo 属于当前 Assignment，可保持执行上下文连续性',
    };
  }

  const sharedTargetFiles = countSharedTargetFiles(candidate.targetFiles, context.currentTargetFiles);
  if (sharedTargetFiles > 0) {
    return {
      level: 'shared_target_files',
      score: 200 + Math.min(sharedTargetFiles, 5),
      reason: `候选 Todo 与当前上下文共享 ${sharedTargetFiles} 个目标文件，可复用代码上下文`,
    };
  }

  return {
    level: 'none',
    score: 0,
    reason: '候选 Todo 与当前上下文缺少 Assignment 或目标文件亲和度，不应自动续领',
  };
}

export function selectClaimNextTodoCandidate(
  candidates: UnifiedTodo[],
  context: ClaimNextTodoAffinityContext,
): ClaimNextTodoSelection {
  let selected: UnifiedTodo | null = null;
  let selectedAffinity: ClaimNextTodoAffinity = {
    level: 'none',
    score: 0,
    reason: '没有找到满足上下文亲和度约束的候选 Todo',
  };

  for (const candidate of candidates) {
    const affinity = evaluateClaimNextTodoAffinity(candidate, context);
    if (affinity.score <= 0) {
      continue;
    }
    if (
      !selected
      || affinity.score > selectedAffinity.score
      || (affinity.score === selectedAffinity.score && candidate.priority < selected.priority)
    ) {
      selected = candidate;
      selectedAffinity = affinity;
    }
  }

  return {
    selected,
    affinity: selectedAffinity,
  };
}
