import type { DiagramPayload } from './diagram-payload';

export interface DiagramPosition {
  x: number;
  y: number;
}

export interface NormalizedDiagramNode {
  id: string;
  label: string;
  type?: string;
  position?: DiagramPosition;
  data: Record<string, unknown>;
}

export interface NormalizedDiagramEdge {
  id: string;
  source: string;
  target: string;
  label?: string;
  type?: string;
  data: Record<string, unknown>;
}

export interface NormalizedDiagramGraph {
  nodes: NormalizedDiagramNode[];
  edges: NormalizedDiagramEdge[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function readString(record: Record<string, unknown>, keys: string[]): string | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'string' && value.trim()) {
      return value.trim();
    }
    if (typeof value === 'number' && Number.isFinite(value)) {
      return String(value);
    }
  }
  return undefined;
}

function readNumber(record: Record<string, unknown>, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'number' && Number.isFinite(value)) {
      return value;
    }
    if (typeof value === 'string' && value.trim() && Number.isFinite(Number(value))) {
      return Number(value);
    }
  }
  return undefined;
}

function readNestedRecord(record: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const value = record[key];
  return isRecord(value) ? value : undefined;
}

function normalizeNode(value: unknown, index: number, usedIds: Set<string>): NormalizedDiagramNode | null {
  if (typeof value === 'string' || typeof value === 'number') {
    const base = String(value).trim();
    if (!base) return null;
    const id = uniqueId(base, usedIds);
    return { id, label: base, data: {} };
  }

  if (!isRecord(value)) return null;
  const data = readNestedRecord(value, 'data') ?? {};
  const idSource = readString(value, ['id', 'key', 'name'])
    ?? readString(data, ['id', 'key', 'name'])
    ?? `node-${index + 1}`;
  const id = uniqueId(idSource, usedIds);
  const label = readString(value, ['label', 'title', 'name'])
    ?? readString(data, ['label', 'title', 'name'])
    ?? id;
  const positionRecord = readNestedRecord(value, 'position');
  const x = positionRecord
    ? readNumber(positionRecord, ['x'])
    : readNumber(value, ['x']);
  const y = positionRecord
    ? readNumber(positionRecord, ['y'])
    : readNumber(value, ['y']);
  const position = x === undefined || y === undefined ? undefined : { x, y };
  const type = readString(value, ['type']) ?? readString(data, ['type']);

  return {
    id,
    label,
    type,
    position,
    data: {
      ...data,
      label,
      raw: value,
    },
  };
}

function normalizeEdge(value: unknown, index: number, nodeIds: Set<string>): NormalizedDiagramEdge | null {
  if (!isRecord(value)) return null;
  const data = readNestedRecord(value, 'data') ?? {};
  const source = readString(value, ['source', 'from', 'tail'])
    ?? readString(data, ['source', 'from', 'tail']);
  const target = readString(value, ['target', 'to', 'head'])
    ?? readString(data, ['target', 'to', 'head']);
  if (!source || !target || !nodeIds.has(source) || !nodeIds.has(target)) {
    return null;
  }

  const label = readString(value, ['label', 'title', 'name'])
    ?? readString(data, ['label', 'title', 'name']);
  const id = readString(value, ['id', 'key'])
    ?? readString(data, ['id', 'key'])
    ?? `${source}-${target}-${index + 1}`;
  const type = readString(value, ['type']) ?? readString(data, ['type']);

  return {
    id,
    source,
    target,
    label,
    type,
    data: {
      ...data,
      label,
      raw: value,
    },
  };
}

function uniqueId(base: string, usedIds: Set<string>): string {
  const normalized = base.trim() || `node-${usedIds.size + 1}`;
  if (!usedIds.has(normalized)) {
    usedIds.add(normalized);
    return normalized;
  }

  let suffix = 2;
  while (usedIds.has(`${normalized}-${suffix}`)) {
    suffix += 1;
  }
  const resolved = `${normalized}-${suffix}`;
  usedIds.add(resolved);
  return resolved;
}

export function normalizeDiagramGraph(graph: DiagramPayload['graph']): NormalizedDiagramGraph {
  const nodeValues = Array.isArray(graph?.nodes) ? graph.nodes : [];
  const edgeValues = Array.isArray(graph?.edges) ? graph.edges : [];
  const usedIds = new Set<string>();
  const nodes = nodeValues
    .map((node, index) => normalizeNode(node, index, usedIds))
    .filter((node): node is NormalizedDiagramNode => !!node);
  const nodeIds = new Set(nodes.map((node) => node.id));
  const edges = edgeValues
    .map((edge, index) => normalizeEdge(edge, index, nodeIds))
    .filter((edge): edge is NormalizedDiagramEdge => !!edge);

  return { nodes, edges };
}
