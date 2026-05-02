<script lang="ts">
  import type { Component } from 'svelte';
  import type { DiagramPayload } from '../lib/diagram-payload';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    payload: DiagramPayload;
    embedded?: boolean;
  }

  let { payload, embedded = false }: Props = $props();

  type MermaidRendererProps = {
    code: string;
    title?: string;
    diagramType?: string;
    layout?: string;
    embedded?: boolean;
  };
  type GraphvizRendererProps = {
    source: string;
    title?: string;
    layout?: string;
    embedded?: boolean;
  };
  type CytoscapeRendererProps = {
    graph: DiagramPayload['graph'];
    title?: string;
    layout?: string;
    embedded?: boolean;
  };
  type SvelteFlowRendererProps = {
    graph: DiagramPayload['graph'];
    title?: string;
    embedded?: boolean;
  };

  let MermaidComponent = $state<Component<MermaidRendererProps> | null>(null);
  let GraphvizComponent = $state<Component<GraphvizRendererProps> | null>(null);
  let CytoscapeComponent = $state<Component<CytoscapeRendererProps> | null>(null);
  let SvelteFlowComponent = $state<Component<SvelteFlowRendererProps> | null>(null);
  let loadError = $state('');
  let failedKind = $state<DiagramPayload['kind'] | null>(null);
  let failedMessage = $state('');
  let requestedKind = $state<DiagramPayload['kind'] | null>(null);

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
</script>

{#if loadError}
  <div class="diagram-loading diagram-error" class:embedded>
    <span>{i18n.t('diagramRenderer.renderFailed')}</span>
    <small>{loadError}</small>
  </div>
{:else if payload.kind === 'mermaid' && payload.source}
  {#if MermaidComponent}
    <MermaidComponent
      code={payload.source}
      title={payload.title}
      diagramType={payload.diagramType}
      layout={payload.layout}
      embedded={embedded}
    />
  {:else}
    <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
  {/if}
{:else if payload.kind === 'dot' && payload.source}
  {#if GraphvizComponent}
    <GraphvizComponent source={payload.source} title={payload.title} layout={payload.layout} embedded={embedded} />
  {:else}
    <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
  {/if}
{:else if payload.kind === 'graph' || payload.kind === 'flow'}
  {#if payload.kind === 'graph'}
    {#if CytoscapeComponent}
      <CytoscapeComponent graph={payload.graph} title={payload.title} layout={payload.layout} embedded={embedded} />
    {:else}
      <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
    {/if}
  {:else}
    {#if SvelteFlowComponent}
      <SvelteFlowComponent graph={payload.graph} title={payload.title} embedded={embedded} />
    {:else}
      <div class="diagram-loading" class:embedded>{i18n.t('diagramRenderer.rendering')}</div>
    {/if}
  {/if}
{/if}

<style>
  .diagram-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 180px;
    margin: var(--space-2, 8px) 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--code-bg);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
  }

  .diagram-loading.embedded {
    margin: 0;
    border: none;
    border-radius: 0;
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
