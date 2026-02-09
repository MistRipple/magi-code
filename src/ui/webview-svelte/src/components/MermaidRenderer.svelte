<script lang="ts">
  import { onMount } from 'svelte';
  import mermaid from 'mermaid';
  import Icon from './Icon.svelte';
  import { postMessage } from '../lib/vscode-bridge';

  // Props
  interface Props {
    code: string;
    title?: string;
    diagramType?: string;
  }

  let { code, title = '', diagramType = '' }: Props = $props();

  // 状态
  let svgContent = $state('');
  let error = $state('');
  let isRendering = $state(true);
  let scale = $state(1);
  let translateX = $state(0);
  let translateY = $state(0);
  let lastRenderedCode = $state('');

  // 生成唯一 ID
  const getUniqueId = () => `mermaid-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;

  // 全局初始化标志
  let mermaidInitialized = false;

  // 从 mermaid 代码中提取标题
  const extractedTitle = $derived.by(() => {
    if (title) return title;
    if (!code) return '';

    // 尝试匹配 YAML frontmatter 格式: ---\ntitle: xxx\n---
    const yamlMatch = code.match(/^---\s*\n(?:.*\n)*?title:\s*(.+?)\n(?:.*\n)*?---/m);
    if (yamlMatch) return yamlMatch[1].trim();

    // 尝试匹配 accTitle 格式
    const accMatch = code.match(/accTitle:\s*(.+?)(?:\n|$)/);
    if (accMatch) return accMatch[1].trim();

    // 尝试匹配 flowchart/graph 后的标题注释
    const commentMatch = code.match(/(?:flowchart|graph|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|pie|mindmap|timeline).*?\n\s*%%\s*(.+?)(?:\n|$)/);
    if (commentMatch) return commentMatch[1].trim();

    // 思维导图：从根节点提取标题 root((xxx)) 或 root(xxx) 或 root[xxx] 或直接 root 后的文本
    const mindmapMatch = code.match(/^\s*mindmap\s*\n\s*root\s*(?:\(\((.+?)\)\)|\((.+?)\)|\[(.+?)\]|(.+?)(?:\n|$))/m);
    if (mindmapMatch) {
      const rootText = mindmapMatch[1] || mindmapMatch[2] || mindmapMatch[3] || mindmapMatch[4];
      if (rootText) return rootText.trim();
    }

    // 流程图：从第一个节点提取标题
    const flowchartMatch = code.match(/(?:flowchart|graph)\s+(?:TD|TB|BT|RL|LR)\s*\n\s*\w+\s*(?:\[\[(.+?)\]\]|\[(.+?)\]|\(\((.+?)\)\)|\((.+?)\)|\{(.+?)\})/m);
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
    ];

    for (const [pattern, type] of typePatterns) {
      if (pattern.test(code)) return type;
    }
    return '';
  });

  // 图表类型显示名
  const typeDisplayName = $derived.by(() => {
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
      timeline: '时间线',
    };
    return typeMap[detectedType] || 'Mermaid';
  });

  onMount(() => {
    console.log('[MermaidRenderer] mounted, code:', code?.substring(0, 50));

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

  // 重新渲染
  $effect(() => {
    if (code && code !== lastRenderedCode && mermaidInitialized) {
      doRender();
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
      type: 'openMermaidPanel',
      code: code,
      title: extractedTitle || typeDisplayName
    });
  }
</script>

<div class="mermaid-container" class:has-error={!!error}>
  <!-- 头部 -->
  <div class="mermaid-header">
    <div class="header-left">
      <Icon name="git-branch" size={14} />
      <span class="header-type">{typeDisplayName}</span>
      {#if extractedTitle}
        <span class="header-title">{extractedTitle}</span>
      {/if}
    </div>
    <div class="header-actions">
      <button class="header-btn" onclick={copySvg} title="复制 SVG">
        <Icon name="copy" size={14} />
      </button>
      <button class="header-btn" onclick={openInNewTab} title="在新标签页打开">
        <Icon name="external-link" size={14} />
      </button>
    </div>
  </div>

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
    aria-label="Interactive Mermaid diagram"
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
        <button class="control-btn" onclick={zoomIn} title="放大">
          <Icon name="plus" size={14} />
        </button>
        <button class="control-btn" onclick={zoomOut} title="缩小">
          <Icon name="minus" size={14} />
        </button>
        <button class="control-btn" onclick={resetView} title="重置视图">
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
    background: var(--surface-1, rgba(255,255,255,0.02));
    margin: var(--space-2, 8px) 0;
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

  .mermaid-content {
    position: relative;
    min-height: 200px;
    max-height: 500px;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-4, 16px);
    background: var(--code-bg, rgba(0,0,0,0.2));
    cursor: grab;
  }

  .mermaid-content.dragging {
    cursor: grabbing;
  }

  .svg-wrapper {
    transform-origin: center center;
    transition: transform 0.05s ease-out;
    user-select: none;
  }

  .mermaid-content.dragging .svg-wrapper {
    transition: none;
  }

  .svg-wrapper :global(svg) {
    max-width: none;
    height: auto;
    display: block;
  }

  /* Mermaid SVG 样式 */
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
    background: var(--surface-2, rgba(0,0,0,0.4));
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
