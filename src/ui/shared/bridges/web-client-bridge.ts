import {
  agentUrl,
  dispatchAgentConnectionEvent,
  probeReachableAgentBaseUrl,
  resolveAgentBaseUrl,
} from '../../webview-svelte/src/web/agent-api';
import {
  getAgentLanAccessInfo,
  getAgentTunnelStatus,
  approveAgentChange,
  approveAllAgentChanges,
  addAgentAdr,
  addAgentCustomTool,
  addAgentFaq,
  addAgentMcpServer,
  addAgentRepository,
  answerAgentClarification,
  answerAgentWorkerQuestion,
  appendAgentTaskMessage,
  clearAgentNotifications,
  clearAgentProjectKnowledge,
  clearAgentAllTasks,
  confirmAgentRecovery,
  closeAgentSession,
  respondAgentInteraction,
  respondAgentToolAuthorization,
  connectAgentMcpServer,
  deleteAgentTask,
  deleteAgentQueuedTaskMessage,
  deleteAgentSession,
  deleteAgentAdr,
  deleteAgentFaq,
  deleteAgentLearning,
  deleteAgentMcpServer,
  deleteAgentRepository,
  disconnectAgentMcpServer,
  enhanceAgentPrompt,
    executeAgentTask,
    fetchAgentModelList,
    getAgentMcpServerTools,
    getAgentExecutionStats,
    getAgentRuntimeSettings,
    getAgentChangeDiff,
    getAgentFilePreview,
  interruptAgentTask,
  installAgentLocalSkill,
  installAgentSkill,
  loadAgentSkillLibrary,
  markAllAgentNotificationsRead,
  refreshAgentMcpTools,
  refreshAgentRepository,
  removeAgentNotification,
  removeAgentCustomTool,
  removeAgentInstructionSkill,
  renameAgentSession,
  resetAgentExecutionStats,
  resetAgentProfileConfig,
  saveAgentCurrentSession,
  saveAgentAuxiliaryConfig,
  saveAgentOrchestratorConfig,
  saveAgentProfileConfig,
  saveAgentSkillsConfig,
    saveAgentWorkerConfig,
    revertAgentChange,
  revertAgentMissionChanges,
  revertAllAgentChanges,
  resumeAgentTask,
  abandonAgentChain,
  testAgentAuxiliaryConnection,
  testAgentOrchestratorConnection,
  testAgentWorkerConnection,
  setAgentInteractionMode,
  startAgentTunnel,
  startAgentTask,
  stopAgentTunnel,
  updateAgentQueuedTaskMessage,
  updateAgentAdr,
  updateAgentFaq,
  updateAgentMcpServer,
  updateAgentRepository,
  updateAgentRuntimeSetting,
  updateAgentSkill,
  updateAllAgentSkills,
} from '../../webview-svelte/src/web/agent-api';
import type { ClientBridge, ClientBridgeMessage, SupportedLocale } from './client-bridge';
import {
  MessageCategory,
  MessageLifecycle,
  MessageType,
  type DataMessageType,
  type StandardMessage,
} from '../../../protocol/message-protocol';
import type { SessionBootstrapSnapshot } from '../../../shared/session-bootstrap';
import type {
  SettingsBootstrapPayload,
  SettingsBootstrapSnapshot,
  SettingsRuntimeSnapshot,
} from '../../../shared/settings-bootstrap';
import { buildSettingsBootstrapSnapshot } from '../../../shared/settings-bootstrap';

type BootstrapPayload = SessionBootstrapSnapshot & {
 agent?: {
   runtimeEpoch?: string;
 };
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
};

const listeners: Set<(message: ClientBridgeMessage) => void> = new Set();
let bridgeListenerRegistered = false;
let currentWorkspaceId = '';
let currentWorkspacePath = '';
let currentSessionId = '';
let currentRuntimeEpoch = '';
let cachedSettingsBootstrap: SettingsBootstrapPayload | null = null;
let cachedRuntimeSettings: SettingsRuntimeSnapshot | null = null;
let activeEventStream: EventSource | null = null;
let activeEventStreamKey = '';
let bridgeRecovering = false;
// fetchBootstrap 防重入：同一时刻只允许一个 bootstrap 请求在飞行中，
// 后续调用复用同一 Promise，避免重复 dispatchBootstrap 打乱 eventSeq 追踪。
let bootstrapInFlight: Promise<void> | null = null;
let settingsBootstrapInFlight: Promise<void> | null = null;
let recoveryAttempt = 0;
let recoveryTimer: number | null = null;
let recoveryInFlight: Promise<void> | null = null;

const RECOVERY_BASE_DELAY_MS = 1000;
const RECOVERY_MAX_DELAY_MS = 10_000;

function normalizeErrorMessage(error: unknown): string | undefined {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return undefined;
}

function emitRecoveringState(reason: string, error?: unknown): void {
  bridgeRecovering = true;
  dispatchAgentConnectionEvent({
    status: 'recovering',
    reason,
    error: normalizeErrorMessage(error),
    baseUrl: resolveAgentBaseUrl(),
  });
}

function emitConnectedState(reason: string, recovered: boolean): void {
  bridgeRecovering = false;
  dispatchAgentConnectionEvent({
    status: 'connected',
    reason,
    recovered,
    baseUrl: resolveAgentBaseUrl(),
  });
}

function clearRecoveryTimer(): void {
  if (recoveryTimer !== null) {
    window.clearTimeout(recoveryTimer);
    recoveryTimer = null;
  }
}

function ensureWindowListener(): void {
  if (bridgeListenerRegistered || typeof window === 'undefined') {
    return;
  }
  bridgeListenerRegistered = true;
  window.addEventListener('message', (event) => {
    const message = event.data as ClientBridgeMessage;
    emitMessage(message);
  });
  window.addEventListener('storage', (event) => {
    if (event.key !== 'magi-agent-base-url') {
      return;
    }
    closeEventStream();
    scheduleRecovery('agent_base_url_changed', undefined, true);
  });
  window.addEventListener('focus', () => {
    if (!activeEventStream && (currentWorkspaceId || currentWorkspacePath || currentSessionId)) {
      scheduleRecovery('window_focus', undefined, true);
    }
  });
}

function emitMessage(message: ClientBridgeMessage): void {
 // SSE 首帧 runtimeEpoch：检测后端代际变化，但禁止整页刷新。
 if (message.type === 'runtimeEpoch') {
   const incomingEpoch = typeof message.epoch === 'string' ? message.epoch : '';
   if (incomingEpoch && currentRuntimeEpoch && incomingEpoch !== currentRuntimeEpoch) {
     console.warn('[web-client-bridge] SSE runtimeEpoch 变化，后端已重启，执行无刷新桥恢复', {
       previous: currentRuntimeEpoch,
       current: incomingEpoch,
     });
     currentRuntimeEpoch = incomingEpoch;
     closeEventStream();
     scheduleRecovery('runtime_epoch_changed', undefined, true);
     return;
   }
   if (incomingEpoch) {
     currentRuntimeEpoch = incomingEpoch;
   }
   return; // runtimeEpoch 是内部控制消息，不广播给前端组件
 }
  listeners.forEach((listener) => {
    try {
      listener(message);
    } catch (error) {
      console.error('[web-client-bridge] 消息处理错误:', error);
    }
  });
}

function emitDataMessage(dataType: DataMessageType, payload: Record<string, unknown>): void {
  const now = Date.now();
  const message: StandardMessage = {
    id: `web-data-${dataType}-${now}`,
    traceId: `web-data-${dataType}`,
    category: MessageCategory.DATA,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [],
    metadata: {},
    timestamp: now,
    updatedAt: now,
    data: {
      dataType,
      payload,
    },
  };
  emitMessage({ type: 'unifiedMessage', message });
}

function getCurrentUrl(): URL | null {
  if (typeof window === 'undefined') {
    return null;
  }
  return new URL(window.location.href);
}

function resolveWorkspaceQuery(): { workspaceId: string; workspacePath: string; sessionId: string } {
  const currentUrl = getCurrentUrl();
  const workspaceId = currentUrl?.searchParams.get('workspaceId')?.trim() || localStorage.getItem('magi-workspace-id') || '';
  const workspacePath = currentUrl?.searchParams.get('workspacePath')?.trim() || localStorage.getItem('magi-workspace-path') || '';
  const sessionId = currentUrl?.searchParams.get('sessionId')?.trim() || localStorage.getItem('magi-session-id') || '';
  return { workspaceId, workspacePath, sessionId };
}

function persistWorkspaceBinding(workspaceId: string, workspacePath: string, sessionId: string): void {
  currentWorkspaceId = workspaceId;
  currentWorkspacePath = workspacePath;
  currentSessionId = sessionId;
  if (workspaceId) {
    localStorage.setItem('magi-workspace-id', workspaceId);
  }
  if (workspacePath) {
    localStorage.setItem('magi-workspace-path', workspacePath);
  }
  if (sessionId) {
    localStorage.setItem('magi-session-id', sessionId);
  } else {
    localStorage.removeItem('magi-session-id');
  }

  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return;
  }
  const nextUrl = new URL(currentUrl.toString());
  if (workspaceId) {
    nextUrl.searchParams.set('workspaceId', workspaceId);
  } else {
    nextUrl.searchParams.delete('workspaceId');
  }
  if (workspacePath) {
    nextUrl.searchParams.set('workspacePath', workspacePath);
  } else {
    nextUrl.searchParams.delete('workspacePath');
  }
  if (sessionId) {
    nextUrl.searchParams.set('sessionId', sessionId);
  } else {
    nextUrl.searchParams.delete('sessionId');
  }
  if (nextUrl.toString() !== currentUrl.toString()) {
    window.history.replaceState(window.history.state, '', nextUrl);
  }
}

function closeEventStream(): void {
  activeEventStream?.close();
  activeEventStream = null;
  activeEventStreamKey = '';
}

async function restoreBridgeState(reason: string, force = false): Promise<void> {
  if (recoveryInFlight) {
    return recoveryInFlight;
  }
  recoveryInFlight = (async () => {
    const recovered = bridgeRecovering || recoveryAttempt > 0;
    const reachableBaseUrl = await probeReachableAgentBaseUrl();
    if (!reachableBaseUrl) {
      throw new Error('无法连接 Local Agent，正在等待恢复。');
    }
    if (force) {
      cachedSettingsBootstrap = null;
      cachedRuntimeSettings = null;
    }
    await Promise.all([
      fetchBootstrap(),
      dispatchSettingsBootstrap(force),
    ]);
    clearRecoveryTimer();
    recoveryAttempt = 0;
    emitConnectedState(reason, recovered);
  })().finally(() => {
    recoveryInFlight = null;
  });
  return recoveryInFlight;
}

function scheduleRecovery(reason: string, error?: unknown, immediate = false): void {
  emitRecoveringState(reason, error);
  if (recoveryTimer !== null || recoveryInFlight) {
    return;
  }
  const delay = immediate
    ? 0
    : Math.min(RECOVERY_MAX_DELAY_MS, RECOVERY_BASE_DELAY_MS * (2 ** Math.min(recoveryAttempt, 3)));
  recoveryTimer = window.setTimeout(() => {
    recoveryTimer = null;
    void restoreBridgeState(reason, true).catch((recoveryError) => {
      recoveryAttempt += 1;
      scheduleRecovery('retry', recoveryError);
    });
  }, delay);
}

function ensureEventStream(): void {
  if (typeof window === 'undefined') {
    return;
  }
  const query = new URLSearchParams();
  if (currentWorkspaceId) {
    query.set('workspaceId', currentWorkspaceId);
  }
  if (currentWorkspacePath) {
    query.set('workspacePath', currentWorkspacePath);
  }
  if (currentSessionId) {
    query.set('sessionId', currentSessionId);
  }
  const nextKey = query.toString();
  if (!nextKey) {
    closeEventStream();
    return;
  }
  if (activeEventStream && activeEventStreamKey === nextKey) {
    return;
  }
  closeEventStream();
  const stream = new EventSource(agentUrl('/api/events', nextKey));
  stream.onopen = () => {
    if (activeEventStream !== stream) {
      return;
    }
    if (bridgeRecovering && !recoveryInFlight) {
      void restoreBridgeState('event_stream_open', true).catch((error) => {
        scheduleRecovery('event_stream_open', error);
      });
    }
  };
  stream.onmessage = (event) => {
    try {
      emitMessage(JSON.parse(event.data) as ClientBridgeMessage);
    } catch (error) {
      console.error('[web-client-bridge] 事件流消息解析失败:', error);
    }
  };
  stream.onerror = () => {
    stream.close();
    if (activeEventStream === stream) {
      activeEventStream = null;
      activeEventStreamKey = '';
      scheduleRecovery('event_stream_error');
    }
  };
  activeEventStream = stream;
  activeEventStreamKey = nextKey;
}

function dispatchBootstrap(payload: BootstrapPayload): void {
 // 检测 runtimeEpoch 代际变化：后端重启后执行无刷新状态重建，不允许整页刷新打断用户会话。
 const incomingEpoch = payload.agent?.runtimeEpoch || '';
 if (incomingEpoch && currentRuntimeEpoch && incomingEpoch !== currentRuntimeEpoch) {
   console.warn('[web-client-bridge] runtimeEpoch 变化，后端已重启，执行无刷新状态重建', {
     previous: currentRuntimeEpoch,
     current: incomingEpoch,
   });
 }
 if (incomingEpoch) {
   currentRuntimeEpoch = incomingEpoch;
 }
  persistWorkspaceBinding(payload.workspace.workspaceId, payload.workspace.rootPath, payload.sessionId);
  ensureEventStream();
  emitDataMessage('sessionBootstrapLoaded', payload as unknown as Record<string, unknown>);
}

async function fetchBootstrap(): Promise<void> {
  // 防重入：如果已有 bootstrap 请求在飞行中，直接复用
  if (bootstrapInFlight) {
    return bootstrapInFlight;
  }
  const doFetch = async (): Promise<void> => {
    const { workspaceId, workspacePath, sessionId } = resolveWorkspaceQuery();
    const query = new URLSearchParams();
    if (workspaceId) {
      query.set('workspaceId', workspaceId);
    }
    if (workspacePath) {
      query.set('workspacePath', workspacePath);
    }
    if (sessionId) {
      query.set('sessionId', sessionId);
    }
    const response = await fetch(agentUrl('/api/bootstrap', query.toString()));
    if (!response.ok) {
      throw new Error(`bootstrap failed: ${response.status}`);
    }
    const payload = await response.json() as BootstrapPayload;
    dispatchBootstrap(payload);
  };
  bootstrapInFlight = doFetch().finally(() => {
    bootstrapInFlight = null;
  });
  return bootstrapInFlight;
}

async function fetchSettingsBootstrap(force = false): Promise<SettingsBootstrapPayload> {
  if (!force && cachedSettingsBootstrap) {
    return cachedSettingsBootstrap;
  }
  const response = await fetch(agentUrl('/api/settings/bootstrap'));
  if (!response.ok) {
    throw new Error(`settings bootstrap failed: ${response.status}`);
  }
  cachedSettingsBootstrap = await response.json() as SettingsBootstrapPayload;
  return cachedSettingsBootstrap;
}

async function fetchRuntimeSettings(force = false): Promise<SettingsRuntimeSnapshot> {
  if (!force && cachedRuntimeSettings) {
    return cachedRuntimeSettings;
  }
  const payload = await getAgentRuntimeSettings();
  cachedRuntimeSettings = {
    locale: payload.locale === 'en-US' ? 'en-US' : 'zh-CN',
    deepTask: Boolean(payload.deepTask),
  };
  return cachedRuntimeSettings;
}

async function dispatchSettingsBootstrap(force = false): Promise<void> {
  if (!force && settingsBootstrapInFlight) {
    return settingsBootstrapInFlight;
  }
  const doDispatch = async (): Promise<void> => {
    const [payload, runtimeSettings] = await Promise.all([
      fetchSettingsBootstrap(force),
      fetchRuntimeSettings(force),
    ]);
    const snapshot: SettingsBootstrapSnapshot = buildSettingsBootstrapSnapshot(payload, runtimeSettings);
    emitDataMessage('settingsBootstrapLoaded', snapshot as unknown as Record<string, unknown>);
  };
  settingsBootstrapInFlight = doDispatch().finally(() => {
    settingsBootstrapInFlight = null;
  });
  return settingsBootstrapInFlight;
}

async function dispatchExecutionStats(): Promise<void> {
  const payload = await getAgentExecutionStats();
  emitDataMessage('executionStatsUpdate', payload as unknown as Record<string, unknown>);
}

async function dispatchProjectKnowledge(): Promise<void> {
  const query = new URLSearchParams();
  if (currentWorkspaceId) {
    query.set('workspaceId', currentWorkspaceId);
  }
  if (currentWorkspacePath) {
    query.set('workspacePath', currentWorkspacePath);
  }
  const response = await fetch(agentUrl('/api/knowledge', query.toString()));
  if (!response.ok) {
    throw new Error(`project knowledge failed: ${response.status}`);
  }
  const payload = await response.json() as Record<string, unknown>;
  emitDataMessage('projectKnowledgeLoaded', payload);
}

async function emitKnowledgePayload(): Promise<void> {
  await dispatchProjectKnowledge();
}

async function createSession(): Promise<void> {
  const response = await fetch(agentUrl('/api/session/new'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      workspaceId: currentWorkspaceId,
      workspacePath: currentWorkspacePath,
    }),
  });
  if (!response.ok) {
    throw new Error(`create session failed: ${response.status}`);
  }
  dispatchBootstrap(await response.json() as BootstrapPayload);
}

async function switchSession(sessionId: string): Promise<void> {
  const response = await fetch(agentUrl('/api/session/switch'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      workspaceId: currentWorkspaceId,
      workspacePath: currentWorkspacePath,
      sessionId,
    }),
  });
  if (!response.ok) {
    throw new Error(`switch session failed: ${response.status}`);
  }
  dispatchBootstrap(await response.json() as BootstrapPayload);
}

async function deleteSession(sessionId: string): Promise<void> {
  const payload = await deleteAgentSession(sessionId);
  dispatchBootstrap(payload as unknown as BootstrapPayload);
}

async function renameSession(sessionId: string, name: string): Promise<void> {
  const payload = await renameAgentSession(sessionId, name);
  dispatchBootstrap(payload as unknown as BootstrapPayload);
}

async function closeSession(sessionId: string): Promise<void> {
  const payload = await closeAgentSession(sessionId);
  dispatchBootstrap(payload as unknown as BootstrapPayload);
}

async function saveCurrentSession(): Promise<void> {
  const payload = await saveAgentCurrentSession();
  dispatchBootstrap(payload as unknown as BootstrapPayload);
}

async function executeTask(prompt: string, requestId?: string): Promise<void> {
  await executeAgentTask(prompt, requestId);
}

async function interruptTask(): Promise<void> {
  await interruptAgentTask();
}

async function clearAllTasks(): Promise<void> {
  await clearAgentAllTasks();
}

async function startTask(taskId: string): Promise<void> {
  await startAgentTask(taskId);
}

async function resumeTask(taskId: string): Promise<void> {
  await resumeAgentTask(taskId);
}

async function deleteTask(taskId: string): Promise<void> {
  await deleteAgentTask(taskId);
}

async function setInteractionMode(mode: string): Promise<void> {
  await setAgentInteractionMode(mode);
}

async function appendTaskMessage(taskId: string, content: string): Promise<void> {
  await appendAgentTaskMessage(taskId, content);
}

async function updateQueuedTaskMessage(queueId: string, content: string): Promise<void> {
  await updateAgentQueuedTaskMessage(queueId, content);
}

async function deleteQueuedTaskMessage(queueId: string): Promise<void> {
  await deleteAgentQueuedTaskMessage(queueId);
}

async function confirmRecovery(decision: 'retry' | 'rollback' | 'continue'): Promise<void> {
  await confirmAgentRecovery(decision);
}

async function respondToolAuthorization(requestId: string, allowed: boolean): Promise<void> {
  await respondAgentToolAuthorization(requestId, allowed);
}

async function respondInteraction(requestId: string, response: unknown): Promise<void> {
  await respondAgentInteraction(requestId, response);
}

async function submitClarificationAnswer(
  answers: Record<string, string> | null,
  additionalInfo?: string | null,
): Promise<void> {
  await answerAgentClarification(answers, additionalInfo);
}

async function submitWorkerQuestionAnswer(answer: string | null): Promise<void> {
  await answerAgentWorkerQuestion(answer);
}

function escapePreviewHtml(content: string): string {
  return content
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

function openPreviewWindow(title: string, subtitle: string, content: string, mode: 'file' | 'diff'): void {
  const popup = window.open('', '_blank', 'noopener,noreferrer');
  if (!popup) {
    throw new Error('浏览器阻止了预览窗口，请允许当前站点打开新窗口。');
  }
  const escapedTitle = escapePreviewHtml(title);
  const escapedSubtitle = escapePreviewHtml(subtitle);
  const escapedContent = escapePreviewHtml(content);
  const bodyClass = mode === 'diff' ? 'diff' : 'file';
  popup.document.write(`<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${escapedTitle}</title>
  <style>
    :root { color-scheme: light dark; }
    body { margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #0f172a; color: #e2e8f0; }
    .wrap { padding: 20px; }
    .title { font-size: 20px; font-weight: 700; margin: 0 0 4px; }
    .subtitle { font-size: 12px; color: #94a3b8; margin: 0 0 16px; }
    pre { margin: 0; padding: 16px; border-radius: 12px; background: #111827; border: 1px solid rgba(148,163,184,.18); overflow: auto; line-height: 1.55; }
    .diff pre { background: #0b1220; }
    code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; white-space: pre-wrap; word-break: break-word; }
  </style>
</head>
<body class="${bodyClass}">
  <div class="wrap">
    <h1 class="title">${escapedTitle}</h1>
    <p class="subtitle">${escapedSubtitle}</p>
    <pre><code>${escapedContent}</code></pre>
  </div>
</body>
</html>`);
  popup.document.close();
}

function openMermaidPreview(code: string, title?: string): void {
  openPreviewWindow(title?.trim() || 'Mermaid 图表', 'Mermaid 源码预览', code, 'file');
}

function logUnsupportedInteraction(messageType: string): void {
  console.warn(`[web-client-bridge] 当前 Agent 运行时尚未接入交互消息: ${messageType}`);
}

async function openFilePreview(filePath: string): Promise<void> {
  const payload = await getAgentFilePreview(filePath);
  openPreviewWindow(payload.filePath, '文件预览', payload.content || '', 'file');
}

async function openDiffPreview(filePath: string): Promise<void> {
  const payload = await getAgentChangeDiff(filePath);
  openPreviewWindow(payload.filePath, '差异预览', payload.diff || '', 'diff');
}

async function updateSetting(key: string, value: unknown): Promise<void> {
  const payload = await updateAgentRuntimeSetting(key, value);
  cachedRuntimeSettings = {
    locale: payload.locale,
    deepTask: payload.deepTask,
  };
  if (key === 'locale') {
    localStorage.setItem('magi-locale', payload.locale);
  }
  await dispatchSettingsBootstrap(true);
}

async function resetExecutionStats(): Promise<void> {
  await resetAgentExecutionStats();
  await dispatchExecutionStats();
}

async function markAllNotificationsRead(): Promise<void> {
  const payload = await markAllAgentNotificationsRead();
  emitDataMessage('sessionNotificationsLoaded', payload as unknown as Record<string, unknown>);
}

async function clearAllNotifications(): Promise<void> {
  const payload = await clearAgentNotifications();
  emitDataMessage('sessionNotificationsLoaded', payload as unknown as Record<string, unknown>);
}

async function removeNotification(notificationId: string): Promise<void> {
  const payload = await removeAgentNotification(notificationId);
  emitDataMessage('sessionNotificationsLoaded', payload as unknown as Record<string, unknown>);
}

async function enhancePrompt(prompt: string): Promise<void> {
  const payload = await enhanceAgentPrompt(prompt);
  emitDataMessage('promptEnhanced', payload);
}

async function saveWorkerConfig(worker: string, config: Record<string, unknown>): Promise<void> {
  await saveAgentWorkerConfig(worker, config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
}

async function saveProfileConfig(data: Record<string, unknown>): Promise<void> {
  const payload = await saveAgentProfileConfig(data);
  emitDataMessage('profileConfigSaved', payload);
  await dispatchSettingsBootstrap(true);
}

async function resetProfileConfig(): Promise<void> {
  const payload = await resetAgentProfileConfig();
  emitDataMessage('profileConfigReset', payload);
  await dispatchSettingsBootstrap(true);
}

async function saveOrchestratorConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentOrchestratorConfig(config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
}

async function saveAuxiliaryConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentAuxiliaryConfig(config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
}

async function testWorkerConnection(worker: string, config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentWorkerConnection(worker, config);
  emitDataMessage('workerConnectionTestResult', payload);
}

async function testOrchestratorConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentOrchestratorConnection(config);
  emitDataMessage('orchestratorConnectionTestResult', payload);
}

async function testAuxiliaryConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentAuxiliaryConnection(config);
  emitDataMessage('auxiliaryConnectionTestResult', payload);
}

async function fetchModelList(config: Record<string, unknown>, target: string): Promise<void> {
  const payload = await fetchAgentModelList(config, target);
  emitDataMessage('modelListFetched', payload);
}

async function addMcpServer(server: Record<string, unknown>): Promise<void> {
  const payload = await addAgentMcpServer(server);
  emitDataMessage('mcpServerAdded', payload);
  await dispatchSettingsBootstrap(true);
}

async function updateMcpServer(serverId: string, updates: Record<string, unknown>): Promise<void> {
  const payload = await updateAgentMcpServer(serverId, updates);
  emitDataMessage('mcpServerUpdated', payload);
  await dispatchSettingsBootstrap(true);
}

async function deleteMcpServer(serverId: string): Promise<void> {
  const payload = await deleteAgentMcpServer(serverId);
  emitDataMessage('mcpServerDeleted', payload);
  await dispatchSettingsBootstrap(true);
}

async function getMcpServerTools(serverId: string): Promise<void> {
  const payload = await getAgentMcpServerTools(serverId);
  emitDataMessage('mcpServerTools', payload);
}

async function refreshMcpTools(serverId: string): Promise<void> {
  const payload = await refreshAgentMcpTools(serverId);
  emitDataMessage('mcpToolsRefreshed', payload);
  await dispatchSettingsBootstrap(true);
}

async function connectMcpServer(serverId: string): Promise<void> {
  await connectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
}

async function disconnectMcpServer(serverId: string): Promise<void> {
  await disconnectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
}

async function addRepository(url: string): Promise<void> {
  const payload = await addAgentRepository(url);
  emitDataMessage('repositoryAdded', payload);
  await dispatchSettingsBootstrap(true);
}

async function updateRepository(repositoryId: string, updates: Record<string, unknown>): Promise<void> {
  await updateAgentRepository(repositoryId, updates);
  await dispatchSettingsBootstrap(true);
}

async function deleteRepository(repositoryId: string): Promise<void> {
  const payload = await deleteAgentRepository(repositoryId);
  emitDataMessage('repositoryDeleted', payload);
  await dispatchSettingsBootstrap(true);
}

async function refreshRepository(repositoryId: string): Promise<void> {
  const payload = await refreshAgentRepository(repositoryId);
  emitDataMessage('repositoryRefreshed', payload);
  await dispatchSettingsBootstrap(true);
}

async function loadSkillLibrary(): Promise<void> {
  const payload = await loadAgentSkillLibrary();
  emitDataMessage('skillLibraryLoaded', payload);
}

async function installSkill(skillId: string): Promise<void> {
  try {
    const payload = await installAgentSkill(skillId);
    emitDataMessage('skillInstalled', payload);
    await dispatchSettingsBootstrap(true);
    await loadSkillLibrary();
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      skillId,
      error: error instanceof Error ? error.message : String(error),
      source: 'repository',
    });
  }
}

async function installLocalSkill(): Promise<void> {
  try {
    const payload = await installAgentLocalSkill();
    emitDataMessage('skillInstalled', payload);
    await dispatchSettingsBootstrap(true);
    await loadSkillLibrary();
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      error: error instanceof Error ? error.message : String(error),
      source: 'local',
    });
  }
}

async function saveSkillsConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentSkillsConfig(config);
  await dispatchSettingsBootstrap(true);
}

async function addCustomTool(tool: Record<string, unknown>): Promise<void> {
  const payload = await addAgentCustomTool(tool);
  emitDataMessage('customToolAdded', payload);
  await dispatchSettingsBootstrap(true);
}

async function removeCustomTool(toolName: string): Promise<void> {
  const payload = await removeAgentCustomTool(toolName);
  emitDataMessage('customToolRemoved', payload);
  await dispatchSettingsBootstrap(true);
}

async function removeInstructionSkill(skillName: string): Promise<void> {
  const payload = await removeAgentInstructionSkill(skillName);
  emitDataMessage('instructionSkillRemoved', payload);
  await dispatchSettingsBootstrap(true);
}

async function updateSkill(skillName: string): Promise<void> {
  const payload = await updateAgentSkill(skillName);
  emitDataMessage('skillUpdated', payload);
  await dispatchSettingsBootstrap(true);
}

async function updateAllSkills(): Promise<void> {
  const payload = await updateAllAgentSkills();
  emitDataMessage('allSkillsUpdated', payload);
  await dispatchSettingsBootstrap(true);
}

async function clearProjectKnowledge(): Promise<void> {
  await clearAgentProjectKnowledge();
  await emitKnowledgePayload();
}

async function deleteAdr(id: string): Promise<void> {
  await deleteAgentAdr(id);
  await emitKnowledgePayload();
}

async function deleteFaq(id: string): Promise<void> {
  await deleteAgentFaq(id);
  await emitKnowledgePayload();
}

async function deleteLearning(id: string): Promise<void> {
  await deleteAgentLearning(id);
  await emitKnowledgePayload();
}

export function createWebClientBridge(): ClientBridge {
  ensureWindowListener();

  return {
    kind: 'web',
    postMessage(message: ClientBridgeMessage): void {
      switch (message.type) {
        case 'webviewReady':
        case 'getState':
        case 'requestState':
          void restoreBridgeState('request_state').catch((error) => {
            console.error('[web-client-bridge] bootstrap 失败:', error);
            scheduleRecovery('request_state', error);
          });
          return;
        case 'loadSettingsBootstrap':
          void dispatchSettingsBootstrap(Boolean(message.force)).catch((error) => {
            console.error('[web-client-bridge] settings 配置加载失败:', error);
          });
          return;
        case 'saveProfileConfig':
          if (message.data && typeof message.data === 'object') {
            void saveProfileConfig(message.data as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 保存画像配置失败:', error);
            });
          }
          return;
        case 'resetProfileConfig':
          void resetProfileConfig().catch((error) => {
            console.error('[web-client-bridge] 重置画像配置失败:', error);
          });
          return;
        case 'saveWorkerConfig':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void saveWorkerConfig(message.worker, message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 保存 worker 配置失败:', error);
            });
          }
          return;
        case 'saveOrchestratorConfig':
          if (message.config && typeof message.config === 'object') {
            void saveOrchestratorConfig(message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 保存编排模型配置失败:', error);
            });
          }
          return;
        case 'saveAuxiliaryConfig':
          if (message.config && typeof message.config === 'object') {
            void saveAuxiliaryConfig(message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 保存辅助模型配置失败:', error);
            });
          }
          return;
        case 'testWorkerConnection':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void testWorkerConnection(message.worker, message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 测试 worker 连接失败:', error);
            });
          }
          return;
        case 'testOrchestratorConnection':
          if (message.config && typeof message.config === 'object') {
            void testOrchestratorConnection(message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 测试编排模型连接失败:', error);
            });
          }
          return;
        case 'testAuxiliaryConnection':
          if (message.config && typeof message.config === 'object') {
            void testAuxiliaryConnection(message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 测试辅助模型连接失败:', error);
            });
          }
          return;
        case 'fetchModelList':
          if (message.config && typeof message.config === 'object' && typeof message.target === 'string') {
            void fetchModelList(message.config as Record<string, unknown>, message.target).catch((error) => {
              console.error('[web-client-bridge] 获取模型列表失败:', error);
            });
          }
          return;
        case 'addMCPServer':
          if (message.server && typeof message.server === 'object') {
            void addMcpServer(message.server as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 添加 MCP 服务器失败:', error);
            });
          }
          return;
        case 'updateMCPServer':
          if (typeof message.serverId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateMcpServer(message.serverId, message.updates as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 更新 MCP 服务器失败:', error);
            });
          }
          return;
        case 'deleteMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void deleteMcpServer(message.serverId).catch((error) => {
              console.error('[web-client-bridge] 删除 MCP 服务器失败:', error);
            });
          }
          return;
        case 'getMCPServerTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void getMcpServerTools(message.serverId).catch((error) => {
              console.error('[web-client-bridge] 获取 MCP 工具失败:', error);
            });
          }
          return;
        case 'refreshMCPTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void refreshMcpTools(message.serverId).catch((error) => {
              console.error('[web-client-bridge] 刷新 MCP 工具失败:', error);
            });
          }
          return;
        case 'addRepository':
          if (typeof message.url === 'string' && message.url.trim()) {
            void addRepository(message.url).catch((error) => {
              console.error('[web-client-bridge] 添加仓库失败:', error);
            });
          }
          return;
        case 'updateRepository':
          if (typeof message.repositoryId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateRepository(message.repositoryId, message.updates as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 更新仓库失败:', error);
            });
          }
          return;
        case 'deleteRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void deleteRepository(message.repositoryId).catch((error) => {
              console.error('[web-client-bridge] 删除仓库失败:', error);
            });
          }
          return;
        case 'refreshRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void refreshRepository(message.repositoryId).catch((error) => {
              console.error('[web-client-bridge] 刷新仓库失败:', error);
            });
          }
          return;
        case 'loadSkillLibrary':
          void loadSkillLibrary().catch((error) => {
            console.error('[web-client-bridge] 加载技能库失败:', error);
          });
          return;
        case 'installSkill':
          if (typeof message.skillId === 'string' && message.skillId.trim()) {
            void installSkill(message.skillId).catch((error) => {
              console.error('[web-client-bridge] 安装技能失败:', error);
            });
          }
          return;
        case 'installLocalSkill':
          void installLocalSkill().catch((error) => {
            console.error('[web-client-bridge] 安装本地技能失败:', error);
          });
          return;
        case 'removeCustomTool':
          if (typeof message.toolName === 'string' && message.toolName.trim()) {
            void removeCustomTool(message.toolName).catch((error) => {
              console.error('[web-client-bridge] 删除自定义工具失败:', error);
            });
          }
          return;
        case 'removeInstructionSkill':
          if (typeof message.skillName === 'string' && message.skillName.trim()) {
            void removeInstructionSkill(message.skillName).catch((error) => {
              console.error('[web-client-bridge] 删除指令技能失败:', error);
            });
          }
          return;
        case 'updateSkill':
          if (typeof message.skillName === 'string' && message.skillName.trim()) {
            void updateSkill(message.skillName).catch((error) => {
              console.error('[web-client-bridge] 更新技能失败:', error);
            });
          }
          return;
        case 'updateAllSkills':
          void updateAllSkills().catch((error) => {
            console.error('[web-client-bridge] 更新全部技能失败:', error);
          });
          return;
        case 'newSession':
          void createSession().catch((error) => {
            console.error('[web-client-bridge] 新建会话失败:', error);
          });
          return;
        case 'saveCurrentSession':
          void saveCurrentSession().catch((error) => {
            console.error('[web-client-bridge] 保存当前会话失败:', error);
          });
          return;
        case 'markAllNotificationsRead':
          void markAllNotificationsRead().catch((error) => {
            console.error('[web-client-bridge] 标记通知已读失败:', error);
          });
          return;
        case 'clearAllNotifications':
          void clearAllNotifications().catch((error) => {
            console.error('[web-client-bridge] 清空通知失败:', error);
          });
          return;
        case 'removeNotification':
          if (typeof message.notificationId === 'string' && message.notificationId.trim()) {
            void removeNotification(message.notificationId).catch((error) => {
              console.error('[web-client-bridge] 删除通知失败:', error);
            });
          }
          return;
        case 'executeTask':
          if (typeof message.prompt === 'string' && message.prompt.trim()) {
            void executeTask(
              message.prompt,
              typeof message.requestId === 'string' ? message.requestId : undefined,
            ).catch((error) => {
              console.error('[web-client-bridge] 执行任务失败:', error);
            });
          }
          return;
        case 'appendMessage':
          if (typeof message.content === 'string' && message.content.trim()) {
            void appendTaskMessage(
              typeof message.taskId === 'string' ? message.taskId : '',
              message.content,
            ).catch((error) => {
              console.error('[web-client-bridge] 追加消息失败:', error);
            });
          }
          return;
        case 'updateQueuedMessage':
          if (
            typeof message.queueId === 'string' && message.queueId.trim()
            && typeof message.content === 'string'
          ) {
            void updateQueuedTaskMessage(message.queueId, message.content).catch((error) => {
              console.error('[web-client-bridge] 更新暂存消息失败:', error);
            });
          }
          return;
        case 'deleteQueuedMessage':
          if (typeof message.queueId === 'string' && message.queueId.trim()) {
            void deleteQueuedTaskMessage(message.queueId).catch((error) => {
              console.error('[web-client-bridge] 删除暂存消息失败:', error);
            });
          }
          return;
        case 'interruptTask':
          void interruptTask().catch((error) => {
            console.error('[web-client-bridge] 中断任务失败:', error);
          });
          return;
        case 'continueTask':
          if (typeof message.prompt === 'string' && message.prompt.trim()) {
            void executeTask(message.prompt).catch((error) => {
              console.error('[web-client-bridge] 继续任务失败:', error);
            });
          }
          return;
        case 'pauseTask':
          void interruptTask().catch((error) => {
            console.error('[web-client-bridge] 暂停任务失败:', error);
          });
          return;
        case 'startTask':
          if (typeof message.taskId === 'string' && message.taskId.trim()) {
            void startTask(message.taskId).catch((error) => {
              console.error('[web-client-bridge] 启动任务失败:', error);
            });
          }
          return;
        case 'resumeTask':
          if (typeof message.taskId === 'string' && message.taskId.trim()) {
            void resumeTask(message.taskId).catch((error) => {
              console.error('[web-client-bridge] 恢复任务失败:', error);
            });
          }
          return;
        case 'deleteTask':
          if (typeof message.taskId === 'string' && message.taskId.trim()) {
            void deleteTask(message.taskId).catch((error) => {
              console.error('[web-client-bridge] 删除任务失败:', error);
            });
          }
          return;
        case 'abandonChain':
          if (typeof message.chainId === 'string' && message.chainId.trim()) {
            void abandonAgentChain(message.chainId).catch((error) => {
              console.error('[web-client-bridge] 放弃执行链失败:', error);
            });
          }
          return;
        case 'clearAllTasks':
          void clearAllTasks().catch((error) => {
            console.error('[web-client-bridge] 清空任务失败:', error);
          });
          return;
        case 'switchSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void switchSession(message.sessionId).catch((error) => {
              console.error('[web-client-bridge] 切换会话失败:', error);
            });
          }
          return;
        case 'renameSession':
          if (
            typeof message.sessionId === 'string' && message.sessionId.trim()
            && typeof message.name === 'string' && message.name.trim()
          ) {
            void renameSession(message.sessionId, message.name).catch((error) => {
              console.error('[web-client-bridge] 重命名会话失败:', error);
            });
          }
          return;
        case 'closeSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void closeSession(message.sessionId).catch((error) => {
              console.error('[web-client-bridge] 关闭会话失败:', error);
            });
          }
          return;
        case 'deleteSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void deleteSession(message.sessionId).catch((error) => {
              console.error('[web-client-bridge] 删除会话失败:', error);
            });
          }
          return;
        case 'updateSetting':
          if (typeof message.key === 'string' && (message.key === 'locale' || message.key === 'deepTask')) {
            void updateSetting(message.key, message.value).catch((error) => {
              console.error('[web-client-bridge] 更新设置失败:', error);
            });
          }
          return;
        case 'setInteractionMode':
          if (typeof message.mode === 'string' && message.mode.trim()) {
            void setInteractionMode(message.mode).catch((error) => {
              console.error('[web-client-bridge] 更新交互模式失败:', error);
            });
          }
          return;
        case 'requestExecutionStats':
          void dispatchExecutionStats().catch((error) => {
            console.error('[web-client-bridge] 执行统计加载失败:', error);
          });
          return;
        case 'resetExecutionStats':
          void resetExecutionStats().catch((error) => {
            console.error('[web-client-bridge] 重置执行统计失败:', error);
          });
          return;
        case 'getLanAccessInfo': {
          void getAgentLanAccessInfo().then((payload) => {
            emitDataMessage('lanAccessInfo', { ...payload });
          }).catch((error) => {
            console.error('[web-client-bridge] 局域网地址加载失败:', error);
          });
          return;
        }
        case 'getTunnelStatus': {
          void getAgentTunnelStatus().then((payload) => {
            emitDataMessage('tunnelState', { ...payload });
          }).catch((error) => {
            console.error('[web-client-bridge] 隧道状态加载失败:', error);
          });
          return;
        }
        case 'startTunnel': {
          void startAgentTunnel().then((payload) => {
            emitDataMessage('tunnelState', { ...payload });
          }).catch((error) => {
            console.error('[web-client-bridge] 启动隧道失败:', error);
          });
          return;
        }
        case 'stopTunnel': {
          void stopAgentTunnel().then((payload) => {
            emitDataMessage('tunnelState', { ...payload });
          }).catch((error) => {
            console.error('[web-client-bridge] 停止隧道失败:', error);
          });
          return;
        }
        case 'openLink':
          if (typeof message.url === 'string' && message.url.trim()) {
            window.open(message.url, '_blank', 'noopener,noreferrer');
          }
          return;
        case 'openMermaidPanel':
          if (typeof message.code === 'string' && message.code.trim()) {
            openMermaidPreview(message.code, typeof message.title === 'string' ? message.title : undefined);
          }
          return;
        case 'openFile':
          {
            const filePath = typeof message.filepath === 'string' && message.filepath.trim()
              ? message.filepath
              : (typeof message.filePath === 'string' ? message.filePath : '');
            if (filePath.trim()) {
              void openFilePreview(filePath).catch((error) => {
                console.error('[web-client-bridge] 打开文件预览失败:', error);
              });
            }
          }
          return;
        case 'viewDiff':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            void openDiffPreview(message.filePath).catch((error) => {
              console.error('[web-client-bridge] 打开差异预览失败:', error);
            });
          }
          return;
        case 'approveChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            void approveAgentChange(message.filePath).then(() => fetchBootstrap()).catch((error) => {
              console.error('[web-client-bridge] 批准变更失败:', error);
            });
          }
          return;
        case 'revertChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            void revertAgentChange(message.filePath).then(() => fetchBootstrap()).catch((error) => {
              console.error('[web-client-bridge] 还原变更失败:', error);
            });
          }
          return;
        case 'approveAllChanges':
          void approveAllAgentChanges().then(() => fetchBootstrap()).catch((error) => {
            console.error('[web-client-bridge] 批准全部变更失败:', error);
          });
          return;
        case 'revertAllChanges':
          void revertAllAgentChanges().then(() => fetchBootstrap()).catch((error) => {
            console.error('[web-client-bridge] 还原全部变更失败:', error);
          });
          return;
        case 'revertMission':
          if (typeof message.missionId === 'string' && message.missionId.trim()) {
            void revertAgentMissionChanges(message.missionId).then(() => fetchBootstrap()).catch((error) => {
              console.error('[web-client-bridge] 还原轮次变更失败:', error);
            });
          }
          return;
        case 'getProjectKnowledge':
          void dispatchProjectKnowledge().catch((error) => {
            console.error('[web-client-bridge] 项目知识加载失败:', error);
          });
          return;
        case 'addADR':
          if (message.adr && typeof message.adr === 'object') {
            void addAgentAdr(message.adr as Record<string, unknown>).then(() => emitKnowledgePayload()).catch((error) => {
              console.error('[web-client-bridge] 添加 ADR 失败:', error);
            });
          }
          return;
        case 'updateADR':
          if (typeof message.id === 'string' && message.updates && typeof message.updates === 'object') {
            void updateAgentAdr(message.id, message.updates as Record<string, unknown>).then(() => emitKnowledgePayload()).catch((error) => {
              console.error('[web-client-bridge] 更新 ADR 失败:', error);
            });
          }
          return;
        case 'addFAQ':
          if (message.faq && typeof message.faq === 'object') {
            void addAgentFaq(message.faq as Record<string, unknown>).then(() => emitKnowledgePayload()).catch((error) => {
              console.error('[web-client-bridge] 添加 FAQ 失败:', error);
            });
          }
          return;
        case 'updateFAQ':
          if (typeof message.id === 'string' && message.updates && typeof message.updates === 'object') {
            void updateAgentFaq(message.id, message.updates as Record<string, unknown>).then(() => emitKnowledgePayload()).catch((error) => {
              console.error('[web-client-bridge] 更新 FAQ 失败:', error);
            });
          }
          return;
        case 'clearProjectKnowledge':
          void clearProjectKnowledge().catch((error) => {
            console.error('[web-client-bridge] 清空项目知识失败:', error);
          });
          return;
        case 'deleteADR':
          if (typeof message.id === 'string' && message.id.trim()) {
            void deleteAdr(message.id).catch((error) => {
              console.error('[web-client-bridge] 删除 ADR 失败:', error);
            });
          }
          return;
        case 'deleteFAQ':
          if (typeof message.id === 'string' && message.id.trim()) {
            void deleteFaq(message.id).catch((error) => {
              console.error('[web-client-bridge] 删除 FAQ 失败:', error);
            });
          }
          return;
        case 'deleteLearning':
          if (typeof message.id === 'string' && message.id.trim()) {
            void deleteLearning(message.id).catch((error) => {
              console.error('[web-client-bridge] 删除经验失败:', error);
            });
          }
          return;
        case 'connectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void connectMcpServer(message.serverId).catch((error) => {
              console.error('[web-client-bridge] 连接 MCP 服务器失败:', error);
            });
          }
          return;
        case 'disconnectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void disconnectMcpServer(message.serverId).catch((error) => {
              console.error('[web-client-bridge] 断开 MCP 服务器失败:', error);
            });
          }
          return;
        case 'saveSkillsConfig':
          if (message.config && typeof message.config === 'object') {
            void saveSkillsConfig(message.config as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 保存技能配置失败:', error);
            });
          }
          return;
        case 'addCustomTool':
          if (message.tool && typeof message.tool === 'object') {
            void addCustomTool(message.tool as Record<string, unknown>).catch((error) => {
              console.error('[web-client-bridge] 添加自定义工具失败:', error);
            });
          }
          return;
        case 'enhancePrompt':
          if (typeof message.prompt === 'string' && message.prompt.trim()) {
            void enhancePrompt(message.prompt).catch((error) => {
              console.error('[web-client-bridge] 增强提示词失败:', error);
            });
          }
          return;
        case 'login':
        case 'logout':
          console.info(`[web-client-bridge] Web 端忽略本地鉴权消息: ${message.type}`);
          return;
        case 'uiError':
          console.error('[web-client-bridge] UI 错误上报:', {
            component: message.component,
            detail: message.detail,
            stack: message.stack,
          });
          return;
        case 'selectWorker':
          console.info('[web-client-bridge] Web 端 Worker 选择由前端本地视图状态自行处理。');
          return;
        case 'toggleBuiltInTool':
          console.info('[web-client-bridge] 内置工具由运行时固定管理，已忽略切换请求。');
          return;
        case 'confirmRecovery':
          if (
            message.decision === 'retry'
            || message.decision === 'rollback'
            || message.decision === 'continue'
          ) {
            void confirmRecovery(message.decision).catch((error) => {
              console.error('[web-client-bridge] 恢复确认失败:', error);
            });
          }
          return;
        case 'answerClarification':
          void submitClarificationAnswer(
            message.answers && typeof message.answers === 'object'
              ? message.answers as Record<string, string>
              : null,
            typeof message.additionalInfo === 'string' ? message.additionalInfo : null,
          ).catch((error) => {
            console.error('[web-client-bridge] 澄清回答提交失败:', error);
          });
          return;
        case 'answerWorkerQuestion':
          void submitWorkerQuestionAnswer(
            typeof message.answer === 'string' ? message.answer : null,
          ).catch((error) => {
            console.error('[web-client-bridge] Worker 问答提交失败:', error);
          });
          return;
        case 'toolAuthorizationResponse':
          if (typeof message.requestId === 'string' && message.requestId.trim()) {
            void respondToolAuthorization(message.requestId, Boolean(message.allowed)).catch((error) => {
              console.error('[web-client-bridge] 工具授权响应失败:', error);
            });
          }
          return;
        case 'interactionResponse':
          if (typeof message.requestId === 'string' && message.requestId.trim()) {
            void respondInteraction(message.requestId, message.response).catch((error) => {
              console.error('[web-client-bridge] 交互响应失败:', error);
            });
          }
          return;
        default:
          console.log('[web-client-bridge] 未接入的消息已忽略:', message.type);
      }
    },
    onMessage(listener: (message: ClientBridgeMessage) => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getState<T>(): T | undefined {
      const stored = localStorage.getItem('webview-state');
      return stored ? JSON.parse(stored) as T : undefined;
    },
    setState<T>(state: T): void {
      localStorage.setItem('webview-state', JSON.stringify(state));
    },
    getInitialSessionId(): string {
      return resolveWorkspaceQuery().sessionId;
    },
    getInitialLocale(): SupportedLocale {
      if (typeof window !== 'undefined') {
        const storedLocale = localStorage.getItem('magi-locale');
        if (storedLocale === 'zh-CN' || storedLocale === 'en-US') {
          return storedLocale;
        }
        const locale = (window as unknown as { __INITIAL_LOCALE__?: string }).__INITIAL_LOCALE__;
        if (locale === 'zh-CN' || locale === 'en-US') {
          return locale;
        }
      }
      return 'zh-CN';
    },
    notifyReady(): void {
      void restoreBridgeState('notify_ready').catch((error) => {
        console.error('[web-client-bridge] Web 入口初始化失败:', error);
        scheduleRecovery('notify_ready', error);
      });
    },
  };
}
