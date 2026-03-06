/**
 * SupplementaryInstructionQueue - 补充指令队列
 *
 * 从 MissionDrivenEngine 提取的独立状态机（P1-4 修复）。
 * 职责：管理执行中用户发送的补充指令的暂存、消费和清理。
 *
 * 设计要点：
 * - 补充指令不中断当前任务，在下一决策点生效
 * - 基于游标的多 Worker 消费模型，每个 Worker 独立消费进度
 * - 当所有 Worker 都消费过的指令自动清理（prune）
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { WorkerSlot } from '../../types';
import type { Mission } from '../mission';
import type { OrchestratorResponse } from '../protocols/worker-report';
import { createAdjustResponse } from '../protocols/worker-report';

/**
 * 补充指令条目
 */
interface SupplementaryInstruction {
  id: string;
  index: number;
  content: string;
  timestamp: number;
  source: 'user';
  targetWorker?: WorkerSlot;
}

/**
 * SupplementaryInstructionQueue - 补充指令队列管理
 *
 * 独立的状态机，管理执行中用户发送的追加消息。
 * 通过 EventEmitter 向外通知指令接收事件。
 */
export class SupplementaryInstructionQueue {
  private instructions: SupplementaryInstruction[] = [];
  private instructionIndex = 0;
  private cursors: Map<string, number> = new Map();

  constructor(private emitter: EventEmitter) {}

  /**
   * 注入补充指令（执行中用户发送的追加消息）
   *
   * 规范要求：
   * - 补充指令不中断当前任务
   * - 编排者接收并暂存
   * - 在下一决策点（工具调用前/步骤边界/思考完成/等待确认）生效
   *
   * @param content 用户输入的补充内容
   * @param isRunning 引擎是否正在执行
   * @returns 是否成功注入
   */
  inject(content: string, isRunning: boolean, targetWorker?: WorkerSlot): boolean {
    if (!isRunning) {
      logger.warn('引擎.补充指令.拒绝', { reason: '没有正在执行的任务' }, LogCategory.ORCHESTRATOR);
      return false;
    }

    const instruction: SupplementaryInstruction = {
      id: `supp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      index: ++this.instructionIndex,
      content: content.trim(),
      timestamp: Date.now(),
      source: 'user',
      targetWorker,
    };

    this.instructions.push(instruction);

    logger.info('引擎.补充指令.已暂存', {
      id: instruction.id,
      preview: content.substring(0, 50),
      queueSize: this.instructions.length,
      targetWorker: targetWorker || 'broadcast',
    }, LogCategory.ORCHESTRATOR);

    // 触发事件通知 UI 指令已接收
    this.emitter.emit('supplementaryInstructionReceived', {
      id: instruction.id,
      count: this.instructions.length,
      targetWorker,
    });

    return true;
  }

  /**
   * 获取并消费待处理的补充指令
   * 在决策点调用此方法获取上下文
   *
   * @param workerId 可选，指定 Worker 的消费游标
   * @returns 待处理的补充指令内容数组
   */
  consume(workerId?: WorkerSlot): string[] {
    if (this.instructions.length === 0) {
      return [];
    }

    // 未提供 workerId 时，消费全部待处理指令
    if (!workerId) {
      const contents = this.instructions.map(i => i.content);
      const count = contents.length;
      this.instructions = [];
      this.cursors.clear();
      logger.info('引擎.补充指令.已消费', { count }, LogCategory.ORCHESTRATOR);
      return contents;
    }

    const lastIndex = this.cursors.get(workerId) || 0;
    const pending = this.instructions.filter(i =>
      i.index > lastIndex && (!i.targetWorker || i.targetWorker === workerId)
    );
    if (pending.length === 0) {
      return [];
    }

    const latestIndex = pending[pending.length - 1].index;
    this.cursors.set(workerId, latestIndex);
    this.prune();

    logger.info('引擎.补充指令.已消费', {
      workerId,
      count: pending.length,
      latestIndex,
    }, LogCategory.ORCHESTRATOR);

    return pending.map(i => i.content);
  }

  /**
   * 查看当前待处理的补充指令数量（不消费）
   */
  getPendingCount(): number {
    return this.instructions.length;
  }

  /**
   * 重置补充指令状态（开始新任务前）
   */
  reset(): void {
    this.instructions = [];
    this.instructionIndex = 0;
    this.cursors.clear();
  }

  /**
   * 在决策点构建补充指令调整响应
   */
  buildAdjustment(workerId: WorkerSlot): OrchestratorResponse | null {
    const instructions = this.consume(workerId);
    if (instructions.length === 0) {
      return null;
    }

    const formatted = instructions.map(i => `- ${i}`).join('\n');
    return createAdjustResponse({
      newInstructions: `[System] 用户补充指令：\n${formatted}`,
    });
  }

  /**
   * 将待处理补充指令应用到 Mission（用于等待确认后的统一注入）
   */
  applyToMission(mission: Mission): void {
    const instructions = this.consume();
    if (instructions.length === 0) {
      return;
    }
    const formatted = instructions.map(i => `- ${i}`).join('\n');
    const content = `[System] 用户补充指令：\n${formatted}`;
    for (const assignment of mission.assignments) {
      assignment.guidancePrompt = assignment.guidancePrompt
        ? `${assignment.guidancePrompt}\n\n${content}`
        : content;
    }
  }

  // ============================================================================
  // 内部方法
  // ============================================================================

  /**
   * 清理已被所有已知 Worker 消费的补充指令
   */
  private prune(): void {
    if (this.instructions.length === 0) return;
    if (this.cursors.size === 0) return;

    const minBroadcastCursor = Math.min(...this.cursors.values());
    if (!Number.isFinite(minBroadcastCursor)) return;

    this.instructions = this.instructions.filter(instruction => {
      if (instruction.targetWorker) {
        const cursor = this.cursors.get(instruction.targetWorker) || 0;
        return cursor < instruction.index;
      }
      return minBroadcastCursor < instruction.index;
    });
  }
}
