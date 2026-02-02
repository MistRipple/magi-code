/**
 * 广播管理器模块
 * 负责向多个客户端广播消息
 */

const logger = require('../utils/logger');

/**
 * 广播管理器类
 */
class BroadcastManager {
  constructor(connectionManager) {
    this.connectionManager = connectionManager;
  }

  /**
   * 向所有客户端广播消息
   * @param {object} message - 消息对象
   * @param {string} excludeClientId - 排除的客户端 ID（可选，通常排除发送者）
   * @returns {object} 广播结果 { success: number, failed: number }
   */
  broadcast(message, excludeClientId = null) {
    const onlineClients = this.connectionManager.getOnlineClients();
    let successCount = 0;
    let failedCount = 0;
    
    onlineClients.forEach(clientId => {
      // 如果指定了排除 ID，则跳过该客户端
      if (excludeClientId && clientId === excludeClientId) {
        return;
      }
      
      const success = this.connectionManager.sendToClient(clientId, message);
      
      if (success) {
        successCount++;
      } else {
        failedCount++;
      }
    });
    
    logger.info(`广播完成: 成功=${successCount}, 失败=${failedCount}`);
    
    return {
      success: successCount,
      failed: failedCount,
      total: onlineClients.length
    };
  }

  /**
   * 向指定的多个客户端广播消息
   * @param {object} message - 消息对象
   * @param {string[]} clientIds - 客户端 ID 数组
   * @returns {object} 广播结果
   */
  broadcastToClients(message, clientIds) {
    let successCount = 0;
    let failedCount = 0;
    
    clientIds.forEach(clientId => {
      const success = this.connectionManager.sendToClient(clientId, message);
      
      if (success) {
        successCount++;
      } else {
        failedCount++;
      }
    });
    
    logger.info(`定向广播完成: 成功=${successCount}, 失败=${failedCount}`);
    
    return {
      success: successCount,
      failed: failedCount,
      total: clientIds.length
    };
  }

  /**
   * 向满足条件的客户端广播消息
   * @param {object} message - 消息对象
   * @param {Function} filterFn - 过滤函数 (clientId, clientInfo) => boolean
   * @returns {object} 广播结果
   */
  broadcastWithFilter(message, filterFn) {
    const onlineClients = this.connectionManager.getOnlineClients();
    const targetClients = [];
    
    // 筛选目标客户端
    onlineClients.forEach(clientId => {
      const clientInfo = this.connectionManager.getClientInfo(clientId);
      
      if (filterFn(clientId, clientInfo)) {
        targetClients.push(clientId);
      }
    });
    
    // 向筛选出的客户端广播
    return this.broadcastToClients(message, targetClients);
  }

  /**
   * 向除指定客户端外的所有人广播（常用于转发某个客户端的消息）
   * @param {object} message - 消息对象
   * @param {string} excludeClientId - 要排除的客户端 ID
   * @returns {object} 广播结果
   */
  broadcastExcept(message, excludeClientId) {
    return this.broadcast(message, excludeClientId);
  }

  /**
   * 获取广播统计信息
   * @returns {object} 统计信息
   */
  getStats() {
    return {
      onlineClients: this.connectionManager.getClientCount(),
      timestamp: Date.now()
    };
  }
}

module.exports = BroadcastManager;
