/**
 * WebSocket 客户端封装类
 * 提供自动重连、事件回调、消息收发功能
 */

class WebSocketClient {
  constructor(url, options = {}) {
    // 配置参数
    this.url = url;
    this.reconnectInterval = options.reconnectInterval || 3000; // 重连间隔 3 秒
    this.maxReconnectAttempts = options.maxReconnectAttempts || 5; // 最大重连次数
    this.heartbeatInterval = options.heartbeatInterval || 25000; // 心跳间隔 25 秒

    // 状态管理
    this.ws = null;
    this.reconnectAttempts = 0;
    this.reconnectTimer = null;
    this.heartbeatTimer = null;
    this.isManualClose = false;
    this.connectionId = null;

    // 事件回调
    this.callbacks = {
      onOpen: null,
      onMessage: null,
      onClose: null,
      onError: null,
      onReconnecting: null,
      onReconnectFailed: null
    };
  }

  /**
   * 连接到 WebSocket 服务器
   */
  connect() {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      console.warn('[WebSocketClient] 已存在活动连接');
      return;
    }

    try {
      console.log(`[WebSocketClient] 正在连接: ${this.url}`);
      this.ws = new WebSocket(this.url);

      // 连接成功
      this.ws.onopen = (event) => {
        console.log('[WebSocketClient] ✅ 连接成功');
        this.reconnectAttempts = 0; // 重置重连计数
        this.startHeartbeat(); // 启动心跳

        if (this.callbacks.onOpen) {
          this.callbacks.onOpen(event);
        }
      };

      // 收到消息
      this.ws.onmessage = (event) => {
        try {
          const message = JSON.parse(event.data);
          
          // 处理系统消息
          if (message.type === 'connected') {
            this.connectionId = message.connectionId;
            console.log(`[WebSocketClient] 连接ID: ${this.connectionId}`);
          } else if (message.type === 'pong') {
            // 心跳响应，无需处理
            return;
          }

          if (this.callbacks.onMessage) {
            this.callbacks.onMessage(message, event);
          }
        } catch (error) {
          console.error('[WebSocketClient] 消息解析失败:', error);
        }
      };

      // 连接关闭
      this.ws.onclose = (event) => {
        console.log(`[WebSocketClient] ❌ 连接关闭, 代码: ${event.code}, 原因: ${event.reason}`);
        this.stopHeartbeat();

        if (this.callbacks.onClose) {
          this.callbacks.onClose(event);
        }

        // 自动重连（非手动关闭）
        if (!this.isManualClose) {
          this.attemptReconnect();
        }
      };

      // 连接错误
      this.ws.onerror = (error) => {
        console.error('[WebSocketClient] ⚠️  连接错误:', error);

        if (this.callbacks.onError) {
          this.callbacks.onError(error);
        }
      };

    } catch (error) {
      console.error('[WebSocketClient] 创建连接失败:', error);
      this.attemptReconnect();
    }
  }

  /**
   * 尝试重连
   */
  attemptReconnect() {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error(`[WebSocketClient] 🚫 重连失败，已达到最大尝试次数 (${this.maxReconnectAttempts})`);
      
      if (this.callbacks.onReconnectFailed) {
        this.callbacks.onReconnectFailed(this.reconnectAttempts);
      }
      return;
    }

    this.reconnectAttempts++;
    const delay = this.reconnectInterval * Math.pow(1.5, this.reconnectAttempts - 1); // 指数退避

    console.log(`[WebSocketClient] 🔄 准备重连 (${this.reconnectAttempts}/${this.maxReconnectAttempts}), 延迟: ${delay}ms`);

    if (this.callbacks.onReconnecting) {
      this.callbacks.onReconnecting(this.reconnectAttempts, delay);
    }

    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, delay);
  }

  /**
   * 发送消息
   * @param {Object} data - 消息数据
   * @returns {boolean} 是否发送成功
   */
  send(data) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      console.warn('[WebSocketClient] 连接未就绪，无法发送消息');
      return false;
    }

    try {
      const message = JSON.stringify(data);
      this.ws.send(message);
      console.log('[WebSocketClient] 📤 消息已发送:', data);
      return true;
    } catch (error) {
      console.error('[WebSocketClient] 发送消息失败:', error);
      return false;
    }
  }

  /**
   * 发送聊天消息
   * @param {string} content - 消息内容
   */
  sendChat(content) {
    return this.send({
      type: 'chat',
      content,
      timestamp: Date.now()
    });
  }

  /**
   * 发送私聊消息
   * @param {string} to - 目标连接ID
   * @param {string} content - 消息内容
   */
  sendPrivate(to, content) {
    return this.send({
      type: 'private',
      to,
      content,
      timestamp: Date.now()
    });
  }

  /**
   * 启动心跳
   */
  startHeartbeat() {
    this.heartbeatTimer = setInterval(() => {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.send({ type: 'ping' });
      }
    }, this.heartbeatInterval);

    console.log(`[WebSocketClient] ❤️  心跳已启动，间隔: ${this.heartbeatInterval}ms`);
  }

  /**
   * 停止心跳
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
      console.log('[WebSocketClient] 心跳已停止');
    }
  }

  /**
   * 关闭连接
   * @param {number} code - 关闭代码
   * @param {string} reason - 关闭原因
   */
  close(code = 1000, reason = '客户端主动关闭') {
    this.isManualClose = true;
    this.stopHeartbeat();

    // 清除重连定时器
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    if (this.ws) {
      this.ws.close(code, reason);
      this.ws = null;
    }

    console.log('[WebSocketClient] 连接已关闭');
  }

  /**
   * 注册事件回调
   */
  onOpen(callback) {
    this.callbacks.onOpen = callback;
    return this;
  }

  onMessage(callback) {
    this.callbacks.onMessage = callback;
    return this;
  }

  onClose(callback) {
    this.callbacks.onClose = callback;
    return this;
  }

  onError(callback) {
    this.callbacks.onError = callback;
    return this;
  }

  onReconnecting(callback) {
    this.callbacks.onReconnecting = callback;
    return this;
  }

  onReconnectFailed(callback) {
    this.callbacks.onReconnectFailed = callback;
    return this;
  }

  /**
   * 获取连接状态
   * @returns {string} 状态描述
   */
  getState() {
    if (!this.ws) return 'CLOSED';
    
    const states = ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'];
    return states[this.ws.readyState] || 'UNKNOWN';
  }

  /**
   * 是否已连接
   * @returns {boolean}
   */
  isConnected() {
    return this.ws && this.ws.readyState === WebSocket.OPEN;
  }
}

// 导出（浏览器环境）
if (typeof window !== 'undefined') {
  window.WebSocketClient = WebSocketClient;
}

// 导出（Node.js 环境）
if (typeof module !== 'undefined' && module.exports) {
  module.exports = WebSocketClient;
}
