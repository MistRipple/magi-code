// Markdown 和代码渲染模块
// 负责 Markdown 解析和代码块渲染
// 使用 marked v17 作为 Markdown 解析器
// 使用 Augment 风格的组件设计

import { escapeHtml } from './render-utils.js';
import { collapseState } from '../../core/collapse-state.js';
import { getConfig } from '../../core/config.js';
import { renderCodeBlock, renderInlineCode, renderThinking, renderToolCall } from './components.js';

// ============================================
// Markdown 渲染 (使用 marked v17)
// ============================================

// marked 实例是否已配置
let markedConfigured = false;

/**
 * 配置 marked 渲染器（只执行一次）
 */
function configureMarked() {
  if (markedConfigured || typeof marked === 'undefined') return;

  // marked v17 使用 marked.use() 配置渲染器
  marked.use({
    breaks: true,       // 将换行符转换为 <br>
    gfm: true,          // GitHub Flavored Markdown
    async: false,       // 同步模式
    renderer: {
      // 自定义代码块渲染器
      code(token) {
        const code = token.text || '';
        const lang = token.lang || '';
        return renderCodeBlock({
          code: code,
          language: lang,
          showCopyButton: true
        });
      },

      // 自定义行内代码渲染器
      codespan(token) {
        return renderInlineCode(token.text || '');
      },

      // 自定义链接渲染器
      link(token) {
        const href = token.href || '';
        const title = token.title ? ' title="' + escapeHtml(token.title) + '"' : '';
        const text = token.text || href;
        return '<a href="' + escapeHtml(href) + '" target="_blank" rel="noopener" class="c-link"' + title + '>' + text + '</a>';
      }
    }
  });

  markedConfigured = true;
}

/**
 * 渲染 Markdown 内容
 * @param {string} content - Markdown 内容
 * @returns {string} HTML 字符串
 */
export function renderMarkdown(content) {
  if (!content) return '';

  // 检查 marked 是否可用
  if (typeof marked === 'undefined') {
    console.warn('[renderMarkdown] marked 库未加载，使用简单渲染');
    return escapeHtml(content).replace(/\n/g, '<br>');
  }

  try {
    // 确保 marked 已配置，并移除输出的首尾空白
    configureMarked();
    return marked.parse(content).trim();
  } catch (e) {
    console.error('[renderMarkdown] 解析错误:', e);
    return escapeHtml(content).replace(/\n/g, '<br>');
  }
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
          html += renderCodeBlock({
            code: block.content,
            language: block.language,
            filepath: block.filepath,
            showCopyButton: true,
            showApplyButton: !!block.filepath
          });
        }
        break;

      case 'thinking':
        if (block.content && block.content.trim()) {
          html += renderThinking({
            thinking: [block.content],
            isStreaming: block.isStreaming,
            panelId: 'thinking-' + Math.random().toString(36).substr(2, 9),
            autoExpand: block.isStreaming
          });
        }
        break;

      case 'tool_call':
        html += renderToolCall({
          name: block.toolName,
          input: block.input,
          output: block.output,
          error: block.error,
          panelId: 'tool-' + Math.random().toString(36).substr(2, 9)
        });
        break;

      case 'plan':
        html += renderPlanBlock(block);
        hasMarkdown = true;
        break;

      case 'file_change':
        html += renderFileChangeBlock(block);
        break;

      default:
        if (block.content) {
          html += '<div class="unknown-block">' + escapeHtml(block.content) + '</div>';
        }
    }
  });

  return { html, isMarkdown: hasMarkdown };
}

function renderPlanBlock(plan) {
  const content = renderStructuredPlanContent(plan);
  if (!content) return '';
  return '<div class="plan-content">' + content + '</div>';
}

function renderFileChangeBlock(change) {
  const diff = change.diff && String(change.diff).trim() ? change.diff : '';
  if (diff) {
    return renderCodeBlock({
      code: diff,
      language: 'diff',
      filepath: change.filePath,
      showCopyButton: true
    });
  }

  const labelMap = {
    create: '新增文件',
    modify: '修改文件',
    delete: '删除文件'
  };
  const label = labelMap[change.changeType] || '文件变更';
  const title = change.filePath ? label + ': ' + change.filePath : label;
  return '<div class="unknown-block">' + escapeHtml(title) + '</div>';
}

function renderStructuredPlanContent(plan) {
  let html = '<div class="structured-plan-content">';

  if (plan.goal) {
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title">目标</div>';
    html += '<div class="plan-section-content">' + escapeHtml(plan.goal) + '</div>';
    html += '</div>';
  }

  if (plan.analysis) {
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title">分析</div>';
    html += '<div class="plan-section-content">' + escapeHtml(plan.analysis) + '</div>';
    html += '</div>';
  }

  if (plan.constraints && Array.isArray(plan.constraints) && plan.constraints.length > 0) {
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title">约束条件</div>';
    html += '<ul class="plan-list">';
    for (const constraint of plan.constraints) {
      html += '<li>' + escapeHtml(String(constraint)) + '</li>';
    }
    html += '</ul>';
    html += '</div>';
  }

  if (plan.acceptanceCriteria && Array.isArray(plan.acceptanceCriteria) && plan.acceptanceCriteria.length > 0) {
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title"><svg class="icon-inline" viewBox="0 0 16 16" width="14" height="14"><path fill="currentColor" d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0z"/></svg> 验收标准</div>';
    html += '<ul class="plan-list">';
    for (const criteria of plan.acceptanceCriteria) {
      html += '<li>' + escapeHtml(String(criteria)) + '</li>';
    }
    html += '</ul>';
    html += '</div>';
  }

  if (plan.riskLevel) {
    const riskColors = {
      'low': 'var(--vscode-testing-iconPassed)',
      'medium': 'var(--vscode-editorWarning-foreground)',
      'high': 'var(--vscode-errorForeground)'
    };
    const riskLabels = {
      'low': '低',
      'medium': '中',
      'high': '高'
    };
    const riskColor = riskColors[plan.riskLevel] || 'var(--vscode-foreground)';
    const riskLabel = riskLabels[plan.riskLevel] || plan.riskLevel;
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title">风险等级</div>';
    html += '<div class="plan-section-content">';
    html += '<span class="risk-badge" style="background: ' + riskColor + '; color: #fff; padding: 2px 8px; border-radius: 4px; font-size: 0.9em;">' + escapeHtml(riskLabel) + '</span>';
    html += '</div>';
    html += '</div>';
  }

  if (plan.riskFactors && Array.isArray(plan.riskFactors) && plan.riskFactors.length > 0) {
    html += '<div class="plan-section">';
    html += '<div class="plan-section-title">风险因素</div>';
    html += '<ul class="plan-list risk-list">';
    for (const factor of plan.riskFactors) {
      html += '<li>' + escapeHtml(String(factor)) + '</li>';
    }
    html += '</ul>';
    html += '</div>';
  }

  html += '</div>';
  return html;
}

