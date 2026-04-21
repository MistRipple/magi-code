import type { TerminalOperation } from '../types/message';

/** 需要渲染为终端会话卡片的工具集合 */
export const TERMINAL_TOOLS = new Set(['shell']);

export interface LeadingJsonMatch {
  jsonText: string;
  tailText: string;
}

function extractLeadingJsonFromOffset(text: string, startIndex: number): LeadingJsonMatch | null {
  if (!text || startIndex < 0 || startIndex >= text.length) return null;
  const first = text[startIndex];
  if (first !== '{' && first !== '[') return null;

  const openChar = first;
  const closeChar = first === '{' ? '}' : ']';
  let depth = 0;
  let inString = false;
  let escaping = false;

  for (let i = startIndex; i < text.length; i += 1) {
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
          jsonText: text.slice(startIndex, i + 1),
          tailText: text.slice(i + 1).trim(),
        };
      }
    }
  }

  return null;
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
  const trimmed = text.trim();
  if (!trimmed) return null;

  // 首选严格前导 JSON
  const direct = extractLeadingJsonFromOffset(trimmed, 0);
  if (direct) {
    return direct;
  }

  // 容错：允许前面夹带非 JSON 前缀，自动扫描首个可解析 JSON 片段
  for (let i = 0; i < trimmed.length; i += 1) {
    const ch = trimmed[i];
    if (ch !== '{' && ch !== '[') continue;
    const recovered = extractLeadingJsonFromOffset(trimmed, i);
    if (recovered) {
      return recovered;
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
