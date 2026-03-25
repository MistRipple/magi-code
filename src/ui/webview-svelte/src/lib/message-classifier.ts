const WORKER_SLOTS = new Set(['claude', 'codex', 'gemini']);

export function normalizeWorkerSlot(value: unknown): 'claude' | 'codex' | 'gemini' | null {
  if (!value || typeof value !== 'string') return null;
  const lower = value.toLowerCase().trim();
  if (WORKER_SLOTS.has(lower)) return lower as 'claude' | 'codex' | 'gemini';
  return null;
}
