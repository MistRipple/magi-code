<script lang="ts">
  import type { ToolCallStatus, ToolOrigin } from '../types/message';
  import ToolOriginBadge from './ToolOriginBadge.svelte';
  import Icon from './Icon.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import FileSpan from './FileSpan.svelte';
  import DiagramRenderer from './DiagramRenderer.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import ToolApprovalAction from './ToolApprovalAction.svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { vscode } from '../lib/vscode-bridge';
  import { extractLeadingJson } from '../lib/terminal-utils';
  import type { IconName } from '../lib/icons';
  import { resolveToolIcon } from '../lib/tool-icons';
  import type { StandardizedToolResult, ToolPolicyPayload } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { getCurrentSessionId, messagesState } from '../stores/messages.svelte';
  import { reportIncident } from '../lib/notifications';
  import { parseToolDiagramPayload } from '../lib/diagram-payload';
  import { openAgentTab } from '../stores/right-pane.svelte';
  import { getAgentRunState } from '../stores/agent-run-store.svelte';
  import { agentChildVisualStatus, resolveAgentChildStatus } from '../lib/agent-child-status';
  import { getAgentBrandInfo, getAgentRunColor } from '../lib/agent-colors';
  import { agentModelDisplay } from '../lib/agent-model-display';
  import { parseToolIdentity } from '../lib/tool-identity';
  import { parseUserQuestionResult } from '../shared/user-question';
  import { isUserQuestionPending } from '../stores/user-question-store.svelte';
  import { resolveToolDisplayName } from '../lib/tool-display-name';
  import {
    formatViewImageToolOutput,
    parseViewImagePreview,
  } from '../lib/view-image-preview';
  import { browserToolSummary, formatBrowserReadToolOutput, parseBrowserScreenshotPreview } from '../lib/browser-tool-display';
  import {
    formatImageGenerationToolOutput,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import { agentUrl, buildFilePreviewQuery } from '../web/agent-api';
  import { desktopContextMenu } from '../lib/desktop-context-menu-contract';
  import {
    ACCESS_MODE_APPROVAL_ERROR_CODES,
    isStructuredToolErrorPayload,
    parseToolApprovalPayload,
    parseToolPayloadRecord,
    publicToolPayloadMessage,
    toolPayloadRestrictionKind,
    toolPayloadErrorCode,
    toolPayloadStatus,
  } from '../lib/tool-error-payload';
  import {
    firstToolDisplayText,
    hasInternalRedactedDisplayValue,
    normalizeToolDisplayText,
    resolveToolCardTarget,
    sanitizeToolDisplayPayload,
  } from '../lib/tool-call-display';
  import type { ConversationPresentationRole } from '../lib/conversation-presentation';
  import { onDestroy, untrack, type Snippet } from 'svelte';
  import Disclosure from './Disclosure.svelte';

  interface ErrorDiagnosis {
    category: 'model_input' | 'context_stale' | 'permission' | 'policy' | 'model_output' | 'workspace_write' | 'runtime';
    categoryLabel: string;
    ownerLabel: string;
    message: string;
    hint: string;
  }

  // Props
  interface Props {
    name: string;
    id?: string;
    origin?: ToolOrigin;
    input?: unknown;
    output?: unknown;
    error?: string;
    standardized?: StandardizedToolResult;
    status?: ToolCallStatus;
    duration?: number;
    filepath?: string;
    filePreviewScope?: FilePreviewScope;
    presentationRole?: ConversationPresentationRole;
    compactAgent?: boolean;
    agentChildStatus?: unknown;
    agentChildStatusUpdatedAt?: unknown;
    outputFooter?: Snippet;
  }

  let {
    name,
    id,
    origin,
    input,
    output,
    error,
    standardized,
    status = 'success',
    duration,
    filepath,
    filePreviewScope = undefined,
    presentationRole = 'process',
    compactAgent = false,
    agentChildStatus,
    agentChildStatusUpdatedAt,
    outputFooter,
  }: Props = $props();

  // 过程工具默认折叠；摘要投影提升出的用户产物和待处理事项默认直接可见。
  // 首次渲染后不再跟随状态更新，避免覆盖用户主动展开或收起的选择。
  let collapsed = $state(untrack(() => presentationRole !== 'artifact' && presentationRole !== 'attention'));
  let copySuccess = $state(false);
  let copyRevision = $state(0);
  let copyResetTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(copyResetTimer));
  let lastLoggedErrorSignature = $state('');
  let lastReportedAgentSpawnFailureSignature = $state('');
  let lastAutoExpandedApprovalId = $state('');

  // 格式化内容
  function formatContent(content: unknown): string {
    if (content === null || content === undefined) return '';

    if (typeof content === 'string') {
      const trimmed = content.trim();
      if (!trimmed) return '';

      // 工具输出若以 JSON 开头，优先美化 JSON，同时保留尾随文本
      const leadingJson = extractLeadingJson(trimmed);
      if (leadingJson) {
        try {
          const formattedJson = JSON.stringify(JSON.parse(leadingJson.jsonText), null, 2);
          return leadingJson.tailText ? `${formattedJson}\n\n${leadingJson.tailText}` : formattedJson;
        } catch {
          // 非法 JSON 字符串保持原样
        }
      }

      return trimmed;
    }

    try {
      return JSON.stringify(content, null, 2);
    } catch {
      return String(content).trim();
    }
  }

  function formatToolOutput(toolName: string, content: unknown): string {
    const generatedImageOutput = formatImageGenerationToolOutput(toolName, content);
    if (generatedImageOutput !== null) {
      return generatedImageOutput;
    }
    const viewImageOutput = formatViewImageToolOutput(toolName, content);
    if (viewImageOutput !== null) {
      return viewImageOutput;
    }
    const browserReadOutput = formatBrowserReadToolOutput(toolName, content);
    if (browserReadOutput !== null) {
      return browserReadOutput;
    }
    return formatContent(content);
  }

  function formatToolInput(content: unknown): string {
    if (content === null || content === undefined) return '';
    const sanitized = sanitizeToolDisplayPayload(content);
    if (sanitized === undefined) {
      return hasInternalRedactedDisplayValue(content) ? i18n.t('toolCall.redactedInput') : '';
    }
    return formatContent(sanitized);
  }

  function hasDisplayContent(content: unknown): boolean {
    if (content === null || content === undefined) return false;
    if (typeof content === 'string') return content.trim().length > 0;
    if (Array.isArray(content)) return content.length > 0;
    return true;
  }

  // 获取工具图标（基于当前项目实际工具名）
  function getToolIcon(toolName: string): IconName {
    if (!toolName || typeof toolName !== 'string') {
      vscode.postMessage({
        type: 'uiError',
        component: 'ToolCall',
        detail: { toolName, id, status },
        stack: new Error('ToolCall: invalid toolName').stack,
      });
      throw new Error('ToolCall: invalid toolName');
    }

    return resolveToolIcon(toolName);
  }

  type VisualToolStatus = ToolCallStatus | 'degraded';

  function visualStatusInfo(value: VisualToolStatus): { class: string } {
    const map: Record<string, { class: string }> = {
      pending: { class: 'pending' },
      running: { class: 'running' },
      success: { class: 'success' },
      error: { class: 'error' },
      degraded: { class: 'degraded' },
      cancelled: { class: 'cancelled' },
      unconfirmed: { class: 'unconfirmed' },
    };
    return map[value] || { class: 'success' };
  }

  // 状态信息
  const statusInfo = $derived.by(() => {
    return visualStatusInfo(visualStatus);
  });

  // 文件变更工具：diff 面板由 FileChangeCard 展示，ToolCall 仅渲染紧凑 header
  const isFileMutationTool = $derived(
    name === 'file_patch' || name === 'file_write' || name === 'apply_patch' || name === 'file_remove'
  );
  const isCompactMutation = $derived(isFileMutationTool && (status === 'running' || status === 'pending'));

  // agent_spawn 工具调用：父代理创建代理并投递初始任务消息。每一次调用即父代理
  // ToolCall 流中的一张内嵌卡片，多个并行 agent_spawn 即多张并列卡片，点击卡片
  // 打开右侧 RightPane 代理 transcript（按 metadata.taskId 过滤）。
  //
  // input 形态：{ role, display_name, goal }
  // output 形态：{ tool, status: 'started', child_task_id, role, title, assignment, ... }
  // 代理终态结果由主线后续 agent_wait 收集。
  const isAgentSpawn = $derived(name === 'agent_spawn');

  interface AgentSpawnDisplay {
    /** 代理展示名（首选 output.title，回退到 input.display_name） */
    title: string;
    /** 代理角色（如 executor / explorer / tester），用于图标与角色名称 */
    role: string;
    /** 代理 TaskId，作为 RightPane tab 去重 key；未就绪时为 undefined */
    childTaskId: string | undefined;
    /** 代理创建或运行终态，未终态为 undefined */
    outcome: 'started' | 'queued' | 'completed' | 'succeeded' | 'degraded' | 'rejected' | 'failed' | 'killed' | undefined;
    /** 失败原因摘要，若有 */
    error: string | undefined;
    errorCode: string | undefined;
    failureStage: string | undefined;
    instruction: string | undefined;
    diagnosticRef: string | undefined;
  }

  const agentSpawnDisplay = $derived.by((): AgentSpawnDisplay | null => {
    if (!isAgentSpawn) return null;
    const inputObj = (input && typeof input === 'object' && !Array.isArray(input))
      ? input as Record<string, unknown>
      : {};
    const inputTitle = typeof inputObj.display_name === 'string' ? inputObj.display_name.trim() : '';
    const inputRole = typeof inputObj.role === 'string' ? inputObj.role.trim() : '';

    // output 可能是 JSON 字符串（tool_batch 返回 .to_string()）或已被解析为 object。
    const parsedOutput = parseToolPayloadRecord(output);

    const outputTitle = typeof parsedOutput?.title === 'string' ? (parsedOutput.title as string).trim() : '';
    const outputRole = typeof parsedOutput?.role === 'string' ? (parsedOutput.role as string).trim() : '';
    const outputChildId = typeof parsedOutput?.child_task_id === 'string'
      ? (parsedOutput.child_task_id as string).trim()
      : '';
    const outputStatus = typeof parsedOutput?.status === 'string' ? parsedOutput.status : '';
    const outputError = typeof parsedOutput?.error === 'string' ? (parsedOutput.error as string) : '';
    const outputInstruction = typeof parsedOutput?.instruction === 'string'
      ? (parsedOutput.instruction as string).trim()
      : '';
    const outputFailureStage = typeof parsedOutput?.failure_stage === 'string'
      ? (parsedOutput.failure_stage as string).trim()
      : '';
    const outputDiagnosticRef = typeof parsedOutput?.diagnostic_ref === 'string'
      ? (parsedOutput.diagnostic_ref as string).trim()
      : '';

    const outcome: AgentSpawnDisplay['outcome'] =
      outputStatus === 'started' || outputStatus === 'queued' || outputStatus === 'completed' || outputStatus === 'succeeded' || outputStatus === 'degraded' || outputStatus === 'rejected' || outputStatus === 'failed' || outputStatus === 'killed'
        ? outputStatus
        : undefined;

    return {
      title: outputTitle || inputTitle || i18n.t('toolCall.agentSpawn.defaultTitle'),
      role: outputRole || inputRole || '',
      childTaskId: outputChildId || undefined,
      outcome,
      error: outputError || undefined,
      errorCode: toolPayloadErrorCode(parsedOutput) || undefined,
      failureStage: outputFailureStage || undefined,
      instruction: outputInstruction || undefined,
      diagnosticRef: outputDiagnosticRef || undefined,
    };
  });

  const agentSpawnRoleVisual = $derived.by(() => {
    const role = agentSpawnDisplay?.role;
    if (!role) return null;
    return getAgentBrandInfo(role, (key) => i18n.t(key));
  });

  const agentSpawnColor = $derived(getAgentRunColor(agentSpawnDisplay?.childTaskId || id || agentSpawnDisplay?.title || name));
  const agentSpawnProjection = $derived(isAgentSpawn ? getAgentRunState(
    filePreviewScope?.sessionId || getCurrentSessionId(),
    filePreviewScope?.workspaceId ?? messagesState.currentWorkspaceId,
  ).projection : null);
  const agentSpawnModel = $derived.by(() => {
    if (!isAgentSpawn) return agentModelDisplay(null, (key) => i18n.t(key));
    const receipt = parseToolPayloadRecord(parseToolPayloadRecord(output)?.model_selection);
    const taskId = agentSpawnDisplay?.childTaskId;
    const task = agentSpawnProjection?.root_task.task_id === taskId
      ? agentSpawnProjection?.root_task
      : agentSpawnProjection?.tasks.find((task) => task.task_id === taskId);
    return agentModelDisplay(receipt ?? task?.executor_binding?.model_selection, (key) => i18n.t(key));
  });

  const agentSpawnVisualStatus = $derived.by((): VisualToolStatus => {
    const childStatus = agentChildVisualStatus(resolveAgentChildStatus(
      agentSpawnProjection, agentSpawnDisplay?.childTaskId || '', agentChildStatus, agentChildStatusUpdatedAt,
    ));
    if (childStatus) return childStatus;
    if (agentSpawnDisplay?.outcome === 'queued') return 'pending';
    if (agentSpawnDisplay?.outcome === 'killed') return 'cancelled';
    if (agentSpawnDisplay?.outcome === 'rejected' || agentSpawnDisplay?.outcome === 'failed') return 'error';
    if (agentSpawnDisplay?.outcome === 'degraded') return 'degraded';
    if (agentSpawnDisplay?.outcome === 'started') return 'running';
    if (agentSpawnDisplay?.outcome === 'succeeded' || agentSpawnDisplay?.outcome === 'completed') return 'success';
    return status;
  });

  const agentSpawnStatusInfo = $derived(visualStatusInfo(agentSpawnVisualStatus));
  const agentSpawnStatusIcon = $derived<IconName>(
    agentSpawnVisualStatus === 'running' ? 'loader'
    : agentSpawnVisualStatus === 'pending' ? 'clock'
    : agentSpawnVisualStatus === 'success' ? 'check-circle'
    : agentSpawnVisualStatus === 'error' ? 'x-circle'
    : agentSpawnVisualStatus === 'cancelled' ? 'stop' : 'alert-triangle',
  );
  const agentSpawnStatusLabel = $derived(agentSpawnVisualStatus === 'error'
    ? i18n.t('activeAgentCenter.status.failed')
    : agentSpawnVisualStatus === 'degraded' ? i18n.t('activeAgentCenter.status.degraded')
    : toolStatusLabel(agentSpawnVisualStatus));
  const agentSpawnFailed = $derived(agentSpawnVisualStatus === 'error');
  const agentSpawnDegraded = $derived(agentSpawnVisualStatus === 'degraded');
  const agentSpawnCreationFailed = $derived(agentSpawnFailed && !agentSpawnDisplay?.childTaskId);

  function agentSpawnFailureStageLabel(stage: string | undefined): string {
    if (stage === 'input_validation') return i18n.t('toolCall.agentSpawn.stageInputValidation');
    if (stage === 'registration') return i18n.t('toolCall.agentSpawn.stageRegistration');
    if (stage === 'plan_binding') return i18n.t('toolCall.agentSpawn.stagePlanBinding');
    if (stage === 'dispatch') return i18n.t('toolCall.agentSpawn.stageDispatch');
    if (stage === 'model_invocation') return i18n.t('toolCall.agentSpawn.stageModelInvocation');
    if (stage === 'git_preflight') return i18n.t('toolCall.agentSpawn.stageGitPreflight');
    if (stage === 'execution_admission') return i18n.t('toolCall.agentSpawn.stageExecutionAdmission');
    if (stage === 'task_execution') return i18n.t('toolCall.agentSpawn.stageTaskExecution');
    if (stage === 'cancellation') return i18n.t('toolCall.agentSpawn.stageCancellation');
    if (stage === 'runtime_lookup') return i18n.t('toolCall.agentSpawn.stageRuntimeLookup');
    if (stage === 'scope_validation') return i18n.t('toolCall.agentSpawn.stageScopeValidation');
    return stage || '';
  }

  $effect(() => {
    const display = agentSpawnDisplay;
    if (!isAgentSpawn || !agentSpawnCreationFailed || !display?.error || !display.errorCode) {
      return;
    }
    const signature = `${id || display.diagnosticRef || display.title}:${display.errorCode}:${display.error}`;
    if (signature === lastReportedAgentSpawnFailureSignature) {
      return;
    }
    lastReportedAgentSpawnFailureSignature = signature;
    reportIncident(display.error, {
      scope: 'session',
      source: 'agent-spawn',
      title: i18n.t('toolCall.agentSpawn.creationFailed'),
      errorCode: display.errorCode,
      failureStage: display.failureStage,
    });
  });

  function openAgentSpawnTab(display: AgentSpawnDisplay | null): void {
    if (!display || !display.childTaskId) return;
    openAgentTab(filePreviewScope?.sessionId || getCurrentSessionId(), display.childTaskId, {
      label: display.title,
      workspaceId: filePreviewScope?.workspaceId ?? messagesState.currentWorkspaceId,
      workspacePath: filePreviewScope?.workspacePath ?? messagesState.currentWorkspacePath,
    });
  }

  // 目录/文件只读工具：只需紧凑 header
  const isCompactReadOnlyTool = $derived(name === 'file_read');
  // 仅 view 类工具支持点击整行 header 打开文件
  const isHeaderOpenableTool = $derived(name === 'file_read' || name === 'view');

  // 折叠态只计算卡片存在性和标题摘要；完整输入输出、图片和图表在展开后解析。
  const sanitizedInput = $derived(sanitizeToolDisplayPayload(input));
  const hasInput = $derived(
    name !== 'ask_user_question'
      && (hasDisplayContent(sanitizedInput) || hasInternalRedactedDisplayValue(input)),
  );
  const toolApproval = $derived.by(() => (
    parseToolApprovalPayload(output)
    || parseToolApprovalPayload(error)
    || parseToolApprovalPayload(standardized?.message)
  ));
  // 向用户提问：对话流里只展示“问了什么、用户答了什么”的摘要，不展示原始 JSON 输入输出。
  const userQuestionSummary = $derived(
    name === 'ask_user_question' ? parseUserQuestionResult(output) : null,
  );
  const outputIsStructuredError = $derived(!toolApproval && !userQuestionSummary && isStructuredToolErrorPayload(output));
  const hasOutput = $derived(!toolApproval && !userQuestionSummary && !outputIsStructuredError && hasDisplayContent(output));
  const structuredErrorText = $derived.by(() => {
    if (!outputIsStructuredError) {
      return '';
    }
    const publicMessage = publicToolPayloadMessage(output);
    const errorCode = toolPayloadErrorCode(output);
    const payloadStatus = toolPayloadStatus(output);
    const restrictionKind = toolPayloadRestrictionKind(output);
    const payload = parseToolPayloadRecord(output);
    return JSON.stringify({
      error_code: errorCode,
      status: payloadStatus || 'failed',
      error: publicMessage || undefined,
      restriction_kind: restrictionKind || undefined,
      required_access_profile: payload?.required_access_profile,
    });
  });
  const errorForDiagnosis = $derived((error && error.trim()) || structuredErrorText);
  const hasError = $derived(!!errorForDiagnosis);
  // 写操作已经发出但无法确认是否生效（例如浏览器命令执行中途连接断开）。这是 item 的正式状态，
  // 不是普通失败：副作用可能已经发生，盲目重试可能重复提交，所以单独呈现并提示先确认状态。
  const outcomeUnconfirmed = $derived(status === 'unconfirmed');
  const visualStatus = $derived<VisualToolStatus>(status);

  const hasContent = $derived(
    hasInput || hasOutput || hasError || Boolean(toolApproval) || Boolean(userQuestionSummary) || Boolean(outputFooter),
  );
  // 待授权是用户必须处理的交互，即使它来自文件变更这类紧凑工具，也不能
  // 被紧凑卡片规则吞掉；授权状态会自动展开到卡片内容区。
  const canExpand = $derived(
    hasContent && (Boolean(toolApproval) || hasError || outcomeUnconfirmed || (!isCompactReadOnlyTool && !isCompactMutation)),
  );
  const shouldRenderCard = $derived(hasContent || isCompactReadOnlyTool || isCompactMutation);
  const instanceId = $props.id();
  const detailId = $derived(
    id ? `tool-call-detail-${id.replace(/[^a-zA-Z0-9_-]/gu, '-')}` : `tool-call-detail-${instanceId}`,
  );
  const detailVisible = $derived(canExpand && !collapsed);
  const inputText = $derived(detailVisible ? formatToolInput(input) : '');
  const generatedImagePreview = $derived(
    detailVisible && !outputIsStructuredError ? parseImageGenerationPreview(name, output) : null,
  );
  // view_image 的模型结果包含可直接渲染的 base64 数据。它必须在卡片折叠时
  // 也可用，因为用户点击工具卡标题就是直接打开图片预览的入口。
  const viewImagePreview = $derived(
    !outputIsStructuredError ? parseViewImagePreview(name, output) : null,
  );
  const imagePreview = $derived.by(() => {
    if (!detailVisible || outputIsStructuredError) return null;
    if (viewImagePreview) return { ...viewImagePreview, revisedPrompt: '', open: handleOpenFile };
    if (generatedImagePreview) {
      return {
        ...generatedImagePreview,
        src: agentUrl('/api/files/raw', buildFilePreviewQuery(generatedImagePreview.path, {
          sourceFolderId: filePreviewScope?.sourceFolderId,
          workspaceId: filePreviewScope?.workspaceId,
          workspacePath: filePreviewScope?.workspacePath,
          sessionId: filePreviewScope?.sessionId,
        })),
        open: handleOpenFile,
      };
    }
    // 浏览器截图保存在会话的浏览器 artifact 目录（不属于工作区），按会话读取；
    // 点击时以图片地址直接在右栏打开，个人会话同样可用。
    const screenshot = parseBrowserScreenshotPreview(name, output);
    const sessionId = filePreviewScope?.sessionId || getCurrentSessionId();
    if (!screenshot || !sessionId) return null;
    const src = agentUrl(
      `/api/browser/artifacts/${encodeURIComponent(sessionId)}/${encodeURIComponent(screenshot.fileName)}`,
    );
    return {
      path: screenshot.path,
      mime: screenshot.mime,
      bytes: screenshot.bytes,
      revisedPrompt: '',
      src,
      open: () => openImageInPreview(screenshot.path, src, screenshot.mime, screenshot.bytes),
    };
  });

  function openImageInPreview(filepath: string, src: string, mime: string, size: number | undefined) {
    if (typeof window === 'undefined') return;
    window.dispatchEvent(new CustomEvent('magi:previewFile', {
      detail: { filepath, ...filePreviewScope, imageDataUrl: src, mime, size, contentKind: 'binary' },
      cancelable: true,
    }));
  }
  const outputText = $derived(
    detailVisible && !outputIsStructuredError ? formatToolOutput(name, output) : '',
  );
  const diagramPayload = $derived(
    detailVisible ? parseToolDiagramPayload(name, output) : null,
  );
  const isDiagramTool = $derived(!!diagramPayload);

  $effect(() => {
    const approvalId = toolApproval?.approvalId || '';
    if (!approvalId || approvalId === lastAutoExpandedApprovalId) return;
    lastAutoExpandedApprovalId = approvalId;
    collapsed = false;
  });

  const skillApplyPolicy = $derived.by(() => {
    if (!detailVisible || name !== 'skill_apply') return null;
    const data = standardized?.data;
    if (!data || typeof data !== 'object' || Array.isArray(data)) return null;
    const toolPolicy = (data as Record<string, unknown>).toolPolicy;
    if (!toolPolicy || typeof toolPolicy !== 'object' || Array.isArray(toolPolicy)) return null;
    return toolPolicy as ToolPolicyPayload;
  });

  // 获取工具显示名
  function getToolDisplayName(toolName: string): string {
    return resolveToolDisplayName(toolName, i18n);
  }

  // 从工具参数中提取语义摘要
  function getToolSummary(toolName: string, toolInput: unknown, toolOutput: unknown): string {
    if (!toolInput || typeof toolInput !== 'object') return '';
    const args = toolInput as Record<string, unknown>;
    const parsedTool = parseToolIdentity(toolName);
    const browserSummary = browserToolSummary(toolName, args, toolOutput);
    if (browserSummary !== null) return browserSummary;
    if (parsedTool.source === 'skill') {
      return typeof args.payload === 'string'
        ? args.payload
        : typeof args.input === 'string'
          ? args.input
          : '';
    }
    switch (toolName) {
      case 'shell_exec':
        return normalizeToolDisplayText(args.command);
      case 'file_read':
      case 'view_image':
      case 'file_write':
      case 'file_patch':
      case 'apply_patch': {
        return firstToolDisplayText(args.path, args.file_path, args.image_path);
      }
      case 'image_generate':
        return normalizeToolDisplayText(args.prompt);
      case 'search_text':
        return firstToolDisplayText(args.pattern, args.query);
      case 'search_semantic':
      case 'knowledge_query':
        return normalizeToolDisplayText(args.query);
      case 'skill_apply':
        return normalizeToolDisplayText(args.skill_name);
      case 'file_remove': {
        const paths = args.paths;
        if (Array.isArray(paths) && paths.length > 0) {
          const displayPaths = paths
            .map((path) => normalizeToolDisplayText(path))
            .filter(Boolean);
          if (displayPaths.length === 0) return '';
          return displayPaths.length === 1
            ? displayPaths[0]
            : i18n.t('toolCall.fileRemoveSummary', { firstFile: displayPaths[0], count: displayPaths.length });
        }
        return normalizeToolDisplayText(args.path);
      }
      case 'web_fetch':
        return normalizeToolDisplayText(args.url);
      case 'web_search':
        return normalizeToolDisplayText(args.query);
      case 'diagram_render':
        return normalizeToolDisplayText(args.title);
      case 'process_inspect':
        return typeof args.pid === 'number' ? String(args.pid) : normalizeToolDisplayText(args.pid);
      case 'diff_preview':
        return '';
      case 'file_mkdir':
        return normalizeToolDisplayText(args.path);
      case 'file_copy':
      case 'file_move': {
        const source = normalizeToolDisplayText(args.source);
        const destination = normalizeToolDisplayText(args.destination);
        return source && destination ? `${source} → ${destination}` : source || destination;
      }
      case 'code_symbols': {
        const action = normalizeToolDisplayText(args.action);
        const symbolName = normalizeToolDisplayText(args.name);
        const p = normalizeToolDisplayText(args.path);
        return [action, symbolName || p].filter(Boolean).join(' ');
      }
      case 'agent_wait': {
        const taskIds = Array.isArray(args.task_ids) ? args.task_ids : [];
        return taskIds.map((taskId) => normalizeToolDisplayText(String(taskId))).filter(Boolean).join(', ');
      }
      default:
        // MCP 或其他未知工具：尝试提取常见字段
        return firstToolDisplayText(args.command, args.path, args.query, args.url);
    }
  }

  // 判断 file_read 是否为目录查看模式
  const isDirectoryView = $derived.by(() => {
    if (name !== 'file_read') return false;
    if (!input || typeof input !== 'object') return false;
    const args = input as Record<string, unknown>;
    if (args.type === 'directory') return true;
    const p = typeof args.path === 'string' ? args.path.trim() : '';
    return p === '.' || p === '' || p.endsWith('/');
  });

  const toolIcon = $derived(getToolIcon(name));
  const toolDisplayName = $derived(
    name === 'file_read'
      ? (isDirectoryView ? i18n.t('toolCall.displayName.viewDirectory') : i18n.t('toolCall.displayName.viewFile'))
      : getToolDisplayName(name)
  );
  const toolCardTarget = $derived(resolveToolCardTarget({
    toolName: name,
    input,
    output,
    explicitFilepath: filepath,
    directoryView: isDirectoryView,
  }));
  const toolTargetSummary = $derived.by(() => {
    if (toolCardTarget.primaryPath || toolCardTarget.paths.length === 0) {
      return '';
    }
    const [firstPath, ...restPaths] = toolCardTarget.paths;
    return restPaths.length === 0
      ? firstPath
      : i18n.t('toolCall.multipleFilesSummary', {
        firstFile: firstPath,
        count: toolCardTarget.paths.length,
      });
  });
  const toolSummary = $derived(
    toolTargetSummary || getToolSummary(name, input, output)
  );

  // 判断输出内容是否包含 markdown 格式（标题、表格、列表等）
  const isToolOutputStreaming = $derived(status === 'running' || status === 'pending');
  const isMarkdownOutput = $derived.by(() => {
    if (!outputText || outputText.length < 20) return false;
    // JSON 输出不走 Markdown 渲染，避免负数/列表结构被误判
    if (outputText.startsWith('{') || outputText.startsWith('[')) return false;
    // 检测常见 markdown 标记：标题、表格、列表、引用、加粗、分隔线、代码块
    return /^#{1,4}\s|^\|.+\|$|^\s*[-*]\s|^\s*\d+\.\s|^>\s|^---$|```|\*\*[^*]+\*\*/m.test(outputText);
  });

  function toolErrorCodeForDiagnosis(errorText?: string, toolResult?: StandardizedToolResult): string {
    return (
      toolResult?.errorCode
      || toolPayloadErrorCode(errorText)
      || toolPayloadErrorCode(toolResult?.message)
      || ''
    ).toLowerCase();
  }

  function detectErrorDiagnosis(errorText?: string, toolResult?: StandardizedToolResult): ErrorDiagnosis | null {
    const rawMessage = `${toolResult?.message || ''}\n${errorText || ''}`.trim();
    if (!rawMessage) return null;

    const errorCode = toolErrorCodeForDiagnosis(errorText, toolResult);
    // 只取消息前 300 字符做关键词匹配，避免工具输出正文中的常见词（如 authorization、timeout）
    // 导致误分类。后端结构化错误前缀（如 "Tool blocked:", "Command rejected:"）都在开头。
    const messageHead = rawMessage.slice(0, 300).toLowerCase();
    /** 匹配 errorCode 或消息头部 */
    const matches = (...patterns: string[]): boolean =>
      patterns.some((pattern) => errorCode.includes(pattern) || messageHead.includes(pattern));
    /** 仅匹配 errorCode（不匹配消息内容，用于宽泛关键词如 authorization） */
    const codeMatches = (...patterns: string[]): boolean =>
      patterns.some((pattern) => errorCode.includes(pattern));

    if (matches('file_context_stale', '[file_context_stale]')) {
      return {
        category: 'context_stale',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.contextStale.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.contextStale.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.contextStale.message'),
        hint: i18n.t('toolCall.errorDiagnosis.contextStale.hint'),
      };
    }

    if (matches(
      'tool_policy_not_allowed',
      'tool_policy_external_not_allowed',
      'tool_policy_read_only',
      'tool_policy_shell_write_disallowed',
      'tool_policy_missing_path',
      'tool_policy_path_not_allowed',
      'tool_policy_path_forbidden',
      'tool_policy_rejected',
      'tool_policy_failed',
      'skill_tool_policy_rejected',
      'skill_tool_policy_failed',
      'external_tool_policy_rejected',
      'skill_tool_scope_mismatch',
      'tool_safety_rejected',
      'tool_safety_failed',
    )) {
      const restrictionKind = toolPayloadRestrictionKind(errorText) || toolPayloadRestrictionKind(toolResult?.message);
      const detailKey = restrictionKind === 'path_scope'
        ? 'pathScope'
        : restrictionKind === 'read_only'
          ? 'readOnly'
          : restrictionKind === 'tool_scope'
            ? 'toolScope'
            : 'default';
      return {
        category: 'policy',
        categoryLabel: i18n.t(`toolCall.errorDiagnosis.policy.${detailKey}.categoryLabel`),
        ownerLabel: i18n.t(`toolCall.errorDiagnosis.policy.${detailKey}.ownerLabel`),
        message: i18n.t(`toolCall.errorDiagnosis.policy.${detailKey}.message`),
        hint: i18n.t(`toolCall.errorDiagnosis.policy.${detailKey}.hint`),
      };
    }

    if (matches(
      'file_write_apply_failed',
      'file_write_save_failed',
      'file_remove_apply_failed',
      'write_conflict',
      'file_write_failed',
      'file_mkdir_failed',
      'file_copy_failed',
      'file_move_failed',
      'file_patch_failed',
      'file_remove_rejected',
      'file_remove_failed',
      'apply_patch_failed',
      'view_image_failed',
    )) {
      return {
        category: 'workspace_write',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.workspaceWrite.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.workspaceWrite.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.workspaceWrite.message'),
        hint: i18n.t('toolCall.errorDiagnosis.workspaceWrite.hint'),
      };
    }

    if (matches(
      'file_patch_no_match',
      'file_patch_ambiguous_match',
      'file_patch_not_applicable',
      'file_patch_invalid_input',
      'file_remove_invalid_args',
      'file_path_required',
      'file_path_outside_workspace',
      'tool_rejected',
      'tool_arguments_invalid',
      'command rejected',
      'argument parse failed',
      '缺少 shell 命令',
      'missing shell command',
      'path is required',
      'old_str_1 is required',
      'old_str and new_str are identical',
      'old_str appears multiple times',
      'old_str not found',
      'no match found close',
    )) {
      return {
        category: 'model_input',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.modelInput.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.modelInput.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.modelInput.message'),
        hint: i18n.t('toolCall.errorDiagnosis.modelInput.hint'),
      };
    }

    // 用户权限拦截（Ask 模式弹窗拒绝 / 权限开关关闭）
    // 仅匹配 errorCode，不对 message 做子串匹配 — 'authorization' 在代码中过于常见，易误判
    if (
      codeMatches(
        'tool_blocked',
        ...ACCESS_MODE_APPROVAL_ERROR_CODES,
      )
      || messageHead.includes('user denied tool authorization')
    ) {
      return {
        category: 'permission',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.permission.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.permission.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.permission.message'),
        hint: i18n.t('toolCall.errorDiagnosis.permission.hint'),
      };
    }

    return {
      category: 'runtime',
      categoryLabel: i18n.t('toolCall.errorDiagnosis.runtime.categoryLabel'),
      ownerLabel: i18n.t('toolCall.errorDiagnosis.runtime.ownerLabel'),
      message: i18n.t('toolCall.errorDiagnosis.runtime.message'),
      hint: i18n.t('toolCall.errorDiagnosis.runtime.hint'),
    };
  }

  const errorDiagnosis = $derived.by(() => detectErrorDiagnosis(errorForDiagnosis, standardized));
  const publicPayloadErrorMessage = $derived(
    publicToolPayloadMessage(output)
    || publicToolPayloadMessage(error)
    || publicToolPayloadMessage(standardized?.message)
  );
  const publicErrorMessage = $derived(
    publicPayloadErrorMessage
    || (errorDiagnosis?.category === 'model_input' ? errorForDiagnosis : '')
    || errorDiagnosis?.message
    || i18n.t('toolCall.errorDiagnosis.runtime.message')
  );
  const headerSummary = $derived.by(() => {
    if (!hasError) {
      return toolSummary;
    }
    return [toolSummary, publicErrorMessage].filter(Boolean).join(', ');
  });

  function toolStatusLabel(value: VisualToolStatus): string {
    if (value === 'unconfirmed') return i18n.t('toolCall.status.unconfirmed');
    if (value === 'cancelled') return i18n.t('terminalSession.status.cancelled');
    if (value === 'degraded') return i18n.t('toolCall.agentSpawn.degraded');
    return i18n.t(`terminalSession.status.${value}`);
  }

  $effect(() => {
    if (!hasError) {
      return;
    }
    const signature = `${id || name}:${status}:${errorForDiagnosis}`;
    if (signature === lastLoggedErrorSignature) {
      return;
    }
    lastLoggedErrorSignature = signature;
    console.warn('Tool call failed', {
      toolName: name,
      toolCallId: id,
      category: errorDiagnosis?.category || 'runtime',
      error: errorForDiagnosis,
    });
  });

  function toggle() {
    if (!canExpand) {
      return;
    }
    collapsed = !collapsed;
  }

  async function copyOutput() {
    const content = formatToolOutput(name, output);
    if (!content) return;
    try {
      await navigator.clipboard.writeText(content);
      copySuccess = true;
      copyRevision += 1;
      clearTimeout(copyResetTimer);
      copyResetTimer = setTimeout(() => { copySuccess = false; }, 2500);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }

  // 从统一目标解析结果中提取可点击文件；多文件/目录模式只展示摘要，避免误打开。
  const toolFilepath = $derived(toolCardTarget.primaryPath);

  // 处理文件点击
  function handleOpenFile() {
    if (!toolFilepath) {
      return;
    }
    const directImagePreview = viewImagePreview;
    if (typeof window !== 'undefined') {
      const previewEvent = new CustomEvent('magi:previewFile', {
        detail: {
          filepath: toolFilepath,
          ...filePreviewScope,
          ...(directImagePreview?.path === toolFilepath
            ? {
                imageDataUrl: directImagePreview.src,
                mime: directImagePreview.mime,
                size: directImagePreview.bytes,
                contentKind: 'binary',
              }
            : {}),
        },
        cancelable: true,
      });
      window.dispatchEvent(previewEvent);
      if (previewEvent.defaultPrevented) {
        return;
      }
    }
    vscode.postMessage({
      type: 'openFile',
      filepath: toolFilepath,
      sessionId: filePreviewScope?.sessionId || getCurrentSessionId() || undefined,
      sourceFolderId: filePreviewScope?.sourceFolderId,
      workspaceId: filePreviewScope?.workspaceId,
      workspacePath: filePreviewScope?.workspacePath,
    });
  }
</script>

{#snippet headerContent()}
  <span class="tool-icon">
    <Icon name={toolIcon} size={14} />
  </span>

  <span class="tool-title">
    <span class="tool-name" title={toolDisplayName}>{toolDisplayName}</span>
    {#if status === 'error' && errorDiagnosis}
      <span class="error-tag error-{errorDiagnosis.category}" title={errorDiagnosis.ownerLabel}>
        {errorDiagnosis.categoryLabel}
      </span>
    {/if}
    {#if toolFilepath && !hasError}
      <FileSpan
        filepath={toolFilepath}
        showIcon={false}
        clickable={true}
        onClick={handleOpenFile}
        filePreviewScope={filePreviewScope}
      />
    {:else if headerSummary}
      <span class="tool-summary" title={headerSummary}>{headerSummary}</span>
    {/if}
  </span>

  <ToolCardMeta status={statusInfo.class} label={toolStatusLabel(visualStatus)} expanded={canExpand ? !collapsed : undefined} {duration}><ToolOriginBadge {origin} /></ToolCardMeta>
{/snippet}

{#if isAgentSpawn && agentSpawnDisplay}
  {@const display = agentSpawnDisplay}
  {@const canOpen = !!display.childTaskId}
  <button
    type="button"
    class="message-card agent-spawn-card status-{agentSpawnStatusInfo.class}"
    class:compact-agent={compactAgent}
    style="--agent-accent: {agentSpawnColor.color};"
    aria-label={[display.title, agentSpawnModel.label, agentSpawnStatusLabel, canOpen ? i18n.t('toolCall.agentSpawn.viewDetails') : ''].filter(Boolean).join(', ')}
    class:clickable={canOpen}
    disabled={!canOpen}
    onclick={canOpen ? () => openAgentSpawnTab(display) : undefined}
    data-tool-name="agent_spawn"
    data-tool-call-id={id || undefined}
    data-agent-task-id={display.childTaskId || undefined}
  >
    <span class="agent-spawn-icon">
      <Icon name={agentSpawnRoleVisual?.icon ?? 'bot'} size={14} />
    </span>
    <span class="agent-spawn-body">
      <span class="agent-spawn-title-line">
        <span class="agent-spawn-title" title={display.title}>{display.title}</span>
        {#if agentSpawnRoleVisual && !compactAgent}
          <span class="agent-spawn-role">
            {agentSpawnRoleVisual.label}
          </span>
        {/if}
      </span>
      {#if agentSpawnModel.model}
        <span class="agent-spawn-model" title={agentSpawnModel.label}>
          <span class="agent-spawn-model-name">{agentSpawnModel.model}</span>
          {#if agentSpawnModel.effort}<span class="agent-spawn-effort">· {agentSpawnModel.effort}</span>{/if}
        </span>
      {/if}
      {#if agentSpawnCreationFailed || ((agentSpawnFailed || agentSpawnDegraded) && display.error)}
        <span class="agent-spawn-error">
          {agentSpawnDegraded
            ? i18n.t('toolCall.agentSpawn.degraded')
            : agentSpawnCreationFailed
            ? i18n.t('toolCall.agentSpawn.creationFailed')
            : i18n.t('toolCall.agentSpawn.runtimeFailed')}
          {#if display.error}
            ：{display.error}
          {/if}
        </span>
        {#if agentSpawnCreationFailed && (display.errorCode || display.failureStage)}
          <span class="agent-spawn-diagnostic ui-meta">
            {#if display.errorCode}
              <span>{i18n.t('toolCall.agentSpawn.failureCode')}{display.errorCode}</span>
            {/if}
            {#if display.failureStage}
              <span>{i18n.t('toolCall.agentSpawn.failureStage')}{agentSpawnFailureStageLabel(display.failureStage)}</span>
            {/if}
          </span>
        {/if}
        {#if agentSpawnCreationFailed && display.instruction}
          <span class="agent-spawn-instruction">{i18n.t('toolCall.agentSpawn.failureInstruction')}{display.instruction}</span>
        {/if}
      {/if}
    </span>
    <span class="agent-spawn-state" title={agentSpawnDegraded ? i18n.t('toolCall.agentSpawn.degraded') : agentSpawnStatusLabel}>
      <Icon name={agentSpawnStatusIcon} size={12} class={agentSpawnVisualStatus === 'running' ? 'spinning' : ''} />
      <span>{agentSpawnStatusLabel}</span>
    </span>
    {#if canOpen}<span class="agent-spawn-chevron"><Icon name="chevron-right" size={12} /></span>{/if}
  </button>
{:else if isFileMutationTool && status === 'success'}
  <!-- 文件变更工具完成：由 FileChangeCard 全权展示 -->
{:else}
  {#if shouldRenderCard}
    <div
      class="message-card tool-call"
      class:collapsed={canExpand && collapsed}
      class:has-error={hasError && !outcomeUnconfirmed}
      class:has-warning={outcomeUnconfirmed}
      class:file-mutation={isCompactMutation}
      class:compact-readonly={isCompactReadOnlyTool}
      data-tool-name={name}
      data-tool-call-id={id || undefined}
    >
      {#if canExpand}
        <button
          class="tool-header"
          onclick={toggle}
          aria-expanded={!collapsed}
          aria-controls={detailId}
        >
          {@render headerContent()}
        </button>
      {:else}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class="tool-header"
          class:file-mutation-header={isCompactMutation || isCompactReadOnlyTool}
          class:clickable={isHeaderOpenableTool && !!toolFilepath}
          onclick={isHeaderOpenableTool && toolFilepath ? handleOpenFile : undefined}
          onkeydown={(e) => {
            if (isHeaderOpenableTool && toolFilepath && (e.key === 'Enter' || e.key === ' ')) {
              e.preventDefault();
              handleOpenFile();
            }
          }}
          role={isHeaderOpenableTool && toolFilepath ? "button" : undefined}
          tabindex={isHeaderOpenableTool && toolFilepath ? 0 : undefined}
        >
          {@render headerContent()}
        </div>
      {/if}

      <Disclosure expanded={canExpand && !collapsed}>
        <div class="tool-content" class:diagram-content={isDiagramTool} id={detailId}>
          {#if hasInput && !isDiagramTool}
            <div class="tool-section">
              <div class="section-header">
                <span class="section-label">{i18n.t('toolCall.section.input')}</span>
              </div>
              <pre class="section-content">{inputText}</pre>
            </div>
          {/if}

          {#if userQuestionSummary}
            <div class="tool-section">
              {#if userQuestionSummary.status === 'awaiting'}
                <!-- 已答复或取消的问题不再显示等待；重启后可恢复的问题继续由 daemon 待答投影决定。 -->
                <div class="uq-summary-note">
                  {userQuestionSummary.questionId
                    && !isUserQuestionPending(filePreviewScope?.sessionId || getCurrentSessionId() || '', userQuestionSummary.questionId)
                    ? i18n.t('toolCall.userQuestion.ended')
                    : i18n.t('toolCall.userQuestion.awaiting')}
                </div>
              {:else if userQuestionSummary.status === 'skipped'}
                <div class="uq-summary-note">{i18n.t('toolCall.userQuestion.skipped')}</div>
              {:else}
                <ul class="uq-summary">
                  {#each userQuestionSummary.items as item, index (index)}
                    <li>
                      <div class="uq-summary-question">
                        {#if item.header}<span class="uq-summary-chip">{item.header}</span>{/if}{item.question}
                      </div>
                      <div class="uq-summary-answer">
                        {[...item.selected, ...(item.other ? [`${i18n.t('toolCall.userQuestion.other')}：${item.other}`] : [])].join('、')}
                      </div>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          {/if}

          {#if toolApproval}
            <div class="tool-section approval">
              <ToolApprovalAction approval={toolApproval} />
            </div>
          {/if}

          {#if hasOutput}
            <div class="tool-section diagram-section">
              {#if diagramPayload}
                <DiagramRenderer payload={diagramPayload} embedded />
              {:else}
                <div class="section-header">
                  <span class="section-label">{i18n.t('toolCall.section.output')}</span>
                  <button class="copy-btn" class:copied={copySuccess} onclick={copyOutput} title={copySuccess ? i18n.t('toolCall.copySuccess') : i18n.t('toolCall.copyOutput')}>
                    {#key copyRevision}<Icon name={copySuccess ? 'check' : 'copy'} size={12} />{/key}
                  </button>
                </div>
                {#if imagePreview}
                  <div class="image-output-preview">
                    <button
                      type="button"
                      class="image-output-open"
                      onclick={imagePreview.path ? imagePreview.open : undefined}
                      disabled={!imagePreview.path}
                      title={imagePreview.path ? i18n.t('toolCall.openGeneratedImage') : undefined}
                      use:desktopContextMenu={{
                        kind: 'image',
                        filePath: imagePreview.path || undefined,
                        fileScope: filePreviewScope,
                        open: imagePreview.path ? imagePreview.open : undefined,
                      }}
                    >
                      <img src={imagePreview.src} alt={imagePreview.path || toolDisplayName} />
                    </button>
                    <div class="image-output-meta">
                      {#if imagePreview.path}
                        <span title={imagePreview.path}>{imagePreview.path}</span>
                      {/if}
                      <span>{imagePreview.mime}</span>
                      {#if typeof imagePreview.bytes === 'number'}
                        <span>{imagePreview.bytes.toLocaleString(i18n.locale)} bytes</span>
                      {/if}
                      {#if imagePreview.revisedPrompt}
                        <span title={imagePreview.revisedPrompt}>{imagePreview.revisedPrompt}</span>
                      {/if}
                    </div>
                  </div>
                {:else if isMarkdownOutput}
                  <div class="markdown-output">
                    <MarkdownContent content={outputText} isStreaming={isToolOutputStreaming} {filePreviewScope} />
                  </div>
                {:else}
                  <pre class="section-content">{outputText}</pre>
                {/if}
              {/if}
            </div>
          {/if}

          {#if skillApplyPolicy}
            <div class="tool-section">
              <div class="section-header">
                <span class="section-label">{i18n.t('toolCall.policy.title')}</span>
              </div>
              <div class="policy-card">
                {#if skillApplyPolicy.activeInstructionSkillName}
                  <div class="policy-row">
                    <span class="policy-key">{i18n.t('toolCall.policy.skill')}</span>
                    <span class="policy-value">{skillApplyPolicy.activeInstructionSkillName}</span>
                  </div>
                {/if}
                {#if skillApplyPolicy.allowedToolNames && skillApplyPolicy.allowedToolNames.length > 0}
                  <div class="policy-row policy-column">
                    <span class="policy-key">{i18n.t('toolCall.policy.allowedTools')}</span>
                    <span class="policy-value policy-wrap">{skillApplyPolicy.allowedToolNames.join(', ')}</span>
                  </div>
                {/if}
                {#if skillApplyPolicy.readOnly}
                  <div class="policy-row">
                    <span class="policy-key">{i18n.t('toolCall.policy.mode')}</span>
                    <span class="policy-value">{i18n.t('toolCall.policy.readOnly')}</span>
                  </div>
                {/if}
                {#if skillApplyPolicy.allowedFilePatternGroups && skillApplyPolicy.allowedFilePatternGroups.length > 0}
                  <div class="policy-row policy-column">
                    <span class="policy-key">{i18n.t('toolCall.policy.allowedPaths')}</span>
                    <span class="policy-value policy-wrap">
                      {skillApplyPolicy.allowedFilePatternGroups.map((group) => group.join(' | ')).join(' ; ')}
                    </span>
                  </div>
                {/if}
                {#if skillApplyPolicy.forbiddenFilePatterns && skillApplyPolicy.forbiddenFilePatterns.length > 0}
                  <div class="policy-row policy-column">
                    <span class="policy-key">{i18n.t('toolCall.policy.blockedPaths')}</span>
                    <span class="policy-value policy-wrap">{skillApplyPolicy.forbiddenFilePatterns.join(', ')}</span>
                  </div>
                {/if}
              </div>
            </div>
          {/if}

          {#if hasError}
            <div class="tool-section error" class:unconfirmed={outcomeUnconfirmed}>
              <div class="section-header">
                <span class="section-label">{i18n.t(outcomeUnconfirmed ? 'toolCall.section.unconfirmed' : 'toolCall.section.error')}</span>
                {#if errorDiagnosis && !outcomeUnconfirmed}
                  <span class="diagnosis-owner">{errorDiagnosis.ownerLabel}</span>
                {/if}
              </div>
              <div class="section-content error-content">{publicErrorMessage}</div>
              {#if outcomeUnconfirmed}
                <div class="error-hint">{i18n.t('toolCall.unconfirmedHint')}</div>
              {:else if errorDiagnosis}
                <div class="error-hint">{errorDiagnosis.hint}</div>
              {/if}
            </div>
          {/if}

          {#if outputFooter}{@render outputFooter()}{/if}
        </div>
      </Disclosure>
    </div>
  {/if}
{/if}

<style>
  .tool-call {
    margin-top: var(--space-2);
    overflow: hidden;
  }

  /* 文件变更工具：紧凑 header-only 卡片，不可展开 */
  .tool-call.file-mutation {
    border: none;
    background: transparent;
    margin-top: var(--space-2);
  }

  /* 只读查看工具（file_read）：紧凑但有卡片背景 */
  .tool-call.compact-readonly {
    margin-top: var(--space-2);
  }

  .file-mutation-header.clickable {
    cursor: pointer;
  }

  .tool-icon {
    display: flex;
    color: var(--foreground-muted);
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    overflow: hidden;
  }

  .tool-summary {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 1;
  }

  .error-tag { display:inline-flex; padding:0; font-size:var(--text-xs); line-height:1.4; white-space:nowrap; flex-shrink:0; color:var(--foreground-muted); }
  .error-model_input,.error-permission,.error-policy,.error-model_output { color:var(--warning); }
  .error-context_stale { color:var(--info); }
  .error-workspace_write,.error-runtime { color:var(--error); }

  .tool-content {
    padding: 12px var(--tool-card-padding) 14px;
    border-top: var(--magi-hairline) solid var(--border);
    background: transparent;
  }

  .tool-content.diagram-content {
    padding: 0;
    background: var(--code-bg);
  }

  .tool-section { margin-top: var(--space-3); }
  .tool-section:first-child { margin-top: 0; }
  .diagram-section { margin-top: 0; }
  .tool-content.diagram-content .diagram-section { margin: 0; }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-2);
  }

  .section-label {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .policy-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
  }

  .policy-row {
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
  }

  .policy-row.policy-column {
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-1);
  }

  .policy-key {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    flex-shrink: 0;
  }

  .policy-value {
    font-size: var(--text-sm);
    color: var(--foreground);
  }

  .policy-wrap {
    white-space: pre-wrap;
    word-break: break-word;
  }

  .copy-btn {
    display: flex;
    align-items: center;
    padding: 2px 6px;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .copy-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .copy-btn.copied :global(svg) {
    animation: magi-feedback-pop 500ms var(--magi-spring-bouncy),
      magi-feedback-fade 160ms 2340ms ease-out forwards;
  }

  .section-content {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    background: var(--code-bg);
    padding: var(--space-3);
    border-radius: var(--radius-sm);
    overflow-x: auto;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 300px;
    overflow-y: auto;
  }

  .image-output-preview {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .image-output-preview img {
    display: block;
    max-width: min(100%, 640px);
    max-height: 420px;
    object-fit: contain;
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--code-bg);
  }

  .image-output-open {
    width: fit-content;
    max-width: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: pointer;
  }

  .image-output-open:disabled {
    cursor: default;
  }

  .image-output-open:not(:disabled):focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
    border-radius: var(--radius-sm);
  }

  .image-output-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .image-output-meta span {
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .error-content {
    color: var(--error);
    background: transparent;
  }

  .tool-section.unconfirmed .error-content {
    color: var(--warning);
    background: transparent;
  }

  .diagnosis-owner {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .error-hint {
    background: transparent;
    box-shadow: none;
    margin-top: var(--space-3);
    padding: 0;
    border-radius: 0;
    border: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    background: var(--surface-1);
  }

  .markdown-output {
    font-size: var(--text-sm);
    background: var(--code-bg);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-sm);
    max-height: 400px;
    overflow-y: auto;
  }

  .tool-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: var(--space-3);
    padding-top: var(--space-3);
    border-top: 1px dashed var(--border);
  }

  /* 识别色属于 Task；状态仍独立使用语义色。两种模式复用同一入口。 */
  .agent-spawn-card {
    width: 100%;
    box-sizing: border-box;
    appearance: none;
    font: inherit;
    text-align: left;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 8px 12px;
    margin-top: var(--space-2);
    color: var(--foreground);
    border: 0;
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--agent-accent) 5%, var(--magi-surface-main));
    box-shadow: none;
    transition: background-color var(--transition-fast);
  }

  .agent-spawn-card::before {
    content: '';
    position: absolute;
    inset: 7px auto 7px 0;
    width: 2px;
    border-radius: 2px;
    background: var(--agent-accent);
  }

  .agent-spawn-card:disabled { opacity: 1; }
  .agent-spawn-card.clickable { cursor: pointer; }
  .agent-spawn-card:focus-visible { outline: 0; box-shadow: inset 0 0 0 1.5px var(--primary); }
  .agent-spawn-card.clickable:hover { background: color-mix(in srgb, var(--agent-accent) 10%, var(--magi-surface-main)); }

  .agent-spawn-icon {
    display: flex;
    color: color-mix(in srgb, var(--agent-accent) 75%, var(--foreground));
    flex-shrink: 0;
  }

  .agent-spawn-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }

  .agent-spawn-model {
    display: flex;
    align-items: baseline;
    gap: 4px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
    white-space: nowrap;
  }
  .agent-spawn-model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .agent-spawn-effort { flex-shrink: 0; }

  .agent-spawn-title-line { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
  .agent-spawn-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    line-height: 1.45;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .agent-spawn-role { flex-shrink: 0; color: var(--foreground-muted); font-size: var(--text-xs); }
  .agent-spawn-state {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .status-running .agent-spawn-state { color: var(--info); }
  .status-error .agent-spawn-state { color: var(--error); }
  .status-degraded .agent-spawn-state,
  .status-pending .agent-spawn-state { color: var(--warning); }
  .agent-spawn-chevron { display: flex; color: var(--foreground-muted); flex-shrink: 0; }

  .agent-spawn-error { color: var(--error); }
  .status-degraded .agent-spawn-error { color: var(--warning); }
  .agent-spawn-error,
  .agent-spawn-diagnostic,
  .agent-spawn-instruction {
    font-size: var(--text-xs);
    line-height: 1.45;
    white-space: normal;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .agent-spawn-diagnostic { color: var(--foreground-muted); }
  .agent-spawn-instruction { color: var(--foreground-secondary); }

  .agent-spawn-card.compact-agent { margin-top: 4px; padding-block: 6px; }
  @container (max-width: 360px) { .agent-spawn-role { display: none; } }
  @media (prefers-reduced-motion: reduce) { .agent-spawn-card { transition: none; } }

  .uq-summary {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .uq-summary-question {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .uq-summary-chip {
    display: inline-block;
    margin-right: var(--space-1);
    padding: 0 var(--space-2);
    border: var(--magi-hairline) solid var(--border);
    border-radius: 999px;
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .uq-summary-answer {
    margin-top: 2px;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    overflow-wrap: anywhere;
  }

  .uq-summary-note {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
</style>
