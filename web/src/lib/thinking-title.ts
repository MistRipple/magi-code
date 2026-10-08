import type { ThinkingBlock } from '../types/message';

type Translate = (key: string) => string;

/**
 * 思考块的状态标题。原始风格的思考卡片和摘要风格的阶段标题共用同一个来源，
 * 同一段思考在两种模式下说的是同一句话。
 */
export function resolveThinkingTitle(
  group: Pick<ThinkingBlock, 'status' | 'isStreaming'> | undefined,
  translate: Translate,
): string {
  if (group?.status === 'failed') return translate('thinkingBlock.failedTitle');
  if (group?.status === 'blocked' || group?.status === 'cancelled') {
    return translate('thinkingBlock.interruptedTitle');
  }
  return group?.isStreaming
    ? translate('thinkingBlock.streamingTitle')
    : translate('thinkingBlock.completedTitle');
}
