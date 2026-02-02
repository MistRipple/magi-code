/**
 * WebSocket 服务契约适配器
 * 提供符合项目 API 接口契约的标准化接口
 */

import WebSocketServerCore from '../server/websocket-server.js';

/**
 * 契约接口：消息推送服务
 */
class MessagePushServiceContract {
  /**
   * 初始化服务
   * @param {Object} config - 配置对象
   * @returns {Promise<void>}
   */
  async initialize(config) {
    throw new Error('必须实现 initialize 方法');
  }

  /**
   * 发送消息给单个客户端
   * @param {string} clientId - 客户端ID
   * @param {Object} message - 消息对象
   * @returns {Promise<boolean>}
   */
  async sendToClient(clientId, message) {
    throw new Error('必须实现 sendToClient 方法');
  }

  /**
   * 广播消息给所有客户端
   * @param {Object} message - 消息对象
   * @param {Object} options - 广播选项
   * @returns {Promise<Object>}
   */
  async broadcast(message, options = {}) {
    throw new Error('必须实现 broadcast 方法');
  }

  /**
   * 获取服务状态
   * @returns {Promise<Object>}
   */
  async getStatus() {
    throw new Error('必须实现 getStatus 方法');
  }

  /**
   * 关闭服务
   * @returns {Promise<void>}
   */
  async shutdown() {
    throw new Error('必须实现 shutdown 方法');
  }
}

/**
 * WebSocket 服务适配器实现
 * 实现 MessagePushServiceContract 契约
 */
class WebSocketServiceAdapter extends MessagePushServiceContract {
  constructor() {
    super();
    this.server = null;
    this.isInitialized = false;
    this.config = null;
    
    // 事件监听器注册表
    this.eventListeners = {
      onConnection: [],
      onMessage: [],
      onDisconnect: [],
      onError: []
    };
  }

  /**
   * 初始化服务（实现契约）
   * @param {Object} config - 配置对象
   * @param {number} config.port - 监听端口
   * @param {number} config.heartbeatInterval - 心跳间隔
   * @param {number} config.heartbeatTimeout - 心跳超时
   * @returns {Promise<void>}
   */
  async initialize(config = {}) {
    if (this.isInitialized) {
      throw new Error('服务已初始化');
    }

    this.config = {
      port: config.port || 8080,
      heartbeatInterval: config.heartbeatInterval || 30000,
      heartbeatTimeout: config.heartbeatTimeout || 35000,
      ...config
    };

    // 创建 WebSocket 服务器实例
    this.server = new WebSocketServerCore(this.config);

    // 注册内部事件处理
    this._setupEventHandlers();

    // 启动服务
    await this.server.start();
    
    this.isInitialized = true;
    console.log('[契约适配器] WebSocket 服务初始化成功');
  }

  /**
   * 发送消息给单个客户端（实现契约）
   * @param {string} clientId - 客户端ID（连接ID）
   * @param {Object} message - 消息对象
   * @returns {Promise<boolean>}
   */
  async sendToClient(clientId, message) {
    this._ensureInitialized();

    try {
      const success = this.server.sendToClient(clientId, {
        ...message,
        timestamp: message.timestamp || Date.now()
      });

      return success;
    } catch (error) {
      console.error('[契约适配器] 发送消息失败:', error);
      return false;
    }
  }

  /**
   * 广播消息给所有客户端（实现契约）
   * @param {Object} message - 消息对象
   * @param {Object} options - 广播选项
   * @param {Array<string>} options.exclude - 排除的客户端ID列表
   * @param {Function} options.filter - 客户端过滤函数
   * @returns {Promise<Object>}
   */
  async broadcast(message, options = {}) {
    this._ensureInitialized();

    try {
      const excludeIds = options.exclude || [];
      
      // 如果提供了过滤函数，先获取所有连接并过滤
      if (options.filter && typeof options.filter === 'function') {
        const allConnections = this.server.connectionManager.getAllConnections();
        const filteredIds = allConnections
          .filter(conn => !options.filter(conn))
          .map(conn => conn.id);
        
        excludeIds.push(...filteredIds);
      }

      const stats = this.server.broadcast({
        ...message,
        timestamp: message.timestamp || Date.now()
      }, excludeIds);

      return {
        success: true,
        sentCount: stats.success,
        failedCount: stats.failed,
        totalCount: stats.success + stats.failed
      };
    } catch (error) {
      console.error('[契约适配器] 广播消息失败:', error);
      return {
        success: false,
        sentCount: 0,
        failedCount: 0,
        totalCount: 0,
        error: error.message
      };
    }
  }

  /**
   * 获取服务状态（实现契约）
   * @returns {Promise<Object>}
   */
  async getStatus() {
    if (!this.isInitialized) {
      return {
        initialized: false,
        running: false,
        connections: { total: 0, alive: 0, inactive: 0 },
        uptime: 0
      };
    }

    const stats = this.server.getStats();
    
    return {
      initialized: this.isInitialized,
      running: true,
      port: this.config.port,
      connections: {
        total: stats.total,
        alive: stats.alive,
        inactive: stats.inactive
      },
      uptime: stats.uptime,
      config: {
        heartbeatInterval: this.config.heartbeatInterval,
        heartbeatTimeout: this.config.heartbeatTimeout
      }
    };
  }

  /**
   * 关闭服务（实现契约）
   * @returns {Promise<void>}
   */
  async shutdown() {
    if (!this.isInitialized) {
      return;
    }

    console.log('[契约适配器] 正在关闭 WebSocket 服务...');
    
    if (this.server) {
      await this.server.close();
    }

    this.isInitialized = false;
    this.server = null;
    
    console.log('[契约适配器] WebSocket 服务已关闭');
  }

  /**
   * 注册连接事件监听器
   * @param {Function} callback - 回调函数 (clientId, metadata) => {}
   */
  onConnection(callback) {
    this.eventListeners.onConnection.push(callback);
    return this;
  }

  /**
   * 注册消息事件监听器
   * @param {Function} callback - 回调函数 (clientId, message) => {}
   */
  onMessage(callback) {
    this.eventListeners.onMessage.push(callback);
    return this;
  }

  /**
   * 注册断开事件监听器
   * @param {Function} callback - 回调函数 (clientId, reason) => {}
   */
  onDisconnect(callback) {
    this.eventListeners.onDisconnect.push(callback);
    return this;
  }

  /**
   * 注册错误事件监听器
   * @param {Function} callback - 回调函数 (clientId, error) => {}
   */
  onError(callback) {
    this.eventListeners.onError.push(callback);
    return this;
  }

  /**
   * 获取所有连接的客户端列表
   * @returns {Array<Object>}
   */
  getConnectedClients() {
    this._ensureInitialized();
    
    return this.server.connectionManager.getAllConnections().map(conn => ({
      id: conn.id,
      connectedAt: conn.connectedAt,
      lastHeartbeat: new Date(conn.lastHeartbeat),
      isAlive: conn.isAlive,
      metadata: conn.metadata
    }));
  }

  /**
   * 断开指定客户端
   * @param {string} clientId - 客户端ID
   * @param {string} reason - 断开原因
   * @returns {boolean}
   */
  disconnectClient(clientId, reason = '服务端主动断开') {
    this._ensureInitialized();
    
    const conn = this.server.connectionManager.getConnection(clientId);
    if (conn && conn.ws) {
      conn.ws.close(1000, reason);
      return true;
    }
    return false;
  }

  /**
   * 设置内部事件处理器
   * @private
   */
  _setupEventHandlers() {
    // 连接事件
    this.server.onConnection((connectionId, req) => {
      const metadata = {
        ip: req.socket.remoteAddress,
        userAgent: req.headers['user-agent'],
        connectedAt: new Date()
      };

      this.eventListeners.onConnection.forEach(callback => {
        try {
          callback(connectionId, metadata);
        } catch (error) {
          console.error('[契约适配器] 连接事件回调错误:', error);
        }
      });
    });

    // 消息事件
    this.server.onMessage((connectionId, message) => {
      this.eventListeners.onMessage.forEach(callback => {
        try {
          callback(connectionId, message);
        } catch (error) {
          console.error('[契约适配器] 消息事件回调错误:', error);
        }
      });
    });

    // 关闭事件
    this.server.onClose((connectionId, code, reason) => {
      this.eventListeners.onDisconnect.forEach(callback => {
        try {
          callback(connectionId, { code, reason });
        } catch (error) {
          console.error('[契约适配器] 断开事件回调错误:', error);
        }
      });
    });

    // 错误事件
    this.server.onError((connectionId, error) => {
      this.eventListeners.onError.forEach(callback => {
        try {
          callback(connectionId, error);
        } catch (error) {
          console.error('[契约适配器] 错误事件回调错误:', error);
        }
      });
    });
  }

  /**
   * 确保服务已初始化
   * @private
   */
  _ensureInitialized() {
    if (!this.isInitialized) {
      throw new Error('服务未初始化，请先调用 initialize()');
    }
  }
}

// 导出契约和适配器
export { MessagePushServiceContract, WebSocketServiceAdapter };
export default WebSocketServiceAdapter;
