<script lang="ts">
  import { onMount } from 'svelte';
  import mermaid from 'mermaid';
  import Icon from './Icon.svelte';
  import { postMessage } from '../lib/vscode-bridge';
  import { i18n } from '../stores/i18n.svelte';
  import { sanitizeSvgContent } from '../shared/svg-sanitizer';

  // Props
  interface Props {
    code: string;
    title?: string;
    diagramType?: string;
    layout?: string;
    embedded?: boolean;
  }

  let { code, title = '', diagramType = '', layout = 'auto', embedded = false }: Props = $props();

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

  const normalizedCode = $derived(normalizeMermaidSource(code));
  const renderKey = $derived(`${normalizedCode}\n::${layout}\n::${activeThemeMode}`);

  // 从 mermaid 代码中提取标题
  const extractedTitle = $derived.by(() => {
    if (title) return title;
    if (!normalizedCode) return '';

    // 尝试匹配 YAML frontmatter 格式: ---\ntitle: xxx\n---
    const yamlMatch = normalizedCode.match(/^---\s*\n(?:.*\n)*?title:\s*(.+?)\n(?:.*\n)*?---/m);
    if (yamlMatch) return yamlMatch[1].trim();

    // 尝试匹配 accTitle 格式
    const accMatch = normalizedCode.match(/accTitle:\s*(.+?)(?:\n|$)/);
    if (accMatch) return accMatch[1].trim();

    // 尝试匹配 flowchart/graph 后的标题注释
    const commentMatch = normalizedCode.match(/(?:flowchart|graph|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|pie|mindmap|timeline).*?\n\s*%%\s*(.+?)(?:\n|$)/);
    if (commentMatch) return commentMatch[1].trim();

    // 思维导图：从根节点提取标题 root((xxx)) 或 root(xxx) 或 root[xxx] 或直接 root 后的文本
    const mindmapMatch = normalizedCode.match(/^\s*mindmap\s*\n\s*root\s*(?:\(\((.+?)\)\)|\((.+?)\)|\[(.+?)\]|(.+?)(?:\n|$))/m);
    if (mindmapMatch) {
      const rootText = mindmapMatch[1] || mindmapMatch[2] || mindmapMatch[3] || mindmapMatch[4];
      if (rootText) return rootText.trim();
    }

    // 流程图：从第一个节点提取标题
    const flowchartMatch = normalizedCode.match(/(?:flowchart|graph)\s+(?:TD|TB|BT|RL|LR)\s*\n\s*\w+\s*(?:\[\[(.+?)\]\]|\[(.+?)\]|\(\((.+?)\)\)|\((.+?)\)|\{(.+?)\})/m);
    if (flowchartMatch) {
      const nodeText = flowchartMatch[1] || flowchartMatch[2] || flowchartMatch[3] || flowchartMatch[4] || flowchartMatch[5];
      if (nodeText) return nodeText.trim();
    }

    return '';
  });

  // 检测图表类型
  const detectedType = $derived.by(() => {
    if (diagramType) return diagramType;
    if (!code) return '';

    const typePatterns: [RegExp, string][] = [
      [/^\s*flowchart/mi, 'flowchart'],
      [/^\s*graph/mi, 'flowchart'],
      [/^\s*sequenceDiagram/mi, 'sequence'],
      [/^\s*classDiagram/mi, 'class'],
      [/^\s*stateDiagram/mi, 'state'],
      [/^\s*erDiagram/mi, 'er'],
      [/^\s*gantt/mi, 'gantt'],
      [/^\s*pie/mi, 'pie'],
      [/^\s*journey/mi, 'journey'],
      [/^\s*gitGraph/mi, 'git'],
      [/^\s*mindmap/mi, 'mindmap'],
      [/^\s*timeline/mi, 'timeline'],
      [/^\s*quadrantChart/mi, 'quadrant'],
      [/^\s*requirementDiagram/mi, 'requirement'],
      [/^\s*C4Context/mi, 'c4'],
      [/^\s*sankey/mi, 'sankey'],
      [/^\s*xychart/mi, 'xychart'],
      [/^\s*block-beta/mi, 'block'],
    ];

    for (const [pattern, type] of typePatterns) {
      if (pattern.test(code)) return type;
    }
    return '';
  });

  // 图表类型显示名
  const typeDisplayName = $derived.by(() => {
    const typeMap: Record<string, string> = {
      flowchart: 'mermaidRenderer.diagramType.flowchart',
      sequence: 'mermaidRenderer.diagramType.sequence',
      class: 'mermaidRenderer.diagramType.class',
      state: 'mermaidRenderer.diagramType.state',
      er: 'mermaidRenderer.diagramType.er',
      gantt: 'mermaidRenderer.diagramType.gantt',
      pie: 'mermaidRenderer.diagramType.pie',
      journey: 'mermaidRenderer.diagramType.journey',
      git: 'mermaidRenderer.diagramType.git',
      mindmap: 'mermaidRenderer.diagramType.mindmap',
      timeline: 'mermaidRenderer.diagramType.timeline',
      quadrant: 'mermaidRenderer.diagramType.quadrant',
      requirement: 'mermaidRenderer.diagramType.requirement',
      c4: 'mermaidRenderer.diagramType.c4',
      sankey: 'mermaidRenderer.diagramType.sankey',
      xychart: 'mermaidRenderer.diagramType.xychart',
      block: 'mermaidRenderer.diagramType.block',
    };
    const key = typeMap[detectedType];
    return key ? i18n.t(key) : 'Mermaid';
  });

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
      error = i18n.t('mermaidRenderer.noCode');
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
        throw new Error(i18n.t('mermaidRenderer.renderFailed'));
      }
      svgContent = sanitizedSvg;
      lastRenderedKey = currentRenderKey;
      scale = 1;
      translateX = 0;
      translateY = 0;
    } catch (e) {
      if (token !== renderToken) return;
      console.error('[MermaidRenderer] 渲染错误:', e);
      error = e instanceof Error ? e.message : i18n.t('mermaidRenderer.renderFailed');
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

  // 复制 SVG
  async function copySvg() {
    if (svgContent) {
      try {
        await navigator.clipboard.writeText(svgContent);
      } catch (e) {
        console.error('复制失败:', e);
      }
    }
  }

  // 在新标签页打开
  function openInNewTab() {
    postMessage({
      type: 'openDiagramPanel',
      kind: 'mermaid',
      source: normalizedCode,
      svgContent,
      title: extractedTitle || typeDisplayName
    });
  }
</script>

<div class="mermaid-container" class:has-error={!!error} class:embedded>
  <!-- 头部 -->
  {#if !embedded}
    <div class="mermaid-header">
      <div class="header-left">
        <Icon name="git-branch" size={14} />
        <span class="header-type">{typeDisplayName}</span>
        {#if extractedTitle}
          <span class="header-title">{extractedTitle}</span>
        {/if}
      </div>
      <div class="header-actions">
        <button class="header-btn" onclick={copySvg} disabled={isRendering || !!error || !svgContent} title={i18n.t('mermaidRenderer.copySvg')}>
          <Icon name="copy" size={14} />
        </button>
        <button class="header-btn" onclick={openInNewTab} disabled={isRendering || !!error || !svgContent} title={i18n.t('mermaidRenderer.openInNewTab')}>
          <Icon name="external-link" size={14} />
        </button>
      </div>
    </div>
  {/if}

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
    aria-label={i18n.t('mermaidRenderer.ariaLabel')}
  >
    {#if isRendering}
      <div class="loading">
        <span class="spinner"></span>
        <span>{i18n.t('mermaidRenderer.rendering')}</span>
      </div>
    {:else if error}
      <div class="error">
        <Icon name="alert-circle" size={20} />
        <span class="error-title">{i18n.t('mermaidRenderer.renderFailed')}</span>
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
        <button class="control-btn" onclick={zoomIn} title={i18n.t('mermaidRenderer.zoomIn')}>
          <Icon name="plus" size={14} />
        </button>
        <button class="control-btn" onclick={zoomOut} title={i18n.t('mermaidRenderer.zoomOut')}>
          <Icon name="minus" size={14} />
        </button>
        <button class="control-btn" onclick={resetView} title={i18n.t('mermaidRenderer.resetView')}>
          <Icon name="refresh" size={14} />
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .mermaid-container {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
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
    background: var(--surface-1);
    margin: var(--space-2, 8px) 0;
  }

  .mermaid-container.embedded {
    border: none;
    border-radius: 0;
    margin: 0;
    background: transparent;
  }

  .mermaid-container.has-error {
    border-color: var(--error);
  }

  .mermaid-header {
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
    color: var(--info);
    overflow: hidden;
  }

  .header-type {
    font-size: var(--text-sm, 13px);
    font-weight: 500;
    flex-shrink: 0;
  }

  .header-title {
    font-size: var(--text-sm, 13px);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }

  .header-btn {
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

  .header-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .header-btn:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .header-btn:disabled:hover {
    background: transparent;
    color: var(--foreground-muted);
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
  .svg-wrapper :global(.mindmap-branch),
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
