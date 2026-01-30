<script lang="ts">
  import { onMount } from 'svelte';
  import mermaid from 'mermaid';
  import Icon from './Icon.svelte';
  import { postMessage } from '../lib/vscode-bridge';

  // Props
  interface Props {
    code: string;
    title?: string;
    theme?: 'default' | 'dark' | 'forest' | 'neutral';
    diagramType?: string;
  }

  let { code, title = '', theme = 'dark', diagramType = '' }: Props = $props();

  // 状态
  let containerRef: HTMLDivElement | null = $state(null);
  let svgContent = $state('');
  let error = $state('');
  let isRendering = $state(true);
  let isZoomed = $state(false);
  let scale = $state(1);
  let translateX = $state(0);
  let translateY = $state(0);
  let lastRenderedCode = $state(''); // 组件级别的上次渲染代码

  // 生成唯一 ID（每次渲染时更新）
  const getUniqueId = () => `mermaid-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;

  // 全局初始化标志（mermaid.initialize 只需调用一次）
  let mermaidInitialized = false;

  onMount(() => {
    console.log('[MermaidRenderer] mounted, code:', code?.substring(0, 50));

    // 全局初始化 mermaid（只执行一次）
    if (!mermaidInitialized) {
      mermaid.initialize({
        startOnLoad: false,
        theme: 'dark',
        securityLevel: 'loose',
        fontFamily: 'ui-sans-serif, system-ui, sans-serif',
        themeVariables: {
          darkMode: true,
          background: '#1e1e1e',
          primaryColor: '#4a9eff',
          primaryTextColor: '#ffffff',
          primaryBorderColor: '#4a9eff',
          lineColor: '#888888',
          secondaryColor: '#2d5a8a',
          tertiaryColor: '#1a3a5c',
          textColor: '#e0e0e0',
          mainBkg: '#2d2d2d',
          nodeBorder: '#4a9eff',
          clusterBkg: '#1a1a1a',
          clusterBorder: '#4a9eff',
          titleColor: '#ffffff',
          edgeLabelBackground: '#2d2d2d',
        },
        flowchart: {
          htmlLabels: true,
          curve: 'basis',
          nodeSpacing: 50,
          rankSpacing: 50,
        },
        sequence: {
          diagramMarginX: 20,
          diagramMarginY: 20,
          actorMargin: 50,
          width: 150,
          height: 65,
        },
      });
      mermaidInitialized = true;
    }

    // 初始渲染
    doRender();
  });

  // 渲染图表
  async function doRender() {
    console.log('[MermaidRenderer] doRender called, code length:', code?.length);
    if (!code) {
      error = '没有提供 Mermaid 代码';
      isRendering = false;
      return;
    }

    try {
      isRendering = true;
      error = '';

      // 每次渲染使用新的 ID，避免 Mermaid 缓存问题
      const diagramId = getUniqueId();
      console.log('[MermaidRenderer] calling mermaid.render with id:', diagramId);
      const { svg } = await mermaid.render(diagramId, code.trim());
      console.log('[MermaidRenderer] render success, svg length:', svg?.length);
      svgContent = svg;
      lastRenderedCode = code;
    } catch (e) {
      console.error('[MermaidRenderer] 渲染错误:', e);
      error = e instanceof Error ? e.message : '渲染失败';
    } finally {
      isRendering = false;
    }
  }

  // 重新渲染（当 code 变化时）
  $effect(() => {
    // 只有当 code 确实变化时才重新渲染（跳过初始渲染，由 onMount 处理）
    if (code && code !== lastRenderedCode && mermaidInitialized) {
      doRender();
    }
  });

  // 缩放控制
  function zoomIn() {
    console.log('[MermaidRenderer] zoomIn, current scale:', scale);
    scale = Math.min(scale * 1.2, 3);
    console.log('[MermaidRenderer] zoomIn, new scale:', scale);
  }

  function zoomOut() {
    console.log('[MermaidRenderer] zoomOut, current scale:', scale);
    scale = Math.max(scale / 1.2, 0.5);
    console.log('[MermaidRenderer] zoomOut, new scale:', scale);
  }

  function resetZoom() {
    scale = 1;
    translateX = 0;
    translateY = 0;
  }

  function toggleFullscreen() {
    isZoomed = !isZoomed;
    if (!isZoomed) {
      resetZoom();
    }
  }

  // 拖拽控制
  let isDragging = $state(false);
  let startX = 0;
  let startY = 0;

  function handleMouseDown(e: MouseEvent) {
    if (scale > 1) {
      isDragging = true;
      startX = e.clientX - translateX;
      startY = e.clientY - translateY;
    }
  }

  function handleMouseMove(e: MouseEvent) {
    if (isDragging) {
      translateX = e.clientX - startX;
      translateY = e.clientY - startY;
    }
  }

  function handleMouseUp() {
    isDragging = false;
  }

  // 获取图表类型显示名
  function getDiagramTypeName(type: string): string {
    const typeMap: Record<string, string> = {
      flowchart: '流程图',
      sequence: '时序图',
      class: '类图',
      state: '状态图',
      er: 'ER 图',
      gantt: '甘特图',
      pie: '饼图',
      journey: '用户旅程',
      git: 'Git 图',
      mindmap: '思维导图',
    };
    return typeMap[type] || type || '图表';
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

  // 下载 SVG
  function downloadSvg() {
    if (svgContent) {
      const blob = new Blob([svgContent], { type: 'image/svg+xml' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${title || 'diagram'}.svg`;
      a.click();
      URL.revokeObjectURL(url);
    }
  }

  // 在新标签页打开
  function openInNewTab() {
    // 通过 VSCode API 发送消息到扩展
    postMessage({
      type: 'openMermaidPanel',
      code: code,
      title: title || getDiagramTypeName(diagramType)
    });
  }
</script>

<div class="mermaid-container" class:zoomed={isZoomed} class:has-error={!!error}>
  <!-- 头部 -->
  <div class="mermaid-header">
    <div class="header-left">
      <Icon name="git-branch" size={14} />
      <span class="diagram-type">{getDiagramTypeName(diagramType)}</span>
      {#if title}
        <span class="diagram-title">{title}</span>
      {/if}
    </div>
    <div class="header-actions">
      <button class="action-btn" onclick={zoomOut} title="缩小">
        <Icon name="minus" size={14} />
      </button>
      <span class="zoom-level">{Math.round(scale * 100)}%</span>
      <button class="action-btn" onclick={zoomIn} title="放大">
        <Icon name="plus" size={14} />
      </button>
      <button class="action-btn" onclick={resetZoom} title="重置">
        <Icon name="refresh" size={14} />
      </button>
      <button class="action-btn" onclick={toggleFullscreen} title={isZoomed ? '退出全屏' : '全屏'}>
        <Icon name={isZoomed ? 'minimize' : 'maximize'} size={14} />
      </button>
      <button class="action-btn" onclick={copySvg} title="复制 SVG">
        <Icon name="copy" size={14} />
      </button>
      <button class="action-btn" onclick={downloadSvg} title="下载 SVG">
        <Icon name="download" size={14} />
      </button>
      <button class="action-btn" onclick={openInNewTab} title="在新标签页打开">
        <Icon name="external-link" size={14} />
      </button>
    </div>
  </div>

  <!-- 图表区域 -->
  <div
    class="mermaid-content"
    bind:this={containerRef}
    onmousedown={handleMouseDown}
    onmousemove={handleMouseMove}
    onmouseup={handleMouseUp}
    onmouseleave={handleMouseUp}
    class:dragging={isDragging}
  >
    {#if isRendering}
      <div class="loading">
        <span class="spinner"></span>
        <span>渲染中...</span>
      </div>
    {:else if error}
      <div class="error">
        <Icon name="alert-circle" size={20} />
        <span class="error-title">渲染失败</span>
        <pre class="error-message">{error}</pre>
        <pre class="error-code">{code}</pre>
      </div>
    {:else}
      <div
        class="svg-wrapper"
        style="transform: scale({scale}) translate({translateX / scale}px, {translateY / scale}px);"
      >
        {@html svgContent}
      </div>
    {/if}
  </div>
</div>

<style>
  .mermaid-container {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
    background: var(--surface-1, rgba(255,255,255,0.02));
    margin: var(--space-2, 8px) 0;
  }

  .mermaid-container.zoomed {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 1000;
    border-radius: 0;
    margin: 0;
    background: var(--background);
  }

  .mermaid-container.has-error {
    border-color: var(--error);
  }

  .mermaid-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-2, 8px) var(--space-3, 12px);
    background: var(--surface-2, rgba(0,0,0,0.1));
    border-bottom: 1px solid var(--border);
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    color: var(--info);
  }

  .diagram-type {
    font-size: var(--text-sm, 13px);
    font-weight: 500;
  }

  .diagram-title {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    opacity: 0.8;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .action-btn {
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
    transition: all var(--transition-fast);
  }

  .action-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .zoom-level {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    min-width: 40px;
    text-align: center;
  }

  .mermaid-content {
    position: relative;
    min-height: 200px;
    max-height: 500px;
    overflow: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-4, 16px);
    background: var(--code-bg, rgba(0,0,0,0.2));
  }

  .zoomed .mermaid-content {
    max-height: none;
    height: calc(100vh - 50px);
  }

  .mermaid-content.dragging {
    cursor: grabbing;
  }

  .svg-wrapper {
    transform-origin: center center;
    transition: transform 0.1s ease-out;
  }

  .svg-wrapper :global(svg) {
    max-width: 100%;
    height: auto;
    display: block;
  }

  /* 确保 Mermaid SVG 内容可见 */
  .svg-wrapper :global(.node rect),
  .svg-wrapper :global(.node circle),
  .svg-wrapper :global(.node ellipse),
  .svg-wrapper :global(.node polygon),
  .svg-wrapper :global(.node path) {
    fill: #2d2d2d;
    stroke: #4a9eff;
  }

  .svg-wrapper :global(.node .label),
  .svg-wrapper :global(.nodeLabel),
  .svg-wrapper :global(.label text),
  .svg-wrapper :global(text) {
    fill: #e0e0e0 !important;
    color: #e0e0e0 !important;
  }

  .svg-wrapper :global(.edgePath path),
  .svg-wrapper :global(.flowchart-link) {
    stroke: #888888;
  }

  .svg-wrapper :global(.edgeLabel),
  .svg-wrapper :global(.edgeLabel text) {
    fill: #e0e0e0;
    background-color: #2d2d2d;
  }

  .svg-wrapper :global(.cluster rect) {
    fill: #1a1a1a;
    stroke: #4a9eff;
  }

  .svg-wrapper :global(marker path) {
    fill: #888888;
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

  .error-code {
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    background: var(--surface-2);
    padding: var(--space-2, 8px);
    border-radius: var(--radius-sm);
    max-width: 100%;
    overflow-x: auto;
    margin: 0;
    color: var(--foreground-muted);
  }
</style>
