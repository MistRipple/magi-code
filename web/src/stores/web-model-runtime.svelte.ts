/**
 * Web 模型运行态投影（《实现计划》§3.12、§5.1）。
 *
 * 这里只保存 daemon 投影过来的**可重建**事实：在飞 turn 的阶段 / 排队位置与
 * 账号消息条数。它不缓存任何对话正文，也不替代 canonical；进程重启或投影失败
 * 时回落到「无阶段」，界面按既有的通用运行指示显示。
 *
 * 设计依据：设计基线 A16、§5.10、§5.13。
 */
import type {
  WebModelQuota,
  WebModelRuntimeEntry,
  WebModelRuntimeResponse,
} from '../web/agent-api';

/** 顶层编排线程 id，与 daemon 的 `DEFAULT_WEB_MODEL_THREAD_ID` 一致。 */
export const WEB_MODEL_ORCHESTRATOR_THREAD_ID = 'orchestrator';

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

export function webModelRuntimeEntries(sessionId = ''): WebModelRuntimeEntry[] {
  const scoped = sessionId.trim()
    ? runtimeState.entries.filter((entry) => entry.sessionId === sessionId.trim())
    : runtimeState.entries;
  return scoped;
}

/**
 * 某条线程当前的在飞阶段。
 *
 * 未指定 `threadId` 时取编排线程（会话内运行指示行展示的就是顶层线程）；
 * 找不到条目时返回 `null`，界面不显示 Web 专属阶段。
 */
export function webModelTurnStage(
  sessionId: string,
  threadId = WEB_MODEL_ORCHESTRATOR_THREAD_ID,
): { stage: WebModelRuntimeEntry['stage']; queuePosition: number | null } | null {
  const scoped = webModelRuntimeEntries(sessionId);
  const active = scoped.filter((item) => item.active !== false);
  if (active.length === 0) return null;
  const entry = active.find((item) => item.threadId === threadId) ?? active[0];
  return { stage: entry.stage, queuePosition: entry.queuePosition };
}

/** 该会话已发出的账号消息条数（A16：T2 每个工具轮次 +1，T3 同回复续接不额外计）。 */
export function webModelSentMessages(sessionId: string): number {
  return webModelRuntimeEntries(sessionId)
    .reduce((total, entry) => total + (Number.isFinite(entry.sentMessages) ? entry.sentMessages : 0), 0);
}

/**
 * 当前会话应显示的临时对话宿主。
 *
 * 运行态条目包含 daemon 已物化的 `pageId` 投影；优先选编排线程，避免子代理
 * 的后台页面抢走用户正在查看的主会话页面。没有运行态投影时回退到主页。
 */
export function webModelActivePageId(
  sessionId: string,
  threadId = WEB_MODEL_ORCHESTRATOR_THREAD_ID,
  availablePageIds?: readonly string[],
): string | null {
  const availablePages = availablePageIds ? new Set(availablePageIds) : null;
  const entries = webModelRuntimeEntries(sessionId)
    // `active` means that a turn is currently in flight; it does not mean that
    // the temporary conversation is still usable.  A finished turn must keep
    // its page selected so the user can see the same multi-turn ChatGPT
    // conversation after Magi has received the reply.  The host list is the
    // authoritative physical-guest projection, so use it to exclude pages
    // that were released/suspended or are stale after an epoch rebuild.
    .filter((entry) => (
      typeof entry.pageId === 'string'
      && entry.pageId.trim()
      && (!availablePages || availablePages.has(entry.pageId))
    ));
  const entry = entries.find((item) => item.threadId === threadId)
    ?? entries[0];
  return entry?.pageId?.trim() || null;
}

/**
 * 是否需要在界面提示「已接管」（§5.8、§9.3 #4）。
 *
 * 绑定已转成 `user_owned` / `invalidated` 时，下一次发送会推进 epoch 并全量重放；
 * 提示只在当前实例上成立，新一轮成功后会写回 `magi_owned` 并自动消失。
 */
export function webModelTakeoverPending(sessionId: string): boolean {
  const latest = new Map<string, WebModelRuntimeEntry>();
  for (const entry of webModelRuntimeEntries(sessionId)) {
    const identity = [entry.threadId, entry.engineId, entry.effort].join('\u0001');
    const previous = latest.get(identity);
    if (!previous || entry.epoch >= previous.epoch) latest.set(identity, entry);
  }
  return [...latest.values()].some(
    (entry) => entry.ownership === 'user_owned' || entry.ownership === 'invalidated',
  );
}

/** 当前有在飞 turn 的对话实例数（编排者 + 子代理各算一条）。 */
export function webModelActiveTurnCount(): number {
  return runtimeState.entries.filter((entry) => entry.active !== false).length;
}
