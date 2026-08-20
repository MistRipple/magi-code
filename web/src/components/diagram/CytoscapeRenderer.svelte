<script module lang="ts">
  const registeredLayouts = new Set<string>();
  const layoutRegistrationPromises = new Map<string, Promise<void>>();
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import cytoscape from 'cytoscape';
  import type { Core, ElementDefinition, LayoutOptions, StylesheetJson } from 'cytoscape';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import type { DiagramPayload } from '../../lib/diagram-payload';
  import { normalizeDiagramGraph } from '../../lib/diagram-graph';

  interface Props {
    graph: DiagramPayload['graph'];
    layout?: string;
    rootNodeId?: string | null;
    onNodeClick?: (node: Record<string, unknown>) => void;
    onEdgeClick?: (edge: Record<string, unknown>) => void;
  }

  let { graph, layout = 'auto', rootNodeId = null, onNodeClick, onEdgeClick }: Props = $props();

  let container: HTMLDivElement;
  let cy: Core | null = null;
  let error = $state('');
  let mounted = $state(false);
  let lastGraphKey = $state('');
  let renderToken = 0;

  const normalized = $derived(normalizeDiagramGraph(graph));
  const graphKey = $derived(JSON.stringify({
    nodes: normalized.nodes.map((node) => [node.id, node.label, node.type, node.position]),
    edges: normalized.edges.map((edge) => [edge.id, edge.source, edge.target, edge.label, edge.type, edge.data.status]),
    layout,
    rootNodeId,
  }));

  function readThemeToken(name: string, fallback: string): string {
    if (typeof window === 'undefined') return fallback;
    const rootStyles = window.getComputedStyle(document.documentElement);
    const bodyStyles = document.body ? window.getComputedStyle(document.body) : null;
    return bodyStyles?.getPropertyValue(name).trim()
      || rootStyles.getPropertyValue(name).trim()
      || fallback;
  }

  function createStyle(): StylesheetJson {
    const muted = readThemeToken('--foreground-muted', '#94a3b8');
    const nodeBg = readThemeToken('--diagram-node-bg', '#f8fafc');
    const nodeText = readThemeToken('--diagram-node-text', '#111827');
    const border = readThemeToken('--primary', '#2563eb');
    const connector = readThemeToken('--diagram-connector', '#cbd5e1');
    const codeBg = readThemeToken('--code-bg', '#111827');

    return [
      {
        selector: 'node',
        style: {
          label: 'data(label)',
          'background-color': nodeBg,
          'border-color': border,
          'border-width': 1.5,
          color: nodeText,
          'font-size': 12,
          'font-weight': 600,
          'font-family': 'ui-sans-serif, system-ui, sans-serif',
          'text-valign': 'center',
          'text-halign': 'center',
          'text-wrap': 'wrap',
          // CJK 文本没有空格，必须允许按任意字符换行；预留四行中文标题的高度，
          // 避免长标题从固定尺寸节点中溢出，形成“文字脱离卡片”的错觉。
          'text-overflow-wrap': 'anywhere',
          'text-justification': 'center',
          'line-height': 1.2,
          'text-max-width': '140px',
          width: 160,
          height: 64,
          padding: '8px',
          'z-index': 10,
          shape: 'round-rectangle',
        },
      },
      {
        selector: 'node[type="knowledge"]',
        style: {
          'background-color': readThemeToken('--knowledge-graph-knowledge-bg', '#e7f0ff'),
          'border-color': readThemeToken('--knowledge-graph-knowledge-border', '#2563eb'),
        },
      },
      {
        selector: 'node[type="file"]',
        style: {
          'background-color': readThemeToken('--knowledge-graph-file-bg', '#edf7f2'),
          'border-color': readThemeToken('--knowledge-graph-file-border', '#16825d'),
        },
      },
      {
        selector: 'node[type="symbol"]',
        style: {
          'background-color': readThemeToken('--knowledge-graph-symbol-bg', '#fff4df'),
          'border-color': readThemeToken('--knowledge-graph-symbol-border', '#c47700'),
        },
      },
      {
        selector: 'edge',
        style: {
          label: '',
          'line-color': connector,
          'target-arrow-color': connector,
          'target-arrow-shape': 'triangle',
          'curve-style': 'bezier',
          'control-point-step-size': 56,
          'edge-distances': 'intersection',
          'line-cap': 'round',
          opacity: 0.5,
          width: 1.6,
          color: muted,
          'font-size': 11,
          'font-family': 'ui-sans-serif, system-ui, sans-serif',
          'text-background-color': codeBg,
          'text-background-opacity': 0.85,
          'text-background-padding': '3px',
          'text-rotation': 'autorotate',
          'z-index': 1,
        },
      },
      {
        selector: 'edge[origin="deterministic_code"]',
        style: {
          opacity: 0.32,
          width: 1.2,
        },
      },
      {
        selector: 'edge[status="candidate"]',
        style: {
          'line-style': 'dashed',
          'line-color': readThemeToken('--knowledge-graph-candidate', '#c47700'),
          'target-arrow-color': readThemeToken('--knowledge-graph-candidate', '#c47700'),
          opacity: 0.78,
          width: 1.9,
        },
      },
      {
        selector: 'edge[status="rejected"]',
        style: {
          opacity: 0.35,
          'line-style': 'dotted',
        },
      },
      {
        selector: 'edge[status="dangling"]',
        style: {
          'line-style': 'dashed',
          'line-color': readThemeToken('--knowledge-graph-dangling', '#b42318'),
          'target-arrow-color': readThemeToken('--knowledge-graph-dangling', '#b42318'),
        },
      },
      {
        selector: ':selected',
        style: {
          'border-width': 2,
          'border-color': border,
          'line-color': border,
          'target-arrow-color': border,
          label: 'data(label)',
          opacity: 1,
          width: 2.4,
          'text-background-opacity': 0.95,
          'text-margin-y': -7,
          'z-index': 20,
        },
      },
    ];
  }

  function resolveLayoutName(): string {
    switch (layout.trim().toLowerCase()) {
      case 'grid':
        return 'grid';
      case 'circle':
        return 'circle';
      case 'preset':
        return 'preset';
      case 'dagre':
      case 'elk':
      case 'tidy-tree':
        return 'breadthfirst';
      case 'breadthfirst':
        return 'concentric';
      case 'force':
      case 'fcose':
        return 'fcose';
      case 'cose-bilkent':
      case 'bilkent':
        return 'cose-bilkent';
      case 'cose':
        return 'cose';
      case 'auto':
      default:
        return normalized.nodes.length > 120 ? 'grid' : 'fcose';
    }
  }

  function registerExternalLayout(
    name: string,
    loader: () => Promise<{ default: cytoscape.Ext }>,
  ): Promise<void> {
    if (registeredLayouts.has(name)) {
      return Promise.resolve();
    }
    const existing = layoutRegistrationPromises.get(name);
    if (existing) {
      return existing;
    }
    const promise = loader()
      .then((module) => {
        if (!registeredLayouts.has(name)) {
          cytoscape.use(module.default);
          registeredLayouts.add(name);
        }
      })
      .catch((registrationError) => {
        layoutRegistrationPromises.delete(name);
        throw registrationError;
      });
    layoutRegistrationPromises.set(name, promise);
    return promise;
  }

  function ensureLayoutRegistered(layoutName: string): Promise<void> {
    if (layoutName === 'fcose') {
      return registerExternalLayout('fcose', () => import('cytoscape-fcose'));
    }
    if (layoutName === 'cose-bilkent') {
      return registerExternalLayout('cose-bilkent', () => import('cytoscape-cose-bilkent'));
    }
    return Promise.resolve();
  }

  function toElements(): ElementDefinition[] {
    const nodes = normalized.nodes.map((node) => ({
      group: 'nodes' as const,
      data: {
        ...node.data,
        id: node.id,
        label: node.label,
        type: node.type || '',
      },
      position: node.position,
      locked: false,
      grabbable: false,
      selectable: true,
    }));
    const edges = normalized.edges.map((edge) => ({
      group: 'edges' as const,
      data: {
        ...edge.data,
        id: edge.id,
        source: edge.source,
        target: edge.target,
        label: edge.label || '',
        type: edge.type || '',
        status: typeof edge.data.status === 'string' ? edge.data.status : '',
      },
      selectable: true,
    }));
    return [...nodes, ...edges];
  }

  function resolveRootNodeId(): string | null {
    const nodeIds = new Set(normalized.nodes.map((node) => node.id));
    return rootNodeId && nodeIds.has(rootNodeId)
      ? rootNodeId
      : normalized.nodes.find((node) => node.type === 'knowledge')?.id ?? normalized.nodes[0]?.id;
  }

  function nodeDepths(root: string | null): Map<string, number> {
    if (!root) return new Map();
    const adjacency = new Map<string, Set<string>>();
    for (const node of normalized.nodes) adjacency.set(node.id, new Set());
    for (const edge of normalized.edges) {
      adjacency.get(edge.source)?.add(edge.target);
      adjacency.get(edge.target)?.add(edge.source);
    }

    const depths = new Map<string, number>([[root, 0]]);
    const queue = [root];
    for (let index = 0; index < queue.length; index += 1) {
      const current = queue[index];
      const nextDepth = (depths.get(current) ?? 0) + 1;
      for (const neighbor of [...(adjacency.get(current) ?? [])].sort()) {
        if (!depths.has(neighbor)) {
          depths.set(neighbor, nextDepth);
          queue.push(neighbor);
        }
      }
    }

    return depths;
  }

  async function updateGraph(): Promise<void> {
    if (!cy) return;
    const currentRenderToken = ++renderToken;
    if (normalized.nodes.length === 0) {
      error = i18n.t('diagramRenderer.emptyGraph');
      cy.elements().remove();
      return;
    }

    error = '';
    const layoutName = resolveLayoutName();
    try {
      await ensureLayoutRegistered(layoutName);
    } catch (registrationError) {
      if (currentRenderToken !== renderToken) return;
      console.warn('[CytoscapeRenderer] graph layout registration failed:', registrationError);
      error = i18n.t('diagramRenderer.renderHint');
      return;
    }
    if (!cy || currentRenderToken !== renderToken) return;
    const elements: ElementDefinition[] = toElements();
    const focusRootId = layoutName === 'concentric' ? resolveRootNodeId() : null;
    const depths = nodeDepths(focusRootId);
    cy.batch(() => {
      cy?.elements().remove();
      cy?.add(elements);
      cy?.style(createStyle());
    });
    cy.layout({
      name: layoutName,
      fit: true,
      padding: 32,
      ...(layoutName === 'breadthfirst' ? {
        directed: true,
        direction: 'rightward',
        spacingFactor: 1.45,
        avoidOverlap: true,
        nodeDimensionsIncludeLabels: true,
      } : {}),
      ...(layoutName === 'concentric' ? {
        animate: false,
        avoidOverlap: true,
        nodeDimensionsIncludeLabels: true,
        minNodeSpacing: 28,
        startAngle: -Math.PI / 2,
        clockwise: true,
        concentric: (node: { id: () => string }) => {
          const depth = depths.get(node.id()) ?? 1;
          return Math.max(1, 3 - Math.min(depth, 2));
        },
        levelWidth: () => 1,
      } : {}),
      ...(layoutName === 'fcose' ? {
        animate: false,
        quality: 'proof',
        nodeDimensionsIncludeLabels: true,
        nodeRepulsion: 12000,
        idealEdgeLength: 160,
        edgeElasticity: 0.22,
        gravity: 0.15,
        numIter: 3200,
        tilingPaddingHorizontal: 28,
        tilingPaddingVertical: 28,
      } : {}),
      ...(layoutName === 'cose-bilkent' ? {
        animate: false,
        quality: 'proof',
        nodeDimensionsIncludeLabels: true,
        nodeRepulsion: 12000,
        idealEdgeLength: 160,
        edgeElasticity: 0.22,
        gravity: 0.15,
        numIter: 3200,
      } : {}),
    } as LayoutOptions).run();
    requestAnimationFrame(() => {
      cy?.resize();
      cy?.fit(undefined, 32);
    });
    lastGraphKey = graphKey;
  }

  $effect(() => {
    if (mounted && cy && graphKey !== lastGraphKey) {
      void updateGraph();
    }
  });

  onMount(() => {
    mounted = true;
    cy = cytoscape({
      container,
      elements: [],
      minZoom: 0.15,
      maxZoom: 4,
      boxSelectionEnabled: false,
      autoungrabify: true,
      autounselectify: false,
      style: createStyle(),
    });
    cy.on('tap', 'node', (event) => {
      onNodeClick?.(event.target.data() as Record<string, unknown>);
    });
    cy.on('tap', 'edge', (event) => {
      onEdgeClick?.(event.target.data() as Record<string, unknown>);
    });
    void updateGraph();

    const observer = new MutationObserver(() => {
      cy?.style(createStyle());
      cy?.resize();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-vscode-theme-id'],
    });
    if (document.body) {
      observer.observe(document.body, {
        attributes: true,
        attributeFilter: ['class', 'style', 'data-vscode-theme-id'],
      });
    }

    const resizeObserver = new ResizeObserver(() => {
      cy?.resize();
      cy?.fit(undefined, 32);
    });
    resizeObserver.observe(container);

    return () => {
      renderToken += 1;
      observer.disconnect();
      resizeObserver.disconnect();
      cy?.destroy();
      cy = null;
    };
  });

  function fitView(): void {
    cy?.resize();
    cy?.fit(undefined, 32);
  }

  function zoomIn(): void {
    cy?.zoom({
      level: Math.min((cy.zoom() || 1) * 1.2, 4),
      renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 },
    });
  }

  function zoomOut(): void {
    cy?.zoom({
      level: Math.max((cy.zoom() || 1) / 1.2, 0.15),
      renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 },
    });
  }

</script>

<div class="graph-renderer">
  <div class="graph-content">
    {#if error}
      <div class="error">
        <Icon name="alert-circle" size={20} />
        <span>{error}</span>
      </div>
    {/if}
    <div bind:this={container} class="cy-host" class:hidden={!!error}></div>
    {#if !error}
      <div class="floating-controls">
        <button class="control-btn" onclick={zoomIn} title={i18n.t('diagramRenderer.zoomIn')}>
          <Icon name="plus" size={14} />
        </button>
        <button class="control-btn" onclick={zoomOut} title={i18n.t('diagramRenderer.zoomOut')}>
          <Icon name="minus" size={14} />
        </button>
        <button class="control-btn" onclick={fitView} title={i18n.t('diagramRenderer.fitView')}>
          <Icon name="maximize" size={14} />
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .graph-renderer {
    overflow: hidden;
    background: transparent;
  }

  .control-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all 0.15s;
  }

  .control-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .graph-content {
    position: relative;
    min-height: 320px;
    background: var(--code-bg);
  }

  .cy-host {
    width: 100%;
    height: 420px;
  }

  .cy-host.hidden {
    visibility: hidden;
  }

  .floating-controls {
    position: absolute;
    bottom: var(--space-3, 12px);
    left: var(--space-3, 12px);
    display: flex;
    flex-direction: column;
    gap: var(--space-1, 4px);
  }

  .control-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
  }

  .error {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-2, 8px);
    padding: var(--space-4, 16px);
    color: var(--error);
    font-size: var(--text-sm, 13px);
    text-align: center;
  }
</style>
