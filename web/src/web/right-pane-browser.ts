/**
 * 右栏浏览器 Tab 的唯一创建入口。
 *
 * 「在右栏打开一个网页」曾经有两条独立实现：`RightPane` 的
 * `createBrowserPane`（点击消息里的链接）与 `openHtmlInMagiBrowser`
 * （工作区 HTML 文件），而且两者的消费者都挂在右栏组件里。右栏在 Desktop
 * 下是懒加载 + 条件挂载的，折叠或从未展开时组件并不存在，派发出去的事件
 * 没有任何监听者，链接点击被静默丢弃。
 *
 * 这里把「解析作用域 → 复用同 URL Tab → 创建 Browser Session 与 Tab →
 * 注册右栏 Tab」收敛成一份实现，由始终挂载的 Shell 调用，因此点击结果
 * 不再取决于右栏此刻是否挂载。
 *
 * 本模块只负责创建与注册；真实 Chromium guest 的物化、激活与恢复仍由
 * Shell 的 Desktop 面板激活事务和 `browser-authority-coordinator` 完成。
 */
import { i18n } from '../stores/i18n.svelte';
import { addToast } from '../stores/messages.svelte';
import {
  openBrowserTab,
  rightPaneState,
  setActiveRightPaneTab,
  setRightPaneCollapsed,
  type BrowserTabPayload,
  type RightPaneTab,
  type SessionPaneState,
} from '../stores/right-pane.svelte';
import { navigateSession, waitForSessionNavigation } from '../shared/session-navigation.svelte';
import {
  AgentApiError,
  agentUrl,
  buildFilePreviewQuery,
  createBrowserSession,
  createBrowserTab,
  getBrowserCapabilities,
  materializeSession,
} from './agent-api';

/** 一次打开动作要落到的 Magi 会话；字段缺失时回退到右栏当前作用域。 */
export interface BrowserOpenBinding {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

export interface HtmlFileBrowserTarget extends BrowserOpenBinding {
  filepath: string;
  sourceFolderId?: string;
}

/**
 * 一次「在右栏打开链接」的收敛结果，供调用方决定是否退回系统浏览器。
 *
 * - `opened`：已在内置浏览器处理（新建或复用 Tab）。
 * - `fallback-external`：当前环境没有内置 BrowserSurface（Web / 手机 Web），
 *   调用方应把地址交给系统浏览器。
 * - `rejected`：内置浏览器不可用（被设置关闭、控制通道未就绪）或创建失败；
 *   原因已经以提示形式给出，调用方不要再叠加第二条反馈。
 */
export type RightPaneBrowserOpenOutcome = 'opened' | 'fallback-external' | 'rejected';

interface ResolvedBrowserTarget {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

// 并发点击（例如一次双击、或同时点开两个链接）必须串行收敛，否则后一次
// 能力查询 / 会话创建会与第一次交错，产生重复 Browser Session 或重复 Tab。
let browserOpenLane: Promise<void> = Promise.resolve();

function enqueue<T>(operation: () => Promise<T>): Promise<T> {
  const result = browserOpenLane.then(operation, operation);
  browserOpenLane = result.then(() => undefined, () => undefined);
  return result;
}

function trim(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

/**
 * 内置浏览器可用性。
 *
 * 与 `RightPane` 的 `canCreateBrowserPane` 同一口径，但按需查询能力而不是
 * 依赖组件内已加载的派生状态，调用方因此不必先挂载右栏。查询失败（daemon
 * 不可达）按不可用处理，让调用方退回系统浏览器，不静默吞掉点击。
 */
async function inAppBrowserAvailable(): Promise<
  { kind: 'desktop' } | { kind: 'external' } | { kind: 'blocked' }
> {
  if (typeof window === 'undefined') return { kind: 'external' };
  if (window.magiDesktop?.runtime !== 'electron') return { kind: 'external' };
  try {
    const capabilities = await getBrowserCapabilities();
    const usable = capabilities.inAppBrowserEnabled
      && capabilities.platformCapabilities.desktopBrowserSurface;
    return usable ? { kind: 'desktop' } : { kind: 'blocked' };
  } catch (error) {
    console.warn('[right-pane-browser] 读取浏览器能力失败:', error);
    return { kind: 'blocked' };
  }
}

function browserOpenFailureFeedback(error: unknown): { type: 'warning' | 'error'; message: string } {
  if (error instanceof AgentApiError) {
    if (error.errorCode === 'BROWSER_GLOBAL_TAB_LIMIT_REACHED') {
      return { type: 'warning', message: i18n.t('browser.error.globalTabLimitReached') };
    }
    if (error.errorCode === 'BROWSER_SESSION_TAB_LIMIT_REACHED') {
      return { type: 'warning', message: i18n.t('browser.error.sessionTabLimitReached') };
    }
  }
  return { type: 'error', message: i18n.t('browser.error.openInternal') };
}

function reportBrowserOpenFailure(error: unknown): void {
  console.warn('[right-pane-browser] 新建浏览器面板失败:', error);
  const feedback = browserOpenFailureFeedback(error);
  addToast(feedback.type, feedback.message, undefined, { forceVisible: true });
}

function reportInAppBrowserUnavailable(): void {
  addToast(
    'warning',
    i18n.t(window.magiDesktop?.runtime === 'electron'
      ? 'browser.error.internalUnavailable'
      : 'browser.error.desktopRequired'),
    undefined,
    { forceVisible: true },
  );
}

/**
 * 在同一 Magi 会话作用域里，按身份定位已存在的同 URL 浏览器 Tab。
 *
 * 不按 `activeScopeKey` 反查：会话可能刚刚被物化，作用域 key 的收敛晚于
 * 本次调用；直接按 payload 的归属匹配，与 RightPane 的浏览器宿主投影同口径。
 */
function findBrowserTabForUrl(
  target: ResolvedBrowserTarget,
  url: string,
): { scopeKey: string; tab: RightPaneTab } | null {
  for (const [scopeKey, pane] of Object.entries(rightPaneState.perSession)) {
    const tab = findInPane(pane, target, url);
    if (tab) return { scopeKey, tab };
  }
  return null;
}

function findInPane(
  pane: SessionPaneState,
  target: ResolvedBrowserTarget,
  url: string,
): RightPaneTab | null {
  // 空白页是「再开一个浏览器」的显式意图，允许多个并存，不参与复用。
  if (url === 'about:blank') return null;
  return pane.openTabs.find((tab) => {
    if (tab.kind !== 'browser') return false;
    const payload = tab.payload as BrowserTabPayload;
    return payload.url === url
      && trim(payload.workspaceId) === target.workspaceId
      && trim(payload.sessionId) === target.sessionId;
  }) ?? null;
}

/**
 * 解析链接要落到的 Magi 会话。
 *
 * 调用方给出的绑定是权威的：字段显式传空串表示「个人 / 尚无会话」，不回退到
 * 右栏的旧作用域，否则个人草稿里点链接可能被误挂到上一个工作区。只有字段
 * 缺省（undefined）时才读右栏当前作用域。
 *
 * 浏览器会话与 Magi 会话同生命周期：已有真实会话时直接复用；否则先物化会话
 * 并等主 Renderer 完成导航，再创建浏览器资源，避免浏览器事件抢在 Composer
 * 完成绑定前切换右栏作用域。工作区作用域的导航要求 `workspacePath` 非空，
 * 调用方必须一并提供。
 */
async function resolveBrowserTarget(binding: BrowserOpenBinding): Promise<ResolvedBrowserTarget> {
  const workspaceId = binding.workspaceId !== undefined
    ? trim(binding.workspaceId)
    : rightPaneState.activeWorkspaceId.trim();
  const workspacePath = trim(binding.workspacePath);
  const sessionId = binding.sessionId !== undefined
    ? trim(binding.sessionId)
    : rightPaneState.activeSessionId.trim();
  if (sessionId) {
    return { workspaceId, workspacePath, sessionId };
  }
  if (workspaceId && !workspacePath) {
    throw new Error('工作区作用域缺少 workspacePath，无法物化会话');
  }
  const materialized = await materializeSession(
    workspaceId || null,
    workspaceId ? workspacePath : undefined,
  );
  const resolvedSessionId = materialized.sessionId;
  const navigation = navigateSession(workspaceId
    ? {
        kind: 'session',
        scope: 'workspace',
        workspaceId,
        workspacePath,
        sessionId: resolvedSessionId,
      }
    : { kind: 'session', scope: 'personal', sessionId: resolvedSessionId });
  if (!navigation) {
    throw new Error('当前已有会话导航操作正在进行');
  }
  await waitForSessionNavigation(navigation);
  return { workspaceId, workspacePath, sessionId: resolvedSessionId };
}

async function createBrowserPaneForTarget(
  target: ResolvedBrowserTarget,
  url: string,
): Promise<void> {
  const browserSession = await createBrowserSession(
    target.workspaceId,
    target.sessionId,
    target.workspacePath,
  );
  const tab = await createBrowserTab(browserSession.browserSessionId, url);
  openBrowserTab(browserSession.browserSessionId, tab.tabId, {
    workspaceId: target.workspaceId,
    workspacePath: target.workspacePath,
    sessionId: target.sessionId,
    label: i18n.t('browser.tab.new'),
    lifecycle: tab.lifecycle,
    url: tab.url,
    navigationRevision: tab.navigationRevision,
  });
}

/**
 * 在右栏打开一个网页地址（消息里的链接、代码块里的 URL）。
 *
 * 同一 URL 复用已有 Tab，重复点击不叠加新页面。
 */
export function openUrlInRightPaneBrowser(
  rawUrl: string,
  binding: BrowserOpenBinding = {},
): Promise<RightPaneBrowserOpenOutcome> {
  const url = rawUrl.trim();
  if (!url) return Promise.resolve('rejected');
  return enqueue(async (): Promise<RightPaneBrowserOpenOutcome> => {
    const availability = await inAppBrowserAvailable();
    if (availability.kind === 'external') return 'fallback-external';
    if (availability.kind === 'blocked') {
      reportInAppBrowserUnavailable();
      return 'rejected';
    }
    try {
      const target = await resolveBrowserTarget(binding);
      const existing = findBrowserTabForUrl(target, url);
      if (existing) {
        setActiveRightPaneTab(existing.scopeKey, existing.tab.id);
        setRightPaneCollapsed(existing.scopeKey, false);
        return 'opened';
      }
      await createBrowserPaneForTarget(target, url);
      return 'opened';
    } catch (error) {
      reportBrowserOpenFailure(error);
      return 'rejected';
    }
  });
}

/**
 * 在右栏把会话或项目 HTML 文件作为可运行网页打开。
 *
 * 项目 / 会话由调用方给出（文件树与变更卡片知道自己属于哪个会话），
 * 缺失时按 URL 链接同样的规则物化会话，保证预览页始终挂在一个真实 Magi
 * 会话上；没有内置浏览器时不做系统浏览器兜底（HTML 文件不是网址）。
 */
export function openHtmlFileInRightPaneBrowser(
  input: HtmlFileBrowserTarget,
): Promise<RightPaneBrowserOpenOutcome> {
  const filepath = input.filepath.trim();
  const workspaceId = trim(input.workspaceId);
  const workspacePath = trim(input.workspacePath);
  const sessionId = trim(input.sessionId);
  if (!filepath || (!workspaceId && !workspacePath && !sessionId)) {
    return Promise.resolve('rejected');
  }
  return enqueue(async (): Promise<RightPaneBrowserOpenOutcome> => {
    const availability = await inAppBrowserAvailable();
    if (availability.kind !== 'desktop') {
      reportInAppBrowserUnavailable();
      return 'rejected';
    }
    try {
      const target = await resolveBrowserTarget({ workspaceId, workspacePath, sessionId });
      const url = agentUrl('/api/files/site-open', buildFilePreviewQuery(filepath, {
        sourceFolderId: input.sourceFolderId,
        sessionId: target.sessionId,
        workspaceId: target.workspaceId,
        workspacePath: target.workspacePath,
      }));
      await createBrowserPaneForTarget(target, url);
      return 'opened';
    } catch (error) {
      reportBrowserOpenFailure(error);
      return 'rejected';
    }
  });
}
