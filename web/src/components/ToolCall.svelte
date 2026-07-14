<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import FileSpan from './FileSpan.svelte';
  import DiagramRenderer from './DiagramRenderer.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import AccessProfileSwitchAction from './AccessProfileSwitchAction.svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { vscode } from '../lib/vscode-bridge';
  import { extractLeadingJson } from '../lib/terminal-utils';
  import type { IconName } from '../lib/icons';
  import type { StandardizedToolResult, ToolPolicyPayload } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { getCurrentSessionId, messagesState } from '../stores/messages.svelte';
  import { diagramSummary, parseToolDiagramPayload } from '../lib/diagram-payload';
  import { openAgentTab } from '../stores/right-pane.svelte';
  import { getAgentRunState } from '../stores/agent-run-store.svelte';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { parseToolIdentity } from '../lib/tool-identity';
  import {
    formatViewImageToolOutput,
    parseViewImagePreview,
  } from '../lib/view-image-preview';
  import {
    formatImageGenerationToolOutput,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import { agentUrl, buildFilePreviewQuery } from '../web/agent-api';
  import {
    ACCESS_MODE_APPROVAL_ERROR_CODES,
    isAccessModeApprovalErrorPayload,
    isStructuredToolErrorPayload,
    publicToolPayloadMessage,
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

  interface ErrorDiagnosis {
    category: 'model_input' | 'context_stale' | 'permission' | 'role_constraint' | 'policy' | 'model_output' | 'workspace_write' | 'runtime';
    categoryLabel: string;
    ownerLabel: string;
    message: string;
    hint: string;
  }

  // Props
  interface Props {
    name: string;
    id?: string;
    input?: unknown;
    output?: unknown;
    error?: string;
    standardized?: StandardizedToolResult;
    status?: 'pending' | 'running' | 'success' | 'error';
    duration?: number;
    filepath?: string;
    filePreviewScope?: FilePreviewScope;
  }

  let {
    name,
    id,
    input,
    output,
    error,
    standardized,
    status = 'success',
    duration,
    filepath,
    filePreviewScope = undefined,
  }: Props = $props();

  let collapsed = $state(untrack(() => !(status === 'running' || status === 'pending')));
  let lastStatus = $state(untrack(() => status));
  let userToggled = $state(false);
  let copySuccess = $state(false);
  let lastLoggedErrorSignature = $state('');

  const TOOL_DISPLAY_NAME_KEYS: Record<string, string> = {
    'tool_result': 'toolCall.displayName.default',
    'skill_apply': 'toolCall.displayName.skillApply',
    'shell_exec': 'toolCall.displayName.shell',
    'file_read': 'toolCall.displayName.fileView',
    'view_image': 'toolCall.displayName.viewImage',
    'image_generate': 'toolCall.displayName.imageGenerate',
    'file_write': 'toolCall.displayName.fileCreate',
    'file_patch': 'toolCall.displayName.fileEdit',
    'apply_patch': 'toolCall.displayName.applyPatch',
    'file_remove': 'toolCall.displayName.fileRemove',
    'file_mkdir': 'toolCall.displayName.fileMkdir',
    'file_copy': 'toolCall.displayName.fileCopy',
    'file_move': 'toolCall.displayName.fileMove',
    'search_text': 'toolCall.displayName.grepSearch',
    'search_semantic': 'toolCall.displayName.codebaseRetrieval',
    'process_inspect': 'toolCall.displayName.processInspect',
    'diff_preview': 'toolCall.displayName.diffPreview',
    'web_search': 'toolCall.displayName.webSearch',
    'web_fetch': 'toolCall.displayName.webFetch',
    'diagram_render': 'toolCall.displayName.diagramRender',
    'knowledge_query': 'toolCall.displayName.knowledgeQuery',
    'code_symbols': 'toolCall.displayName.codeSymbols',
    'tool_catalog': 'toolCall.displayName.toolCatalog',
    'agent_spawn': 'toolCall.displayName.agentSpawn',
    'agent_wait': 'toolCall.displayName.agentWait',
    'todo_write': 'toolCall.displayName.todoWrite',
    'memory_write': 'toolCall.displayName.memoryWrite',
  };

  function toolDisplayNameI18nKey(name: string): string {
    const explicitKey = TOOL_DISPLAY_NAME_KEYS[name];
    if (explicitKey) return explicitKey;
    const suffix = name
      .split('_')
      .filter(Boolean)
      .map((part, index) => index === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1))
      .join('');
    return suffix ? `toolCall.displayName.${suffix}` : '';
  }

  function formatToolNameFallback(name: string): string {
    const parts = name.split('_').map((part) => part.trim()).filter(Boolean);
    if (parts.length === 0) return name;
    return parts
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join(' ');
  }

  $effect(() => {
    if (status === lastStatus) {
      return;
    }
    lastStatus = status;
    if (status === 'running' || status === 'pending') {
      if (!userToggled) {
        collapsed = false;
      }
      return;
    }
    if (!userToggled) {
      collapsed = true;
    }
  });

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

    const parsedTool = parseToolIdentity(toolName);
    if (parsedTool.source === 'mcp') {
      return 'plug';
    }
    if (parsedTool.source === 'skill') {
      return 'plug';
    }
    const baseToolName = parsedTool.baseName;

    const iconMap: Record<string, IconName> = {
      'view_image': 'eye',
      'image_generate': 'sparkles',
      'file_remove': 'trash',
      'web_search': 'search',
      'web_fetch': 'globe',
      'diagram_render': 'git-branch',
      'skill_apply': 'skill',
      'shell_exec': 'terminal',
      'file_read': 'eye',
      'file_write': 'file-plus',
      'file_patch': 'pencil',
      'apply_patch': 'file-edit',
      'file_mkdir': 'folder',
      'file_copy': 'file-plus',
      'file_move': 'file-plus',
      'search_text': 'search',
      'search_semantic': 'search',
      'process_inspect': 'terminal',
      'diff_preview': 'file-text',
      'knowledge_query': 'question',
      'code_symbols': 'search',
      'tool_catalog': 'tool',
      'agent_spawn': 'bot',
      'agent_wait': 'hourglass',
      'todo_write': 'list',
      'memory_write': 'database',
    };

    if (iconMap[baseToolName]) {
      return iconMap[baseToolName];
    }

    const lowerName = baseToolName.toLowerCase();
    if (lowerName.includes('search') || lowerName.includes('semantic') || lowerName.includes('query')) return 'search';
    if (lowerName.includes('read') || lowerName.includes('view')) return 'file-text';
    if (lowerName.includes('write') || lowerName.includes('edit')) return 'file-edit';
    if (lowerName.includes('delete') || lowerName.includes('remove')) return 'file-minus';
    if (lowerName.includes('process')) return 'terminal';
    if (lowerName.includes('web') || lowerName.includes('fetch') || lowerName.includes('browser')) return 'globe';
    if (lowerName.includes('diagram') || lowerName.includes('mermaid')) return 'git-branch';
    if (lowerName.includes('task')) return 'document';
    if (lowerName.includes('mcp')) return 'plug';
    return 'tool';
  }

  type VisualToolStatus = 'pending' | 'running' | 'success' | 'error';

  function visualStatusInfo(value: VisualToolStatus): { class: string } {
    const map: Record<string, { class: string }> = {
      pending: { class: 'pending' },
      running: { class: 'running' },
      success: { class: 'success' },
      error: { class: 'error' },
    };
    return map[value] || { class: 'success' };
  }

  // 状态信息
  const statusInfo = $derived.by(() => {
    return visualStatusInfo(status);
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
    /** 代理角色（如 executor / explorer / tester），用于角标与色 token */
    role: string;
    /** 代理 TaskId，作为 RightPane tab 去重 key；未就绪时为 undefined */
    childTaskId: string | undefined;
    /** 代理终态结果字符串（succeeded / degraded / failed / killed），未终态为 undefined */
    outcome: 'succeeded' | 'degraded' | 'failed' | 'killed' | undefined;
    /** 失败原因摘要，若有 */
    error: string | undefined;
  }

  const agentSpawnDisplay = $derived.by((): AgentSpawnDisplay | null => {
    if (!isAgentSpawn) return null;
    const inputObj = (input && typeof input === 'object' && !Array.isArray(input))
      ? input as Record<string, unknown>
      : {};
    const inputTitle = typeof inputObj.display_name === 'string' ? inputObj.display_name.trim() : '';
    const inputRole = typeof inputObj.role === 'string' ? inputObj.role.trim() : '';

    // output 可能是 JSON 字符串（tool_batch 返回 .to_string()）或已被解析为 object。
    let parsedOutput: Record<string, unknown> | null = null;
    if (output && typeof output === 'object' && !Array.isArray(output)) {
      parsedOutput = output as Record<string, unknown>;
    } else if (typeof output === 'string') {
      const trimmed = output.trim();
      if (trimmed.startsWith('{')) {
        try {
          const parsed = JSON.parse(trimmed);
          if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
            parsedOutput = parsed as Record<string, unknown>;
          }
        } catch {
          // 流式过程中可能拿到不完整 JSON，忽略
        }
      }
    }

    const outputTitle = typeof parsedOutput?.title === 'string' ? (parsedOutput.title as string).trim() : '';
    const outputRole = typeof parsedOutput?.role === 'string' ? (parsedOutput.role as string).trim() : '';
    const outputChildId = typeof parsedOutput?.child_task_id === 'string'
      ? (parsedOutput.child_task_id as string).trim()
      : '';
    const outputStatus = typeof parsedOutput?.status === 'string' ? parsedOutput.status : '';
    const outputError = typeof parsedOutput?.error === 'string' ? (parsedOutput.error as string) : '';

    const outcome: AgentSpawnDisplay['outcome'] =
      outputStatus === 'succeeded' || outputStatus === 'degraded' || outputStatus === 'failed' || outputStatus === 'killed'
        ? outputStatus
        : undefined;

    return {
      title: outputTitle || inputTitle || i18n.t('toolCall.agentSpawn.defaultTitle'),
      role: outputRole || inputRole || '',
      childTaskId: outputChildId || undefined,
      outcome,
      error: outputError || undefined,
    };
  });

  const agentSpawnRoleVisual = $derived.by(() => {
    const role = agentSpawnDisplay?.role;
    if (!role) return null;
    return getAgentVisualInfo(role);
  });

  const agentProjectionTask = $derived.by(() => {
    const childTaskId = agentSpawnDisplay?.childTaskId;
    if (!childTaskId) return null;
    const projectionState = getAgentRunState(
      filePreviewScope?.sessionId || getCurrentSessionId(),
      filePreviewScope?.workspaceId || messagesState.currentWorkspaceId,
    );
    const projection = projectionState.projection;
    if (!projection) return null;
    const agent = projection.agents.find((item) => item.agentRunId === childTaskId);
    const task = projection.tasks.find((item) => item.task_id === childTaskId);
    return { agent, task };
  });

  const agentSpawnVisualStatus = $derived.by((): VisualToolStatus => {
    const projectionStatus = agentProjectionTask?.agent?.status || agentProjectionTask?.task?.status;
    if (projectionStatus === 'pending') return 'pending';
    if (projectionStatus === 'running') return 'running';
    if (projectionStatus === 'failed' || projectionStatus === 'killed') return 'error';
    if (projectionStatus === 'completed') return 'success';
    if (agentSpawnDisplay?.outcome === 'failed' || agentSpawnDisplay?.outcome === 'killed') return 'error';
    if (agentSpawnDisplay?.outcome === 'succeeded' || agentSpawnDisplay?.outcome === 'degraded') return 'success';
    return status;
  });

  const agentSpawnStatusInfo = $derived(visualStatusInfo(agentSpawnVisualStatus));
  const agentSpawnFailed = $derived(
    agentSpawnVisualStatus === 'error'
    || agentSpawnDisplay?.outcome === 'failed'
    || agentSpawnDisplay?.outcome === 'killed'
  );
  const agentSpawnRunning = $derived(agentSpawnVisualStatus === 'running' || agentSpawnVisualStatus === 'pending');

  function openAgentSpawnTab(display: AgentSpawnDisplay | null): void {
    if (!display || !display.childTaskId) return;
    openAgentTab(filePreviewScope?.sessionId || getCurrentSessionId(), display.childTaskId, {
      label: display.title,
      accentToken: agentSpawnRoleVisual?.color ?? null,
      workspaceId: filePreviewScope?.workspaceId || messagesState.currentWorkspaceId,
      workspacePath: filePreviewScope?.workspacePath || messagesState.currentWorkspacePath,
    });
  }


  // 目录/文件只读工具：只需紧凑 header
  const isCompactReadOnlyTool = $derived(name === 'file_read');
  // 仅 view 类工具支持点击整行 header 打开文件
  const isHeaderOpenableTool = $derived(name === 'file_read' || name === 'view');

  // 检查是否有内容
  const inputText = $derived(formatToolInput(input));
  const hasInput = $derived(!!inputText);
  const outputIsStructuredError = $derived(isStructuredToolErrorPayload(output));
  const generatedImagePreview = $derived(
    outputIsStructuredError ? null : parseImageGenerationPreview(name, output),
  );
  const imagePreview = $derived.by(() => {
    if (outputIsStructuredError) return null;
    const viewed = parseViewImagePreview(name, output);
    if (viewed) return { ...viewed, revisedPrompt: '' };
    if (!generatedImagePreview) return null;
    return {
      ...generatedImagePreview,
      src: agentUrl('/api/files/raw', buildFilePreviewQuery(generatedImagePreview.path, {
        workspaceId: filePreviewScope?.workspaceId,
        workspacePath: filePreviewScope?.workspacePath,
        sessionId: '',
      })),
    };
  });
  const outputText = $derived(outputIsStructuredError ? '' : formatToolOutput(name, output));
  const hasOutput = $derived(!!output && (!!outputText || !!imagePreview));
  const structuredErrorText = $derived.by(() => {
    if (!outputIsStructuredError) {
      return '';
    }
    const publicMessage = publicToolPayloadMessage(output);
    if (publicMessage) {
      return publicMessage;
    }
    const errorCode = toolPayloadErrorCode(output);
    const payloadStatus = toolPayloadStatus(output);
    return JSON.stringify({
      error_code: errorCode,
      status: payloadStatus || 'failed',
    });
  });
  const errorForDiagnosis = $derived((error && error.trim()) || structuredErrorText);
  const hasError = $derived(!!errorForDiagnosis);

  const diagramPayload = $derived(parseToolDiagramPayload(name, output));
  const isDiagramTool = $derived(!!diagramPayload);

  $effect(() => {
    if (diagramPayload && status === 'success' && !userToggled) {
      collapsed = false;
    }
  });

  $effect(() => {
    if (generatedImagePreview && status === 'success' && !userToggled) {
      collapsed = false;
    }
  });

  const skillApplyPolicy = $derived.by(() => {
    if (name !== 'skill_apply') return null;
    const data = standardized?.data;
    if (!data || typeof data !== 'object' || Array.isArray(data)) return null;
    const toolPolicy = (data as Record<string, unknown>).toolPolicy;
    if (!toolPolicy || typeof toolPolicy !== 'object' || Array.isArray(toolPolicy)) return null;
    return toolPolicy as ToolPolicyPayload;
  });
  const hasContent = $derived(hasInput || hasOutput || hasError);
  const canExpand = $derived(hasContent && !isCompactReadOnlyTool && !isCompactMutation);
  const shouldRenderCard = $derived(hasContent || isCompactReadOnlyTool || isCompactMutation);

  // 获取工具显示名
  function getToolDisplayName(toolName: string): string {
    if (!toolName || typeof toolName !== 'string') return i18n.t('toolCall.displayName.default');
    const parsedTool = parseToolIdentity(toolName);
    if (parsedTool.source === 'mcp') {
      return parsedTool.displayName;
    }
    if (parsedTool.source === 'skill') {
      return parsedTool.displayName;
    }
    const baseToolName = parsedTool.baseName;
    const key = toolDisplayNameI18nKey(baseToolName);
    const translated = key ? i18n.t(key) : '';
    return translated && translated !== key ? translated : formatToolNameFallback(baseToolName);
  }

  // 从工具参数中提取语义摘要
  function getToolSummary(toolName: string, toolInput: unknown): string {
    if (!toolInput || typeof toolInput !== 'object') return '';
    const args = toolInput as Record<string, unknown>;
    const parsedTool = parseToolIdentity(toolName);
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
    diagramPayload
      ? diagramSummary(diagramPayload)
      : toolTargetSummary || getToolSummary(name, input)
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

  function isAccessModeApprovalError(errorText?: string, toolResult?: StandardizedToolResult): boolean {
    const errorCode = toolErrorCodeForDiagnosis(errorText, toolResult);
    return ACCESS_MODE_APPROVAL_ERROR_CODES.some((pattern) => errorCode.includes(pattern));
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
      return {
        category: 'policy',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.policy.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.policy.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.policy.message'),
        hint: i18n.t('toolCall.errorDiagnosis.policy.hint'),
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
      'file_remove_invalid_args',
      'file_path_required',
      'file_path_outside_workspace',
      'tool_rejected',
      'command rejected',
      'argument parse failed',
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

    // 主模型角色约束（agent_spawn 引导）— 与用户权限无关，是系统架构层面的职责划分
    if (matches(
      'orchestrator',
      'agent_spawn delegation',
      '请通过 agent_spawn 委派给代理',
    )) {
      return {
        category: 'role_constraint',
        categoryLabel: i18n.t('toolCall.errorDiagnosis.roleConstraint.categoryLabel'),
        ownerLabel: i18n.t('toolCall.errorDiagnosis.roleConstraint.ownerLabel'),
        message: i18n.t('toolCall.errorDiagnosis.roleConstraint.message'),
        hint: i18n.t('toolCall.errorDiagnosis.roleConstraint.hint'),
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
  const shouldOfferFullAccessSwitch = $derived.by(() =>
    isAccessModeApprovalError(errorForDiagnosis, standardized)
    || isAccessModeApprovalErrorPayload(output)
    || isAccessModeApprovalErrorPayload(error)
    || isAccessModeApprovalErrorPayload(standardized?.message)
  );
  const publicPayloadErrorMessage = $derived(
    publicToolPayloadMessage(output)
    || publicToolPayloadMessage(error)
    || publicToolPayloadMessage(standardized?.message)
  );
  const publicErrorMessage = $derived(
    publicPayloadErrorMessage
    || errorDiagnosis?.message
    || i18n.t('toolCall.errorDiagnosis.runtime.message')
  );

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
    userToggled = true;
  }

  async function copyOutput() {
    const content = formatToolOutput(name, output);
    if (!content) return;
    try {
      await navigator.clipboard.writeText(content);
      copySuccess = true;
      setTimeout(() => { copySuccess = false; }, 2000);
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
    if (typeof window !== 'undefined') {
      const previewEvent = new CustomEvent('magi:previewFile', {
        detail: { filepath: toolFilepath, ...filePreviewScope },
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
    <span class="tool-name">{toolDisplayName}</span>
    {#if status === 'error' && errorDiagnosis}
      <span class="error-tag error-{errorDiagnosis.category}" title={errorDiagnosis.ownerLabel}>
        {errorDiagnosis.categoryLabel}
      </span>
    {/if}
    {#if toolFilepath}
      <FileSpan filepath={toolFilepath} showIcon={false} clickable={true} onClick={handleOpenFile} />
    {:else if toolSummary}
      <span class="tool-summary" title={toolSummary}>{toolSummary}</span>
    {/if}
  </span>

  <span class="tool-status status-{statusInfo.class}">
    {#if status === 'running' || status === 'pending'}
      <span class="status-dot pulsing"></span>
    {:else}
      <span class="status-dot"></span>
    {/if}
  </span>
{/snippet}

{#if isAgentSpawn && agentSpawnDisplay}
  {@const display = agentSpawnDisplay}
  {@const canOpen = !!display.childTaskId}
  <button
    type="button"
    class="agent-spawn-card status-{agentSpawnStatusInfo.class}"
    class:clickable={canOpen}
    disabled={!canOpen}
    onclick={canOpen ? () => openAgentSpawnTab(display) : undefined}
    data-tool-name="agent_spawn"
    data-tool-call-id={id || undefined}
    data-agent-task-id={display.childTaskId || undefined}
  >
    <span class="agent-spawn-icon">
      <Icon name={agentSpawnRoleVisual?.icon ?? 'bot'} size={16} />
    </span>
    <span class="agent-spawn-body">
      <span class="agent-spawn-title-line">
        <span class="agent-spawn-title">{display.title}</span>
        {#if agentSpawnRoleVisual}
          <span
            class="agent-spawn-role-badge"
            style="color: {agentSpawnRoleVisual.color}; background: {agentSpawnRoleVisual.muted};"
          >
            {agentSpawnRoleVisual.label}
          </span>
        {/if}
      </span>
      {#if agentSpawnFailed}
        <span class="agent-spawn-error">{i18n.t('toolCall.agentSpawn.failed')}</span>
      {/if}
    </span>
    <span class="agent-spawn-meta">
      <span class="tool-status status-{agentSpawnStatusInfo.class}">
        {#if agentSpawnRunning}
          <span class="status-dot pulsing"></span>
        {:else}
          <span class="status-dot"></span>
        {/if}
      </span>
      {#if canOpen}
        <span class="agent-spawn-cta">
          {i18n.t('toolCall.agentSpawn.viewDetails')}
          <Icon name="chevron-right" size={12} />
        </span>
      {:else if agentSpawnRunning}
        <span class="agent-spawn-cta agent-spawn-cta-pending">{i18n.t('toolCall.agentSpawn.dispatching')}</span>
      {/if}
    </span>
  </button>
{:else if isFileMutationTool && status === 'success'}
  <!-- 文件变更工具完成：由 FileChangeCard 全权展示 -->
{:else}
  {#if shouldRenderCard}
    <div
      class="tool-call"
      class:collapsed={canExpand && collapsed}
      class:has-error={hasError}
      class:file-mutation={isCompactMutation}
      class:compact-readonly={isCompactReadOnlyTool}
      data-tool-name={name}
      data-tool-call-id={id || undefined}
    >
      {#if canExpand}
        <button class="tool-header" onclick={toggle}>
          <span class="chevron">
            <Icon name="chevron-right" size={12} />
          </span>
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

      {#if canExpand && !collapsed}
        <div class="tool-content" class:diagram-content={isDiagramTool}>
          {#if hasInput && !isDiagramTool}
            <div class="tool-section">
              <div class="section-header">
                <span class="section-label">{i18n.t('toolCall.section.input')}</span>
              </div>
              <pre class="section-content">{inputText}</pre>
            </div>
          {/if}

          {#if hasOutput}
            <div class="tool-section diagram-section">
              {#if diagramPayload}
                <DiagramRenderer payload={diagramPayload} embedded />
              {:else}
                <div class="section-header">
                  <span class="section-label">{i18n.t('toolCall.section.output')}</span>
                  <button class="copy-btn" onclick={copyOutput} title={copySuccess ? i18n.t('toolCall.copySuccess') : i18n.t('toolCall.copyOutput')}>
                    <Icon name={copySuccess ? 'check' : 'copy'} size={12} />
                  </button>
                </div>
                {#if imagePreview}
                  <div class="image-output-preview">
                    <button
                      type="button"
                      class="image-output-open"
                      onclick={imagePreview.path ? handleOpenFile : undefined}
                      disabled={!imagePreview.path}
                      title={imagePreview.path ? i18n.t('toolCall.openGeneratedImage') : undefined}
                    >
                      <img src={imagePreview.src} alt={imagePreview.path || toolDisplayName} />
                    </button>
                    <div class="image-output-meta">
                      {#if imagePreview.path}
                        <span title={imagePreview.path}>{imagePreview.path}</span>
                      {/if}
                      <span>{imagePreview.mime}</span>
                      {#if typeof imagePreview.bytes === 'number'}
                        <span>{imagePreview.bytes.toLocaleString()} bytes</span>
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
            <div class="tool-section error">
              <div class="section-header">
                <span class="section-label">{i18n.t('toolCall.section.error')}</span>
                {#if errorDiagnosis}
                  <span class="diagnosis-owner">{errorDiagnosis.ownerLabel}</span>
                {/if}
              </div>
              <div class="section-content error-content">{publicErrorMessage}</div>
              {#if errorDiagnosis}
                <div class="error-hint">{errorDiagnosis.hint}</div>
              {/if}
              {#if shouldOfferFullAccessSwitch}
                <AccessProfileSwitchAction />
              {/if}
            </div>
          {/if}

          {#if duration}
            <div class="tool-meta">
              <Icon name="clock" size={12} />
              {i18n.t('toolCall.duration')} <strong>{(duration / 1000).toFixed(2)}s</strong>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
{/if}

<style>
  .tool-call {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-top: var(--space-2);
    overflow: hidden;
    background: var(--surface-1);
  }

  .tool-call.has-error {
    border-color: var(--error);
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

  /* header 共享规范（高度/padding/字号/accent 条/chevron）见 styles/tool-card.css；
     此处仅保留 ToolCall 特有的 clickable 变体 */
  .file-mutation-header.clickable {
    cursor: pointer;
  }

  /* tool-icon 中性化：accent 条承担状态色，图标用 muted 避免三层颜色冲突 */
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
    opacity: 0.8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 1;
  }

  .error-tag {
    display: inline-flex;
    align-items: center;
    padding: 1px 6px;
    border-radius: 999px;
    border: 1px solid transparent;
    font-size: 10px;
    line-height: 1.4;
    font-weight: 500;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .error-model_input {
    color: var(--warning);
    border-color: rgba(245, 158, 11, 0.45);
    background: rgba(245, 158, 11, 0.12);
  }

  .error-context_stale {
    color: var(--info);
    border-color: rgba(59, 130, 246, 0.45);
    background: rgba(59, 130, 246, 0.12);
  }

  .error-permission {
    color: var(--warning);
    border-color: rgba(234, 179, 8, 0.45);
    background: rgba(234, 179, 8, 0.12);
  }

  .error-role_constraint {
    color: var(--info);
    border-color: rgba(139, 92, 246, 0.45);
    background: rgba(139, 92, 246, 0.12);
  }

  .error-policy {
    color: var(--warning);
    border-color: rgba(249, 115, 22, 0.45);
    background: rgba(249, 115, 22, 0.12);
  }

  .error-model_output {
    color: var(--warning);
    border-color: rgba(14, 165, 233, 0.45);
    background: rgba(14, 165, 233, 0.12);
  }

  .error-workspace_write {
    color: var(--error);
    border-color: rgba(220, 38, 38, 0.45);
    background: rgba(220, 38, 38, 0.12);
  }

  .error-runtime {
    color: var(--error);
    border-color: rgba(239, 68, 68, 0.45);
    background: rgba(239, 68, 68, 0.12);
  }

  .tool-status {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }

  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }

  .status-dot.pulsing {
    animation: pulse 1.5s ease-in-out infinite;
  }

  .status-pending { color: var(--warning); }
  .status-running { color: var(--info); }
  .status-success { color: var(--success); }
  .status-error { color: var(--error); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.3; }
  }

  .tool-content {
    padding: var(--space-3);
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    animation: slideDown 0.2s ease-out;
    transform-origin: top;
  }

  .tool-content.diagram-content {
    padding: 0;
    background: var(--code-bg);
  }

  @keyframes slideDown {
    from { opacity: 0; max-height: 0; transform: translateY(-8px); }
    to { opacity: 1; max-height: 500px; transform: translateY(0); }
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
    border: 1px solid var(--border);
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
    border: 1px solid var(--border);
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
    background: rgba(239, 68, 68, 0.1);
  }

  .diagnosis-owner {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .error-hint {
    margin-top: var(--space-3);
    padding: var(--space-3);
    border-radius: var(--radius-sm);
    border: 1px dashed var(--border);
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

  /* ===== agent_spawn 代理派发卡片 ===== */
  /* 父代理 ToolCall 流中的内嵌单元——一次 agent_spawn 即一张卡片，多个并行派发
     即多张并列卡片，点击进入 RightPane 查看该代理完整 transcript（按
     metadata.taskId 过滤）。 */
  .agent-spawn-card {
    width: 100%;
    appearance: none;
    font: inherit;
    text-align: left;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    margin-top: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    color: inherit;
    transition: border-color var(--transition-fast), background var(--transition-fast);
  }

  .agent-spawn-card:disabled {
    opacity: 1;
  }

  .agent-spawn-card.clickable {
    cursor: pointer;
  }

  .agent-spawn-card.clickable:hover {
    border-color: var(--info);
    background: var(--surface-hover);
  }

  .agent-spawn-card.status-error {
    border-color: var(--error);
  }

  .agent-spawn-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    color: var(--foreground-muted);
    flex-shrink: 0;
  }

  .agent-spawn-body {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    flex: 1;
    min-width: 0;
  }

  .agent-spawn-title-line {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .agent-spawn-title {
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .agent-spawn-role-badge {
    display: inline-flex;
    align-items: center;
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 11px;
    line-height: 1.4;
    font-weight: 500;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .agent-spawn-error {
    font-size: var(--text-xs);
    color: var(--error);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .agent-spawn-meta {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    flex-shrink: 0;
  }

  .agent-spawn-cta {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .agent-spawn-card.clickable:hover .agent-spawn-cta {
    color: var(--info);
  }

  .agent-spawn-cta-pending {
    font-style: italic;
  }
</style>
