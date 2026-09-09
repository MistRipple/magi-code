import {
  activateBrowserTab,
  getBrowserSession,
  getCurrentBrowserSession,
  setActiveBrowserTab,
  type BrowserSessionSnapshot,
  type BrowserTabSnapshot,
} from './agent-api';

export interface BrowserAuthorityContext {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

export interface BrowserAuthorityActivationTarget {
  browserSessionId: string;
  tabId: string;
  lifecycle: 'creating' | 'ready' | 'suspended' | 'crashed';
}

type CurrentSessionSnapshotConsumer = (snapshot: BrowserSessionSnapshot | null) => void;
type SessionSnapshotConsumer = (snapshot: BrowserSessionSnapshot) => void;

export interface BrowserAuthorityTabResolution {
  snapshot: BrowserSessionSnapshot;
  tab: BrowserTabSnapshot | null;
}

// BrowserAuthority 的读写共享同一条 promise lane。Renderer、右栏组件和事件恢复
// 可以同时提出意图，但不会再让旧快照或旧激活请求穿插覆盖新的权威状态。
let authorityLane: Promise<void> = Promise.resolve();

function enqueue<T>(operation: () => Promise<T>): Promise<T> {
  const result = authorityLane.then(operation, operation);
  authorityLane = result.then(() => undefined, () => undefined);
  return result;
}

export function synchronizeBrowserAuthority(
  context: BrowserAuthorityContext,
  consumer: CurrentSessionSnapshotConsumer,
): Promise<BrowserSessionSnapshot | null> {
  return enqueue(async () => {
    const snapshot = await getCurrentBrowserSession(
      context.workspaceId,
      context.sessionId,
      context.workspacePath,
    );
    consumer(snapshot);
    return snapshot;
  });
}

/**
 * 为 Desktop Surface 激活准备 BrowserAuthority。
 *
 * 逻辑 Tab 恢复和运行时工具焦点先进入同一条 Authority lane，随后调用方才
 * 允许物化 Main 的真实 Chromium Surface。creating Tab 不在这里重复 restore，
 * 它的创建事务已经由 API 的 CreatePage 负责；否则会出现两个 Host 物化请求。
 */
export function prepareBrowserAuthorityForDesktop(
  target: BrowserAuthorityActivationTarget,
  consumer: SessionSnapshotConsumer,
  options: { forceRestore?: boolean } = {},
): Promise<BrowserSessionSnapshot> {
  return enqueue(async () => {
    let snapshot: BrowserSessionSnapshot;
    if (
      options.forceRestore === true
      || target.lifecycle === 'suspended'
      || target.lifecycle === 'crashed'
    ) {
      snapshot = await activateBrowserTab(target.tabId);
    }
    snapshot = await setActiveBrowserTab(target.browserSessionId, target.tabId);
    consumer(snapshot);
    return snapshot;
  });
}

export function loadBrowserAuthoritySession(
  browserSessionId: string,
  consumer: SessionSnapshotConsumer,
): Promise<BrowserSessionSnapshot> {
  return enqueue(async () => {
    const snapshot = await getBrowserSession(browserSessionId);
    consumer(snapshot);
    return snapshot;
  });
}

/**
 * 读取当前 Browser Tab 的权威恢复资料。
 *
 * Renderer 的右栏投影只负责表达用户意图，不能把旧的 about:blank 或旧
 * navigation revision 传给 Electron。激活前必须从 Authority 读取同一 Tab
 * 的持久化快照；读取与后续恢复命令共用 authority lane，避免重启期间旧
 * 投影与新快照交错覆盖。
 */
export function loadBrowserAuthorityTab(
  browserSessionId: string,
  tabId: string,
): Promise<BrowserAuthorityTabResolution> {
  return enqueue(async () => {
    const snapshot = await getBrowserSession(browserSessionId);
    return {
      snapshot,
      tab: snapshot.tabs.find((candidate) => candidate.tabId === tabId) ?? null,
    };
  });
}
