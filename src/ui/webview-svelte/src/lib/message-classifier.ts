import type { StandardMessage } from '../../../../protocol/message-protocol';
import { MessageType } from '../../../../protocol/message-protocol';
import { MessageCategory } from '../types/message-routing';
import { InteractionType } from '../../../../protocol/message-protocol';

const WORKER_SLOTS = new Set(['claude', 'codex', 'gemini']);

export function normalizeWorkerSlot(value: unknown): 'claude' | 'codex' | 'gemini' | null {
  if (!value || typeof value !== 'string') return null;
  const lower = value.toLowerCase().trim();
  if (WORKER_SLOTS.has(lower)) return lower as 'claude' | 'codex' | 'gemini';
  return null;
}

/**
 * 消息分类器 - 方案 B
 *
 * 完全基于 MessageType 进行分类，不依赖 metadata 魔术字段
 * 分类优先级：
 * 1. 明确的消息类型（USER_INPUT, TASK_CARD, INSTRUCTION）
 * 2. 系统状态消息
 * 3. 基于 source + MessageType 的组合分类
 */
export function classifyMessage(standard: StandardMessage): {
  category: MessageCategory;
  worker?: 'claude' | 'codex' | 'gemini';
} {
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const resolvedWorker = normalizeWorkerSlot(standard.agent) ?? normalizeWorkerSlot(meta?.worker);
  const isStatusMessage = Boolean(meta?.isStatusMessage);

  // ============== 第一优先级：明确的消息类型 ==============
  // 直接通过 MessageType 识别

  // 用户输入消息
  if (standard.type === MessageType.USER_INPUT) {
    return { category: MessageCategory.USER_INPUT };
  }

  // 任务状态卡片
  if (standard.type === MessageType.TASK_CARD) {
    return { category: MessageCategory.TASK_SUMMARY_CARD, worker: resolvedWorker ?? undefined };
  }

  // 任务说明（编排者→Worker）
  if (standard.type === MessageType.INSTRUCTION) {
    return { category: MessageCategory.WORKER_INSTRUCTION, worker: resolvedWorker ?? undefined };
  }

  // ============== 第二优先级：系统状态消息 ==============
  if (isStatusMessage && standard.type === MessageType.PROGRESS) {
    return { category: MessageCategory.SYSTEM_PHASE };
  }

  // ============== 第三优先级：基于 source 的分类 ==============

  if (standard.source === 'orchestrator') {
    // 交互消息
    if (standard.type === MessageType.INTERACTION && standard.interaction) {
      switch (standard.interaction.type) {
        case InteractionType.PLAN_CONFIRMATION:
          return { category: MessageCategory.INTERACTION_CONFIRMATION };
        case InteractionType.PERMISSION:
          return { category: MessageCategory.INTERACTION_TOOL_AUTH };
        case InteractionType.QUESTION:
        case InteractionType.CLARIFICATION:
          return { category: MessageCategory.INTERACTION_QUESTION };
        default:
          return { category: MessageCategory.SYSTEM_NOTICE };
      }
    }

    // 编排者消息类型映射
    switch (standard.type) {
      case MessageType.PLAN:
        return { category: MessageCategory.ORCHESTRATOR_PLAN };
      case MessageType.THINKING:
        return { category: MessageCategory.ORCHESTRATOR_THINKING };
      case MessageType.TOOL_CALL:
        return { category: MessageCategory.ORCHESTRATOR_TOOL_USE };
      case MessageType.RESULT:
        return { category: MessageCategory.ORCHESTRATOR_SUMMARY };
      case MessageType.PROGRESS:
        return { category: MessageCategory.PROGRESS_UPDATE };
      case MessageType.ERROR:
        return { category: MessageCategory.SYSTEM_ERROR };
      case MessageType.SYSTEM:
        return { category: MessageCategory.SYSTEM_NOTICE };
      case MessageType.TEXT:
      default:
        return { category: MessageCategory.ORCHESTRATOR_RESPONSE };
    }
  }

  if (standard.source === 'worker') {
    // 检测是否为代码输出：包含 CodeBlock 且是文本类型
    const hasCodeBlock = standard.blocks?.some(b => b.type === 'code');

    // Worker 消息类型映射
    switch (standard.type) {
      case MessageType.THINKING:
        return { category: MessageCategory.WORKER_THINKING, worker: resolvedWorker ?? undefined };
      case MessageType.TOOL_CALL:
        return { category: MessageCategory.WORKER_TOOL_USE, worker: resolvedWorker ?? undefined };
      case MessageType.ERROR:
        // 🔧 Worker 错误应该显示在主对话区，让用户注意到
        return { category: MessageCategory.SYSTEM_ERROR, worker: resolvedWorker ?? undefined };
      case MessageType.RESULT:
        return { category: MessageCategory.WORKER_SUMMARY, worker: resolvedWorker ?? undefined };
      case MessageType.INTERACTION:
        // 🔧 Worker 交互消息：根据交互类型分类（而不是归并为普通文本）
        if (standard.interaction) {
          switch (standard.interaction.type) {
            case InteractionType.PERMISSION:
              return { category: MessageCategory.INTERACTION_TOOL_AUTH, worker: resolvedWorker ?? undefined };
            case InteractionType.QUESTION:
            case InteractionType.CLARIFICATION:
              return { category: MessageCategory.INTERACTION_QUESTION, worker: resolvedWorker ?? undefined };
            default:
              return { category: MessageCategory.INTERACTION_QUESTION, worker: resolvedWorker ?? undefined };
          }
        }
        return { category: MessageCategory.WORKER_OUTPUT, worker: resolvedWorker ?? undefined };
      case MessageType.PLAN:
        // 🔧 Worker 计划消息：显示在 Worker Tab
        return { category: MessageCategory.WORKER_OUTPUT, worker: resolvedWorker ?? undefined };
      case MessageType.SYSTEM:
        // 🔧 Worker 系统消息
        return { category: MessageCategory.SYSTEM_NOTICE, worker: resolvedWorker ?? undefined };
      case MessageType.TEXT:
        // 代码输出：包含 CodeBlock 的文本消息
        if (hasCodeBlock) {
          return { category: MessageCategory.WORKER_CODE, worker: resolvedWorker ?? undefined };
        }
        return { category: MessageCategory.WORKER_OUTPUT, worker: resolvedWorker ?? undefined };
      case MessageType.PROGRESS:
        return { category: MessageCategory.PROGRESS_UPDATE, worker: resolvedWorker ?? undefined };
      default:
        return { category: MessageCategory.WORKER_OUTPUT, worker: resolvedWorker ?? undefined };
    }
  }

  return { category: MessageCategory.SYSTEM_NOTICE };
}
