import type { TaskDto } from '../shared/rust-backend-types';

export interface AgentTerminalOutput {
  text: string;
  sourceRefIndex: number;
  truncated: boolean;
}

const TERMINAL_OUTPUT_MAX_CHARS = 8000;

function normalizeText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function truncateTerminalText(text: string): { text: string; truncated: boolean } {
  const normalized = text.trim();
  if (normalized.length <= TERMINAL_OUTPUT_MAX_CHARS) {
    return { text: normalized, truncated: false };
  }
  return {
    text: `${normalized.slice(0, TERMINAL_OUTPUT_MAX_CHARS).trimEnd()}...`,
    truncated: true,
  };
}

function textFromOutputBlock(block: unknown): string {
  if (!block || typeof block !== 'object' || Array.isArray(block)) {
    return '';
  }
  const record = block as Record<string, unknown>;
  if (normalizeText(record.type) !== 'text') {
    return '';
  }
  return normalizeText(record.content);
}

function textFromStructuredOutputRef(value: unknown): string {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return '';
  }
  const record = value as Record<string, unknown>;
  const blocks = Array.isArray(record.blocks) ? record.blocks : [];
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    const text = textFromOutputBlock(blocks[index]);
    if (text) {
      return text;
    }
  }
  const result = record.result && typeof record.result === 'object' && !Array.isArray(record.result)
    ? record.result as Record<string, unknown>
    : null;
  return normalizeText(result?.final_text ?? result?.finalText);
}

function parseOutputRef(ref: string): string {
  const normalized = ref.trim();
  if (!normalized) {
    return '';
  }
  if (!normalized.startsWith('{') && !normalized.startsWith('[')) {
    return normalized;
  }
  try {
    const parsed = JSON.parse(normalized) as unknown;
    return textFromStructuredOutputRef(parsed);
  } catch {
    return normalized;
  }
}

export function agentTerminalOutput(task: TaskDto | null | undefined): AgentTerminalOutput | null {
  if (!task || !Array.isArray(task.output_refs) || task.output_refs.length === 0) {
    return null;
  }
  for (let index = task.output_refs.length - 1; index >= 0; index -= 1) {
    const ref = task.output_refs[index];
    if (typeof ref !== 'string') {
      continue;
    }
    const text = parseOutputRef(ref);
    if (!text) {
      continue;
    }
    const truncated = truncateTerminalText(text);
    return {
      text: truncated.text,
      sourceRefIndex: index,
      truncated: truncated.truncated,
    };
  }
  return null;
}
