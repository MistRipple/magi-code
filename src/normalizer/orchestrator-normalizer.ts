/**
 * Orchestrator Normalizer - 编排器消息标准化
 *
 * 将 OrchestratorAgent 的 emitUIMessage 转换为标准消息格式
 * 实现编排器消息与 CLI 消息的统一处理
 */

import { v4 as uuidv4 } from 'uuid';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageType,
  ContentBlock,
  TextBlock,
} from '../protocol';
import { OrchestratorUIMessage, OrchestratorState } from '../orchestrator/protocols/types';
import { parseContentToBlocks } from '../utils/content-parser';

/** 编排器消息类型到内容块类型的映射 */
const MESSAGE_TYPE_MAP: Record<string, ContentBlock['type']> = {
  progress_update: 'text',
  plan_ready: 'text',
  summary: 'text',
  direct_response: 'text',
  confirmationRequest: 'text',
  error: 'text',
};

/**
 * 将 OrchestratorUIMessage 转换为 StandardMessage
 */
export function normalizeOrchestratorMessage(
  uiMessage: OrchestratorUIMessage,
  traceId?: string
): StandardMessage {
  const messageId = `msg-orch-${uuidv4().substring(0, 8)}`;

  // 🔍 检查点 2.1：记录进入 normalizer
  console.log('[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口:', {
    messageId,
    type: uiMessage.type,
    contentLength: uiMessage.content?.length || 0,
    contentPreview: uiMessage.content?.substring(0, 100),
  });

  // 构建内容块
  let blocks: ContentBlock[] = [];

  // 🔧 移除 summaryCard 特殊处理 - 总结内容应该作为普通消息显示
  // const summaryCard = uiMessage.type === 'summary' ? parseSummaryCard(uiMessage.content) : null;

  // 🔧 使用 parseContentToBlocks 处理内容（会自动移除裸露的 JSON）
  if (uiMessage.content) {
    blocks = parseContentToBlocks(uiMessage.content, { source: 'orchestrator' });

    // 🔍 检查点 2.2：记录解析后的 blocks
    console.log('[DEBUG-LAYER-2] parseContentToBlocks 返回:', {
      messageId,
      blocksCount: blocks.length,
      blockTypes: blocks.map(b => b.type),
      firstBlockPreview: blocks[0] ? {
        type: blocks[0].type,
        contentLength: (blocks[0].type === 'text' || blocks[0].type === 'code') ? (blocks[0] as any).content?.length || 0 : 0,
        contentPreview: (blocks[0].type === 'text' || blocks[0].type === 'code') ? (blocks[0] as any).content?.substring(0, 100) : '',
      } : null,
    });

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
    type: messageType,
    cli: 'claude', // 编排器使用 claude
    source: 'orchestrator',
    lifecycle,
    timestamp: uiMessage.timestamp || Date.now(),
    updatedAt: Date.now(),
    blocks,
    metadata: {
      taskId: uiMessage.taskId,
      phase: uiMessage.metadata?.phase,
      subTaskId: uiMessage.metadata?.subTaskId,
      // 🔧 移除 summaryCard - 总结内容作为普通消息显示
    },
  };
}

function parseSummaryCard(content: string): { title: string; sections: Array<{ title: string; items: string[] }> } | null {
  if (!content) return null;
  const lines = content.split('\n').map(line => line.trim()).filter(Boolean);
  if (lines.length === 0) return null;

  let title = '';
  const sections: Array<{ title: string; items: string[] }> = [];
  let current: { title: string; items: string[] } | null = null;

  const pushCurrent = () => {
    if (current && current.items.length > 0) {
      sections.push(current);
    }
    current = null;
  };

  for (const line of lines) {
    if (line.startsWith('## ')) {
      pushCurrent();
      current = { title: line.replace(/^##\s+/, '').trim(), items: [] };
      continue;
    }

    if (/:$/.test(line) || /：$/.test(line)) {
      pushCurrent();
      current = { title: line.replace(/[:：]\s*$/, '').trim(), items: [] };
      continue;
    }

    if (!title) {
      title = line;
      continue;
    }

    if (!current) {
      current = { title: '内容', items: [] };
    }

    const cleaned = line.replace(/^[-*•]\s+/, '').replace(/^\d+\.\s+/, '');
    if (cleaned) {
      current.items.push(cleaned);
    }
  }

  pushCurrent();

  if (!title) {
    title = '执行总结';
  }

  if (sections.length === 0) {
    return null;
  }

  return { title, sections };
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
