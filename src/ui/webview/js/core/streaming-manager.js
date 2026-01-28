// 流式更新管理器
// 统一管理所有流式输出，确保一致的更新路径
// 🔧 重构：不再使用增量 DOM 更新，统一使用全量渲染 + morphdom Diff

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
    // 🔧 优化：从 50ms 减少到 16ms（一帧时间），让更新更平滑
    // 配合 requestAnimationFrame 可实现 60fps 平滑渲染
    this.minUpdateInterval = 16; // 最小更新间隔（毫秒）
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
   * 🔧 重构：由于前端已使用 replace 模式传递完整累积内容
   * 这里主要使用替换逻辑，避免重复追加
   * @private
   */
  _applyDelta(stream, delta) {
    // 文本内容：替换（前端已累积完整内容）
    if (delta.content !== undefined) {
      // 始终使用替换模式，因为 delta.content 已经是完整的累积内容
      stream.content = delta.content;
    }

    // Thinking：替换完整数组（前端已累积）
    if (delta.thinking) {
      if (Array.isArray(delta.thinking)) {
        stream.thinking = delta.thinking;
      } else {
        stream.thinking = [delta.thinking];
      }
    }

    // 工具调用：替换完整数组（前端已累积）
    if (delta.toolCalls) {
      if (Array.isArray(delta.toolCalls)) {
        stream.toolCalls = delta.toolCalls;
      }
    }

    // Parsed Blocks：替换完整数组（前端已累积）
    if (delta.parsedBlocks) {
      stream.parsedBlocks = delta.parsedBlocks;
    }
  }

  /**
   * 渲染单个流式消息
   * 🔧 优化：使用全量渲染 + morphdom Diff，确保流式输出使用 Markdown 格式
   * 性能由双重节流保证：
   * - StreamingManager: minUpdateInterval = 16ms（一帧时间）
   * - requestAnimationFrame: ~16.7ms（浏览器刷新率）
   * - morphdom DOM Diff: 只更新变化的部分
   * @private
   */
  _renderStream(messageId) {
    // 直接调用全量渲染，统一渲染路径
    // 这样可以确保流式输出也使用完整的 Markdown 渲染
    // 而不是简单的文本渲染
    scheduleRender();
  }
}

// 导出单例
export const streamingManager = new StreamingManager();

// 调试接口
if (typeof window !== 'undefined') {
  window.__streamingManager = streamingManager;
}
