/**
 * 房间管理器
 * 负责管理房间的创建、销毁、用户订阅/退订
 */

class RoomManager {
  constructor() {
    // 房间存储：Map<roomId, Room>
    this.rooms = new Map();
    
    // 用户房间映射：Map<userId, Set<roomId>>
    this.userRooms = new Map();
  }

  /**
   * 创建房间
   * @param {string} roomId - 房间ID
   * @param {Object} metadata - 房间元数据
   */
  createRoom(roomId, metadata = {}) {
    if (this.rooms.has(roomId)) {
      console.log(`[RoomManager] 房间已存在: ${roomId}`);
      return false;
    }

    const room = {
      id: roomId,
      metadata: {
        roomId,
        name: metadata.name || roomId,
        description: metadata.description || '',
        createdAt: Date.now(),
        createdBy: metadata.createdBy,
        maxMembers: metadata.maxMembers || 100,
        isPrivate: metadata.isPrivate || false,
        customData: metadata.customData || {}
      },
      members: new Map(), // Map<userId, RoomMember>
      createdAt: Date.now()
    };

    this.rooms.set(roomId, room);
    console.log(`[RoomManager] 房间已创建: ${roomId}`);
    return true;
  }

  /**
   * 删除房间
   * @param {string} roomId - 房间ID
   */
  deleteRoom(roomId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      return false;
    }

    // 清理所有成员的房间映射
    room.members.forEach((member, userId) => {
      const userRoomSet = this.userRooms.get(userId);
      if (userRoomSet) {
        userRoomSet.delete(roomId);
        if (userRoomSet.size === 0) {
          this.userRooms.delete(userId);
        }
      }
    });

    this.rooms.delete(roomId);
    console.log(`[RoomManager] 房间已删除: ${roomId}`);
    return true;
  }

  /**
   * 用户加入房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID
   */
  joinRoom(roomId, userId, connectionId) {
    // 如果房间不存在，自动创建
    if (!this.rooms.has(roomId)) {
      this.createRoom(roomId);
    }

    const room = this.rooms.get(roomId);
    
    // 检查房间成员数限制
    if (room.members.size >= room.metadata.maxMembers) {
      console.warn(`[RoomManager] 房间已满: ${roomId}`);
      return { success: false, reason: '房间已满' };
    }

    // 添加或更新成员
    if (room.members.has(userId)) {
      // 用户已在房间中，添加新的连接ID
      const member = room.members.get(userId);
      if (!member.connectionIds.includes(connectionId)) {
        member.connectionIds.push(connectionId);
      }
    } else {
      // 新成员加入
      room.members.set(userId, {
        userId,
        connectionIds: [connectionId],
        joinedAt: Date.now(),
        role: room.members.size === 0 ? 'owner' : 'member'
      });
    }

    // 更新用户房间映射
    if (!this.userRooms.has(userId)) {
      this.userRooms.set(userId, new Set());
    }
    this.userRooms.get(userId).add(roomId);

    console.log(`[RoomManager] 用户 ${userId} 加入房间 ${roomId}`);
    return { 
      success: true, 
      memberCount: room.members.size,
      members: this.getRoomMembers(roomId)
    };
  }

  /**
   * 用户离开房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID（可选，留空则移除用户所有连接）
   */
  leaveRoom(roomId, userId, connectionId = null) {
    const room = this.rooms.get(roomId);
    if (!room || !room.members.has(userId)) {
      return false;
    }

    const member = room.members.get(userId);

    if (connectionId) {
      // 只移除特定连接
      member.connectionIds = member.connectionIds.filter(id => id !== connectionId);
      
      // 如果用户没有剩余连接，则完全移除
      if (member.connectionIds.length === 0) {
        room.members.delete(userId);
        
        // 更新用户房间映射
        const userRoomSet = this.userRooms.get(userId);
        if (userRoomSet) {
          userRoomSet.delete(roomId);
          if (userRoomSet.size === 0) {
            this.userRooms.delete(userId);
          }
        }
      }
    } else {
      // 移除用户的所有连接
      room.members.delete(userId);
      
      // 更新用户房间映射
      const userRoomSet = this.userRooms.get(userId);
      if (userRoomSet) {
        userRoomSet.delete(roomId);
        if (userRoomSet.size === 0) {
          this.userRooms.delete(userId);
        }
      }
    }

    console.log(`[RoomManager] 用户 ${userId} 离开房间 ${roomId}`);

    // 如果房间为空，自动删除
    if (room.members.size === 0) {
      this.deleteRoom(roomId);
    }

    return true;
  }

  /**
   * 获取房间成员列表
   * @param {string} roomId - 房间ID
   * @returns {Array} 成员列表
   */
  getRoomMembers(roomId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      return [];
    }

    return Array.from(room.members.values()).map(member => ({
      userId: member.userId,
      connectionIds: member.connectionIds,
      joinedAt: member.joinedAt,
      role: member.role
    }));
  }

  /**
   * 获取房间所有连接ID
   * @param {string} roomId - 房间ID
   * @returns {Array} 连接ID列表
   */
  getRoomConnectionIds(roomId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      return [];
    }

    const connectionIds = [];
    room.members.forEach(member => {
      connectionIds.push(...member.connectionIds);
    });
    return connectionIds;
  }

  /**
   * 获取用户加入的所有房间
   * @param {string} userId - 用户ID
   * @returns {Array} 房间ID列表
   */
  getUserRooms(userId) {
    const roomSet = this.userRooms.get(userId);
    if (!roomSet) {
      return [];
    }

    return Array.from(roomSet).map(roomId => {
      const room = this.rooms.get(roomId);
      const member = room ? room.members.get(userId) : null;
      return {
        roomId,
        name: room?.metadata.name,
        joinedAt: member?.joinedAt,
        memberCount: room?.members.size || 0
      };
    });
  }

  /**
   * 检查房间是否存在
   * @param {string} roomId - 房间ID
   * @returns {boolean}
   */
  roomExists(roomId) {
    return this.rooms.has(roomId);
  }

  /**
   * 检查用户是否在房间中
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @returns {boolean}
   */
  isUserInRoom(roomId, userId) {
    const room = this.rooms.get(roomId);
    return room ? room.members.has(userId) : false;
  }

  /**
   * 获取房间元数据
   * @param {string} roomId - 房间ID
   * @returns {Object|null}
   */
  getRoomMetadata(roomId) {
    const room = this.rooms.get(roomId);
    return room ? room.metadata : null;
  }

  /**
   * 更新房间元数据
   * @param {string} roomId - 房间ID
   * @param {Object} metadata - 元数据
   */
  updateRoomMetadata(roomId, metadata) {
    const room = this.rooms.get(roomId);
    if (room) {
      room.metadata = { ...room.metadata, ...metadata };
      return true;
    }
    return false;
  }

  /**
   * 获取所有房间
   * @returns {Array} 房间列表
   */
  getAllRooms() {
    return Array.from(this.rooms.values()).map(room => ({
      ...room.metadata,
      memberCount: room.members.size
    }));
  }

  /**
   * 用户从所有房间退出（通常在连接断开时调用）
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID
   */
  removeUserFromAllRooms(userId, connectionId) {
    const roomSet = this.userRooms.get(userId);
    if (!roomSet) {
      return;
    }

    const roomIds = Array.from(roomSet);
    roomIds.forEach(roomId => {
      this.leaveRoom(roomId, userId, connectionId);
    });

    console.log(`[RoomManager] 用户 ${userId} 已从所有房间退出`);
  }

  /**
   * 获取统计信息
   * @returns {Object}
   */
  getStats() {
    return {
      totalRooms: this.rooms.size,
      totalUsersInRooms: this.userRooms.size,
      rooms: this.getAllRooms()
    };
  }
}

export default RoomManager;
