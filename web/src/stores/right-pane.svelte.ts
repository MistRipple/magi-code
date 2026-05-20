/**
 * Right Pane Store - 右侧多 Tab 面板状态。
 *
 * 设计约束（来自 _temp/worker-panel-design/D-right-pane-tabs.html §6）：
 * - 状态以 session 为边界，跨会话隔离
 * - 三个正交轴：openTabs（LRU 上限 6）/ activeTabId / collapsed
 * - collapsed 与 openTabs 正交：折叠不销毁 tabs，关闭单 tab 才销毁
 * - 全部 tab 关闭 → 强制 collapsed = true（下一次展开为空白 Pane）
 * - togglePane() 仅切 collapsed；openTab*() 触发自动展开
 * - LRU 淘汰：插入第 7 个 tab 时，从非 active tab 里挑 lastActivatedAt 最小的踢掉
 * - 同 kind 同 key（agent: workerTabId / code: filepath）幂等：复用现有 tab 并激活
 */

export type RightPaneTabKind = 'agent' | 'code';

/** Agent tab payload —— 仅需 workerTabId，内容由 canonical projection 投影 */
export interface AgentTabPayload {
  workerTabId: string;
}

/** Code tab payload —— filepath 必填；diff 存在时走 diff 视图，否则走单文件 viewer */
export interface CodeTabPayload {
  filepath: string;
  /** 可选：unified diff 文本；存在时优先走 diff 视图 */
  diff?: string | null;
  /** 可选：单文件源码；不存在时 RightPane 异步拉取 */
  content?: string | null;
  /** 可选：语言提示，用于语法高亮（按扩展名兜底） */
  language?: string | null;
  /** 文件内容类别：text / binary / large_text / symlink / special */
  contentKind?: import('../types/message').EditContentKind;
  /** 文件大小（字节），用于 binary / large_text 元信息展示 */
  size?: number;
  /** MIME 类型，用于 binary 元信息展示 */
  mime?: string;
  /** symlink 目标路径 */
  symlinkTarget?: string;
  /** large_text 头部摘要 */
  headSummary?: string;
  /** large_text 尾部摘要 */
  tailSummary?: string;
}

export type RightPaneTabPayload = AgentTabPayload | CodeTabPayload;

export interface RightPaneTab {
  id: string;
  kind: RightPaneTabKind;
  /** Tab 标题（如 worker label / 文件名）；展示用，可后续更新 */
  label: string;
  /** 强调色 token 名（如 'color-claude'）；null 表示无强调色 */
  accentToken: string | null;
  payload: RightPaneTabPayload;
  /** LRU 淘汰参考时间戳（performance.now 或 Date.now，递增即可） */
  lastActivatedAt: number;
}

export interface SessionPaneState {
  openTabs: RightPaneTab[];
  activeTabId: string | null;
  collapsed: boolean;
}

interface RightPaneRootState {
  /** 当前会话 id；活跃组件按此 key 读取 perSession */
  activeSessionId: string;
  perSession: Record<string, SessionPaneState>;
}

/** LRU 上限；超过即淘汰 lastActivatedAt 最小的非 active tab */
export const RIGHT_PANE_TAB_CAP = 6;

const EMPTY_SESSION_STATE: SessionPaneState = {
  openTabs: [],
  activeTabId: null,
  collapsed: true,
};

/** localStorage 持久化 key，带 schema 版本号方便后续演化 */
const STORAGE_KEY = 'magi-right-pane-state.v1';
/** 持久化 session 总数硬上限：超过后按 lastActivatedAt 倒序保留最近 N 个，防止长期使用膨胀 */
const MAX_PERSISTED_SESSIONS = 50;

interface PersistedShape {
  version: 1;
  activeSessionId: string;
  perSession: Record<string, SessionPaneState>;
}

/**
 * 序列化前裁剪 code tab payload —— content / diff / headSummary / tailSummary
 * 单条可达 100KB+，恢复后由 RightPane.svelte 的 fetchedContents $effect 重新拉取，
 * 不需要进 localStorage。元数据（filepath / contentKind / size / mime / symlinkTarget / language）
 * 全部保留，刷新后能立即识别 tab kind 与文件信息。
 */
function sanitizeTabForPersist(tab: RightPaneTab): RightPaneTab {
  if (tab.kind !== 'code') return tab;
  const payload = tab.payload as CodeTabPayload;
  const slim: CodeTabPayload = {
    filepath: payload.filepath,
    language: payload.language ?? null,
    contentKind: payload.contentKind,
    size: payload.size,
    mime: payload.mime,
    symlinkTarget: payload.symlinkTarget,
    // 显式丢弃：content / diff / headSummary / tailSummary
  };
  return { ...tab, payload: slim };
}

/** 从 localStorage 恢复 perSession + activeSessionId；解析/版本不符则静默回退到空状态 */
function loadPersisted(): void {
  if (typeof window === 'undefined') return;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw) as PersistedShape;
    if (!parsed || parsed.version !== 1) return;
    const recovered: Record<string, SessionPaneState> = {};
    for (const [sid, state] of Object.entries(parsed.perSession ?? {})) {
      if (!state || !Array.isArray(state.openTabs)) continue;
      recovered[sid] = {
        openTabs: state.openTabs,
        activeTabId: typeof state.activeTabId === 'string' ? state.activeTabId : null,
        collapsed: Boolean(state.collapsed),
      };
    }
    rightPaneState.perSession = recovered;
    rightPaneState.activeSessionId =
      typeof parsed.activeSessionId === 'string' ? parsed.activeSessionId : '';
  } catch {
    // 解析失败 → 维持空状态，不影响应用启动
  }
}

/** 把当前 perSession 序列化写入 localStorage；mutation 末尾同步调用 */
function persistState(): void {
  if (typeof window === 'undefined') return;
  try {
    const entries = Object.entries(rightPaneState.perSession);
    let kept: [string, SessionPaneState][] = entries;
    if (entries.length > MAX_PERSISTED_SESSIONS) {
      // 用 session 内最大 lastActivatedAt 作为 session 整体活跃度，倒序保留 top N
      const ranked = entries.map(([sid, state]) => {
        const ts = state.openTabs.reduce((acc, t) => Math.max(acc, t.lastActivatedAt), 0);
        return { sid, state, ts };
      });
      ranked.sort((a, b) => b.ts - a.ts);
      kept = ranked.slice(0, MAX_PERSISTED_SESSIONS).map((x) => [x.sid, x.state]);
    }
    const slim: PersistedShape = {
      version: 1,
      activeSessionId: rightPaneState.activeSessionId,
      perSession: Object.fromEntries(
        kept.map(([sid, state]) => [
          sid,
          {
            openTabs: state.openTabs.map(sanitizeTabForPersist),
            activeTabId: state.activeTabId,
            collapsed: state.collapsed,
          },
        ]),
      ),
    };
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(slim));
  } catch {
    // QuotaExceededError / SecurityError 等 → 静默忽略，不影响主流程
  }
}

export const rightPaneState = $state<RightPaneRootState>({
  activeSessionId: '',
  perSession: {},
});

// 模块加载时立即恢复——必须放在 rightPaneState 定义之后、任何使用方读取之前
loadPersisted();

// 自动持久化：$state proxy 是深度 reactive 的，任何 perSession / activeSessionId / tab 字段
// 的变化都会被 persistState 内部的遍历"读取"触发，从而重新写入 localStorage。
// 用 $effect.root 创建与模块寿命同生命周期的 reactive scope；页面 unload 时浏览器自动 GC。
// 这个收敛实现避免在每个 mutation 末尾手写一次 persist——新增 mutation 函数也不会漏。
if (typeof window !== 'undefined') {
  $effect.root(() => {
    $effect(() => {
      persistState();
    });
  });
}

function normalizeSessionId(sessionId: string | null | undefined): string {
  if (typeof sessionId !== 'string') {
    return '';
  }
  return sessionId.trim();
}

function ensureSession(sessionId: string): SessionPaneState {
  let state = rightPaneState.perSession[sessionId];
  if (!state) {
    state = {
      openTabs: [],
      activeTabId: null,
      collapsed: true,
    };
    rightPaneState.perSession[sessionId] = state;
  }
  return state;
}

function tabKey(kind: RightPaneTabKind, payload: RightPaneTabPayload): string {
  if (kind === 'agent') {
    return `agent:${(payload as AgentTabPayload).workerTabId}`;
  }
  return `code:${(payload as CodeTabPayload).filepath}`;
}

function now(): number {
  if (typeof performance !== 'undefined' && typeof performance.now === 'function') {
    return performance.now();
  }
  return Date.now();
}

/** 选择 LRU 淘汰目标：非 active tab 中 lastActivatedAt 最小的；找不到（理论上不会）返回 null */
function pickLruVictim(state: SessionPaneState): RightPaneTab | null {
  let victim: RightPaneTab | null = null;
  for (const tab of state.openTabs) {
    if (tab.id === state.activeTabId) {
      continue;
    }
    if (!victim || tab.lastActivatedAt < victim.lastActivatedAt) {
      victim = tab;
    }
  }
  return victim;
}

/** 内部：插入或激活已有 tab；负责 LRU 淘汰、自动展开、设为 active */
function upsertTab(
  sessionId: string,
  kind: RightPaneTabKind,
  payload: RightPaneTabPayload,
  label: string,
  accentToken: string | null,
): RightPaneTab | null {
  const session = ensureSession(sessionId);
  const id = tabKey(kind, payload);
  const existing = session.openTabs.find((tab) => tab.id === id);
  const timestamp = now();

  if (existing) {
    existing.label = label;
    existing.accentToken = accentToken;
    existing.payload = payload;
    existing.lastActivatedAt = timestamp;
    session.activeTabId = id;
    session.collapsed = false;
    return existing;
  }

  if (session.openTabs.length >= RIGHT_PANE_TAB_CAP) {
    const victim = pickLruVictim(session);
    if (victim) {
      session.openTabs = session.openTabs.filter((tab) => tab.id !== victim.id);
    }
  }

  const tab: RightPaneTab = {
    id,
    kind,
    label,
    accentToken,
    payload,
    lastActivatedAt: timestamp,
  };
  session.openTabs = [...session.openTabs, tab];
  session.activeTabId = id;
  session.collapsed = false;
  return tab;
}

// ============================================================================
// Public API
// ============================================================================

/**
 * 激活某个 session 的右侧面板上下文。
 * - 切换 session 时调用一次；无显式状态时 ensure 空 state
 */
export function activateRightPaneSession(sessionId: string | null | undefined): void {
  const normalized = normalizeSessionId(sessionId);
  rightPaneState.activeSessionId = normalized;
  if (normalized) {
    ensureSession(normalized);
  }
}

/** 读取某个 session 的面板状态（响应式引用）；空 sessionId 或未初始化时返回空快照 */
export function getRightPaneState(sessionId: string | null | undefined): SessionPaneState {
  const normalized = normalizeSessionId(sessionId);
  if (!normalized) {
    return EMPTY_SESSION_STATE;
  }
  return rightPaneState.perSession[normalized] ?? EMPTY_SESSION_STATE;
}

/** 打开（或激活）一个 agent tab；workerTabId 同时作为去重 key */
export function openAgentTab(
  sessionId: string | null | undefined,
  workerTabId: string | null | undefined,
  options?: { label?: string; accentToken?: string | null },
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const trimmedWorkerTabId = typeof workerTabId === 'string' ? workerTabId.trim() : '';
  if (!trimmedWorkerTabId) {
    return;
  }
  const label = options?.label?.trim() || trimmedWorkerTabId;
  const accentToken = options?.accentToken ?? null;
  upsertTab(
    normalizedSession,
    'agent',
    { workerTabId: trimmedWorkerTabId },
    label,
    accentToken,
  );
}

/** 打开（或激活）一个 code tab；filepath 同时作为去重 key */
export function openCodeTab(
  sessionId: string | null | undefined,
  filepath: string | null | undefined,
  options?: {
    label?: string;
    diff?: string | null;
    content?: string | null;
    language?: string | null;
    contentKind?: import('../types/message').EditContentKind;
    size?: number;
    mime?: string;
    symlinkTarget?: string;
    headSummary?: string;
    tailSummary?: string;
  },
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const trimmedFilepath = typeof filepath === 'string' ? filepath.trim() : '';
  if (!trimmedFilepath) {
    return;
  }
  const baseName = trimmedFilepath.split('/').pop() || trimmedFilepath;
  const label = options?.label?.trim() || baseName;
  upsertTab(
    normalizedSession,
    'code',
    {
      filepath: trimmedFilepath,
      diff: options?.diff ?? null,
      content: options?.content ?? null,
      language: options?.language ?? null,
      contentKind: options?.contentKind,
      size: options?.size,
      mime: options?.mime,
      symlinkTarget: options?.symlinkTarget,
      headSummary: options?.headSummary,
      tailSummary: options?.tailSummary,
    },
    label,
    null,
  );
}

/**
 * 关闭单个 tab（真销毁）。
 * - 关闭 active tab：下一个候选优先选 lastActivatedAt 最大的剩余 tab
 * - 关闭后 openTabs 为空 → 强制 collapsed = true
 */
export function closeTab(
  sessionId: string | null | undefined,
  tabId: string,
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const session = rightPaneState.perSession[normalizedSession];
  if (!session) {
    return;
  }
  const closingActive = session.activeTabId === tabId;
  const nextTabs = session.openTabs.filter((tab) => tab.id !== tabId);
  if (nextTabs.length === session.openTabs.length) {
    return;
  }
  session.openTabs = nextTabs;

  if (nextTabs.length === 0) {
    session.activeTabId = null;
    session.collapsed = true;
    return;
  }

  if (closingActive) {
    let next: RightPaneTab = nextTabs[0];
    for (const tab of nextTabs) {
      if (tab.lastActivatedAt > next.lastActivatedAt) {
        next = tab;
      }
    }
    session.activeTabId = next.id;
    next.lastActivatedAt = now();
  }
}

/** 切换 collapsed；不动 openTabs */
export function toggleRightPane(sessionId: string | null | undefined): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const session = ensureSession(normalizedSession);
  session.collapsed = !session.collapsed;
}

/** 显式设置 collapsed 状态 */
export function setRightPaneCollapsed(
  sessionId: string | null | undefined,
  collapsed: boolean,
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const session = ensureSession(normalizedSession);
  session.collapsed = collapsed;
}

/** 切换 active tab；更新 lastActivatedAt */
export function setActiveRightPaneTab(
  sessionId: string | null | undefined,
  tabId: string,
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const session = rightPaneState.perSession[normalizedSession];
  if (!session) {
    return;
  }
  const tab = session.openTabs.find((t) => t.id === tabId);
  if (!tab) {
    return;
  }
  session.activeTabId = tabId;
  tab.lastActivatedAt = now();
}

/** 清理某个 session 的所有 tab 状态（在 session 关闭/重置时调用） */
export function clearRightPaneSession(sessionId: string | null | undefined): void {
  const normalized = normalizeSessionId(sessionId);
  if (!normalized) {
    rightPaneState.perSession = {};
    rightPaneState.activeSessionId = '';
    return;
  }
  delete rightPaneState.perSession[normalized];
  if (rightPaneState.activeSessionId === normalized) {
    rightPaneState.activeSessionId = '';
  }
}
