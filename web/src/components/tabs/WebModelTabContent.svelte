<script lang="ts">
  /**
   * 应用级 GPT Web 内容槽。
   *
   * 与普通 Browser Tab 的关键区别：
   * - 同一内容槽只承载一个应用级 WebView；临时/已保存对话都复用它；
   * - 宿主**始终挂载**：折叠右栏与关闭视图只改可见性，不卸载组件、
   *   不销毁 `<webview>`，因此后台推理不中断；
   * - 分区、注册与尺寸同步全部复用 `BrowserTabContent` 的既有实现，
   *   不引入第二套 guest 生命周期。
   */
  import BrowserTabContent from './BrowserTabContent.svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { addToast } from '../../stores/messages.svelte';
  import { tick } from 'svelte';
  import {
    ensureWebModelHomeSurface,
    navigateWebModelPage,
    reloadWebModelPage,
    type BrowserTabLifecycle,
    type WebModelPageTarget,
  } from '../../web/agent-api';
  import {
    webModelActiveTurnCount,
    markWebModelStoppedByUser,
    webModelProbeStatus,
    webModelStoppedByUser,
    webModelSentMessages,
    webModelSlotOwner,
  } from '../../stores/web-model-runtime.svelte';
  import { goToSlotOwnerSession, reconnectWebModel, runWebModelProbe, webModelAutoProbeAllowed } from '../../web/web-model-session-projection';

  export interface WebModelHost {
    tabId: string;
    lifecycle: BrowserTabLifecycle;
    url: string;
    navigationRevision: number;
  }

  interface Props {
    browserSessionId: string;
    hosts: WebModelHost[];
    /** 当前显示的宿主（主页或当前会话线程的推理页面）。 */
    activeHostTabId: string | null;
    /** 应用级视图当前是否可见；false 时全部宿主隐藏但保持挂载。 */
    active?: boolean;
    workspaceId?: string;
    workspacePath?: string;
    sessionId?: string;
    desktopSurface?: boolean;
    onTitleChange?: (label: string) => void;
  }

  let {
    browserSessionId,
    hosts,
    activeHostTabId,
    active = true,
    workspaceId,
    workspacePath,
    sessionId,
    desktopSurface,
    onTitleChange,
  }: Props = $props();

  function hostVisible(tabId: string): boolean {
    return active && tabId === activeHostTabId;
  }

  let actionError = $state('');
  let homeSurfaceKey = $state('');
  let homeSurfaceInFlight = false;
  /** 恢复失败后的退避：持续失败时不能随每次投影刷新都重试。 */
  let homeSurfaceRetryAfter = 0;
  /** 单槽位：是否有 GPT Web 推理正在后台进行（视图被隐藏时仍然可见）。 */
  const running = $derived(webModelActiveTurnCount() > 0);
  /** 当前槽位被哪个会话占用；占用者不是当前会话时提示「被其他会话占用」。 */
  const slotOwnerSessionId = $derived(webModelSlotOwner()?.sessionId ?? '');
  /**
   * 该会话已发出的账号消息条数。
   *
   * 口径是「真实发送次数」，不是 token：Web 引擎不参与会话 token 预算，
   * 因此这里单独展示并显式标注。
   */
  const sentMessages = $derived(sessionId ? webModelSentMessages(sessionId) : 0);
  /**
   * 标题栏的「检查连接」：用户打开标签、看到页面是登录状态就直接开始使用——这里一键确认 Magi 与页面
   * 的连接是否真的成功，不成功就重新对齐 / 重连。只重新登记并读取当前页面，不导航、不改变对话。
   */
  let checking = $state(false);
  async function checkConnection(): Promise<void> {
    if (checking) return;
    checking = true;
    actionError = '';
    try {
      let result = await runWebModelProbe();
      if (result.status === 'failed' || result.status === 'desktop_unavailable') {
        // 连接未成功：重新物化 / 对齐页面绑定，仍失败则重启自动化 worker 重连。
        await ensureWebModelHomeSurface().catch(() => undefined);
        result = await reconnectWebModel();
      }
      addToast(
        result.status === 'ok' ? 'success' : 'warning',
        i18n.t(`webModel.check.result.${result.status}`),
        undefined,
        { forceVisible: true },
      );
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      checking = false;
    }
  }
  const connectionStatus = $derived(webModelProbeStatus());
  const homeUrl = $derived(hosts[0]?.url ?? '');
  const homeHost = $derived.by(() => {
    try {
      return new URL(homeUrl).host;
    } catch {
      return '';
    }
  });
  /**
   * 顶部快捷地址：对话页 / OpenAI 平台 Tunnels / Runtime API keys。当前停在哪一页，就只显示另外两个，
   * 用户不必找地址，也不用离开 GPT Web 标签（登录态共用同一个页面）。
   */
  const PAGE_TARGETS: WebModelPageTarget[] = ['chat', 'tunnels', 'api_keys'];
  const currentPageTarget = $derived.by((): WebModelPageTarget | null => {
    try {
      const url = new URL(homeUrl);
      if (url.hostname === 'platform.openai.com') {
        if (url.pathname.includes('/tunnels')) return 'tunnels';
        if (url.pathname.includes('/api-keys')) return 'api_keys';
        return null;
      }
      return 'chat';
    } catch {
      return null;
    }
  });
  const quickTargets = $derived(PAGE_TARGETS.filter((target) => target !== currentPageTarget));
  let navigating = $state(false);
  async function goToPage(target: WebModelPageTarget): Promise<void> {
    if (navigating) return;
    navigating = true;
    actionError = '';
    try {
      await navigateWebModelPage(target);
      // 状态芯片跟着页面走：切页后立刻重新探测（daemon 探测自带页面加载等待），不等自动防抖探测。
      void runWebModelProbe().catch(() => undefined);
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      navigating = false;
    }
  }
  /**
   * 手动刷新：页面卡住或加载失败（`browser_navigation_timeout`）时，标题栏没有任何地址栏 / 刷新入口，
   * 用户只能重启应用。刷新只重新加载当前页面，不换页面、不改变对话，所以进行中也可用。
   */
  let reloading = $state(false);
  async function reloadPage(): Promise<void> {
    if (reloading) return;
    reloading = true;
    actionError = '';
    try {
      await reloadWebModelPage();
      void runWebModelProbe().catch(() => undefined);
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      reloading = false;
    }
  }
  const ownerTitle = $derived(webModelSlotOwner()?.sessionTitle?.trim() || '');
  /** 同一时刻只显示一条提示：错误 > 未登录 > 只读（占用）。 */
  const notice = $derived.by((): { kind: 'error' | 'login' | 'readonly'; text: string; action?: { label: string; run: () => void } } | null => {
    if (actionError) return { kind: 'error', text: actionError };
    if (showLoginHint) return { kind: 'login', text: i18n.t('webModel.login.hint') };
    if (readOnlyForOtherOwner) {
      return {
        kind: 'readonly',
        text: i18n.t('webModel.slot.readOnlyOther', { session: ownerTitle ? `「${ownerTitle}」` : '' }),
        action: {
          label: i18n.t('webModel.slot.goToOwner'),
          run: () => {
            const owner = webModelSlotOwner();
            if (owner) goToSlotOwnerSession(owner);
          },
        },
      };
    }
    return null;
  });

  /** 非拥有者会话打开 GPT Web 只能查看：页面只读并显示当前占用者。 */
  const readOnlyForOtherOwner = $derived(
    Boolean(slotOwnerSessionId) && slotOwnerSessionId !== (sessionId?.trim() || ''),
  );
  /** 未登录时的持久提示条：直到登录成功（探测结论变化）才消失。 */
  const showLoginHint = $derived(webModelProbeStatus() === 'login_required');

  /**
   * 登录完成后自动重新探测：用户在页面里登录后，页面会导航；对主页宿主的 URL / 导航代次变化
   * 做防抖探测，让选择器入口在登录后自动出现，不必再回设置页手点。只读探测，且推理进行中不做。
   */
  let loginProbeTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const home = hosts[0];
    if (!home || !desktopSurface || !active) return;
    void `${home.url}\u0000${home.navigationRevision}`;
    if (loginProbeTimer) clearTimeout(loginProbeTimer);
    loginProbeTimer = setTimeout(() => {
      loginProbeTimer = null;
      if (webModelActiveTurnCount() > 0 || !webModelAutoProbeAllowed()) return;
      void runWebModelProbe().catch(() => {});
    }, 2000);
    return () => {
      if (loginProbeTimer) clearTimeout(loginProbeTimer);
    };
  });

  /**
   * 恢复后主页 Tab 只有逻辑身份，真实 guest 要等本组件挂载完成后才存在。
   * 这里走 daemon 的 App 级不激活恢复路径；不能把它替换成普通 Browser Tab
   * 的 activate，否则打开 GPT Web 会抢走用户当前右栏。
   */
  $effect(() => {
    // 「停止 / 退出」之后页面保持销毁，直到用户再次打开 GPT Web 视图。
    if (active) markWebModelStoppedByUser(false);
    else if (webModelStoppedByUser()) return;
    if (!desktopSurface || !browserSessionId || homeSurfaceInFlight) return;
    if (Date.now() < homeSurfaceRetryAfter) return;
    const home = hosts.find((host) => host.tabId.endsWith('web-model-home')) ?? hosts[0];
    if (!home) return;
    const key = `${browserSessionId}\u0000${home.tabId}\u0000${home.navigationRevision}\u0000${home.lifecycle}`;
    if (key === homeSurfaceKey) return;
    homeSurfaceInFlight = true;
    void tick()
      .then(() => ensureWebModelHomeSurface())
      .then(() => {
        homeSurfaceKey = key;
      })
      .catch((error) => {
        // 组件会在下一次 Authority / Desktop 快照变化后再次尝试；一次
        // 恢复失败不能让 GPT Web Tab 永久停在 about:blank。
        homeSurfaceRetryAfter = Date.now() + 5000;
        console.warn('[WebModelTab] 恢复 GPT Web 主页失败:', error);
      })
      .finally(() => {
        homeSurfaceInFlight = false;
      });
  });

</script>

<div
  class="web-model-content"
  class:web-model-content--offscreen={!active}
  data-web-model-session={browserSessionId}
  aria-hidden={!active}
  style:--wm-top={notice ? '72px' : '36px'}
>
  <!-- 应用级标题栏：只放共享事实（后台推理 / 发送条数 / 连接状态）与两个显式动作。「清除数据」是
       破坏性操作，只在设置里提供，不放在随手可点的标题栏。 -->
  <div class="web-model-bar" data-magi-surface="toolbar">
    <!-- 只读地址：让用户确认页面在 chatgpt.com 上，不可编辑、不可前进后退或刷新。 -->
    <span class="bar-address" title={homeUrl}><Icon name="globe" size={12} />{homeHost}</span>
    <button
      type="button"
      class="bar-action bar-icon"
      data-web-model-reload="1"
      disabled={!desktopSurface || reloading}
      title={i18n.t('webModel.reload.title')}
      aria-label={i18n.t('webModel.reload.title')}
      onclick={() => void reloadPage()}
    ><Icon name="refresh" size={12} /></button>
    {#each quickTargets as target (target)}
      <button
        type="button"
        class="bar-action bar-link"
        data-web-model-goto={target}
        disabled={!desktopSurface || navigating || running}
        title={i18n.t(`webModel.nav.${target}.title`)}
        onclick={() => void goToPage(target)}
      >{i18n.t(`webModel.nav.${target}.label`)}</button>
    {/each}
    {#if running}
      <span class="bar-status" data-web-model-running="1">
        {i18n.t('webModel.background.running')}
      </span>
    {/if}
    {#if sentMessages > 0}
      <span class="bar-quota" data-web-model-sent-messages={sentMessages}>
        {i18n.t('webModel.quota.sentCount', { count: sentMessages })}
      </span>
    {/if}
    <span class="bar-spacer"></span>
    <span
      class="bar-conn"
      class:ok={connectionStatus === 'ok'}
      class:bad={connectionStatus !== 'ok' && connectionStatus !== 'unknown'}
      data-web-model-connection={connectionStatus}
      title={i18n.t('webModel.check.title')}
    >
      <span class="bar-conn-dot"></span>{i18n.t(`webModel.check.state.${connectionStatus}`)}
    </span>
    <button
      type="button"
      class="bar-action"
      data-web-model-check="1"
      disabled={!desktopSurface || checking}
      title={i18n.t('webModel.check.title')}
      onclick={() => void checkConnection()}
    >{checking ? i18n.t('webModel.check.checking') : i18n.t('webModel.check.action')}</button>
  </div>
  <!-- 提示独占一行并把网页内容整体下移：不再盖在页面自带的工具栏上。 -->
  {#if notice}
    <p
      class="bar-notice"
      class:bar-notice--error={notice.kind === 'error'}
      data-web-model-notice={notice.kind}
      role={notice.kind === 'error' ? 'alert' : 'status'}
    >
      <span class="bar-notice-text">{notice.text}</span>
      {#if notice.action}
        <button type="button" class="bar-action" onclick={notice.action.run}>{notice.action.label}</button>
      {/if}
    </p>
  {/if}
  {#each hosts as host (host.tabId)}
    <div
      class="host"
      class:active={hostVisible(host.tabId)}
      class:host--offscreen={!hostVisible(host.tabId)}
      aria-hidden={!hostVisible(host.tabId)}
    >
      {#if readOnlyForOtherOwner}
        <div class="host-readonly-shield" aria-hidden="true"></div>
      {/if}
      <BrowserTabContent
        browserSessionId={browserSessionId}
        tabId={host.tabId}
        lifecycle={host.lifecycle}
        workspaceId={workspaceId}
        workspacePath={workspacePath}
        sessionId={sessionId}
        desktopSurface={desktopSurface}
        surfaceScope="app"
        onTitleChange={onTitleChange}
      />
    </div>
  {/each}
</div>

<style>
  /* 内容槽与每个宿主都绝对定位于 RightPane 的 `.right-pane-body`。
     折叠右栏与关闭视图只改 `hidden`，不卸载组件。 */
  .web-model-content {
    position: absolute;
    inset: 0;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /*
    隐藏必须是「移出可见区域」，不能是 `display: none`。
    appenders：`<webview>` guest 只有在宿主有真实布局尺寸时才能完成注册，
    注册是后台推理的唯一入口；同时离屏保留也让进行中的生成不中断。
  */
  .web-model-content--offscreen {
    transform: translate3d(-20000px, 0, 0);
    pointer-events: none;
  }

  .web-model-bar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 36px;
    padding: 0 8px;
    font-size: 11px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
    border-bottom: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
  }

  .bar-address {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.75;
  }

  .bar-spacer {
    flex: 1 1 auto;
  }

  .bar-status {
    opacity: 0.72;
  }

  .bar-quota {
    opacity: 0.72;
    font-variant-numeric: tabular-nums;
  }


  .bar-notice {
    position: absolute;
    top: 36px;
    left: 0;
    right: 0;
    z-index: 3;
    box-sizing: border-box;
    height: 36px;
    margin: 0;
    padding: 4px 8px;
    overflow: hidden;
    font-size: 11px;
    line-height: 14px;
    color: var(--foreground);
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .bar-notice-text {
    flex: 1 1 auto;
    min-width: 0;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .bar-notice .bar-action {
    flex: none;
  }

  .bar-notice--error {
    color: var(--danger, #c53f4f);
  }

  /* 只读：盖住宿主，拦截所有指针交互（页面仍可见，便于查看占用中的对话）。 */
  .host-readonly-shield {
    position: absolute;
    inset: 0;
    z-index: 2;
    cursor: not-allowed;
  }

  .bar-conn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    opacity: 0.8;
    white-space: nowrap;
  }

  .bar-conn-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--foreground-muted, rgba(127, 127, 127, 0.6));
  }

  .bar-conn.ok .bar-conn-dot {
    background: var(--success, #2f9e63);
  }

  .bar-conn.bad .bar-conn-dot {
    background: var(--warning, #d9962b);
  }

  .bar-action {
    padding: 1px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 11px;
    background: transparent;
    cursor: pointer;
  }

  .bar-link {
    flex: none;
  }

  .bar-icon {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 2px 6px;
  }
  .bar-action:disabled {
    cursor: default;
    opacity: 0.45;
  }


  .host {
    position: absolute;
    top: var(--wm-top, 36px);
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /* 非当前显示宿主同样只离屏，不卸载、不塌缩尺寸：每个活跃对话实例都要
     保持可注册、可继续生成。 */
  .host--offscreen {
    transform: translate3d(-20000px, 0, 0);
    pointer-events: none;
  }

  .web-model-content {
    /* 动作条与宿主共用一个内容槽：宿主从动作条下方开始，避免遮挡站点页面。 */
    position: absolute;
    inset: 0;
  }
</style>
