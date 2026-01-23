// Markdown 和代码渲染模块
// 负责 Markdown 解析和代码块渲染
// 使用 Augment 风格的组件设计

import { escapeHtml } from './render-utils.js';

// ============================================
// Markdown 渲染
// ============================================

export function renderMarkdown(content) {
  if (!content) return '';

  // 检查 marked 是否可用
  if (typeof marked === 'undefined') {
    console.warn('[renderMarkdown] marked 库未加载，使用简单渲染');
    return escapeHtml(content).replace(/\n/g, '<br>');
  }

  try {
    // 配置 marked
    marked.setOptions({
      breaks: true,
      gfm: true,
      headerIds: false,
      mangle: false
    });

    // 自定义渲染器
    const renderer = new marked.Renderer();

    // 自定义代码块渲染 - 使用 Augment 风格的 c-codeblock 组件
    renderer.code = function(code, language) {
      return renderCodeBlock(code, language, null);
    };

    // 自定义行内代码渲染
    renderer.codespan = function(code) {
      return `<code class="c-inline-code">${escapeHtml(code)}</code>`;
    };

    // 自定义链接渲染
    renderer.link = function(href, title, text) {
      const titleAttr = title ? ` title="${escapeHtml(title)}"` : '';
      return `<a href="${escapeHtml(href)}"${titleAttr} target="_blank" rel="noopener" class="c-link">${text}</a>`;
    };

    return marked.parse(content, { renderer });
  } catch (e) {
    console.error('[renderMarkdown] 解析错误:', e);
    return escapeHtml(content).replace(/\n/g, '<br>');
  }
}

// ============================================
// 代码块渲染 - Augment 风格 c-codeblock 组件
// ============================================

export function renderCodeBlock(code, lang, filepath) {
  const codeId = 'code-' + Math.random().toString(36).substr(2, 9);
  const trimmedCode = code.replace(/^\n+/, '').replace(/\n+$/, '');
  if (!trimmedCode) return '';

  const lines = trimmedCode.split('\n');
  const lineCount = lines.length;
  const language = lang || 'text';
  const isDiff = language === 'diff' || lines.some(line => /^[+-](?!\+\+|--)\s/.test(line));

  // 语法高亮
  let highlightedCode = trimmedCode;
  if (typeof hljs !== 'undefined' && language !== 'text' && language !== 'diff') {
    try {
      if (hljs.getLanguage(language)) {
        highlightedCode = hljs.highlight(trimmedCode, { language }).value;
      } else {
        highlightedCode = hljs.highlightAuto(trimmedCode).value;
      }
    } catch (e) {
      highlightedCode = escapeHtml(trimmedCode);
    }
  } else {
    highlightedCode = escapeHtml(trimmedCode);
  }

  // 将高亮后的代码按行分割
  const highlightedLines = highlightedCode.split('\n');

  // 构建代码行 HTML
  let codeLinesHtml = '';
  lines.forEach((originalLine, idx) => {
    let lineClass = 'code-line';
    let displayLine = highlightedLines[idx] || '';

    if (isDiff) {
      if (/^[+](?!\+\+)/.test(originalLine)) {
        lineClass += ' diff-add';
      } else if (/^[-](?!--)/.test(originalLine)) {
        lineClass += ' diff-del';
      }
    }
    codeLinesHtml += '<div class="' + lineClass + '">' + displayLine + '</div>';
  });

  // 检查是否需要折叠（超过 15 行）
  const shouldCollapse = lineCount > 15;
  const expandedClass = shouldCollapse ? '' : ' expanded';

  // 使用 Augment 风格的 c-codeblock 组件
  let html = '<div class="c-codeblock' + (isDiff ? ' c-codeblock--diff' : '') + '" data-code-id="' + codeId + '">';

  // 头部区域
  html += '<div class="c-codeblock__header">';
  html += '<div class="c-codeblock__header-content">';

  // 折叠按钮
  if (shouldCollapse) {
    html += '<div class="c-codeblock__collapse-btn' + expandedClass + '" onclick="toggleCodeBlock(\'' + codeId + '\')"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';
  }

  // 文件路径或语言标签
  html += '<div class="c-codeblock__relpath">';
  if (filepath) {
    html += '<span class="c-codeblock__filename" onclick="openFileInEditor(\'' + escapeHtml(filepath) + '\')" title="点击在编辑器中打开">' + escapeHtml(filepath) + '</span>';
  } else if (language) {
    html += '<span class="c-codeblock__language">' + escapeHtml(language) + '</span>';
  }
  html += '</div>';
  html += '</div>';

  // 操作按钮
  html += '<div class="c-codeblock__action-bar-right">';
  html += '<span class="c-codeblock__line-count">' + lineCount + ' 行</span>';
  html += '<button class="code-copy-btn" onclick="copyCodeBlock(\'' + codeId + '\')" title="复制代码">';
  html += '<svg viewBox="0 0 16 16"><path d="M4 1.5H3a2 2 0 0 0-2 2V14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V3.5a2 2 0 0 0-2-2h-1v1h1a1 1 0 0 1 1 1V14a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1h1v-1z"/><path d="M9.5 1a.5.5 0 0 1 .5.5v1a.5.5 0 0 1-.5.5h-3a.5.5 0 0 1-.5-.5v-1a.5.5 0 0 1 .5-.5h3zm-3-1A1.5 1.5 0 0 0 5 1.5v1A1.5 1.5 0 0 0 6.5 4h3A1.5 1.5 0 0 0 11 2.5v-1A1.5 1.5 0 0 0 9.5 0h-3z"/></svg>';
  html += '</button>';
  html += '</div>';
  html += '</div>';

  // 代码内容区域
  html += '<div class="c-codeblock__container">';
  html += '<div class="c-codeblock__container-inner">';

  if (shouldCollapse) {
    html += '<div class="c-codeblock__truncated collapsible-content' + expandedClass + '">';
    html += '<div class="c-codeblock__code" id="' + codeId + '"><pre><code>' + codeLinesHtml + '</code></pre></div>';
    html += '</div>';
    html += '<div class="c-codeblock__truncated-surface collapsible-expand-btn" onclick="toggleCodeBlock(\'' + codeId + '\')" style="' + (expandedClass ? 'display:none;' : '') + '">';
    html += '<button class="c-codeblock__expand-btn">';
    html += '<svg viewBox="0 0 16 16"><path d="M1.646 4.646a.5.5 0 0 1 .708 0L46-5.647a.5.5 0 0 1 .708.708l-6 6a.5.5 0 0 1-.708 0l-6-6a.5.5 0 0 1 0-.708z"/></svg>';
    html += '<span>显示全部 ' + lineCount + ' 行</span>';
    html += '</button>';
    html += '</div>';
  } else {
    html += '<div class="c-codeblock__code" id="' + codeId + '"><pre><code>' + codeLinesHtml + '</code></pre></div>';
  }

  html += '</div>';
  html += '</div>';
  html += '</div>';

  return html;
}

// ============================================
// 解析块渲染
// ============================================

export function renderParsedBlocks(blocks, agent) {
  if (!blocks || blocks.length === 0) {
    return { html: '', isMarkdown: false };
  }

  let html = '';
  let hasMarkdown = false;

  blocks.forEach((block) => {
    switch (block.type) {
      case 'text':
        if (block.content && block.content.trim()) {
          if (block.isMarkdown !== false) {
            html += renderMarkdown(block.content);
            hasMarkdown = true;
          } else {
            html += '<p>' + escapeHtml(block.content) + '</p>';
          }
        }
        break;

      case 'code':
        if (block.content && block.content.trim()) {
          html += renderCodeBlock(block.content, block.language, block.filepath);
        }
        break;

      case 'thinking':
        if (block.content && block.content.trim()) {
          html += renderThinkingBlock(block.content, block.isStreaming);
        }
        break;

      case 'tool_call':
        if (block.tool) {
          html += renderToolUseBlock(block.tool);
        }
        break;

      default:
        if (block.content) {
          html += '<div class="unknown-block">' + escapeHtml(block.content) + '</div>';
        }
    }
  });

  return { html, isMarkdown: hasMarkdown };
}

// ============================================
// 思考过程渲染 - Augment 风格 c-thinking 组件
// ============================================

export function renderThinkingBlock(content, isStreaming) {
  const thinkingId = 'thinking-' + Math.random().toString(36).substr(2, 9);
  const escapedContent = escapeHtml(content || '');

  // 生成摘要（取前50个字符）
  const summary = content ? content.substring(0, 50).replace(/\n/g, ' ') + (content.length > 50 ? '...' : '') : '思考中...';

  let html = '<details class="c-thinking" id="' + thinkingId + '">';
  html += '<summary class="c-thinking__header">';
  html += '<div class="c-thinking__icon"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16zm.93-9.412-1 4.705c-.07.34.029.533.304.533.194 0 .487-.07.686-.246l-.088.416c-.287.346-.92.598-1.465.598-.703 0-1.002-.422-.808-1.319l.738-3.468c.064-.293.006-.399-.287-.47l-.451-.081.082-.381 2.29-.287zM8 5.5a1 1 0 1 1 0-2 1 1 0 0 1 0 2z"/></svg></div>';
  html += '<span class="c-thinking__title">思考过程</span>';
  html += '<span class="c-thinking__summary">' + escapeHtml(summary) + '</span>';
  if (isStreaming) {
    html += '<span class="c-thinking__cursor"></span>';
  }
  html += '<div class="c-thinking__chevron"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';
  html += '</summary>';
  html += '<div class="c-thinking__content">';
  html += '<div class="c-thinking__text">' + escapedContent.replace(/\n/g, '<br>') + '</div>';
  html += '</div>';
  html += '</details>';

  return html;
}

// ============================================
// 工具调用渲染 - Augment 风格 c-tool-use 组件
// ============================================

export function renderToolUseBlock(tool) {
  const toolId = 'tool-' + Math.random().toString(36).substr(2, 9);
  const toolName = tool.name || '工具调用';
  const inputContent = tool.input || tool.arguments || '';
  const outputContent = tool.output || tool.result || '';
  const errorContent = tool.error || '';

  const hasInput = inputContent && String(inputContent).trim();
  const hasOutput = outputContent && String(outputContent).trim();
  const hasError = errorContent && String(errorContent).trim();

  // 状态判断
  let statusClass = 'running';
  let statusText = '执行中';
  if (hasError) {
    statusClass = 'error';
    statusText = '失败';
  } else if (hasOutput) {
    statusClass = 'success';
    statusText = '完成';
  }

  // 生成参数摘要
  let paramSummary = '';
  if (hasInput) {
    const inputStr = typeof inputContent === 'string' ? inputContent : JSON.stringify(inputContent);
    paramSummary = inputStr.substring(0, 60).replace(/\n/g, ' ') + (inputStr.length > 60 ? '...' : '');
  }

  // 使用 data-panel-id 以匹配 togglePanel 函数
  let html = '<div class="c-tool-use c-tooluse-status--' + statusClass + '" data-panel-id="' + toolId + '">';

  // 头部区域 - 添加容器包装
  html += '<div class="c-tool-use__container">';
  html += '<div class="c-tool-use__header-container" onclick="togglePanel(\'' + toolId + '\')">';
  html += '<div class="c-tool-use__header">';
  html += '<div class="c-tool-use__content">';
  html += '<div class="c-tool-use__collapse-btn"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';
  html += '<div class="c-tool-use__icon">' + getToolIconSvg(toolName) + '</div>';
  html += '<span class="c-tool-use__name">' + escapeHtml(toolName) + '</span>';
  if (paramSummary) {
    html += '<span class="c-tool-use__params-summary">' + escapeHtml(paramSummary) + '</span>';
  }
  html += '</div>'; // c-tool-use__content
  html += '<span class="c-tool-use__status c-tool-use__status--' + statusClass + '">' + statusText + '</span>';
  html += '<div class="c-collapsible-icon"><svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></div>';
  html += '</div>'; // c-tool-use__header
  html += '</div>'; // c-tool-use__header-container

  // 内容区域
  html += '<div class="c-tool-use__body collapsible-content" id="' + toolId + '">';

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

// 工具图标 SVG
function getToolIconSvg(toolName) {
  const name = (toolName || '').toLowerCase();

  if (name.includes('read') || name.includes('file') || name.includes('view')) {
    return '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M14 4.5V14a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V2a2 2 0 0 1 2-2h5.5L14 4.5zm-3 0A1.5 1.5 0 0 1 9.5 3V1H4a1-1 1v12a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V4.5h-2z"/></svg>';
  }
  if (name.includes('write') || name.includes('edit') || name.includes('save')) {
    return '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10zM11.207 2.5 13.5 4.793 14.793 3.5 12.5 1.207 11.207 2.5zm1.586 3L10.5 3.207 4 9.707V10h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.293l6.5-6.5zm-9.761 5.175-.106.106-1.528 3.821 3.821-1.528.106-.106A.5.5 0 0 1 5 12.5V12h-.5a.5.5 0 0 1-.5-.5V11h-.5a.5.5 0 0 1-.468-.325z"/></svg>';
  }
  if (name.includes('search') || name.includes('find') || name.includes('grep')) {
    return '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M11.742 10.344a6.5 6.5 0 1 0-1.397 1.398h-.001c.03.04.062.078.098.115l3.85 3.85a1 1 0 0 0 1.415-1.414l-3.85-3.85a1.007 1.007 0 0 0-.115-.1zM12 6.5a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0z"/></svg>';
  }
  if (name.includes('shell') || name.includes('exec') || name.includes('run') || name.includes('command')) {
    return '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M6 9a.5.5 0 0 1 .5-.5.5.5 0 0 1 0 1h-3A.5.5 0 0 1 63.854 4.146a.5.5 0 1 0-.708.708L4.793 6.5 3.146 8.146a.5.5 0 1 0 .708.708l2-2a.5.5 0 0 0 0-.708l-2-2z"/><path d="M2 1a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V3a2 2 0 0 0-2-2H2zm12 1a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1h12z"/></svg>';
  }

  // 默认工具图标
  return '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M1 0 0 1l2.2 3.081a1 1 0 0 0 .815.419h.07a1 1 0 0 1 .708.293l2.675 2.675-2.617 2.654A3.003 3.003 0 0 0 0 13a3 3 0 1 0 5.878-.851l2.654-2.617.968.968-.305.914a1 1 0 0 0 .242 1.023l3.27 3.27a.997.997 0 0 0 1.414 0l1.586-1.586a.997.997 0 0 0 0-1.414l-3.27-3.27a1 1 0 0 0-1.023-.242L10.5 9.5l-.96-.96 2.68-2.643A3.005 3.005 0 0 0 16 3c0-.269-.035-.53-.102-.777l-2.14 2.141L12 4l-.364-1.757L13.777.102a3 3 0 0 0-3.675 3.68L7.462 6.46 4.793 3.793a1 1 0 0 1-.293-.707v-.071a1 1 0 0 0-.419-.814L1 0Zm9.646 10.646a.5.5 0 0 1 .708 0l2.914 2.915a.5.5 0 0 1-.707.707l-2.915-2.914a.5.5 0 0 1 0-.708ZM3 11l.471.242.529.026.287.445.445.287.026.529L5 13l-.242.471-.026.529-.445.287-.287.445-.529.026L3 15l-.471-.242L2 14.732l-.287-.445L1.268 14l-.026-.529L1 13l.242-.471.026-.529.445-.287.287-.445.529-.026L3 11Z"/></svg>';
}

