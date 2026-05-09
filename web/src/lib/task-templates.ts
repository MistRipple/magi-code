import { i18n } from '../stores/i18n.svelte';

export interface TaskTemplate {
  id: string;
  category: 'understand' | 'fix' | 'change' | 'review' | 'document';
}

const TEMPLATES: TaskTemplate[] = [
  { id: 'explain', category: 'understand' },
  { id: 'trace', category: 'understand' },
  { id: 'debug', category: 'fix' },
  { id: 'refactor', category: 'change' },
  { id: 'tests', category: 'change' },
  { id: 'review', category: 'review' },
  { id: 'docs', category: 'document' },
  { id: 'adr', category: 'document' },
];

export interface ResolvedTemplate {
  id: string;
  category: TaskTemplate['category'];
  label: string;
  description: string;
  prompt: string;
}

export function listTaskTemplates(): ResolvedTemplate[] {
  return TEMPLATES.map((tpl) => ({
    id: tpl.id,
    category: tpl.category,
    label: i18n.t(`taskTemplates.${tpl.id}.label`),
    description: i18n.t(`taskTemplates.${tpl.id}.desc`),
    prompt: i18n.t(`taskTemplates.${tpl.id}.prompt`),
  }));
}

export function categoryLabel(category: TaskTemplate['category']): string {
  return i18n.t(`taskTemplates.category.${category}`);
}
