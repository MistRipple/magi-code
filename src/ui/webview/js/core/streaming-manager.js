// 流式更新管理器
// 统一管理所有流式输出，确保一致的更新路径

import { morphElement } from './dom-diff.js';

// 避免循环依赖：通过回调函数注入
let scheduleRenderCallback = null;

/**
 * 设置渲染回调函数
 * @param {Function} callback - 渲染函数
 */
export function setRenderCallback(callback) {
  scheduleRenderCallback = callback;
}

/**
 * 触发渲染
 */
function scheduleRender() {
  if (scheduleRenderCallback) {
    scheduleRenderCallback();
  } else {
    console.warn('[StreamingManager] 渲染回调未设置');
  }
}

/**
 * 流式更新管理器
 * 负责管理所有活跃的流式输出，提供统一的更新接口
 */
class StreamingManager {
  constructor() {
    // 活跃的流式输出 Map: messageId -> StreamState
    this.activeStreams = new Map();

    // 流式更新节流配置
    this.minUpdateInterval = 50; // 最小更新间隔（毫秒）
    this.lastUpdateTime = new Map(); // messageId -> timestamp
  }

  /**
   * 启动流式输出
   * @param {string} messageId - 消息 ID
   * @param {Object} initialData - 初始数据
   */
  startStream(messageId, initialData = {}) {
    if (!messageId) {
      console.warn('[StreamingManager] messageId 为空，无法启动流式输出');
      return;
    }

    const streamState = {
      messageId,
      startTime: Date.now(),
      lastUpdate: Date.now(),
      content: initialData.content || '',
      thinking: initialData.thinking || [],
      toolCalls: initialData.toolCalls || [],
      parsedBlocks: initialData.parsedBlocks || [],
      role: initialData.role || 'assistant',
      source: initialData.source || 'worker',
      agent: initialData.agent || null,
      streaming: true
    };

    this.activeStreams.set(messageId, streamState);
    console.log('[StreamingManager] 启动流式输出:', messageId);
  }

  /**
   * 更新流式输出
   * @param {string} messageId - 消息 ID
   * @param {Object} delta - 增量数据
   */
  updateStream(messageId, delta) {
    const stream = this.activeStreams.get(messageId);
    if (!stream) {
      console.warn('[StreamingManager] 流式输出不存在:', messageId);
      return false;
    }

    // 节流：避免过于频繁的更新
    const now = Date.now();
    const lastUpdate = this.lastUpdateTime.get(messageId) || 0;
    if (now - lastUpdate < this.minUpdateInterval) {
      // 跳过此次更新，但保存数据
      this._applyDelta(stream, delta);
      return false;
    }

    // 应用增量更新
    this._applyDelta(stream, delta);
    stream.lastUpdate = now;
    this.lastUpdateTime.set(messageId, now);

    // 触发 DOM 更新
    this._renderStream(messageId);
    return true;
  }

  /**
   * 完成流式输出
   * @param {string} messageId - 消息 ID
   * @param {Object} finalData - 最终数据（可选）
   */
  completeStream(messageId, finalData = null) {
    const stream = this.activeStreams.get(messageId);
    if (!stream) {
      console.warn('[StreamingManager] 流式输出不存在:', messageId);
      return;
    }

    // 应用最终数据
    if (finalData) {
      this._applyDelta(stream, finalData);
    }

    stream.streaming = false;
    console.log('[StreamingManager] 完成流式输出:', messageId, '耗时:', Date.now() - stream.startTime, 'ms');

    // 清理
    this.activeStreams.delete(messageId);
    this.lastUpdateTime.delete(messageId);

    // 触发最终渲染
    scheduleRender();
  }

  /**
   * 取消流式输出
   * @param {string} messageId - 消息 ID
   */
  cancelStream(messageId) {
    if (this.activeStreams.has(messageId)) {
      console.log('[StreamingManager] 取消流式输出:', messageId);
      this.activeStreams.delete(messageId);
      this.lastUpdateTime.delete(messageId);
      scheduleRender();
    }
  }

  /**
   * 获取流式状态
   * @param {string} messageId - 消息 ID
   */
  getStream(messageId) {
    return this.activeStreams.get(messageId);
  }

  /**
   * 检查是否有活跃的流式输出
   */
  hasActiveStreams() {
    return this.activeStreams.size > 0;
  }

  /**
   * 获取所有活跃的流式输出
   */
  getActiveStreams() {
    return Array.from(this.activeStreams.values());
  }

  /**
   * 清空所有流式输出
   */
  clearAll() {
    console.log('[StreamingManager] 清空所有流式输出');
    this.activeStreams.clear();
    this.lastUpdateTime.clear();
  }

  // ============================================
  // 私有方法
  // ============================================

  /**
   * 应用增量数据到流式状态
   * @private
   */
  _applyDelta(stream, delta) {
    // 文本内容：追加
    if (delta.content !== undefined) {
      if (delta.updateType === 'replace') {
        stream.content = delta.content;
      } else {
        stream.content += delta.content;
      }
    }

    // Thinking：追加
    if (delta.thinking) {
      if (Array.isArray(delta.thinking)) {
        stream.thinking.push(...delta.thinking);
      } else {
        stream.thinking.push(delta.thinking);
      }
    }

    // 工具调用：追加或更新
    if (delta.toolCalls) {
      if (Array.isArray(delta.toolCalls)) {
        delta.toolCalls.forEach(newTool => {
          const existingIndex = stream.toolCalls.findIndex(t => t.toolId === newTool.toolId);
          if (existingIndex >= 0) {
            // 更新现有工具调用
            stream.toolCalls[existingIndex] = { ...stream.toolCalls[existingIndex], ...newTool };
          } else {
            // 添加新工具调用
            stream.toolCalls.push(newTool);
          }
        });
      }
    }

    // Parsed Blocks：替换或追加
    if (delta.parsedBlocks) {
      if (delta.updateType === 'replace') {
        stream.parsedBlocks = delta.parsedBlocks;
      } else {
        stream.parsedBlocks = delta.parsedBlocks;
      }
    }
  }

  /**
   * 渲染单个流式消息
   * @private
   */
  _renderStream(messageId) {
    const stream = this.activeStreams.get(messageId);
    if (!stream) return;

    // 查找对应的 DOM 元素
    const messageEl = document.querySelector(`[data-message-key="${messageId}"]`);
    if (!messageEl) {
      // DOM 元素不存在，触发全量渲染
      console.log('[StreamingManager] DOM 元素不存在，触发全量渲染:', messageId);
      scheduleRender();
      return;
    }

    // 只更新消息内容部分（不更新整个消息块）
    const contentEl = messageEl.querySelector('.message-content');
    if (contentEl && stream.content) {
      // 使用 morphdom 更新内容
      const newContentHTML = this._renderMessageContent(stream);
      try {
        morphElement(contentEl, `<div class="message-content markdown-rendered">${newContentHTML}</div>`);
      } catch (error) {
        console.error('[StreamingManager] morphElement 失败:', error);
        scheduleRender();
      }
    }

    // 更新 Thinking 面板
    if (stream.thinking && stream.thinking.length > 0) {
      this._updateThinkingPanel(messageEl, stream);
    }

    // 更新工具调用
    if (stream.toolCalls && stream.toolCalls.length > 0) {
      this._updateToolCalls(messageEl, stream);
    }
  }

  /**
   * 渲染消息内容
   * @private
   */
  _renderMessageContent(stream) {
    // 这里需要导入 renderMarkdown 函数
    // 为了避免循环依赖，我们直接使用简单的文本渲染
    // 实际项目中应该使用完整的 Markdown 渲染
    const content = stream.content || '';

    // 简单的换行处理
    return content
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>');
  }

  /**
   * 更新 Thinking 面板
   * @private
   */
  _updateThinkingPanel(messageEl, stream) {
    const thinkingEl = messageEl.querySelector('.panel__content');
    if (!thinkingEl) return;

    const thinkingContent = stream.thinking
      .map(t => typeof t === 'string' ? t : t.content)
      .join('\n\n');

    // 使用 morphdom 更新
    try {
      const newHTML = `<div class="panel__content markdown-rendered">${this._renderMessageContent({ content: thinkingContent })}</div>`;
      morphElement(thinkingEl, newHTML);

      // 流式时自动展开
      const panelEl = messageEl.querySelector('.panel--thinking');
      if (panelEl && stream.streaming) {
        panelEl.classList.add('panel--expanded');
      }
    } catch (error) {
      console.error('[StreamingManager] 更新 Thinking 面板失败:', error);
    }
  }

  /**
   * 更新工具调用
   * @private
   */
  _updateToolCalls(messageEl, stream) {
    // 工具调用的更新比较复杂，暂时触发全量渲染
    // 后续可以优化为增量更新
    scheduleRenderMainContent();
  }
}

// 导出单例
export const streamingManager = new StreamingManager();

// 调试接口
if (typeof window !== 'undefined') {
  window.__streamingManager = streamingManager;
}
