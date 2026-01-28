// DOM Diff 模块
// 使用 morphdom 实现高效的 DOM 更新

/**
 * 将容器的 DOM 树变形为新的 HTML 结构
 * @param {HTMLElement} container - 目标容器
 * @param {string} newHTML - 新的 HTML 字符串
 * @param {Object} options - morphdom 选项
 */
export function morphContainer(container, newHTML, options = {}) {
  if (!container) {
    console.error('[DOMDiff] 容器不存在');
    return;
  }

  if (typeof morphdom === 'undefined') {
    console.warn('[DOMDiff] morphdom 未加载，回退到 innerHTML');
    container.innerHTML = newHTML;
    return;
  }

  // 创建临时容器来解析 HTML
  const tempContainer = document.createElement('div');
  tempContainer.innerHTML = newHTML;

  // 默认选项
  const defaultOptions = {
    // 在更新元素前的钩子
    onBeforeElUpdated: (fromEl, toEl) => {
      // 保留正在输入的元素
      if (fromEl.tagName === 'INPUT' || fromEl.tagName === 'TEXTAREA') {
        if (fromEl === document.activeElement) {
          return false; // 不更新正在输入的元素
        }
      }

      // 保留 details 元素的 open 状态
      if (fromEl.tagName === 'DETAILS' && toEl.tagName === 'DETAILS') {
        if (fromEl.open !== toEl.open) {
          toEl.open = fromEl.open;
        }
      }

      return true;
    },

    // 子节点更新完成后的钩子
    onElUpdated: (el) => {
      // 可以在这里添加更新后的处理逻辑
    },

    // 跳过某些节点的更新
    onBeforeNodeDiscarded: (node) => {
      // 保留某些特殊节点
      return true;
    },

    // 子节点添加前的钩子
    onBeforeNodeAdded: (node) => {
      return node;
    },

    // 子节点移除前的钩子
    onNodeDiscarded: (node) => {
      // 清理事件监听器等
    }
  };

  // 合并选项
  const finalOptions = {
    ...defaultOptions,
    ...options,
    childrenOnly: true,  // 只更新子节点，不替换容器本身
    onBeforeNodeAdded: (node) => {
      const handler = options.onBeforeNodeAdded || defaultOptions.onBeforeNodeAdded;
      const result = handler ? handler(node) : node;
      if (result === true) {
        return node;
      }
      return result;
    }
  };

  try {
    // 执行 morphdom，使用 childrenOnly 选项
    morphdom(container, tempContainer, finalOptions);
  } catch (error) {
    console.error('[DOMDiff] morphdom 执行失败:', error);
    // 回退到 innerHTML
    container.innerHTML = newHTML;
  }
}

/**
 * 更新单个元素
 * @param {HTMLElement} element - 目标元素
 * @param {string} newHTML - 新的 HTML 字符串
 */
export function morphElement(element, newHTML) {
  if (!element) {
    console.error('[DOMDiff] 元素不存在');
    return;
  }

  if (typeof morphdom === 'undefined') {
    console.warn('[DOMDiff] morphdom 未加载，回退到 outerHTML');
    const temp = document.createElement('div');
    temp.innerHTML = newHTML;
    const newElement = temp.firstElementChild;
    if (newElement && element.parentNode) {
      element.parentNode.replaceChild(newElement, element);
    }
    return;
  }

  try {
    const temp = document.createElement('div');
    temp.innerHTML = newHTML;
    const newElement = temp.firstElementChild;

    if (newElement) {
      morphdom(element, newElement);
    }
  } catch (error) {
    console.error('[DOMDiff] morphElement 执行失败:', error);
  }
}

/**
 * 检查 morphdom 是否可用
 */
export function isMorphdomAvailable() {
  return typeof morphdom !== 'undefined';
}

/**
 * 等待 morphdom 加载
 */
export function waitForMorphdom(timeout = 5000) {
  return new Promise((resolve, reject) => {
    if (typeof morphdom !== 'undefined') {
      resolve(true);
      return;
    }

    const startTime = Date.now();
    const checkInterval = setInterval(() => {
      if (typeof morphdom !== 'undefined') {
        clearInterval(checkInterval);
        resolve(true);
      } else if (Date.now() - startTime > timeout) {
        clearInterval(checkInterval);
        reject(new Error('morphdom 加载超时'));
      }
    }, 100);
  });
}
