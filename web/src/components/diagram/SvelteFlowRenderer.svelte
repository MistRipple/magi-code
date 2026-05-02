<script lang="ts">
  import '@xyflow/svelte/dist/style.css';
  import {
    Background,
    BackgroundVariant,
    Controls,
    MarkerType,
    SvelteFlow,
    type Edge,
    type Node,
  } from '@xyflow/svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import type { DiagramPayload } from '../../lib/diagram-payload';
  import {
    normalizeDiagramGraph,
    type DiagramPosition,
    type NormalizedDiagramEdge,
    type NormalizedDiagramNode,
  } from '../../lib/diagram-graph';

  interface Props {
    graph: DiagramPayload['graph'];
    title?: string;
    embedded?: boolean;
  }

  let { graph, title = '', embedded = false }: Props = $props();
  let copied = $state(false);

  const normalized = $derived(normalizeDiagramGraph(graph));
  const generatedPositions = $derived(computePositions(normalized.nodes, normalized.edges));
  const flowNodes = $derived(toFlowNodes(normalized.nodes, generatedPositions));
  const flowEdges = $derived(toFlowEdges(normalized.edges));

  function computePositions(
    nodes: NormalizedDiagramNode[],
    edges: NormalizedDiagramEdge[],
  ): Map<string, DiagramPosition> {
    const positions = new Map<string, DiagramPosition>();
    const explicit = nodes.filter((node) => node.position);
    if (explicit.length === nodes.length) {
      for (const node of nodes) {
        if (node.position) positions.set(node.id, node.position);
      }
      return positions;
    }

    const indegree = new Map(nodes.map((node) => [node.id, 0]));
    const adjacency = new Map(nodes.map((node) => [node.id, [] as string[]]));
    for (const edge of edges) {
      adjacency.get(edge.source)?.push(edge.target);
      indegree.set(edge.target, (indegree.get(edge.target) ?? 0) + 1);
    }

    const level = new Map<string, number>();
    const queue = nodes.filter((node) => (indegree.get(node.id) ?? 0) === 0).map((node) => node.id);
    if (queue.length === 0 && nodes[0]) queue.push(nodes[0].id);

    while (queue.length) {
      const current = queue.shift();
      if (!current) continue;
      const currentLevel = level.get(current) ?? 0;
      for (const next of adjacency.get(current) ?? []) {
        level.set(next, Math.max(level.get(next) ?? 0, currentLevel + 1));
        indegree.set(next, (indegree.get(next) ?? 0) - 1);
        if ((indegree.get(next) ?? 0) <= 0) queue.push(next);
      }
    }

    nodes.forEach((node, index) => {
      if (!level.has(node.id)) {
        level.set(node.id, Math.floor(index / 4));
      }
    });

    const rowsByLevel = new Map<number, string[]>();
    for (const node of nodes) {
      const nodeLevel = level.get(node.id) ?? 0;
      rowsByLevel.set(nodeLevel, [...(rowsByLevel.get(nodeLevel) ?? []), node.id]);
    }

    for (const [nodeLevel, ids] of rowsByLevel.entries()) {
      ids.forEach((id, row) => {
        positions.set(id, {
          x: nodeLevel * 240,
          y: row * 96,
        });
      });
    }

    for (const node of nodes) {
      if (node.position) positions.set(node.id, node.position);
    }
    return positions;
  }

  function toFlowNodes(
    nodes: NormalizedDiagramNode[],
    positions: Map<string, DiagramPosition>,
  ): Node[] {
    return nodes.map((node, index) => ({
      id: node.id,
      type: index === 0 ? 'input' : undefined,
      data: { label: node.label },
      position: positions.get(node.id) ?? { x: 0, y: index * 96 },
      draggable: false,
      selectable: false,
      focusable: false,
      style: [
        'min-width: 132px',
        'max-width: 220px',
        'padding: 8px 12px',
        'border: 1px solid var(--primary)',
        'border-radius: 6px',
        'background: var(--surface-2)',
        'color: var(--foreground)',
        'font-size: 12px',
        'line-height: 1.35',
        'box-shadow: none',
      ].join(';'),
    }));
  }

  function toFlowEdges(edges: NormalizedDiagramEdge[]): Edge[] {
    return edges.map((edge) => ({
      id: edge.id,
      source: edge.source,
      target: edge.target,
      label: edge.label,
      type: 'smoothstep',
      selectable: false,
      focusable: false,
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color: null,
        width: 16,
        height: 16,
      },
      style: 'stroke: var(--diagram-connector, color-mix(in srgb, var(--foreground) 82%, var(--background) 18%)); stroke-width: 2;',
      labelStyle: 'fill: var(--foreground-muted); font-size: 11px;',
    }));
  }

  async function copyGraph(): Promise<void> {
    try {
      await navigator.clipboard.writeText(JSON.stringify(graph, null, 2));
      copied = true;
      setTimeout(() => {
        copied = false;
      }, 1600);
    } catch (e) {
      console.error('[SvelteFlowRenderer] 复制图数据失败:', e);
    }
  }
</script>

<div class="flow-container" class:embedded>
  {#if !embedded}
    <div class="flow-header">
      <div class="header-left">
        <Icon name="git-branch" size={14} />
        <span class="header-type">{i18n.t('diagramRenderer.kind.flow')}</span>
        <span class="header-title">
          {title || `${normalized.nodes.length} ${i18n.t('diagramRenderer.nodes')} · ${normalized.edges.length} ${i18n.t('diagramRenderer.edges')}`}
        </span>
      </div>
      <button class="header-btn" onclick={copyGraph} disabled={!graph} title={i18n.t('diagramRenderer.copyData')}>
        <Icon name={copied ? 'check' : 'copy'} size={14} />
      </button>
    </div>
  {/if}

  <div class="flow-content">
    {#if normalized.nodes.length === 0}
      <div class="empty">
        <Icon name="alert-circle" size={20} />
        <span>{i18n.t('diagramRenderer.emptyGraph')}</span>
      </div>
    {:else}
      <SvelteFlow
        nodes={flowNodes}
        edges={flowEdges}
        fitView
        fitViewOptions={{ padding: 0.18, minZoom: 0.35, maxZoom: 1.2 }}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        deleteKey={null}
        selectionKey={null}
        multiSelectionKey={null}
        panOnDrag={true}
        zoomOnScroll={true}
        zoomOnDoubleClick={false}
        preventScrolling={false}
        proOptions={{ hideAttribution: true }}
        style="width: 100%; height: 100%;"
      >
        <Background variant={BackgroundVariant.Dots} gap={18} size={1.4} />
        <Controls showLock={false} position="bottom-left" />
      </SvelteFlow>
    {/if}
  </div>
</div>

<style>
  .flow-container {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
    background: var(--surface-1);
    margin: var(--space-2, 8px) 0;
  }

  .flow-container.embedded {
    border: none;
    border-radius: 0;
    margin: 0;
    background: transparent;
  }

  .flow-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-2, 8px) var(--space-3, 12px);
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    min-width: 0;
    color: var(--info);
  }

  .header-type,
  .header-title {
    font-size: var(--text-sm, 13px);
  }

  .header-type {
    font-weight: 500;
    flex-shrink: 0;
  }

  .header-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .header-btn {
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

  .header-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .flow-content {
    position: relative;
    height: 420px;
    background: var(--code-bg);
    --xy-background-color: var(--code-bg);
    --xy-background-pattern-dots-color-default: color-mix(in srgb, var(--foreground) 20%, transparent);
    --xy-edge-stroke: var(--diagram-connector, color-mix(in srgb, var(--foreground) 82%, var(--background) 18%));
    --xy-edge-stroke-width: 2;
    --xy-edge-label-background-color: var(--code-bg);
    --xy-edge-label-color: var(--foreground-muted);
    --xy-controls-button-background-color: var(--surface-2);
    --xy-controls-button-background-color-hover: var(--surface-hover);
    --xy-controls-button-color: var(--foreground-muted);
    --xy-controls-button-color-hover: var(--foreground);
    --xy-controls-button-border-color: var(--border);
    --xy-controls-box-shadow: none;
  }

  .flow-content :global(.svelte-flow__edge-path) {
    vector-effect: non-scaling-stroke;
  }

  .flow-content :global(.svelte-flow__edge-textbg) {
    fill: var(--code-bg) !important;
  }

  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-2, 8px);
    color: var(--error);
    font-size: var(--text-sm, 13px);
  }
</style>
