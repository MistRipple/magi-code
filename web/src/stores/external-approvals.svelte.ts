/**
 * 外部调用（外部 MCP 客户端、GPT Web 槽位）的待确认。事实在 daemon，这里只缓存读取结果：
 * 全局托盘和外部会话视图读同一份，批准或拒绝走同一个入口。
 */
import { i18n } from './i18n.svelte';
import { addToast } from './messages.svelte';
import {
  listMcpServerApprovals,
  resolveMcpServerApproval,
  setWebModelToolPolicy,
  type McpServerApproval,
} from '../web/agent-api';
import { MCP_APPROVALS_CHANGED_EVENT } from '../lib/mcp-server-events';
import { OPEN_EXTERNAL_SESSION_EVENT, externalSessions } from './external-sessions.svelte';

const POLL_WHILE_PENDING_MS = 3000;

export const externalApprovals = $state({
  approvals: [] as McpServerApproval[],
  busy: false,
  /** 倒计时用的当前时间（毫秒）。 */
  now: Date.now(),
});

/**
 * 没有管理权限（经公网隧道访问，或 MCP 服务 / GPT Web 插件尚未激活）：请求失败后停用，不反复轮询；
 * daemon 推来新的待确认事件时再试一次（插件可能刚被激活）。
 */
let disabled = false;

/** 已经提醒过（或首次读取时就已存在）的待确认；只对之后新出现的发系统通知。 */
const knownApprovalIds = new Set<string>();
let loadedOnce = false;

/**
 * 外部调用等待确认而用户不在 Magi 里（窗口失焦、隐藏到托盘）时发一条系统通知：待确认只有 90 秒，
 * 错过就超时。点击通知把窗口带到前台并打开对应的外部会话（macOS 上点击通知会激活应用并显示窗口）。
 */
function notifyNewApprovals(approvals: McpServerApproval[]): void {
  const fresh = approvals.filter((approval) => !knownApprovalIds.has(approval.approvalId));
  for (const approval of approvals) knownApprovalIds.add(approval.approvalId);
  if (!loadedOnce) {
    loadedOnce = true;
    return;
  }
  if (typeof window === 'undefined' || typeof Notification === 'undefined') return;
  if (Notification.permission !== 'granted' || document.hasFocus()) return;
  for (const approval of fresh) {
    const notification = new Notification(i18n.t('mcpServer.tray.title'), {
      body: `${approval.clientName}: ${approval.summary}`,
      tag: approval.approvalId,
    });
    notification.onclick = () => {
      window.focus();
      notification.close();
      const session = externalSessions.sessions.find((item) => item.tokenId === approval.tokenId);
      if (session) window.dispatchEvent(new CustomEvent(OPEN_EXTERNAL_SESSION_EVENT, { detail: { session } }));
    };
  }
}

export async function refreshExternalApprovals(): Promise<void> {
  if (disabled) return;
  try {
    const approvals = await listMcpServerApprovals();
    notifyNewApprovals(approvals);
    externalApprovals.approvals = approvals;
  } catch {
    disabled = true;
    externalApprovals.approvals = [];
    // 首次读取失败（例如 MCP 插件尚未激活）也算已建立基线：之后新出现的待确认都要提醒。
    loadedOnce = true;
  }
}

function reportFailure(error: unknown): void {
  addToast('error', i18n.t('mcpServer.error.action', {
    message: error instanceof Error ? error.message : String(error),
  }));
}

export async function resolveExternalApproval(
  approval: McpServerApproval,
  decision: 'allow_once' | 'deny',
): Promise<void> {
  externalApprovals.busy = true;
  try {
    externalApprovals.approvals = await resolveMcpServerApproval(approval.approvalId, decision);
  } catch (error) {
    reportFailure(error);
    await refreshExternalApprovals();
  } finally {
    externalApprovals.busy = false;
  }
}

/** 「始终允许」：把 GPT Web 的授权方式切到始终授权，并放行这一次。随时可在设置里改回。 */
export async function alwaysAllowExternalApproval(approval: McpServerApproval): Promise<void> {
  externalApprovals.busy = true;
  try {
    await setWebModelToolPolicy({ approvalMode: 'always' });
    externalApprovals.approvals = await resolveMcpServerApproval(approval.approvalId, 'allow_once');
    addToast('info', i18n.t('mcpServer.approvals.alwaysSet'));
  } catch (error) {
    reportFailure(error);
    await refreshExternalApprovals();
  } finally {
    externalApprovals.busy = false;
  }
}

/** 开始跟踪待确认：收到事件时拉取；只有存在待确认项时才低频轮询。返回停止函数。 */
export function trackExternalApprovals(): () => void {
  void refreshExternalApprovals();
  const onChanged = () => {
    disabled = false;
    void refreshExternalApprovals();
  };
  window.addEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
  const clock = window.setInterval(() => { externalApprovals.now = Date.now(); }, 1000);
  const poll = window.setInterval(() => {
    if (externalApprovals.approvals.length > 0) void refreshExternalApprovals();
  }, POLL_WHILE_PENDING_MS);
  return () => {
    window.removeEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
    window.clearInterval(clock);
    window.clearInterval(poll);
  };
}
