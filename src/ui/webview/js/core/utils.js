// 工具函数集合
// 此文件包含所有通用工具函数

/**
 * HTML 转义
 */
export function escapeHtml(text) {
  if (text == null) return '';
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

/**
 * 格式化时间戳
 */
export function formatTimestamp(timestamp) {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

/**
 * 格式化经过时间
 */
export function formatElapsed(ms) {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const minutes = String(Math.floor(totalSec / 60)).padStart(2, '0');
  const seconds = String(totalSec % 60).padStart(2, '0');
  return `${minutes}:${seconds}`;
}

/**
 * 格式化相对时间（如"刚刚"、"5分钟前"）
 */
export function formatRelativeTime(timestamp) {
  if (!timestamp) return '';
  const now = Date.now();
  const diff = now - timestamp;

  if (diff < 60000) return '刚刚';
  if (diff < 3600000) {
    const mins = Math.floor(diff / 60000);
    return mins + ' 分钟前';
  }
  if (diff < 86400000) {
    const hours = Math.floor(diff / 3600000);
    return hours + ' 小时前';
  }
  if (diff < 604800000) {
    const days = Math.floor(diff / 86400000);
    return days + ' 天前';
  }
  // 超过一周显示具体日期
  return new Date(timestamp).toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' });
}

/**
 * 生成唯一 ID
 */
export function generateId() {
  return 'id-' + Math.random().toString(36).substr(2, 9) + '-' + Date.now();
}

/**
 * 滚动到底部
 * @param {boolean} smooth - 是否使用平滑滚动（默认 false，避免流式输出时抖动）
 */
export function smoothScrollToBottom(smooth = false) {
  const mainContent = document.getElementById('main-content');
  if (mainContent) {
    if (smooth) {
      mainContent.scrollTo({
        top: mainContent.scrollHeight,
        behavior: 'smooth'
      });
    } else {
      // 🔧 优化：直接设置 scrollTop，避免抖动
      mainContent.scrollTop = mainContent.scrollHeight;
    }
  }
}

/**
 * 检查消息是否需要折叠
 */
export function shouldCollapseMessage(content) {
  if (!content) return false;
  const lineCount = (content.match(/\n/g) || []).length + 1;
  const charCount = content.length;
  return lineCount > 15 && charCount > 500;
}

/**
 * 切换消息展开/折叠状态
 */
export function toggleMessageExpand(btn) {
  const wrapper = btn.closest('.message-collapsible-wrapper');
  if (!wrapper) return;

  const contentEl = wrapper.querySelector('.message-content');
  if (!contentEl) return;

  const isCollapsed = contentEl.classList.contains('collapsed');
  if (isCollapsed) {
    contentEl.classList.remove('collapsed');
    contentEl.classList.add('expandable');
    btn.textContent = '收起';
  } else {
    contentEl.classList.add('collapsed');
    contentEl.classList.remove('expandable');
    btn.textContent = '展开更多';
  }
}

/**
 * 解析代码块语言标签
 */
export function parseCodeBlockMeta(langLine) {
  if (!langLine) return { lang: 'text', filepath: null };

  const colonMatch = langLine.match(/^(\w+):(.+)$/);
  if (colonMatch) {
    return { lang: colonMatch[1], filepath: colonMatch[2].trim() };
  }

  const spaceMatch = langLine.match(/^(\w+)\s+(.+)$/);
  if (spaceMatch) {
    return { lang: spaceMatch[1], filepath: spaceMatch[2].trim() };
  }

  return { lang: langLine || 'text', filepath: null };
}

/**
 * 检查是否应该渲染为代码块
 */
export function shouldRenderAsCodeBlock(content) {
  if (!content) return false;
  const trimmed = content.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith('```')) return false;
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return true;
  if (!content.includes('\n')) return false;

  // 特殊行号格式
  if (/^\s*\d+→/m.test(content)) return true;
  if (/^\s*\d+\s*[:>]/m.test(content)) return true;

  // 检测缩进代码
  const lines = content.split('\n');
  const indentedLines = lines.filter(l => /^\s{2,}|^\t/.test(l) && l.trim());
  return indentedLines.length >= 3;
}

/**
 * 提取单个代码块
 */
export function extractSingleCodeFence(content) {
  if (!content) return null;
  const trimmed = content.trim();
  const match = trimmed.match(/^```(\w*)(?::([^\s\n]+)|\s+([^\n]+))?\s*\n([\s\S]*?)\n?```\s*$/);
  if (!match) return null;
  const lang = match[1] || '';
  const filepath = match[2] || match[3] || undefined;
  const body = match[4] || '';
  return { lang: lang, body: body, filepath: filepath };
}

/**
 * 显示 Toast 通知
 */
export function showToast(message, type = 'info') {
  const container = document.getElementById('toast-container');
  if (!container) return;

  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.textContent = message;

  container.appendChild(toast);

  setTimeout(() => {
    toast.classList.add('fade-out');
    setTimeout(() => toast.remove(), 300);
  }, 3000);
}
