// 卡片渲染模块
// 负责各种卡片组件的渲染：任务卡片、计划卡片、问题卡片、工具调用等

import { escapeHtml, formatRelativeTime, getToolIcon } from './render-utils.js';
import { renderMarkdown } from './markdown-renderer.js';

// ============================================
// 统一卡片渲染
// ============================================

export function renderUnifiedCard(options) {
  const {
    type = 'default',
    variant = '',
    icon = '',
    title = '',
    badges = [],
    time = '',
    content = '',
    footer = '',
    collapsed = false,
    expanded = true,
    panelId = '',
    dataAttrs = {},
    className = ''
  } = options;

  const variantClass = variant ? ' card-' + variant : '';
  const typeClass = ' card-type-' + type;
  const collapsedClass = collapsed ? ' collapsible-panel' : '';
  const extraClass = className ? ' ' + className : '';

  let dataStr = '';
  if (panelId) dataStr += ' data-panel-id="' + panelId + '"';
  Object.entries(dataAttrs).forEach(([key, val]) => {
    dataStr += ' data-' + key + '="' + escapeHtml(String(val)) + '"';
  });

  let html = '<div class="unified-card' + typeClass + variantClass + collapsedClass + extraClass + '"' + dataStr + '>';

  // 卡片头部
  if (title || icon || badges.length || time) {
    const headerClick = collapsed && panelId ? ' onclick="togglePanel(\'' + panelId + '\')"' : '';
    html += '<div class="card-header"' + headerClick + '>';
    if (icon) {
      html += '<span class="card-icon">' + icon + '</span>';
    }
    if (title) {
      html += '<span class="card-title">' + escapeHtml(title) + '</span>';
    }
    if (badges.length > 0) {
      html += '<span class="card-badges">';
      badges.forEach(badge => {
        const badgeClass = badge.class ? ' ' + badge.class : '';
        html += '<span class="card-badge' + badgeClass + '">' + escapeHtml(badge.text) + '</span>';
      });
      html += '</span>';
    }
    if (time) {
      html += '<span class="card-time">' + escapeHtml(time) + '</span>';
    }
    if (collapsed) {
      html += '<span class="collapsible-icon' + (expanded ? ' expanded' : '') + '"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></span>';
    }
    html += '</div>';
  }

  // 卡片内容
  if (content) {
    const contentExpandedClass = collapsed ? (expanded ? ' expanded' : '') : '';
    if (collapsed) {
      html += '<div class="collapsible-content' + contentExpandedClass + '"><div class="card-body">' + content + '</div></div>';
    } else {
      html += '<div class="card-body">' + content + '</div>';
    }
  }

  // 卡片底部
  if (footer) {
    html += '<div class="card-footer">' + footer + '</div>';
  }

  html += '</div>';
  return html;
}

// ============================================
// 工具调用渲染 - 使用 Augment 风格 c-tool-use 组件
// ============================================

export function renderToolCallItem(tool, toolIdx, panelPrefix, isLatest) {
  const inputContent = tool.input || '';
  const outputContent = tool.output || tool.result || '';
  const errorContent = tool.error || '';
  const hasInput = inputContent && String(inputContent).trim();
  const hasOutput = outputContent && String(outputContent).trim();
  const hasError = errorContent && String(errorContent).trim();

  if (!hasInput && !hasOutput && !hasError) return '';

  const toolPanelId = panelPrefix + '-tool-' + toolIdx;
  const toolStatus = tool.status || (hasError ? 'failed' : (hasOutput ? 'completed' : 'running'));
  const statusText = toolStatus === 'running' ? '执行中' : toolStatus === 'failed' ? '失败' : '完成';
  const statusClass = toolStatus === 'running' ? 'running' : toolStatus === 'failed' ? 'error' : 'success';
  const expandedClass = isLatest ? ' expanded' : '';

  // 使用 Augment 风格的 c-tool-use 组件
  let html = '<div class="c-tool-use c-tooluse-status--' + statusClass + '" data-panel-id="' + toolPanelId + '">';
  html += '<div class="c-tool-use__container">';

  // 头部区域
  html += '<div class="c-tool-use__header-container" onclick="togglePanel(\'' + toolPanelId + '\')">';
  html += '<div class="c-tool-use__header">';
  html += '<div class="c-tool-use__content">';

  // 折叠图标
  html += '<div class="c-tool-use__collapse-btn' + expandedClass + '"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';

  // 工具图标
  html += '<div class="c-tool-use__icon">' + getToolIcon(tool.name) + '</div>';

  // 工具名称
  html += '<span class="c-tool-use__name">' + escapeHtml(tool.name || '工具调用') + '</span>';

  // 参数摘要（折叠时显示）
  if (hasInput && !isLatest) {
    const inputStr = typeof inputContent === 'string' ? inputContent : JSON.stringify(inputContent);
    const paramSummary = inputStr.substring(0, 60).replace(/\s+/g, ' ') + (inputStr.length > 60 ? '...' : '');
    html += '<span class="c-tool-use__params-summary">' + escapeHtml(paramSummary) + '</span>';
  }

  html += '</div>'; // c-tool-use__content

  // 状态指示器
  html += '<span class="c-tool-use__status c-tool-use__status--' + statusClass + '">' + statusText + '</span>';

  // 折叠图标
  html += '<div class="c-collapsible-icon' + expandedClass + '"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';

  html += '</div>'; // c-tool-use__header
  html += '</div>'; // c-tool-use__header-container

  // 内容区域（可折叠）
  html += '<div class="c-tool-use__body collapsible-content' + expandedClass + '" id="' + toolPanelId + '">';

  if (hasInput) {
    html += '<div class="c-tool-use__section">';
    html += '<div class="c-tool-use__section-title">输入</div>';
    html += '<pre class="c-tool-use__content">' + escapeHtml(typeof inputContent === 'string' ? inputContent : JSON.stringify(inputContent, null, 2)) + '</pre>';
    html += '</div>';
  }

  if (hasOutput) {
    html += '<div class="c-tool-use__section">';
    html += '<div class="c-tool-use__section-title">输出</div>';
    html += '<pre class="c-tool-use__content">' + escapeHtml(typeof outputContent === 'string' ? outputContent : JSON.stringify(outputContent, null, 2)) + '</pre>';
    html += '</div>';
  }

  if (hasError) {
    html += '<div class="c-tool-use__section c-tool-use__section--error">';
    html += '<div class="c-tool-use__section-title">错误</div>';
    html += '<pre class="c-tool-use__content c-tool-use__content--error">' + escapeHtml(String(errorContent)) + '</pre>';
    html += '</div>';
  }

  html += '</div>'; // c-tool-use__body
  html += '</div>'; // c-tool-use__container
  html += '</div>'; // c-tool-use

  return html;
}

export function renderToolTrack(toolCalls, panelPrefix) {
  if (!toolCalls || toolCalls.length === 0) return '';

  const sorted = [...toolCalls].sort((a, b) => {
    const tsA = a.timestamp || 0;
    const tsB = b.timestamp || 0;
    if (tsA !== tsB) return tsA - tsB;
    return 0;
  });

  // 过滤有效的工具调用
  const validTools = sorted.filter(tool => {
    const inputContent = tool.input || '';
    const outputContent = tool.output || tool.result || tool.error || '';
    return (inputContent && String(inputContent).trim()) || (outputContent && String(outputContent).trim());
  });

  if (validTools.length === 0) return '';

  let html = '<div class="tool-track">';
  validTools.forEach((tool, idx) => {
    const isLatest = idx === validTools.length - 1;
    html += renderToolCallItem(tool, idx, panelPrefix, isLatest);
  });
  html += '</div>';
  return html;
}

// ============================================
// 任务卡片渲染
// ============================================

export function renderTaskCard(m, idx) {
  const agent = m.agent || '';
  const statusClass = m.status || 'started';
  const statusText = statusClass === 'started' ? '执行中' : statusClass === 'completed' ? '已完成' : statusClass === 'failed' ? '失败' : statusClass;
  const badgeStatusClass = statusClass === 'started' ? 'badge-running' : statusClass === 'completed' ? 'badge-completed' : statusClass === 'failed' ? 'badge-failed' : '';

  const badges = [];
  if (agent) {
    badges.push({ text: agent.toUpperCase(), class: 'badge-agent badge-' + agent.toLowerCase() });
  }
  badges.push({ text: statusText, class: badgeStatusClass });

  let contentHtml = '';
  if (m.taskDescription) {
    contentHtml += '<div class="task-description">' + escapeHtml(m.taskDescription) + '</div>';
  }
  if (m.content) {
    contentHtml += '<div class="task-content markdown-rendered">' + renderMarkdown(m.content) + '</div>';
  }

  return renderUnifiedCard({
    type: 'task',
    variant: agent.toLowerCase() || 'default',
    icon: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M2.5 0a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V2a2 2 0 0 0-2-2h-11zm5.5 3a.5.5 0 0 1 .5.5v5.793l1.146-1.147a.5.5 0 0 1 .708.708l-2 2a.5.5 0 0 1-.708 0l-2-2a.5.5 0 1 1 .708-.708L7.5 9.293V3.5a.5.5 0 0 1 .5-.5z"/></svg>',
    title: m.taskTitle || '子任务',
    badges: badges,
    time: m.time || '',
    content: contentHtml,
    collapsed: false,
    dataAttrs: { 'msg-idx': idx, 'subtask-id': m.subTaskId || '' },
    className: 'task-assignment-card'
  });
}

