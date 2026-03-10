/**
 * Mission Data Normalizer - Mission 数据规范化
 *
 * 从 WebviewProvider 提取的纯函数（P1-1 修复）。
 * 职责：确保 LLM 返回的 Assignment/Todo 数据有唯一 ID 和正确关联。
 */

import type { Assignment } from './types';
import type { UnifiedTodo } from '../../todo/types';

// ============================================================================
// 辅助函数
// ============================================================================

function generateEntityId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
}

function asArray<T>(value: T[] | undefined | null): T[] {
  return Array.isArray(value) ? value : [];
}

// ============================================================================
// 规范化函数
// ============================================================================

/**
 * 规范化单个 Todo，确保唯一 ID 和 assignmentId 关联
 */
function normalizeTodo(
  rawTodo: UnifiedTodo | undefined | null,
  assignmentId: string,
  seen: Set<string>,
): UnifiedTodo | null {
  if (!rawTodo || typeof rawTodo !== 'object') {
    return null;
  }
  const rawId = typeof rawTodo.id === 'string' ? rawTodo.id.trim() : '';
  let id = rawId || generateEntityId('todo');
  while (seen.has(id)) {
    id = generateEntityId('todo');
  }
  seen.add(id);
  return {
    ...rawTodo,
    id,
    assignmentId: rawTodo.assignmentId || assignmentId,
  };
}

/**
 * 规范化 Todo 列表
 */
export function normalizeTodos(
  rawTodos: UnifiedTodo[] | undefined | null,
  assignmentId: string,
): UnifiedTodo[] {
  const seen = new Set<string>();
  return asArray(rawTodos)
    .map(todo => normalizeTodo(todo, assignmentId, seen))
    .filter((todo): todo is UnifiedTodo => Boolean(todo));
}

/**
 * 规范化 Assignment 列表，确保唯一 ID 并递归规范化内部 Todo
 */
export function normalizeAssignments(
  rawAssignments: Assignment[] | undefined | null,
): Assignment[] {
  const assignments: Assignment[] = [];
  const seen = new Set<string>();
  for (const raw of asArray(rawAssignments)) {
    if (!raw || typeof raw !== 'object') continue;
    const rawId = typeof raw.id === 'string' ? raw.id.trim() : '';
    let id = rawId || generateEntityId('assignment');
    while (seen.has(id)) {
      id = generateEntityId('assignment');
    }
    seen.add(id);
    const todos = normalizeTodos(raw.todos, id);
    assignments.push({
      ...raw,
      id,
      contextNotes: asArray(raw.contextNotes).filter(
        (item): item is string => typeof item === 'string' && item.trim().length > 0
      ),
      constraints: asArray(raw.constraints),
      acceptanceCriteria: asArray(raw.acceptanceCriteria),
      todos,
    });
  }
  return assignments;
}

/**
 * 生成实体 ID（供外部调用方需要单独生成 ID 时使用）
 */
export { generateEntityId };
