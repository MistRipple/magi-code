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
  }

  let { graph, layout = 'auto' }: Props = $props();

  let container: HTMLDivElement;
  let cy: Core | null = null;
  let error = $state('');
  let mounted = $state(false);
  let lastGraphKey = $state('');
  let renderToken = 0;

  const normalized = $derived(normalizeDiagramGraph(graph));
  const graphKey = $derived(JSON.stringify({
    nodes: normalized.nodes.map((node) => [node.id, node.label, node.position]),
    edges: normalized.edges.map((edge) => [edge.id, edge.source, edge.target, edge.label]),
    layout,
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
          'text-max-width': '130px',
          width: 124,
          height: 44,
          padding: '12px',
          shape: 'round-rectangle',
        },
      },
      {
        selector: 'edge',
        style: {
          label: 'data(label)',
          'line-color': connector,
          'target-arrow-color': connector,
          'target-arrow-shape': 'triangle',
          'curve-style': 'bezier',
          width: 2,
          color: muted,
          'font-size': 11,
          'font-family': 'ui-sans-serif, system-ui, sans-serif',
          'text-background-color': codeBg,
          'text-background-opacity': 0.85,
          'text-background-padding': '3px',
        },
      },
      {
        selector: ':selected',
        style: {
          'border-width': 2,
          'border-color': border,
          'line-color': border,
          'target-arrow-color': border,
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
      },
      selectable: true,
    }));
    return [...nodes, ...edges];
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
    cy.batch(() => {
      cy?.elements().remove();
      cy?.add(toElements());
      cy?.style(createStyle());
    });
    cy.layout({
      name: layoutName,
      fit: true,
      padding: 32,
      ...(layoutName === 'breadthfirst' ? { directed: true, spacingFactor: 1.15 } : {}),
      ...(layoutName === 'fcose' ? { animate: false, quality: 'proof', nodeRepulsion: 6500, idealEdgeLength: 100 } : {}),
      ...(layoutName === 'cose-bilkent' ? { animate: false, quality: 'proof', nodeRepulsion: 6500, idealEdgeLength: 100 } : {}),
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

    return () => {
      renderToken += 1;
      observer.disconnect();
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
