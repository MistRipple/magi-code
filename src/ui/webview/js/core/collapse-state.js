// 折叠状态管理器
// 统一管理所有可折叠面板的展开/折叠状态

/**
 * 折叠状态管理器
 * 负责管理代码块、Thinking 面板、工具调用等的折叠状态
 */
class CollapseStateManager {
  constructor() {
    // 状态存储：panelId -> boolean (true = 展开, false = 折叠)
    this.state = new Map();

    // 从 localStorage 加载状态
    this.loadFromStorage();
  }

  /**
   * 切换面板的展开/折叠状态
   * @param {string} panelId - 面板 ID
   * @returns {boolean} 新的状态（true = 展开）
   */
  toggle(panelId) {
    if (!panelId) {
      console.warn('[CollapseState] panelId 为空');
      return true;
    }

    const current = this.state.get(panelId);
    const newState = current === undefined ? false : !current; // 默认展开，切换后折叠
    this.state.set(panelId, newState);

    // 保存到 localStorage
    this.saveToStorage();

    console.log('[CollapseState] 切换状态:', panelId, '->', newState ? '展开' : '折叠');
    return newState;
  }

  /**
   * 设置面板的展开/折叠状态
   * @param {string} panelId - 面板 ID
   * @param {boolean} expanded - 是否展开
   */
  set(panelId, expanded) {
    if (!panelId) return;

    this.state.set(panelId, Boolean(expanded));
    this.saveToStorage();
  }

  /**
   * 获取面板的展开状态
   * @param {string} panelId - 面板 ID
   * @param {boolean} defaultExpanded - 默认状态（默认展开）
   * @returns {boolean} 是否展开
   */
  isExpanded(panelId, defaultExpanded = true) {
    if (!panelId) return defaultExpanded;

    const state = this.state.get(panelId);
    return state === undefined ? defaultExpanded : state;
  }

  /**
   * 展开面板
   * @param {string} panelId - 面板 ID
   */
  expand(panelId) {
    this.set(panelId, true);
  }

  /**
   * 折叠面板
   * @param {string} panelId - 面板 ID
   */
  collapse(panelId) {
    this.set(panelId, false);
  }

  /**
   * 清除指定面板的状态
   * @param {string} panelId - 面板 ID
   */
  clear(panelId) {
    if (!panelId) return;

    this.state.delete(panelId);
    this.saveToStorage();
  }

  /**
   * 清除所有状态
   */
  clearAll() {
    this.state.clear();
    this.saveToStorage();
    console.log('[CollapseState] 已清除所有状态');
  }

  /**
   * 清除会话相关的状态
   * @param {string} sessionId - 会话 ID
   */
  clearSession(sessionId) {
    if (!sessionId) return;

    let count = 0;
    for (const [panelId] of this.state) {
      if (panelId.startsWith(`session-${sessionId}-`)) {
        this.state.delete(panelId);
        count++;
      }
    }

    if (count > 0) {
      this.saveToStorage();
      console.log('[CollapseState] 已清除会话状态:', sessionId, '共', count, '项');
    }
  }

  /**
   * 获取所有状态
   * @returns {Object} 状态对象
   */
  getAll() {
    return Object.fromEntries(this.state);
  }

  /**
   * 获取状态统计
   * @returns {Object} 统计信息
   */
  getStats() {
    const total = this.state.size;
    let expanded = 0;
    let collapsed = 0;

    for (const [, state] of this.state) {
      if (state) {
        expanded++;
      } else {
        collapsed++;
      }
    }

    return { total, expanded, collapsed };
  }

  /**
   * 保存状态到 localStorage
   */
  saveToStorage() {
    try {
      const data = Object.fromEntries(this.state);
      localStorage.setItem('collapse-state', JSON.stringify(data));
    } catch (error) {
      console.error('[CollapseState] 保存状态失败:', error);
    }
  }

  /**
   * 从 localStorage 加载状态
   */
  loadFromStorage() {
    try {
      const data = localStorage.getItem('collapse-state');
      if (data) {
        const parsed = JSON.parse(data);
        this.state = new Map(Object.entries(parsed));
        console.log('[CollapseState] 状态已加载，共', this.state.size, '项');
      }
    } catch (error) {
      console.error('[CollapseState] 加载状态失败:', error);
      this.state = new Map();
    }
  }

  /**
   * 导出状态（用于调试）
   */
  export() {
    return JSON.stringify(Object.fromEntries(this.state), null, 2);
  }

  /**
   * 导入状态（用于调试）
   * @param {string} json - JSON 字符串
   */
  import(json) {
    try {
      const data = JSON.parse(json);
      this.state = new Map(Object.entries(data));
      this.saveToStorage();
      console.log('[CollapseState] 状态已导入');
    } catch (error) {
      console.error('[CollapseState] 导入状态失败:', error);
    }
  }
}

// 导出单例
export const collapseState = new CollapseStateManager();

// 全局函数：切换面板（供 HTML onclick 使用）
if (typeof window !== 'undefined') {
  window.togglePanel = function(panelId) {
    const expanded = collapseState.toggle(panelId);

    // 更新 DOM
    const panel = document.querySelector(`[data-panel-id="${panelId}"]`);
    if (panel) {
      const content = panel.querySelector('.collapsible-content');
      const icon = panel.querySelector('.collapsible-icon');

      if (content) {
        if (expanded) {
          content.classList.add('expanded');
        } else {
          content.classList.remove('expanded');
        }
      }

      if (icon) {
        if (expanded) {
          icon.classList.add('expanded');
        } else {
          icon.classList.remove('expanded');
        }
      }
    }
  };

  // 全局函数：切换代码块（供 HTML onclick 使用）
  window.toggleCodeBlock = function(codeId) {
    const expanded = collapseState.toggle(codeId);

    // 更新 DOM
    const codeBlock = document.querySelector(`[data-code-id="${codeId}"]`);
    if (codeBlock) {
      const truncated = codeBlock.querySelector('.c-codeblock__truncated');
      if (truncated) {
        if (expanded) {
          truncated.classList.add('expanded');
        } else {
          truncated.classList.remove('expanded');
        }
      }
    }
  };

  // 调试接口
  window.__collapseState = collapseState;
}
