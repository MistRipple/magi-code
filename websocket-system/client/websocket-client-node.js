/**
 * WebSocket 客户端封装类 (Node.js 版本)
 * 提供自动重连、事件回调、消息收发功能
 * 
 * {{ AURA: Add - 创建 Node.js 环境专用客户端，解决浏览器 API 依赖问题 }}
 */

import WebSocket from 'ws';

class WebSocketClientNode {
  constructor(url, options = {}) {
    // 配置参数
    this.url = url;
    this.reconnectInterval = options.reconnectInterval || 3000; // 重连间隔 3 秒
    this.maxReconnectAttempts = options.maxReconnectAttempts || 5; // 最大重连次数
    this.heartbeatInterval = options.heartbeatInterval || 25000; // 心跳间隔 25 秒
    this.connectionTimeout = options.connectionTimeout || 10000; // 连接超时 10 秒

    // 状态管理
    this.ws = null;
    this.reconnectAttempts = 0;
    this.reconnectTimer = null;
    this.heartbeatTimer = null;
    this.isManualClose = false;
    this.connectionId = null;
    this.isReconnecting = false;

    // 消息队列（离线缓存）
    this.messageQueue = [];
    this.maxQueueSize = options.maxQueueSize || 100;

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
   * {{ AURA: Modify - 添加连接超时处理和状态检查 }}
   */
  connect() {
    // 防止重复连接
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      console.warn('[WebSocketClientNode] 已存在活动连接或正在连接中');
      return Promise.resolve();
    }

    return new Promise((resolve, reject) => {
      try {
        console.log(`[WebSocketClientNode] 正在连接: ${this.url}`);
        this.ws = new WebSocket(this.url);
        
        // 连接超时处理
        const connectionTimer = setTimeout(() => {
          if (this.ws.readyState !== WebSocket.OPEN) {
            console.error('[WebSocketClientNode] ⏱️  连接超时');
            this.ws.terminate();
            reject(new Error('连接超时'));
          }
        }, this.connectionTimeout);

        // 连接成功
        this.ws.on('open', () => {
          clearTimeout(connectionTimer);
          console.log('[WebSocketClientNode] ✅ 连接成功');
          this.reconnectAttempts = 0; // 重置重连计数
          this.isReconnecting = false;
          this.startHeartbeat(); // 启动心跳
          this.flushMessageQueue(); // 发送队列中的消息

          if (this.callbacks.onOpen) {
            this.callbacks.onOpen();
          }
          resolve();
        });

        // 收到消息
        this.ws.on('message', (data) => {
          this.handleMessage(data);
        });

        // 连接关闭
        this.ws.on('close', (code, reason) => {
          clearTimeout(connectionTimer);
          console.log(`[WebSocketClientNode] ❌ 连接关闭, 代码: ${code}, 原因: ${reason.toString()}`);
          this.stopHeartbeat();
          this.connectionId = null;

          if (this.callbacks.onClose) {
            this.callbacks.onClose({ code, reason: reason.toString() });
          }

          // 自动重连（非手动关闭且非重连中）
          if (!this.isManualClose && !this.isReconnecting) {
            this.attemptReconnect();
          }
        });

        // 连接错误
        this.ws.on('error', (error) => {
          clearTimeout(connectionTimer);
          console.error('[WebSocketClientNode] ⚠️  连接错误:', error.message);

          if (this.callbacks.onError) {
            this.callbacks.onError(error);
          }
          
          // 首次连接失败时 reject
          if (this.reconnectAttempts === 0 && !this.isReconnecting) {
            reject(error);
          }
        });

      } catch (error) {
        console.error('[WebSocketClientNode] 创建连接失败:', error);
        reject(error);
      }
    });
  }

  /**
   * 处理收到的消息
   * {{ AURA: Add - 统一消息处理逻辑，支持 JSON 和文本 }}
   */
  handleMessage(data) {
    try {
      const message = JSON.parse(data.toString());
      
      // 处理系统消息
      if (message.type === 'connected') {
        this.connectionId = message.connectionId;
        console.log(`[WebSocketClientNode] 连接ID: ${this.connectionId}`);
      } else if (message.type === 'pong') {
        // 心跳响应，记录延迟
        const latency = Date.now() - message.timestamp;
        // console.log(`[WebSocketClientNode] ❤️  心跳延迟: ${latency}ms`);
        return;
      }

      // 触发消息回调
      if (this.callbacks.onMessage) {
        this.callbacks.onMessage(message);
      }
    } catch (error) {
      console.error('[WebSocketClientNode] 消息解析失败:', error);
      // 非 JSON 消息也传递给回调
      if (this.callbacks.onMessage) {
        this.callbacks.onMessage(data.toString());
      }
    }
  }

  /**
   * 尝试重连
   * {{ AURA: Modify - 使用指数退避算法，避免连接风暴 }}
   */
  attemptReconnect() {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error(`[WebSocketClientNode] 🚫 重连失败，已达到最大尝试次数 (${this.maxReconnectAttempts})`);
      
      if (this.callbacks.onReconnectFailed) {
        this.callbacks.onReconnectFailed(this.reconnectAttempts);
      }
      return;
    }

    this.reconnectAttempts++;
    this.isReconnecting = true;
    
    // 指数退避算法：delay = baseDelay * (backoffFactor ^ (attempts - 1))
    const baseDelay = this.reconnectInterval;
    const backoffFactor = 1.5;
    const delay = Math.min(baseDelay * Math.pow(backoffFactor, this.reconnectAttempts - 1), 30000); // 最大 30 秒

    console.log(`[WebSocketClientNode] 🔄 准备重连 (${this.reconnectAttempts}/${this.maxReconnectAttempts}), 延迟: ${Math.round(delay)}ms`);

    if (this.callbacks.onReconnecting) {
      this.callbacks.onReconnecting(this.reconnectAttempts, delay);
    }

    this.reconnectTimer = setTimeout(() => {
      this.connect().catch(error => {
        console.error('[WebSocketClientNode] 重连失败:', error.message);
        // 继续尝试重连
        this.isReconnecting = false;
        this.attemptReconnect();
      });
    }, delay);
  }

  /**
   * 发送消息
   * {{ AURA: Modify - 添加离线消息队列和发送确认 }}
   * @param {Object|string} data - 消息数据
   * @param {Object} options - 发送选项
   * @returns {Promise<boolean>} 是否发送成功
   */
  send(data, options = {}) {
    const { queueIfOffline = true } = options;

    // 检查连接状态
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      console.warn('[WebSocketClientNode] 连接未就绪');
      
      // 如果启用离线队列，加入队列
      if (queueIfOffline && this.messageQueue.length < this.maxQueueSize) {
        this.messageQueue.push(data);
        console.log(`[WebSocketClientNode] 消息已加入队列 (${this.messageQueue.length}/${this.maxQueueSize})`);
        return Promise.resolve(false);
      }
      
      return Promise.reject(new Error('连接未就绪且队列已满'));
    }

    return new Promise((resolve, reject) => {
      try {
        const message = typeof data === 'string' ? data : JSON.stringify(data);
        this.ws.send(message, (error) => {
          if (error) {
            console.error('[WebSocketClientNode] 发送消息失败:', error);
            reject(error);
          } else {
            console.log('[WebSocketClientNode] 📤 消息已发送');
            resolve(true);
          }
        });
      } catch (error) {
        console.error('[WebSocketClientNode] 发送消息异常:', error);
        reject(error);
      }
    });
  }

  /**
   * 刷新消息队列
   * {{ AURA: Add - 连接恢复后发送缓存的消息 }}
   */
  async flushMessageQueue() {
    if (this.messageQueue.length === 0) return;

    console.log(`[WebSocketClientNode] 📨 开始发送队列消息 (${this.messageQueue.length} 条)`);
    const queue = [...this.messageQueue];
    this.messageQueue = [];

    for (const message of queue) {
      try {
        await this.send(message, { queueIfOffline: false });
      } catch (error) {
        console.error('[WebSocketClientNode] 队列消息发送失败:', error);
        // 失败的消息重新加入队列
        if (this.messageQueue.length < this.maxQueueSize) {
          this.messageQueue.push(message);
        }
      }
    }
  }

  /**
   * 发送聊天消息
   * {{ AURA: Add - 便捷方法 }}
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
   * {{ AURA: Add - 便捷方法 }}
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
   * {{ AURA: Modify - 心跳消息携带时间戳用于延迟计算 }}
   */
  startHeartbeat() {
    this.heartbeatTimer = setInterval(() => {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.send({ 
          type: 'ping',
          timestamp: Date.now() 
        }).catch(() => {
          // 心跳发送失败，可能连接已断开
          console.warn('[WebSocketClientNode] 心跳发送失败');
        });
      }
    }, this.heartbeatInterval);

    console.log(`[WebSocketClientNode] ❤️  心跳已启动，间隔: ${this.heartbeatInterval}ms`);
  }

  /**
   * 停止心跳
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
      console.log('[WebSocketClientNode] 心跳已停止');
    }
  }

  /**
   * 关闭连接
   * {{ AURA: Modify - 清理所有定时器和状态 }}
   */
  close(code = 1000, reason = '客户端主动关闭') {
    this.isManualClose = true;
    this.stopHeartbeat();

    // 清除重连定时器
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    // 清空消息队列
    this.messageQueue = [];

    if (this.ws) {
      this.ws.close(code, reason);
      this.ws = null;
    }

    console.log('[WebSocketClientNode] 连接已关闭');
  }

  /**
   * 手动触发重连
   * {{ AURA: Add - 提供手动重连接口 }}
   */
  reconnect() {
    if (this.ws) {
      this.close();
    }
    this.isManualClose = false;
    this.reconnectAttempts = 0;
    return this.connect();
  }

  /**
   * 注册事件回调
   * {{ AURA: Add - 链式调用支持 }}
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
   * {{ AURA: Add - 状态查询接口 }}
   */
  getState() {
    if (!this.ws) return 'CLOSED';
    
    const states = ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'];
    return states[this.ws.readyState] || 'UNKNOWN';
  }

  /**
   * 是否已连接
   */
  isConnected() {
    return this.ws && this.ws.readyState === WebSocket.OPEN;
  }

  /**
   * 获取连接信息
   * {{ AURA: Add - 提供诊断信息 }}
   */
  getInfo() {
    return {
      url: this.url,
      state: this.getState(),
      connectionId: this.connectionId,
      reconnectAttempts: this.reconnectAttempts,
      queuedMessages: this.messageQueue.length,
      isReconnecting: this.isReconnecting
    };
  }
}

export default WebSocketClientNode;
