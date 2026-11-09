<script lang="ts">
import { rightPaneState, getRightPaneState, closeTab, setActiveRightPaneTab, updateRightPaneTabLabel, setRightPaneCollapsed, clearBrowserTabClosePending, markBrowserTabClosePending, type RightPaneTab, type CodeTabPayload, type AgentTabPayload, type BrowserTabPayload, type TerminalTabPayload, type PluginTabPayload, synchronizeBrowserSessionSnapshot, openTerminalTab, openPluginTab, openCodeTab, openAgentTab, openLauncherTab, LAUNCHER_TAB_ID } from '../stores/right-pane.svelte';
import { recentPaneItems, recordRecentPaneItem, forgetRecentPaneItem, recentCodeKey, recentWebKey, recordableWebUrl, type RecentPaneItem } from '../stores/right-pane-recent.svelte';
import { agentUrl, buildFilePreviewQuery, closeBrowserTab, closeTerminalSession, getBrowserCapabilities, getAgentChangeDiff, getAgentFilePreview, isPublicTunnelAccess, type BrowserCapabilitiesSnapshot } from './agent-api';
  import FileName from '../components/FileName.svelte';
  import FileTypeIcon from '../components/FileTypeIcon.svelte';
  import { resolveComposerSourceFolder } from '../stores/composer-workspace.svelte';
  import { onMount, tick } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import { getAgentBrandInfo, getAgentRunColor } from '../lib/agent-colors';
  import { selectLauncherAgents } from '../lib/active-agent-center';
  import { getAgentRunState, selectAgentRun } from '../stores/agent-run-store.svelte';
  import MarkdownContent from '../components/MarkdownContent.svelte';
  import DiffCodeBlock from '../components/blocks/DiffCodeBlock.svelte';
  import AgentTabContent from '../components/tabs/AgentTabContent.svelte';
  import BrowserTabContent from '../components/tabs/BrowserTabContent.svelte';
  import TerminalTabContent from '../components/tabs/TerminalTabContent.svelte';
  import PluginTabContent from '../components/tabs/PluginTabContent.svelte';
  import AgentListTabContent from '../components/tabs/AgentListTabContent.svelte';
  import LauncherTabContent, { type LauncherAgent, type LauncherTool } from '../components/tabs/LauncherTabContent.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { highlightCode } from '../lib/code-highlighter';
  import {
    isHtmlFile,
    isKnownBinaryFile,
    isMarkdownFile,
    isWordFile,
    isImageFile,
  } from '../lib/file-preview-utils';
  import { loadBrowserAuthoritySession } from './browser-authority-coordinator';
  import {
    openUrlInRightPaneBrowser,
    openHtmlFileInRightPaneBrowser,
  } from './right-pane-browser';
  import {
    pluginProjection,
    trackPluginProjection,
  } from '../stores/plugin-registry.svelte';
  import { openSettings } from '../stores/shell-ui.svelte';

  interface Props {
    workspaceRoot: string;
    overlay?: boolean;
    desktopSurface?: boolean;
    visible?: boolean;
    /** Main 声明的代理正在使用的页面；不可见时在离屏内容槽中保持挂载。 */
    agentRetainedSurfaces?: MagiDesktopBrowserSurfaceSnapshot[];
  }

  let {
    workspaceRoot,
    overlay = false,
    desktopSurface = false,
    visible = true,
    agentRetainedSurfaces = [],
  }: Props = $props();

  // ============ Tab 状态 ============
  const paneScopeKey = $derived(rightPaneState.activeScopeKey);
  const paneState = $derived(getRightPaneState(paneScopeKey));
  const openTabs = $derived(paneState.openTabs);

  const retainedTabIds = $derived(new Set(
    agentRetainedSurfaces
      .map((surface) => surface.tabId),
  ));
  type BrowserHostEntry = {
    tabId: string;
    paneTabId: string | null;
    payload: BrowserTabPayload;
    visible: boolean;
  };
  const browserHosts = $derived.by<BrowserHostEntry[]>(() => {
    const entries: BrowserHostEntry[] = openTabs
      .filter((tab) => tab.kind === 'browser')
      .map((tab) => ({
        tabId: (tab.payload as BrowserTabPayload).tabId,
        paneTabId: tab.id,
        payload: tab.payload as BrowserTabPayload,
        visible: tab.id === paneState.activeTabId && activeTab?.kind === 'browser',
      }));
    for (const surface of agentRetainedSurfaces) {
      if (!retainedTabIds.has(surface.tabId)) continue;
      if (entries.some((entry) => entry.tabId === surface.tabId)) continue;
      const projected = Object.values(rightPaneState.perSession)
        .flatMap((pane) => pane.openTabs)
        .find((tab) => (
          tab.kind === 'browser' && (tab.payload as BrowserTabPayload).tabId === surface.tabId
        ))?.payload as BrowserTabPayload | undefined;
      entries.push({
        tabId: surface.tabId,
        paneTabId: null,
        payload: projected ?? {
          browserSessionId: surface.browserSessionId,
          tabId: surface.tabId,
          lifecycle: 'ready',
          url: 'about:blank',
          navigationRevision: surface.navigationRevision,
          agentOccupied: true,
          sessionId: '',
        },
        visible: false,
      });
    }
    return entries;
  });
  const visiblePaneTabs = $derived(openTabs);
  const effectiveActiveTabId = $derived(
    paneState.activeTabId,
  );
  let tabStripElement: HTMLDivElement | undefined;

  $effect(() => {
    const activeTabId = paneState.activeTabId;
    void openTabs.length;
    if (!activeTabId || !tabStripElement) return;
    void tick().then(() => {
      const strip = tabStripElement;
      const activeElement = Array.from(strip?.children ?? []).find((element) => (
        element instanceof HTMLElement && element.dataset.tabId === activeTabId
      ));
      if (!strip || !(activeElement instanceof HTMLElement)) return;
      const left = activeElement.offsetLeft;
      const right = left + activeElement.offsetWidth;
      if (left < strip.scrollLeft) {
        strip.scrollLeft = left;
      } else if (right > strip.scrollLeft + strip.clientWidth) {
        strip.scrollLeft = right - strip.clientWidth;
      }
    });
  });

  // 右栏展开而没有任何 tab（例如恢复上次的展开状态）时默认打开新标签页。
  // Desktop 的浏览器 Tab 由 BrowserAuthority 异步投影，先等权威快照到达，避免它们之前先冒出一个空白页。
  $effect(() => {
    if (paneState.collapsed || openTabs.length > 0) return;
    if (desktopSurface && !paneState.browserAuthoritySynchronized) return;
    openLauncherTab(paneScopeKey);
  });

  function closePane(): void {
    setRightPaneCollapsed(paneScopeKey, true);
  }
  /** 「新增 → 浏览器」在途：只驱动按钮的 loading 表现。 */
  let creatingBrowserPane = $state(false);
  let browserCapabilities = $state<BrowserCapabilitiesSnapshot | null>(null);
  const pluginManifests = $derived(pluginProjection().manifests);
  const canCreateBrowserPane = $derived(Boolean(
    desktopSurface
      && window.magiDesktop
      && browserCapabilities?.platformCapabilities.desktopBrowserSurface === true
      && browserCapabilities?.inAppBrowserEnabled
  ));
  const canCreateTerminalPane = $derived(Boolean(
    !isPublicTunnelAccess()
      && rightPaneState.activeSessionId.trim(),
  ));
  const browserDisabledReason = $derived(
    i18n.t(desktopSurface ? 'browser.error.internalUnavailable' : 'browser.error.desktopRequired'),
  );

  function applyBrowserCapabilities(snapshot: BrowserCapabilitiesSnapshot): void {
    browserCapabilities = snapshot;
  }

  $effect(() => {
    trackPluginProjection();
  });

  // 插件被停用、撤权或升级后，已打开的插件视图随注册表一起关闭，
  // 不能留下指向已失效包的页面（入口出现与面板打开分别管理，关闭同样由投影决定）。
  $effect(() => {
    const projection = pluginProjection();
    if (!projection.loaded) return;
    for (const tab of openTabs) {
      if (tab.kind !== 'plugin') continue;
      const payload = tab.payload as PluginTabPayload;
      const contributed = projection.manifests.some((manifest) => (
        manifest.id === payload.pluginId
        && (manifest.contributions.views ?? []).some((view) => (
          view.id === payload.contributionId && view.placements.includes('rightPane')
        ))
      ));
      if (!contributed) closeTab(paneScopeKey, tab.id);
    }
  });

  onMount(() => {
    void getBrowserCapabilities()
      .then(applyBrowserCapabilities)
      .catch((error) => console.warn('[RightPane] 获取浏览器能力失败:', error));
    const handleCapabilitiesChanged = (event: Event) => {
      applyBrowserCapabilities((event as CustomEvent<BrowserCapabilitiesSnapshot>).detail);
    };
    window.addEventListener('magi:browserCapabilitiesChanged', handleCapabilitiesChanged);
    return () => {
      window.removeEventListener('magi:browserCapabilitiesChanged', handleCapabilitiesChanged);
    };
  });

  async function closeBrowserPanelResource(
    scopeKey: string,
    payload: BrowserTabPayload,
  ): Promise<void> {
    let closeError: unknown = null;
    try {
      await closeBrowserTab(payload.tabId);
    } catch (error) {
      closeError = error;
      clearBrowserTabClosePending(scopeKey, payload.browserSessionId, payload.tabId);
    }
    try {
      await loadBrowserAuthoritySession(payload.browserSessionId, (snapshot) => {
        synchronizeBrowserSessionSnapshot(snapshot, payload.workspacePath, {
          workspaceId: payload.workspaceId,
          sessionId: payload.sessionId,
        });
      });
    } catch (error) {
      if (closeError) {
        console.warn('[RightPane] 关闭浏览器面板资源失败，权威状态也无法刷新:', {
          closeError,
          error,
          tabId: payload.tabId,
        });
      }
    }
  }

  async function closeTerminalPanelResources(payload: TerminalTabPayload): Promise<void> {
    try {
      await closeTerminalSession({
        terminalTabId: payload.terminalTabId,
        sourceFolderId: payload.sourceFolderId,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
        sessionId: payload.sessionId,
      });
    } catch (error) {
      console.warn('[RightPane] 关闭终端面板资源失败:', error);
    }
  }

  function createTerminalPane(): boolean {
    if (!canCreateTerminalPane) return false;
    return openTerminalTab({
      sourceFolderId: resolveComposerSourceFolder(rightPaneState.activeWorkspaceId || '', rightPaneState.activeSessionId)?.sourceFolderId,
      workspaceId: rightPaneState.activeWorkspaceId,
      workspacePath: workspaceRoot,
      sessionId: rightPaneState.activeSessionId,
    }) !== null;
  }

  type LauncherToolKind = 'browser' | 'terminal' | 'plugin' | 'managePlugins';

  const pluginViews = $derived.by(() => pluginManifests.flatMap((manifest) => manifest.contributions?.views?.filter((view) => view.placements.includes('rightPane')).map((view) => ({ manifest, view })) ?? []));
  const pluginViewNeedsSession = $derived(!rightPaneState.activeSessionId.trim());
  const launcherTools = $derived<LauncherTool[]>([
    {
      key: 'terminal',
      kind: 'terminal' satisfies LauncherToolKind,
      label: i18n.t('rightPane.addPanelTerminal'),
      icon: 'terminal',
      enabled: canCreateTerminalPane,
      disabledReason: i18n.t('rightPane.addPanelTerminalDisabled'),
    },
    {
      key: 'browser',
      kind: 'browser',
      label: i18n.t('rightPane.addPanelBrowser'),
      icon: 'globe',
      enabled: canCreateBrowserPane,
      disabledReason: browserDisabledReason,
    },
    ...(pluginViewNeedsSession ? [] : pluginViews.map(({ manifest, view }): LauncherTool => ({
      key: `plugin:${manifest.id}:${view.id}`,
      kind: 'plugin',
      label: view.title,
      detail: manifest.name,
      icon: 'plug',
      enabled: true,
      disabledReason: '',
      pluginId: manifest.id,
      contributionId: view.id,
    }))),
    {
      key: 'managePlugins',
      kind: 'managePlugins',
      label: i18n.t('rightPane.addPanelManagePlugins'),
      icon: 'plug',
      enabled: true,
      disabledReason: '',
    },
  ]);
  /** 本会话正在做事的代理；数据来自代理运行投影，和运行中心同源，这里不另存状态。 */
  const launcherAgents = $derived.by<LauncherAgent[]>(() => {
    const sessionId = rightPaneState.activeSessionId.trim();
    if (!sessionId) return [];
    const projection = getAgentRunState(sessionId, rightPaneState.activeWorkspaceId).projection;
    return selectLauncherAgents(projection?.agents ?? []).map((agent) => ({
      agentRunId: agent.agentRunId,
      label: agent.displayName.trim() || getAgentBrandInfo(agent.role, (key) => i18n.t(key)).label,
      goal: agent.goal.trim(),
      statusLabel: agent.statusLabel,
      queued: agent.status === 'pending' || agent.lifecycle === 'queued',
      color: getAgentRunColor(agent.agentRunId).color,
    }));
  });

  function openLauncherAgent(agent: LauncherAgent): void {
    const sessionId = rightPaneState.activeSessionId.trim();
    if (!sessionId) return;
    // 与运行中心打开代理的方式一致：先选中该次运行，再打开它的 tab。
    selectAgentRun(sessionId, agent.agentRunId, rightPaneState.activeWorkspaceId, workspaceRoot);
    openAgentTab(sessionId, agent.agentRunId, {
      label: agent.label,
      workspaceId: rightPaneState.activeWorkspaceId,
      workspacePath: workspaceRoot,
    });
    closeLauncher();
  }

  /** 「+」直接新增（或回到）一个新标签页，不再先弹菜单。 */
  function openLauncher(): void {
    openLauncherTab(paneScopeKey);
  }

  /** 选定后新标签页自己退场：目标面板已经打开并激活，空白页不再有用。 */
  function closeLauncher(): void {
    closeTab(paneScopeKey, LAUNCHER_TAB_ID);
  }

  function browserCreationScope() {
    return {
      workspaceId: rightPaneState.activeWorkspaceId.trim(),
      workspacePath: workspaceRoot.trim(),
      sessionId: rightPaneState.activeSessionId.trim(),
    };
  }

  async function chooseLauncherTool(tool: LauncherTool): Promise<void> {
    const kind = tool.kind as LauncherToolKind;
    if (kind === 'browser') {
      // 与消息里的链接共用同一个创建入口；空白页不参与同 URL 复用。
      if (creatingBrowserPane) return;
      creatingBrowserPane = true;
      try {
        await openUrlInRightPaneBrowser('about:blank', browserCreationScope());
        closeLauncher();
      } finally {
        creatingBrowserPane = false;
      }
      return;
    }
    if (kind === 'managePlugins') {
      openSettings('plugins');
      return;
    }
    if (kind === 'plugin') {
      const item = pluginViews.find(({ manifest, view }) => manifest.id === tool.pluginId && view.id === tool.contributionId);
      if (item) {
        openPluginTab({ pluginId: item.manifest.id, contributionId: item.view.id, entry: item.view.entry, label: `${item.manifest.name}: ${item.view.title}`, workspaceId: rightPaneState.activeWorkspaceId, workspacePath: workspaceRoot, sessionId: rightPaneState.activeSessionId });
        closeLauncher();
      }
      return;
    }
    if (createTerminalPane()) closeLauncher();
  }

  async function openRecent(item: RecentPaneItem): Promise<void> {
    if (item.kind === 'code') {
      openCodeTab(item.sessionId ?? rightPaneState.activeSessionId, item.filepath, {
        label: item.label,
        displayPath: item.displayPath,
        sourceFolderId: item.sourceFolderId,
        workspaceId: item.workspaceId,
        workspacePath: item.workspacePath,
        sessionId: item.sessionId,
      });
      forgetRecentPaneItem(paneScopeKey, item.key);
      closeLauncher();
      return;
    }
    if (creatingBrowserPane || !canCreateBrowserPane) return;
    creatingBrowserPane = true;
    try {
      await openUrlInRightPaneBrowser(item.url, browserCreationScope());
      forgetRecentPaneItem(paneScopeKey, item.key);
      closeLauncher();
    } finally {
      creatingBrowserPane = false;
    }
  }

  /** 用户关闭文件或网页时记入「最近」；自动收敛的变更对比页不记。 */
  function rememberClosedTab(tab: RightPaneTab): void {
    const at = Date.now();
    if (tab.kind === 'code') {
      const payload = tab.payload as CodeTabPayload;
      if (payload.isChangeDiff || payload.diff) return;
      recordRecentPaneItem(paneScopeKey, {
        kind: 'code',
        key: recentCodeKey(payload.sourceFolderId, payload.filepath),
        label: tab.label,
        filepath: payload.filepath,
        displayPath: payload.displayPath,
        sourceFolderId: payload.sourceFolderId,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
        sessionId: payload.sessionId,
        at,
      });
    } else if (tab.kind === 'browser') {
      const payload = tab.payload as BrowserTabPayload;
      const url = recordableWebUrl(payload.url);
      if (!url) return;
      recordRecentPaneItem(paneScopeKey, {
        kind: 'web',
        key: recentWebKey(url),
        label: tab.label,
        url,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
        sessionId: payload.sessionId || undefined,
        at,
      });
    }
  }
  const activeTab = $derived.by<RightPaneTab | null>(() => {
    const scopeKey = rightPaneState.activeScopeKey;
    const state = scopeKey ? rightPaneState.perSession[scopeKey] : undefined;
    const activeTabId = state?.activeTabId;
    return activeTabId
      ? state?.openTabs.find((tab) => tab.id === activeTabId) ?? null
      : null;
  });


  // ============ Code tab：内容拉取 ============
  /** filepath → 异步加载的源码内容（仅用于补全 store 中没有 content 时） */
  let fetchedContents = $state<Record<string, string>>({});
  /** filepath → loading 标记 */
  let fetchingFlags = $state<Record<string, boolean>>({});
  /** filepath → 拉取出错时的错误信息 */
  let fetchErrors = $state<Record<string, string>>({});
  interface FetchedDiffDetail {
    diff: string;
    originalContent: string | null;
    currentContent: string | null;
  }
  /** filepath → 异步加载的完整变更详情（diff + 两侧全文）。 */
  let fetchedDiffDetails = $state<Record<string, FetchedDiffDetail>>({});
  /** filepath → diff loading 标记 */
  let fetchingDiffFlags = $state<Record<string, boolean>>({});
  /** filepath → diff 拉取出错时的错误信息 */
  let fetchDiffErrors = $state<Record<string, string>>({});
  /** 每个文档 Tab 独立保存预览/源码模式，避免切换 Tab 时相互污染。 */
  let documentModes = $state<Record<string, 'rendered' | 'raw'>>({});
  let previewRequestSeq = 0;
  const contentRequestSeqByKey = new Map<string, number>();
  const diffRequestSeqByKey = new Map<string, number>();

  function invalidatePreviewRequests(): void {
    contentRequestSeqByKey.clear();
    diffRequestSeqByKey.clear();
  }

  // 工作区内容变更（如切分支）后，清空已拉取的文件内容缓存，触发 $effect 按新分支重新拉取。
  onMount(() => {
    const handleWorkspaceContentChanged = () => {
      invalidatePreviewRequests();
      fetchedContents = {};
      fetchErrors = {};
      fetchingFlags = {};
      fetchedDiffDetails = {};
      fetchDiffErrors = {};
      fetchingDiffFlags = {};
    };
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    return () => window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
  });

  const activeCodePayload = $derived.by<CodeTabPayload | null>(() => {
    if (!activeTab || activeTab.kind !== 'code') return null;
    return activeTab.payload as CodeTabPayload;
  });

  const activeFilePath = $derived(activeCodePayload?.filepath ?? '');
  const activeDisplayFilePath = $derived(activeCodePayload?.displayPath ?? activeFilePath);
  function codePayloadCacheKey(payload: CodeTabPayload | null | undefined): string {
    if (!payload?.filepath) return '';
    return [
      payload.workspaceId ?? '',
      payload.sourceFolderId ?? '',
      payload.workspacePath ?? '',
      payload.sessionId ?? '',
      payload.filepath,
      payload.isChangeDiff ? (payload.changeRevision ?? '') : '',
    ].join('::');
  }

  function pruneRecord<T>(record: Record<string, T>, retainedKeys: Set<string>): Record<string, T> {
    const entries = Object.entries(record).filter(([key]) => retainedKeys.has(key));
    return entries.length === Object.keys(record).length ? record : Object.fromEntries(entries);
  }

  const activeContentCacheKey = $derived.by(() => {
    return codePayloadCacheKey(activeCodePayload);
  });
  const activeContentKind = $derived(activeCodePayload?.contentKind ?? 'text');
  const explicitContent = $derived(activeCodePayload?.content ?? null);
  const explicitDiff = $derived(activeCodePayload?.diff ?? null);
  const hasExplicitDiffContents = $derived(Boolean(
    activeCodePayload
      && Object.prototype.hasOwnProperty.call(activeCodePayload, 'originalContent')
      && Object.prototype.hasOwnProperty.call(activeCodePayload, 'currentContent')
  ));
  const activeWantsDiff = $derived(Boolean(
    activeCodePayload?.isChangeDiff
      && (activeContentKind === 'text' || activeContentKind === 'large_text')
  ));
  const activeChangeWorkspaceBinding = $derived.by(() => {
    const workspaceId = activeCodePayload?.workspaceId?.trim() || '';
    const workspacePath = activeCodePayload?.workspacePath?.trim() || '';
    if (!workspaceId && !workspacePath) return null;
    const sessionId = activeCodePayload?.sessionId?.trim() || '';
    return {
      scope: 'workspace' as const,
      workspaceId,
      workspacePath,
      sourceFolderId: activeCodePayload?.sourceFolderId,
      ...(sessionId ? { sessionId } : {}),
    };
  });
  const activeFilePreviewQuery = $derived.by(() => {
    if (!activeFilePath) return '';
    return buildFilePreviewQuery(activeFilePath, {
      sessionId: activeCodePayload?.sessionId,
      workspaceId: activeCodePayload?.workspaceId,
      workspacePath: activeCodePayload?.workspacePath,
      sourceFolderId: activeCodePayload?.sourceFolderId,
    });
  });
  // 异步内容缓存跟随当前 Tab 集合裁剪，关闭预览后立即释放对应内容。
  $effect(() => {
    const retainedKeys = new Set<string>();
    for (const tab of openTabs) {
      if (tab.kind !== 'code') continue;
      const key = codePayloadCacheKey(tab.payload as CodeTabPayload);
      if (key) retainedKeys.add(key);
    }
    for (const key of contentRequestSeqByKey.keys()) {
      if (!retainedKeys.has(key)) contentRequestSeqByKey.delete(key);
    }
    for (const key of diffRequestSeqByKey.keys()) {
      if (!retainedKeys.has(key)) diffRequestSeqByKey.delete(key);
    }
    fetchedContents = pruneRecord(fetchedContents, retainedKeys);
    fetchErrors = pruneRecord(fetchErrors, retainedKeys);
    fetchingFlags = pruneRecord(fetchingFlags, retainedKeys);
    fetchedDiffDetails = pruneRecord(fetchedDiffDetails, retainedKeys);
    fetchDiffErrors = pruneRecord(fetchDiffErrors, retainedKeys);
    fetchingDiffFlags = pruneRecord(fetchingDiffFlags, retainedKeys);
    documentModes = pruneRecord(documentModes, retainedKeys);
  });

  /**
   * 是否需要异步拉取内容：text 类型、未带 content、未带 diff、且非二进制/word 文件。
   * 触发条件统一在 $effect 里检查，避免重复请求。
   */
  $effect(() => {
    const filepath = activeFilePath;
    const cacheKey = activeContentCacheKey;
    if (!filepath) return;
    if (!cacheKey) return;
    if (typeof explicitContent === 'string') return; // 已经有内容
    if (typeof explicitDiff === 'string' && explicitDiff.trim().length > 0) return; // diff 模式
    if (activeWantsDiff) return; // 变更 tab 缺 diff 时由 changes/diff 恢复，不退化成源码预览
    if (activeContentKind !== 'text') return; // 非文本类不拉取
    if (isWordFile(activeDisplayFilePath) || isKnownBinaryFile(activeDisplayFilePath)) return;
    if (typeof fetchedContents[cacheKey] === 'string') return; // 已成功拉过
    if (typeof fetchErrors[cacheKey] === 'string' && fetchErrors[cacheKey].length > 0) return; // 已失败过，停止重试避免死循环
    if (fetchingFlags[cacheKey]) return; // 拉取中

    const requestSeq = ++previewRequestSeq;
    contentRequestSeqByKey.set(cacheKey, requestSeq);
    fetchingFlags = { ...fetchingFlags, [cacheKey]: true };
    fetchErrors = { ...fetchErrors, [cacheKey]: '' };
    (async () => {
      try {
        const payload = await getAgentFilePreview(filepath, {
          sessionId: activeCodePayload?.sessionId,
          workspaceId: activeCodePayload?.workspaceId || '',
          workspacePath: activeCodePayload?.workspacePath || '',
          sourceFolderId: activeCodePayload?.sourceFolderId,
        });
        if (contentRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        fetchedContents = { ...fetchedContents, [cacheKey]: payload.content || '' };
      } catch (error) {
        if (contentRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        console.warn('[RightPane] file preview load failed:', error);
        fetchErrors = { ...fetchErrors, [cacheKey]: i18n.t('web.filePreviewError') };
      } finally {
        if (contentRequestSeqByKey.get(cacheKey) === requestSeq) {
          contentRequestSeqByKey.delete(cacheKey);
          fetchingFlags = { ...fetchingFlags, [cacheKey]: false };
        }
      }
    })();
  });

  // 变更全文不持久化；缺少两侧全文时按权威接口补齐，以支持展开未变更区段。
  $effect(() => {
    const filepath = activeFilePath;
    const cacheKey = activeContentCacheKey;
    if (!filepath) return;
    if (!cacheKey) return;
    if (!activeWantsDiff) return;
    if (!activeChangeWorkspaceBinding) return;
    if (hasExplicitDiffContents) return;
    if (fetchedDiffDetails[cacheKey]) return;
    if (typeof fetchDiffErrors[cacheKey] === 'string' && fetchDiffErrors[cacheKey].length > 0) return;
    if (fetchingDiffFlags[cacheKey]) return;

    const requestSeq = ++previewRequestSeq;
    diffRequestSeqByKey.set(cacheKey, requestSeq);
    fetchingDiffFlags = { ...fetchingDiffFlags, [cacheKey]: true };
    fetchDiffErrors = { ...fetchDiffErrors, [cacheKey]: '' };
    (async () => {
      try {
        const payload = await getAgentChangeDiff(filepath, {
          ...activeChangeWorkspaceBinding,
        });
        if (diffRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        fetchedDiffDetails = {
          ...fetchedDiffDetails,
          [cacheKey]: {
            diff: payload.diff || '',
            originalContent: typeof payload.originalContent === 'string' ? payload.originalContent : null,
            currentContent: typeof payload.currentContent === 'string' ? payload.currentContent : null,
          },
        };
      } catch (error) {
        if (diffRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        console.warn('[RightPane] change diff load failed:', error);
        fetchDiffErrors = { ...fetchDiffErrors, [cacheKey]: i18n.t('web.filePreviewError') };
      } finally {
        if (diffRequestSeqByKey.get(cacheKey) === requestSeq) {
          diffRequestSeqByKey.delete(cacheKey);
          fetchingDiffFlags = { ...fetchingDiffFlags, [cacheKey]: false };
        }
      }
    })();
  });

  const previewLoading = $derived.by(() => {
    if (!activeContentCacheKey) return false;
    return Boolean(activeWantsDiff
      ? fetchingDiffFlags[activeContentCacheKey]
      : fetchingFlags[activeContentCacheKey]);
  });
  const previewError = $derived.by(() => {
    if (!activeContentCacheKey) return '';
    return activeWantsDiff
      ? (fetchDiffErrors[activeContentCacheKey] || '')
      : (fetchErrors[activeContentCacheKey] || '');
  });
  /** 最终用于渲染的内容：优先 store 显式 content，其次异步拉取结果 */
  const previewContent = $derived.by<string | null>(() => {
    if (typeof explicitContent === 'string') return explicitContent;
    if (!activeContentCacheKey) return null;
    return fetchedContents[activeContentCacheKey] ?? null;
  });

  // ============ 代码高亮 ============
  const EXT_LANG_MAP: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
    py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java',
    cpp: 'cpp', c: 'c', cs: 'csharp', kt: 'kotlin', swift: 'swift',
    html: 'xml', vue: 'xml', svelte: 'xml', xml: 'xml', svg: 'xml',
    css: 'css', scss: 'scss', less: 'less',
    json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini',
    md: 'markdown', sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql', graphql: 'graphql', dockerfile: 'dockerfile',
  };

  const fileLanguage = $derived.by(() => {
    if (!activeDisplayFilePath) return '';
    const ext = activeDisplayFilePath.split('.').pop()?.toLowerCase() ?? '';
    return EXT_LANG_MAP[ext] ?? '';
  });

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  const diffCode = $derived.by(() => {
    if (typeof explicitDiff === 'string' && explicitDiff.trim().length > 0) {
      return explicitDiff.trimEnd();
    }
    if (activeWantsDiff && activeContentCacheKey && fetchedDiffDetails[activeContentCacheKey]) {
      return fetchedDiffDetails[activeContentCacheKey].diff.trimEnd();
    }
    return '';
  });
  const diffOriginalContent = $derived.by<string | null>(() => {
    if (hasExplicitDiffContents) return activeCodePayload?.originalContent ?? null;
    if (!activeContentCacheKey) return null;
    return fetchedDiffDetails[activeContentCacheKey]?.originalContent ?? null;
  });
  const diffCurrentContent = $derived.by<string | null>(() => {
    if (hasExplicitDiffContents) return activeCodePayload?.currentContent ?? null;
    if (!activeContentCacheKey) return null;
    return fetchedDiffDetails[activeContentCacheKey]?.currentContent ?? null;
  });
  const hasDiff = $derived(diffCode.trim().length > 0);

  // ============ 文件类型派生 ============
  const displayPath = $derived(getDisplayPath(activeDisplayFilePath, workspaceRoot));
  const markdownFile = $derived(isMarkdownFile(activeDisplayFilePath));
  const htmlFile = $derived(isHtmlFile(activeDisplayFilePath));
  const documentFile = $derived(markdownFile || htmlFile);
  const wordFile = $derived(isWordFile(activeDisplayFilePath));
  const imageFile = $derived(activeDisplayFilePath ? isImageFile(activeDisplayFilePath) : false);
  const imageSource = $derived(
    activeCodePayload?.imageDataUrl?.trim()
      || agentUrl('/api/files/raw', activeFilePreviewQuery),
  );
  // 图片虽属二进制，但走专门的 <img> 预览分支，故从 binaryFile（元信息兜底）排除。
  const binaryFile = $derived(
    !imageFile
      && (activeContentKind === 'binary' || (activeDisplayFilePath ? isKnownBinaryFile(activeDisplayFilePath) : false)),
  );
  const largeTextFile = $derived(activeContentKind === 'large_text');
  const symlinkFile = $derived(activeContentKind === 'symlink');
  const specialFile = $derived(activeContentKind === 'special');

  // ============ 图片缩放 / 平移 ============
  const IMAGE_ZOOM_MIN = 0.1;
  const IMAGE_ZOOM_MAX = 8;
  const IMAGE_ZOOM_STEP = 0.2;
  let imageZoom = $state(1);
  let imagePanX = $state(0);
  let imagePanY = $state(0);
  let imageDragging = $state(false);
  let imageDragStartX = 0;
  let imageDragStartY = 0;
  let imagePanStartX = 0;
  let imagePanStartY = 0;

  // 切换文件时重置缩放/平移，避免沿用上一张图的视图状态。
  $effect(() => {
    void activeFilePath;
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  });

  function clampZoom(value: number): number {
    return Math.min(IMAGE_ZOOM_MAX, Math.max(IMAGE_ZOOM_MIN, value));
  }

  function setImageZoom(next: number) {
    const clamped = clampZoom(next);
    if (clamped === 1) {
      imagePanX = 0;
      imagePanY = 0;
    }
    imageZoom = clamped;
  }

  function zoomImageIn() {
    setImageZoom(imageZoom + IMAGE_ZOOM_STEP);
  }

  function zoomImageOut() {
    setImageZoom(imageZoom - IMAGE_ZOOM_STEP);
  }

  function resetImageZoom() {
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  }

  function handleImageWheel(event: WheelEvent) {
    event.preventDefault();
    const factor = event.deltaY < 0 ? 1 + IMAGE_ZOOM_STEP : 1 / (1 + IMAGE_ZOOM_STEP);
    setImageZoom(imageZoom * factor);
  }

  function handleImagePointerDown(event: PointerEvent) {
    if (imageZoom <= 1) return;
    imageDragging = true;
    imageDragStartX = event.clientX;
    imageDragStartY = event.clientY;
    imagePanStartX = imagePanX;
    imagePanStartY = imagePanY;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function handleImagePointerMove(event: PointerEvent) {
    if (!imageDragging) return;
    imagePanX = imagePanStartX + (event.clientX - imageDragStartX);
    imagePanY = imagePanStartY + (event.clientY - imageDragStartY);
  }

  function handleImagePointerUp(event: PointerEvent) {
    if (!imageDragging) return;
    imageDragging = false;
    (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
  }

  // ============ Markdown 预览与源码切换 ============
  const documentMode = $derived.by<'rendered' | 'raw'>(() => {
    if (!activeContentCacheKey) return 'rendered';
    if (htmlFile) return 'raw';
    return documentModes[activeContentCacheKey] ?? 'rendered';
  });
  function setDocumentMode(mode: 'rendered' | 'raw'): void {
    if (!activeContentCacheKey) return;
    documentModes = { ...documentModes, [activeContentCacheKey]: mode };
  }

  /**
   * 副标题栏的「在浏览器打开」：把当前 HTML 文件交给统一入口创建预览页。
   *
   * 创建、复用与失败提示都在 `right-pane-browser`，这里只取当前 Tab 的
   * 文件与作用域，并保留按钮的进行中状态。
   */
  let openingHtmlInBrowser = $state(false);
  async function openHtmlInMagiBrowser(): Promise<void> {
    const filePath = activeFilePath.trim();
    if (!isHtmlFile(filePath) || openingHtmlInBrowser) return;
    openingHtmlInBrowser = true;
    try {
      await openHtmlFileInRightPaneBrowser({
        filepath: filePath,
        workspaceId: activeCodePayload?.workspaceId?.trim() ?? '',
        workspacePath: activeCodePayload?.workspacePath?.trim() ?? '',
        sessionId: activeCodePayload?.sessionId?.trim() ?? '',
      });
    } finally {
      openingHtmlInBrowser = false;
    }
  }

  const rawPreviewContent = $derived(previewContent ?? '');
  const truncatedContent = $derived(
    rawPreviewContent.length > 500_000 ? rawPreviewContent.slice(0, 100_000) : rawPreviewContent,
  );
  const isLargeFile = $derived(rawPreviewContent.length > 500_000);
  /**
   * source 视图行高亮：对整段内容做一次高亮（保持跨行 token），
   * 然后按 '\n' 切片，避免逐行高亮丢失多行 token 上下文。
   */
  let sourceLines = $state<string[]>([]);
  $effect(() => {
    const source = truncatedContent;
    const lang = fileLanguage;
    const lines = source.split('\n');
    sourceLines = lines.map(escapeHtml);
    if (!source || !lang) return;

    let cancelled = false;
    void highlightCode(source, lang).then((result) => {
      if (!cancelled && result !== null) sourceLines = result.split('\n');
    }).catch((error) => {
      console.warn('[RightPane] 源码高亮失败:', error);
    });
    return () => {
      cancelled = true;
    };
  });
  const hasContent = $derived(rawPreviewContent.length > 0);
  const codeMode = $derived(
    !previewLoading && !previewError && !wordFile && !binaryFile
      && !largeTextFile && !symlinkFile && !specialFile
      && (hasDiff || (hasContent && (!documentFile || documentMode === 'raw'))),
  );

  // ============ Tab 视觉 ============
  // 原 Task 身份决定识别色，已恢复的 Tab 也与主对话及运行中心一致。

  function tabAccent(tab: RightPaneTab): string {
    if (tab.kind === 'agent') {
      return getAgentRunColor((tab.payload as AgentTabPayload).agentRunId).color;
    }
    return 'var(--info)';
  }

  function tabLabel(tab: RightPaneTab): string {
    if (tab.kind === 'terminal') return i18n.t('terminalPanel.title');
    if (tab.kind === 'launcher') return i18n.t('rightPane.newTab');
    if (tab.kind === 'agents') return i18n.t('rightPane.agents.title');
    return tab.label;
  }

  function tabIcon(tab: RightPaneTab): 'chevron-right' | 'globe' | 'terminal' | 'plus' | 'bot' {
    if (tab.kind === 'browser') return 'globe';
    if (tab.kind === 'agents') return 'bot';
    if (tab.kind === 'launcher') return 'plus';
    return tab.kind === 'terminal' ? 'terminal' : 'chevron-right';
  }

  function tabTooltip(tab: RightPaneTab): string {
    if (tab.kind === 'code') {
      const payload = tab.payload as CodeTabPayload;
      const path = payload.displayPath || payload.filepath;
      return payload.sourceFolderId && payload.workspacePath
        ? `${payload.workspacePath} · ${path}`
        : path;
    }
    if (tab.kind === 'browser') {
      const occupied = (tab.payload as BrowserTabPayload).agentOccupied;
      return `${tabLabel(tab)}: ${i18n.t(occupied ? 'browser.control.agentOccupied' : 'browser.control.released')}`;
    }
    if (tab.kind === 'terminal') {
      const payload = tab.payload as TerminalTabPayload;
      return payload.workspacePath || payload.workspaceId || i18n.t('input.workspace.unbound');
    }
    return tabLabel(tab);
  }

  // ============ 交互 ============
  /**
   * Tab 条 drag-to-pan 状态：
   * - 滚轮鼠标横向需求由 onwheel（deltaY → scrollLeft）解决；
   * - 触控板横滑由原生 deltaX 路径解决；
   * - 这里补的是「按住鼠标在 tab 条上拖动来平移」——VSCode / Chrome tab strip 的标准交互。
   */
  let dragState: { startX: number; startScrollLeft: number; moved: boolean } | null = null;
  let isDraggingTabs = $state(false);
  /** 真实拖拽刚结束的时间戳；用于吞掉紧随 pointerup 的 click 事件，避免拖拽结束误切换 tab */
  let dragEndedAt = 0;
  const DRAG_THRESHOLD = 4;
  const DRAG_CLICK_SUPPRESS_MS = 50;

  function recentlyDragged(): boolean {
    return performance.now() - dragEndedAt < DRAG_CLICK_SUPPRESS_MS;
  }

  function handleTabClick(tabId: string) {
    if (recentlyDragged()) return;
    setActiveRightPaneTab(paneScopeKey, tabId);
  }

  function handleTabClose(event: MouseEvent, tabId: string) {
    event.stopPropagation();
    if (recentlyDragged()) return;
    const tab = openTabs.find((item) => item.id === tabId);
    if (tab) rememberClosedTab(tab);
    closeTab(paneScopeKey, tabId);
    if (tab?.kind === 'browser') {
      const browserPayload = tab.payload as BrowserTabPayload;
      markBrowserTabClosePending(
        paneScopeKey,
        browserPayload.browserSessionId,
        browserPayload.tabId,
      );
      void closeBrowserPanelResource(paneScopeKey, browserPayload);
    } else if (tab?.kind === 'terminal') {
      void closeTerminalPanelResources(tab.payload as TerminalTabPayload);
    }
  }

  /**
   * 单行 tab 条只在水平方向溢出（overflow-x: auto），但标准鼠标滚轮只发出
   * 垂直方向的 deltaY，浏览器不会自动把它翻译成 scrollLeft——结果就是
   * 滚轮鼠标用户完全无法浏览溢出的 tab。这里把 deltaY 转成 scrollLeft，
   * 保留触控板原生 deltaX 走原路径（不重复消费）。
   */
  function handleTabsWheel(event: WheelEvent) {
    if (event.deltaX !== 0) return; // 触控板已经在水平方向输入 delta，不干预
    if (event.deltaY === 0) return;
    const target = event.currentTarget as HTMLDivElement;
    if (target.scrollWidth <= target.clientWidth) return; // 没有溢出就别拦
    target.scrollLeft += event.deltaY;
    event.preventDefault();
  }

  function handleTabsPointerDown(event: PointerEvent) {
    // 只对鼠标主键启用 drag-to-pan；触摸 / 笔 / 触控板交给原生路径
    if (event.pointerType !== 'mouse' || event.button !== 0) return;
    // 关闭按钮 (×) 不接管——保证用户点 × 关闭 tab 时不会进入拖拽
    const targetEl = event.target as HTMLElement | null;
    if (targetEl?.closest('.right-pane-tab-close')) return;
    const strip = event.currentTarget as HTMLDivElement;
    dragState = {
      startX: event.clientX,
      startScrollLeft: strip.scrollLeft,
      moved: false,
    };
  }

  function handleTabsPointerMove(event: PointerEvent) {
    if (!dragState) return;
    const dx = event.clientX - dragState.startX;
    if (!dragState.moved) {
      if (Math.abs(dx) < DRAG_THRESHOLD) return; // 未越过阈值仍按普通点击处理
      dragState.moved = true;
      isDraggingTabs = true;
      const strip = event.currentTarget as HTMLDivElement;
      strip.setPointerCapture(event.pointerId);
    }
    const strip = event.currentTarget as HTMLDivElement;
    strip.scrollLeft = dragState.startScrollLeft - dx;
    event.preventDefault();
  }

  function handleTabsPointerEnd(event: PointerEvent) {
    if (!dragState) return;
    const moved = dragState.moved;
    dragState = null;
    if (moved) {
      dragEndedAt = performance.now();
      isDraggingTabs = false;
      const strip = event.currentTarget as HTMLDivElement;
      if (strip.hasPointerCapture(event.pointerId)) {
        strip.releasePointerCapture(event.pointerId);
      }
    }
  }

  function formatSize(value?: number): string {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return '-';
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function getDisplayPath(path: string, root: string): string {
    if (!path) return '';
    const normalizedPath = path.replace(/\\/g, '/');
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
    if (normalizedRoot && normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return normalizedPath.slice(normalizedRoot.length + 1);
    }
    return path;
  }
</script>

<aside class="right-pane" data-magi-surface={overlay ? 'window' : undefined} aria-label={i18n.t('rightPane.title')}>
  <!-- 顶部 Tab 条；右栏折叠入口由工作台外壳固定在窗口右上角。 -->
  <header class="right-pane-tabbar" class:right-pane-tabbar--overlay={overlay}>
    {#if overlay}
      <button
        type="button"
        class="right-pane-overlay-action"
        onclick={closePane}
        title={i18n.t('rightPane.backToConversation')}
        aria-label={i18n.t('rightPane.backToConversation')}
      >
        <Icon name="chevron-right" size={14} class="right-pane-back-icon" />
      </button>
    {/if}
    <div
      bind:this={tabStripElement}
      class="right-pane-tabs"
      class:dragging={isDraggingTabs}
      role="tablist"
      tabindex="-1"
      aria-label={i18n.t('rightPane.title')}
      onwheel={handleTabsWheel}
      onpointerdown={handleTabsPointerDown}
      onpointermove={handleTabsPointerMove}
      onpointerup={handleTabsPointerEnd}
      onpointercancel={handleTabsPointerEnd}
    >
      {#each visiblePaneTabs as tab (tab.id)}
        {@const isActive = tab.id === effectiveActiveTabId}
        {@const accent = tabAccent(tab)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="right-pane-tab"
          class:active={isActive}
          role="tab"
          data-tab-id={tab.id}
          tabindex="0"
          aria-selected={isActive}
          style={`--tab-accent: ${accent};`}
          title={tabTooltip(tab)}
          onclick={() => handleTabClick(tab.id)}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); handleTabClick(tab.id); } }}
        >
          <span class="right-pane-tab-icon" aria-hidden="true">
            {#if tab.kind === 'code'}
              <FileTypeIcon path={(tab.payload as CodeTabPayload).displayPath ?? (tab.payload as CodeTabPayload).filepath} size={14} />
            {:else}
              <Icon name={tabIcon(tab)} size={12} />
            {/if}
          </span>
          {#if tab.kind === 'browser'}
            {@const agentOccupied = (tab.payload as BrowserTabPayload).agentOccupied}
            <span
              class="browser-control-status"
              class:occupied={agentOccupied}
              title={i18n.t(agentOccupied ? 'browser.control.agentOccupied' : 'browser.control.released')}
              aria-label={i18n.t(agentOccupied ? 'browser.control.agentOccupied' : 'browser.control.released')}
            ></span>
          {/if}
          <span class="right-pane-tab-label" class:mono={tab.kind === 'code'}>{#if tab.kind === 'code'}<FileName name={tabLabel(tab)} />{:else}{tabLabel(tab)}{/if}</span>
          <button
            type="button"
            class="right-pane-tab-close"
            aria-label={i18n.t('rightPane.closeTab')}
            onclick={(event) => handleTabClose(event, tab.id)}
          >
            <Icon name="x" size={10} />
          </button>
        </div>
      {/each}
    </div>
    <div class="right-pane-add-wrap">
      <button
        type="button"
        class="right-pane-add-tab"
        data-open-tab-count={openTabs.length}
        onclick={openLauncher}
        title={i18n.t('rightPane.addPanel')}
        aria-label={i18n.t('rightPane.addPanel')}
        aria-busy={creatingBrowserPane}
      >
        <Icon name={creatingBrowserPane ? 'loader' : 'plus'} size={14} />
      </button>
    </div>
  </header>

  <!-- 当前 code tab 的副标题：路径 + 文档预览操作 -->
  {#if activeTab && activeTab.kind === 'code'}
    <div class="right-pane-subbar">
      <div class="right-pane-path" title={activeDisplayFilePath}><FileName name={displayPath} /></div>
      <div class="right-pane-document-actions">
        {#if documentFile && !hasDiff && !previewLoading && !previewError && !wordFile && !binaryFile && !largeTextFile && !symlinkFile && !specialFile && (htmlFile || hasContent)}
          {#if markdownFile}
            <div class="right-pane-document-modes" role="tablist" aria-label={i18n.t('web.filePreviewTitle')}>
              <button
                type="button"
                class="right-pane-document-mode"
                class:active={documentMode === 'rendered'}
                role="tab"
                aria-selected={documentMode === 'rendered'}
                onclick={() => setDocumentMode('rendered')}
              >{i18n.t('web.filePreviewRendered')}</button>
              <button
                type="button"
                class="right-pane-document-mode"
                class:active={documentMode === 'raw'}
                role="tab"
                aria-selected={documentMode === 'raw'}
                onclick={() => setDocumentMode('raw')}
              >{i18n.t('web.filePreviewRaw')}</button>
            </div>
          {:else if htmlFile && canCreateBrowserPane}
            <button
              type="button"
              class="btn-icon btn-icon--md"
              onclick={() => void openHtmlInMagiBrowser()}
              disabled={openingHtmlInBrowser}
              title={i18n.t('web.filePreviewOpenBrowser')}
              aria-label={i18n.t('web.filePreviewOpenBrowser')}
            ><Icon name="globe" size={16} /></button>
          {/if}
        {/if}
        {#if activeFilePreviewQuery && !activeCodePayload?.imageDataUrl}
          <a class="btn-icon btn-icon--md" href={agentUrl('/api/files/download', activeFilePreviewQuery)}
            download title={i18n.t('web.fileDownload')} aria-label={i18n.t('web.fileDownload')}><Icon name="download" size={16} /></a>
        {/if}
      </div>
    </div>
  {/if}

{#snippet launcherPage()}
    <LauncherTabContent
      tools={launcherTools}
      agents={launcherAgents}
      recent={recentPaneItems(paneScopeKey)}
      webAvailable={canCreateBrowserPane}
      webUnavailableReason={browserDisabledReason}
      busy={creatingBrowserPane}
      onChooseTool={(tool) => void chooseLauncherTool(tool)}
      onOpenAgent={openLauncherAgent}
      onOpenRecent={(item) => void openRecent(item)}
    />
{/snippet}

  <!-- Body：浏览器 guest 始终保留在右栏内容槽中，只切换显示状态。
       切换代码、图片、终端或 Agent 时不销毁 Chromium 文档和会话。 -->
  <div
    class="right-pane-body"
    class:right-pane-body--code={codeMode}
    class:right-pane-body--large-text={largeTextFile}
    class:right-pane-body--browser={activeTab?.kind === 'browser'}
    class:right-pane-body--terminal={activeTab?.kind === 'terminal'}
    class:right-pane-body--plugin={activeTab?.kind === 'plugin'}
  >
    {#each browserHosts as host (host.tabId)}
      {@const retainedInBackground = !host.visible && retainedTabIds.has(host.tabId)}
      <div
        class="right-pane-browser-tab-host"
        class:active={host.visible}
        class:right-pane-browser-tab-host--background={retainedInBackground}
        hidden={!host.visible && !retainedInBackground}
        aria-hidden={!host.visible}
      >
        <BrowserTabContent
          browserSessionId={host.payload.browserSessionId}
          tabId={host.payload.tabId}
          lifecycle={host.payload.lifecycle}
          workspaceId={host.payload.workspaceId}
          workspacePath={host.payload.workspacePath}
          sessionId={host.payload.sessionId || undefined}
          desktopSurface={desktopSurface}
          retainedForAgent={retainedTabIds.has(host.tabId)}
          hostVisible={visible && host.visible}
          onTitleChange={(label) => {
            if (host.paneTabId) updateRightPaneTabLabel(paneScopeKey, host.paneTabId, label);
          }}
        />
      </div>
    {/each}



    <!-- 离场期间保留该页签自己的身份，不能让仍在清理的子组件读取已经清空的 activeTab。 -->
    {#each activeTab ? [activeTab] : [] as renderedTab (renderedTab.id)}
    {#if renderedTab.kind === 'agent'}
      {@const agentPayload = renderedTab.payload as AgentTabPayload}
      <AgentTabContent
        agentRunId={agentPayload.agentRunId}
        workspaceId={agentPayload.workspaceId}
        workspacePath={agentPayload.workspacePath}
        sessionId={agentPayload.sessionId}
      />
    {:else if renderedTab.kind === 'terminal'}
      {@const terminalPayload = renderedTab.payload as TerminalTabPayload}
      {#key terminalPayload.terminalTabId}
      <TerminalTabContent
        terminalTabId={terminalPayload.terminalTabId}
        sourceFolderId={terminalPayload.sourceFolderId}
        workspaceId={terminalPayload.workspaceId}
        workspacePath={terminalPayload.workspacePath}
        sessionId={terminalPayload.sessionId}
      />
      {/key}
    {:else if renderedTab.kind === 'browser'}
      <!-- BrowserTabContent 已在上方的内容槽中渲染；这里不能再进入文件预览分支。 -->
    {:else if renderedTab.kind === 'launcher'}
      {@render launcherPage()}
    {:else if renderedTab.kind === 'agents'}
      <AgentListTabContent />
    {:else if renderedTab.kind === 'plugin'}
      {@const pluginPayload = renderedTab.payload as PluginTabPayload}
      <PluginTabContent
        pluginId={pluginPayload.pluginId}
        entry={pluginPayload.entry}
        label={renderedTab.label}
        active={visible}
      />
    {:else if previewLoading}
      <div class="right-pane-state">{i18n.t('web.filePreviewLoading')}</div>
    {:else if previewError}
      <div class="right-pane-state right-pane-state--error">
        {previewError}
      </div>
    {:else if wordFile}
      <div class="right-pane-state">
        <FileTypeIcon path={activeDisplayFilePath} size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedWord')}</span>
      </div>
    {:else if imageFile}
      <div class="right-pane-image-wrap">
        <div
          class="right-pane-image"
          class:dragging={imageDragging}
          class:zoomed={imageZoom > 1}
          role="img"
          aria-label={displayPath}
          onwheel={handleImageWheel}
          onpointerdown={handleImagePointerDown}
          onpointermove={handleImagePointerMove}
          onpointerup={handleImagePointerUp}
          onpointercancel={handleImagePointerUp}
        >
          <img
            class="right-pane-image-el"
            src={imageSource}
            alt={displayPath}
            draggable="false"
            style={`transform: translate(${imagePanX}px, ${imagePanY}px) scale(${imageZoom});`}
          />
        </div>
        <div class="right-pane-image-controls">
          <button class="image-zoom-btn" onclick={zoomImageOut} disabled={imageZoom <= IMAGE_ZOOM_MIN} title={i18n.t('web.imageZoomOut')} aria-label={i18n.t('web.imageZoomOut')}>
            <Icon name="minus" size={14} />
          </button>
          <button class="image-zoom-level" onclick={resetImageZoom} title={i18n.t('web.imageZoomReset')}>{Math.round(imageZoom * 100)}%</button>
          <button class="image-zoom-btn" onclick={zoomImageIn} disabled={imageZoom >= IMAGE_ZOOM_MAX} title={i18n.t('web.imageZoomIn')} aria-label={i18n.t('web.imageZoomIn')}>
            <Icon name="plus" size={14} />
          </button>
        </div>
      </div>
    {:else if binaryFile}
      <div class="right-pane-state right-pane-state--metadata">
        <FileTypeIcon path={activeDisplayFilePath} size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedBinary')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</span>
        {#if activeCodePayload?.mime}
          <span class="right-pane-meta-line">{i18n.t('edits.nonText.mime')}: {activeCodePayload.mime}</span>
        {/if}
      </div>
    {:else if largeTextFile}
      <div class="right-pane-large-text">
        <div class="right-pane-notice ui-meta"><span>{i18n.t('edits.nonText.largeTextTitle')}</span><span>{i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</span></div>
        {#if activeCodePayload?.headSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.head')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.headSummary}</pre>
          </div>
        {/if}
        {#if activeCodePayload?.tailSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.tail')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.tailSummary}</pre>
          </div>
        {/if}
      </div>
    {:else if symlinkFile}
      <div class="right-pane-state right-pane-state--metadata">
        <FileTypeIcon path={activeDisplayFilePath} size={22} />
        <span>{i18n.t('edits.nonText.symlinkTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.target')}: {activeCodePayload?.symlinkTarget ?? '-'}</span>
      </div>
    {:else if specialFile}
      <div class="right-pane-state right-pane-state--metadata">
        <FileTypeIcon path={activeDisplayFilePath} size={22} />
        <span>{i18n.t('edits.nonText.specialTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.specialHint')}</span>
      </div>
    {:else if hasDiff}
      <div class="right-pane-diff" aria-label={displayPath}>
        <DiffCodeBlock
          diff={diffCode}
          originalContent={diffOriginalContent}
          currentContent={diffCurrentContent}
          ariaLabel={displayPath}
          language={fileLanguage}
          fill={true}
        />
      </div>
    {:else if !hasContent}
      <div class="right-pane-state">{i18n.t('edits.preview.empty')}</div>
    {:else}
      {#if isLargeFile}
        <div class="right-pane-notice">{i18n.t('web.filePreviewLargeFile')}</div>
      {/if}
      {#if markdownFile && documentMode === 'rendered'}
        <div class="right-pane-markdown">
          <MarkdownContent
            content={truncatedContent}
            baseFilePath={activeFilePath}
            filePreviewScope={{
              workspaceId: activeCodePayload?.workspaceId,
              workspacePath: activeCodePayload?.workspacePath,
              sourceFolderId: activeCodePayload?.sourceFolderId,
              sessionId: activeCodePayload?.sessionId,
            }}
          />
        </div>
      {:else}
        <div class="right-pane-source" aria-label={displayPath}>
          {#each sourceLines as line, index (index)}
            <div class="right-pane-source-row">
              <span class="right-pane-source-line-number" aria-hidden="true">{index + 1}</span>
              <code class="right-pane-source-line">{@html line || '&nbsp;'}</code>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
    {:else}
      <!-- 等待 Browser 快照等暂时没有活动页签的状态仍使用原启动页。 -->
      {@render launcherPage()}
    {/each}
  </div>
</aside>

<style>
  .right-pane {
    /* The pane owns its single inset perimeter; child actions remain borderless. */
    display: flex;
    position: relative;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    border: 0;
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border);
    border-radius: var(--radius-lg);
    background: var(--magi-surface-right-pane);
    overflow: hidden;
  }

  .right-pane-browser-tab-host {
    position: absolute;
    inset: 0;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .right-pane-browser-tab-host[hidden] { display: none; }
  /* 后台保留必须是「移出可见区域」而不是 display:none：guest 只有在内容槽有真实
     布局尺寸时才能注册并持续接收自动化命令。 */
  .right-pane-browser-tab-host--background {
    transform: translate3d(-20000px, 0, 0);
    pointer-events: none;
  }

  /* ============ Tab 条 ============ */
  .right-pane-tabbar {
    --tab-inset: var(--space-2);
    --tab-radius: max(0px, calc(var(--radius-lg) - var(--tab-inset)));
    position: relative;
    z-index: 1;
    display: flex;
    align-items: stretch;
    box-sizing: border-box;
    height: 38px;
    flex-shrink: 0;
    background: transparent;
    padding-inline-end: var(--space-2);
  }

  .right-pane-tabs {
    display: flex;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    /* drag-to-pan：默认抓握光标，提示用户「这一条可以按住拖」；
       拖拽进行中切到 grabbing 并禁用文字选择，避免选中 tab 文字 */
    cursor: grab;
    user-select: none;
  }
  .right-pane-tabs::-webkit-scrollbar { display: none; }
  .right-pane-tabs.dragging { cursor: grabbing; }

  .right-pane-tabbar--overlay {
    gap: 4px;
    padding: 0 var(--space-2);
  }

  .right-pane-add-tab {
    flex: 0 0 auto;
    align-self: center;
    width: 28px;
    height: 28px;
    margin: 0 4px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-add-wrap {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
  }

  .right-pane-add-tab:hover:not(:disabled),
  .right-pane-add-tab:focus-visible {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .right-pane-add-tab:disabled {
    cursor: default;
    opacity: 0.4;
  }

  .right-pane-add-tab[aria-busy='true'] :global(svg) {
    animation: right-pane-spin 0.8s linear infinite;
  }

  @keyframes right-pane-spin {
    to { transform: rotate(360deg); }
  }

  .right-pane-overlay-action {
    flex: 0 0 auto;
    align-self: center;
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-overlay-action:hover,
  .right-pane-overlay-action:focus-visible {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  :global(.right-pane-back-icon) {
    transform: rotate(180deg);
  }

  .right-pane-tab {
    flex: 0 0 auto;
    max-width: 180px;
    min-width: 90px;
    padding-block: 0;
    padding-inline: calc(var(--tab-inset) + var(--space-3));
    height: 100%;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    cursor: pointer;
    position: relative;
    isolation: isolate;
    transition: color var(--transition-fast);
  }

  /* 选中与悬停的「气泡」是内缩的视觉层；tab 本身仍占满整条高度，作为完整的点击热区。 */
  .right-pane-tab::before {
    content: '';
    position: absolute;
    inset: var(--tab-inset);
    z-index: -1;
    border-radius: var(--tab-radius);
    background: transparent;
    transition: background var(--transition-fast), box-shadow var(--transition-fast);
  }

  /* 未选中的 tab 之间只留一条很短的淡分隔线，不再用整高的竖线切开。 */
  .right-pane-tab::after {
    content: '';
    position: absolute;
    right: 0;
    top: 50%;
    width: 1px;
    height: 14px;
    transform: translateY(-50%);
    background: var(--border-subtle);
    transition: opacity var(--transition-fast);
  }

  /* 与气泡相邻、最后一个 tab 以及被悬停的 tab 两侧不显示分隔线，避免线贴着气泡边缘。 */
  .right-pane-tab:last-child::after,
  .right-pane-tab.active::after,
  .right-pane-tab:has(+ .right-pane-tab.active)::after,
  .right-pane-tab:hover::after,
  .right-pane-tab:has(+ .right-pane-tab:hover)::after {
    opacity: 0;
  }

  .right-pane-tab:hover {
    color: var(--foreground);
  }

  .right-pane-tab:hover::before {
    background: var(--surface-2);
  }

  .right-pane-tab.active {
    color: var(--foreground);
    font-weight: var(--font-semibold);
  }

  .right-pane-tab.active::before {
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-subtle);
  }

  .right-pane-tab:focus-visible {
    outline: none;
  }

  .right-pane-tab:focus-visible::before {
    box-shadow: inset 0 0 0 2px var(--primary);
  }

  .right-pane-tab-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--tab-accent, var(--foreground-muted));
    flex-shrink: 0;
  }

  .right-pane-tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .right-pane-tab-label.mono { display: flex; font-family: var(--font-mono); font-size: var(--text-xs); }

  .browser-control-status {
    width: 7px;
    height: 7px;
    flex: 0 0 auto;
    border-radius: 50%;
    background: var(--foreground-muted);
  }

  .browser-control-status.occupied {
    background: var(--success);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--success) 16%, transparent);
  }

  .right-pane-tab-close {
    width: 16px;
    height: 16px;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--foreground-muted);
    border: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    opacity: 0;
    flex-shrink: 0;
    padding: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast);
  }

  .right-pane-tab:hover .right-pane-tab-close,
  .right-pane-tab.active .right-pane-tab-close {
    opacity: 0.85;
  }

  .right-pane-tab-close:hover {
    background: color-mix(in srgb, var(--foreground-muted) 18%, transparent);
    opacity: 1;
  }

  /* ============ 副标题（路径 + 文档预览操作） ============ */
  .right-pane-subbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 6px var(--space-4);
    border-bottom: var(--magi-hairline) solid var(--border);
    flex-shrink: 0;
  }

  .right-pane-path {
    display: flex;
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-family: var(--font-mono);
  }

  .right-pane-document-actions,
  .right-pane-document-modes {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    flex-shrink: 0;
  }

  .right-pane-document-mode {
    padding: 3px 10px;
    border: none;
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-document-mode:hover,
  .right-pane-document-mode.active {
    background: color-mix(in srgb, var(--surface-selected) 72%, transparent);
    color: var(--foreground);
  }

  @media (hover: none), (pointer: coarse) {
    .right-pane-document-actions { gap: var(--space-2); }
    .right-pane-document-actions :global(.btn-icon) {
      min-inline-size: 44px;
      min-block-size: 44px;
    }
  }

  /* ============ Body ============ */
  .right-pane-body {
    position: relative;
    min-width: 0;
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4);
  }

  .right-pane-body--large-text {
    display: flex;
    flex-direction: column;
  }

  .right-pane-body--code {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-body--browser {
    position: relative;
    display: flex;
    flex-direction: column;
    width: 100%;
    overflow: visible;
    padding: 0;
  }

  .right-pane-body--terminal {
    display: flex;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-body--plugin {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }


  .right-pane-source {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4) 0;
    background: transparent;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .right-pane-source-row {
    display: grid;
    grid-template-columns: 46px minmax(0, 1fr);
    align-items: start;
    min-width: 0;
  }

  .right-pane-source-line-number {
    position: sticky;
    left: 0;
    z-index: 1;
    padding: 0 10px 0 var(--space-2);
    background: transparent;
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    opacity: 0.46;
    text-align: right;
    user-select: none;
  }

  .right-pane-source-line {
    min-width: 0;
    padding: 0 var(--space-4) 0 var(--space-3);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    overflow-wrap: anywhere;
    tab-size: 2;
    white-space: pre-wrap;
  }

  /* ============ Diff 视图（与对话区共享 DiffCodeBlock） ============ */
  .right-pane-diff {
    display: flex;
    min-height: 0;
    flex: 1;
    overflow: hidden;
    padding: var(--space-4);
    background: transparent;
  }

  .right-pane-markdown {
    max-width: 880px;
    color: var(--foreground);
    line-height: 1.65;
  }

  .right-pane-state {
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    color: var(--foreground-muted);
    text-align: center;
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .right-pane-state--error { color: var(--error); }
  .right-pane-state--metadata { padding: var(--space-4); }

  .right-pane-meta-line {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .right-pane-image-wrap {
    position: relative;
    width: 100%;
    height: 100%;
    box-sizing: border-box;
  }

  .right-pane-image {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 100%;
    padding: var(--space-4);
    overflow: hidden;
    box-sizing: border-box;
    touch-action: none;
  }

  .right-pane-image.zoomed {
    cursor: grab;
  }

  .right-pane-image.dragging {
    cursor: grabbing;
  }

  .right-pane-image-el {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    transform-origin: center center;
    will-change: transform;
    user-select: none;
    -webkit-user-drag: none;
    /* 透明图片用棋盘格底衬出边界，避免与面板同色看不清 */
    background-image:
      linear-gradient(45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(-45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%),
      linear-gradient(-45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%);
    background-size: 16px 16px;
    background-position: 0 0, 0 8px, 8px -8px, -8px 0;
    border-radius: var(--radius-sm, 4px);
  }

  .right-pane-image-controls {
    position: absolute;
    bottom: var(--space-3);
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: 4px 6px;
    background: var(--surface-overlay, rgba(20, 20, 22, 0.82));
    border: var(--magi-hairline) solid var(--border-subtle, rgba(255, 255, 255, 0.12));
    border-radius: var(--radius-md, 8px);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
    backdrop-filter: blur(6px);
  }

  .image-zoom-btn,
  .image-zoom-level {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: var(--foreground-on-overlay, #f5f5f5);
    cursor: pointer;
    border-radius: var(--radius-sm, 4px);
  }

  .image-zoom-btn {
    width: 26px;
    height: 26px;
  }

  .image-zoom-level {
    min-width: 48px;
    height: 26px;
    padding: 0 6px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .image-zoom-btn:hover:not(:disabled),
  .image-zoom-level:hover {
    background: rgba(255, 255, 255, 0.14);
  }

  .image-zoom-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .right-pane-large-text {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .right-pane-summary-section {
    flex: 1;
    min-height: 100px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .right-pane-summary-title {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .right-pane-summary-content {
    flex: 1 1 0;
    min-height: 0;
    margin: 0;
    padding: var(--space-3);
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-1) 82%, var(--background));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .right-pane-notice {
    margin-bottom: var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
</style>
