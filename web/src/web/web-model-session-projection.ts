import {
  getAppBrowserSession,
  getWebModelRuntime,
  type BrowserSessionSnapshot,
  type BrowserTabSnapshot,
} from './agent-api';
import { applyWebModelRuntime } from '../stores/web-model-runtime.svelte';
import {
  synchronizeWebModelAppSession,
  type WebModelHost,
} from '../stores/right-pane.svelte';
import { WEB_MODEL_HOME_TAB_ID } from '../shared/web-model';

/**
 * 应用级 GPT Web 会话 → 右栏 `appTabs` / 内容槽宿主的**唯一投影点**。
 *
 * 设计依据：设计基线 A25、§5.2、§5.4。
 *
 * 为什么必须投影宿主而不是只投影视图指针：推理页面由推理通道在应用级会话里
 * 按需创建（`web-model-page-*`）。`<webview>` guest 只有被 Renderer 挂载后
 * 才存在；guest 不存在时 Desktop 侧拿不到 content-slot binding，App 级命令
 * 会以 `browser_surface_content_slot_unavailable` 失败（R49）。因此内容槽必须
 * 承载「主页 + 每个活跃对话实例一个推理页面」，且全部保持挂载。
 *
 * 只读：`GET /browser/sessions/app` 不创建会话；未打开过 GPT Web 时投影为空，
 * 不会让每个用户都在后台多一份 ChatGPT 主页。
 */
export async function projectWebModelAppSession(): Promise<boolean> {
  let session: BrowserSessionSnapshot | null = null;
  try {
    // 运行态投影与宿主投影同一次刷新：阶段 / 排队位置只影响运行指示行，
    // 失败时清空即可，绝不阻塞宿主投影（§3.12）。
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
  const inferenceHosts = session.tabs
    // Suspended pages are deliberately released by the bridge after an idle
    // turn.  They remain in BrowserAuthority as recoverable logical records,
    // but must not be mounted as live guest hosts; otherwise the renderer can
    // select an about:blank/suspended page and report a false connection state.
    .filter((tab) => (
      tab.tabId !== homeTab.tabId
      && tab.lifecycle !== 'suspended'
      && tab.lifecycle !== 'closed'
    ))
    .map(toWebModelHost);
  synchronizeWebModelAppSession(session.browserSessionId, homeHost, [
    homeHost,
    ...inferenceHosts,
  ]);
  return true;
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
