/**
 * Blocking Manager - 阻塞管理器
 *
 * 职责：
 * - 管理阻塞项
 * - 检查阻塞状态
 * - 处理阻塞超时
 */

import { EventEmitter } from 'events';
import { Mission } from '../../mission';
import { logger, LogCategory } from '../../../logging';

/**
 * 阻塞项类型
 */
export type BlockedItemType = 'assignment' | 'todo';

/**
 * 阻塞原因
 */
export interface BlockingReason {
  /** 阻塞类型 */
  type: 'contract_pending' | 'dependency_incomplete' | 'resource_conflict' | 'approval_required';
  /** 依赖的契约 ID */
  contractId?: string;
  /** 依赖的 Todo ID */
  dependencyId?: string;
  /** 描述 */
  description: string;
}

/**
 * 阻塞项
 */
export interface BlockedItem {
  /** 唯一 ID */
  id: string;
  /** 阻塞项类型 */
  type: BlockedItemType;
  /** Mission ID */
  missionId: string;
  /** Assignment ID */
  assignmentId: string;
  /** Todo ID（如果是 Todo 级别阻塞） */
  todoId?: string;
  /** 阻塞原因 */
  reason: BlockingReason;
  /** 阻塞开始时间 */
  blockedAt: number;
  /** 解除时间 */
  unblockedAt?: number;
  /** 是否已解除 */
  resolved: boolean;
}

export interface BlockingOptions {
  /** 阻塞超时时间（毫秒），超时后跳过阻塞项 */
  timeout?: number;
  /** 阻塞检查间隔（毫秒） */
  checkInterval?: number;
}

export class BlockingManager extends EventEmitter {
  private blockedItems: Map<string, BlockedItem> = new Map();

  /**
   * 记录阻塞
   */
  recordBlocking(
    type: BlockedItemType,
    missionId: string,
    assignmentId: string,
    reason: BlockingReason,
    todoId?: string
  ): string {
    const blockingId = `${type}_${assignmentId}${todoId ? `_${todoId}` : ''}`;

    const blockedItem: BlockedItem = {
      id: blockingId,
      type,
      missionId,
      assignmentId,
      todoId,
      reason,
      blockedAt: Date.now(),
      resolved: false,
    };

    this.blockedItems.set(blockingId, blockedItem);

    logger.warn(
      LogCategory.ORCHESTRATOR,
      `记录阻塞: ${type} ${assignmentId}${todoId ? ` (Todo: ${todoId})` : ''} - ${reason.description}`
    );

    this.emit('blocked', blockedItem);

    return blockingId;
  }

  /**
   * 解除阻塞
   */
  resolveBlocking(blockingId: string): void {
    const blockedItem = this.blockedItems.get(blockingId);
    if (!blockedItem) {
      return;
    }

    blockedItem.resolved = true;
    blockedItem.unblockedAt = Date.now();

    logger.info(
      LogCategory.ORCHESTRATOR,
      `解除阻塞: ${blockedItem.type} ${blockedItem.assignmentId}`
    );

    this.emit('unblocked', blockedItem);
  }

  /**
   * 等待阻塞解除
   */
  async waitForUnblocking(
    blockingId: string,
    mission: Mission,
    options: BlockingOptions = {}
  ): Promise<boolean> {
    const blockedItem = this.blockedItems.get(blockingId);
    if (!blockedItem) {
      return true;
    }

    const timeout = options.timeout || 300000; // 默认 5 分钟
    const checkInterval = options.checkInterval || 5000; // 默认 5 秒
    const startTime = Date.now();

    while (Date.now() - startTime < timeout) {
      // 检查是否已解除
      if (blockedItem.resolved) {
        return true;
      }

      // 检查阻塞条件是否仍然存在
      if (!this.isStillBlocked(blockedItem, mission)) {
        this.resolveBlocking(blockingId);
        return true;
      }

      // 等待后重试
      await this.sleep(checkInterval);
    }

    // 超时
    logger.warn(
      LogCategory.ORCHESTRATOR,
      `阻塞超时: ${blockedItem.type} ${blockedItem.assignmentId}`
    );

    return false;
  }

  /**
   * 检查是否仍然阻塞
   */
  private isStillBlocked(blockedItem: BlockedItem, mission: Mission): boolean {
    const { reason } = blockedItem;

    switch (reason.type) {
      case 'contract_pending':
        return this.isContractPending(reason.contractId!, mission);

      case 'dependency_incomplete':
        return this.isDependencyIncomplete(reason.dependencyId!, mission);

      case 'resource_conflict':
        // 资源冲突需要外部解决
        return true;

      case 'approval_required':
        // 需要人工批准
        return true;

      default:
        return true;
    }
  }

  /**
   * 检查契约是否待完成
   */
  private isContractPending(contractId: string, mission: Mission): boolean {
    const assignment = mission.assignments.find(
      a => a.producerContracts.includes(contractId)
    );

    if (!assignment) {
      return false;
    }

    return !assignment.todos?.every(todo => todo.status === 'completed');
  }

  /**
   * 检查依赖是否未完成
   */
  private isDependencyIncomplete(dependencyId: string, mission: Mission): boolean {
    // 查找依赖的 Todo
    for (const assignment of mission.assignments) {
      if (assignment.todos) {
        const todo = assignment.todos.find(t => t.id === dependencyId);
        if (todo) {
          return todo.status !== 'completed';
        }
      }
    }

    return false;
  }

  /**
   * 获取所有阻塞项
   */
  getBlockedItems(): BlockedItem[] {
    return Array.from(this.blockedItems.values());
  }

  /**
   * 获取未解除的阻塞项
   */
  getUnresolvedItems(): BlockedItem[] {
    return Array.from(this.blockedItems.values()).filter(item => !item.resolved);
  }

  /**
   * 清除所有阻塞项
   */
  clear(): void {
    this.blockedItems.clear();
  }

  /**
   * 睡眠
   */
  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
