/**
 * Right Pane Store - 右侧多 Tab 面板状态。
 *
 * 设计约束（代理详情与代码详情共用的右侧多 Tab 面板）：
 * - 状态以 workspace/session 为边界，跨会话隔离；workspace 文件预览允许无 session
 * - 三个正交轴：openTabs / activeTabId / collapsed
 * - collapsed 与 openTabs 正交：折叠不销毁 tabs，关闭单 tab 才销毁
 * - 全部 tab 关闭 → 强制 collapsed = true（下一次展开为空白 Pane）
 * - togglePane() 仅切 collapsed；openTab*() 触发自动展开
 * - 同 kind 同 key（agent: agentRunId / code: filepath / browser: BrowserTabId）幂等：复用现有 tab 并激活
 * - 浏览器 Tab 的存在性只由 BrowserAuthority 决定，前端 localStorage 只保存布局，不保存浏览器 Tab 实体
 * - terminal 以 terminalTabId 作为唯一键；每次新建都是独立命令终端，不共享输出历史
 */

export type RightPaneTabKind = 'agent' | 'code' | 'browser' | 'terminal';

/** Agent tab payload —— 代理运行 ID，内容由 canonical projection 按 metadata.taskId 过滤运行输出 */
export interface AgentTabPayload {
  agentRunId: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

/** Code tab payload —— filepath 必填；diff 存在时走 diff 视图，否则走单文件 viewer */
export interface CodeTabPayload {
  filepath: string;
  displayPath?: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  /** 可选：unified diff 文本；存在时优先走 diff 视图 */
  diff?: string | null;
  /** 差异基线全文；只保留在内存中，用于展开未变更区段。 */
  originalContent?: string | null;
  /** 差异当前全文；只保留在内存中，用于展开未变更区段。 */
  currentContent?: string | null;
  /** 该 tab 来自变更记录；刷新恢复时用权威 changes/diff 接口重取 diff，不持久化大文本 */
  isChangeDiff?: boolean;
  /** 变更投影版本；变化时废弃旧 diff 缓存并从权威接口重新读取。 */
  changeRevision?: string;
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
  /** 工具输出携带的瞬时图片数据；不得写入 localStorage。 */
  imageDataUrl?: string;
}

export interface BrowserTabPayload {
  browserSessionId: string;
  tabId: string;
  workspaceId: string;
  workspacePath?: string;
  sessionId: string;
}

/** Terminal tab payload —— 用户手动命令终端的稳定实例 ID，不持久化运行输出。 */
export interface TerminalTabPayload {
  terminalTabId: string;
  workspaceId: string;
  workspacePath?: string;
  sessionId: string;
}

export interface BrowserAuthorityTabProjection {
  tabId: string;
  lifecycle: 'creating' | 'ready' | 'suspended' | 'crashed' | 'closed';
  url: string;
  title: string;
}

export interface BrowserAuthoritySessionProjection {
  browserSessionId: string;
  activeTabId: string | null;
  tabs: BrowserAuthorityTabProjection[];
}

export type RightPaneTabPayload = AgentTabPayload | CodeTabPayload | BrowserTabPayload | TerminalTabPayload;

export interface RightPaneTab {
  id: string;
  kind: RightPaneTabKind;
  /** Tab 标题（如代理名称 / 文件名）；展示用，可后续更新 */
  label: string;
  /** 强调色，可传 CSS 颜色值或 token 名（如 'color-claude'）；null 表示无强调色 */
  accentToken: string | null;
  payload: RightPaneTabPayload;
  /** 最近激活时间，用于关闭活动 Tab 后选择恢复目标。 */
  lastActivatedAt: number;
}

export interface SessionPaneState {
  openTabs: RightPaneTab[];
  activeTabId: string | null;
  collapsed: boolean;
}

interface RightPaneRootState {
  /** 当前右侧面板作用域 key：workspace 或 workspace + session 共同决定，避免跨工作区串面板 */
  activeScopeKey: string;
  /** 当前工作区 id；仅用于后续打开 tab 时补齐作用域 */
  activeWorkspaceId: string;
  /** 当前原始会话 id；展示与调用外部 session API 时使用 */
  activeSessionId: string;
  perSession: Record<string, SessionPaneState>;
}

const EMPTY_SESSION_STATE: SessionPaneState = {
  openTabs: [],
  activeTabId: null,
  collapsed: true,
};

/** localStorage 持久化 key，带 schema 版本号方便后续演化 */
const STORAGE_KEY = 'magi-right-pane-state.v3';
/** 持久化 session 总数硬上限：超过后按 lastActivatedAt 倒序保留最近 N 个，防止长期使用膨胀 */
const MAX_PERSISTED_SESSIONS = 50;
const WORKSPACE_SCOPE_PREFIX = 'workspace:';

interface PersistedShape {
  version: 3 | 4;
  activeScopeKey: string;
  activeWorkspaceId: string;
  activeSessionId: string;
  perSession: Record<string, SessionPaneState>;
}

function normalizeWorkspaceId(workspaceId: string | null | undefined): string {
  return typeof workspaceId === 'string' ? workspaceId.trim() : '';
}

function normalizeSessionId(sessionId: string | null | undefined): string {
  return typeof sessionId === 'string' ? sessionId.trim() : '';
}

function sessionScopeKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return '';
  }
  const normalizedWorkspaceId = normalizeWorkspaceId(workspaceId);
  return normalizedWorkspaceId
    ? `${normalizedWorkspaceId}\u0000${normalizedSessionId}`
    : `session:${normalizedSessionId}`;
}

function workspaceScopeKey(workspaceId: string | null | undefined): string {
  const normalizedWorkspaceId = normalizeWorkspaceId(workspaceId);
  return normalizedWorkspaceId ? `${WORKSPACE_SCOPE_PREFIX}${normalizedWorkspaceId}` : '';
}

function paneScopeKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  return sessionScopeKey(workspaceId, sessionId) || workspaceScopeKey(workspaceId);
}

function normalizeStoredScopeKey(scopeKeyOrSessionId: string | null | undefined): string {
  const value = normalizeSessionId(scopeKeyOrSessionId);
  if (!value) {
    return '';
  }
  if (
    rightPaneState.perSession[value]
    || value.includes('\u0000')
    || value.startsWith('session:')
    || value.startsWith(WORKSPACE_SCOPE_PREFIX)
  ) {
    return value;
  }
  return sessionScopeKey(rightPaneState.activeWorkspaceId, value);
}

/**
 * 序列化前裁剪 code tab payload —— content / diff / headSummary / tailSummary / imageDataUrl
 * 单条可达 100KB+，恢复后由 RightPane.svelte 的 fetchedContents $effect 重新拉取，
 * 不需要进 localStorage。元数据（filepath / contentKind / size / mime / symlinkTarget / language）
 * 全部保留，刷新后能立即识别 tab kind 与文件信息。
 */
function sanitizeTabForPersist(tab: RightPaneTab): RightPaneTab {
  if (tab.kind !== 'code') return tab;
  const payload = tab.payload as CodeTabPayload;
  const slim: CodeTabPayload = {
    filepath: payload.filepath,
    displayPath: payload.displayPath,
    workspaceId: payload.workspaceId,
    workspacePath: payload.workspacePath,
    sessionId: payload.sessionId,
    isChangeDiff: payload.isChangeDiff,
    changeRevision: payload.changeRevision,
    language: payload.language ?? null,
    contentKind: payload.contentKind,
    size: payload.size,
    mime: payload.mime,
    symlinkTarget: payload.symlinkTarget,
    // 显式丢弃：content / diff / headSummary / tailSummary / imageDataUrl
  };
  return { ...tab, payload: slim };
}

function tabsForPersist(tabs: RightPaneTab[]): RightPaneTab[] {
  // BrowserAuthority 在 daemon 重启和运行组件升级时负责恢复浏览器 Tab。
  // 不把浏览器实体复制到 localStorage，避免前端先挂载已经失效的 Host 引用。
  return tabs.filter((tab) => tab.kind !== 'browser').map(sanitizeTabForPersist);
}

function isRestorableTab(tab: RightPaneTab): boolean {
  if (!tab || typeof tab.id !== 'string') {
    return false;
  }
  if (tab.kind === 'agent') {
    const agentRunId = (tab.payload as AgentTabPayload | undefined)?.agentRunId?.trim() || '';
    return Boolean(agentRunId) && !agentRunId.includes('[redacted]');
  }
  if (tab.kind === 'code') {
    return Boolean((tab.payload as CodeTabPayload | undefined)?.filepath?.trim());
  }
  if (tab.kind === 'browser') return false;
  if (tab.kind === 'terminal') {
    const payload = tab.payload as TerminalTabPayload | undefined;
    return Boolean(
      payload?.terminalTabId?.trim()
      && payload?.workspaceId?.trim()
      && payload?.sessionId?.trim(),
    );
  }
  return false;
}

/** 从 localStorage 恢复 perSession + activeSessionId；解析/版本不符则静默回退到空状态 */
function loadPersisted(): void {
  if (typeof window === 'undefined') return;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw) as PersistedShape;
    if (!parsed || (parsed.version !== 3 && parsed.version !== 4)) return;
    const recovered: Record<string, SessionPaneState> = {};
    for (const [sid, state] of Object.entries(parsed.perSession ?? {})) {
      if (!state || !Array.isArray(state.openTabs)) continue;
      const openTabs = state.openTabs.filter(isRestorableTab);
      const activeTabId = typeof state.activeTabId === 'string'
        && openTabs.some((tab) => tab.id === state.activeTabId)
        ? state.activeTabId
        : openTabs[0]?.id ?? null;
      recovered[sid] = {
        openTabs,
        activeTabId,
        // 浏览器 Tab 会在连接 BrowserAuthority 后重新投影，不能因启动时
        // 尚未投影而把用户原本展开的右侧面板永久折叠。
        collapsed: Boolean(state.collapsed),
      };
    }
    rightPaneState.perSession = recovered;
    rightPaneState.activeWorkspaceId = normalizeWorkspaceId(parsed.activeWorkspaceId);
    rightPaneState.activeSessionId = normalizeSessionId(parsed.activeSessionId);
    const activeScopeKey = normalizeSessionId(parsed.activeScopeKey)
      || sessionScopeKey(rightPaneState.activeWorkspaceId, rightPaneState.activeSessionId);
    rightPaneState.activeScopeKey = recovered[activeScopeKey] ? activeScopeKey : '';
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
      version: 4,
      activeScopeKey: rightPaneState.activeScopeKey,
      activeWorkspaceId: rightPaneState.activeWorkspaceId,
      activeSessionId: rightPaneState.activeSessionId,
      perSession: Object.fromEntries(
        kept.map(([sid, state]) => [
          sid,
          {
            openTabs: tabsForPersist(state.openTabs),
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
  activeScopeKey: '',
  activeWorkspaceId: '',
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

function ensureSession(scopeKey: string): SessionPaneState {
  let state = rightPaneState.perSession[scopeKey];
  if (!state) {
    state = {
      openTabs: [],
      activeTabId: null,
      collapsed: true,
    };
    rightPaneState.perSession[scopeKey] = state;
  }
  return state;
}

function tabKey(kind: RightPaneTabKind, payload: RightPaneTabPayload): string {
  if (kind === 'agent') {
    return `agent:${(payload as AgentTabPayload).agentRunId}`;
  }
  if (kind === 'code') {
    return `code:${(payload as CodeTabPayload).filepath}`;
  }
  if (kind === 'terminal') {
    return `terminal:${(payload as TerminalTabPayload).terminalTabId}`;
  }
  const browserPayload = payload as BrowserTabPayload;
  return `browser:${browserPayload.browserSessionId}:${browserPayload.tabId}`;
}

function now(): number {
  if (typeof performance !== 'undefined' && typeof performance.now === 'function') {
    return performance.now();
  }
  return Date.now();
}

function migrateWorkspacePaneIntoSession(workspaceId: string, targetScopeKey: string): void {
  const sourceScopeKey = workspaceScopeKey(workspaceId);
  if (!sourceScopeKey || sourceScopeKey === targetScopeKey) {
    return;
  }
  const source = rightPaneState.perSession[sourceScopeKey];
  if (!source || source.openTabs.length === 0) {
    return;
  }

  const target = ensureSession(targetScopeKey);
  for (const sourceTab of source.openTabs) {
    const existingIndex = target.openTabs.findIndex((tab) => tab.id === sourceTab.id);
    if (existingIndex >= 0) {
      if (sourceTab.lastActivatedAt > target.openTabs[existingIndex].lastActivatedAt) {
        target.openTabs[existingIndex] = sourceTab;
      }
      continue;
    }
    target.openTabs = [...target.openTabs, sourceTab];
  }

  if (source.activeTabId && target.openTabs.some((tab) => tab.id === source.activeTabId)) {
    target.activeTabId = source.activeTabId;
  }
  target.collapsed = target.openTabs.length === 0 ? true : target.collapsed && source.collapsed;
  delete rightPaneState.perSession[sourceScopeKey];
}

/** 内部：插入或激活已有 tab；负责自动展开并设为 active。 */
function upsertTab(
  scopeKey: string,
  kind: RightPaneTabKind,
  payload: RightPaneTabPayload,
  label: string,
  accentToken: string | null,
  activate = true,
): RightPaneTab | null {
  rightPaneState.activeScopeKey = scopeKey;
  if (kind === 'code') {
    const codePayload = payload as CodeTabPayload;
    rightPaneState.activeWorkspaceId = normalizeWorkspaceId(codePayload.workspaceId);
    rightPaneState.activeSessionId = normalizeSessionId(codePayload.sessionId);
  } else if (kind === 'browser') {
    const browserPayload = payload as BrowserTabPayload;
    rightPaneState.activeWorkspaceId = normalizeWorkspaceId(browserPayload.workspaceId);
    rightPaneState.activeSessionId = normalizeSessionId(browserPayload.sessionId);
  } else if (kind === 'terminal') {
    const terminalPayload = payload as TerminalTabPayload;
    rightPaneState.activeWorkspaceId = normalizeWorkspaceId(terminalPayload.workspaceId);
    rightPaneState.activeSessionId = normalizeSessionId(terminalPayload.sessionId);
  }
  const session = ensureSession(scopeKey);
  const id = tabKey(kind, payload);
  const existing = session.openTabs.find((tab) => tab.id === id);
  const timestamp = now();

  if (existing) {
    existing.label = label;
    existing.accentToken = accentToken;
    existing.payload = payload;
    if (activate) {
      existing.lastActivatedAt = timestamp;
      session.activeTabId = id;
      session.collapsed = false;
    }
    return existing;
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
  if (activate) {
    session.activeTabId = id;
    session.collapsed = false;
  }
  return tab;
}

// ============================================================================
// Public API
// ============================================================================

/**
 * 激活右侧面板上下文。
 * - 有 session 时使用 workspace/session 作用域
 * - 无 session 时使用 workspace 作用域，保证文件树/知识库预览仍可打开
 */
export function activateRightPaneSession(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): void {
  const normalizedWorkspaceId = normalizeWorkspaceId(workspaceId);
  const normalizedSessionId = normalizeSessionId(sessionId);
  const scopeKey = paneScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (normalizedWorkspaceId && normalizedSessionId) {
    migrateWorkspacePaneIntoSession(normalizedWorkspaceId, scopeKey);
  }
  rightPaneState.activeWorkspaceId = normalizedWorkspaceId;
  rightPaneState.activeSessionId = normalizedSessionId;
  rightPaneState.activeScopeKey = scopeKey;
  if (scopeKey) {
    ensureSession(scopeKey);
  }
}

/** 读取某个 session 的面板状态（响应式引用）；空 sessionId 或未初始化时返回空快照 */
export function getRightPaneState(scopeKeyOrSessionId: string | null | undefined): SessionPaneState {
  const scopeKey = normalizeStoredScopeKey(scopeKeyOrSessionId);
  if (!scopeKey) {
    return EMPTY_SESSION_STATE;
  }
  return rightPaneState.perSession[scopeKey] ?? EMPTY_SESSION_STATE;
}

/** 打开（或激活）一个 agent tab；agentRunId 同时作为去重 key */
export function openAgentTab(
  sessionId: string | null | undefined,
  agentRunId: string | null | undefined,
  options?: {
    label?: string;
    displayPath?: string;
    accentToken?: string | null;
    workspaceId?: string | null;
    workspacePath?: string | null;
  },
): void {
  const normalizedSession = normalizeSessionId(sessionId);
  if (!normalizedSession) {
    return;
  }
  const workspaceId = normalizeWorkspaceId(options?.workspaceId)
    || (normalizedSession === rightPaneState.activeSessionId ? rightPaneState.activeWorkspaceId : '');
  const scopeKey = sessionScopeKey(workspaceId, normalizedSession);
  if (!scopeKey) {
    return;
  }
  const trimmedAgentRunId = typeof agentRunId === 'string' ? agentRunId.trim() : '';
  if (!trimmedAgentRunId) {
    return;
  }
  const label = options?.label?.trim() || trimmedAgentRunId;
  const accentToken = options?.accentToken ?? null;
  rightPaneState.activeWorkspaceId = workspaceId;
  rightPaneState.activeSessionId = normalizedSession;
  upsertTab(
    scopeKey,
    'agent',
    {
      agentRunId: trimmedAgentRunId,
      workspaceId,
      workspacePath: typeof options?.workspacePath === 'string' ? options.workspacePath.trim() : undefined,
      sessionId: normalizedSession,
    },
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
    displayPath?: string;
    diff?: string | null;
    originalContent?: string | null;
    currentContent?: string | null;
    isChangeDiff?: boolean;
    changeRevision?: string;
    content?: string | null;
    language?: string | null;
    workspaceId?: string;
    workspacePath?: string;
    sessionId?: string;
    contentKind?: import('../types/message').EditContentKind;
    size?: number;
    mime?: string;
    symlinkTarget?: string;
    headSummary?: string;
    tailSummary?: string;
    imageDataUrl?: string;
  },
): void {
  const trimmedFilepath = typeof filepath === 'string' ? filepath.trim() : '';
  if (!trimmedFilepath) {
    return;
  }
  const workspaceId = normalizeWorkspaceId(options?.workspaceId)
    || rightPaneState.activeWorkspaceId;
  const requestedSession = Object.prototype.hasOwnProperty.call(options ?? {}, 'sessionId')
    ? normalizeSessionId(options?.sessionId)
    : normalizeSessionId(sessionId);
  const normalizedSession = requestedSession
    || (workspaceId === rightPaneState.activeWorkspaceId ? rightPaneState.activeSessionId : '');
  const scopeKey = paneScopeKey(workspaceId, normalizedSession);
  if (!scopeKey) {
    return;
  }
  const displayPath = options?.displayPath?.trim() || trimmedFilepath;
  const baseName = displayPath.split(/[\\/]/u).pop() || displayPath;
  const label = options?.label?.trim() || baseName;
  rightPaneState.activeWorkspaceId = workspaceId;
  rightPaneState.activeSessionId = normalizedSession;
  upsertTab(
    scopeKey,
    'code',
    {
      filepath: trimmedFilepath,
      displayPath,
      workspaceId,
      workspacePath: options?.workspacePath,
      sessionId: normalizedSession || undefined,
      diff: options?.diff ?? null,
      ...(Object.prototype.hasOwnProperty.call(options ?? {}, 'originalContent')
        ? { originalContent: options?.originalContent ?? null }
        : {}),
      ...(Object.prototype.hasOwnProperty.call(options ?? {}, 'currentContent')
        ? { currentContent: options?.currentContent ?? null }
        : {}),
      isChangeDiff: options?.isChangeDiff,
      changeRevision: options?.changeRevision,
      content: options?.content ?? null,
      language: options?.language ?? null,
      contentKind: options?.contentKind,
      size: options?.size,
      mime: options?.mime,
      symlinkTarget: options?.symlinkTarget,
      headSummary: options?.headSummary,
      tailSummary: options?.tailSummary,
      imageDataUrl: options?.imageDataUrl,
    },
    label,
    null,
  );
}

export function openBrowserTab(
  browserSessionId: string | null | undefined,
  tabId: string | null | undefined,
  options: {
    workspaceId: string;
    workspacePath?: string;
    sessionId: string;
    label?: string;
  },
): void {
  const normalizedBrowserSessionId = typeof browserSessionId === 'string'
    ? browserSessionId.trim()
    : '';
  const normalizedTabId = typeof tabId === 'string' ? tabId.trim() : '';
  const workspaceId = normalizeWorkspaceId(options.workspaceId);
  const sessionId = normalizeSessionId(options.sessionId);
  const scopeKey = sessionScopeKey(workspaceId, sessionId);
  if (!normalizedBrowserSessionId || !normalizedTabId || !workspaceId || !sessionId || !scopeKey) {
    return;
  }
  upsertTab(
    scopeKey,
    'browser',
    {
      browserSessionId: normalizedBrowserSessionId,
      tabId: normalizedTabId,
      workspaceId,
      workspacePath: options.workspacePath?.trim() || undefined,
      sessionId,
    },
    options.label?.trim() || 'Browser',
    null,
  );
}

let terminalTabCounter = 0;

function newTerminalTabId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  terminalTabCounter += 1;
  return `terminal-${Date.now()}-${terminalTabCounter}`;
}

/**
 * 新建独立的用户命令终端 Tab。每个 Tab 的执行记录仅保留在内存中，刷新页面后不伪造
 * 旧进程或旧输出仍然可用；后台进程本身继续由 shell_exec 按 session/workspace 管理。
 */
export function openTerminalTab(options: {
  workspaceId: string;
  workspacePath?: string;
  sessionId: string;
}): string | null {
  const workspaceId = normalizeWorkspaceId(options.workspaceId);
  const sessionId = normalizeSessionId(options.sessionId);
  const scopeKey = sessionScopeKey(workspaceId, sessionId);
  if (!workspaceId || !sessionId || !scopeKey) {
    return null;
  }
  const terminalTabId = newTerminalTabId();
  upsertTab(
    scopeKey,
    'terminal',
    {
      terminalTabId,
      workspaceId,
      workspacePath: options.workspacePath?.trim() || undefined,
      sessionId,
    },
    'Terminal',
    null,
  );
  return terminalTabId;
}

/**
 * 将 BrowserAuthority 会话快照投影到右侧一级 Tab。
 * BrowserAuthority 决定 Browser Page 的存在性和活动 Page；本地 store 只保留布局状态。
 */
export function synchronizeBrowserTabs(
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
  sessionId: string | null | undefined,
  snapshot: BrowserAuthoritySessionProjection | null,
  options?: {
    revealActiveTab?: boolean;
    newTabLabel?: string;
  },
): void {
  const normalizedWorkspaceId = normalizeWorkspaceId(workspaceId);
  const normalizedSessionId = normalizeSessionId(sessionId);
  const scopeKey = sessionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (!scopeKey) return;

  const pane = ensureSession(scopeKey);
  const previousActiveTabId = pane.activeTabId;
  const previousActiveTab = pane.openTabs.find((tab) => tab.id === previousActiveTabId);
  const browserSessionId = snapshot?.browserSessionId.trim() || '';
  const authorityTabs = (snapshot?.tabs ?? []).filter((tab) => (
    tab.tabId.trim() && tab.lifecycle !== 'closed'
  ));
  const authorityPaneIds = new Set(
    authorityTabs.map((tab) => `browser:${browserSessionId}:${tab.tabId.trim()}`),
  );

  pane.openTabs = pane.openTabs.filter((tab) => (
    tab.kind !== 'browser' || authorityPaneIds.has(tab.id)
  ));

  for (const tab of authorityTabs) {
    const normalizedTabId = tab.tabId.trim();
    const title = tab.title.trim();
    const url = tab.url.trim();
    const label = title
      || (url && url !== 'about:blank' ? url : options?.newTabLabel?.trim() || 'Browser');
    upsertTab(
      scopeKey,
      'browser',
      {
        browserSessionId,
        tabId: normalizedTabId,
        workspaceId: normalizedWorkspaceId,
        workspacePath: normalizeWorkspaceId(workspacePath) || undefined,
        sessionId: normalizedSessionId,
      },
      label,
      null,
      false,
    );
  }

  const authorityActivePaneId = snapshot?.activeTabId?.trim()
    ? `browser:${browserSessionId}:${snapshot.activeTabId.trim()}`
    : null;
  const authorityActiveExists = Boolean(
    authorityActivePaneId
      && pane.openTabs.some((tab) => tab.id === authorityActivePaneId),
  );
  const previousActiveStillExists = Boolean(
    previousActiveTabId
      && pane.openTabs.some((tab) => tab.id === previousActiveTabId),
  );

  if (options?.revealActiveTab && authorityActivePaneId && authorityActiveExists) {
    pane.activeTabId = authorityActivePaneId;
    pane.collapsed = false;
    const active = pane.openTabs.find((tab) => tab.id === authorityActivePaneId);
    if (active) active.lastActivatedAt = now();
    return;
  }

  if (previousActiveTab?.kind === 'browser' && authorityActivePaneId && authorityActiveExists) {
    pane.activeTabId = authorityActivePaneId;
    return;
  }

  if (previousActiveStillExists) {
    pane.activeTabId = previousActiveTabId;
    return;
  }

  // 页面刷新后浏览器 Tab 不从 localStorage 恢复，首轮权威投影没有本地
  // activeTabId。此时使用 BrowserAuthority 的活动 Tab，但不改变 collapsed。
  if (!previousActiveTabId && authorityActiveExists) {
    pane.activeTabId = authorityActivePaneId;
    return;
  }

  if (pane.openTabs.length === 0) {
    pane.activeTabId = null;
    pane.collapsed = true;
    return;
  }

  let next = pane.openTabs[0];
  for (const tab of pane.openTabs) {
    if (tab.lastActivatedAt > next.lastActivatedAt) next = tab;
  }
  pane.activeTabId = next.id;
}

export interface PendingChangeTabProjection {
  filePath: string;
  snapshotId?: string;
  updatedAt?: number;
  contentKind?: import('../types/message').EditContentKind;
  size?: number;
  mime?: string;
  symlinkTarget?: string;
  headSummary?: string;
  tailSummary?: string;
}

export function changeDiffRevision(change: PendingChangeTabProjection): string {
  return [
    change.snapshotId?.trim() ?? '',
    typeof change.updatedAt === 'number' && Number.isFinite(change.updatedAt)
      ? String(change.updatedAt)
      : '0',
  ].join(':');
}

/**
 * 用权威 pending changes 投影同步已打开的变更 diff 页签。
 * 已退出变更集的页签立即关闭；仍存在但版本变化的页签清除旧内容，交由 RightPane 重新拉取。
 */
export function synchronizeChangeDiffTabs(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
  pendingChanges: readonly PendingChangeTabProjection[],
): void {
  const scopeKey = sessionScopeKey(workspaceId, sessionId);
  if (!scopeKey) {
    return;
  }
  const pane = rightPaneState.perSession[scopeKey];
  if (!pane) {
    return;
  }
  const changesByPath = new Map(
    pendingChanges
      .map((change) => [change.filePath.trim(), change] as const)
      .filter(([filePath]) => filePath.length > 0),
  );
  const staleTabIds: string[] = [];
  for (const tab of pane.openTabs) {
    if (tab.kind !== 'code') {
      continue;
    }
    const payload = tab.payload as CodeTabPayload;
    if (!payload.isChangeDiff) {
      continue;
    }
    const change = changesByPath.get(payload.filepath);
    if (!change) {
      staleTabIds.push(tab.id);
      continue;
    }
    const nextRevision = changeDiffRevision(change);
    if (payload.changeRevision === nextRevision) {
      continue;
    }
    const {
      diff: _diff,
      originalContent: _originalContent,
      currentContent: _currentContent,
      content: _content,
      ...stablePayload
    } = payload;
    tab.payload = {
      ...stablePayload,
      changeRevision: nextRevision,
      contentKind: change.contentKind,
      size: change.size,
      mime: change.mime,
      symlinkTarget: change.symlinkTarget,
      headSummary: change.headSummary,
      tailSummary: change.tailSummary,
    };
  }
  for (const tabId of staleTabIds) {
    closeTab(scopeKey, tabId);
  }
}

/**
 * 关闭单个 tab（真销毁）。
 * - 关闭 active tab：下一个候选优先选 lastActivatedAt 最大的剩余 tab
 * - 关闭后 openTabs 为空 → 强制 collapsed = true
 */
export function closeTab(
  scopeKeyOrSessionId: string | null | undefined,
  tabId: string,
): void {
  const scopeKey = normalizeStoredScopeKey(scopeKeyOrSessionId);
  if (!scopeKey) {
    return;
  }
  const session = rightPaneState.perSession[scopeKey];
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

/** 显式设置 collapsed 状态 */
export function setRightPaneCollapsed(
  scopeKeyOrSessionId: string | null | undefined,
  collapsed: boolean,
): void {
  const scopeKey = normalizeStoredScopeKey(scopeKeyOrSessionId);
  if (!scopeKey) {
    return;
  }
  const session = ensureSession(scopeKey);
  session.collapsed = collapsed;
}

/** 切换 active tab；更新 lastActivatedAt */
export function setActiveRightPaneTab(
  scopeKeyOrSessionId: string | null | undefined,
  tabId: string,
): void {
  const scopeKey = normalizeStoredScopeKey(scopeKeyOrSessionId);
  if (!scopeKey) {
    return;
  }
  const session = rightPaneState.perSession[scopeKey];
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

/** 更新 tab 的展示标题，不改变激活顺序。 */
export function updateRightPaneTabLabel(
  scopeKeyOrSessionId: string | null | undefined,
  tabId: string,
  label: string,
): void {
  const scopeKey = normalizeStoredScopeKey(scopeKeyOrSessionId);
  const normalizedLabel = label.trim();
  if (!scopeKey || !normalizedLabel) return;
  const tab = rightPaneState.perSession[scopeKey]?.openTabs.find((item) => item.id === tabId);
  if (tab && tab.label !== normalizedLabel) {
    tab.label = normalizedLabel;
  }
}

/** 清理某个 session 的所有 tab 状态（在 session 关闭/重置时调用） */
export function clearRightPaneSession(
  scopeKeyOrWorkspaceId: string | null | undefined,
  sessionId?: string | null,
): void {
  const scopeKey = sessionId === undefined
    ? normalizeStoredScopeKey(scopeKeyOrWorkspaceId)
    : sessionScopeKey(scopeKeyOrWorkspaceId, sessionId);
  if (!scopeKey) {
    rightPaneState.perSession = {};
    rightPaneState.activeScopeKey = '';
    rightPaneState.activeWorkspaceId = '';
    rightPaneState.activeSessionId = '';
    return;
  }
  delete rightPaneState.perSession[scopeKey];
  if (rightPaneState.activeScopeKey === scopeKey) {
    rightPaneState.activeScopeKey = '';
    rightPaneState.activeWorkspaceId = '';
    rightPaneState.activeSessionId = '';
  }
}
