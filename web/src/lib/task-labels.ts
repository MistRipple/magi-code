import { i18n } from '../stores/i18n.svelte';
import type { TaskDto } from '../shared/rust-backend-types';

type TaskDisplayInput = Pick<TaskDto, 'kind' | 'title' | 'goal' | 'task_id'>;

function compactChineseSpacing(text: string): string {
  return text.replace(/([\u4e00-\u9fff])\s+([\u4e00-\u9fff])/g, '$1$2');
}

function stripInternalTaskIds(text: string): string {
  return text.replace(
    /\b(?:task|mission|worker|agent)-[a-z0-9]+(?:-[a-z0-9]+)*\b/gi,
    i18n.t('taskLabels.related'),
  );
}

function normalizeTaskText(text: string | null | undefined): string {
  return compactChineseSpacing(stripInternalTaskIds((text ?? '').trim()));
}

export function getTaskDisplayTitle(task: TaskDisplayInput): string {
  return normalizeTaskText(task.title) || i18n.t('taskLabels.unnamed');
}

export function getTaskDisplayGoal(task: TaskDisplayInput): string {
  return normalizeTaskText(task.goal);
}

export function getTaskDisplayText(text: string | null | undefined): string {
  return normalizeTaskText(text);
}
