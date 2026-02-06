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
  let selectedItem = $state<ADR | FAQ | null>(null);
  let selectedType = $state<'adr' | 'faq' | null>(null);

  // 统计信息
  const fileCount = $derived(codeIndex?.files?.length || 0);
  const totalLines = $derived(
    codeIndex?.files?.reduce((sum, f) => sum + (f.lines || 0), 0) || 0
  );

  // 过滤后的 ADR 列表
  const filteredAdrs = $derived.by(() => {
    let result = adrs;
    if (adrFilter !== 'all') {
      result = result.filter(adr => adr.status === adrFilter);
    }
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase();
      result = result.filter(adr =>
        (() => {
          if (!adr.title || typeof adr.title !== 'string') {
            vscode.postMessage({
              type: 'uiError',
              component: 'KnowledgePanel',
              detail: { kind: 'ADR', adr },
              stack: new Error('KnowledgePanel: invalid ADR title').stack,
            });
            throw new Error('KnowledgePanel: invalid ADR title');
          }
          return adr.title.toLowerCase().includes(query) ||
            adr.context?.toLowerCase().includes(query);
        })()
      );
    }
    return result;
  });

  // 过滤后的 FAQ 列表
  const filteredFaqs = $derived.by(() => {
    if (!searchQuery.trim()) return faqs;
    const query = searchQuery.toLowerCase();
    return faqs.filter(faq =>
      (() => {
        if (!faq.question || typeof faq.question !== 'string') {
          vscode.postMessage({
            type: 'uiError',
            component: 'KnowledgePanel',
            detail: { kind: 'FAQ', faq },
            stack: new Error('KnowledgePanel: invalid FAQ question').stack,
          });
          throw new Error('KnowledgePanel: invalid FAQ question');
        }
        return faq.question.toLowerCase().includes(query) ||
          faq.answer?.toLowerCase().includes(query);
      })()
    );
  });

  function switchTab(tabId: typeof currentTab) {
    currentTab = tabId;
    selectedItem = null;
    selectedType = null;
  }

  function refresh() {
    isLoading = true;
    vscode.postMessage({ type: 'getProjectKnowledge' });
  }

  function selectAdr(adr: ADR) {
    selectedItem = adr;
    selectedType = 'adr';
  }

  function selectFaq(faq: FAQ) {
    selectedItem = faq;
    selectedType = 'faq';
  }

  function closeDetail() {
    selectedItem = null;
    selectedType = null;
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
  <!-- 头部：Tab 栏 + 搜索 + 刷新 -->
  <div class="knowledge-header">
    <div class="knowledge-tabs">
      <button class="knowledge-tab" class:active={currentTab === 'overview'} onclick={() => switchTab('overview')}>
        <Icon name="stats" size={14} />
        <span>概览</span>
      </button>
      <button class="knowledge-tab" class:active={currentTab === 'adr'} onclick={() => switchTab('adr')}>
        <Icon name="document" size={14} />
        <span>ADR</span>
        {#if adrs.length > 0}
          <span class="knowledge-tab-badge">{adrs.length}</span>
        {/if}
      </button>
      <button class="knowledge-tab" class:active={currentTab === 'faq'} onclick={() => switchTab('faq')}>
        <Icon name="question" size={14} />
        <span>FAQ</span>
        {#if faqs.length > 0}
          <span class="knowledge-tab-badge">{faqs.length}</span>
        {/if}
      </button>
    </div>
    <div class="knowledge-actions">
      <div class="knowledge-search-box">
        <Icon name="search" size={14} class="knowledge-search-icon" />
        <input
          type="text"
          class="knowledge-search-input"
          placeholder="搜索..."
          bind:value={searchQuery}
        />
      </div>
      <button class="btn-icon btn-icon--md" onclick={refresh} disabled={isLoading} title="刷新">
        <Icon name="refresh" size={14} class={isLoading ? 'spinning' : ''} />
      </button>
    </div>
  </div>

  <!-- 主内容区：列表 + 详情 -->
  <div class="knowledge-main">
    <!-- 列表区域 -->
    <div class="knowledge-list-area" class:has-detail={selectedItem !== null}>
      {#if isLoading}
        <div class="loading-state">
          <div class="spinner"></div>
          <span>加载中...</span>
        </div>
      {:else if currentTab === 'overview'}
        <!-- 概览内容 -->
        <div class="knowledge-overview">
          <div class="knowledge-stats-grid">
            <div class="knowledge-stat-card">
              <div class="knowledge-stat-icon files">
                <Icon name="file" size={16} />
              </div>
              <div class="knowledge-stat-info">
                <div class="knowledge-stat-label">文件</div>
                <div class="knowledge-stat-value">{fileCount.toLocaleString()}</div>
              </div>
            </div>
            <div class="knowledge-stat-card">
              <div class="knowledge-stat-icon lines">
                <Icon name="grid" size={16} />
              </div>
              <div class="knowledge-stat-info">
                <div class="knowledge-stat-label">代码行</div>
                <div class="knowledge-stat-value">{totalLines.toLocaleString()}</div>
              </div>
            </div>
            <div class="knowledge-stat-card">
              <div class="knowledge-stat-icon adr">
                <Icon name="document" size={16} />
              </div>
              <div class="knowledge-stat-info">
                <div class="knowledge-stat-label">ADR</div>
                <div class="knowledge-stat-value">{adrs.length}</div>
              </div>
            </div>
            <div class="knowledge-stat-card">
              <div class="knowledge-stat-icon faq">
                <Icon name="question" size={16} />
              </div>
              <div class="knowledge-stat-info">
                <div class="knowledge-stat-label">FAQ</div>
                <div class="knowledge-stat-value">{faqs.length}</div>
              </div>
            </div>
          </div>

          {#if codeIndex?.techStack && codeIndex.techStack.length > 0}
            <div class="knowledge-section">
              <h4 class="knowledge-section-title">技术栈</h4>
              <div class="knowledge-tech-stack">
                {#each codeIndex.techStack as tech}
                  <span class="knowledge-tech-badge">{tech}</span>
                {/each}
              </div>
            </div>
          {/if}

          {#if codeIndex?.entryPoints && codeIndex.entryPoints.length > 0}
            <div class="knowledge-section">
              <h4 class="knowledge-section-title">入口文件</h4>
              <div class="knowledge-entry-list">
                {#each codeIndex.entryPoints as entry}
                  <div class="knowledge-entry-item">
                    <Icon name="file" size={12} />
                    <span>{entry}</span>
                  </div>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      {:else if currentTab === 'adr'}
        <!-- ADR 列表内容 -->
        <div class="knowledge-list-content">
          <div class="knowledge-filter-bar">
            <button class="knowledge-filter-btn" class:active={adrFilter === 'all'} onclick={() => adrFilter = 'all'}>全部</button>
            <button class="knowledge-filter-btn" class:active={adrFilter === 'proposed'} onclick={() => adrFilter = 'proposed'}>提议中</button>
            <button class="knowledge-filter-btn" class:active={adrFilter === 'accepted'} onclick={() => adrFilter = 'accepted'}>已接受</button>
            <button class="knowledge-filter-btn" class:active={adrFilter === 'archived'} onclick={() => adrFilter = 'archived'}>已归档</button>
            <button class="knowledge-filter-btn" class:active={adrFilter === 'superseded'} onclick={() => adrFilter = 'superseded'}>已替代</button>
          </div>
          <div class="knowledge-list">
            {#if filteredAdrs.length === 0}
              <div class="knowledge-empty">
                <Icon name="document" size={32} class="knowledge-empty-icon" />
                <div class="knowledge-empty-text">暂无架构决策记录</div>
                <div class="knowledge-empty-hint">完成任务后会自动提取</div>
              </div>
            {:else}
              {#each filteredAdrs as adr (adr.id)}
                <button type="button" class="knowledge-list-item" class:selected={selectedItem === adr} onclick={() => selectAdr(adr)}>
                  <div class="knowledge-item-header">
                    <span class="knowledge-item-title">{adr.title}</span>
                    {#if adr.status}
                      <span class="knowledge-item-status {adr.status}">{adr.status}</span>
                    {/if}
                  </div>
                  {#if adr.context}
                    <p class="knowledge-item-desc">{adr.context.substring(0, 100)}...</p>
                  {/if}
                  {#if adr.tags && adr.tags.length > 0}
                    <div class="knowledge-item-tags">
                      {#each adr.tags as tag}
                        <span class="knowledge-item-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </button>
              {/each}
            {/if}
          </div>
        </div>
      {:else if currentTab === 'faq'}
        <!-- FAQ 列表内容 -->
        <div class="knowledge-list-content">
          <div class="knowledge-list">
            {#if filteredFaqs.length === 0}
              <div class="knowledge-empty">
                <Icon name="question" size={32} class="knowledge-empty-icon" />
                <div class="knowledge-empty-text">暂无常见问题</div>
                <div class="knowledge-empty-hint">完成任务后会自动提取</div>
              </div>
            {:else}
              {#each filteredFaqs as faq (faq.id)}
                <button type="button" class="knowledge-list-item" class:selected={selectedItem === faq} onclick={() => selectFaq(faq)}>
                  <div class="knowledge-item-header">
                    <span class="knowledge-item-title">{faq.question}</span>
                    {#if faq.category}
                      <span class="knowledge-item-category">{faq.category}</span>
                    {/if}
                  </div>
                  {#if faq.answer}
                    <p class="knowledge-item-desc">{faq.answer.substring(0, 100)}...</p>
                  {/if}
                  {#if faq.tags && faq.tags.length > 0}
                    <div class="knowledge-item-tags">
                      {#each faq.tags as tag}
                        <span class="knowledge-item-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </button>
              {/each}
            {/if}
          </div>
        </div>
      {/if}
    </div>

    <!-- 详情面板 -->
    {#if selectedItem !== null}
      <div class="knowledge-detail-panel">
        <div class="knowledge-detail-header">
          <button class="btn-icon btn-icon--md" onclick={closeDetail} title="关闭">
            <Icon name="close" size={14} />
          </button>
        </div>
        <div class="knowledge-detail-content">
          {#if selectedType === 'adr' && selectedItem}
            {@const adr = selectedItem as ADR}
            <h3 class="knowledge-detail-title">{adr.title}</h3>
            {#if adr.status}
              <span class="knowledge-detail-status {adr.status}">{adr.status}</span>
            {/if}
            {#if adr.date}
              <div class="knowledge-detail-date">日期：{adr.date}</div>
            {/if}
            {#if adr.context}
              <div class="knowledge-detail-section">
                <h4>上下文</h4>
                <p>{adr.context}</p>
              </div>
            {/if}
            {#if adr.decision}
              <div class="knowledge-detail-section">
                <h4>决策</h4>
                <p>{adr.decision}</p>
              </div>
            {/if}
            {#if adr.consequences}
              <div class="knowledge-detail-section">
                <h4>后果</h4>
                <p>{adr.consequences}</p>
              </div>
            {/if}
            {#if adr.tags && adr.tags.length > 0}
              <div class="knowledge-detail-tags">
                {#each adr.tags as tag}
                  <span class="knowledge-item-tag">{tag}</span>
                {/each}
              </div>
            {/if}
          {:else if selectedType === 'faq' && selectedItem}
            {@const faq = selectedItem as FAQ}
            <h3 class="knowledge-detail-title">{faq.question}</h3>
            {#if faq.category}
              <span class="knowledge-detail-category">{faq.category}</span>
            {/if}
            {#if faq.answer}
              <div class="knowledge-detail-section">
                <h4>回答</h4>
                <p>{faq.answer}</p>
              </div>
            {/if}
            {#if faq.tags && faq.tags.length > 0}
              <div class="knowledge-detail-tags">
                {#each faq.tags as tag}
                  <span class="knowledge-item-tag">{tag}</span>
                {/each}
              </div>
            {/if}
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .knowledge-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  /* 头部区域 */
  .knowledge-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }

  .knowledge-tabs {
    display: flex;
    gap: var(--space-1);
  }

  .knowledge-tab {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .knowledge-tab:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .knowledge-tab.active {
    background: var(--background);
    color: var(--foreground);
  }

  .knowledge-tab-badge {
    font-size: var(--text-2xs);
    padding: var(--space-1) var(--space-2);
    background: var(--primary);
    color: white;
    border-radius: var(--radius-full);
    min-width: 16px;
    text-align: center;
  }

  .knowledge-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .knowledge-search-box {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }

  .knowledge-search-box :global(.knowledge-search-icon) {
    color: var(--foreground-muted);
    flex-shrink: 0;
  }

  .knowledge-search-input {
    border: none;
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
    width: 120px;
    outline: none;
  }

  .knowledge-search-input::placeholder {
    color: var(--foreground-muted);
  }

  .btn-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: var(--btn-height-sm);
    height: var(--btn-height-sm);
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .btn-icon:hover:not(:disabled) {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .btn-icon:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* 主内容区 */
  .knowledge-main {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  .knowledge-list-area {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .knowledge-list-area.has-detail {
    flex: 0 0 50%;
    border-right: 1px solid var(--border);
  }

  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    height: 100%;
    color: var(--foreground-muted);
  }

  .spinner {
    width: var(--icon-xl);
    height: var(--icon-xl);
    border: 2px solid var(--border);
    border-top-color: var(--primary);
    border-radius: var(--radius-full);
    animation: spin 1s linear infinite;
  }

  /* 概览区域 */
  .knowledge-overview {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .knowledge-stats-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: var(--space-3);
  }

  .knowledge-stat-card {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }

  .knowledge-stat-icon {
    width: var(--avatar-lg);
    height: var(--avatar-lg);
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius-md);
  }

  .knowledge-stat-icon.files { background: var(--info-muted); color: var(--info); }
  .knowledge-stat-icon.lines { background: var(--success-muted); color: var(--success); }
  .knowledge-stat-icon.adr { background: var(--primary-muted); color: var(--primary); }
  .knowledge-stat-icon.faq { background: var(--warning-muted); color: var(--warning); }

  .knowledge-stat-info {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .knowledge-stat-label {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
  }

  .knowledge-stat-value {
    font-size: var(--text-xl);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .knowledge-section {
    margin-top: var(--space-3);
  }

  .knowledge-section-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    margin: 0 0 var(--space-3) 0;
    color: var(--foreground);
  }

  .knowledge-tech-stack {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .knowledge-tech-badge {
    font-size: var(--text-xs);
    padding: var(--space-1) var(--space-3);
    background: var(--surface-2);
    color: var(--foreground);
    border-radius: var(--radius-full);
  }

  .knowledge-entry-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .knowledge-entry-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-1);
    border-radius: var(--radius-sm);
  }

  /* 列表内容 */
  .knowledge-list-content {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .knowledge-filter-bar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding-bottom: var(--space-3);
    border-bottom: 1px solid var(--border);
    margin-bottom: var(--space-3);
  }

  .knowledge-filter-btn {
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .knowledge-filter-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .knowledge-filter-btn.active {
    background: var(--primary);
    border-color: var(--primary);
    color: white;
  }

  .knowledge-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .knowledge-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-8);
    color: var(--foreground-muted);
    text-align: center;
  }

  .knowledge-empty :global(.knowledge-empty-icon) {
    opacity: 0.5;
  }

  .knowledge-empty-text {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .knowledge-empty-hint {
    font-size: var(--text-sm);
    opacity: 0.7;
  }

  .knowledge-list-item {
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .knowledge-list-item:hover {
    background: var(--surface-hover);
  }

  .knowledge-list-item.selected {
    background: var(--surface-selected);
    border-color: var(--primary);
  }

  .knowledge-item-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    margin-bottom: var(--space-2);
  }

  .knowledge-item-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .knowledge-item-status,
  .knowledge-item-category {
    font-size: var(--text-2xs);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
    text-transform: capitalize;
  }

  .knowledge-item-status.proposed { background: var(--info-muted); color: var(--info); }
  .knowledge-item-status.accepted { background: var(--success-muted); color: var(--success); }
  .knowledge-item-status.archived { background: var(--error-muted); color: var(--error); }
  .knowledge-item-status.superseded { background: var(--warning-muted); color: var(--warning); }
  .knowledge-item-category { background: var(--surface-2); color: var(--foreground-muted); }

  .knowledge-item-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0;
    line-height: var(--leading-normal);
  }

  .knowledge-item-tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .knowledge-item-tag {
    font-size: var(--text-2xs);
    padding: var(--space-1) var(--space-2);
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
  }

  /* 详情面板 */
  .knowledge-detail-panel {
    flex: 0 0 50%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--background);
  }

  .knowledge-detail-header {
    display: flex;
    justify-content: flex-end;
    padding: var(--space-3);
    border-bottom: 1px solid var(--border);
  }

  .knowledge-detail-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .knowledge-detail-title {
    font-size: var(--text-lg);
    font-weight: var(--font-semibold);
    margin: 0 0 var(--space-3) 0;
    color: var(--foreground);
  }

  .knowledge-detail-status,
  .knowledge-detail-category {
    display: inline-block;
    font-size: var(--text-xs);
    padding: var(--space-1) var(--space-3);
    border-radius: var(--radius-sm);
    margin-bottom: var(--space-3);
    text-transform: capitalize;
  }

  .knowledge-detail-status.proposed { background: var(--info-muted); color: var(--info); }
  .knowledge-detail-status.accepted { background: var(--success-muted); color: var(--success); }
  .knowledge-detail-status.archived { background: var(--error-muted); color: var(--error); }
  .knowledge-detail-status.superseded { background: var(--warning-muted); color: var(--warning); }
  .knowledge-detail-category { background: var(--surface-2); color: var(--foreground-muted); }

  .knowledge-detail-date {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin-bottom: var(--space-4);
  }

  .knowledge-detail-section {
    margin-bottom: var(--space-4);
  }

  .knowledge-detail-section h4 {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    margin: 0 0 var(--space-2) 0;
    color: var(--foreground);
  }

  .knowledge-detail-section p {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0;
    line-height: var(--leading-relaxed);
    white-space: pre-wrap;
  }

  .knowledge-detail-tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-4);
  }
</style>
