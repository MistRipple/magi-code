/**
 * 代码块组件渲染器 (Code Block Component Renderer)
 * 使用新的panel设计系统
 *
 * 功能：
 * - 渲染代码块
 * - 语言标识
 * - 文件路径显示
 * - 复制/应用按钮
 * - 语法高亮支持
 * - 行号显示
 */

import { escapeHtml } from '../../core/utils.js';

/**
 * 语言显示名称映射
 */
const LANGUAGE_NAMES = {
  'js': 'JavaScript',
  'javascript': 'JavaScript',
  'ts': 'TypeScript',
  'typescript': 'TypeScript',
  'jsx': 'JSX',
  'tsx': 'TSX',
  'py': 'Python',
  'python': 'Python',
  'rb': 'Ruby',
  'ruby': 'Ruby',
  'go': 'Go',
  'rs': 'Rust',
  'rust': 'Rust',
  'java': 'Java',
  'c': 'C',
  'cpp': 'C++',
  'cs': 'C#',
  'csharp': 'C#',
  'php': 'PHP',
  'swift': 'Swift',
  'kt': 'Kotlin',
  'kotlin': 'Kotlin',
  'sh': 'Shell',
  'bash': 'Bash',
  'zsh': 'Zsh',
  'fish': 'Fish',
  'ps1': 'PowerShell',
  'powershell': 'PowerShell',
  'sql': 'SQL',
  'html': 'HTML',
  'css': 'CSS',
  'scss': 'SCSS',
  'sass': 'Sass',
  'less': 'Less',
  'json': 'JSON',
  'yaml': 'YAML',
  'yml': 'YAML',
  'xml': 'XML',
  'md': 'Markdown',
  'markdown': 'Markdown',
  'diff': 'Diff',
  'plaintext': 'Text',
  'text': 'Text'
};

/**
 * 获取语言显示名称
 * @param {string} lang - 语言标识
 * @returns {string} 显示名称
 */
function getLanguageName(lang) {
  if (!lang) return 'Code';
  return LANGUAGE_NAMES[lang.toLowerCase()] || lang.toUpperCase();
}

/**
 * 生成唯一ID
 * @returns {string} 唯一ID
 */
function generateId() {
  return 'code-' + Date.now() + '-' + Math.random().toString(36).substr(2, 9);
}

/**
 * 渲染代码块
 * @param {Object} options - 渲染选项
 * @param {string} options.code - 代码内容
 * @param {string} options.language - 语言标识
 * @param {string} options.filepath - 文件路径
 * @param {boolean} options.showLineNumbers - 是否显示行号
 * @param {boolean} options.showCopyButton - 是否显示复制按钮
 * @param {boolean} options.showApplyButton - 是否显示应用按钮
 * @param {number} options.maxHeight - 最大高度（超过则可折叠）
 * @param {string} options.blockId - 代码块ID（用于复制等操作）
 * @returns {string} HTML字符串
 */
export function renderCodeBlock({
  code,
  language = '',
  filepath = '',
  showLineNumbers = false,
  showCopyButton = true,
  showApplyButton = false,
  maxHeight = 0,
  blockId
}) {
  if (!code) return '';

  // 移除代码首尾的空白字符
  const trimmedCode = code.trim();

  const id = blockId || generateId();
  const langName = getLanguageName(language);
  const lines = trimmedCode.split('\n');
  const isCollapsible = maxHeight > 0 && lines.length > 15;

  // 使用新的 c-collapsible 结构
  // 代码块默认展开，用户可以点击折叠
  let html = '<div class="c-collapsible c-collapsible--code" data-code-id="' + id + '"';
  if (language) {
    html += ' data-language="' + escapeHtml(language) + '"';
  }
  html += '>';

  // Header
  html += '<div class="c-collapsible__header">';
  html += '<div class="c-collapsible__header-inner" data-action="toggle-collapsible">';

  // 折叠指示箭头
  html += '<div class="c-collapsible__chevron">';
  html += '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
  html += '<path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>';
  html += '</svg>';
  html += '</div>';

  // 代码图标
  html += '<div class="c-collapsible__icon">';
  html += '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">';
  html += '<path d="M5.854 4.854a.5.5 0 1 0-.708-.708l-3.5 3.5a.5.5 0 0 0 0 .708l3.5 3.5a.5.5 0 0 0 .708-.708L2.707 8l3.147-3.146zm4.292 0a.5.5 0 0 1 .708-.708l3.5 3.5a.5.5 0 0 1 0 .708l-3.5 3.5a.5.5 0 0 1-.708-.708L13.293 8l-3.147-3.146z"/>';
  html += '</svg>';
  html += '</div>';

  // 标题区域
  html += '<div class="c-collapsible__title">';
  html += '<span class="c-collapsible__title-text">' + langName + '</span>';
  if (filepath) {
    html += '<span style="color: var(--ds-color-neutral-11); font-size: 11px; opacity: 0.7;" title="' + escapeHtml(filepath) + '">';
    html += escapeHtml(filepath);
    html += '</span>';
  }
  html += '</div>';

  // 操作按钮区域
  html += '<div class="c-collapsible__actions">';

  // 复制按钮
  if (showCopyButton) {
    html += '<button class="c-collapsible__copy-btn" data-action="copy-code" data-code-id="' + id + '" title="复制代码">';
    html += '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
    html += '<path d="M4 2a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V2zm2-1a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V2a1 1 0 0 0-1-1H6zM2 5a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-1h1v1a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h1v1H2z"/>';
    html += '</svg>';
    html += '<span>复制</span>';
    html += '</button>';
  }

  // 应用按钮
  if (showApplyButton && filepath) {
    html += '<button class="c-collapsible__copy-btn" data-action="apply-code" data-code-id="' + id + '" title="应用到文件">';
    html += '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">';
    html += '<path d="M12.736 3.97a.733.733 0 0 1 1.047 0c.286.289.29.756.01 1.05L7.88 12.01a.733.733 0 0 1-1.065.02L3.217 8.384a.757.757 0 0 1 0-1.06.733.733 0 0 1 1.047 0l3.052 3.093 5.4-6.425a.247.247 0 0 1 .02-.022Z"/>';
    html += '</svg>';
    html += '<span>应用</span>';
    html += '</button>';
  }

  html += '</div>'; // c-collapsible__actions
  html += '</div>'; // c-collapsible__header-inner
  html += '</div>'; // c-collapsible__header

  // Content 区域 - 使用 grid 动画
  html += '<div class="c-collapsible__content">';
  html += '<div class="c-collapsible__content-inner">';
  html += '<div class="c-collapsible__body">';

  if (showLineNumbers) {
    html += '<div class="code-with-line-numbers">';
    html += '<div class="code-line-numbers">';
    lines.forEach((_, index) => {
      html += '<span class="code-line-number">' + (index + 1) + '</span>\n';
    });
    html += '</div>';
    html += '<pre class="code-pre"><code class="code-content';
    if (language) {
      html += ' language-' + escapeHtml(language);
    }
    html += '">' + escapeHtml(trimmedCode) + '</code></pre>';
    html += '</div>';
  } else {
    html += '<pre class="code-pre"><code class="code-content';
    if (language) {
      html += ' language-' + escapeHtml(language);
    }
    html += '">' + escapeHtml(trimmedCode) + '</code></pre>';
  }

  html += '</div>'; // c-collapsible__body
  html += '</div>'; // c-collapsible__content-inner
  html += '</div>'; // c-collapsible__content

  html += '</div>'; // c-collapsible

  return html;
}

/**
 * 渲染内联代码
 * @param {string} code - 代码内容
 * @returns {string} HTML字符串
 */
export function renderInlineCode(code) {
  if (!code) return '';
  return '<code class="c-code-inline">' + escapeHtml(code) + '</code>';
}

/**
 * 复制代码块到剪贴板（浏览器环境下的实现示例）
 * 注意：这个函数需要在浏览器环境中通过全局函数调用
 */
export function copyCodeBlockImpl(codeId) {
  const codeBlock = document.querySelector('[data-code-id="' + codeId + '"]');
  if (!codeBlock) return;

  const codeElement = codeBlock.querySelector('.code-content');
  if (!codeElement) return;

  const code = codeElement.textContent;

  // 使用 Clipboard API
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(code).then(() => {
      // 显示复制成功状态
      const copyBtn = codeBlock.querySelector('.c-collapsible__copy-btn[data-action="copy-code"]');
      if (copyBtn) {
        const originalHTML = copyBtn.innerHTML;
        copyBtn.style.color = 'var(--ds-color-success-9, #22c55e)';
        copyBtn.innerHTML = '<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor"><path d="M12.736 3.97a.733.733 0 0 1 1.047 0c.286.289.29.756.01 1.05L7.88 12.01a.733.733 0 0 1-1.065.02L3.217 8.384a.757.757 0 0 1 0-1.06.733.733 0 0 1 1.047 0l3.052 3.093 5.4-6.425a.247.247 0 0 1 .02-.022Z"/></svg><span>已复制</span>';

        setTimeout(() => {
          copyBtn.style.color = '';
          copyBtn.innerHTML = originalHTML;
        }, 2000);
      }
    }).catch(err => {
      console.error('Failed to copy code:', err);
    });
  }
}

/**
 * 切换代码块的展开/折叠状态
 */
export function toggleCodeBlockImpl(codeId) {
  const codeBlock = document.querySelector('[data-code-id="' + codeId + '"]');
  if (!codeBlock) return;

  codeBlock.classList.toggle('is-collapsed');
}

/**
 * 应用代码到文件（需要与VSCode通信）
 */
export function applyCodeBlockImpl(codeId) {
  const codeBlock = document.querySelector('[data-code-id="' + codeId + '"]');
  if (!codeBlock) return;

  const codeElement = codeBlock.querySelector('.code-content');
  const filepathElement = codeBlock.querySelector('.c-collapsible__title span:nth-child(2)');

  if (!codeElement || !filepathElement) return;

  const code = codeElement.textContent;
  const filepath = filepathElement.textContent.trim();
  const language = codeBlock.getAttribute('data-language') || '';

  // 发送消息到VSCode
  if (window.vscode) {
    window.vscode.postMessage({
      type: 'applyCode',
      filepath: filepath,
      code: code,
      language: language
    });
  }
}

export { getLanguageName, generateId };
