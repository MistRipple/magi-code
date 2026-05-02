export type DiagramKind = 'mermaid' | 'dot' | 'graph' | 'flow';

export interface DiagramPayload {
  kind: DiagramKind;
  title?: string;
  source?: string;
  graph?: {
    nodes?: unknown[];
    edges?: unknown[];
    [key: string]: unknown;
  };
  layout?: string;
  theme?: string;
  interactive?: boolean;
  diagramType?: string;
  summary?: string;
}

const DIAGRAM_KINDS = new Set<DiagramKind>(['mermaid', 'dot', 'graph', 'flow']);

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeKind(value: unknown): DiagramKind | null {
  if (typeof value !== 'string') return null;
  const kind = value.trim().toLowerCase();
  return DIAGRAM_KINDS.has(kind as DiagramKind) ? (kind as DiagramKind) : null;
}

function parseJsonObject(value: unknown): Record<string, unknown> | null {
  if (isRecord(value)) return value;
  if (typeof value !== 'string') return null;
  try {
    const parsed = JSON.parse(value);
    return isRecord(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function isMermaidMindmapSource(source: unknown): boolean {
  return typeof source === 'string' && /^\s*mindmap(?:\s|\n|$)/i.test(source);
}

function normalizeGraph(value: unknown): DiagramPayload['graph'] | undefined {
  if (!isRecord(value)) return undefined;
  const elements = value.elements;
  if (isRecord(elements)) {
    const nodes = Array.isArray(elements.nodes) ? elements.nodes : undefined;
    const edges = Array.isArray(elements.edges) ? elements.edges : undefined;
    if (nodes && edges) return { ...value, nodes, edges };
  }
  if (Array.isArray(elements)) {
    const nodes = elements.filter((item) => {
      if (!isRecord(item)) return false;
      if (item.group === 'nodes') return true;
      const data = isRecord(item.data) ? item.data : undefined;
      return !data || !('source' in data && 'target' in data);
    });
    const edges = elements.filter((item) => {
      if (!isRecord(item)) return false;
      if (item.group === 'edges') return true;
      const data = isRecord(item.data) ? item.data : undefined;
      return !!data && 'source' in data && 'target' in data;
    });
    if (nodes.length || edges.length) return { ...value, nodes, edges };
  }

  const nodes = Array.isArray(value.nodes) ? value.nodes : undefined;
  const edges = Array.isArray(value.edges) ? value.edges : undefined;
  if (!nodes || !edges) return undefined;
  return { ...value, nodes, edges };
}

export function parseToolDiagramPayload(toolName: string, output: unknown): DiagramPayload | null {
  const data = parseJsonObject(output);
  if (!data) return null;

  if (toolName === 'diagram_render' || data.type === 'diagram_render') {
    const kind = normalizeKind(data.kind);
    if (!kind) return null;
    const source = typeof data.source === 'string' ? data.source : undefined;
    const diagramType = typeof data.diagram_type === 'string'
      ? data.diagram_type
      : typeof data.diagramType === 'string'
        ? data.diagramType
        : undefined;
    if (
      kind === 'mermaid'
      && (diagramType?.toLowerCase() === 'mindmap' || isMermaidMindmapSource(source))
    ) {
      return null;
    }
    return {
      kind,
      title: typeof data.title === 'string' ? data.title : undefined,
      source,
      graph: normalizeGraph(data.graph),
      layout: typeof data.layout === 'string' ? data.layout : undefined,
      theme: typeof data.theme === 'string' ? data.theme : undefined,
      interactive: typeof data.interactive === 'boolean' ? data.interactive : undefined,
      diagramType,
      summary: typeof data.summary === 'string' ? data.summary : undefined,
    };
  }

  return null;
}

export function parseCodeBlockDiagramPayload(language: string, code: string): DiagramPayload | null {
  const normalizedLanguage = language.trim().toLowerCase();
  if (normalizedLanguage === 'mermaid') {
    if (isMermaidMindmapSource(code)) return null;
    return { kind: 'mermaid', source: code };
  }
  if (normalizedLanguage === 'dot' || normalizedLanguage === 'graphviz') {
    return { kind: 'dot', source: code };
  }
  if (normalizedLanguage === 'cytoscape' || normalizedLanguage === 'cyjs') {
    const graph = normalizeGraph(parseJsonObject(code));
    return graph ? { kind: 'graph', graph } : null;
  }
  if (normalizedLanguage === 'svelte-flow' || normalizedLanguage === 'svelteflow') {
    const graph = normalizeGraph(parseJsonObject(code));
    return graph ? { kind: 'flow', graph } : null;
  }
  return null;
}

export function diagramSummary(payload: DiagramPayload): string {
  if (payload.title?.trim()) {
    return payload.title.trim();
  }
  return payload.summary?.trim() || '';
}
