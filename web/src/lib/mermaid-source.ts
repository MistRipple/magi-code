const FLOWCHART_DECLARATION = /^\s*(?:flowchart|graph)\s+/i;
const FLOWCHART_CONTROL_LINE = /^\s*(?:%%|---|end\b|subgraph\b|classDef\b|class\b|style\b|linkStyle\b|click\b|accTitle\b|accDescr\b)/i;
const FLOWCHART_RISKY_LABEL_CHARS = /[()/:：,，、]/u;

export function normalizeMermaidSource(source: string): string {
  let normalized = source.trim();
  if (!normalized) return '';
  let previous = '';
  while (previous !== normalized) {
    previous = normalized;
    normalized = normalized.replace(/([A-Za-z0-9_\]\})\u4e00-\u9fff])\s*[.。．]\s*$/u, '$1');
  }
  return normalized;
}

export function mermaidRenderSourceCandidates(source: string): string[] {
  const normalized = normalizeMermaidSource(source);
  if (!normalized) return [''];
  const repaired = repairFlowchartRiskyLabels(normalized);
  return repaired === normalized ? [normalized] : [normalized, repaired];
}

export function repairFlowchartRiskyLabels(source: string): string {
  if (!FLOWCHART_DECLARATION.test(source)) {
    return source;
  }
  return source
    .split('\n')
    .map((line) => FLOWCHART_CONTROL_LINE.test(line) ? line : repairFlowchartLineLabels(line))
    .join('\n');
}

function repairFlowchartLineLabels(line: string): string {
  return repairDelimitedLabels(
    repairDelimitedLabels(line, '[', ']', '["', '"]'),
    '{',
    '}',
    '{"',
    '"}',
  );
}

function repairDelimitedLabels(
  line: string,
  open: '[' | '{',
  close: ']' | '}',
  quotedOpen: string,
  quotedClose: string,
): string {
  let result = '';
  let cursor = 0;
  while (cursor < line.length) {
    const openIndex = line.indexOf(open, cursor);
    if (openIndex < 0) {
      result += line.slice(cursor);
      break;
    }
    const closeIndex = line.indexOf(close, openIndex + 1);
    if (closeIndex < 0) {
      result += line.slice(cursor);
      break;
    }
    const label = line.slice(openIndex + 1, closeIndex);
    if (shouldQuoteFlowchartLabel(line, openIndex, label)) {
      result += line.slice(cursor, openIndex);
      result += `${quotedOpen}${escapeMermaidLabel(label.trim())}${quotedClose}`;
      cursor = closeIndex + 1;
      continue;
    }
    result += line.slice(cursor, closeIndex + 1);
    cursor = closeIndex + 1;
  }
  return result;
}

function shouldQuoteFlowchartLabel(line: string, openIndex: number, label: string): boolean {
  const trimmed = label.trim();
  if (!trimmed || trimmed.startsWith('"') || trimmed.startsWith("'")) {
    return false;
  }
  if (!FLOWCHART_RISKY_LABEL_CHARS.test(trimmed)) {
    return false;
  }
  const prefix = line.slice(0, openIndex);
  return /(?:^|[\s;|])[\w.-]+$/.test(prefix);
}

function escapeMermaidLabel(label: string): string {
  return label.replace(/"/g, '#quot;');
}
