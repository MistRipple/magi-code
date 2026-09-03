import {
  activateBrowserTab,
  getBrowserSession,
  getCurrentBrowserSession,
  setActiveBrowserTab,
  type BrowserSessionSnapshot,
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
): Promise<BrowserSessionSnapshot> {
  return enqueue(async () => {
    let snapshot: BrowserSessionSnapshot;
    if (target.lifecycle === 'suspended' || target.lifecycle === 'crashed') {
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
