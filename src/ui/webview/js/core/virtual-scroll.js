// 虚拟滚动引擎
// 只渲染可视区域内的消息，优化长对话性能

// ============================================
// 配置
// ============================================

const CONFIG = {
  // 预估消息高度（用于初始计算）
  estimatedItemHeight: 120,
  // 缓冲区大小（可视区域外额外渲染的消息数）
  bufferSize: 5,
  // 启用虚拟滚动的最小消息数
  minItemsForVirtualization: 50,
  // 滚动防抖延迟
  scrollDebounceMs: 16
};

// ============================================
// 状态
// ============================================

let containerEl = null;
let contentEl = null;
let items = [];
let itemHeights = new Map(); // 消息索引 -> 实际高度
let scrollTop = 0;
let containerHeight = 0;
let totalHeight = 0;
let visibleRange = { start: 0, end: 0 };
let scrollRAF = null;
let renderCallback = null;

// ============================================
// 核心 API
// ============================================

/**
 * 初始化虚拟滚动
 */
export function initVirtualScroll(container, options = {}) {
  containerEl = container;
  renderCallback = options.renderItem;
  
  // 合并配置
  Object.assign(CONFIG, options.config || {});
  
  // 创建内容容器
  contentEl = container.querySelector('.virtual-scroll-content');
  if (!contentEl) {
    contentEl = document.createElement('div');
    contentEl.className = 'virtual-scroll-content';
    contentEl.style.position = 'relative';
    container.appendChild(contentEl);
  }
  
  // 监听滚动
  container.addEventListener('scroll', handleScroll, { passive: true });
  
  // 监听容器大小变化
  if (typeof ResizeObserver !== 'undefined') {
    const resizeObserver = new ResizeObserver(() => {
      containerHeight = container.clientHeight;
      updateVisibleRange();
    });
    resizeObserver.observe(container);
  }
  
  containerHeight = container.clientHeight;
  
  return {
    setItems,
    updateItem,
    scrollToIndex,
    scrollToBottom,
    destroy
  };
}

/**
 * 设置消息列表
 */
export function setItems(newItems) {
  items = newItems;
  recalculateTotalHeight();
  updateVisibleRange();
  render();
}

/**
 * 更新单个消息
 */
export function updateItem(index, item) {
  if (index >= 0 && index < items.length) {
    items[index] = item;
    // 清除缓存的高度，下次渲染时重新计算
    itemHeights.delete(index);
    render();
  }
}

/**
 * 滚动到指定索引
 */
export function scrollToIndex(index, behavior = 'smooth') {
  if (!containerEl || index < 0 || index >= items.length) return;
  
  let offset = 0;
  for (let i = 0; i < index; i++) {
    offset += getItemHeight(i);
  }
  
  containerEl.scrollTo({ top: offset, behavior });
}

/**
 * 滚动到底部
 */
export function scrollToBottom(behavior = 'smooth') {
  if (!containerEl) return;
  containerEl.scrollTo({ top: totalHeight, behavior });
}

/**
 * 销毁虚拟滚动
 */
export function destroy() {
  if (containerEl) {
    containerEl.removeEventListener('scroll', handleScroll);
  }
  containerEl = null;
  contentEl = null;
  items = [];
  itemHeights.clear();
  if (scrollRAF) {
    cancelAnimationFrame(scrollRAF);
    scrollRAF = null;
  }
}

// ============================================
// 内部函数
// ============================================

function handleScroll() {
  if (scrollRAF) return;

  scrollRAF = requestAnimationFrame(() => {
    scrollRAF = null;
    scrollTop = containerEl.scrollTop;
    updateVisibleRange();
    render();
  });
}

function getItemHeight(index) {
  if (itemHeights.has(index)) {
    return itemHeights.get(index);
  }
  return CONFIG.estimatedItemHeight;
}

function recalculateTotalHeight() {
  totalHeight = 0;
  for (let i = 0; i < items.length; i++) {
    totalHeight += getItemHeight(i);
  }
}

function updateVisibleRange() {
  if (!containerEl || items.length === 0) {
    visibleRange = { start: 0, end: 0 };
    return;
  }

  let offset = 0;
  let start = 0;
  let end = items.length;

  // 找到第一个可见项
  for (let i = 0; i < items.length; i++) {
    const height = getItemHeight(i);
    if (offset + height > scrollTop) {
      start = Math.max(0, i - CONFIG.bufferSize);
      break;
    }
    offset += height;
  }

  // 找到最后一个可见项
  const viewportBottom = scrollTop + containerHeight;
  for (let i = start; i < items.length; i++) {
    offset += getItemHeight(i);
    if (offset > viewportBottom) {
      end = Math.min(items.length, i + CONFIG.bufferSize + 1);
      break;
    }
  }

  visibleRange = { start, end };
}

function render() {
  if (!contentEl || !renderCallback) return;

  // 设置总高度
  contentEl.style.height = totalHeight + 'px';

  // 计算可见项的偏移量
  let offsetTop = 0;
  for (let i = 0; i < visibleRange.start; i++) {
    offsetTop += getItemHeight(i);
  }

  // 渲染可见项
  let html = '';
  for (let i = visibleRange.start; i < visibleRange.end; i++) {
    const item = items[i];
    const itemHtml = renderCallback(item, i);
    html += `<div class="virtual-item" data-index="${i}" style="position:absolute;top:${offsetTop}px;width:100%;">${itemHtml}</div>`;
    offsetTop += getItemHeight(i);
  }

  contentEl.innerHTML = html;

  // 更新实际高度缓存
  contentEl.querySelectorAll('.virtual-item').forEach(el => {
    const index = parseInt(el.dataset.index, 10);
    const actualHeight = el.offsetHeight;
    if (actualHeight > 0 && actualHeight !== itemHeights.get(index)) {
      itemHeights.set(index, actualHeight);
    }
  });
}

// ============================================
// 工具函数
// ============================================

/**
 * 检查是否应该启用虚拟滚动
 */
export function shouldEnableVirtualScroll(itemCount) {
  return itemCount >= CONFIG.minItemsForVirtualization;
}

/**
 * 获取虚拟滚动统计信息
 */
export function getVirtualScrollStats() {
  return {
    totalItems: items.length,
    visibleRange: { ...visibleRange },
    renderedCount: visibleRange.end - visibleRange.start,
    totalHeight,
    scrollTop,
    containerHeight
  };
}