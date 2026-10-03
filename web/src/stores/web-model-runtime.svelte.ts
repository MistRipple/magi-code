/**
 * GPT Web 单槽位的运行态投影。
 *
 * 这里只保存 daemon 投影过来的**可重建**事实：当前由哪个会话占用槽位、是否在推理、
 * 账号消息条数。它不缓存任何对话正文，也不替代 canonical；进程重启或投影失败
 * 时回落到「无占用」。
 */
import type {
  WebModelQuota,
  WebModelRuntimeEntry,
  WebModelRuntimeResponse,
} from '../web/agent-api';

interface WebModelRuntimeState {
  entries: WebModelRuntimeEntry[];
  quota: WebModelQuota | null;
  loadedAt: number;
}

let runtimeState = $state<WebModelRuntimeState>({
  entries: [],
  quota: null,
  loadedAt: 0,
});

/** daemon 投影到达时整体替换：不做增量合并，避免留下幽灵条目。 */
export function applyWebModelRuntime(snapshot: WebModelRuntimeResponse | null): void {
  if (!snapshot) {
    runtimeState.entries = [];
    runtimeState.quota = null;
    return;
  }
  runtimeState.entries = snapshot.entries;
  runtimeState.quota = snapshot.quota;
  runtimeState.loadedAt = Date.now();
}

export function clearWebModelRuntime(): void {
  applyWebModelRuntime(null);
}

export function webModelRuntimeState(): WebModelRuntimeState {
  return runtimeState;
}

/** 当前占用 GPT Web 槽位的会话（没有占用时为 `null`）。 */
export function webModelSlotOwner(): WebModelRuntimeEntry | null {
  return runtimeState.entries[0] ?? null;
}

/** 某个会话在飞 turn 的阶段；不是该会话占用或没有在飞 turn 时返回 `null`。 */
export function webModelTurnStage(sessionId: string): { stage: WebModelRuntimeEntry['stage'] } | null {
  const entry = runtimeState.entries.find(
    (item) => item.sessionId === sessionId.trim() && item.active !== false,
  );
  return entry ? { stage: entry.stage } : null;
}

/** 当前有在飞 turn 的数量（单槽位，只会是 0 或 1）。 */
export function webModelActiveTurnCount(): number {
  return runtimeState.entries.filter((entry) => entry.active !== false).length;
}

/** 该会话已发出的账号消息条数。 */
export function webModelSentMessages(sessionId: string): number {
  return runtimeState.entries
    .filter((entry) => entry.sessionId === sessionId.trim())
    .reduce((total, entry) => total + (Number.isFinite(entry.sentMessages) ? entry.sentMessages : 0), 0);
}

/** 最近一次探测的状态（daemon 探测结论的前端镜像，只用于提示条；不据此推断可用性）。 */
let probeStatus = $state<{ status: string; reason: string }>({ status: 'unknown', reason: '' });

export function applyWebModelProbeStatus(status: string, reason?: string | null): void {
  probeStatus.status = status;
  probeStatus.reason = reason ?? '';
}

export function webModelProbeStatus(): string {
  return probeStatus.status;
}

/**
 * 用户显式「停止 / 退出」后，GPT Web 页面应保持销毁，直到用户再次打开视图。
 * 启动 / 重启时没有这个标记，宿主会静默物化以恢复持久登录态。
 */
let stoppedByUser = $state(false);

export function markWebModelStoppedByUser(stopped: boolean): void {
  stoppedByUser = stopped;
}

export function webModelStoppedByUser(): boolean {
  return stoppedByUser;
}
