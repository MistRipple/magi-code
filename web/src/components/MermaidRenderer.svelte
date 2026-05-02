<script lang="ts">
  import { onMount } from 'svelte';
  import mermaid from 'mermaid';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { sanitizeSvgContent } from '../shared/svg-sanitizer';

  // Props
  interface Props {
    code: string;
    layout?: string;
  }

  let { code, layout = 'auto' }: Props = $props();

  // 状态
  let svgContent = $state('');
  let error = $state('');
  let isRendering = $state(true);
  let scale = $state(1);
  let translateX = $state(0);
  let translateY = $state(0);
  let lastRenderedKey = $state('');
  let activeThemeMode = $state<'light' | 'dark'>('dark');
  let mermaidReady = $state(false);
  let renderToken = 0;

  // 生成唯一 ID
  const getUniqueId = () => `mermaid-${Date.now()}-${Math.random().toString(36).substring(2, 11)}`;

  function normalizeMermaidSource(source: string): string {
    let normalized = source.trim();
    if (!normalized) return '';
    let previous = '';
    while (previous !== normalized) {
      previous = normalized;
      normalized = normalized.replace(/([A-Za-z0-9_\]\})\u4e00-\u9fff])\s*[.。．]\s*$/u, '$1');
    }
    return normalized;
  }

  function isUnsupportedMindmapSource(source: string): boolean {
    return /^\s*mindmap(?:\s|\n|$)/i.test(source);
  }

  const normalizedCode = $derived(normalizeMermaidSource(code));
  const renderKey = $derived(`${normalizedCode}\n::${layout}\n::${activeThemeMode}`);

  function resolveThemeMode(): 'light' | 'dark' {
    if (typeof document === 'undefined') {
      return 'dark';
    }
    const classNames = [
      ...Array.from(document.documentElement.classList),
      ...(document.body ? Array.from(document.body.classList) : []),
    ];
    if (classNames.includes('theme-light') || classNames.includes('vscode-light')) {
      return 'light';
    }
    return 'dark';
  }

  function readThemeToken(name: string, fallback: string): string {
    if (typeof window === 'undefined') {
      return fallback;
    }
    const rootStyles = window.getComputedStyle(document.documentElement);
    const bodyStyles = document.body ? window.getComputedStyle(document.body) : null;
    return bodyStyles?.getPropertyValue(name).trim()
      || rootStyles.getPropertyValue(name).trim()
      || fallback;
  }

  function initializeMermaid(mode: 'light' | 'dark'): void {
    const background = readThemeToken('--background', mode === 'light' ? '#ffffff' : '#11161d');
    const surface1 = readThemeToken('--surface-1', mode === 'light' ? 'rgba(0, 0, 0, 0.02)' : 'rgba(255, 255, 255, 0.02)');
    const surface2 = readThemeToken('--surface-2', mode === 'light' ? 'rgba(0, 0, 0, 0.04)' : 'rgba(255, 255, 255, 0.04)');
    const foreground = readThemeToken('--foreground', mode === 'light' ? '#1f2937' : '#e5e7eb');
    const primary = readThemeToken('--primary', '#2563eb');
    const connector = readThemeToken('--diagram-connector', mode === 'light' ? '#475569' : '#cbd5e1');
    const flowchartRenderer = layout === 'elk'
      ? 'elk'
      : layout === 'dagre'
        ? 'dagre-wrapper'
        : undefined;

    mermaid.initialize({
      startOnLoad: false,
      theme: 'base',
      securityLevel: 'strict',
      fontFamily: 'ui-sans-serif, system-ui, sans-serif',
      themeVariables: {
        darkMode: mode === 'dark',
        background,
        primaryColor: surface2,
        primaryTextColor: foreground,
        primaryBorderColor: primary,
        lineColor: connector,
        secondaryColor: surface2,
        tertiaryColor: surface1,
        textColor: foreground,
        mainBkg: surface2,
        nodeBorder: primary,
        clusterBkg: surface1,
        clusterBorder: primary,
        titleColor: foreground,
        edgeLabelBackground: background,
        actorBkg: surface2,
        actorBorder: primary,
        actorTextColor: foreground,
        noteBkgColor: surface2,
        noteBorderColor: primary,
        noteTextColor: foreground,
        labelBoxBkgColor: surface2,
        labelBoxBorderColor: primary,
        signalColor: connector,
        signalTextColor: foreground,
      },
      flowchart: {
        htmlLabels: false,
        curve: 'basis',
        nodeSpacing: 50,
        rankSpacing: 50,
        ...(flowchartRenderer ? { defaultRenderer: flowchartRenderer } : {}),
      },
      sequence: {
        diagramMarginX: 20,
        diagramMarginY: 20,
        actorMargin: 50,
        width: 150,
        height: 65,
      },
    });
    activeThemeMode = mode;
    mermaidReady = true;
  }

  onMount(() => {
    initializeMermaid(resolveThemeMode());

    const observer = new MutationObserver(() => {
      const nextMode = resolveThemeMode();
      if (nextMode === activeThemeMode) {
        return;
      }
      initializeMermaid(nextMode);
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
    };
  });

  // 渲染图表
  async function doRender() {
    const token = ++renderToken;
    const currentRenderKey = renderKey;
    const currentCode = normalizedCode;
    isRendering = true;
    error = '';

    if (!currentCode) {
      error = i18n.t('diagramRenderer.noSource');
      isRendering = false;
      svgContent = '';
      lastRenderedKey = currentRenderKey;
      return;
    }
    if (isUnsupportedMindmapSource(currentCode)) {
      error = i18n.t('diagramRenderer.unsupportedMindmap');
      isRendering = false;
      svgContent = '';
      lastRenderedKey = currentRenderKey;
      return;
    }

    try {
      const diagramId = getUniqueId();
      const { svg } = await mermaid.render(diagramId, currentCode);
      if (token !== renderToken) return;
      const sanitizedSvg = sanitizeSvgContent(svg);
      if (!sanitizedSvg) {
        throw new Error(i18n.t('diagramRenderer.renderFailed'));
      }
      svgContent = sanitizedSvg;
      lastRenderedKey = currentRenderKey;
      scale = 1;
      translateX = 0;
      translateY = 0;
    } catch (e) {
      if (token !== renderToken) return;
      console.error('[DiagramRenderer] 渲染错误:', e);
      error = e instanceof Error ? e.message : i18n.t('diagramRenderer.renderFailed');
      svgContent = '';
      lastRenderedKey = currentRenderKey;
    } finally {
      if (token === renderToken) {
        isRendering = false;
      }
    }
  }

  // 重新渲染
  $effect(() => {
    if (mermaidReady && renderKey !== lastRenderedKey) {
      void doRender();
    }
  });

  // 缩放控制
  function zoomIn() {
    scale = Math.min(scale * 1.2, 10);
  }

  function zoomOut() {
    scale = Math.max(scale / 1.2, 0.3);
  }

  function resetView() {
    scale = 1;
    translateX = 0;
    translateY = 0;
  }

  // 拖拽控制
  let isDragging = $state(false);
  let dragStartX = 0;
  let dragStartY = 0;
  let initialTranslateX = 0;
  let initialTranslateY = 0;

  function handleMouseDown(e: MouseEvent) {
    isDragging = true;
    dragStartX = e.clientX;
    dragStartY = e.clientY;
    initialTranslateX = translateX;
    initialTranslateY = translateY;
    e.preventDefault();
  }

  function handleMouseMove(e: MouseEvent) {
    if (isDragging) {
      translateX = initialTranslateX + (e.clientX - dragStartX);
      translateY = initialTranslateY + (e.clientY - dragStartY);
    }
  }

  function handleMouseUp() {
    isDragging = false;
  }

</script>

<div class="mermaid-renderer">
  <!-- 图表区域 -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="mermaid-content"
    onmousedown={handleMouseDown}
    onmousemove={handleMouseMove}
    onmouseup={handleMouseUp}
    onmouseleave={handleMouseUp}
    class:dragging={isDragging}
    role="application"
    aria-label={i18n.t('diagramRenderer.ariaLabel')}
  >
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
      <div
        class="svg-wrapper"
        style="transform: translate({translateX}px, {translateY}px) scale({scale});"
      >
        {@html svgContent}
      </div>
    {/if}

    <!-- 浮动控制按钮（Augment 风格） -->
    {#if !isRendering && !error}
      <div class="floating-controls">
        <button class="control-btn" onclick={zoomIn} title={i18n.t('diagramRenderer.zoomIn')}>
          <Icon name="plus" size={14} />
        </button>
        <button class="control-btn" onclick={zoomOut} title={i18n.t('diagramRenderer.zoomOut')}>
          <Icon name="minus" size={14} />
        </button>
        <button class="control-btn" onclick={resetView} title={i18n.t('diagramRenderer.resetView')}>
          <Icon name="refresh" size={14} />
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .mermaid-renderer {
    --mermaid-node-bg: var(--surface-2);
    --mermaid-node-border: var(--primary);
    --mermaid-text: var(--foreground);
    --mermaid-line: var(
      --diagram-connector,
      color-mix(in srgb, var(--foreground) 82%, var(--background) 18%)
    );
    --mermaid-line-soft: color-mix(in srgb, var(--mermaid-line) 70%, transparent);
    --mermaid-line-width: 2px;
    --mermaid-cluster-bg: var(--surface-1);
    --mermaid-edge-label-bg: var(--background);
  }

  .mermaid-content {
    position: relative;
    min-height: 200px;
    max-height: min(640px, 62vh);
    overflow: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-4, 16px);
    background: var(--code-bg);
    cursor: grab;
  }

  .mermaid-content.dragging {
    cursor: grabbing;
  }

  .svg-wrapper {
    max-width: 100%;
    max-height: 100%;
    transform-origin: center center;
    transition: transform 0.05s ease-out;
    user-select: none;
  }

  .mermaid-content.dragging .svg-wrapper {
    transition: none;
  }

  .svg-wrapper :global(svg) {
    max-width: 100%;
    max-height: calc(min(640px, 62vh) - 32px);
    height: auto;
    display: block;
  }

  /* Mermaid SVG 样式 */
  .svg-wrapper :global(.node rect),
  .svg-wrapper :global(.node circle),
  .svg-wrapper :global(.node ellipse),
  .svg-wrapper :global(.node polygon),
  .svg-wrapper :global(.node path) {
    fill: var(--mermaid-node-bg) !important;
    stroke: var(--mermaid-node-border) !important;
  }

  .svg-wrapper :global(.node .label),
  .svg-wrapper :global(.nodeLabel),
  .svg-wrapper :global(.label text),
  .svg-wrapper :global(text) {
    fill: var(--mermaid-text) !important;
    color: var(--mermaid-text) !important;
  }

  .svg-wrapper :global(.edgePath path),
  .svg-wrapper :global(.flowchart-link),
  .svg-wrapper :global(.messageLine0),
  .svg-wrapper :global(.messageLine1),
  .svg-wrapper :global(path[marker-end]),
  .svg-wrapper :global(polyline),
  .svg-wrapper :global(line),
  .svg-wrapper :global(.transition),
  .svg-wrapper :global(.relation),
  .svg-wrapper :global(.relationshipLine),
  .svg-wrapper :global(.branch),
  .svg-wrapper :global(path[class*="edge"]),
  .svg-wrapper :global(path[class*="relationship"]),
  .svg-wrapper :global(path[class*="transition"]),
  .svg-wrapper :global(line[class*="messageLine"]) {
    stroke: var(--mermaid-line) !important;
    stroke-width: var(--mermaid-line-width) !important;
    stroke-opacity: 1 !important;
    stroke-linecap: round;
    stroke-linejoin: round;
    vector-effect: non-scaling-stroke;
  }

  .svg-wrapper :global(.actor-line),
  .svg-wrapper :global(.loopLine),
  .svg-wrapper :global(.activation0),
  .svg-wrapper :global(.activation1),
  .svg-wrapper :global(.activation2) {
    stroke: var(--mermaid-line-soft) !important;
    stroke-width: 1.5px !important;
    stroke-opacity: 1 !important;
    vector-effect: non-scaling-stroke;
  }

  .svg-wrapper :global(.edgeLabel),
  .svg-wrapper :global(.edgeLabel text) {
    fill: var(--mermaid-text) !important;
    background-color: var(--mermaid-edge-label-bg) !important;
  }

  .svg-wrapper :global(.cluster rect) {
    fill: var(--mermaid-cluster-bg) !important;
    stroke: var(--mermaid-node-border) !important;
  }

  .svg-wrapper :global(marker path),
  .svg-wrapper :global(marker polygon) {
    fill: var(--mermaid-line) !important;
    stroke: var(--mermaid-line) !important;
    stroke-opacity: 1 !important;
  }

  /* 浮动控制按钮（Augment 风格，左下角垂直排列） */
  .floating-controls {
    position: absolute;
    bottom: var(--space-3, 12px);
    left: var(--space-3, 12px);
    display: flex;
    flex-direction: column;
    gap: var(--space-1, 4px);
  }

  .control-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    background: var(--surface-2);
    backdrop-filter: blur(8px);
    border: 1px solid var(--border);
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all 0.15s;
  }

  .control-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
    border-color: var(--primary);
  }

  .loading {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2, 8px);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
  }

  .spinner {
    width: 24px;
    height: 24px;
    border: 2px solid var(--border);
    border-top-color: var(--primary);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }

  @keyframes spin { to { transform: rotate(360deg); } }

  .error {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2, 8px);
    padding: var(--space-4, 16px);
    color: var(--error);
    text-align: center;
  }

  .error-title {
    font-weight: 500;
    font-size: var(--text-sm, 13px);
  }

  .error-message {
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    background: rgba(239, 68, 68, 0.1);
    padding: var(--space-2, 8px);
    border-radius: var(--radius-sm);
    max-width: 100%;
    overflow-x: auto;
    margin: 0;
  }
</style>
