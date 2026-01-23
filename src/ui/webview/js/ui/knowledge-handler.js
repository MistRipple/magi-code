// 知识 Tab 处理模块
// 处理项目知识的加载、显示和交互
// 新设计：Tab + 列表 + 详情面板

import { postMessage } from '../core/vscode-api.js';
import { escapeHtml } from '../core/utils.js';
import { showToast } from './message-handler.js';

// ============================================
// 状态管理
// ============================================

let state = {
  currentTab: 'overview',        // 当前激活的 Tab: 'overview' | 'adr' | 'faq'
  selectedItemId: null,          // 当前选中的列表项 ID
  isDetailPanelOpen: false,      // 详情面板是否打开
  currentFilter: 'all',          // 当前 ADR 过滤状态
  currentSearchQuery: '',        // 当前搜索关键词
  projectKnowledge: {
    codeIndex: null,
    adrs: [],
    faqs: []
  },
  isKnowledgeLoaded: false
};

// ============================================
// Tab 切换逻辑
// ============================================

export function switchTab(tabName) {
  if (state.currentTab === tabName) return;

  state.currentTab = tabName;

  // 更新 Tab 按钮状态
  document.querySelectorAll('.knowledge-tab').forEach(tab => {
    const isActive = tab.dataset.knowledgeTab === tabName;
    tab.classList.toggle('active', isActive);
  });

  // 更新内容区域显示
  document.querySelectorAll('.knowledge-list-content').forEach(content => {
    content.classList.remove('active');
  });

  const activeContent = document.getElementById(`knowledge-${tabName}-content`);
  if (activeContent) {
    activeContent.classList.add('active');
  }

  // 关闭详情面板
  closeDetailPanel();

  // 如果还没加载数据，加载数据
  if (!state.isKnowledgeLoaded) {
    loadProjectKnowledge();
  } else {
    // 已加载，根据 Tab 渲染对应内容
    renderCurrentTab();
  }
}

function renderCurrentTab() {
  switch (state.currentTab) {
    case 'overview':
      renderOverview();
      break;
    case 'adr':
      renderADRList();
      break;
    case 'faq':
      renderFAQList();
      break;
  }
}

// ============================================
// 加载项目知识
// ============================================

export function loadProjectKnowledge() {
  if (state.isKnowledgeLoaded) {
    // 已加载，直接渲染
    renderCurrentTab();
    return;
  }

  // 显示加载状态
  showLoadingState();

  // 请求后端加载数据
  postMessage({ type: 'getProjectKnowledge' });
}

function showLoadingState() {
  // 代码索引加载状态
  const indexContent = document.getElementById('knowledge-index-content');
  if (indexContent) {
    indexContent.innerHTML = `
      <div class="knowledge-loading">
        <div class="knowledge-loading-spinner"></div>
        <div class="knowledge-loading-text">加载代码索引...</div>
      </div>
    `;
  }

  // ADR 加载状态
  const adrList = document.getElementById('knowledge-adr-list');
  if (adrList) {
    adrList.innerHTML = `
      <div class="knowledge-loading">
        <div class="knowledge-loading-spinner"></div>
        <div class="knowledge-loading-text">加载架构决策记录...</div>
      </div>
    `;
  }

  // FAQ 加载状态
  const faqList = document.getElementById('knowledge-faq-list');
  if (faqList) {
    faqList.innerHTML = `
      <div class="knowledge-loading">
        <div class="knowledge-loading-spinner"></div>
        <div class="knowledge-loading-text">加载常见问题...</div>
      </div>
    `;
  }
}

// ============================================
// 处理后端响应
// ============================================

export function handleProjectKnowledgeLoaded(codeIndex, adrs, faqs) {
  state.projectKnowledge.codeIndex = codeIndex;
  state.projectKnowledge.adrs = adrs || [];
  state.projectKnowledge.faqs = faqs || [];
  state.isKnowledgeLoaded = true;

  // 恢复刷新按钮状态
  const refreshBtn = document.getElementById('knowledge-refresh-btn');
  if (refreshBtn) {
    refreshBtn.classList.remove('loading');
    refreshBtn.disabled = false;
  }

  // 更新徽章计数
  updateTabBadges();

  // 渲染当前 Tab
  renderCurrentTab();
}

export function handleADRsLoaded(adrs) {
  state.projectKnowledge.adrs = adrs || [];
  updateTabBadges();
  if (state.currentTab === 'adr') {
    renderADRList();
  }
}

export function handleFAQsLoaded(faqs) {
  state.projectKnowledge.faqs = faqs || [];
  updateTabBadges();
  if (state.currentTab === 'faq') {
    renderFAQList();
  }
}

export function handleFAQSearchResults(results) {
  if (state.currentTab === 'faq') {
    renderFAQList(results);
  }
}

export function handleADRDeleted(id) {
  // 从状态中移除
  state.projectKnowledge.adrs = state.projectKnowledge.adrs.filter(adr => adr.id !== id);

  // 如果删除的是当前选中的项，关闭详情面板
  if (state.selectedItemId === id) {
    closeDetailPanel();
  }

  // 更新徽章和列表
  updateTabBadges();
  if (state.currentTab === 'adr') {
    renderADRList();
  }

  // 更新概览统计
  if (state.currentTab === 'overview') {
    renderOverview();
  }

  showToast('ADR 已删除', 'success');
}

export function handleFAQDeleted(id) {
  // 从状态中移除
  state.projectKnowledge.faqs = state.projectKnowledge.faqs.filter(faq => faq.id !== id);

  // 如果删除的是当前选中的项，关闭详情面板
  if (state.selectedItemId === id) {
    closeDetailPanel();
  }

  // 更新徽章和列表
  updateTabBadges();
  if (state.currentTab === 'faq') {
    renderFAQList();
  }

  // 更新概览统计
  if (state.currentTab === 'overview') {
    renderOverview();
  }

  showToast('FAQ 已删除', 'success');
}

function updateTabBadges() {
  // 更新 ADR 徽章
  const adrTab = document.querySelector('.knowledge-tab[data-knowledge-tab="adr"]');
  if (adrTab) {
    const badge = adrTab.querySelector('.knowledge-tab-badge');
    if (badge) {
      badge.textContent = state.projectKnowledge.adrs.length;
    }
  }

  // 更新 FAQ 徽章
  const faqTab = document.querySelector('.knowledge-tab[data-knowledge-tab="faq"]');
  if (faqTab) {
    const badge = faqTab.querySelector('.knowledge-tab-badge');
    if (badge) {
      badge.textContent = state.projectKnowledge.faqs.length;
    }
  }
}

// ============================================
// 渲染函数 - 概览页面
// ============================================

function renderOverview() {
  const codeIndex = state.projectKnowledge.codeIndex;
  const adrs = state.projectKnowledge.adrs;
  const faqs = state.projectKnowledge.faqs;

  // 更新统计卡片
  const fileCount = codeIndex?.files ? codeIndex.files.length : 0;
  const totalLines = codeIndex?.files ? codeIndex.files.reduce((sum, f) => sum + (f.lines || 0), 0) : 0;

  document.getElementById('stat-files').textContent = fileCount.toLocaleString();
  document.getElementById('stat-lines').textContent = totalLines.toLocaleString();
  document.getElementById('stat-adrs').textContent = adrs.length;
  document.getElementById('stat-faqs').textContent = faqs.length;

  // 渲染详细信息
  const detailsContainer = document.getElementById('knowledge-overview-details');
  if (!detailsContainer) return;

  if (!codeIndex) {
    detailsContainer.innerHTML = `
      <div class="knowledge-empty">
        <svg class="knowledge-empty-icon" viewBox="0 0 16 16">
          <path d="M9.4 16.6L4.8 12l4.6-4.6L8 6l-6 6 6 6 1.4-1.4zm5.2 0l4.6-4.6-4.6-4.6L16 6l6 6-6 6-1.4-1.4z"/>
        </svg>
        <div class="knowledge-empty-text">暂无代码索引</div>
        <div class="knowledge-empty-hint">项目代码索引将在首次扫描后显示</div>
      </div>
    `;
    return;
  }

  const techStack = codeIndex.techStack || [];
  const entryPoints = codeIndex.entryPoints || [];
  const files = codeIndex.files || [];

  let html = '';

  // 技术栈
  if (techStack.length > 0) {
    html += `
      <div class="knowledge-overview-section">
        <div class="knowledge-overview-section-title">技术栈</div>
        <div class="knowledge-tech-stack">
          ${techStack.map(tech => `
            <span class="knowledge-tech-badge">${escapeHtml(tech)}</span>
          `).join('')}
        </div>
      </div>
    `;
  }

  // 关键文件
  const keyFiles = getKeyFiles(files, entryPoints);
  if (keyFiles.length > 0) {
    html += `
      <div class="knowledge-overview-section">
        <div class="knowledge-overview-section-header">
          <div class="knowledge-overview-section-title">关键文件</div>
          <div class="knowledge-overview-section-subtitle">显示 ${keyFiles.length} 个重要文件，共 ${files.length} 个文件</div>
        </div>
        <div class="knowledge-key-files">
          ${keyFiles.map(file => renderKeyFile(file)).join('')}
        </div>
      </div>
    `;
  }

  detailsContainer.innerHTML = html;
}

function getKeyFiles(files, entryPoints) {
  const keyFiles = [];
  const addedPaths = new Set();

  // 1. 添加入口文件
  entryPoints.forEach(entryPath => {
    const file = files.find(f => f.path === entryPath);
    if (file && !addedPaths.has(file.path)) {
      keyFiles.push({ ...file, type: 'entry', icon: 'file' });
      addedPaths.add(file.path);
    }
  });

  // 2. 添加配置文件
  const configPatterns = [
    'package.json',
    'tsconfig.json',
    'vite.config',
    'webpack.config',
    'rollup.config',
    '.eslintrc',
    '.prettierrc',
    'jest.config',
    'vitest.config'
  ];

  files.forEach(file => {
    const fileName = file.path.split('/').pop() || '';
    const isConfig = configPatterns.some(pattern => fileName.includes(pattern));
    if (isConfig && !addedPaths.has(file.path)) {
      keyFiles.push({ ...file, type: 'config', icon: 'gear' });
      addedPaths.add(file.path);
    }
  });

  // 3. 添加文档文件
  const docPatterns = ['README', 'CHANGELOG', 'LICENSE', 'CONTRIBUTING'];
  files.forEach(file => {
    const fileName = file.path.split('/').pop() || '';
    const isDoc = docPatterns.some(pattern => fileName.toUpperCase().includes(pattern));
    if (isDoc && !addedPaths.has(file.path)) {
      keyFiles.push({ ...file, type: 'doc', icon: 'book' });
      addedPaths.add(file.path);
    }
  });

  // 限制最多显示 15 个文件
  return keyFiles.slice(0, 15);
}

function renderKeyFile(file) {
  const fileName = file.path.split('/').pop() || file.path;
  const fileSize = file.size ? formatFileSize(file.size) : '';
  const lines = file.lines ? `${file.lines} 行` : '';

  let typeLabel = '';
  if (file.type === 'entry') {
    typeLabel = '<span class="knowledge-file-type-badge entry">入口</span>';
  } else if (file.type === 'config') {
    typeLabel = '<span class="knowledge-file-type-badge config">配置</span>';
  } else if (file.type === 'doc') {
    typeLabel = '<span class="knowledge-file-type-badge doc">文档</span>';
  }

  // SVG 图标
  let iconSvg = '';
  if (file.icon === 'file') {
    iconSvg = '<svg class="knowledge-key-file-icon-svg" viewBox="0 0 16 16"><path d="M9.5 1.1l3.4 3.5.1.4v8c0 .4-.3.8-.8.8H3.8c-.4 0-.8-.3-.8-.8V1.9c0-.4.3-.8.8-.8h5.3c.2 0 .3.1.4.2zM9 2H4v11h8V5H9V2z"/></svg>';
  } else if (file.icon === 'gear') {
    iconSvg = '<svg class="knowledge-key-file-icon-svg" viewBox="0 0 16 16"><path d="M9.1 4.4L8.6 2H7.4l-.5 2.4-.7.3-2-1.3-.9.8 1.3 2-.3.7-2.4.5v1.2l2.4.5.3.7-1.3 2 .8.9 2-1.3.7.3.5 2.4h1.2l.5-2.4.7-.3 2 1.3.9-.8-1.3-2 .3-.7 2.4-.5V7.4l-2.4-.5-.3-.7 1.3-2-.8-.9-2 1.3-.7-.3zM8 11c-1.7 0-3-1.3-3-3s1.3-3 3-3 3 1.3 3 3-1.3 3-3 3z"/></svg>';
  } else if (file.icon === 'book') {
    iconSvg = '<svg class="knowledge-key-file-icon-svg" viewBox="0 0 16 16"><path d="M3 2.5C3 1.7 3.7 1 4.5 1h7c.8 0 1.5.7 1.5 1.5v11c0 .8-.7 1.5-1.5 1.5h-7c-.8 0-1.5-.7-1.5-1.5v-11zM4.5 2c-.3 0-.5.2-.5.5v11c0 .3.2.5.5.5h7c.3 0 .5-.2.5-.5v-11c0-.3-.2-.5-.5-.5h-7zM5 4h6v1H5V4zm0 2h6v1H5V6zm0 2h4v1H5V8z"/></svg>';
  }

  return `
    <div class="knowledge-key-file-item" data-path="${escapeHtml(file.path)}" title="${escapeHtml(file.path)}">
      <div class="knowledge-key-file-icon">${iconSvg}</div>
      <div class="knowledge-key-file-info">
        <div class="knowledge-key-file-name">
          ${escapeHtml(fileName)}
          ${typeLabel}
        </div>
        <div class="knowledge-key-file-path">${escapeHtml(file.path)}</div>
      </div>
      <div class="knowledge-key-file-meta">
        ${lines ? `<span>${lines}</span>` : ''}
        ${fileSize ? `<span>${fileSize}</span>` : ''}
      </div>
    </div>
  `;
}

function formatFileSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
}

function handleKeyFileClick(path) {
  // 发送消息到后端，在编辑器中打开文件
  postMessage({ type: 'openFile', filepath: path });
}

// ============================================
// 渲染函数 - ADR 列表
// ============================================

function renderADRList(filteredADRs) {
  const container = document.getElementById('knowledge-adr-list');
  if (!container) return;

  const adrs = filteredADRs || state.projectKnowledge.adrs;

  if (!adrs || adrs.length === 0) {
    container.innerHTML = `
      <div class="knowledge-empty">
        <svg class="knowledge-empty-icon" viewBox="0 0 24 24" fill="currentColor">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6zm4 18H6V4h7v5h5v11z"/>
        </svg>
        <div class="knowledge-empty-text">暂无架构决策记录</div>
        <div class="knowledge-empty-hint">架构决策记录将从会话中自动提取</div>
      </div>
    `;
    return;
  }

  const html = adrs.map(adr => {
    const statusClass = adr.status || 'proposed';
    const statusText = getADRStatusText(adr.status);
    const date = adr.date ? new Date(adr.date).toLocaleDateString('zh-CN') : '';
    const description = adr.context ? adr.context.substring(0, 80) + (adr.context.length > 80 ? '...' : '') : '';

    return `
      <div class="knowledge-list-item" data-id="${escapeHtml(adr.id)}" data-type="adr">
        <div class="knowledge-list-item-content">
          <div class="knowledge-list-item-header">
            <h4 class="knowledge-list-item-title">${escapeHtml(adr.title)}</h4>
            <span class="knowledge-list-item-badge ${statusClass}">${statusText}</span>
          </div>
          ${description ? `<div class="knowledge-list-item-desc">${escapeHtml(description)}</div>` : ''}
          <div class="knowledge-list-item-meta">
            ${date ? `<span>${date}</span>` : ''}
            ${adr.tags && adr.tags.length > 0 ? `<span>${adr.tags.join(' · ')}</span>` : ''}
          </div>
        </div>
        <div class="knowledge-list-item-actions">
          <button class="knowledge-item-delete-btn" data-id="${escapeHtml(adr.id)}" data-type="adr" title="删除">
            <svg viewBox="0 0 16 16">
              <path d="M5.5 5.5A.5.5 0 0 1 6 6v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm2.5 0a.5.5 0 0 1 .5.5v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm3 .5a.5.5 0 0 0-1 0v6a.5.5 0 0 0 1 0V6z"/>
              <path d="M14.5 3a1 1 0 0 1-1 1H13v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4h-.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1H6a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1h3.5a1 1 0 0 1 1 1v1zM4.118 4 4 4.059V13a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V4.059L11.882 4H4.118zM2.5 3V2h11v1h-11z"/>
            </svg>
          </button>
        </div>
      </div>
    `;
  }).join('');

  container.innerHTML = html;

  // 添加点击事件监听
  container.querySelectorAll('.knowledge-list-item').forEach(item => {
    item.addEventListener('click', (e) => {
      // 如果点击的是删除按钮，不触发列表项点击
      if (e.target.closest('.knowledge-item-delete-btn')) {
        return;
      }
      const itemId = item.dataset.id;
      const itemType = item.dataset.type;
      handleListItemClick(itemId, itemType);
    });
  });

  // 添加删除按钮事件监听
  container.querySelectorAll('.knowledge-item-delete-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const id = btn.dataset.id;
      handleDeleteADR(id);
    });
  });
}

function getADRStatusText(status) {
  const statusMap = {
    proposed: '提议中',
    accepted: '已接受',
    deprecated: '已废弃',
    superseded: '已替代'
  };
  return statusMap[status] || status || '提议中';
}

// ============================================
// 渲染函数 - FAQ 列表
// ============================================

function renderFAQList(filteredFAQs) {
  const container = document.getElementById('knowledge-faq-list');
  if (!container) return;

  const faqs = filteredFAQs || state.projectKnowledge.faqs;

  if (!faqs || faqs.length === 0) {
    container.innerHTML = `
      <div class="knowledge-empty">
        <svg class="knowledge-empty-icon" viewBox="0 0 24 24" fill="currentColor">
          <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 17h-2v-2h2v2zm2.07-7.75l-.9.92C13.45 12.9 13 13.5 13 15h-2v-.5c0-1.1.45-2.1 1.17-2.83l1.24-1.26c.37-.36.59-.86.59-1.41 0-1.1-.9-2-2-2s-2 .9-2 2H8c0-2.21 1.79-4 4-4s4 1.79 4 4c0 .88-.36 1.68-.93 2.25z"/>
        </svg>
        <div class="knowledge-empty-text">暂无常见问题</div>
        <div class="knowledge-empty-hint">常见问题将从会话中自动提取</div>
      </div>
    `;
    return;
  }

  const html = faqs.map(faq => {
    const tags = faq.tags || [];
    const category = faq.category || 'general';
    const answer = faq.answer ? faq.answer.substring(0, 80) + (faq.answer.length > 80 ? '...' : '') : '';

    return `
      <div class="knowledge-list-item" data-id="${escapeHtml(faq.id)}" data-type="faq">
        <div class="knowledge-list-item-content">
          <div class="knowledge-list-item-header">
            <h4 class="knowledge-list-item-title">${escapeHtml(faq.question)}</h4>
          </div>
          ${answer ? `<div class="knowledge-list-item-desc">${escapeHtml(answer)}</div>` : ''}
          <div class="knowledge-list-item-meta">
            <span>${escapeHtml(category)}</span>
            ${tags.length > 0 ? `
              <div class="knowledge-list-item-tags">
                ${tags.map(tag => `<span class="knowledge-list-item-tag">${escapeHtml(tag)}</span>`).join('')}
              </div>
            ` : ''}
            ${faq.useCount ? `<span>使用 ${faq.useCount} 次</span>` : ''}
          </div>
        </div>
        <div class="knowledge-list-item-actions">
          <button class="knowledge-item-delete-btn" data-id="${escapeHtml(faq.id)}" data-type="faq" title="删除">
            <svg viewBox="0 0 16 16">
              <path d="M5.5 5.5A.5.5 0 0 1 6 6v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm2.5 0a.5.5 0 0 1 .5.5v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm3 .5a.5.5 0 0 0-1 0v6a.5.5 0 0 0 1 0V6z"/>
              <path d="M14.5 3a1 1 0 0 1-1 1H13v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4h-.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1H6a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1h3.5a1 1 0 0 1 1 1v1zM4.118 4 4 4.059V13a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V4.059L11.882 4H4.118zM2.5 3V2h11v1h-11z"/>
            </svg>
          </button>
        </div>
      </div>
    `;
  }).join('');

  container.innerHTML = html;

  // 添加点击事件监听
  container.querySelectorAll('.knowledge-list-item').forEach(item => {
    item.addEventListener('click', (e) => {
      // 如果点击的是删除按钮，不触发列表项点击
      if (e.target.closest('.knowledge-item-delete-btn')) {
        return;
      }
      const itemId = item.dataset.id;
      const itemType = item.dataset.type;
      handleListItemClick(itemId, itemType);
    });
  });

  // 添加删除按钮事件监听
  container.querySelectorAll('.knowledge-item-delete-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const id = btn.dataset.id;
      handleDeleteFAQ(id);
    });
  });

  // 添加点击事件监听
  container.querySelectorAll('.knowledge-list-item').forEach(item => {
    item.addEventListener('click', () => {
      const itemId = item.dataset.id;
      const itemType = item.dataset.type;
      handleListItemClick(itemId, itemType);
    });
  });
}

// ============================================
// 详情面板逻辑
// ============================================

function handleListItemClick(itemId, itemType) {
  // 更新选中状态
  document.querySelectorAll('.knowledge-list-item').forEach(item => {
    item.classList.remove('selected');
  });

  const clickedItem = document.querySelector(`.knowledge-list-item[data-id="${itemId}"]`);
  if (clickedItem) {
    clickedItem.classList.add('selected');
  }

  // 打开详情面板
  openDetailPanel(itemId, itemType);
}

function openDetailPanel(itemId, itemType) {
  state.selectedItemId = itemId;
  state.isDetailPanelOpen = true;

  const panel = document.getElementById('knowledge-detail-panel');
  if (!panel) return;

  panel.classList.add('open');

  // 根据类型渲染详情
  if (itemType === 'adr') {
    const adr = state.projectKnowledge.adrs.find(a => a.id === itemId);
    if (adr) {
      renderADRDetail(adr);
    }
  } else if (itemType === 'faq') {
    const faq = state.projectKnowledge.faqs.find(f => f.id === itemId);
    if (faq) {
      renderFAQDetail(faq);
    }
  }
}

function closeDetailPanel() {
  state.selectedItemId = null;
  state.isDetailPanelOpen = false;

  const panel = document.getElementById('knowledge-detail-panel');
  if (panel) {
    panel.classList.remove('open');
  }

  // 移除所有选中状态
  document.querySelectorAll('.knowledge-list-item').forEach(item => {
    item.classList.remove('selected');
  });
}

function renderADRDetail(adr) {
  const content = document.getElementById('knowledge-detail-content');
  if (!content) return;

  const statusClass = adr.status || 'proposed';
  const statusText = getADRStatusText(adr.status);
  const date = adr.date ? new Date(adr.date).toLocaleDateString('zh-CN') : '';

  const html = `
    <h3 class="knowledge-detail-title">${escapeHtml(adr.title)}</h3>

    <div class="knowledge-detail-meta">
      <div class="knowledge-detail-meta-item">
        <span class="knowledge-detail-meta-label">状态:</span>
        <span class="knowledge-list-item-badge ${statusClass}">${statusText}</span>
      </div>
      ${date ? `
        <div class="knowledge-detail-meta-item">
          <span class="knowledge-detail-meta-label">日期:</span>
          <span class="knowledge-detail-meta-value">${date}</span>
        </div>
      ` : ''}
      ${adr.tags && adr.tags.length > 0 ? `
        <div class="knowledge-detail-meta-item">
          <span class="knowledge-detail-meta-label">标签:</span>
          <div class="knowledge-detail-tags">
            ${adr.tags.map(tag => `<span class="knowledge-detail-tag">${escapeHtml(tag)}</span>`).join('')}
          </div>
        </div>
      ` : ''}
    </div>

    <div class="knowledge-detail-section">
      <h4 class="knowledge-detail-section-title">背景</h4>
      <div class="knowledge-detail-section-content">${escapeHtml(adr.context || '')}</div>
    </div>

    <div class="knowledge-detail-section">
      <h4 class="knowledge-detail-section-title">决策</h4>
      <div class="knowledge-detail-section-content">${escapeHtml(adr.decision || '')}</div>
    </div>

    <div class="knowledge-detail-section">
      <h4 class="knowledge-detail-section-title">影响</h4>
      <div class="knowledge-detail-section-content">${escapeHtml(adr.consequences || '')}</div>
    </div>

    ${adr.alternatives && adr.alternatives.length > 0 ? `
      <div class="knowledge-detail-section">
        <h4 class="knowledge-detail-section-title">替代方案</h4>
        <ul class="knowledge-detail-list">
          ${adr.alternatives.map(alt => `<li>${escapeHtml(alt)}</li>`).join('')}
        </ul>
      </div>
    ` : ''}
  `;

  content.innerHTML = html;
}

function renderFAQDetail(faq) {
  const content = document.getElementById('knowledge-detail-content');
  if (!content) return;

  const category = faq.category || 'general';
  const tags = faq.tags || [];

  const html = `
    <h3 class="knowledge-detail-title">${escapeHtml(faq.question)}</h3>

    <div class="knowledge-detail-meta">
      <div class="knowledge-detail-meta-item">
        <span class="knowledge-detail-meta-label">分类:</span>
        <span class="knowledge-detail-meta-value">${escapeHtml(category)}</span>
      </div>
      ${faq.useCount ? `
        <div class="knowledge-detail-meta-item">
          <span class="knowledge-detail-meta-label">使用次数:</span>
          <span class="knowledge-detail-meta-value">${faq.useCount}</span>
        </div>
      ` : ''}
      ${tags.length > 0 ? `
        <div class="knowledge-detail-meta-item">
          <span class="knowledge-detail-meta-label">标签:</span>
          <div class="knowledge-detail-tags">
            ${tags.map(tag => `<span class="knowledge-detail-tag">${escapeHtml(tag)}</span>`).join('')}
          </div>
        </div>
      ` : ''}
    </div>

    <div class="knowledge-detail-section">
      <h4 class="knowledge-detail-section-title">回答</h4>
      <div class="knowledge-detail-section-content">${escapeHtml(faq.answer || '')}</div>
    </div>
  `;

  content.innerHTML = html;
}

// ============================================
// 交互处理函数
// ============================================

function handleSearch(query) {
  state.currentSearchQuery = query.trim();

  if (!state.currentSearchQuery) {
    // 空搜索，显示全部
    renderCurrentTab();
    return;
  }

  // 根据当前 Tab 执行搜索
  switch (state.currentTab) {
    case 'overview':
      // 概览页不支持搜索，显示提示
      showToast('概览页暂不支持搜索', 'info');
      break;

    case 'adr':
      // 搜索 ADR
      const filteredADRs = state.projectKnowledge.adrs.filter(adr => {
        const searchText = `${adr.title} ${adr.context} ${adr.decision}`.toLowerCase();
        return searchText.includes(state.currentSearchQuery.toLowerCase());
      });
      renderADRList(filteredADRs);
      break;

    case 'faq':
      // 使用后端搜索 FAQ
      postMessage({ type: 'searchFAQs', keyword: state.currentSearchQuery });
      break;
  }
}

function handleFilterChange(status) {
  state.currentFilter = status;

  // 更新过滤按钮状态
  document.querySelectorAll('.knowledge-filter-btn').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.filter === status);
  });

  // 请求过滤后的数据
  if (status === 'all') {
    postMessage({ type: 'getADRs' });
  } else {
    postMessage({ type: 'getADRs', filter: { status } });
  }
}

function handleRefresh() {
  // 设置刷新按钮 loading 状态
  const refreshBtn = document.getElementById('knowledge-refresh-btn');
  if (refreshBtn) {
    refreshBtn.classList.add('loading');
    refreshBtn.disabled = true;
  }

  // 清除缓存
  state.isKnowledgeLoaded = false;
  state.projectKnowledge = {
    codeIndex: null,
    adrs: [],
    faqs: []
  };

  // 关闭详情面板
  closeDetailPanel();

  // 重新加载
  loadProjectKnowledge();

  showToast('正在刷新知识库...', 'info');
}

// ============================================
// 删除功能
// ============================================

function handleDeleteADR(id) {
  const adr = state.projectKnowledge.adrs.find(a => a.id === id);
  if (!adr) return;

  if (confirm(`确定要删除 ADR "${adr.title}" 吗？\n\n此操作不可撤销。`)) {
    postMessage({ type: 'deleteADR', id });
    showToast('正在删除...', 'info');
  }
}

function handleDeleteFAQ(id) {
  const faq = state.projectKnowledge.faqs.find(f => f.id === id);
  if (!faq) return;

  if (confirm(`确定要删除 FAQ "${faq.question}" 吗？\n\n此操作不可撤销。`)) {
    postMessage({ type: 'deleteFAQ', id });
    showToast('正在删除...', 'info');
  }
}

// ============================================
// 初始化事件监听器
// ============================================

export function initializeKnowledgeEventListeners() {
  // Tab 切换
  document.querySelectorAll('.knowledge-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      const tabName = tab.dataset.knowledgeTab;
      if (tabName) {
        switchTab(tabName);
      }
    });
  });

  // 详情面板关闭按钮
  const closeBtn = document.getElementById('knowledge-detail-close');
  if (closeBtn) {
    closeBtn.addEventListener('click', () => {
      closeDetailPanel();
    });
  }

  // 搜索框
  const searchInput = document.getElementById('knowledge-search-input');
  if (searchInput) {
    let searchTimeout = null;
    searchInput.addEventListener('input', (e) => {
      const query = e.target.value;

      // 防抖处理
      if (searchTimeout) {
        clearTimeout(searchTimeout);
      }

      searchTimeout = setTimeout(() => {
        handleSearch(query);
      }, 300); // 300ms 防抖
    });
  }

  // 刷新按钮
  const refreshBtn = document.getElementById('knowledge-refresh-btn');
  if (refreshBtn) {
    refreshBtn.addEventListener('click', () => {
      handleRefresh();
    });
  }

  // 过滤按钮
  document.querySelectorAll('.knowledge-filter-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      const filter = btn.dataset.filter;
      if (filter) {
        handleFilterChange(filter);
      }
    });
  });

  // 关键文件点击（使用事件委托）
  const overviewDetails = document.getElementById('knowledge-overview-details');
  if (overviewDetails) {
    overviewDetails.addEventListener('click', (e) => {
      const fileItem = e.target.closest('.knowledge-key-file-item');
      if (fileItem) {
        const path = fileItem.dataset.path;
        if (path) {
          handleKeyFileClick(path);
        }
      }
    });
  }
}

// ============================================
// 导出状态（用于调试）
// ============================================

export function getKnowledgeState() {
  return {
    ...state
  };
}
