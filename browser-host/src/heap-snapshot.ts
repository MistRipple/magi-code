import { readFile } from "node:fs/promises";

interface RawHeapSnapshot {
  snapshot: {
    meta: {
      node_fields: string[];
      node_types: unknown[][];
      edge_fields: string[];
      edge_types: unknown[][];
    };
    node_count: number;
    edge_count: number;
  };
  nodes: number[];
  edges: number[];
  strings: string[];
}

interface HeapNode {
  ordinal: number;
  type: string;
  name: string;
  id: number;
  selfSize: number;
  edgeCount: number;
  edgeStart: number;
  detachedness: number;
}

interface HeapEdge {
  type: string;
  name: string | number;
  from: number;
  to: number;
}

interface HeapAggregate {
  id: number;
  type: string;
  name: string;
  count: number;
  self_size: number;
  retained_size: number;
  node_ordinals: number[];
}

interface DominatorData {
  idom: Int32Array;
  retainedSizes: Float64Array;
}

export class HeapSnapshotModel {
  readonly filePath: string;
  readonly nodeCount: number;
  readonly edgeCount: number;
  readonly totalSelfSize: number;
  readonly nodes: HeapNode[];
  readonly #rawEdges: number[];
  readonly #strings: string[];
  readonly #edgeFields: Map<string, number>;
  readonly #edgeTypes: string[];
  readonly #edgeFieldCount: number;
  readonly #nodeFieldCount: number;
  #retainers?: HeapEdge[][];
  #dominators?: DominatorData;
  #aggregates?: HeapAggregate[];

  private constructor(filePath: string, raw: RawHeapSnapshot) {
    this.filePath = filePath;
    const meta = raw.snapshot.meta;
    const nodeFields = new Map(meta.node_fields.map((name, index) => [name, index]));
    const nodeFieldCount = meta.node_fields.length;
    this.#nodeFieldCount = nodeFieldCount;
    const nodeTypes = stringTypeTable(meta.node_types[nodeFields.get("type") ?? 0]);
    this.#edgeFields = new Map(meta.edge_fields.map((name, index) => [name, index]));
    this.#edgeFieldCount = meta.edge_fields.length;
    this.#edgeTypes = stringTypeTable(meta.edge_types[this.#edgeFields.get("type") ?? 0]);
    this.#rawEdges = raw.edges;
    this.#strings = raw.strings;
    this.nodeCount = raw.snapshot.node_count;
    this.edgeCount = raw.snapshot.edge_count;
    this.nodes = [];
    let edgeStart = 0;
    let totalSelfSize = 0;
    for (let ordinal = 0; ordinal < this.nodeCount; ordinal += 1) {
      const offset = ordinal * nodeFieldCount;
      const typeIndex = raw.nodes[offset + requiredField(nodeFields, "type")] ?? 0;
      const nameIndex = raw.nodes[offset + requiredField(nodeFields, "name")] ?? 0;
      const selfSize = raw.nodes[offset + requiredField(nodeFields, "self_size")] ?? 0;
      const edgeCount = raw.nodes[offset + requiredField(nodeFields, "edge_count")] ?? 0;
      const detachedField = nodeFields.get("detachedness");
      this.nodes.push({
        ordinal,
        type: nodeTypes[typeIndex] ?? `type-${typeIndex}`,
        name: raw.strings[nameIndex] ?? "",
        id: raw.nodes[offset + requiredField(nodeFields, "id")] ?? 0,
        selfSize,
        edgeCount,
        edgeStart,
        detachedness: detachedField === undefined ? 0 : raw.nodes[offset + detachedField] ?? 0,
      });
      edgeStart += edgeCount * this.#edgeFieldCount;
      totalSelfSize += selfSize;
    }
    if (edgeStart !== raw.edges.length) {
      throw new Error(`heap snapshot edge table length mismatch: expected ${edgeStart}, actual ${raw.edges.length}`);
    }
    this.totalSelfSize = totalSelfSize;
  }

  static async load(filePath: string): Promise<HeapSnapshotModel> {
    const encoded = await readFile(filePath, "utf8");
    const raw = JSON.parse(encoded) as RawHeapSnapshot;
    validateRawSnapshot(raw);
    return new HeapSnapshotModel(filePath, raw);
  }

  summary(): Record<string, unknown> {
    const types = new Map<string, { count: number; self_size: number }>();
    for (const node of this.nodes) {
      const value = types.get(node.type) ?? { count: 0, self_size: 0 };
      value.count += 1;
      value.self_size += node.selfSize;
      types.set(node.type, value);
    }
    return {
      file_path: this.filePath,
      node_count: this.nodeCount,
      edge_count: this.edgeCount,
      total_self_size: this.totalSelfSize,
      types: [...types.entries()].map(([type, value]) => ({ type, ...value })),
    };
  }

  details(pageIndex = 0, pageSize = 100): Record<string, unknown> {
    const aggregates = this.aggregates();
    return {
      ...this.summary(),
      aggregates: paginate(aggregates.map(publicAggregate), pageIndex, pageSize),
    };
  }

  classNodes(classId: number, pageIndex = 0, pageSize = 100): Record<string, unknown> {
    const aggregate = this.aggregates().find(value => value.id === classId);
    if (!aggregate) throw new Error(`heap snapshot class does not exist: ${classId}`);
    return {
      class: publicAggregate(aggregate),
      nodes: paginate(aggregate.node_ordinals.map(ordinal => this.publicNode(ordinal)), pageIndex, pageSize),
    };
  }

  objectDetails(nodeId: number): Record<string, unknown> {
    const ordinal = this.ordinalForId(nodeId);
    return {
      node: this.publicNode(ordinal),
      outgoing_edge_count: this.nodes[ordinal].edgeCount,
      retainer_count: this.retainers()[ordinal].length,
    };
  }

  edges(nodeId: number, pageIndex = 0, pageSize = 100): Record<string, unknown> {
    const ordinal = this.ordinalForId(nodeId);
    return {
      node: this.publicNode(ordinal),
      edges: paginate(this.outgoingEdges(ordinal).map(edge => this.publicEdge(edge)), pageIndex, pageSize),
    };
  }

  retainersFor(nodeId: number, pageIndex = 0, pageSize = 100): Record<string, unknown> {
    const ordinal = this.ordinalForId(nodeId);
    return {
      node: this.publicNode(ordinal),
      retainers: paginate(this.retainers()[ordinal].map(edge => this.publicEdge(edge)), pageIndex, pageSize),
    };
  }

  dominators(nodeId: number): Record<string, unknown> {
    let ordinal = this.ordinalForId(nodeId);
    const data = this.dominatorData();
    const chain: Record<string, unknown>[] = [];
    const seen = new Set<number>();
    while (!seen.has(ordinal)) {
      seen.add(ordinal);
      chain.push(this.publicNode(ordinal));
      const parent = data.idom[ordinal];
      if (parent < 0 || parent === ordinal) break;
      ordinal = parent;
    }
    return { chain };
  }

  duplicateStrings(pageIndex = 0, pageSize = 100): Record<string, unknown> {
    const strings = new Map<string, { count: number; self_size: number; node_ids: number[] }>();
    for (const node of this.nodes) {
      if (!node.type.includes("string") || !node.name) continue;
      const current = strings.get(node.name) ?? { count: 0, self_size: 0, node_ids: [] };
      current.count += 1;
      current.self_size += node.selfSize;
      if (current.node_ids.length < 20) current.node_ids.push(node.id);
      strings.set(node.name, current);
    }
    const duplicates = [...strings.entries()]
      .filter(([, value]) => value.count > 1)
      .map(([value, stats]) => ({ value, ...stats }))
      .sort((left, right) => right.self_size - left.self_size || right.count - left.count);
    return { duplicate_strings: paginate(duplicates, pageIndex, pageSize) };
  }

  retainingPaths(nodeId: number, maxDepth = 8, maxNodes = 200, maxSiblings = 20): Record<string, unknown> {
    const start = this.ordinalForId(nodeId);
    const reverse = this.retainers();
    const paths: Array<Array<Record<string, unknown>>> = [];
    const queue: Array<{ ordinal: number; path: number[] }> = [{ ordinal: start, path: [start] }];
    let visited = 0;
    while (queue.length > 0 && visited < maxNodes) {
      const current = queue.shift()!;
      visited += 1;
      if (current.ordinal === 0 || current.path.length > maxDepth) {
        paths.push(current.path.map(ordinal => this.publicNode(ordinal)));
        continue;
      }
      const parents = reverse[current.ordinal].slice(0, maxSiblings);
      if (parents.length === 0) {
        paths.push(current.path.map(ordinal => this.publicNode(ordinal)));
      }
      for (const edge of parents) {
        if (current.path.includes(edge.from)) continue;
        queue.push({ ordinal: edge.from, path: [...current.path, edge.from] });
      }
    }
    return { paths, visited_nodes: visited, truncated: queue.length > 0 };
  }

  compare(current: HeapSnapshotModel, classId?: number): Record<string, unknown> {
    const base = new Map(this.aggregates().map(value => [`${value.type}\0${value.name}`, value]));
    const next = new Map(current.aggregates().map(value => [`${value.type}\0${value.name}`, value]));
    const keys = new Set([...base.keys(), ...next.keys()]);
    let changes = [...keys].map(key => {
      const before = base.get(key);
      const after = next.get(key);
      return {
        type: after?.type ?? before?.type ?? "unknown",
        name: after?.name ?? before?.name ?? "",
        count_delta: (after?.count ?? 0) - (before?.count ?? 0),
        self_size_delta: (after?.self_size ?? 0) - (before?.self_size ?? 0),
        retained_size_delta: (after?.retained_size ?? 0) - (before?.retained_size ?? 0),
      };
    }).filter(value => value.count_delta !== 0 || value.self_size_delta !== 0 || value.retained_size_delta !== 0);
    if (classId !== undefined) {
      const aggregate = current.aggregates().find(value => value.id === classId);
      if (!aggregate) throw new Error(`heap snapshot class does not exist: ${classId}`);
      changes = changes.filter(value => value.type === aggregate.type && value.name === aggregate.name);
    }
    changes.sort((left, right) => Math.abs(right.retained_size_delta) - Math.abs(left.retained_size_delta));
    return { base_file_path: this.filePath, current_file_path: current.filePath, changes };
  }

  private aggregates(): HeapAggregate[] {
    if (this.#aggregates) return this.#aggregates;
    const retained = this.dominatorData().retainedSizes;
    const grouped = new Map<string, Omit<HeapAggregate, "id">>();
    for (const node of this.nodes) {
      const key = `${node.type}\0${node.name}`;
      const current = grouped.get(key) ?? {
        type: node.type,
        name: node.name,
        count: 0,
        self_size: 0,
        retained_size: 0,
        node_ordinals: [],
      };
      current.count += 1;
      current.self_size += node.selfSize;
      current.retained_size += retained[node.ordinal];
      current.node_ordinals.push(node.ordinal);
      grouped.set(key, current);
    }
    this.#aggregates = [...grouped.values()]
      .sort((left, right) => right.retained_size - left.retained_size || right.self_size - left.self_size)
      .map((value, index) => ({ id: index + 1, ...value }));
    return this.#aggregates;
  }

  private outgoingEdges(ordinal: number): HeapEdge[] {
    const node = this.nodes[ordinal];
    const typeField = requiredField(this.#edgeFields, "type");
    const nameField = requiredField(this.#edgeFields, "name_or_index");
    const toField = requiredField(this.#edgeFields, "to_node");
    const edges: HeapEdge[] = [];
    for (let index = 0; index < node.edgeCount; index += 1) {
      const offset = node.edgeStart + index * this.#edgeFieldCount;
      const type = this.#edgeTypes[this.#rawEdges[offset + typeField] ?? 0] ?? "unknown";
      const rawName = this.#rawEdges[offset + nameField] ?? 0;
      edges.push({
        type,
        name: type === "element" || type === "hidden" ? rawName : this.#strings[rawName] ?? rawName,
        from: ordinal,
        to: Math.floor((this.#rawEdges[offset + toField] ?? 0) / this.#nodeFieldCount),
      });
    }
    return edges;
  }

  private retainers(): HeapEdge[][] {
    if (this.#retainers) return this.#retainers;
    const reverse = Array.from({ length: this.nodeCount }, () => [] as HeapEdge[]);
    for (const node of this.nodes) {
      for (const edge of this.outgoingEdges(node.ordinal)) {
        if (edge.to >= 0 && edge.to < reverse.length) reverse[edge.to].push(edge);
      }
    }
    this.#retainers = reverse;
    return reverse;
  }

  private dominatorData(): DominatorData {
    if (this.#dominators) return this.#dominators;
    const successors = this.nodes.map(node => this.outgoingEdges(node.ordinal)
      .filter(edge => edge.type !== "weak")
      .map(edge => edge.to)
      .filter(ordinal => ordinal >= 0 && ordinal < this.nodeCount));
    const predecessors = Array.from({ length: this.nodeCount }, () => [] as number[]);
    for (let from = 0; from < successors.length; from += 1) {
      for (const to of successors[from]) predecessors[to].push(from);
    }
    const postorder: number[] = [];
    const visited = new Uint8Array(this.nodeCount);
    const stack: Array<{ node: number; next: number }> = [{ node: 0, next: 0 }];
    visited[0] = 1;
    while (stack.length > 0) {
      const top = stack[stack.length - 1];
      if (top.next < successors[top.node].length) {
        const child = successors[top.node][top.next++];
        if (!visited[child]) {
          visited[child] = 1;
          stack.push({ node: child, next: 0 });
        }
      } else {
        postorder.push(top.node);
        stack.pop();
      }
    }
    const rpo = postorder.reverse();
    const rpoIndex = new Int32Array(this.nodeCount).fill(-1);
    rpo.forEach((ordinal, index) => { rpoIndex[ordinal] = index; });
    const idom = new Int32Array(this.nodeCount).fill(-1);
    idom[0] = 0;
    const intersect = (left: number, right: number): number => {
      let first = left;
      let second = right;
      while (first !== second) {
        while (rpoIndex[first] > rpoIndex[second]) first = idom[first];
        while (rpoIndex[second] > rpoIndex[first]) second = idom[second];
      }
      return first;
    };
    let changed = true;
    while (changed) {
      changed = false;
      for (const node of rpo.slice(1)) {
        const known = predecessors[node].filter(parent => idom[parent] >= 0);
        if (known.length === 0) continue;
        let parent = known[0];
        for (const candidate of known.slice(1)) parent = intersect(candidate, parent);
        if (idom[node] !== parent) {
          idom[node] = parent;
          changed = true;
        }
      }
    }
    const retainedSizes = new Float64Array(this.nodes.map(node => node.selfSize));
    for (const node of [...rpo].reverse()) {
      const parent = idom[node];
      if (parent >= 0 && parent !== node) retainedSizes[parent] += retainedSizes[node];
    }
    this.#dominators = { idom, retainedSizes };
    return this.#dominators;
  }

  private ordinalForId(nodeId: number): number {
    const node = this.nodes.find(value => value.id === nodeId);
    if (!node) throw new Error(`heap snapshot node does not exist: ${nodeId}`);
    return node.ordinal;
  }

  private publicNode(ordinal: number): Record<string, unknown> {
    const node = this.nodes[ordinal];
    return {
      node_id: node.id,
      ordinal,
      type: node.type,
      name: node.name,
      self_size: node.selfSize,
      retained_size: this.dominatorData().retainedSizes[ordinal],
      detached: node.detachedness > 0,
    };
  }

  private publicEdge(edge: HeapEdge): Record<string, unknown> {
    return {
      type: edge.type,
      name: edge.name,
      from: this.publicNode(edge.from),
      to: this.publicNode(edge.to),
    };
  }
}

function validateRawSnapshot(value: RawHeapSnapshot): void {
  if (!value?.snapshot?.meta || !Array.isArray(value.nodes) || !Array.isArray(value.edges) || !Array.isArray(value.strings)) {
    throw new Error("invalid V8 heap snapshot file");
  }
}

function stringTypeTable(value: unknown): string[] {
  if (!Array.isArray(value) || !value.every(entry => typeof entry === "string")) {
    throw new Error("invalid V8 heap snapshot type table");
  }
  return value;
}

function requiredField(fields: Map<string, number>, name: string): number {
  const value = fields.get(name);
  if (value === undefined) throw new Error(`heap snapshot field is missing: ${name}`);
  return value;
}

function paginate<T>(items: T[], pageIndex: number, pageSize: number): Record<string, unknown> {
  const safeIndex = Number.isSafeInteger(pageIndex) && pageIndex >= 0 ? pageIndex : 0;
  const safeSize = Number.isSafeInteger(pageSize) ? Math.max(1, Math.min(500, pageSize)) : 100;
  const start = safeIndex * safeSize;
  return {
    page_index: safeIndex,
    page_size: safeSize,
    total: items.length,
    items: items.slice(start, start + safeSize),
  };
}

function publicAggregate(value: HeapAggregate): Record<string, unknown> {
  return {
    id: value.id,
    type: value.type,
    name: value.name,
    count: value.count,
    self_size: value.self_size,
    retained_size: value.retained_size,
  };
}
