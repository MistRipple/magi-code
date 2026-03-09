import type { TerminalOperation } from '../types/message';

/** 需要渲染为终端会话卡片的工具集合 */
export const TERMINAL_TOOLS = new Set(['shell']);

export interface LeadingJsonMatch {
  jsonText: string;
  tailText: string;
}

export function normalizeTerminalToolName(name?: string): string {
  if (!name) return 'shell';
  return name;
}

export function getTerminalToolDisplayName(name?: string): string {
  return normalizeTerminalToolName(name);
}

export function normalizeTerminalOperation(name: string): TerminalOperation | null {
  const normalized = normalizeTerminalToolName(name);
  return TERMINAL_TOOLS.has(normalized) ? normalized as TerminalOperation : null;
}

export function extractLeadingJson(text: string): LeadingJsonMatch | null {
  if (!text) return null;
  const first = text[0];
  if (first !== '{' && first !== '[') return null;

  const openChar = first;
  const closeChar = first === '{' ? '}' : ']';
  let depth = 0;
  let inString = false;
  let escaping = false;

  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];

    if (inString) {
      if (escaping) {
        escaping = false;
        continue;
      }
      if (ch === '\\') {
        escaping = true;
        continue;
      }
      if (ch === '"') {
        inString = false;
      }
      continue;
    }

    if (ch === '"') {
      inString = true;
      continue;
    }

    if (ch === openChar) {
      depth += 1;
      continue;
    }

    if (ch === closeChar) {
      depth -= 1;
      if (depth === 0) {
        return {
          jsonText: text.slice(0, i + 1),
          tailText: text.slice(i + 1).trim(),
        };
      }
    }
  }

  return null;
}

export function parseLeadingJson(content?: string): Record<string, unknown> | unknown[] | null {
  if (!content || typeof content !== 'string') return null;
  const trimmed = content.trim();
  if (!trimmed) return null;
  const leading = extractLeadingJson(trimmed);
  const jsonText = leading?.jsonText || trimmed;
  try {
    return JSON.parse(jsonText) as Record<string, unknown> | unknown[];
  } catch {
    return null;
  }
}

