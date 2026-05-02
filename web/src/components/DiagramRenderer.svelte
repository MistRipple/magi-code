<script lang="ts">
  import type { Component } from 'svelte';
  import Icon from './Icon.svelte';
  import type { DiagramPayload } from '../lib/diagram-payload';
  import { postMessage } from '../lib/vscode-bridge';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    payload: DiagramPayload;
    embedded?: boolean;
  }

  let { payload, embedded = false }: Props = $props();

  type MermaidRendererProps = {
    code: string;
    layout?: string;
  };
  type GraphvizRendererProps = {
    source: string;
    layout?: string;
  };
  type CytoscapeRendererProps = {
    graph: DiagramPayload['graph'];
    layout?: string;
  };
  type SvelteFlowRendererProps = {
    graph: DiagramPayload['graph'];
  };

  let MermaidComponent = $state<Component<MermaidRendererProps> | null>(null);
  let GraphvizComponent = $state<Component<GraphvizRendererProps> | null>(null);
  let CytoscapeComponent = $state<Component<CytoscapeRendererProps> | null>(null);
  let SvelteFlowComponent = $state<Component<SvelteFlowRendererProps> | null>(null);
  let loadError = $state('');
  let failedKind = $state<DiagramPayload['kind'] | null>(null);
  let failedMessage = $state('');
  let requestedKind = $state<DiagramPayload['kind'] | null>(null);
  let copied = $state(false);

  function recordLoadError(kind: DiagramPayload['kind'], error: unknown): void {
    failedKind = kind;
    failedMessage = error instanceof Error ? error.message : String(error);
    loadError = failedMessage;
  }

  function ensureRenderer(kind: DiagramPayload['kind']): void {
    requestedKind = kind;
    if (failedKind === kind) {
      loadError = failedMessage;
      return;
    }
    loadError = '';
    switch (kind) {
      case 'mermaid':
        if (!MermaidComponent) {
          void import('./MermaidRenderer.svelte').then((module) => {
            if (requestedKind !== kind) return;
            MermaidComponent = module.default;
          }).catch((error) => {
            if (requestedKind === kind) recordLoadError(kind, error);
          });
        }
        break;
      case 'dot':
        if (!GraphvizComponent) {
          void import('./diagram/GraphvizRenderer.svelte').then((module) => {
            if (requestedKind !== kind) return;
            GraphvizComponent = module.default;
          }).catch((error) => {
            if (requestedKind === kind) recordLoadError(kind, error);
          });
        }
        break;
      case 'graph':
        if (!CytoscapeComponent) {
          void import('./diagram/CytoscapeRenderer.svelte').then((module) => {
            if (requestedKind !== kind) return;
            CytoscapeComponent = module.default;
          }).catch((error) => {
            if (requestedKind === kind) recordLoadError(kind, error);
          });
        }
        break;
      case 'flow':
        if (!SvelteFlowComponent) {
          void import('./diagram/SvelteFlowRenderer.svelte').then((module) => {
            if (requestedKind !== kind) return;
            SvelteFlowComponent = module.default;
          }).catch((error) => {
            if (requestedKind === kind) recordLoadError(kind, error);
          });
        }
        break;
    }
  }

  $effect(() => {
    ensureRenderer(payload.kind);
  });

  function diagramTypeLabel(type: string | undefined): string {
    if (!type) return '';
    const keyMap: Record<string, string> = {
      flowchart: 'diagramRenderer.diagramType.flowchart',
      sequence: 'diagramRenderer.diagramType.sequence',
      class: 'diagramRenderer.diagramType.class',
      state: 'diagramRenderer.diagramType.state',
      er: 'diagramRenderer.diagramType.er',
      gantt: 'diagramRenderer.diagramType.gantt',
      pie: 'diagramRenderer.diagramType.pie',
      journey: 'diagramRenderer.diagramType.journey',
      git: 'diagramRenderer.diagramType.git',
      timeline: 'diagramRenderer.diagramType.timeline',
      quadrant: 'diagramRenderer.diagramType.quadrant',
      requirement: 'diagramRenderer.diagramType.requirement',
      c4: 'diagramRenderer.diagramType.c4',
      sankey: 'diagramRenderer.diagramType.sankey',
      xychart: 'diagramRenderer.diagramType.xychart',
      block: 'diagramRenderer.diagramType.block',
      dot: 'diagramRenderer.kind.dot',
      graph: 'diagramRenderer.kind.graph',
      flow: 'diagramRenderer.kind.flow',
    };
    const key = keyMap[type.trim().toLowerCase()];
    return key ? i18n.t(key) : '';
  }

  function graphStats(): string {
    const nodes = Array.isArray(payload.graph?.nodes) ? payload.graph.nodes.length : 0;
    const edges = Array.isArray(payload.graph?.edges) ? payload.graph.edges.length : 0;
    return nodes || edges
      ? `${nodes} ${i18n.t('diagramRenderer.nodes')} · ${edges} ${i18n.t('diagramRenderer.edges')}`
      : '';
  }

  const previewText = $derived.by(() => {
    if (payload.source?.trim()) {
      return payload.source.trim();
    }
    if (payload.graph) {
      return JSON.stringify(payload.graph, null, 2);
    }
    return '';
  });

  const displayTitle = $derived.by(() => {
    const title = payload.title?.trim();
    if (title) return title;
    return diagramTypeLabel(payload.diagramType) || graphStats() || i18n.t('diagramRenderer.title');
  });

  const hasSubtitle = $derived(displayTitle !== i18n.t('diagramRenderer.title'));
  const canCopyPayload = $derived(previewText.trim().length > 0);

  async function copyPayload(): Promise<void> {
    if (!canCopyPayload) return;
    try {
      await navigator.clipboard.writeText(previewText);
      copied = true;
      setTimeout(() => {
        copied = false;
      }, 1600);
    } catch (error) {
      console.error('[DiagramRenderer] 复制图表数据失败:', error);
    }
  }

  function openPreview(): void {
    if (!previewText.trim()) return;
    postMessage({
      type: 'openDiagramPanel',
      kind: 'diagram',
      source: previewText,
      title: displayTitle,
    });
  }
</script>

{#snippet rendererContent()}
  {#if loadError}
    <div class="diagram-loading diagram-error" class:embedded>
      <span>{i18n.t('diagramRenderer.renderFailed')}</span>
      <small>{loadError}</small>
    </div>
  {:else if payload.kind === 'mermaid' && payload.source}
    {#if MermaidComponent}
      <MermaidComponent
        code={payload.source}
        layout={payload.layout}
      />
    {:else}
      <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
    {/if}
  {:else if payload.kind === 'dot' && payload.source}
    {#if GraphvizComponent}
      <GraphvizComponent source={payload.source} layout={payload.layout} />
    {:else}
      <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
    {/if}
  {:else if payload.kind === 'graph' || payload.kind === 'flow'}
    {#if payload.kind === 'graph'}
      {#if CytoscapeComponent}
        <CytoscapeComponent graph={payload.graph} layout={payload.layout} />
      {:else}
        <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
      {/if}
    {:else}
      {#if SvelteFlowComponent}
        <SvelteFlowComponent graph={payload.graph} />
      {:else}
        <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
      {/if}
    {/if}
  {/if}
{/snippet}

{#if embedded}
  {@render rendererContent()}
{:else}
  <div class="diagram-shell">
    <div class="diagram-header">
      <div class="header-left">
        <Icon name="git-branch" size={14} />
        <span class="header-type">{i18n.t('diagramRenderer.title')}</span>
        {#if hasSubtitle}
          <span class="header-title">{displayTitle}</span>
        {/if}
      </div>
      <div class="header-actions">
        <button
          class="header-btn"
          onclick={copyPayload}
          disabled={!canCopyPayload}
          title={i18n.t('diagramRenderer.copyData')}
        >
          <Icon name={copied ? 'check' : 'copy'} size={14} />
        </button>
        <button
          class="header-btn"
          onclick={openPreview}
          disabled={!canCopyPayload}
          title={i18n.t('diagramRenderer.openInNewTab')}
        >
          <Icon name="external-link" size={14} />
        </button>
      </div>
    </div>
    <div class="diagram-body">
      {@render rendererContent()}
    </div>
  </div>
{/if}

<style>
  .diagram-shell {
    overflow: hidden;
    margin: var(--space-2, 8px) 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
  }

  .diagram-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3, 12px);
    padding: var(--space-2, 8px) var(--space-3, 12px);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }

  .header-left {
    display: flex;
    min-width: 0;
    align-items: center;
    gap: var(--space-2, 8px);
    color: var(--info);
  }

  .header-type,
  .header-title {
    font-size: var(--text-sm, 13px);
  }

  .header-type {
    flex-shrink: 0;
    font-weight: 500;
  }

  .header-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .header-actions {
    display: flex;
    flex-shrink: 0;
    align-items: center;
    gap: 4px;
  }

  .header-btn {
    display: flex;
    width: 28px;
    height: 28px;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all 0.15s;
  }

  .header-btn:hover:not(:disabled) {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .header-btn:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .diagram-body {
    background: var(--code-bg);
  }

  .diagram-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 180px;
    margin: 0;
    background: var(--code-bg);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
  }

  .diagram-loading.embedded {
    min-height: 260px;
  }

  .diagram-error {
    flex-direction: column;
    color: var(--error);
    text-align: center;
  }

  .diagram-error small {
    max-width: 100%;
    overflow-wrap: anywhere;
    color: var(--foreground-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
  }
</style>
