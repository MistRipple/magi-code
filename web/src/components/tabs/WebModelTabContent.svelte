<script lang="ts">
  /**
   * 应用级 GPT Web 内容槽（设计基线 A25、§5.2）。
   *
   * 与普通 Browser Tab 的关键区别：
   * - 同一内容槽承载**多个宿主**：一个主页 + 每个活跃对话实例一个推理页面
   *   （数量 ≤ 并发上限，不设预热池）；
   * - 所有宿主**始终挂载**：折叠右栏与关闭视图只改可见性，不卸载组件、
   *   不销毁任何 `<webview>`，因此后台推理不中断；
   * - 分区、注册与尺寸同步全部复用 `BrowserTabContent` 的既有实现，
   *   不引入第二套 guest 生命周期。
   */
  import BrowserTabContent from './BrowserTabContent.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { addToast } from '../../stores/messages.svelte';
  import { tick } from 'svelte';
  import {
    ensureWebModelHomeSurface,
    resetWebModelConversation,
    resetWebModels,
    type BrowserTabLifecycle,
  } from '../../web/agent-api';
  import {
    webModelRuntimeEntries,
    webModelSentMessages,
    webModelTakeoverPending,
  } from '../../stores/web-model-runtime.svelte';

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
    /**
     * 当前窗口是否承载应用级推理（A24 / §5.13）。
     *
     * 产品目前只创建一个窗口入口，因此默认 `true`；宿主补上第二个窗口入口后，
     * 非 Primary 窗口传 `false`，这里显示「打开主窗口继续」而不是复制第二份
     * 用于推理的 Surface。
     */
    primaryWindow?: boolean;
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
    primaryWindow = true,
    onTitleChange,
  }: Props = $props();

  function hostVisible(tabId: string): boolean {
    return active && tabId === activeHostTabId;
  }

  let busy = $state(false);
  let actionError = $state('');
  let homeSurfaceKey = $state('');
  let homeSurfaceInFlight = false;
  /**
   * 后台推理中的对话实例数量。
   *
   * 内容槽里除主页外每个宿主就是一个活跃对话实例（§5.2 不设预热池，空闲页面会被
   * 释放），因此这个数量就是「正在后台继续的推理页面」数，不需要第二份计数事实。
   */
  const runningHostCount = $derived(
    sessionId
      ? webModelRuntimeEntries(sessionId).filter((entry) => entry.active !== false).length
      : 0,
  );
  /**
   * 该会话已发出的账号消息条数（A16）。
   *
   * 口径是「真实发送次数」，不是 token：Web 引擎不参与会话 token 预算，
   * 因此这里单独展示并显式标注（§5.1、§9.3 #14）。
   */
  const sentMessages = $derived(sessionId ? webModelSentMessages(sessionId) : 0);
  /** 已接管提示：绑定转成 user_owned / invalidated 时显示（§5.8）。 */
  const takeoverPending = $derived(sessionId ? webModelTakeoverPending(sessionId) : false);
  const canAct = $derived(Boolean(desktopSurface) && active && !busy);

  /**
   * 恢复后主页 Tab 只有逻辑身份，真实 guest 要等本组件挂载完成后才存在。
   * 这里走 daemon 的 App 级不激活恢复路径；不能把它替换成普通 Browser Tab
   * 的 activate，否则打开 GPT Web 会抢走用户当前右栏。
   */
  $effect(() => {
    if (!desktopSurface || !primaryWindow || !browserSessionId || homeSurfaceInFlight) return;
    const home = hosts.find((host) => host.tabId.endsWith('web-model-home')) ?? hosts[0];
    if (!home) return;
    const key = `${browserSessionId}\u0000${home.tabId}\u0000${home.navigationRevision}`;
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
        console.warn('[WebModelTab] 恢复 GPT Web 主页失败:', error);
      })
      .finally(() => {
        homeSurfaceInFlight = false;
      });
  });

  /** 「重置为 Magi 对话」（§5.8、工单 3.11）：只推进当前线程的绑定 epoch。 */
  async function resetConversation(): Promise<void> {
    const id = sessionId?.trim() || '';
    if (!id || busy) return;
    const confirmText = `${i18n.t('webModel.reset.confirmTitle')}\n${i18n.t('webModel.reset.confirmBody')}`;
    if (typeof window !== 'undefined' && !window.confirm(confirmText)) return;
    busy = true;
    actionError = '';
    try {
      await resetWebModelConversation(id);
      addToast('success', i18n.t('webModel.reset.done'));
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
      console.warn('[WebModelTab] 重置 Web 对话失败:', error);
    } finally {
      busy = false;
    }
  }

  /**
   * 「清除数据」（工单 3.11）：先让 daemon 撤销在飞回复并隐藏 Web 模型，再按应用级
   * 分区粒度清登录态。顺序固定，不能反过来——否则清理期间还会有渲染请求落进旧页面。
   */
  async function clearData(): Promise<void> {
    if (busy) return;
    const desktop = window.magiDesktop;
    const confirmText = i18n.t('webModel.clearData.confirmBody');
    if (!desktop?.clearWebModelData) {
      addToast('warning', i18n.t('settings.browser.webModel.needDesktop'));
      return;
    }
    if (typeof window !== 'undefined' && !window.confirm(confirmText)) return;
    busy = true;
    actionError = '';
    try {
      await resetWebModels();
      await desktop.clearWebModelData();
      addToast('success', i18n.t('webModel.clearData.done'));
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
      console.warn('[WebModelTab] 清除 GPT Web 数据失败:', error);
    } finally {
      busy = false;
    }
  }
</script>

<div
  class="web-model-content"
  class:web-model-content--offscreen={!active}
  data-web-model-session={browserSessionId}
  aria-hidden={!active}
>
  <!--
    应用级动作条：只放「后台推理中」这个共享事实与两个显式动作（§5.8、工单 3.11）。
    它不复制任何 daemon 事实：数量由内容槽里的宿主数派生，动作全部是显式命令。
  -->
  <div class="web-model-bar" data-magi-surface="toolbar">
    {#if runningHostCount > 0}
      <span class="bar-status" data-web-model-running-count={runningHostCount}>
        {i18n.t('webModel.background.running')} · {runningHostCount}
      </span>
    {/if}
    {#if sentMessages > 0}
      <span class="bar-quota" data-web-model-sent-messages={sentMessages}>
        {i18n.t('webModel.quota.sentCount', { count: sentMessages })}
      </span>
    {/if}
    <span class="bar-spacer"></span>
    <button
      type="button"
      class="bar-action"
      disabled={!canAct || !sessionId}
      title={i18n.t('webModel.action.resetConversation')}
      onclick={() => void resetConversation()}
    >{i18n.t('webModel.action.resetConversation')}</button>
    <button
      type="button"
      class="bar-action"
      data-web-model-clear-data="1"
      disabled={!canAct}
      title={i18n.t('webModel.action.clearData')}
      onclick={() => void clearData()}
    >{i18n.t('webModel.action.clearData')}</button>
  </div>
  {#if actionError}
    <p class="bar-error" role="alert">{actionError}</p>
  {/if}
  {#if takeoverPending}
    <p class="bar-notice" data-web-model-takeover="1" role="status">
      {i18n.t('webModel.takeover.notice')}
    </p>
  {/if}
  {#if !primaryWindow}
    <!--
      非 Primary 窗口不复制用于推理的 Surface（A24）：只提示去哪里继续。
      宿主目前只有一个窗口入口，这段在出现第二个入口前不会被渲染。
    -->
    <p class="bar-notice bar-notice--secondary" role="status">
      {i18n.t('webModel.window.secondaryOnly')}
    </p>
  {/if}
  {#each primaryWindow ? hosts : [] as host (host.tabId)}
    <div
      class="host"
      class:active={hostVisible(host.tabId)}
      class:host--offscreen={!hostVisible(host.tabId)}
      aria-hidden={!hostVisible(host.tabId)}
    >
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
     折叠右栏与关闭视图只改 `hidden`，不卸载组件（A25）。 */
  .web-model-content {
    position: absolute;
    inset: 0;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /*
    隐藏必须是「移出可见区域」，不能是 `display: none`（A25、R49）。
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
    height: 26px;
    padding: 0 8px;
    font-size: 11px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
    border-bottom: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
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
    top: 26px;
    left: 8px;
    right: 8px;
    z-index: 2;
    margin: 0;
    padding: 2px 0;
    font-size: 11px;
    color: var(--foreground-muted);
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .bar-action {
    padding: 1px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 11px;
    background: transparent;
    cursor: pointer;
  }

  .bar-action:disabled {
    cursor: default;
    opacity: 0.45;
  }

  .bar-error {
    position: absolute;
    top: 26px;
    left: 8px;
    right: 8px;
    z-index: 2;
    margin: 0;
    font-size: 11px;
    color: var(--danger, #c53f4f);
  }

  .host {
    position: absolute;
    top: 26px;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /* 非当前显示宿主同样只离屏，不卸载、不塌缩尺寸：每个活跃对话实例都要
     保持可注册、可继续生成（§5.2 多宿主、A25）。 */
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
