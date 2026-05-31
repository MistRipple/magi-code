<script module lang="ts">
  import { instance, type Viz } from '@viz-js/viz';

  const vizPromise: Promise<Viz> = instance();
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { sanitizeSvgContent } from '../../shared/svg-sanitizer';

  interface Props {
    source: string;
    layout?: string;
  }

  let { source, layout = 'auto' }: Props = $props();

  let svgContent = $state('');
  let error = $state('');
  let isRendering = $state(true);
  let scale = $state(1);
  let lastRenderKey = $state('');
  let mounted = $state(false);
  let renderToken = 0;

  const renderKey = $derived(`${source}\n::${layout}`);

  function resolveEngine(value: string): string {
    switch (value.trim().toLowerCase()) {
      case 'force':
      case 'cose':
        return 'neato';
      case 'circle':
        return 'circo';
      case 'grid':
      case 'preset':
      case 'dagre':
      case 'elk':
      case 'tidy-tree':
      case 'auto':
      default:
        return 'dot';
    }
  }

  async function renderDot(): Promise<void> {
    const trimmedSource = source.trim();
    if (!trimmedSource) {
      error = i18n.t('diagramRenderer.noSource');
      isRendering = false;
      svgContent = '';
      return;
    }

    const token = ++renderToken;
    isRendering = true;
    error = '';

    try {
      const viz = await vizPromise;
      if (token !== renderToken) return;
      const rawSvg = viz.renderString(trimmedSource, {
        format: 'svg',
        engine: resolveEngine(layout),
        graphAttributes: {
          bgcolor: 'transparent',
        },
      });
      if (token !== renderToken) return;
      const sanitized = sanitizeSvgContent(rawSvg);
      if (!sanitized) {
        throw new Error(i18n.t('diagramRenderer.renderFailed'));
      }
      svgContent = sanitized;
      lastRenderKey = renderKey;
      scale = 1;
    } catch (e) {
      if (token !== renderToken) return;
      svgContent = '';
      console.warn('[GraphvizRenderer] diagram render failed:', e);
      error = i18n.t('diagramRenderer.renderHint');
    } finally {
      if (token === renderToken) {
        isRendering = false;
      }
    }
  }

  $effect(() => {
    if (mounted && renderKey !== lastRenderKey) {
      void renderDot();
    }
  });

  onMount(() => {
    mounted = true;
    return () => {
      renderToken += 1;
    };
  });

  function zoomIn(): void {
    scale = Math.min(scale * 1.2, 6);
  }

  function zoomOut(): void {
    scale = Math.max(scale / 1.2, 0.35);
  }

  function fitView(): void {
    scale = 1;
  }

</script>

<div class="graphviz-renderer">
  <div class="graphviz-content">
    {#if isRendering}
      <div class="loading">
        <span class="spinner"></span>
        <span>{i18n.t('diagramRenderer.rendering')}</span>
      </div>
    {:else if error}
      <div class="error">
        <Icon name="alert-circle" size={20} />
        <span class="error-title">{i18n.t('diagramRenderer.renderFailed')}</span>
        <pre class="error-message">{error}</pre>
      </div>
    {:else}
      <div class="svg-wrapper" style="transform: scale({scale});">
        {@html svgContent}
      </div>
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
  .graphviz-renderer {
    overflow: hidden;
    background: transparent;
  }

  .control-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all 0.15s;
  }

  .control-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .graphviz-content {
    position: relative;
    min-height: 220px;
    max-height: 560px;
    overflow: auto;
    padding: var(--space-4, 16px);
    background: var(--code-bg);
  }

  .svg-wrapper {
    width: max-content;
    min-width: 100%;
    transform-origin: top left;
    transition: transform 0.08s ease-out;
  }

  .svg-wrapper :global(svg) {
    display: block;
    max-width: 100%;
    height: auto;
    margin: 0 auto;
  }

  .svg-wrapper :global(path),
  .svg-wrapper :global(polyline),
  .svg-wrapper :global(line) {
    stroke-width: 1.6px;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .svg-wrapper :global(.edge path),
  .svg-wrapper :global(.edge polygon),
  .svg-wrapper :global(.edge ellipse) {
    stroke: var(--diagram-connector, color-mix(in srgb, var(--foreground) 82%, var(--background) 18%)) !important;
  }

  .svg-wrapper :global(.edge polygon),
  .svg-wrapper :global(marker path),
  .svg-wrapper :global(marker polygon) {
    fill: var(--diagram-connector, color-mix(in srgb, var(--foreground) 82%, var(--background) 18%)) !important;
    stroke: var(--diagram-connector, color-mix(in srgb, var(--foreground) 82%, var(--background) 18%)) !important;
  }

  .svg-wrapper :global(.node text),
  .svg-wrapper :global(.edge text),
  .svg-wrapper :global(text) {
    fill: var(--foreground) !important;
  }

  .floating-controls {
    position: sticky;
    bottom: var(--space-3, 12px);
    left: var(--space-3, 12px);
    display: inline-flex;
    flex-direction: column;
    gap: var(--space-1, 4px);
  }

  .control-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
  }

  .loading,
  .error {
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-2, 8px);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
    text-align: center;
  }

  .spinner {
    width: 24px;
    height: 24px;
    border: 2px solid var(--border);
    border-top-color: var(--primary);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }

  .error {
    color: var(--error);
  }

  .error-title {
    font-weight: 500;
  }

  .error-message {
    max-width: 100%;
    margin: 0;
    padding: var(--space-2, 8px);
    overflow-x: auto;
    border-radius: var(--radius-sm);
    background: rgba(239, 68, 68, 0.1);
    color: var(--error);
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    white-space: pre-wrap;
  }

  @keyframes spin { to { transform: rotate(360deg); } }
</style>
