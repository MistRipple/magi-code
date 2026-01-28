/**
 * 工具调用组件渲染器 (Tool Call Component Renderer)
 * 使用新的panel设计系统
 *
 * 功能：
 * - 渲染工具调用卡片
 * - 状态指示（pending/running/success/error）
 * - 输入/输出展示
 * - 折叠/展开控制
 */

import { escapeHtml } from '../../core/utils.js';

/**
 * 格式化工具参数/结果为可读文本
 * @param {any} content - 内容（可能是字符串、对象或数组）
 * @returns {string} 格式化后的文本
 */
function formatToolContent(content) {
  if (!content) return '';

  // 如果是字符串，移除首尾空白后返回
  if (typeof content === 'string') {
    return content.trim();
  }

  // 如果是对象或数组，格式化为JSON
  try {
    return JSON.stringify(content, null, 2);
  } catch (e) {
    return String(content).trim();
  }
}

/**
 * 获取工具状态相关的类名和文本
 * @param {string} status - 状态（pending/running/success/error/failed）
 * @returns {Object} { statusClass, statusText }
 */
function getToolStatus(status) {
  const normalizedStatus = (status || 'success').toLowerCase();

  const statusMap = {
    'pending': { class: 'pending', text: '等待中' },
    'running': { class: 'running', text: '执行中' },
    'success': { class: 'success', text: '成功' },
    'completed': { class: 'success', text: '完成' },
    'error': { class: 'error', text: '失败' },
    'failed': { class: 'error', text: '失败' }
  };

  return statusMap[normalizedStatus] || { class: 'success', text: '完成' };
}

/**
 * 获取工具图标SVG
 * @param {string} toolName - 工具名称
 * @returns {string} SVG HTML
 */
function getToolIcon(toolName) {
  // 默认工具图标
  const defaultIcon = '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M.293 1.293a1 1 0 0 1 1.414 0L8 7.586 14.293 1.293a1 1 0 1 1 1.414 1.414L9.414 9l6.293 6.293a1 1 0 0 1-1.414 1.414L8 10.414l-6.293 6.293a1 1 0 0 1-1.414-1.414L6.586 9 .293 2.707a1 1 0 0 1 0-1.414z"/></svg>';

  // 可以根据工具名称返回不同的图标
  const iconMap = {
    'Read': '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M5 4a.5.5 0 0 0 0 1h6a.5.5 0 0 0 0-1H5zm-.5 2.5A.5.5 0 0 1 5 6h6a.5.5 0 0 1 0 1H5a.5.5 0 0 1-.5-.5zM5 8a.5.5 0 0 0 0 1h6a.5.5 0 0 0 0-1H5zm0 2a.5.5 0 0 0 0 1h3a.5.5 0 0 0 0-1H5z"/><path d="M2 2a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V2zm10-1H4a1 1 0 0 0-1 1v12a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V2a1 1 0 0 0-1-1z"/></svg>',
    'Write': '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M15.502 1.94a.5.5 0 0 1 0 .706L14.459 3.69l-2-2L13.502.646a.5.5 0 0 1 .707 0l1.293 1.293zm-1.75 2.456-2-2L4.939 9.21a.5.5 0 0 0-.121.196l-.805 2.414a.25.25 0 0 0 .316.316l2.414-.805a.5.5 0 0 0 .196-.12l6.813-6.814z"/><path fill-rule="evenodd" d="M1 13.5A1.5 1.5 0 0 0 2.5 15h11a1.5 1.5 0 0 0 1.5-1.5v-6a.5.5 0 0 0-1 0v6a.5.5 0 0 1-.5.5h-11a.5.5 0 0 1-.5-.5v-11a.5.5 0 0 1 .5-.5H9a.5.5 0 0 0 0-1H2.5A1.5 1.5 0 0 0 1 2.5v11z"/></svg>',
    'Bash': '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M14 1a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1h12zM2 0a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V2a2 2 0 0 0-2-2H2z"/><path d="M6 9a.5.5 0 0 1 .5-.5h3a.5.5 0 0 1 0 1h-3A.5.5 0 0 1 6 9zM3.854 4.146a.5.5 0 1 0-.708.708L4.793 6.5 3.146 8.146a.5.5 0 1 0 .708.708l2-2a.5.5 0 0 0 0-.708l-2-2z"/></svg>',
    'Grep': '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M11.742 10.344a6.5 6.5 0 1 0-1.397 1.398h-.001c.03.04.062.078.098.115l3.85 3.85a1 1 0 0 0 1.415-1.414l-3.85-3.85a1.007 1.007 0 0 0-.115-.1zM12 6.5a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0z"/></svg>',
    'Edit': '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10zM11.207 2.5 13.5 4.793 14.793 3.5 12.5 1.207 11.207 2.5zm1.586 3L10.5 3.207 4 9.707V10h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.293l6.5-6.5zm-9.761 5.175-.106.106-1.528 3.821 3.821-1.528.106-.106A.5.5 0 0 1 5 12.5V12h-.5a.5.5 0 0 1-.5-.5V11h-.5a.5.5 0 0 1-.468-.325z"/></svg>'
  };

  return iconMap[toolName] || defaultIcon;
}

/**
 * 渲染工具调用卡片
 * @param {Object} options - 渲染选项
 * @param {string} options.name - 工具名称
 * @param {string} options.id - 工具调用ID
 * @param {any} options.input - 输入参数
 * @param {any} options.output - 输出结果
 * @param {string} options.error - 错误信息
 * @param {string} options.status - 状态（pending/running/success/error）
 * @param {number} options.duration - 执行时长（毫秒）
 * @param {boolean} options.isExpanded - 是否展开
 * @param {string} options.panelId - 面板ID
 * @returns {string} HTML字符串
 */
export function renderToolCall({
  name,
  id,
  input,
  output,
  error,
  status = 'success',
  duration,
  isExpanded = false,
  panelId
}) {
  const statusInfo = getToolStatus(status);
  const hasInput = input && String(input).trim();
  const hasOutput = output && String(output).trim();
  const hasError = error && String(error).trim();

  // 如果没有任何内容，返回空
  if (!hasInput && !hasOutput && !hasError) {
    return '';
  }

  // 使用新的 c-collapsible 结构
  let html = '<div class="c-collapsible c-collapsible--tool';
  if (!isExpanded) {
    html += ' is-collapsed';
  }
  html += '" data-panel-id="' + panelId + '" data-status="' + statusInfo.class + '">';

  // Header
  html += '<div class="c-collapsible__header">';
  html += '<div class="c-collapsible__header-inner" data-action="toggle-collapsible">';

  // 折叠指示箭头
  html += '<div class="c-collapsible__chevron">';
  html += '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
  html += '<path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>';
  html += '</svg>';
  html += '</div>';

  // 工具图标
  html += '<div class="c-collapsible__icon">' + getToolIcon(name) + '</div>';

  // 标题区域
  html += '<div class="c-collapsible__title">';
  html += '<span class="c-collapsible__title-text">' + escapeHtml(name || '工具调用') + '</span>';
  if (id) {
    html += '<span style="color: var(--ds-color-neutral-11); font-size: 11px; opacity: 0.7;">#' + escapeHtml(id) + '</span>';
  }
  html += '</div>';

  // 状态指示器
  html += '<span class="c-collapsible__status c-collapsible__status--' + statusInfo.class + '">';
  if (statusInfo.class === 'running') {
    html += '<svg class="c-collapsible__status-icon--running" width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
    html += '<path d="M8 3a5 5 0 1 0 4.546 2.914.5.5 0 0 1 .908-.417A6 6 0 1 1 8 2v1z"/>';
    html += '<path d="M8 4.466V.534a.25.25 0 0 1 .41-.192l2.36 1.966c.12.1.12.284 0 .384L8.41 4.658A.25.25 0 0 1 8 4.466z"/>';
    html += '</svg>';
  }
  html += statusInfo.text;
  html += '</span>';

  html += '</div>'; // c-collapsible__header-inner
  html += '</div>'; // c-collapsible__header

  // Content - 使用 grid 动画
  html += '<div class="c-collapsible__content">';
  html += '<div class="c-collapsible__content-inner">';
  html += '<div class="c-collapsible__body">';

  // 输入部分
  if (hasInput) {
    html += '<div class="tool-section">';
    html += '<div class="tool-section__label">输入</div>';
    html += '<div class="tool-section__content">';
    html += escapeHtml(formatToolContent(input));
    html += '</div>';
    html += '</div>';
  }

  // 输出部分
  if (hasOutput) {
    html += '<div class="tool-section">';
    html += '<div class="tool-section__label">输出</div>';
    html += '<div class="tool-section__content">';
    html += escapeHtml(formatToolContent(output));
    html += '</div>';
    html += '</div>';
  }

  // 错误部分
  if (hasError) {
    html += '<div class="tool-section tool-section--error">';
    html += '<div class="tool-section__label">错误信息</div>';
    html += '<div class="tool-section__content">';
    html += escapeHtml(String(error));
    html += '</div>';
    html += '</div>';
  }

  // 元信息
  if (duration) {
    html += '<div class="tool-meta">';
    html += '<span>耗时: <strong>' + (duration / 1000).toFixed(2) + 's</strong></span>';
    html += '</div>';
  }

  html += '</div>'; // c-collapsible__body
  html += '</div>'; // c-collapsible__content-inner
  html += '</div>'; // c-collapsible__content

  html += '</div>'; // c-collapsible

  return html;
}

/**
 * 渲染工具调用列表
 * @param {Array<Object>} toolCalls - 工具调用数组
 * @param {string} panelPrefix - 面板ID前缀
 * @returns {string} HTML字符串
 */
export function renderToolCallList(toolCalls, panelPrefix = 'tool') {
  if (!toolCalls || toolCalls.length === 0) {
    return '';
  }

  let html = '<div class="tool-call-list">';

  toolCalls.forEach((tool, index) => {
    const panelId = panelPrefix + '-' + index;
    const isLatest = index === toolCalls.length - 1;

    html += renderToolCall({
      name: tool.name,
      id: tool.id || tool.tool_use_id,
      input: tool.input,
      output: tool.output || tool.result,
      error: tool.error,
      status: tool.status,
      duration: tool.duration,
      isExpanded: isLatest, // 默认展开最后一个
      panelId: panelId
    });
  });

  html += '</div>';

  return html;
}

/**
 * 更新工具调用状态
 * @param {HTMLElement} toolElement - 工具调用DOM元素
 * @param {string} newStatus - 新状态
 */
export function updateToolCallStatus(toolElement, newStatus) {
  if (!toolElement) return;

  const statusInfo = getToolStatus(newStatus);
  const oldStatus = toolElement.getAttribute('data-status');

  // 更新data-status属性
  toolElement.setAttribute('data-status', statusInfo.class);

  // 更新状态指示器
  const statusSpan = toolElement.querySelector('.c-collapsible__status');
  if (statusSpan) {
    // 移除旧状态类
    statusSpan.classList.remove('c-collapsible__status--' + oldStatus);
    statusSpan.classList.add('c-collapsible__status--' + statusInfo.class);

    // 更新内容
    if (statusInfo.class === 'running') {
      statusSpan.innerHTML = '<svg class="c-collapsible__status-icon--running" width="12" height="12" viewBox="0 0 16 16" fill="currentColor"><path d="M8 3a5 5 0 1 0 4.546 2.914.5.5 0 0 1 .908-.417A6 6 0 1 1 8 2v1z"/><path d="M8 4.466V.534a.25.25 0 0 1 .41-.192l2.36 1.966c.12.1.12.284 0 .384L8.41 4.658A.25.25 0 0 1 8 4.466z"/></svg>' + statusInfo.text;
    } else {
      statusSpan.textContent = statusInfo.text;
    }
  }
}

/**
 * 添加加载指示器（运行中状态）
 * @param {HTMLElement} toolElement - 工具调用DOM元素
 */
export function addToolCallLoading(toolElement) {
  if (!toolElement) return;

  updateToolCallStatus(toolElement, 'running');
}

/**
 * 移除加载指示器
 * @param {HTMLElement} toolElement - 工具调用DOM元素
 */
export function removeToolCallLoading(toolElement) {
  if (!toolElement) return;

  // 状态将通过updateToolCallStatus更新
}

export { getToolIcon, getToolStatus, formatToolContent };
