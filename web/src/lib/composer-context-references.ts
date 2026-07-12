export type ComposerContextReferenceKind = 'file' | 'directory';

export interface ComposerContextReferenceInput {
  kind: ComposerContextReferenceKind;
  path: string;
  name: string;
}

export interface ComposerContextReference extends ComposerContextReferenceInput {
  id: string;
}

export const MAX_COMPOSER_CONTEXT_REFERENCES = 20;

function normalizePath(path: string): string {
  const trimmed = path.trim();
  if (trimmed === '/') return trimmed;
  return trimmed.replace(/[\\/]+$/u, '');
}

export function addComposerContextReference(
  references: ComposerContextReference[],
  input: ComposerContextReferenceInput,
): ComposerContextReference[] {
  const path = normalizePath(input.path);
  if (!path) return references;
  const id = `${input.kind}:${path}`;
  if (references.some((reference) => reference.id === id)) return references;
  if (references.length >= MAX_COMPOSER_CONTEXT_REFERENCES) return references;
  const name = input.name.trim() || path.split(/[\\/]/u).filter(Boolean).pop() || path;
  return [...references, { id, kind: input.kind, path, name }];
}

export function toSessionContextReferencePayload(
  references: ComposerContextReference[],
): ComposerContextReferenceInput[] {
  return references.map(({ kind, path, name }) => ({ kind, path, name }));
}
