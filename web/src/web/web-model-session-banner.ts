import type { WebConversationProjection } from '../shared/settings-bootstrap';
import type { WebModelActionKind } from './web-model-actions';

/**
 * 会话头部的 GPT Web 状态条（设计基线 §4.4、§9）。
 *
 * 纯函数：只把 daemon 的事实（会话绑定投影、槽位占用者、工具通道状态）映射成展示结论，
 * 不自造可用性。`blocksSend` 表示发送必然被 daemon 拒绝，界面先行禁用并说明原因。
 */
export interface WebSessionBanner {
  id: 'lost' | 'conflict' | 'unavailable' | 'busy' | 'unsynced' | 'noTools' | 'workspaceRequired';
  tone: 'warning' | 'info';
  textKey: string;
  params?: Record<string, string>;
  action?: { kind: WebModelActionKind; labelKey: string };
  blocksSend: boolean;
}

export interface WebSessionBannerInput {
  usesWeb: boolean;
  projection: WebConversationProjection | null | undefined;
  /** 当前占用槽位的会话；没有占用者为 null。 */
  slotOwnerSessionId: string | null;
  /** 占用者的会话名称（可能缺失）。 */
  slotOwnerTitle?: string | null;
  sessionId: string;
  /** 本会话有 turn 正在进行（此时不判失效）。 */
  turnActive: boolean;
  /** 占用信息已在本轮 turn 结束后刷新过；否则不下"已失效"的结论。 */
  runtimeFresh: boolean;
  /** 选择器入口投影的工具通道状态。 */
  toolsAvailable: boolean | null;
  toolsDetail?: string;
  /** GPT Web 的项目工具必须绑定到明确的工作区会话。 */
  workspaceRequired?: boolean;
}

export function resolveWebSessionBanner(input: WebSessionBannerInput): WebSessionBanner | null {
  if (!input.usesWeb) return null;
  const projection = input.projection;
  const owner = input.slotOwnerSessionId;
  const isOwner = owner === input.sessionId;

  if (projection?.mode === 'saved') {
    if (projection.syncState === 'conflict') {
      return {
        id: 'conflict',
        tone: 'warning',
        textKey: 'webModel.banner.conflict',
        action: { kind: 'openView', labelKey: 'webModel.action.openHome' },
        blocksSend: true,
      };
    }
    if (projection.syncState === 'stale' || projection.syncState === 'deleted') {
      return {
        id: 'unavailable',
        tone: 'warning',
        textKey: 'webModel.banner.savedUnavailable',
        action: { kind: 'switchModel', labelKey: 'webModel.action.switchModel' },
        blocksSend: true,
      };
    }
  } else if (
    projection?.syncState === 'active' && !isOwner && !input.turnActive && input.runtimeFresh
  ) {
    // 临时对话的上下文只存在于槽位的页面里：曾经有消息被网页接受（syncState = active），
    // 现在却没有持有槽位，就一定已经丢失（重启 / 页面重载 / 停止 / 退出 / 清除数据）。
    // 第一条消息就失败的会话网页里从没有过上下文（unbound），不属于失效，可以直接重试。
    return {
      id: 'lost',
      tone: 'warning',
      textKey: 'webModel.banner.lost',
      action: { kind: 'switchModel', labelKey: 'webModel.action.switchModel' },
      blocksSend: true,
    };
  }

  if (owner && !isOwner) {
    return {
      id: 'busy',
      tone: 'warning',
      textKey: 'webModel.banner.busy',
      params: { session: input.slotOwnerTitle?.trim() ? `「${input.slotOwnerTitle.trim()}」` : '' },
      action: { kind: 'releaseSession', labelKey: 'webModel.action.releaseAndUse' },
      blocksSend: true,
    };
  }

  if (projection?.mode === 'saved' && projection.hasRemoteConversation && !projection.remoteTitle) {
    return {
      id: 'unsynced',
      tone: 'info',
      textKey: 'webModel.banner.titleUnsynced',
      blocksSend: false,
    };
  }

  if (input.toolsAvailable === false) {
    return {
      id: 'noTools',
      tone: 'info',
      textKey: 'webModel.banner.noTools',
      params: { detail: input.toolsDetail ?? '' },
      action: { kind: 'openTunnelSettings', labelKey: 'webModel.action.configureTunnel' },
      blocksSend: false,
    };
  }
  if (input.workspaceRequired) {
    return {
      id: 'workspaceRequired',
      tone: 'info',
      textKey: 'webModel.banner.workspaceRequired',
      blocksSend: false,
    };
  }
  return null;
}
