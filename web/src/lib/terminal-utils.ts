import type { TerminalOperation } from '../types/message';

/** 需要渲染为终端会话卡片的工具集合 */
export const TERMINAL_TOOLS = new Set<string>(['shell_exec']);
const TERMINAL_TOOL_ALIASES: Record<string, TerminalOperation> = {
  shell_exec: 'shell_exec',
};

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
  const normalized = name?.trim();
  if (!normalized) return 'shell_exec';
  return TERMINAL_TOOL_ALIASES[normalized] ?? normalized;
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

function readTerminalString(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function joinTerminalStreams(stdout: unknown, stderr: unknown): string {
  const stdoutText = readTerminalString(stdout);
  const stderrText = readTerminalString(stderr);
  if (!stdoutText) return stderrText;
  if (!stderrText) return stdoutText;
  return stdoutText.endsWith('\n') ? `${stdoutText}${stderrText}` : `${stdoutText}\n${stderrText}`;
}

export function resolveTerminalArgumentId(argumentsValue?: Record<string, unknown>): number | undefined {
  const terminalId = argumentsValue?.terminal_id;
  if (!Number.isInteger(terminalId) || (terminalId as number) <= 0) {
    return undefined;
  }

  const action = readTerminalString(argumentsValue?.action).trim().toLowerCase();
  const command = readTerminalString(argumentsValue?.command).trim();
  if (action === 'run' || (!action && command)) {
    return undefined;
  }
  return terminalId as number;
}

export function terminalPayloadOutput(payload?: Record<string, unknown> | null): string {
  if (!payload) return '';
  return (
    readTerminalString(payload.output)
    || readTerminalString(payload.final_output)
    || joinTerminalStreams(payload.stdout, payload.stderr)
  );
}

export function terminalPayloadErrorText(payload?: Record<string, unknown> | null): string {
  if (!payload) return '';
  return (
    readTerminalString(payload.error).trim()
    || readTerminalString(payload.summary).trim()
    || readTerminalString(payload.message).trim()
    || readTerminalString(payload.stderr).trim()
  );
}
