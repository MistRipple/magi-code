import type { CanonicalTurn } from '../shared/protocol/canonical-turn';

/**
 * 输入框的纯规则：附件上限、会话命令与附件的互斥、拖入文件的筛选、排队消息的摘要。
 *
 * 这些规则只描述「界面能不能这样组合」，业务结论仍由 daemon 校验；放在这里是为了让
 * 输入框、斜杠菜单和排队卡片对同一件事给出同一个答案，也方便脱离组件做回归。
 */

export const MAX_COMPOSER_IMAGES = 5;
export const MAX_COMPOSER_IMAGE_BYTES = 10 * 1024 * 1024;

export interface ComposerAttachmentCounts {
  images: number;
  contextReferences: number;
  browserAnnotations: number;
  browserNodeSelections: number;
}

export function composerAttachmentTotal(counts: ComposerAttachmentCounts): number {
  return counts.images
    + counts.contextReferences
    + counts.browserAnnotations
    + counts.browserNodeSelections;
}

/** `/compact` 当前不可用的原因；null 表示可用。 */
export type SessionCommandDisabledReason = 'draft-session' | 'has-attachments';

/**
 * 会话命令（`/compact`）是 daemon 执行的独立轮次：新建草稿会话没有可压缩的历史，
 * 并且 daemon 拒绝它与图片、上下文引用、浏览器标注同时提交。
 */
export function sessionCommandDisabledReason(input: {
  sessionCommandsAvailable: boolean;
  attachmentTotal: number;
}): SessionCommandDisabledReason | null {
  if (!input.sessionCommandsAvailable) return 'draft-session';
  if (input.attachmentTotal > 0) return 'has-attachments';
  return null;
}

export interface DroppedFileLike {
  name: string;
  type: string;
  size: number;
}

export interface DroppedImageSelection {
  /** 可以加入输入框的文件下标。 */
  accepted: number[];
  /** 因超过张数上限被丢弃的文件数。 */
  overLimit: number;
  /** 因体积过大被丢弃的文件。 */
  tooLarge: Array<{ name: string; size: number }>;
  /** 不是图片、被忽略的文件数。 */
  ignored: number;
}

/** 从拖入 / 粘贴的文件里挑出能作为图片附件的部分，并说明其余的去向。 */
export function selectDroppedImages(
  files: DroppedFileLike[],
  currentCount: number,
): DroppedImageSelection {
  const result: DroppedImageSelection = { accepted: [], overLimit: 0, tooLarge: [], ignored: 0 };
  let count = currentCount;
  files.forEach((file, index) => {
    if (!file.type.toLowerCase().startsWith('image/')) {
      result.ignored += 1;
      return;
    }
    if (file.size > MAX_COMPOSER_IMAGE_BYTES) {
      result.tooLarge.push({ name: file.name, size: file.size });
      return;
    }
    if (count >= MAX_COMPOSER_IMAGES) {
      result.overLimit += 1;
      return;
    }
    result.accepted.push(index);
    count += 1;
  });
  return result;
}

export function formatImageSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '';
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export interface QueuedMessageShape {
  interruptTurnId?: string | null;
  images?: unknown[];
  contextReferences?: unknown[];
  browserAnnotationRefs?: string[];
  browserNodeSelections?: unknown[];
}

export interface QueuedAttachmentSummary {
  images: number;
  references: number;
  annotations: number;
}

/** 排队卡片上需要提示的非纯文字部分，免得用户以为排队的只是一句话。 */
export function summarizeQueuedMessage(queued: QueuedMessageShape): QueuedAttachmentSummary {
  return {
    images: queued.images?.length ?? 0,
    references: (queued.contextReferences?.length ?? 0) + (queued.browserNodeSelections?.length ?? 0),
    annotations: queued.browserAnnotationRefs?.length ?? 0,
  };
}

/**
 * 编辑保留 daemon 原输入，保存时按 revision 更新。浏览器标注在队列里只剩 ID，
 * 无法还原为编辑对象；替代输入也不能编辑以免改变已接纳的停止意图。
 */
export function queuedMessageEditable(queued: QueuedMessageShape): boolean {
  return !queued.interruptTurnId && (queued.browserAnnotationRefs?.length ?? 0) === 0;
}

/** 只投影当前主线的恢复资格；子代理与正常完成的历史不能激活输入框继续。 */
export function composerCanContinue(input: {
  sessionId: string;
  turns: readonly CanonicalTurn[];
  runtimeCanResume: boolean;
}): boolean {
  if (!input.sessionId) return false;
  const latest = input.turns
    .filter(turn => turn.sessionId === input.sessionId && !turn.metadata?.agentExecution
      && !turn.items.some(item => item.metadata?.sessionCommand))
    .reduce<CanonicalTurn | null>((last, turn) => {
      if (!last) return turn;
      if (turn.turnSeq !== last.turnSeq) return turn.turnSeq > last.turnSeq ? turn : last;
      return turn.turnId.localeCompare(last.turnId) > 0 ? turn : last;
    }, null);
  if (!latest || !['cancelled', 'failed', 'interrupted'].includes(latest.status)) return false;
  if (input.runtimeCanResume) return true;
  if (latest.items.some(item => item.metadata?.noticeKind === 'session_interrupted'
    && item.metadata?.recoveryState === 'ready')) return true;
  const user = latest.items.find(item => item.kind === 'user_message');
  return Boolean(user?.content?.trim() && user.worker?.taskId
    && (latest.status !== 'cancelled' || user.metadata?.interruptionSource === 'user'));
}

/** 编辑入口只能属于当前主线最新轮次；隐藏的 Continue 控制轮次也必须参与比较。 */
export function isLatestMainlineTurn(input: {
  sessionId: string;
  turnId: string;
  turns: readonly CanonicalTurn[];
}): boolean {
  const sessionId = input.sessionId.trim();
  const turnId = input.turnId.trim();
  if (!sessionId || !turnId) return false;
  const latest = input.turns
    .filter(turn => turn.sessionId === sessionId && !turn.metadata?.agentExecution
      && !turn.items.some(item => item.metadata?.sessionCommand))
    .reduce<CanonicalTurn | null>((last, turn) => {
      if (!last) return turn;
      if (turn.turnSeq !== last.turnSeq) return turn.turnSeq > last.turnSeq ? turn : last;
      return turn.turnId.localeCompare(last.turnId) > 0 ? turn : last;
    }, null);
  return latest?.turnId === turnId;
}
