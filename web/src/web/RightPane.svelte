<script lang="ts">
  import hljs from 'highlight.js';
  import { onMount } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import MarkdownContent from '../components/MarkdownContent.svelte';
  import AgentTabContent from '../components/tabs/AgentTabContent.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    isKnownBinaryFile,
    isMarkdownFile,
    isWordFile,
    isImageFile,
  } from '../lib/file-preview-utils';
  import {
    rightPaneState,
    getRightPaneState,
    closeTab,
    setActiveRightPaneTab,
    toggleRightPane,
    type RightPaneTab,
    type CodeTabPayload,
    type AgentTabPayload,
  } from '../stores/right-pane.svelte';
  import { getAgentFilePreview, agentUrl, buildFilePreviewQuery } from './agent-api';

  interface Props {
    workspaceRoot: string;
  }

  let { workspaceRoot }: Props = $props();

  // ============ Tab 状态 ============
  const sessionId = $derived(rightPaneState.activeSessionId);
  const paneState = $derived(getRightPaneState(sessionId));
  const openTabs = $derived(paneState.openTabs);
  const activeTab = $derived.by<RightPaneTab | null>(() => {
    if (!paneState.activeTabId) return null;
    return openTabs.find((tab) => tab.id === paneState.activeTabId) ?? null;
  });

  // ============ Code tab：内容拉取 ============
  /** filepath → 异步加载的源码内容（仅用于补全 store 中没有 content 时） */
  let fetchedContents = $state<Record<string, string>>({});
  /** filepath → loading 标记 */
  let fetchingFlags = $state<Record<string, boolean>>({});
  /** filepath → 拉取出错时的错误信息 */
  let fetchErrors = $state<Record<string, string>>({});

  // 工作区内容变更（如切分支）后，清空已拉取的文件内容缓存，触发 $effect 按新分支重新拉取。
  onMount(() => {
    const handleWorkspaceContentChanged = () => {
      fetchedContents = {};
      fetchErrors = {};
      fetchingFlags = {};
    };
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    return () => window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
  });

  const activeCodePayload = $derived.by<CodeTabPayload | null>(() => {
    if (!activeTab || activeTab.kind !== 'code') return null;
    return activeTab.payload as CodeTabPayload;
  });

  const activeFilePath = $derived(activeCodePayload?.filepath ?? '');
  const activeContentCacheKey = $derived.by(() => {
    if (!activeFilePath) return '';
    return [
      activeCodePayload?.workspaceId ?? '',
      activeCodePayload?.workspacePath ?? '',
      activeCodePayload?.sessionId ?? '',
      activeFilePath,
    ].join('::');
  });
  const activeContentKind = $derived(activeCodePayload?.contentKind ?? 'text');
  const explicitContent = $derived(activeCodePayload?.content ?? null);
  const explicitDiff = $derived(activeCodePayload?.diff ?? null);
  const activeFilePreviewQuery = $derived.by(() => {
    if (!activeFilePath) return '';
    return buildFilePreviewQuery(activeFilePath, {
      includeSession: false,
      sessionId: activeCodePayload?.sessionId,
      workspaceId: activeCodePayload?.workspaceId,
      workspacePath: activeCodePayload?.workspacePath,
    });
  });

  /**
   * 是否需要异步拉取内容：text 类型、未带 content、未带 diff、且非二进制/word 文件。
   * 触发条件统一在 $effect 里检查，避免重复请求。
   */
  $effect(() => {
    const filepath = activeFilePath;
    const cacheKey = activeContentCacheKey;
    if (!filepath) return;
    if (!cacheKey) return;
    if (typeof explicitContent === 'string') return; // 已经有内容
    if (typeof explicitDiff === 'string' && explicitDiff.trim().length > 0) return; // diff 模式
    if (activeContentKind !== 'text') return; // 非文本类不拉取
    if (isWordFile(filepath) || isKnownBinaryFile(filepath)) return;
    if (typeof fetchedContents[cacheKey] === 'string') return; // 已成功拉过
    if (typeof fetchErrors[cacheKey] === 'string' && fetchErrors[cacheKey].length > 0) return; // 已失败过，停止重试避免死循环
    if (fetchingFlags[cacheKey]) return; // 拉取中

    fetchingFlags = { ...fetchingFlags, [cacheKey]: true };
    fetchErrors = { ...fetchErrors, [cacheKey]: '' };
    (async () => {
      try {
        const payload = await getAgentFilePreview(filepath, {
          includeSession: false,
          sessionId: activeCodePayload?.sessionId,
          workspaceId: activeCodePayload?.workspaceId,
          workspacePath: activeCodePayload?.workspacePath,
        });
        fetchedContents = { ...fetchedContents, [cacheKey]: payload.content || '' };
      } catch (error) {
        console.warn('[RightPane] file preview load failed:', error);
        fetchErrors = { ...fetchErrors, [cacheKey]: i18n.t('web.filePreviewError') };
      } finally {
        fetchingFlags = { ...fetchingFlags, [cacheKey]: false };
      }
    })();
  });

  const previewLoading = $derived(Boolean(activeContentCacheKey && fetchingFlags[activeContentCacheKey]));
  const previewError = $derived(activeContentCacheKey ? (fetchErrors[activeContentCacheKey] || '') : '');
  /** 最终用于渲染的内容：优先 store 显式 content，其次异步拉取结果 */
  const previewContent = $derived.by<string | null>(() => {
    if (typeof explicitContent === 'string') return explicitContent;
    if (!activeContentCacheKey) return null;
    return fetchedContents[activeContentCacheKey] ?? null;
  });

  // ============ Diff 渲染（hunk-aware：单列行号 + 类代码块外观） ============
  interface DiffLine {
    type: 'add' | 'delete' | 'context';
    content: string;
    /**
     * 用于左侧 gutter 的显示行号：
     * - context / add：取 new 文件行号
     * - delete：取 old 文件行号
     * 这样所有行号都在同一列对齐，避免双列错开；context 行只显示一个行号。
     */
    displayLine: number;
  }

  interface DiffHunk {
    /** raw hunk header (如 "@@ -1,7 +1,8 @@ ..."); 用于 tooltip / 显示 */
    header: string;
    /** 解析自 header 的旧文件起始行 */
    oldStart: number;
    /** 解析自 header 的新文件起始行 */
    newStart: number;
    lines: DiffLine[];
  }

  const EXT_LANG_MAP: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
    py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java',
    cpp: 'cpp', c: 'c', cs: 'csharp', kt: 'kotlin', swift: 'swift',
    html: 'xml', vue: 'xml', svelte: 'xml', xml: 'xml', svg: 'xml',
    css: 'css', scss: 'scss', less: 'less',
    json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini',
    md: 'markdown', sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql', graphql: 'graphql', dockerfile: 'dockerfile',
  };

  const diffLanguage = $derived.by(() => {
    if (!activeFilePath) return '';
    const ext = activeFilePath.split('.').pop()?.toLowerCase() ?? '';
    return EXT_LANG_MAP[ext] ?? '';
  });

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  /**
   * Unified diff 解析：按 @@ hunk 边界切分，逐行推算 old/new 行号。
   * - 文件头（---, +++）忽略
   * - @@ -a,b +c,d @@ 头解析出起始行号；缺省 b/d 时视为 1
   * - 每个 hunk 内：context 同步递增两侧；'-' 仅递增 old；'+' 仅递增 new
   */
  const diffHunks = $derived.by<DiffHunk[]>(() => {
    if (typeof explicitDiff !== 'string' || explicitDiff.trim().length === 0) return [];
    const hunkHeaderRe = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;
    const hunks: DiffHunk[] = [];
    let current: DiffHunk | null = null;
    let oldCursor = 0;
    let newCursor = 0;
    for (const raw of explicitDiff.split('\n')) {
      if (raw.startsWith('---') || raw.startsWith('+++')) continue;
      const m = hunkHeaderRe.exec(raw);
      if (m) {
        oldCursor = parseInt(m[1], 10) || 0;
        newCursor = parseInt(m[3], 10) || 0;
        current = { header: raw, oldStart: oldCursor, newStart: newCursor, lines: [] };
        hunks.push(current);
        continue;
      }
      if (!current) continue; // 没有 hunk 头之前的杂行直接丢
      if (raw.startsWith('+')) {
        current.lines.push({ type: 'add', content: raw.slice(1), displayLine: newCursor });
        newCursor += 1;
      } else if (raw.startsWith('-')) {
        current.lines.push({ type: 'delete', content: raw.slice(1), displayLine: oldCursor });
        oldCursor += 1;
      } else {
        // context: 以空格起始或空行；old==new，取任一即可
        current.lines.push({
          type: 'context',
          content: raw.startsWith(' ') ? raw.slice(1) : raw,
          displayLine: newCursor,
        });
        oldCursor += 1;
        newCursor += 1;
      }
    }
    return hunks;
  });

  const hasDiff = $derived(diffHunks.some((h) => h.lines.length > 0));

  /**
   * 对每个 hunk 做整段语法高亮（保持跨行 token 不被截断），
   * 返回 hunkIndex → 该 hunk 各行的 HTML 数组。
   */
  const highlightedHunkLines = $derived.by<string[][]>(() => {
    if (diffHunks.length === 0) return [];
    const lang = diffLanguage;
    const useHljs = lang && hljs.getLanguage(lang);
    return diffHunks.map((hunk) => {
      if (hunk.lines.length === 0) return [];
      if (!useHljs) return hunk.lines.map((l) => escapeHtml(l.content));
      try {
        const joined = hunk.lines.map((l) => l.content).join('\n');
        return hljs.highlight(joined, { language: lang }).value.split('\n');
      } catch {
        return hunk.lines.map((l) => escapeHtml(l.content));
      }
    });
  });

  // ============ 文件类型派生 ============
  const displayPath = $derived(getDisplayPath(activeFilePath, workspaceRoot));
  const markdownFile = $derived(isMarkdownFile(activeFilePath));
  const wordFile = $derived(isWordFile(activeFilePath));
  const imageFile = $derived(activeFilePath ? isImageFile(activeFilePath) : false);
  // 图片虽属二进制，但走专门的 <img> 预览分支，故从 binaryFile（元信息兜底）排除。
  const binaryFile = $derived(
    !imageFile
      && (activeContentKind === 'binary' || (activeFilePath ? isKnownBinaryFile(activeFilePath) : false)),
  );
  const largeTextFile = $derived(activeContentKind === 'large_text');
  const symlinkFile = $derived(activeContentKind === 'symlink');
  const specialFile = $derived(activeContentKind === 'special');

  // ============ 图片缩放 / 平移 ============
  const IMAGE_ZOOM_MIN = 0.1;
  const IMAGE_ZOOM_MAX = 8;
  const IMAGE_ZOOM_STEP = 0.2;
  let imageZoom = $state(1);
  let imagePanX = $state(0);
  let imagePanY = $state(0);
  let imageDragging = $state(false);
  let imageDragStartX = 0;
  let imageDragStartY = 0;
  let imagePanStartX = 0;
  let imagePanStartY = 0;

  // 切换文件时重置缩放/平移，避免沿用上一张图的视图状态。
  $effect(() => {
    void activeFilePath;
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  });

  function clampZoom(value: number): number {
    return Math.min(IMAGE_ZOOM_MAX, Math.max(IMAGE_ZOOM_MIN, value));
  }

  function setImageZoom(next: number) {
    const clamped = clampZoom(next);
    if (clamped === 1) {
      imagePanX = 0;
      imagePanY = 0;
    }
    imageZoom = clamped;
  }

  function zoomImageIn() {
    setImageZoom(imageZoom + IMAGE_ZOOM_STEP);
  }

  function zoomImageOut() {
    setImageZoom(imageZoom - IMAGE_ZOOM_STEP);
  }

  function resetImageZoom() {
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  }

  function handleImageWheel(event: WheelEvent) {
    event.preventDefault();
    const factor = event.deltaY < 0 ? 1 + IMAGE_ZOOM_STEP : 1 / (1 + IMAGE_ZOOM_STEP);
    setImageZoom(imageZoom * factor);
  }

  function handleImagePointerDown(event: PointerEvent) {
    if (imageZoom <= 1) return;
    imageDragging = true;
    imageDragStartX = event.clientX;
    imageDragStartY = event.clientY;
    imagePanStartX = imagePanX;
    imagePanStartY = imagePanY;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function handleImagePointerMove(event: PointerEvent) {
    if (!imageDragging) return;
    imagePanX = imagePanStartX + (event.clientX - imageDragStartX);
    imagePanY = imagePanStartY + (event.clientY - imageDragStartY);
  }

  function handleImagePointerUp(event: PointerEvent) {
    if (!imageDragging) return;
    imageDragging = false;
    (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
  }

  // ============ Markdown 渲染/源码切换 ============
  let markdownMode = $state<'rendered' | 'raw'>('rendered');
  const rawPreviewContent = $derived(previewContent ?? '');
  const truncatedContent = $derived(
    rawPreviewContent.length > 500_000 ? rawPreviewContent.slice(0, 100_000) : rawPreviewContent,
  );
  const isLargeFile = $derived(rawPreviewContent.length > 500_000);
  /**
   * source 视图行高亮：对整段内容做一次 hljs.highlight（保持跨行 token），
   * 然后按 '\n' 切片，避免逐行高亮丢失多行 token 上下文。
   */
  const sourceLines = $derived.by<string[]>(() => {
    const lines = truncatedContent.split('\n');
    if (lines.length === 0) return [];
    const lang = diffLanguage;
    const useHljs = lang && hljs.getLanguage(lang);
    if (!useHljs) return lines.map(escapeHtml);
    try {
      return hljs.highlight(truncatedContent, { language: lang }).value.split('\n');
    } catch {
      return lines.map(escapeHtml);
    }
  });
  const hasContent = $derived(rawPreviewContent.length > 0);
  const codeMode = $derived(
    !previewLoading && !previewError && !wordFile && !binaryFile
      && !largeTextFile && !symlinkFile && !specialFile
      && (hasDiff || (hasContent && (!markdownFile || markdownMode === 'raw'))),
  );

  // ============ Tab 视觉 ============
  // 代理 tab 的 label / accentToken 由 ToolCall 触发 openAgentTab 时一次性写入；
  // RightPane 不再二次按 roleId 反查 registry —— tab 本身即为视觉真源。

  function tabAccent(tab: RightPaneTab): string {
    if (tab.kind === 'agent') {
      return tab.accentToken ? `var(--${tab.accentToken})` : 'var(--accent)';
    }
    return 'var(--info)';
  }

  function tabLabel(tab: RightPaneTab): string {
    return tab.label;
  }

  function tabIcon(tab: RightPaneTab): 'file-text' | 'chevron-right' {
    return tab.kind === 'code' ? 'file-text' : 'chevron-right';
  }

  function tabTooltip(tab: RightPaneTab): string {
    if (tab.kind === 'code') {
      return (tab.payload as CodeTabPayload).filepath;
    }
    return tabLabel(tab);
  }

  // ============ 交互 ============
  /**
   * Tab 条 drag-to-pan 状态：
   * - 滚轮鼠标横向需求由 onwheel（deltaY → scrollLeft）解决；
   * - 触控板横滑由原生 deltaX 路径解决；
   * - 这里补的是「按住鼠标在 tab 条上拖动来平移」——VSCode / Chrome tab strip 的标准交互。
   */
  let dragState: { startX: number; startScrollLeft: number; moved: boolean } | null = null;
  let isDraggingTabs = $state(false);
  /** 真实拖拽刚结束的时间戳；用于吞掉紧随 pointerup 的 click 事件，避免拖拽结束误切换 tab */
  let dragEndedAt = 0;
  const DRAG_THRESHOLD = 4;
  const DRAG_CLICK_SUPPRESS_MS = 50;

  function recentlyDragged(): boolean {
    return performance.now() - dragEndedAt < DRAG_CLICK_SUPPRESS_MS;
  }

  function handleTabClick(tabId: string) {
    if (recentlyDragged()) return;
    setActiveRightPaneTab(sessionId, tabId);
  }

  function handleTabClose(event: MouseEvent, tabId: string) {
    event.stopPropagation();
    if (recentlyDragged()) return;
    closeTab(sessionId, tabId);
  }

  /**
   * 单行 tab 条只在水平方向溢出（overflow-x: auto），但标准鼠标滚轮只发出
   * 垂直方向的 deltaY，浏览器不会自动把它翻译成 scrollLeft——结果就是
   * 滚轮鼠标用户完全无法浏览溢出的 tab。这里把 deltaY 转成 scrollLeft，
   * 保留触控板原生 deltaX 走原路径（不重复消费）。
   */
  function handleTabsWheel(event: WheelEvent) {
    if (event.deltaX !== 0) return; // 触控板已经在水平方向输入 delta，不干预
    if (event.deltaY === 0) return;
    const target = event.currentTarget as HTMLDivElement;
    if (target.scrollWidth <= target.clientWidth) return; // 没有溢出就别拦
    target.scrollLeft += event.deltaY;
    event.preventDefault();
  }

  function handleTabsPointerDown(event: PointerEvent) {
    // 只对鼠标主键启用 drag-to-pan；触摸 / 笔 / 触控板交给原生路径
    if (event.pointerType !== 'mouse' || event.button !== 0) return;
    // 关闭按钮 (×) 不接管——保证用户点 × 关闭 tab 时不会进入拖拽
    const targetEl = event.target as HTMLElement | null;
    if (targetEl?.closest('.right-pane-tab-close')) return;
    const strip = event.currentTarget as HTMLDivElement;
    dragState = {
      startX: event.clientX,
      startScrollLeft: strip.scrollLeft,
      moved: false,
    };
  }

  function handleTabsPointerMove(event: PointerEvent) {
    if (!dragState) return;
    const dx = event.clientX - dragState.startX;
    if (!dragState.moved) {
      if (Math.abs(dx) < DRAG_THRESHOLD) return; // 未越过阈值仍按普通点击处理
      dragState.moved = true;
      isDraggingTabs = true;
      const strip = event.currentTarget as HTMLDivElement;
      strip.setPointerCapture(event.pointerId);
    }
    const strip = event.currentTarget as HTMLDivElement;
    strip.scrollLeft = dragState.startScrollLeft - dx;
    event.preventDefault();
  }

  function handleTabsPointerEnd(event: PointerEvent) {
    if (!dragState) return;
    const moved = dragState.moved;
    dragState = null;
    if (moved) {
      dragEndedAt = performance.now();
      isDraggingTabs = false;
      const strip = event.currentTarget as HTMLDivElement;
      if (strip.hasPointerCapture(event.pointerId)) {
        strip.releasePointerCapture(event.pointerId);
      }
    }
  }

  function formatSize(value?: number): string {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return '-';
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function getDisplayPath(path: string, root: string): string {
    if (!path) return '';
    const normalizedPath = path.replace(/\\/g, '/');
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
    if (normalizedRoot && normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return normalizedPath.slice(normalizedRoot.length + 1);
    }
    return path;
  }
</script>

<aside class="right-pane" aria-label={i18n.t('rightPane.title')}>
  <!-- 顶部 Tab 条 + 折叠按钮 -->
  <header class="right-pane-tabbar">
    <div
      class="right-pane-tabs"
      class:dragging={isDraggingTabs}
      role="tablist"
      tabindex="-1"
      aria-label={i18n.t('rightPane.title')}
      onwheel={handleTabsWheel}
      onpointerdown={handleTabsPointerDown}
      onpointermove={handleTabsPointerMove}
      onpointerup={handleTabsPointerEnd}
      onpointercancel={handleTabsPointerEnd}
    >
      {#each openTabs as tab (tab.id)}
        {@const isActive = tab.id === paneState.activeTabId}
        {@const accent = tabAccent(tab)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="right-pane-tab"
          class:active={isActive}
          role="tab"
          tabindex="0"
          aria-selected={isActive}
          style={`--tab-accent: ${accent};`}
          title={tabTooltip(tab)}
          onclick={() => handleTabClick(tab.id)}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); handleTabClick(tab.id); } }}
        >
          <span class="right-pane-tab-icon" aria-hidden="true">
            <Icon name={tabIcon(tab)} size={12} />
          </span>
          <span class="right-pane-tab-label" class:mono={tab.kind === 'code'}>{tabLabel(tab)}</span>
          <button
            type="button"
            class="right-pane-tab-close"
            aria-label={i18n.t('rightPane.closeTab')}
            onclick={(event) => handleTabClose(event, tab.id)}
          >
            <Icon name="x" size={10} />
          </button>
        </div>
      {/each}
    </div>
    <button
      type="button"
      class="right-pane-collapse-btn"
      onclick={() => toggleRightPane(sessionId)}
      title={i18n.t('rightPane.collapse')}
      aria-label={i18n.t('rightPane.collapse')}
    >
      <Icon name="sidebar-toggle" size={14} />
    </button>
  </header>

  <!-- 当前 code tab 的副标题：路径 + Markdown 渲染/源码切换 -->
  {#if activeTab && activeTab.kind === 'code'}
    <div class="right-pane-subbar">
      <div class="right-pane-path" title={activeFilePath}>{displayPath}</div>
      {#if markdownFile && !hasDiff && !previewLoading && !previewError && !wordFile && !binaryFile && !largeTextFile && !symlinkFile && !specialFile && hasContent}
        <div class="right-pane-markdown-modes" role="tablist" aria-label={i18n.t('web.filePreviewTitle')}>
          <button
            type="button"
            class="right-pane-markdown-mode"
            class:active={markdownMode === 'rendered'}
            onclick={() => markdownMode = 'rendered'}
          >{i18n.t('web.filePreviewRendered')}</button>
          <button
            type="button"
            class="right-pane-markdown-mode"
            class:active={markdownMode === 'raw'}
            onclick={() => markdownMode = 'raw'}
          >{i18n.t('web.filePreviewRaw')}</button>
        </div>
      {/if}
    </div>
  {/if}

  <!-- Body：按 activeTab 路由 -->
  <div class="right-pane-body" class:right-pane-body--code={codeMode}>
    {#if !activeTab}
      <div class="right-pane-state">
        <Icon name="sidebar-toggle" size={22} />
        <span>{i18n.t('rightPane.empty.title')}</span>
        <span class="right-pane-meta-line">{i18n.t('rightPane.empty.hint')}</span>
      </div>
    {:else if activeTab.kind === 'agent'}
      <AgentTabContent taskId={(activeTab.payload as AgentTabPayload).taskId} />
    {:else if previewLoading}
      <div class="right-pane-state">{i18n.t('web.filePreviewLoading')}</div>
    {:else if previewError}
      <div class="right-pane-state right-pane-state--error">
        {previewError}
      </div>
    {:else if wordFile}
      <div class="right-pane-state">
        <Icon name="document" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedWord')}</span>
      </div>
    {:else if imageFile}
      <div class="right-pane-image-wrap">
        <div
          class="right-pane-image"
          class:dragging={imageDragging}
          class:zoomed={imageZoom > 1}
          role="img"
          aria-label={displayPath}
          onwheel={handleImageWheel}
          onpointerdown={handleImagePointerDown}
          onpointermove={handleImagePointerMove}
          onpointerup={handleImagePointerUp}
          onpointercancel={handleImagePointerUp}
        >
          <img
            class="right-pane-image-el"
            src={agentUrl('/api/files/raw', activeFilePreviewQuery)}
            alt={displayPath}
            draggable="false"
            style={`transform: translate(${imagePanX}px, ${imagePanY}px) scale(${imageZoom});`}
          />
        </div>
        <div class="right-pane-image-controls">
          <button class="image-zoom-btn" onclick={zoomImageOut} disabled={imageZoom <= IMAGE_ZOOM_MIN} title={i18n.t('web.imageZoomOut')} aria-label={i18n.t('web.imageZoomOut')}>
            <Icon name="minus" size={14} />
          </button>
          <button class="image-zoom-level" onclick={resetImageZoom} title={i18n.t('web.imageZoomReset')}>{Math.round(imageZoom * 100)}%</button>
          <button class="image-zoom-btn" onclick={zoomImageIn} disabled={imageZoom >= IMAGE_ZOOM_MAX} title={i18n.t('web.imageZoomIn')} aria-label={i18n.t('web.imageZoomIn')}>
            <Icon name="plus" size={14} />
          </button>
        </div>
      </div>
    {:else if binaryFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedBinary')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</span>
        {#if activeCodePayload?.mime}
          <span class="right-pane-meta-line">{i18n.t('edits.nonText.mime')}: {activeCodePayload.mime}</span>
        {/if}
      </div>
    {:else if largeTextFile}
      <div class="right-pane-large-text">
        <div class="right-pane-notice">{i18n.t('edits.nonText.largeTextTitle')} · {i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</div>
        {#if activeCodePayload?.headSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.head')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.headSummary}</pre>
          </div>
        {/if}
        {#if activeCodePayload?.tailSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.tail')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.tailSummary}</pre>
          </div>
        {/if}
      </div>
    {:else if symlinkFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.symlinkTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.target')}: {activeCodePayload?.symlinkTarget ?? '-'}</span>
      </div>
    {:else if specialFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.specialTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.specialHint')}</span>
      </div>
    {:else if hasDiff}
      <div class="right-pane-diff" aria-label={displayPath}>
        {#each diffHunks as hunk, hi (hi)}
          {#if hi > 0}
            <div class="right-pane-diff-hunk-sep" aria-hidden="true"></div>
          {/if}
          <div class="right-pane-diff-hunk">
            <div class="right-pane-diff-hunk-header" title={hunk.header}>{hunk.header}</div>
            {#each hunk.lines as line, li (li)}
              <div class="right-pane-diff-row {line.type}">
                <span class="right-pane-diff-gutter" aria-hidden="true">{line.displayLine}</span>
                <code class="right-pane-diff-content">{@html highlightedHunkLines[hi]?.[li] ?? ''}</code>
              </div>
            {/each}
          </div>
        {/each}
      </div>
    {:else if !hasContent}
      <div class="right-pane-state">{i18n.t('edits.preview.empty')}</div>
    {:else}
      {#if isLargeFile}
        <div class="right-pane-notice">{i18n.t('web.filePreviewLargeFile')}</div>
      {/if}
      {#if markdownFile && markdownMode === 'rendered'}
        <div class="right-pane-markdown">
          <MarkdownContent content={truncatedContent} />
        </div>
      {:else}
        <div class="right-pane-source" aria-label={displayPath}>
          {#each sourceLines as line, index (index)}
            <div class="right-pane-source-row">
              <span class="right-pane-source-line-number" aria-hidden="true">{index + 1}</span>
              <code class="right-pane-source-line">{@html line || '&nbsp;'}</code>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
</aside>

<style>
  .right-pane {
    /* 与左侧 sidebar 同款卡片样式：1px border + radius-lg + surface-1 底，
       overflow:hidden 用于让顶部 tabbar 的高亮条/底色被卡片圆角裁切，避免溢出 */
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--background);
    overflow: hidden;
  }

  /* ============ Tab 条 ============ */
  .right-pane-tabbar {
    display: flex;
    align-items: stretch;
    height: 38px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: var(--background);
    padding-right: var(--space-2);
  }

  .right-pane-tabs {
    display: flex;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    /* drag-to-pan：默认抓握光标，提示用户「这一条可以按住拖」；
       拖拽进行中切到 grabbing 并禁用文字选择，避免选中 tab 文字 */
    cursor: grab;
    user-select: none;
  }
  .right-pane-tabs::-webkit-scrollbar { display: none; }
  .right-pane-tabs.dragging { cursor: grabbing; }

  /* 面板内折叠按钮：与 Header 的 toggle 等价，主要服务于窄屏 overlay 模式（顶部按钮被遮挡） */
  .right-pane-collapse-btn {
    flex: 0 0 auto;
    align-self: center;
    width: 28px;
    height: 28px;
    margin-left: var(--space-2);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }
  .right-pane-collapse-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .right-pane-collapse-btn:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent, currentColor) 60%, transparent);
    outline-offset: -2px;
  }

  .right-pane-tab {
    flex: 0 0 auto;
    max-width: 180px;
    min-width: 90px;
    padding: 0 var(--space-3) 0 var(--space-4);
    height: 100%;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    cursor: pointer;
    position: relative;
    border-right: 1px solid var(--border-subtle);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-tab:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .right-pane-tab.active {
    background: var(--surface-1);
    color: var(--foreground);
    font-weight: var(--font-semibold);
  }

  .right-pane-tab.active::before {
    content: '';
    position: absolute;
    left: 0; right: 0; top: 0;
    height: 2px;
    background: var(--tab-accent, var(--primary));
  }

  .right-pane-tab-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--tab-accent, var(--foreground-muted));
    flex-shrink: 0;
  }

  .right-pane-tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .right-pane-tab-label.mono { font-family: var(--font-mono); font-size: var(--text-xs); }

  .right-pane-tab-close {
    width: 16px;
    height: 16px;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--foreground-muted);
    border: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    opacity: 0;
    flex-shrink: 0;
    padding: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast);
  }

  .right-pane-tab:hover .right-pane-tab-close,
  .right-pane-tab.active .right-pane-tab-close {
    opacity: 0.85;
  }

  .right-pane-tab-close:hover {
    background: color-mix(in srgb, var(--foreground-muted) 18%, transparent);
    opacity: 1;
  }

  /* ============ 副标题（路径 + Markdown 切换） ============ */
  .right-pane-subbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 6px var(--space-4);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .right-pane-path {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-family: var(--font-mono);
  }

  .right-pane-markdown-modes {
    display: inline-flex;
    gap: 2px;
    flex-shrink: 0;
  }

  .right-pane-markdown-mode {
    padding: 3px 10px;
    border: none;
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-markdown-mode:hover,
  .right-pane-markdown-mode.active {
    background: color-mix(in srgb, var(--surface-selected) 72%, transparent);
    color: var(--foreground);
  }

  /* ============ Body ============ */
  .right-pane-body {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4);
  }

  .right-pane-body--code {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-source {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4) 0;
    background: transparent;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .right-pane-source-row {
    display: grid;
    grid-template-columns: 46px minmax(0, 1fr);
    align-items: start;
    min-width: 0;
  }

  .right-pane-source-line-number {
    position: sticky;
    left: 0;
    z-index: 1;
    padding: 0 10px 0 var(--space-2);
    background: transparent;
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    opacity: 0.46;
    text-align: right;
    user-select: none;
  }

  .right-pane-source-line {
    min-width: 0;
    padding: 0 var(--space-4) 0 var(--space-3);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    overflow-wrap: anywhere;
    tab-size: 2;
    white-space: pre-wrap;
  }

  /* ============ Diff 视图（单列行号 + 代码块外观） ============ */
  .right-pane-diff {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4);
    background: transparent;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .right-pane-diff-hunk {
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md, 8px);
    overflow: hidden;
  }

  .right-pane-diff-hunk-header {
    padding: 4px var(--space-3) 4px 56px;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    background: color-mix(in srgb, var(--surface-2) 60%, transparent);
    border-bottom: 1px solid var(--border-subtle);
    opacity: 0.85;
    user-select: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .right-pane-diff-hunk-sep {
    height: var(--space-3);
  }

  .right-pane-diff-row {
    display: grid;
    grid-template-columns: 48px minmax(0, 1fr);
    align-items: start;
    min-height: 20px;
  }

  .right-pane-diff-gutter {
    padding: 0 8px 0 4px;
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    font-size: var(--text-2xs);
    opacity: 0.5;
    text-align: right;
    user-select: none;
    overflow: hidden;
  }

  .right-pane-diff-content {
    min-width: 0;
    padding: 0 var(--space-3) 0 var(--space-2);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    overflow-wrap: anywhere;
    tab-size: 2;
    white-space: pre-wrap;
  }

  .right-pane-diff-row.add {
    background: color-mix(in oklab, var(--success) 15%, transparent);
  }
  .right-pane-diff-row.add .right-pane-diff-gutter { color: var(--success); opacity: 0.85; }

  .right-pane-diff-row.delete {
    background: color-mix(in oklab, var(--error) 15%, transparent);
  }
  .right-pane-diff-row.delete .right-pane-diff-gutter { color: var(--error); opacity: 0.85; }

  .right-pane-diff-row.context { color: var(--foreground-muted); }

  :global(body.vscode-light) .right-pane-diff-row.add,
  :global(body.theme-light) .right-pane-diff-row.add,
  :global(:root.theme-light) .right-pane-diff-row.add {
    background: #dafbe1;
  }
  :global(body.vscode-light) .right-pane-diff-row.delete,
  :global(body.theme-light) .right-pane-diff-row.delete,
  :global(:root.theme-light) .right-pane-diff-row.delete {
    background: #ffeef0;
  }

  .right-pane-markdown {
    max-width: 880px;
    color: var(--foreground);
    line-height: 1.65;
  }

  .right-pane-state {
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    color: var(--foreground-muted);
    text-align: center;
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .right-pane-state--error { color: var(--error); }
  .right-pane-state--metadata { padding: var(--space-4); }

  .right-pane-meta-line {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .right-pane-image-wrap {
    position: relative;
    width: 100%;
    height: 100%;
    box-sizing: border-box;
  }

  .right-pane-image {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 100%;
    padding: var(--space-4);
    overflow: hidden;
    box-sizing: border-box;
    touch-action: none;
  }

  .right-pane-image.zoomed {
    cursor: grab;
  }

  .right-pane-image.dragging {
    cursor: grabbing;
  }

  .right-pane-image-el {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    transform-origin: center center;
    will-change: transform;
    user-select: none;
    -webkit-user-drag: none;
    /* 透明图片用棋盘格底衬出边界，避免与面板同色看不清 */
    background-image:
      linear-gradient(45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(-45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%),
      linear-gradient(-45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%);
    background-size: 16px 16px;
    background-position: 0 0, 0 8px, 8px -8px, -8px 0;
    border-radius: var(--radius-sm, 4px);
  }

  .right-pane-image-controls {
    position: absolute;
    bottom: var(--space-3);
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: 4px 6px;
    background: var(--surface-overlay, rgba(20, 20, 22, 0.82));
    border: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.12));
    border-radius: var(--radius-md, 8px);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
    backdrop-filter: blur(6px);
  }

  .image-zoom-btn,
  .image-zoom-level {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: var(--foreground-on-overlay, #f5f5f5);
    cursor: pointer;
    border-radius: var(--radius-sm, 4px);
  }

  .image-zoom-btn {
    width: 26px;
    height: 26px;
  }

  .image-zoom-level {
    min-width: 48px;
    height: 26px;
    padding: 0 6px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .image-zoom-btn:hover:not(:disabled),
  .image-zoom-level:hover {
    background: rgba(255, 255, 255, 0.14);
  }

  .image-zoom-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .right-pane-large-text {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .right-pane-summary-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .right-pane-summary-title {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .right-pane-summary-content {
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-1) 82%, var(--background));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
    max-height: 260px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .right-pane-notice {
    margin-bottom: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--warning, #f59e0b) 30%, var(--border));
    background: color-mix(in srgb, var(--warning, #f59e0b) 10%, transparent);
    color: var(--foreground);
    font-size: var(--text-xs);
  }
</style>
