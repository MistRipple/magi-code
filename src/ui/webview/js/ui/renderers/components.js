/**
 * 组件渲染器统一导出
 * 使用新的设计系统组件
 *
 * 这个文件集中导出所有重构后的组件渲染器，方便在其他模块中导入使用
 */

// Thinking 组件
export {
  renderThinking,
  updateThinkingContent,
  toggleThinking,
  completeThinking,
  generateThinkingSummary
} from './thinking-renderer.js';

// ToolCall 组件
export {
  renderToolCall,
  renderToolCallList,
  updateToolCallStatus,
  addToolCallLoading,
  removeToolCallLoading,
  getToolIcon,
  getToolStatus,
  formatToolContent
} from './tool-call-renderer.js';

// CodeBlock 组件
import {
  renderCodeBlock,
  renderInlineCode,
  copyCodeBlockImpl,
  toggleCodeBlockImpl,
  applyCodeBlockImpl,
  getLanguageName,
  generateId as generateCodeBlockId
} from './code-block-renderer.js';

export {
  renderCodeBlock,
  renderInlineCode,
  copyCodeBlockImpl,
  toggleCodeBlockImpl,
  applyCodeBlockImpl,
  getLanguageName,
  generateCodeBlockId
};

/**
 * 全局函数注册（用于在HTML onclick等属性中调用）
 * 应在页面加载时调用此函数
 * 使用同步导入避免竞态条件
 */
export function registerGlobalFunctions() {
  if (typeof window !== 'undefined') {
    // 同步注册代码块相关全局函数
    window.copyCodeBlock = (codeId) => {
      copyCodeBlockImpl(codeId);
    };

    window.toggleCodeBlock = (codeId) => {
      toggleCodeBlockImpl(codeId);
    };

    window.applyCodeBlock = (codeId) => {
      applyCodeBlockImpl(codeId);
    };

    // 设置事件委托处理面板切换
    setupPanelEventDelegation();

    console.log('[Components] Global functions registered successfully');
  }
}

/**
 * 设置面板事件委托
 * 处理 data-action="toggle-panel" 和 data-action="toggle-collapsible" 的点击事件
 */
function setupPanelEventDelegation() {
  // 避免重复绑定
  if (window.__panelEventDelegationSetup) {
    console.log('[Components] Panel event delegation already setup');
    return;
  }
  window.__panelEventDelegationSetup = true;

  console.log('[Components] Setting up panel event delegation');

  // 使用 capture: true 确保事件在捕获阶段被处理
  document.addEventListener('click', (e) => {
    // 忽略按钮点击（复制/应用按钮）
    if (e.target.closest('[data-action="copy-code"]') || e.target.closest('[data-action="apply-code"]')) {
      return;
    }

    // 查找带有 data-action="toggle-collapsible" 的元素（新样式）
    const toggleCollapsible = e.target.closest('[data-action="toggle-collapsible"]');
    if (toggleCollapsible) {
      console.log('[Components] toggle-collapsible clicked', e.target.tagName);
      const collapsible = toggleCollapsible.closest('.c-collapsible');
      if (collapsible) {
        const wasCollapsed = collapsible.classList.contains('is-collapsed');
        collapsible.classList.toggle('is-collapsed');
        console.log('[Components] Panel toggled:', wasCollapsed ? 'expanded' : 'collapsed');
        e.preventDefault();
        e.stopPropagation();
        return;
      }
    }

    // 查找带有 data-action="toggle-panel" 的元素（旧样式兼容）
    const toggleBtn = e.target.closest('[data-action="toggle-panel"]');
    if (toggleBtn) {
      const panel = toggleBtn.closest('.panel, .c-collapsible');
      if (panel) {
        if (panel.classList.contains('c-collapsible')) {
          panel.classList.toggle('is-collapsed');
        } else {
          panel.classList.toggle('panel--collapsed');
        }
        e.preventDefault();
        e.stopPropagation();
      }
    }
  }, true); // capture: true

  console.log('[Components] Panel event delegation setup complete');
}
