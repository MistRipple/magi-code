<script lang="ts">
  import { vscode } from '../lib/vscode-bridge';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';

  // 知识类型定义
  interface CodeIndex {
    files?: Array<{ path: string; lines?: number; size?: number }>;
    techStack?: string[];
    entryPoints?: string[];
  }

  interface ADR {
    id: string;
    title: string;
    status?: string;
    date?: string;
    context?: string;
    decision?: string;
    consequences?: string;
    tags?: string[];
  }

  interface FAQ {
    id: string;
    question: string;
    answer?: string;
    category?: string;
    tags?: string[];
  }

  // 状态
  let currentTab = $state<'overview' | 'adr' | 'faq'>('overview');
  let isLoading = $state(true);
  let codeIndex = $state<CodeIndex | null>(null);
  let adrs = $state<ADR[]>([]);
  let faqs = $state<FAQ[]>([]);
  let searchQuery = $state('');
  let adrFilter = $state<'all' | 'proposed' | 'accepted' | 'archived' | 'superseded'>('all');
  let expandedAdrId = $state<string | null>(null);
  let expandedFaqId = $state<string | null>(null);
  let showClearConfirm = $state(false);

  // 统计信息
  const fileCount = $derived(codeIndex?.files?.length || 0);
  const totalLines = $derived(
    codeIndex?.files?.reduce((sum, f) => sum + (f.lines || 0), 0) || 0
  );

  // 过滤后的 ADR 列表（安全过滤，跳过无效数据）
  const filteredAdrs = $derived.by(() => {
    let result = adrs.filter(adr => adr.title && typeof adr.title === 'string');
    if (adrFilter !== 'all') {
      result = result.filter(adr => adr.status === adrFilter);
    }
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase();
      result = result.filter(adr =>
        adr.title.toLowerCase().includes(query) ||
        adr.context?.toLowerCase().includes(query) ||
        adr.tags?.some(t => t.toLowerCase().includes(query))
      );
    }
    return result;
  });

  // 过滤后的 FAQ 列表（安全过滤，跳过无效数据）
  const filteredFaqs = $derived.by(() => {
    let result = faqs.filter(faq => faq.question && typeof faq.question === 'string');
    if (!searchQuery.trim()) return result;
    const query = searchQuery.toLowerCase();
    return result.filter(faq =>
      faq.question.toLowerCase().includes(query) ||
      faq.answer?.toLowerCase().includes(query) ||
      faq.tags?.some(t => t.toLowerCase().includes(query))
    );
  });

  function switchTab(tabId: typeof currentTab) {
    currentTab = tabId;
    expandedAdrId = null;
    expandedFaqId = null;
  }

  function refresh() {
    isLoading = true;
    vscode.postMessage({ type: 'getProjectKnowledge' });
  }

  function toggleAdr(adr: ADR) {
    expandedAdrId = expandedAdrId === adr.id ? null : adr.id;
  }

  function toggleFaq(faq: FAQ) {
    expandedFaqId = expandedFaqId === faq.id ? null : faq.id;
  }

  function deleteAdr(id: string, e: Event) {
    e.stopPropagation();
    vscode.postMessage({ type: 'deleteADR', id });
  }

  function deleteFaq(id: string, e: Event) {
    e.stopPropagation();
    vscode.postMessage({ type: 'deleteFAQ', id });
  }

  function confirmClear() {
    showClearConfirm = true;
  }

  function cancelClear() {
    showClearConfirm = false;
  }

  function executeClear() {
    showClearConfirm = false;
    isLoading = true;
    vscode.postMessage({ type: 'clearProjectKnowledge' });
  }

  // ADR 状态的显示文本
  function statusLabel(status?: string): string {
    const map: Record<string, string> = {
      proposed: '提议中',
      accepted: '已接受',
      archived: '已归档',
      superseded: '已替代',
    };
    return status ? (map[status] || status) : '';
  }

  // 监听来自扩展的消息
  $effect(() => {
    const unsubscribe = vscode.onMessage((msg) => {
      if (msg.type !== 'unifiedMessage') return;
      const standard = msg.message as StandardMessage;
      if (!standard || standard.category !== MessageCategory.DATA || !standard.data) return;
      if (standard.data.dataType !== 'projectKnowledgeLoaded') return;

      const payload = standard.data.payload as { codeIndex?: any; adrs?: any[]; faqs?: any[] };
      codeIndex = payload?.codeIndex
        ? {
            ...payload.codeIndex,
            files: ensureArray(payload.codeIndex.files),
            techStack: ensureArray(payload.codeIndex.techStack),
            entryPoints: ensureArray(payload.codeIndex.entryPoints)
          }
        : null;
      adrs = ensureArray(payload?.adrs);
      faqs = ensureArray(payload?.faqs);
      isLoading = false;
    });

    // 初始化时请求数据
    vscode.postMessage({ type: 'getProjectKnowledge' });

    return () => unsubscribe();
  });
</script>

<div class="knowledge-panel">
  <!-- 头部：Tab 栏 -->
  <div class="kp-tabs-bar">
    <button class="kp-tab" class:active={currentTab === 'overview'} onclick={() => switchTab('overview')}>
      <Icon name="stats" size={13} />
      <span>概览</span>
    </button>
    <button class="kp-tab" class:active={currentTab === 'adr'} onclick={() => switchTab('adr')}>
      <Icon name="document" size={13} />
      <span>ADR</span>
      {#if adrs.length > 0}
        <span class="kp-tab-count">{adrs.length}</span>
      {/if}
    </button>
    <button class="kp-tab" class:active={currentTab === 'faq'} onclick={() => switchTab('faq')}>
      <Icon name="question" size={13} />
      <span>FAQ</span>
      {#if faqs.length > 0}
        <span class="kp-tab-count">{faqs.length}</span>
      {/if}
    </button>
    <div class="kp-tab-actions">
      <button class="kp-icon-btn" onclick={refresh} disabled={isLoading} title="刷新">
        <Icon name="refresh" size={14} class={isLoading ? 'spinning' : ''} />
      </button>
      <button
        class="kp-icon-btn kp-icon-btn--danger"
        onclick={confirmClear}
        disabled={isLoading || (adrs.length === 0 && faqs.length === 0)}
        title="清空知识库"
      >
        <Icon name="delete" size={14} />
      </button>
    </div>
  </div>

  <!-- 搜索栏（Tab 下方独立行） -->
  {#if currentTab !== 'overview'}
    <div class="kp-search-bar">
      <Icon name="search" size={13} />
      <input
        type="text"
        class="kp-search-input"
        placeholder={currentTab === 'adr' ? '搜索架构决策...' : '搜索常见问题...'}
        bind:value={searchQuery}
      />
      {#if searchQuery}
        <button class="kp-search-clear" onclick={() => searchQuery = ''}>
          <Icon name="close" size={12} />
        </button>
      {/if}
    </div>
  {/if}

  <!-- 清空确认弹窗 -->
  {#if showClearConfirm}
    <div class="kp-confirm-overlay" role="dialog">
      <div class="kp-confirm-dialog">
        <div class="kp-confirm-icon">
          <Icon name="warning" size={24} />
        </div>
        <div class="kp-confirm-title">确认清空知识库</div>
        <p class="kp-confirm-desc">
          将清空所有 ADR（{adrs.length}）、FAQ（{faqs.length}）和经验记录。此操作不可撤销。
        </p>
        <div class="kp-confirm-actions">
          <button class="kp-confirm-btn kp-confirm-btn--cancel" onclick={cancelClear}>取消</button>
          <button class="kp-confirm-btn kp-confirm-btn--danger" onclick={executeClear}>确认清空</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- 主内容区 -->
  <div class="kp-content">
    {#if isLoading}
      <div class="kp-loading">
        <div class="kp-spinner"></div>
        <span>加载中...</span>
      </div>
    {:else if currentTab === 'overview'}
      <!-- 概览 -->
      <div class="kp-overview">
        <!-- 紧凑统计条 -->
        <div class="kp-stats-row">
          <div class="kp-stat">
            <span class="kp-stat-value">{fileCount.toLocaleString()}</span>
            <span class="kp-stat-label">文件</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{totalLines.toLocaleString()}</span>
            <span class="kp-stat-label">代码行</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{adrs.length}</span>
            <span class="kp-stat-label">ADR</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{faqs.length}</span>
            <span class="kp-stat-label">FAQ</span>
          </div>
        </div>

        {#if codeIndex?.techStack && codeIndex.techStack.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="code" size={13} />
              <span>技术栈</span>
            </h4>
            <div class="kp-tech-grid">
              {#each codeIndex.techStack as tech}
                <span class="kp-tech-badge">{tech}</span>
              {/each}
            </div>
          </div>
        {/if}

        {#if codeIndex?.entryPoints && codeIndex.entryPoints.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="target" size={13} />
              <span>入口文件</span>
            </h4>
            <div class="kp-entry-list">
              {#each codeIndex.entryPoints as entry}
                <div class="kp-entry-item">
                  <Icon name="file-text" size={12} />
                  <span>{entry}</span>
                </div>
              {/each}
            </div>
          </div>
        {/if}

        <!-- 最近的 ADR 预览 -->
        {#if adrs.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="document" size={13} />
              <span>最近的决策</span>
              <button class="kp-section-link" onclick={() => switchTab('adr')}>查看全部</button>
            </h4>
            {#each adrs.slice(0, 3) as adr (adr.id)}
              <div class="kp-preview-item">
                <span class="kp-preview-dot {adr.status || 'default'}"></span>
                <span class="kp-preview-text">{adr.title}</span>
                {#if adr.status}
                  <span class="kp-preview-status">{statusLabel(adr.status)}</span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>

    {:else if currentTab === 'adr'}
      <!-- ADR Tab -->
      <div class="kp-filter-bar">
        {#each [['all', '全部'], ['proposed', '提议中'], ['accepted', '已接受'], ['archived', '已归档'], ['superseded', '已替代']] as [value, label]}
          <button class="kp-filter-chip" class:active={adrFilter === value} onclick={() => adrFilter = value as any}>{label}</button>
        {/each}
      </div>
      <div class="kp-list">
        {#if filteredAdrs.length === 0}
          <div class="kp-empty">
            <Icon name="document" size={28} />
            <div class="kp-empty-title">暂无架构决策记录</div>
            <div class="kp-empty-hint">Agent 完成任务后会自动提取并归档</div>
          </div>
        {:else}
          {#each filteredAdrs as adr (adr.id)}
            {@const isExpanded = expandedAdrId === adr.id}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header" role="button" tabindex="0" onclick={() => toggleAdr(adr)} onkeydown={(e) => e.key === 'Enter' && toggleAdr(adr)}>
                <span class="kp-card-indicator {adr.status || 'default'}"></span>
                <div class="kp-card-main">
                  <span class="kp-card-title">{adr.title}</span>
                  {#if !isExpanded && adr.context}
                    <p class="kp-card-preview">{adr.context}</p>
                  {/if}
                </div>
                <div class="kp-card-meta">
                  {#if adr.status}
                    <span class="kp-status-badge {adr.status}">{statusLabel(adr.status)}</span>
                  {/if}
                  <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
                </div>
                <button class="kp-card-delete" title="删除" onclick={(e) => deleteAdr(adr.id, e)}>
                  <Icon name="trash" size={12} />
                </button>
              </div>
              {#if isExpanded}
                <div class="kp-card-body">
                  {#if adr.date}
                    <div class="kp-detail-meta">
                      <Icon name="clock" size={12} />
                      <span>{adr.date}</span>
                    </div>
                  {/if}
                  {#if adr.context}
                    <div class="kp-detail-block">
                      <h5>上下文</h5>
                      <p>{adr.context}</p>
                    </div>
                  {/if}
                  {#if adr.decision}
                    <div class="kp-detail-block">
                      <h5>决策</h5>
                      <p>{adr.decision}</p>
                    </div>
                  {/if}
                  {#if adr.consequences}
                    <div class="kp-detail-block">
                      <h5>后果</h5>
                      <p>{adr.consequences}</p>
                    </div>
                  {/if}
                  {#if adr.tags && adr.tags.length > 0}
                    <div class="kp-tag-list">
                      {#each adr.tags as tag}
                        <span class="kp-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

    {:else if currentTab === 'faq'}
      <!-- FAQ Tab -->
      <div class="kp-list">
        {#if filteredFaqs.length === 0}
          <div class="kp-empty">
            <Icon name="question" size={28} />
            <div class="kp-empty-title">暂无常见问题</div>
            <div class="kp-empty-hint">Agent 完成任务后会自动提取并归档</div>
          </div>
        {:else}
          {#each filteredFaqs as faq (faq.id)}
            {@const isExpanded = expandedFaqId === faq.id}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header" role="button" tabindex="0" onclick={() => toggleFaq(faq)} onkeydown={(e) => e.key === 'Enter' && toggleFaq(faq)}>
                <span class="kp-card-indicator faq"></span>
                <div class="kp-card-main">
                  <span class="kp-card-title">{faq.question}</span>
                  {#if !isExpanded && faq.answer}
                    <p class="kp-card-preview">{faq.answer}</p>
                  {/if}
                </div>
                <div class="kp-card-meta">
                  {#if faq.category}
                    <span class="kp-category-badge">{faq.category}</span>
                  {/if}
                  <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
                </div>
                <button class="kp-card-delete" title="删除" onclick={(e) => deleteFaq(faq.id, e)}>
                  <Icon name="trash" size={12} />
                </button>
              </div>
              {#if isExpanded}
                <div class="kp-card-body">
                  {#if faq.answer}
                    <div class="kp-detail-block">
                      <p>{faq.answer}</p>
                    </div>
                  {/if}
                  {#if faq.tags && faq.tags.length > 0}
                    <div class="kp-tag-list">
                      {#each faq.tags as tag}
                        <span class="kp-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  /* ============================================
     KnowledgePanel - 知识库面板
     设计参考: Linear/Notion 知识管理界面
     适配 VSCode 侧边栏窄面板约束
     ============================================ */

  .knowledge-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
  }

  /* ---- Tab 栏（下划线风格） ---- */
  .kp-tabs-bar {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
    background: var(--surface-1);
  }

  .kp-tab {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .kp-tab:hover {
    color: var(--foreground);
  }

  .kp-tab.active {
    color: var(--foreground);
    border-bottom-color: var(--primary);
  }

  .kp-tab-count {
    font-size: 10px;
    min-width: 16px;
    height: 16px;
    line-height: 16px;
    text-align: center;
    padding: 0 4px;
    background: var(--surface-3);
    color: var(--foreground-muted);
    border-radius: var(--radius-full);
  }

  .kp-tab.active .kp-tab-count {
    background: var(--primary);
    color: white;
  }

  .kp-tab-actions {
    display: flex;
    gap: var(--space-1);
    margin-left: auto;
  }

  .kp-icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-icon-btn:hover:not(:disabled) {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-icon-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .kp-icon-btn--danger:hover:not(:disabled) {
    background: var(--error-muted);
    color: var(--error);
  }

  :global(.spinning) {
    animation: kp-spin 1s linear infinite;
  }

  @keyframes kp-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ---- 搜索栏 ---- */
  .kp-search-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
    color: var(--foreground-muted);
  }

  .kp-search-input {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
    outline: none;
    min-width: 0;
  }

  .kp-search-input::placeholder {
    color: var(--foreground-muted);
    opacity: 0.6;
  }

  .kp-search-clear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    background: var(--surface-3);
    border: none;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-search-clear:hover {
    background: var(--surface-active);
    color: var(--foreground);
  }


  /* ---- 主内容区 ---- */
  .kp-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-3);
  }

  .kp-loading {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    height: 200px;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .kp-spinner {
    width: 20px;
    height: 20px;
    border: 2px solid var(--border);
    border-top-color: var(--primary);
    border-radius: var(--radius-full);
    animation: kp-spin 1s linear infinite;
  }

  /* ---- 概览 ---- */
  .kp-overview {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .kp-stats-row {
    display: flex;
    align-items: center;
    justify-content: space-around;
    padding: var(--space-3) var(--space-2);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .kp-stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }

  .kp-stat-value {
    font-size: var(--text-md);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    font-variant-numeric: tabular-nums;
  }

  .kp-stat-label {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .kp-stat-divider {
    width: 1px;
    height: 24px;
    background: var(--border);
  }

  .kp-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .kp-section-title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0;
  }

  .kp-section-link {
    margin-left: auto;
    font-size: var(--text-xs);
    font-weight: var(--font-normal);
    color: var(--primary);
    background: none;
    border: none;
    cursor: pointer;
    padding: 0;
  }

  .kp-section-link:hover {
    text-decoration: underline;
  }

  .kp-tech-grid {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .kp-tech-badge {
    font-size: var(--text-xs);
    padding: 2px 8px;
    background: var(--surface-2);
    color: var(--foreground);
    border-radius: var(--radius-full);
    border: 1px solid var(--border-subtle);
  }

  .kp-entry-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .kp-entry-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-1);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
  }

  /* 概览预览条目 */
  .kp-preview-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-sm);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
  }

  .kp-preview-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .kp-preview-dot.proposed { background: var(--info); }
  .kp-preview-dot.accepted { background: var(--success); }
  .kp-preview-dot.archived { background: var(--foreground-muted); }
  .kp-preview-dot.superseded { background: var(--warning); }
  .kp-preview-dot.default { background: var(--foreground-muted); opacity: 0.4; }

  .kp-preview-text {
    flex: 1;
    min-width: 0;
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .kp-preview-status {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    flex-shrink: 0;
  }

  /* ---- 过滤栏 ---- */
  .kp-filter-bar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    margin-bottom: var(--space-3);
  }

  .kp-filter-chip {
    padding: 2px 10px;
    font-size: var(--text-xs);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-filter-chip:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-filter-chip.active {
    background: var(--primary);
    border-color: var(--primary);
    color: white;
  }

  /* ---- 列表 ---- */
  .kp-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .kp-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-8) var(--space-4);
    color: var(--foreground-muted);
    text-align: center;
    opacity: 0.7;
  }

  .kp-empty-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    opacity: 0.8;
  }

  .kp-empty-hint {
    font-size: var(--text-xs);
    opacity: 0.6;
  }

  /* ---- 卡片（ADR / FAQ 条目） ---- */
  .kp-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    overflow: hidden;
    transition: border-color var(--transition-fast);
  }

  .kp-card:hover {
    border-color: var(--foreground-muted);
  }

  .kp-card.expanded {
    border-color: var(--primary);
  }

  .kp-card-header {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-3);
    cursor: pointer;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    color: inherit;
    position: relative;
  }

  .kp-card-header:hover .kp-card-delete {
    opacity: 1;
  }

  .kp-card-indicator {
    width: 3px;
    min-height: 20px;
    border-radius: 2px;
    flex-shrink: 0;
    margin-top: 2px;
  }

  .kp-card-indicator.proposed { background: var(--info); }
  .kp-card-indicator.accepted { background: var(--success); }
  .kp-card-indicator.archived { background: var(--foreground-muted); }
  .kp-card-indicator.superseded { background: var(--warning); }
  .kp-card-indicator.default { background: var(--border); }
  .kp-card-indicator.faq { background: var(--color-gemini); }

  .kp-card-main {
    flex: 1;
    min-width: 0;
  }

  .kp-card-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    display: block;
  }

  .kp-card-preview {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin: 4px 0 0;
    line-height: var(--leading-normal);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .kp-card-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-shrink: 0;
    color: var(--foreground-muted);
  }

  .kp-card-delete {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    opacity: 0;
    transition: all var(--transition-fast);
  }

  .kp-card-delete:hover {
    background: var(--error-muted);
    color: var(--error);
  }

  /* ---- 卡片展开详情 ---- */
  .kp-card-body {
    padding: 0 var(--space-3) var(--space-3) calc(var(--space-3) + 3px + var(--space-2));
    border-top: 1px solid var(--border);
  }

  .kp-detail-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-2) 0;
  }

  .kp-detail-block {
    margin-top: var(--space-3);
  }

  .kp-detail-block h5 {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0 0 var(--space-2) 0;
    text-transform: uppercase;
    letter-spacing: 0.3px;
    opacity: 0.7;
  }

  .kp-detail-block p {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0;
    line-height: var(--leading-relaxed);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .kp-tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }

  .kp-tag {
    font-size: var(--text-2xs);
    padding: 1px 6px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
  }

  /* ---- 状态/分类徽章 ---- */
  .kp-status-badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    white-space: nowrap;
  }

  .kp-status-badge.proposed { background: var(--info-muted); color: var(--info); }
  .kp-status-badge.accepted { background: var(--success-muted); color: var(--success); }
  .kp-status-badge.archived { background: var(--surface-3); color: var(--foreground-muted); }
  .kp-status-badge.superseded { background: var(--warning-muted); color: var(--warning); }

  .kp-category-badge {
    font-size: 10px;
    padding: 1px 6px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  /* ---- 确认弹窗 ---- */
  .kp-confirm-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--overlay);
    z-index: var(--z-modal);
  }

  .kp-confirm-dialog {
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-5);
    max-width: 300px;
    width: 90%;
    box-shadow: var(--shadow-lg);
    text-align: center;
  }

  .kp-confirm-icon {
    color: var(--warning);
    margin-bottom: var(--space-3);
  }

  .kp-confirm-title {
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .kp-confirm-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0 0 var(--space-5) 0;
    line-height: var(--leading-normal);
  }

  .kp-confirm-actions {
    display: flex;
    justify-content: center;
    gap: var(--space-3);
  }

  .kp-confirm-btn {
    padding: var(--space-2) var(--space-5);
    font-size: var(--text-sm);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-confirm-btn--cancel {
    background: transparent;
    color: var(--foreground-muted);
  }

  .kp-confirm-btn--cancel:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-confirm-btn--danger {
    background: var(--error);
    border-color: var(--error);
    color: white;
  }

  .kp-confirm-btn--danger:hover {
    opacity: 0.9;
  }
</style>