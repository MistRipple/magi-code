/**
 * WebSocket 连接管理器
 * 负责管理所有客户端连接，提供增删查功能
 */

class ConnectionManager {
  constructor() {
    // 连接池：Map<connectionId, connectionInfo>
    this.connections = new Map();
    // 连接计数器，用于生成唯一ID
    this.connectionCounter = 0;
  }

  /**
   * 添加新连接
   * @param {WebSocket} ws - WebSocket 连接对象
   * @param {Object} metadata - 连接元数据（可选）
   * @returns {string} 连接ID
   */
  addConnection(ws, metadata = {}) {
    const connectionId = `conn_${++this.connectionCounter}_${Date.now()}`;
    
    const connectionInfo = {
      id: connectionId,
      ws,
      metadata,
      connectedAt: new Date(),
      lastHeartbeat: Date.now(),
      isAlive: true
    };

    this.connections.set(connectionId, connectionInfo);
    console.log(`[ConnectionManager] 新连接已添加: ${connectionId}, 当前总连接数: ${this.connections.size}`);
    
    return connectionId;
  }

  /**
   * 移除连接
   * @param {string} connectionId - 连接ID
   * @returns {boolean} 是否成功移除
   */
  removeConnection(connectionId) {
    const result = this.connections.delete(connectionId);
    if (result) {
      console.log(`[ConnectionManager] 连接已移除: ${connectionId}, 剩余连接数: ${this.connections.size}`);
    }
    return result;
  }

  /**
   * 获取连接信息
   * @param {string} connectionId - 连接ID
   * @returns {Object|null} 连接信息
   */
  getConnection(connectionId) {
    return this.connections.get(connectionId) || null;
  }

  /**
   * 获取所有连接
   * @returns {Array} 所有连接信息数组
   */
  getAllConnections() {
    return Array.from(this.connections.values());
  }

  /**
   * 获取所有活跃连接
   * @returns {Array} 活跃连接数组
   */
  getAliveConnections() {
    return this.getAllConnections().filter(conn => conn.isAlive);
  }

  /**
   * 更新心跳时间
   * @param {string} connectionId - 连接ID
   */
  updateHeartbeat(connectionId) {
    const conn = this.connections.get(connectionId);
    if (conn) {
      conn.lastHeartbeat = Date.now();
      conn.isAlive = true;
    }
  }

  /**
   * 标记连接为非活跃
   * @param {string} connectionId - 连接ID
   */
  markAsInactive(connectionId) {
    const conn = this.connections.get(connectionId);
    if (conn) {
      conn.isAlive = false;
    }
  }

  /**
   * 检测超时连接
   * @param {number} timeout - 超时时间（毫秒）
   * @returns {Array} 超时连接ID数组
   */
  getTimeoutConnections(timeout) {
    const now = Date.now();
    const timeoutConnections = [];

    this.connections.forEach((conn, id) => {
      if (now - conn.lastHeartbeat > timeout) {
        timeoutConnections.push(id);
      }
    });

    return timeoutConnections;
  }

  /**
   * 获取连接统计信息
   * @returns {Object} 统计信息
   */
  getStats() {
    const all = this.getAllConnections();
    const alive = this.getAliveConnections();

    return {
      total: all.length,
      alive: alive.length,
      inactive: all.length - alive.length
    };
  }

  /**
   * 清空所有连接
   */
  clear() {
    this.connections.clear();
    console.log('[ConnectionManager] 所有连接已清空');
  }
}

export default ConnectionManager;
