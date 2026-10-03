import {
  getAppBrowserSession,
  getWebModelRuntime,
  probeWebModel,
  type WebModelProbeResponse,
  type BrowserSessionSnapshot,
  type BrowserTabSnapshot,
} from './agent-api';
import { applyWebModelProbeStatus, applyWebModelRuntime } from '../stores/web-model-runtime.svelte';
import {
  synchronizeWebModelAppSession,
  type WebModelHost,
} from '../stores/right-pane.svelte';
import { WEB_MODEL_HOME_TAB_ID } from '../shared/web-model';
import { composerWorkspaceState } from '../stores/composer-workspace.svelte';
import { navigateSession } from '../shared/session-navigation.svelte';
import type { WebModelRuntimeEntry } from './agent-api';

/**
 * 应用级 GPT Web 会话 → 右栏 `appTabs` / 内容槽宿主的**唯一投影点**。
 *
 *
 * GPT Web 产品只允许一个活跃会话和一个 WebView。主页 Tab 同时就是当前
 * 临时/已保存对话的页面；不能按 Magi 会话或每轮推理创建额外宿主。
 *
 * 只读：`GET /browser/sessions/app` 不创建会话；未打开过 GPT Web 时投影为空，
 * 不会让每个用户都在后台多一份 ChatGPT 主页。
 */
export async function projectWebModelAppSession(): Promise<boolean> {
  let session: BrowserSessionSnapshot | null = null;
  try {
    // 运行态投影与宿主投影同一次刷新：阶段 / 排队位置只影响运行指示行，
    // 失败时清空即可，绝不阻塞宿主投影。
    await refreshWebModelRuntime();
    session = await getAppBrowserSession();
  } catch (error) {
    console.warn('[web-model] 读取应用级浏览器会话失败:', error);
    return false;
  }
  const homeTab = session?.tabs.find((tab) => tab.tabId === WEB_MODEL_HOME_TAB_ID) ?? null;
  if (!session || !homeTab) {
    synchronizeWebModelAppSession(null, null);
    return false;
  }
  const homeHost = toWebModelHost(homeTab);
  synchronizeWebModelAppSession(session.browserSessionId, homeHost, [homeHost]);
  void probeWebModelAfterRestore();
  return true;
}

let restoreProbeStarted = false;
const RESTORE_PROBE_ATTEMPTS = 5;
const RESTORE_PROBE_DELAY_MS = 2500;

/**
 * 重启后恢复 GPT Web 入口：daemon 的探测结论只在进程内存里，重启即清空，而登录态在持久
 * 分区里仍然有效。应用级主页宿主被投影出来后（说明用户用过 GPT Web），自动做一次只读探测，
 * 让已登录的用户不必每次重启都去设置页点「连接」。
 *
 * 宿主 guest 要等内容槽挂载并完成注册才可读，所以对「宿主暂不可用 / 命令层失败」做有限次
 * 重试；任何确定的结论（含未登录）都立即停止，绝不用旧结果占位。
 */
async function probeWebModelAfterRestore(): Promise<void> {
  if (restoreProbeStarted) return;
  restoreProbeStarted = true;
  let reconnectTried = false;
  for (let attempt = 0; attempt < RESTORE_PROBE_ATTEMPTS; attempt += 1) {
    try {
      // 第一次探测失败后改走一次重连（重启自动化 worker）：失败多半是 worker 持有过期的页面绑定，
      // 一直探测同一个过期绑定没有意义。重连只做一次，且不受失败退避限制。
      if (attempt >= 1 && !reconnectTried) {
        reconnectTried = true;
        const reconnected = await reconnectWebModel();
        if (reconnected.status !== 'desktop_unavailable' && reconnected.status !== 'failed') return;
      }
      if (!webModelAutoProbeAllowed()) {
        await new Promise((resolve) => setTimeout(resolve, RESTORE_PROBE_DELAY_MS));
        continue;
      }
      const result = await runWebModelProbe();
      if (result.status !== 'desktop_unavailable' && result.status !== 'failed') return;
    } catch (error) {
      console.warn('[web-model] 重启后探测 GPT Web 失败:', error);
    }
    await new Promise((resolve) => setTimeout(resolve, RESTORE_PROBE_DELAY_MS));
  }
}

/**
 * 只读刷新 Web 模型运行态投影（阶段 / 排队位置 / 发送条数）。
 *
 * 与宿主投影分开成函数，是为了让调用方能在失败时只清空运行态而不影响宿主：
 * 宿主丢了会中断推理，运行态丢了只是少一行提示。
 */
export async function refreshWebModelRuntime(): Promise<void> {
  try {
    applyWebModelRuntime(await getWebModelRuntime());
  } catch (error) {
    console.warn('[web-model] 读取 Web 模型运行态失败:', error);
    applyWebModelRuntime(null);
  }
}

/**
 * 页面标题只用于 Tab 条展示；宿主身份只由 `tabId` 决定，不复制 Authority 事实。
 */
function toWebModelHost(tab: BrowserTabSnapshot): WebModelHost {
  return {
    tabId: tab.tabId,
    lifecycle: tab.lifecycle,
    url: tab.url,
    navigationRevision: tab.navigationRevision,
  };
}

/**
 * 唯一的探测入口：把 daemon 的探测结论同时镜像给提示条。设置页、失败卡片动作、重启恢复、
 * 登录完成后的自动探测都走这里，避免各处各记一份状态。
 */
/**
 * 「重连」：先让 daemon 重新对齐页面绑定再探测；仍失败说明自动化 worker 持有的页面绑定已过期
 * （例如 Renderer 刷新后 `<webview>` 被重建），此时重启 worker（只重启自动化进程，不动页面、
 * 不动对话、不动登录态）再探测一次。
 */
export async function reconnectWebModel(): Promise<WebModelProbeResponse> {
  let result = await runWebModelProbe();
  if (result.status !== 'failed' && result.status !== 'desktop_unavailable') return result;
  const desktop = typeof window !== 'undefined' ? window.magiDesktop : undefined;
  if (desktop?.restartBrowserAutomation) {
    await desktop.restartBrowserAutomation();
    await new Promise((resolve) => setTimeout(resolve, 1500));
    result = await runWebModelProbe();
  }
  return result;
}

let probeInFlight: Promise<WebModelProbeResponse> | null = null;
let lastFailedProbeAt = 0;
/** 命令层失败（页面 guest 尚未就绪等）后的自动探测退避：不能连续冲击宿主。 */
const AUTO_PROBE_FAILURE_BACKOFF_MS = 15_000;

export function webModelAutoProbeAllowed(): boolean {
  return Date.now() - lastFailedProbeAt >= AUTO_PROBE_FAILURE_BACKOFF_MS;
}

export function runWebModelProbe(): Promise<WebModelProbeResponse> {
  // 单飞：同一时刻只有一次探测在途，并发调用方共享结果。
  if (probeInFlight) return probeInFlight;
  probeInFlight = (async () => {
    try {
      const result = await probeWebModel();
      if (result.status === 'failed') lastFailedProbeAt = Date.now();
      else applyWebModelProbeStatus(result.status, result.reason);
      return result;
    } finally {
      probeInFlight = null;
    }
  })();
  return probeInFlight;
}

/** 「转到占用会话」：按 daemon 投影的占用者（会话 + 项目）导航。找不到项目时按个人会话处理。 */
export function goToSlotOwnerSession(owner: Pick<WebModelRuntimeEntry, 'sessionId' | 'workspaceId'>): boolean {
  const workspaceId = owner.workspaceId?.trim() || '';
  const workspace = workspaceId
    ? composerWorkspaceState.workspaces.find((item) => item.workspaceId === workspaceId)
    : undefined;
  const transaction = navigateSession(
    workspace
      ? {
          kind: 'session',
          scope: 'workspace',
          workspaceId: workspace.workspaceId,
          workspacePath: workspace.rootPath,
          sessionId: owner.sessionId,
        }
      : { kind: 'session', scope: 'personal', sessionId: owner.sessionId },
  );
  return transaction !== null;
}
