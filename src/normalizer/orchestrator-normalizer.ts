/**
 * Orchestrator Normalizer - 编排器消息标准化
 *
 * 将编排器的 UI 消息转换为标准消息格式
 * 实现编排器消息与模型消息的统一处理
 */

import { v4 as uuidv4 } from 'uuid';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageType,
  MessageCategory,
  ContentBlock,
  TextBlock,
} from '../protocol';
import { OrchestratorUIMessage, OrchestratorState } from '../orchestrator/protocols/types';
import { parseContentToBlocks } from '../utils/content-parser';

/**
 * 将 OrchestratorUIMessage 转换为 StandardMessage
 */
export function normalizeOrchestratorMessage(
  uiMessage: OrchestratorUIMessage,
  traceId?: string
): StandardMessage {
  const messageId = `msg-orch-${uuidv4().substring(0, 8)}`;

  // 构建内容块
  let blocks: ContentBlock[] = [];

  // 使用 parseContentToBlocks 处理内容（会自动移除裸露的 JSON）
  if (uiMessage.content) {
    blocks = parseContentToBlocks(uiMessage.content, { source: 'orchestrator' });

    // 如果解析后是文本块，根据消息类型设置 isMarkdown
    if (blocks.length > 0 && blocks[0].type === 'text') {
      const textBlock = blocks[0] as TextBlock;
      if (uiMessage.type === 'plan_ready' || uiMessage.type === 'summary') {
        textBlock.isMarkdown = true;
      }
    }
  }

  // 如果有计划信息，且与主内容不同，添加为额外块
  if (uiMessage.metadata?.formattedPlan && uiMessage.metadata.formattedPlan !== uiMessage.content) {
    const planBlocks = parseContentToBlocks(uiMessage.metadata.formattedPlan, { source: 'orchestrator' });
    // 设置为 Markdown
    planBlocks.forEach(block => {
      if (block.type === 'text') {
        (block as TextBlock).isMarkdown = true;
      }
    });
    blocks.push(...planBlocks);
  }

  // 🔧 清理非法块，避免下游 keyed each 崩溃
  const safeBlocks = blocks.filter(
    (block) => !!block && typeof block === 'object' && typeof (block as ContentBlock).type === 'string'
  );

  // 确定消息类型
  let messageType: MessageType = MessageType.TEXT;
  if (uiMessage.type === 'error') {
    messageType = MessageType.ERROR;
  } else if (uiMessage.type === 'progress_update') {
    messageType = MessageType.PROGRESS;
  } else if (uiMessage.type === 'plan_ready') {
    messageType = MessageType.PLAN;
  } else if (uiMessage.type === 'summary') {
    messageType = MessageType.RESULT;
  }

  // 确定生命周期状态
  let lifecycle: MessageLifecycle = MessageLifecycle.COMPLETED;
  if (uiMessage.type === 'progress_update') {
    lifecycle = MessageLifecycle.STREAMING;
  } else if (uiMessage.type === 'error') {
    lifecycle = MessageLifecycle.FAILED;
  }

  return {
    id: messageId,
    traceId: traceId || `trace-${uuidv4().substring(0, 8)}`,
    category: MessageCategory.CONTENT,  // 🔧 统一消息通道：编排器输出为 CONTENT 类别
    type: messageType,
    agent: 'orchestrator',
    source: 'orchestrator',
    lifecycle,
    timestamp: uiMessage.timestamp || Date.now(),
    updatedAt: Date.now(),
    blocks: safeBlocks,
    metadata: {
      taskId: uiMessage.taskId,
      phase: uiMessage.metadata?.phase,
      subTaskId: uiMessage.metadata?.subTaskId,
    },
  };
}

/**
 * 创建编排器进度更新
 */
export function createOrchestratorUpdate(
  messageId: string,
  content: string,
  _phase?: OrchestratorState,
  _progress?: number
): StreamUpdate {
  return {
    messageId,
    timestamp: Date.now(),
    updateType: 'append',
    appendText: content,
  };
}

/**
 * 判断是否为编排器状态消息（不需要显示给用户）
 */
export function isInternalStateMessage(uiMessage: OrchestratorUIMessage): boolean {
  const internalPatterns = [
    /^正在分析任务依赖关系/,
    /^执行模式已调整/,
    /^已移除冗余/,
    /^编排纪律提示/,
  ];

  return internalPatterns.some(pattern => pattern.test(uiMessage.content));
}

/**
 * 获取消息优先级（用于批处理排序）
 */
export function getMessagePriority(uiMessage: OrchestratorUIMessage): number {
  const priorities: Record<string, number> = {
    error: 100,
    summary: 90,
    plan_ready: 80,
    direct_response: 70,
    confirmationRequest: 60,
    progress_update: 50,
  };

  return priorities[uiMessage.type] || 0;
}
