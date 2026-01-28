/**
 * 思考过程组件渲染器 (Thinking Component Renderer)
 * 使用新的panel设计系统
 *
 * 功能：
 * - 渲染思考过程面板
 * - 智能摘要生成
 * - 流式状态支持
 * - 折叠/展开控制
 */

import { escapeHtml } from '../../core/utils.js';

/**
 * 简单的 Markdown 渲染（用于思考内容）
 * 避免与 markdown-renderer.js 形成循环依赖
 * 使用 marked v17 作为解析器
 * @param {string} content - 内容
 * @returns {string} HTML
 */
function renderThinkingMarkdown(content) {
  if (!content) return '';

  // 检查 marked 是否可用
  if (typeof marked !== 'undefined') {
    try {
      // 使用简单的 marked.parse，并移除输出的首尾空白
      return marked.parse(content, { breaks: true, gfm: true }).trim();
    } catch (e) {
      console.warn('[ThinkingRenderer] marked 解析失败:', e);
    }
  }

  // 简单回退
  return escapeHtml(content).replace(/\n/g, '<br>');
}

/**
 * 生成思考过程的智能摘要
 * @param {string} content - 思考内容
 * @param {number} maxLength - 最大长度（默认50个字符）
 * @returns {string} 摘要文本
 */
function generateThinkingSummary(content, maxLength = 50) {
  if (!content || !content.trim()) {
    return '正在思考...';
  }

  // 移除Markdown标记和多余空白
  const plainText = content
    .replace(/[#*_`~\[\]()]/g, '')
    .replace(/\s+/g, ' ')
    .trim();

  // 提取关键句子（优先提取第一个完整句子）
  const firstSentence = plainText.split(/[。！？.!?]/)[0];

  if (firstSentence.length <= maxLength) {
    return firstSentence;
  }

  // 截断并添加省略号
  return plainText.substring(0, maxLength).trim() + '...';
}

/**
 * 渲染思考过程组件
 * @param {Object} options - 渲染选项
 * @param {Array<string|Object>} options.thinking - 思考步骤数组
 * @param {boolean} options.isStreaming - 是否正在流式输出
 * @param {string} options.panelId - 面板ID
 * @param {boolean} options.autoExpand - 是否自动展开（流式时默认展开）
 * @returns {string} HTML字符串
 */
export function renderThinking({ thinking, isStreaming = false, panelId, autoExpand }) {
  if (!thinking || thinking.length === 0) {
    return '';
  }

  // 提取思考内容并移除首尾空白
  const thinkingContent = thinking
    .map(t => typeof t === 'string' ? t : t.content)
    .join('\n\n')
    .trim();

  // 生成智能摘要
  const summary = generateThinkingSummary(thinkingContent);

  // 确定是否展开
  const isExpanded = autoExpand !== undefined ? autoExpand : isStreaming;

  // 使用新的 c-collapsible 结构
  let html = '<div class="c-collapsible c-collapsible--thinking';
  if (isStreaming) {
    html += ' is-streaming';
  }
  if (!isExpanded) {
    html += ' is-collapsed';
  }
  html += '" data-panel-id="' + panelId + '">';

  // Header
  html += '<div class="c-collapsible__header">';
  html += '<div class="c-collapsible__header-inner" data-action="toggle-collapsible">';

  // 折叠指示箭头
  html += '<div class="c-collapsible__chevron">';
  html += '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
  html += '<path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>';
  html += '</svg>';
  html += '</div>';

  // 图标
  html += '<div class="c-collapsible__icon">';
  html += '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">';
  html += '<path d="M8.5 5.5a.5.5 0 0 0-1 0v3.362l-1.429 2.38a.5.5 0 1 0 .858.515l1.5-2.5A.5.5 0 0 0 8.5 9V5.5z"/>';
  html += '<path d="M6.5 0a.5.5 0 0 0 0 1H7v1.07a7.001 7.001 0 0 0-3.273 12.474l-.602.602a.5.5 0 0 0 .707.708l.746-.746A6.97 6.97 0 0 0 8 16a6.97 6.97 0 0 0 3.422-.892l.746.746a.5.5 0 0 0 .707-.708l-.601-.602A7.001 7.001 0 0 0 9 2.07V1h.5a.5.5 0 0 0 0-1h-3zm1.038 3.018a6.093 6.093 0 0 1 .924 0 6 6 0 1 1-.924 0z"/>';
  html += '</svg>';
  html += '</div>';

  // 标题区域
  html += '<div class="c-collapsible__title">';
  html += '<span class="c-collapsible__title-text">思考过程</span>';
  html += '<span class="c-collapsible__summary">' + escapeHtml(summary) + '</span>';
  html += '</div>';

  // 徽章
  html += '<span class="c-collapsible__badge c-collapsible__badge--thinking">' + thinking.length + ' 步</span>';

  html += '</div>'; // c-collapsible__header-inner
  html += '</div>'; // c-collapsible__header

  // Content - 使用 grid 动画
  html += '<div class="c-collapsible__content">';
  html += '<div class="c-collapsible__content-inner">';
  html += '<div class="c-collapsible__body">';
  html += renderThinkingMarkdown(thinkingContent);
  html += '</div>';
  html += '</div>';
  html += '</div>';

  html += '</div>'; // c-collapsible

  return html;
}

/**
 * 更新思考过程内容（用于流式更新）
 * @param {HTMLElement} thinkingElement - 思考过程DOM元素
 * @param {string} newContent - 新的内容
 */
export function updateThinkingContent(thinkingElement, newContent) {
  if (!thinkingElement) return;

  const contentElement = thinkingElement.querySelector('.c-collapsible__body');
  if (contentElement) {
    contentElement.innerHTML = renderThinkingMarkdown(newContent);
  }

  // 更新摘要
  const summaryElement = thinkingElement.querySelector('.c-collapsible__summary');
  if (summaryElement) {
    const newSummary = generateThinkingSummary(newContent);
    summaryElement.textContent = newSummary;
  }
}

/**
 * 切换思考过程的展开/折叠状态
 * @param {HTMLElement} thinkingElement - 思考过程DOM元素
 * @param {boolean} expand - true展开，false折叠，undefined切换
 */
export function toggleThinking(thinkingElement, expand) {
  if (!thinkingElement) return;

  if (expand === undefined) {
    thinkingElement.classList.toggle('is-collapsed');
  } else {
    if (expand) {
      thinkingElement.classList.remove('is-collapsed');
    } else {
      thinkingElement.classList.add('is-collapsed');
    }
  }
}

/**
 * 完成思考过程流式输出（移除流式状态）
 * @param {HTMLElement} thinkingElement - 思考过程DOM元素
 * @param {boolean} autoCollapse - 是否自动折叠（默认true）
 */
export function completeThinking(thinkingElement, autoCollapse = true) {
  if (!thinkingElement) return;

  // 移除流式状态
  thinkingElement.classList.remove('is-streaming');

  // 自动折叠
  if (autoCollapse) {
    toggleThinking(thinkingElement, false);
  }
}

export { generateThinkingSummary };
