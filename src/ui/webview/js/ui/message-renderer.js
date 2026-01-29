// 消息渲染模块
// 此文件包含所有消息和 UI 渲染相关的函数

import {
  threadMessages,
  agentOutputs,
  currentBottomTab,
  currentSessionId,
  isProcessing,
  thinkingStartAt,
  processingActor,
  scrollPositions,
  autoScrollEnabled,
  pendingChanges,
  tasks,
  sessions,
  currentDependencyAnalysis,
  isDependencyPanelExpanded,
  streamingHintTimer,
  setDependencyAnalysis,
  setDependencyPanelExpanded,
  saveScrollPosition,
  saveWebviewState,
  setStreamingHintTimer,
  stopStreamingHintTimer
} from '../core/state.js';

import { postMessage } from '../core/vscode-api.js';

// 渲染器模块
import { renderParsedBlocks, renderMarkdown } from './renderers/markdown-renderer.js';

// 渲染辅助函数（统一从 render-utils.js 导入）
import {
  getRoleIcon,
  getRoleInfo,
  getMessageGroupKey,
  cleanInternalProtocolData,
  formatTime
} from './renderers/render-utils.js';

// 新设计系统组件渲染器
import {
  renderThinking,
  renderToolCallList,
  renderCodeBlock,
  renderInlineCode,
  registerGlobalFunctions
} from './renderers/components.js';

// DOM Diff 引擎
import { morphContainer, isMorphdomAvailable } from '../core/dom-diff.js';

// 流式更新管理器
import { streamingManager, setRenderCallback } from '../core/streaming-manager.js';

// 配置管理
import { getConfig } from '../core/config.js';

const STREAM_TIMEOUT = 5 * 60 * 1000;

function updateStreamingHintsWithTimeout() {
  const now = Date.now();
  let hasTimeout = false;

  threadMessages.forEach(m => {
    if (m.streaming && m.startedAt && (now - m.startedAt > STREAM_TIMEOUT)) {
      m.streaming = false;
      m.timeout = true;
      m.content = (m.content || '') + '\n\n[超时] 响应超时，已自动结束';
      hasTimeout = true;
    }
  });

  ['claude', 'codex', 'gemini'].forEach(agent => {
    const msgs = agentOutputs[agent] || [];
    msgs.forEach(m => {
      if (m.streaming && m.startedAt && (now - m.startedAt > STREAM_TIMEOUT)) {
        m.streaming = false;
        m.timeout = true;
        m.content = (m.content || '') + '\n\n[超时] 响应超时，已自动结束';
        hasTimeout = true;
      }
    });
  });

  if (hasTimeout) {
    saveWebviewState();
    renderMainContent();
  }
}

function startStreamingHintTimer() {
  if (streamingHintTimer) return;
  const timer = setInterval(updateStreamingHintsWithTimeout, 1000);
  setStreamingHintTimer(timer);
  updateStreamingHintsWithTimeout();
}

// 创建 state 对象供向后兼容
const state = {
  get tasks() { return tasks; },
  get pendingChanges() { return pendingChanges; }
};

import {
  escapeHtml,
  formatTimestamp,
  formatElapsed,
  formatRelativeTime,
  shouldCollapseMessage,
  toggleMessageExpand,
  parseCodeBlockMeta,
  shouldRenderAsCodeBlock,
  extractSingleCodeFence,
  smoothScrollToBottom
} from '../core/utils.js';

// ============================================
// 辅助函数
// ============================================

/**
 * 获取消息的唯一标识（用于 DOM diff）
 */
export function getMessageKey(message) {
  return message.standardMessageId || message.streamKey || message.id || null;
}

/**
 * 生成 Thinking 内容的智能摘要
 * @param {string} content - Thinking 内容
 * @returns {string} 摘要文本
 */

// 旧的 generateThinkingSummary 函数已删除
// 现在使用新的组件渲染器中的实现

// ============================================
// 渲染函数
// ============================================

let mcpServers = [];
let repositories = [];
let skillsConfig = null;
let currentEditingMCPServer = null;

export function setMcpServers(list) {
  mcpServers = Array.isArray(list) ? list : [];
}

export function setRepositories(list) {
  repositories = Array.isArray(list) ? list : [];
}

export function setSkillsConfig(config) {
  skillsConfig = config || null;
}

// ============================================
// 主渲染函数
// ============================================

export function renderMainContent() {
      const container = document.getElementById('main-content');
      if (!container) return;

      // 添加滚动监听器（只添加一次）
      if (!container.dataset.scrollListener) {
        container.dataset.scrollListener = 'true';
        container.addEventListener('scroll', () => {
          const atBottom = container.scrollTop + container.clientHeight >= container.scrollHeight - 20;
          autoScrollEnabled[currentBottomTab] = atBottom;
          saveWebviewState();
        }, { passive: true });
      }

      // 判断是否在底部（用于流式输出时自动滚动）
      const scrollHeight = container.scrollHeight;
      const clientHeight = container.clientHeight;
      const scrollTop = container.scrollTop;
      const wasAtBottom = autoScrollEnabled[currentBottomTab]
        || scrollTop + clientHeight >= scrollHeight - 50;

      // 渲染对应的视图
      if (currentBottomTab === 'thread') {
        renderThreadView(container);
      } else if (['claude', 'codex', 'gemini'].includes(currentBottomTab)) {
        renderAgentOutputView(container, currentBottomTab);
      }

      // 🔧 优化：如果之前在底部，渲染后保持在底部（用于流式输出）
      // 使用 requestAnimationFrame 确保在 DOM 更新后执行
      // 但不使用 smooth 滚动，避免抖动
      if (wasAtBottom) {
        requestAnimationFrame(() => {
          // 直接设置 scrollTop，不使用 smooth 动画
          container.scrollTop = container.scrollHeight;
          autoScrollEnabled[currentBottomTab] = true;
        });
      }
      // morphdom 会自动保留其他情况下的滚动位置
      // overflow-anchor: auto 会帮助维持滚动锚点
    }

export function renderThreadView(container) {
      if (threadMessages.length === 0) {
        const emptyHTML = '<div class="empty-state"><div class="empty-state-icon"><svg width="24" height="24" viewBox="0 0 16 16" fill="currentColor"><path d="M14 1a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H4.414A2 2 0 0 0 3 11.586l-2 2V2a1 1 0 0 1 1-1h12zM2 0a2 2 0 0 0-2 2v12.793a.5.5 0 0 0 .854.353l2.853-2.853A1 1 0 0 1 4.414 12H14a2 2 0 0 0 2-2V2a2 2 0 0 0-2-2H2z"/></svg></div><span class="empty-state-text">开始新对话</span><span class="empty-state-hint">输入任务描述，按 ⌘↵ 发送</span></div>';
        morphContainer(container, emptyHTML);
        return;
      }

      // 依赖分析面板（Thread面板特有）
      const dependencyHtml = renderDependencyPanel() || '';

      // 使用统一的消息列表渲染函数
      const messagesHtml = renderMessageList(threadMessages, {
        tabType: 'thread',
        defaultAgent: null, // Thread 面板没有默认 Worker
        toolPanelPrefix: 'tool-thread-'
      }) || '';

      // Thread 面板特有：显示运行中的 Worker 状态卡片
      const runningEntries = collectWorkerStatusEntries();
      const workerStatusHtml = runningEntries.length > 0 ? (renderWorkerStatusCard(runningEntries) || '') : '';

      // 统一的动画渲染入口
      const animationHtml = renderStreamingAnimation(threadMessages) || '';

      // 组合完整 HTML
      const html = dependencyHtml + messagesHtml + workerStatusHtml + animationHtml;

      // 使用 morphdom 更新 DOM（保留未变化的节点）
      morphContainer(container, html);

      // 启动/停止流式动画计时器
      if (container.querySelector('.message-streaming-hint, .message-streaming-footer')) {
        startStreamingHintTimer();
      } else {
        stopStreamingHintTimer();
      }
    }

export function renderAgentOutputView(container, agent) {
      const messages = agentOutputs[agent] || [];
      if (!messages.length) {
        const emptyHTML = '<div class="empty-state"><div class="empty-state-icon"><svg width="24" height="24" viewBox="0 0 16 16" fill="currentColor"><path d="M6 9a.5.5 0 0 1 .5-.5h3a.5.5 0 0 1 0 1h-3A.5.5 0 0 1 6 9zM3.854 4.146a.5.5 0 1 0-.708.708L4.793 6.5 3.146 8.146a.5.5 0 1 0 .708.708l2-2a.5.5 0 0 0 0-.708l-2-2z"/><path d="M2 1a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V3a2 2 0 0 0-2-2H2zm12 1a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1h12z"/></svg></div><span class="empty-state-text">' + agent.toUpperCase() + ' 输出</span><span class="empty-state-hint">暂无输出内容</span></div>';
        morphContainer(container, emptyHTML);
        return;
      }

      // 使用统一的消息列表渲染函数
      let html = renderMessageList(messages, {
        tabType: 'agent',
        defaultAgent: agent,
        toolPanelPrefix: 'tool-agent-' + agent + '-'
      }) || '';

      // Worker 面板也使用统一的动画渲染入口
      html += renderStreamingAnimationForAgent(messages, agent) || '';

      // 使用 morphdom 更新 DOM
      morphContainer(container, html);

      // Worker 面板也需要启动/停止动画计时器
      if (container.querySelector('.message-streaming-hint, .message-streaming-footer')) {
        startStreamingHintTimer();
      }
    }


// ============================================
// 消息渲染
// ============================================

export function renderMessageList(messages, options) {
      const { tabType, defaultAgent, toolPanelPrefix } = options;

      if (!messages || messages.length === 0) {
        return '';
      }

      // 按时间戳排序
      const sortedMessages = [...messages]
        .map((m, idx) => ({ m, idx }))
        .sort((a, b) => {
          const tsA = a.m.timestamp || a.m.startedAt || a.idx;
          const tsB = b.m.timestamp || b.m.startedAt || b.idx;
          if (tsA !== tsB) return tsA - tsB;
          return a.idx - b.idx;
        });

      // 🔧 重构：动画逻辑已移至 renderStreamingAnimation 函数
      // renderMessageList 只负责渲染消息，不处理动画

      let html = '';
      let prevMessageKey = null;

      sortedMessages.forEach(({ m, idx }) => {
        // 跳过空内容消息（避免只渲染徽章/时间的空行）
        // 🔧 修复：对于 orchestrator 消息，即使内容为空也应该渲染（可能是工具调用或状态消息）
        const isOrchestrator = m.source === 'orchestrator';
        const hasRenderableContent =
          m.streaming ||
          (m.content && String(m.content).trim()) ||
          (m.codeBlocks && m.codeBlocks.length > 0) ||
          (m.thinking && m.thinking.length > 0) ||
          (m.images && m.images.length > 0) ||
          (m.metadata && (m.metadata.subTaskCard || m.metadata.summaryCard)) ||
          (m.parsedBlocks && m.parsedBlocks.some(b =>
            (b.type === 'text' && b.content && String(b.content).trim()) ||
            (b.type === 'code' && b.content && String(b.content).trim()) ||
            (b.type === 'thinking' && b.content && String(b.content).trim()) ||
            (b.type === 'tool_call' && (b.input || b.output || b.error))
          )) ||
          (m.toolCalls && m.toolCalls.some(t =>
            (t.input && String(t.input).trim()) ||
            (t.output && String(t.output).trim()) ||
            (t.result && String(t.result).trim()) ||
            (t.error && String(t.error).trim())
          )) ||
          (isOrchestrator && m.standardMessageId);  // 🔧 orchestrator 消息总是渲染

        // 尝试渲染特殊消息
        const specialHtml = renderSpecialMessage(m, idx, tabType);
        if (specialHtml !== null) {
          html += specialHtml;
          prevMessageKey = null; // 特殊消息不参与分组
          return;
        }

        // 🔧 修复：跳过 Worker 的空消息（如果是普通文本类型的空消息）
        // 编排者消息保留渲染（可能有状态意义），流式消息保留渲染
        if (!isOrchestrator && !m.streaming && !hasRenderableContent && !m.toolCalls?.length && !m.metadata) {
          return;
        }

        if (!hasRenderableContent && !isOrchestrator) {
          return;
        }

        // 普通消息渲染
        const isUser = m.role === 'user';
        const source = m.source || 'worker';
        const currentMessageKey = getMessageGroupKey(m, source);
        const isGrouped = prevMessageKey === currentMessageKey;
        prevMessageKey = currentMessageKey;

        const { roleName, badgeClass } = getRoleInfo(m, source, defaultAgent);
        const agent = m.agent || defaultAgent || 'claude';

        html += renderMessageBlock(m, idx, {
          isUser,
          source,
          agent,
          grouped: isGrouped,
          roleName,
          badgeClass,
          toolPanelPrefix: toolPanelPrefix + idx
        });
      });

      return html;
    }

export function renderMessageBlock(message, idx, options) {
      const isUser = !!options.isUser;
      const source = options.source || 'worker';
      const agent = options.agent || message.agent || 'claude';
      const grouped = !!options.grouped;
      const roleName = options.roleName || '';
      const badgeClass = options.badgeClass || '';
      const toolPanelPrefix = options.toolPanelPrefix || 'tool-thread-';
      // 🔧 重构：移除 showStreamingHint，动画统一由 renderStreamingAnimation 处理

      const streamingClass = message.streaming ? ' streaming' : '';
      const messageTypeClass = isUser ? ' user-message' : ' assistant-message';
      const groupedClass = grouped ? ' grouped' : '';
      // 🆕 澄清消息和 Worker 问题消息的特殊样式类
      const clarificationClass = message.isClarification ? ' clarification-message' : '';
      const workerQuestionClass = message.isWorkerQuestion ? ' worker-question-message' : '';
      const streamKeyAttr = message.streamKey ? (' data-stream-key="' + message.streamKey + '"') : '';
      const agentAttr = agent ? (' data-agent="' + agent + '"') : '';
      // 🔧 增量更新：添加消息唯一标识用于 DOM 追踪
      const messageKey = getMessageKey(message);
      const messageKeyAttr = messageKey ? (' data-message-key="' + messageKey + '"') : '';
      let html = '<div class="message' + messageTypeClass + streamingClass + groupedClass + clarificationClass + workerQuestionClass + '" data-msg-idx="' + idx + '"' + streamKeyAttr + agentAttr + messageKeyAttr + '>';
      html += '<div class="message-body">';

      // 悬停操作按钮（非用户消息，且存在可操作项）
      if (!isUser && ((message.isClarification && window._pendingClarification) || (message.isWorkerQuestion && window._pendingWorkerQuestion))) {
        html += '<div class="message-actions">';
        // 🆕 澄清消息添加跳过按钮
        if (message.isClarification && window._pendingClarification) {
          html += '<button class="message-action-btn" onclick="skipClarification()" title="跳过澄清"><svg width="16" height="16" viewBox="0 0 16 16"><path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/></svg></button>';
        }
        // 🆕 Worker 问题消息添加跳过按钮
        if (message.isWorkerQuestion && window._pendingWorkerQuestion) {
          html += '<button class="message-action-btn" onclick="skipWorkerQuestion()" title="跳过问题"><svg width="16" height="16" viewBox="0 0 16 16"><path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/></svg></button>';
        }
        html += '</div>';
      }

      html += '<div class="message-header">';
      // 🆕 澄清消息显示特殊徽章
      if (message.isClarification) {
        html += '<span class="badge badge--sm badge--warning"><svg viewBox="0 0 16 16" width="12" height="12" fill="currentColor"><path d="M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14zm0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16z"/><path d="M5.255 5.786a.237.237 0 0 0 .241.247h.825c.138 0 .248-.113.266-.25.09-.656.54-1.134 1.342-1.134.686 0 1.314.343 1.314 1.168 0 .635-.374.927-.965 1.371-.673.489-1.206 1.06-1.168 1.987l.003.217a.25.25 0 0 0 .25.246h.811a.25.25 0 0 0 .25-.25v-.105c0-.718.273-.927 1.01-1.486.609-.463 1.244-.977 1.244-2.056 0-1.511-1.276-2.241-2.673-2.241-1.267 0-2.655.59-2.75 2.286zm1.557 5.763c0 .533.425.927 1.01.927.609 0 1.028-.394 1.028-.927 0-.552-.42-.94-1.029-.94-.584 0-1.009.388-1.009.94z"/></svg>需求澄清</span>';
      }
      // 🆕 Worker 问题消息显示特殊徽章
      if (message.isWorkerQuestion) {
        const workerId = message.workerQuestionData?.workerId || 'Worker';
        html += '<span class="badge badge--sm badge--warning"><svg viewBox="0 0 16 16" width="12" height="12" fill="currentColor"><path d="M11 6a3 3 0 1 1-6 0 3 3 0 0 1 6 0z"/><path fill-rule="evenodd" d="M0 8a8 8 0 1 1 16 0A8 8 0 0 1 0 8zm8-7a7 7 0 0 0-5.468 11.37C3.242 11.226 4.805 10 8 10s4.757 1.225 5.468 2.37A7 7 0 0 0 8 1z"/></svg>' + escapeHtml(workerId) + ' 提问</span>';
      }
      if (!isUser && roleName && !message.isClarification && !message.isWorkerQuestion) {
        html += '<span class="badge badge--md badge--' + badgeClass + '">' + roleName + '</span>';
      }
      // 使用相对时间（无时间则不渲染，避免空占位）
      const timestamp = message.timestamp || message.startedAt;
      const timeDisplay = timestamp ? formatRelativeTime(timestamp) : (message.time || '');
      if (timeDisplay) {
        const timestampAttr = timestamp ? (' data-timestamp="' + timestamp + '"') : '';
        html += '<span class="message-time"' + timestampAttr + '>' + timeDisplay + '</span>';
      }
      html += '</div>';

      if (!isUser && message.thinking && message.thinking.length > 0) {
        const panelId = (toolPanelPrefix || 'panel-') + 'thinking-' + idx;

        // 使用新的 thinking 组件渲染器
        html += renderThinking({
          thinking: message.thinking,
          isStreaming: !!message.streaming,
          panelId: panelId,
          autoExpand: message.streaming
        });
      }

      let contentHtml = '';
      let contentIsMarkdown = false;
      let rawContent = '';
      if (message.images && message.images.length > 0) {
        contentHtml += '<div class="message-images">';
        message.images.forEach(imgSrc => {
          contentHtml += '<img src="' + imgSrc + '" class="message-image-thumb" onclick="showImageViewer(this.src)" />';
        });
        contentHtml += '</div>';
      }
      if (!isUser && message.content) {
        rawContent = message.content;
        // 🆕 使用智能渲染：优先使用后端已解析的 blocks
        const rendered = renderMessageContentSmart(message, agent);
        contentHtml += rendered.html;
        contentIsMarkdown = rendered.isMarkdown;
      } else if (!isUser && !message.content && !message.streaming) {
        // 🔧 修复：空内容的非流式消息显示占位符（可能是工具调用消息）
        if (message.toolCalls && message.toolCalls.length > 0) {
          // 有工具调用，不需要占位符
          rawContent = '';
        } else if (source === 'orchestrator') {
          // Orchestrator 空消息显示占位符
          contentHtml += '<div class="empty-message-placeholder" style="color: var(--vscode-descriptionForeground); font-style: italic; padding: 8px 0;">（处理中...）</div>';
          rawContent = '';
        } else {
          rawContent = message.content || '';
          contentHtml += escapeHtml(rawContent);
        }
      } else {
        rawContent = message.content || '';
        contentHtml += escapeHtml(rawContent);
      }

      // 🔧 修复：移除对普通消息的折叠
      // 根据设计原则，只有特殊内容面板才需要折叠：
      // - 思考过程 (thinking) - 已在上面通过 collapsible-panel 处理
      // - 工具调用 (toolCalls) - 在下面通过 renderToolTrack 处理
      // - 代码块 - 在 renderParsedBlocks 中通过 collapsible-panel 处理
      // 普通AI回复（包括统计、解释等）不应该被折叠
      const contentClass = 'message-content' + (contentIsMarkdown ? ' markdown-rendered' : '');
      html += '<div class="' + contentClass + '">' + contentHtml + '</div>';

      if (!isUser && message.toolCalls && message.toolCalls.length > 0) {
        // 使用新的工具调用组件渲染器
        html += renderToolCallList(message.toolCalls, toolPanelPrefix + idx);
      }

      if (!isUser && message.reconnecting) {
        html += '<div class="reconnect-indicator"><svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><path d="M11.534 7h3.932a.25.25 0 0 1 .192.41l-1.966 2.36a.25.25 0 0 1-.384 0l-1.966-2.36a.25.25 0 0 1 .192-.41zm-11 2h3.932a.25.25 0 0 0 .192-.41L2.692 6.23a.25.25 0 0 0-.384 0L.342 8.59A.25.25 0 0 0 .534 9z"/><path d="M8 3c-1.552 0-2.94.707-3.857 1.818a.5.5 0 1 1-.771-.636A6.002 6.002 0 0 1 13.917 7H12.9A5.002 5.002 0 0 0 8 3zM3.1 9a5.002 5.002 0 0 0 8.757 2.182.5.5 0 1 1 .771.636A6.002 6.002 0 0 1 2.083 9H3.1z"/></svg>' + escapeHtml(message.reconnectMessage || '正在重连...') + '</div>';
      }

      // 🔧 重构：动画逻辑已移至 renderStreamingAnimation 函数
      // renderMessageBlock 不再负责渲染动画，确保单一入口

      html += '</div></div>';
      return html;
    }

export function renderUnifiedCard(options) {
      const {
        type = 'default',      // 卡片类型：task, summary, error, notice, plan, question, tool
        variant = '',          // 变体：orchestrator, claude, codex, gemini, success, error, warning, info
        icon = '',             // 图标 SVG 字符串
        title = '',            // 标题
        badges = [],           // 徽章数组 [{ text, class }]
        time = '',             // 时间
        content = '',          // 内容 HTML
        footer = '',           // 底部 HTML
        collapsed = false,     // 是否可折叠
        expanded = true,       // 是否展开
        panelId = '',          // 折叠面板 ID
        dataAttrs = {},        // 额外 data-* 属性
        className = ''         // 额外 CSS 类
      } = options;

      const variantClass = variant ? ' card-' + variant : '';
      const typeClass = ' card-type-' + type;
      const collapsedClass = collapsed ? ' collapsible-panel' : '';
      const extraClass = className ? ' ' + className : '';

      // 构建 data 属性
      let dataStr = '';
      if (panelId) dataStr += ' data-panel-id="' + panelId + '"';
      Object.entries(dataAttrs).forEach(([key, val]) => {
        dataStr += ' data-' + key + '="' + escapeHtml(String(val)) + '"';
      });

      let html = '<div class="unified-card' + typeClass + variantClass + collapsedClass + extraClass + '"' + dataStr + '>';

      // 卡片头部
      if (title || icon || badges.length || time) {
        const headerClick = collapsed && panelId ? ' onclick="togglePanel(\'' + panelId + '\')"' : '';
        html += '<div class="card-header"' + headerClick + '>';
        if (icon) {
          html += '<span class="card-icon">' + icon + '</span>';
        }
        if (title) {
          html += '<span class="card-title">' + escapeHtml(title) + '</span>';
        }
        if (badges.length > 0) {
          html += '<span class="card-badges">';
          badges.forEach(badge => {
            const badgeClass = badge.class ? ' ' + badge.class : '';
            html += '<span class="card-badge' + badgeClass + '">' + escapeHtml(badge.text) + '</span>';
          });
          html += '</span>';
        }
        if (time) {
          html += '<span class="card-time">' + escapeHtml(time) + '</span>';
        }
        if (collapsed) {
          html += '<span class="collapsible-icon' + (expanded ? ' expanded' : '') + '"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M6 12.796V3.204L11.481 8 6 12.796z"/></svg></span>';
        }
        html += '</div>';
      }

      // 卡片内容
      if (content) {
        const contentExpandedClass = collapsed ? (expanded ? ' expanded' : '') : '';
        if (collapsed) {
          html += '<div class="collapsible-content' + contentExpandedClass + '"><div class="card-body">' + content + '</div></div>';
        } else {
          html += '<div class="card-body">' + content + '</div>';
        }
      }

      // 卡片底部
      if (footer) {
        html += '<div class="card-footer">' + footer + '</div>';
      }

      html += '</div>';
      return html;
    }

export function renderSpecialMessage(m, idx, tabType) {
      if (m.metadata?.subTaskCard) {
        return renderSubTaskSummaryCard(m);
      }
      if (m.metadata?.summaryCard) {
        return renderSummaryCard(m);
      }
      // 任务卡片（Worker 面板专用）
      if (m.type === 'task_card') {
        return renderTaskCard(m, idx);
      }
      // 系统通知（所有面板通用）
      if (m.type === 'system_notice') {
        return renderSystemNotice(m, idx);
      }
      // 错误消息（所有面板通用）
      if (m.type === 'error' || m.isError) {
        return renderErrorMessage(m, idx);
      }
      // 以下仅 Thread 面板支持
      if (tabType === 'thread') {
        if (m.type === 'plan_confirmation') {
          return renderPlanConfirmationCard(m, idx);
        }
        if (m.type === 'plan_ready') {
          return renderPlanPreviewCard(m, idx);
        }
        if (m.type === 'question_request') {
          return renderQuestionCard(m, idx);
        }
        // 🔧 Worker 询问卡片
        if (m.type === 'worker_question') {
          return renderWorkerQuestionCard(m, idx);
        }
        // 注意：clarification_request 和 worker_question 现在作为普通 assistant 消息显示
        // 不再使用特殊卡片渲染
      }
      return null; // 普通消息，需要使用 renderMessageBlock
    }

export function renderMessageContentSmart(message, agent) {
      let blocks = (message.parsedBlocks && message.parsedBlocks.length > 0)
        ? message.parsedBlocks
        : (message.content ? [{ type: 'text', content: message.content, isMarkdown: true }] : []);

      if (!blocks.length) {
        return { html: '', isMarkdown: false };
      }

      // 🔧 新增：过滤内部协议数据
      blocks = blocks.map(block => {
        if (block.type === 'text' && block.content) {
          const cleaned = cleanInternalProtocolData(block.content);
          if (cleaned !== block.content) {
            return { ...block, content: cleaned };
          }
        }
        return block;
      }).filter(block => {
        // 过滤掉清理后为空的文本块
        if (block.type === 'text') {
          return block.content && block.content.trim();
        }
        return true;
      });

      if (!blocks.length) {
        return { html: '', isMarkdown: false };
      }

      // 如果已经有 toolCalls 渲染通道，避免 tool_call 块重复渲染
      if (message.toolCalls && message.toolCalls.length > 0) {
        blocks = blocks.filter(block => block.type !== 'tool_call');
      }

      return renderParsedBlocks(blocks, agent);
    }



// ============================================
// 卡片渲染
// ============================================

export function renderSubTaskSummaryCard(message) {
      const card = message.metadata?.subTaskCard;
      if (!card) return null;
      const statusText = card.status === 'failed' ? '失败' : '完成';
      const badgeClass = card.status === 'failed' ? 'badge-failed' : 'badge-completed';
      const agent = card.executor || card.agent || '';
      const agentClass = agent.toLowerCase().includes('claude') ? 'claude' : agent.toLowerCase().includes('codex') ? 'codex' : agent.toLowerCase().includes('gemini') ? 'gemini' : '';

      // 构建概览统计
      let contentHtml = '<div class="worker-summary-overview">';
      contentHtml += '<div class="worker-summary-stat">';
      contentHtml += '<span class="worker-summary-stat-label">执行者</span>';
      contentHtml += '<span class="worker-summary-stat-value">' + escapeHtml(agent || '未知') + '</span>';
      contentHtml += '</div>';
      contentHtml += '<div class="worker-summary-stat">';
      contentHtml += '<span class="worker-summary-stat-label">耗时</span>';
      contentHtml += '<span class="worker-summary-stat-value">' + escapeHtml(card.duration || '-') + '</span>';
      contentHtml += '</div>';
      contentHtml += '<div class="worker-summary-stat">';
      contentHtml += '<span class="worker-summary-stat-label">状态</span>';
      contentHtml += '<span class="worker-summary-stat-value ' + (card.status === 'failed' ? 'error' : 'success') + '">' + statusText + '</span>';
      contentHtml += '</div>';
      if (card.toolCount !== undefined) {
        contentHtml += '<div class="worker-summary-stat">';
        contentHtml += '<span class="worker-summary-stat-label">工具调用</span>';
        contentHtml += '<span class="worker-summary-stat-value">' + card.toolCount + ' 次</span>';
        contentHtml += '</div>';
      }
      contentHtml += '</div>';

      // 任务描述
      if (card.description) {
        contentHtml += '<div class="tool-section">';
        contentHtml += '<div class="tool-section-title">任务描述</div>';
        contentHtml += '<div class="tool-text">' + escapeHtml(card.description) + '</div>';
        contentHtml += '</div>';
      }

      // 错误信息（如果有）
      if (card.error) {
        contentHtml += '<div class="tool-section">';
        contentHtml += '<div class="tool-section-title" style="color: var(--color-error)">错误信息</div>';
        contentHtml += '<div class="tool-text" style="color: var(--color-error)">' + escapeHtml(card.error) + '</div>';
        contentHtml += '</div>';
      }

      // 文件变更列表
      const changes = card.changes && card.changes.length > 0 ? card.changes : [];
      if (changes.length > 0) {
        contentHtml += '<div class="worker-summary-changes">';
        contentHtml += '<div class="worker-summary-changes-title">文件变更 (' + changes.length + ')</div>';
        contentHtml += '<ul class="worker-summary-file-list">';
        changes.forEach(file => {
          const fileStr = String(file);
          // 检测变更类型
          let iconClass = 'modified';
          let iconSvg = '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M5.5 7.5a.5.5 0 0 1 .5-.5h4a.5.5 0 0 1 0 1h-4a.5.5 0 0 1-.5-.5z"/><path d="M8 2a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM1.5 7.5a6.5 6.5 0 1 1 13 0 6.5 6.5 0 0 1-13 0z"/></svg>';
          if (fileStr.includes('新增') || fileStr.includes('create') || fileStr.includes('+')) {
            iconClass = 'created';
            iconSvg = '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 4a.5.5 0 0 1 .5.5v3h3a.5.5 0 0 1 0 1h-3v3a.5.5 0 0 1-1 0v-3h-3a.5.5 0 0 1 0-1h3v-3A.5.5 0 0 1 8 4z"/></svg>';
          } else if (fileStr.includes('删除') || fileStr.includes('delete') || fileStr.includes('-')) {
            iconClass = 'deleted';
            iconSvg = '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M5.5 7.5a.5.5 0 0 1 .5-.5h4a.5.5 0 0 1 0 1h-4a.5.5 0 0 1-.5-.5z"/></svg>';
          }
          contentHtml += '<li class="worker-summary-file-item">';
          contentHtml += '<span class="worker-summary-file-icon ' + iconClass + '">' + iconSvg + '</span>';
          contentHtml += '<span class="worker-summary-file-path">' + escapeHtml(fileStr) + '</span>';
          contentHtml += '</li>';
        });
        contentHtml += '</ul>';
        contentHtml += '</div>';
      }

      // 验证提醒
      const verification = card.verification && card.verification.length > 0 ? card.verification : [];
      if (verification.length > 0) {
        contentHtml += '<div class="tool-section">';
        contentHtml += '<div class="tool-section-title" style="color: var(--color-warning)">验证提醒</div>';
        contentHtml += '<ul class="summary-list">';
        verification.forEach(item => {
          contentHtml += '<li>' + escapeHtml(item) + '</li>';
        });
        contentHtml += '</ul>';
        contentHtml += '</div>';
      }

      return renderUnifiedCard({
        type: 'summary',
        variant: card.status === 'failed' ? 'error' : 'success',
        icon: getRoleIcon(agentClass || 'success'),
        title: card.title || 'Worker 执行摘要',
        badges: [
          { text: agent ? agent.toUpperCase() : 'WORKER', class: 'badge--' + (agentClass || 'primary') },
          { text: statusText, class: 'badge--' + (card.status === 'failed' ? 'error' : 'success') }
        ],
        content: contentHtml,
        collapsed: true,       // 默认折叠
        expanded: false,
        className: 'worker-summary-card',
      });
    }

export function renderSummaryCard(message) {
      const card = message.metadata?.summaryCard;
      if (!card) return null;
      return renderUnifiedCard({
        type: 'summary',
        variant: 'success',
        title: card.title || '执行总结',
        badges: [{ text: '总结', class: 'badge--success' }],
        content: renderSummarySections(card.sections || []),
        collapsed: false,
      });
    }

// 旧的 renderToolCallItem 和 renderToolTrack 函数已被删除
// 现在使用新的组件渲染器：renderToolCallList (来自 renderers/tool-call-renderer.js)

export function renderStructuredPlanContent(plan) {
      let html = '<div class="structured-plan-content">';

      // 目标
      if (plan.goal) {
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title">目标</div>';
        html += '<div class="plan-section-content">' + escapeHtml(plan.goal) + '</div>';
        html += '</div>';
      }

      // 分析
      if (plan.analysis) {
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title">分析</div>';
        html += '<div class="plan-section-content">' + escapeHtml(plan.analysis) + '</div>';
        html += '</div>';
      }

      // 约束条件
      if (plan.constraints && Array.isArray(plan.constraints) && plan.constraints.length > 0) {
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title">约束条件</div>';
        html += '<ul class="plan-list">';
        for (const constraint of plan.constraints) {
          html += '<li>' + escapeHtml(String(constraint)) + '</li>';
        }
        html += '</ul>';
        html += '</div>';
      }

      // 验收标准
      if (plan.acceptanceCriteria && Array.isArray(plan.acceptanceCriteria) && plan.acceptanceCriteria.length > 0) {
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title"><svg class="icon-inline" viewBox="0 0 16 16" width="14" height="14"><path fill="currentColor" d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0z"/></svg> 验收标准</div>';
        html += '<ul class="plan-list">';
        for (const criteria of plan.acceptanceCriteria) {
          html += '<li>' + escapeHtml(String(criteria)) + '</li>';
        }
        html += '</ul>';
        html += '</div>';
      }

      // 风险等级
      if (plan.riskLevel) {
        const riskColors = {
          'low': 'var(--vscode-testing-iconPassed)',
          'medium': 'var(--vscode-editorWarning-foreground)',
          'high': 'var(--vscode-errorForeground)'
        };
        const riskLabels = {
          'low': '低',
          'medium': '中',
          'high': '高'
        };
        const riskColor = riskColors[plan.riskLevel] || 'var(--vscode-foreground)';
        const riskLabel = riskLabels[plan.riskLevel] || plan.riskLevel;
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title">风险等级</div>';
        html += '<div class="plan-section-content">';
        html += '<span class="risk-badge" style="background: ' + riskColor + '; color: #fff; padding: 2px 8px; border-radius: 4px; font-size: 0.9em;">' + escapeHtml(riskLabel) + '</span>';
        html += '</div>';
        html += '</div>';
      }

      // 风险因素
      if (plan.riskFactors && Array.isArray(plan.riskFactors) && plan.riskFactors.length > 0) {
        html += '<div class="plan-section">';
        html += '<div class="plan-section-title">风险因素</div>';
        html += '<ul class="plan-list risk-list">';
        for (const factor of plan.riskFactors) {
          html += '<li>' + escapeHtml(String(factor)) + '</li>';
        }
        html += '</ul>';
        html += '</div>';
      }

      html += '</div>';
      return html;
    }


export function updateEditsBadge() {
      const badge = document.getElementById('edits-badge');
      if (badge) {
        const count = pendingChanges.length;
        badge.textContent = count.toString();
        badge.style.display = count > 0 ? 'inline' : 'none';
      }
    }

export function updateTasksBadge() {
      const badge = document.getElementById('tasks-badge');
      if (badge) {
        const currentTask = (tasks || []).find(t => t.status === 'running' && t.subTasks?.length > 0)
          || (tasks || []).filter(t => t.subTasks?.length > 0).slice(-1)[0];
        const subTasks = currentTask?.subTasks || [];
        if (subTasks.length > 0) {
          const completedCount = subTasks.filter(st => st.status === 'completed').length;
          const totalCount = subTasks.length;
          badge.textContent = completedCount + '/' + totalCount;
          badge.style.display = 'inline';
        } else {
          badge.style.display = 'none';
        }
      }
    }

export function renderDependencyPanel() {
      if (!currentDependencyAnalysis) return '';

      const { message, analysis, mermaid } = currentDependencyAnalysis;
      const conflicts = analysis?.fileConflicts || [];
      const batches = analysis?.executionBatches || [];
      const criticalPath = analysis?.criticalPath || [];

      const isExpanded = isDependencyPanelExpanded;
      const chevron = isExpanded ? '▼' : '▶';

      let html = '<div class="dependency-panel">';
      html += `<div class="dependency-panel-header" onclick="toggleDependencyPanel()">`;
      html += `<span class="dependency-chevron">${chevron}</span>`;
      html += `<span class="dependency-title">任务依赖分析</span>`;
      html += `<span class="dependency-summary">${message || ''}</span>`;
      html += '</div>';

      if (isExpanded) {
        html += '<div class="dependency-panel-content">';

        // 执行批次信息
        if (batches.length > 0) {
          html += '<div class="dependency-section">';
          html += '<h4 class="dependency-section-title">执行批次</h4>';
          html += '<div class="dependency-batches">';
          batches.forEach(batch => {
            html += `<div class="dependency-batch">`;
            html += `<span class="batch-label">批次 ${batch.batchIndex + 1}</span>`;
            html += `<span class="batch-tasks">${batch.taskIds.length} 个任务并行</span>`;
            html += '</div>';
          });
          html += '</div>';
          html += '</div>';
        }

        // 关键路径
        if (criticalPath.length > 0) {
          html += '<div class="dependency-section">';
          html += '<h4 class="dependency-section-title">关键路径</h4>';
          html += `<div class="dependency-path">${criticalPath.join(' → ')}</div>`;
          html += '</div>';
        }

        // 文件冲突
        if (conflicts.length > 0) {
          html += '<div class="dependency-section">';
          html += '<h4 class="dependency-section-title">文件冲突</h4>';
          html += '<div class="dependency-conflicts">';
          conflicts.forEach(conflict => {
            html += `<div class="dependency-conflict">`;
            html += `<span class="conflict-file">${conflict.file}</span>`;
            html += `<span class="conflict-tasks">${conflict.taskIds.length} 个任务冲突</span>`;
            html += '</div>';
          });
          html += '</div>';
          html += '</div>';
        }

        html += '</div>'; // Close dependency-panel-content
      }

      html += '</div>'; // Close dependency-panel

      return html;
    }

export function showDependencyAnalysis(data) {
      if (!data) return;
      setDependencyAnalysis(data);
      setDependencyPanelExpanded(true);
      renderMainContent();
    }

export function toggleDependencyPanel() {
      setDependencyPanelExpanded(!isDependencyPanelExpanded);
      renderMainContent();
    }

export function renderStreamingAnimation(messages) {
      // 找到最后一条 streaming 消息
      const lastStreamingMsg = [...(messages || [])].reverse().find(m => m.streaming);

      if (lastStreamingMsg) {
        // 有 streaming 消息时，追加动画
        const startAt = lastStreamingMsg.startedAt || thinkingStartAt || Date.now();
        const elapsed = formatElapsed(Date.now() - startAt);
        const hasContent = lastStreamingMsg.content && lastStreamingMsg.content.trim();

        if (!hasContent) {
          // 没有内容时显示"正在思考"
          return '<div class="message assistant-message streaming-animation-message">' +
                 '<div class="message-body">' +
                 '<div class="message-content message-streaming-hint" data-start-at="' + startAt + '">' +
                 '<span class="thinking-indicator"><span class="dot"></span><span class="dot"></span><span class="dot"></span></span> ' +
                 '正在思考 <span class="thinking-elapsed">用时 ' + elapsed + '</span></div>' +
                 '</div></div>';
        } else {
          // 有内容时显示"正在输出"
          return '<div class="message assistant-message streaming-animation-message">' +
                 '<div class="message-body">' +
                 '<div class="message-streaming-footer" data-start-at="' + startAt + '">' +
                 '<span class="thinking-indicator"><span class="dot"></span><span class="dot"></span><span class="dot"></span></span> ' +
                 '正在输出 <span class="thinking-elapsed">用时 ' + elapsed + '</span></div>' +
                 '</div></div>';
        }
      }

      // 没有 streaming 消息但正在处理时，显示兜底动画
      if (isProcessing) {
        const startAt = thinkingStartAt || Date.now();
        const elapsed = formatElapsed(Date.now() - startAt);
        // 根据 processingActor 显示不同的角色
        const actorName = processingActor?.source === 'orchestrator' ? '编排者' :
                          (processingActor?.agent ? processingActor.agent.toUpperCase() : '');
        const displayText = actorName ? actorName + ' 正在思考' : '正在思考';
        return '<div class="message assistant-message loading-message">' +
               '<div class="message-body">' +
               '<div class="message-content message-streaming-hint" data-start-at="' + startAt + '">' +
               '<span class="thinking-indicator"><span class="dot"></span><span class="dot"></span><span class="dot"></span></span> ' +
               displayText + ' <span class="thinking-elapsed">用时 ' + elapsed + '</span></div>' +
               '</div></div>';
      }

      return '';
    }

export function renderStreamingAnimationForAgent(messages, agent) {
      // 找到最后一条 streaming 消息
      const lastStreamingMsg = [...(messages || [])].reverse().find(m => m.streaming);

      if (lastStreamingMsg) {
        // 有 streaming 消息时，追加动画
        const startAt = lastStreamingMsg.startedAt || thinkingStartAt || Date.now();
        const elapsed = formatElapsed(Date.now() - startAt);
        const hasContent = lastStreamingMsg.content && lastStreamingMsg.content.trim();
        const agentName = agent ? agent.toUpperCase() : 'Worker';

        if (!hasContent) {
          // 没有内容时显示"正在思考"
          return '<div class="message assistant-message streaming-animation-message" data-agent="' + agent + '">' +
                 '<div class="message-body">' +
                 '<div class="message-content message-streaming-hint" data-start-at="' + startAt + '">' +
                 '<span class="thinking-indicator"><span class="dot"></span><span class="dot"></span><span class="dot"></span></span> ' +
                 agentName + ' 正在思考 <span class="thinking-elapsed">用时 ' + elapsed + '</span></div>' +
                 '</div></div>';
        } else {
          // 有内容时显示"正在输出"
          return '<div class="message assistant-message streaming-animation-message" data-agent="' + agent + '">' +
                 '<div class="message-body">' +
                 '<div class="message-streaming-footer" data-start-at="' + startAt + '">' +
                 '<span class="thinking-indicator"><span class="dot"></span><span class="dot"></span><span class="dot"></span></span> ' +
                 agentName + ' 正在输出 <span class="thinking-elapsed">用时 ' + elapsed + '</span></div>' +
                 '</div></div>';
        }
      }

      return '';
    }


// ============================================
// 任务和状态
// ============================================

export function renderWorkerStatusCard(entries) {
      const statusMap = { running: '执行中', pending: '等待中', completed: '已完成', failed: '失败' };

      // 构建列表内容
      let listHtml = '<div class="worker-status-list">';
      entries.forEach(item => {
        const agentClass = item.worker.includes('claude') ? 'claude' : item.worker.includes('codex') ? 'codex' : item.worker.includes('gemini') ? 'gemini' : '';
        const statusClass = item.status === 'running' ? 'warning' : item.status === 'completed' ? 'success' : item.status === 'failed' ? 'error' : 'neutral';
        const progressClass = item.status === 'completed' ? 'completed' : item.status === 'failed' ? 'failed' : '';
        const progress = item.progress !== undefined ? Math.min(100, Math.max(0, item.progress)) : (item.status === 'running' ? 50 : 0);
        const duration = item.duration || item.time || '';

        listHtml += '<div class="worker-status-item">';

        // 第一行：Worker 名称、描述、状态
        listHtml += '<div class="worker-status-row">';
        listHtml += '<span class="task-agent ' + agentClass + '">' + escapeHtml(item.worker) + '</span>';
        listHtml += '<span class="task-desc" title="' + escapeHtml(item.description) + '">' + escapeHtml(item.description) + '</span>';
        listHtml += '<span class="badge badge--sm badge--' + statusClass + '">' + (statusMap[item.status] || item.status) + '</span>';
        listHtml += '</div>';

        // 第二行：当前操作（如果有）
        if (item.currentAction && item.status === 'running') {
          listHtml += '<div class="task-current">' + escapeHtml(item.currentAction) + '</div>';
        }

        // 第三行：进度条（如果是 running 状态或有进度信息）
        if (item.status === 'running' || item.progress !== undefined) {
          listHtml += '<div class="worker-status-progress">';
          listHtml += '<div class="worker-status-progress-bar">';
          listHtml += '<div class="worker-status-progress-fill ' + progressClass + '" style="width: ' + progress + '%"></div>';
          listHtml += '</div>';
          listHtml += '<span class="worker-status-progress-text">' + progress + '%</span>';
          if (duration) {
            listHtml += '<span class="worker-status-time">' + escapeHtml(duration) + '</span>';
          }
          listHtml += '</div>';
        }

        listHtml += '</div>';
      });
      listHtml += '</div>';

      // 统计完成数量
      const runningCount = entries.filter(e => e.status === 'running').length;
      const completedCount = entries.filter(e => e.status === 'completed').length;
      const badgeText = runningCount > 0 ? runningCount + ' 执行中' : completedCount + '/' + entries.length + ' 完成';

      return renderUnifiedCard({
        type: 'worker-status',
        variant: 'info',
        icon: getRoleIcon('orchestrator'),
        title: '子代理运行状态',
        badges: [{ text: badgeText, class: runningCount > 0 ? 'badge--warning' : 'badge--primary' }],
        content: listHtml,
        className: 'worker-status-card'
      });
    }

export function renderTaskCard(m, idx) {
      const agent = m.agent || '';
      const statusClass = m.status || 'started';
      const statusText = statusClass === 'started' ? '执行中' : statusClass === 'completed' ? '已完成' : statusClass === 'failed' ? '失败' : statusClass;
      const badgeStatusClass = statusClass === 'started' ? 'badge-running' : statusClass === 'completed' ? 'badge-completed' : statusClass === 'failed' ? 'badge-failed' : '';

      // 构建任务分配卡片内容
      let contentHtml = '<div class="task-assignment-content">';

      // 任务描述
      if (m.description) {
        contentHtml += '<div class="task-field">';
        contentHtml += '<span class="task-field-label">任务描述</span>';
        contentHtml += '<span class="task-field-value">' + escapeHtml(m.description) + '</span>';
        contentHtml += '</div>';
      }

      // 目标文件
      if (m.targetFiles && m.targetFiles.length > 0) {
        contentHtml += '<div class="task-field">';
        contentHtml += '<span class="task-field-label">目标文件</span>';
        contentHtml += '<ul class="task-file-list">';
        m.targetFiles.forEach(file => {
          contentHtml += '<li><code>' + escapeHtml(file) + '</code></li>';
        });
        contentHtml += '</ul></div>';
      }

      // 分配原因
      if (m.reason) {
        contentHtml += '<div class="task-field task-reason">';
        contentHtml += '<span class="task-field-label">分配原因</span>';
        contentHtml += '<span class="task-field-value">' + escapeHtml(m.reason) + '</span>';
        contentHtml += '</div>';
      }

      // 添加进度条（如果有进度信息）
      if (m.progress !== undefined || m.subtasks) {
        contentHtml += renderTaskProgress(m);
      }

      // 添加子任务状态列表
      if (m.subtasks && m.subtasks.length > 0) {
        contentHtml += renderSubtaskStatusList(m.subtasks);
      }

      contentHtml += '</div>';

      return renderUnifiedCard({
        type: 'task-assignment',
        variant: 'orchestrator',  // 使用紫色边框标识来自编排者
        icon: getRoleIcon('orchestrator'),
        title: '来自编排者的任务',
        badges: [
          { text: agent ? agent.toUpperCase() + ' 执行' : '执行中', class: 'badge-' + (agent.toLowerCase() || 'info') },
          { text: statusText, class: badgeStatusClass }
        ],
        time: m.time || '',
        content: contentHtml,
        dataAttrs: { 'msg-idx': idx, 'subtask-id': m.subTaskId || '' },
        className: 'task-assignment-card'
      });
    }

export function renderTaskProgress(task) {
      let completed = 0;
      let total = 1;
      let failed = 0;

      if (task.subtasks && task.subtasks.length > 0) {
        total = task.subtasks.length;
        completed = task.subtasks.filter(s => s.status === 'completed').length;
        failed = task.subtasks.filter(s => s.status === 'failed').length;
      } else if (task.progress !== undefined) {
        completed = task.progress;
        total = task.total || 100;
      }

      const percent = Math.round((completed / total) * 100);
      const progressClass = failed > 0 ? 'failed' : (percent >= 100 ? 'completed' : '');

      let html = '<div class="task-progress-container">';
      html += '<div class="task-progress-header">';
      html += '<span class="task-progress-label">进度</span>';
      html += '<span class="task-progress-percent">' + completed + '/' + total + ' (' + percent + '%)</span>';
      html += '</div>';
      html += '<div class="task-progress-bar">';
      html += '<div class="task-progress-fill ' + progressClass + '" style="width: ' + percent + '%"></div>';
      html += '</div>';
      html += '</div>';

      return html;
    }

export function renderSubtaskStatusList(subtasks) {
      if (!subtasks || subtasks.length === 0) return '';

      const statusIcons = {
        pending: '<svg width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>',
        running: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 3a5 5 0 1 0 4.546 2.914.5.5 0 0 1 .908-.417A6 6 0 1 1 8 2v1z"/><path d="M8 4.466V.534a.25.25 0 0 1 .41-.192l2.36 1.966c.12.1.12.284 0 .384L8.41 4.658A.25.25 0 0 1 8 4.466z"/></svg>',
        paused: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14zm0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16z"/><path d="M5 6.25a1.25 1.25 0 0 1 2.5 0v3.5a1.25 1.25 0 0 1-2.5 0v-3.5zm3.5 0a1.25 1.25 0 0 1 2.5 0v3.5a1.25 1.25 0 0 1-2.5 0v-3.5z"/></svg>',
        retrying: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 3a5 5 0 1 0 4.546 2.914.5.5 0 0 1 .908-.417A6 6 0 1 1 8 2v1z"/><path d="M8 4.466V.534a.25.25 0 0 1 .41-.192l2.36 1.966c.12.1.12.284 0 .384L8.41 4.658A.25.25 0 0 1 8 4.466z"/></svg>',
        completed: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zm-3.97-3.03a.75.75 0 0 0-1.08.022L7.477 9.417 5.384 7.323a.75.75 0 0 0-1.06 1.06L6.97 11.03a.75.75 0 0 0 1.079-.02l3.992-4.99a.75.75 0 0 0-.01-1.05z"/></svg>',
        failed: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zM5.354 4.646a.5.5 0 1 0-.708.708L7.293 8l-2.647 2.646a.5.5 0 0 0 .708.708L8 8.707l2.646 2.647a.5.5 0 0 0 .708-.708L8.707 8l2.647-2.646a.5.5 0 0 0-.708-.708L8 7.293 5.354 4.646z"/></svg>',
        cancelled: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14zm0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16z"/><path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/></svg>',
        skipped: '<svg width="14" height="14" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.5" stroke-dasharray="2,2"/></svg>'
      };

      let html = '<div class="subtask-status-list">';

      subtasks.forEach(subtask => {
        const status = subtask.status || 'pending';
        const agent = subtask.agent || '';
        const desc = subtask.description || subtask.prompt || '';
        const time = subtask.time || '';

        html += '<div class="subtask-status-item ' + status + '">';
        html += '<span class="subtask-status-icon ' + status + '">' + (statusIcons[status] || statusIcons.pending) + '</span>';
        if (agent) {
          html += '<span class="subtask-status-agent ' + agent.toLowerCase() + '">' + agent + '</span>';
        }
        html += '<span class="subtask-status-desc" title="' + escapeHtml(desc) + '">' + escapeHtml(desc.substring(0, 50) + (desc.length > 50 ? '...' : '')) + '</span>';
        if (time) {
          html += '<span class="subtask-status-time">' + time + '</span>';
        }
        html += '</div>';
      });

      html += '</div>';
      return html;
    }

export function renderSystemNotice(m, idx) {
      const type = m.noticeType || 'info';
      const icons = {
        success: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zm-3.97-3.03a.75.75 0 0 0-1.08.022L7.477 9.417 5.384 7.323a.75.75 0 0 0-1.06 1.06L6.97 11.03a.75.75 0 0 0 1.079-.02l3.992-4.99a.75.75 0 0 0-.01-1.05z"/></svg>',
        error: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zM5.354 4.646a.5.5 0 1 0-.708.708L7.293 8l-2.647 2.646a.5.5 0 0 0 .708.708L8 8.707l2.646 2.647a.5.5 0 0 0 .708-.708L8.707 8l2.647-2.646a.5.5 0 0 0-.708-.708L8 7.293 5.354 4.646z"/></svg>',
        warning: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8.982 1.566a1.13 1.13 0 0 0-1.96 0L.165 13.233c-.457.778.091 1.767.98 1.767h13.713c.889 0 1.438-.99.98-1.767L8.982 1.566zM8 5c.535 0 .954.462.9.995l-.35 3.507a.552.552 0 0 1-1.1 0L7.1 5.995A.905.905 0 0 1 8 5zm.002 6a1 1 0 1 1 0 2 1 1 0 0 1 0-2z"/></svg>',
        info: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16zm.93-9.412-1 4.705c-.07.34.029.533.304.533.194 0 .487-.07.686-.246l-.088.416c-.287.346-.92.598-1.465.598-.703 0-1.002-.422-.808-1.319l.738-3.468c.064-.293.006-.399-.287-.47l-.451-.081.082-.381 2.29-.287zM8 5.5a1 1 0 1 1 0-2 1 1 0 0 1 0 2z"/></svg>'
      };
      // 系统通知保持轻量级样式，不使用完整卡片
      let html = '<div class="system-notice ' + type + '" data-msg-idx="' + idx + '">';
      html += '<span class="system-notice-icon">' + (icons[type] || icons.info) + '</span>';
      html += '<span class="system-notice-text">' + escapeHtml(m.content || '') + '</span>';
      html += '<span class="system-notice-time">' + (m.time || '') + '</span>';
      html += '</div>';
      return html;
    }

export function renderErrorMessage(m, idx) {
      const agent = m.agent || 'system';
      const contentHtml = '<div class="error-content">' + escapeHtml(m.content || '未知错误') + '</div>' +
        '<div class="error-hint">查看模型输出 Tab 获取详细信息</div>';

      return renderUnifiedCard({
        type: 'error',
        variant: 'error',
        icon: getRoleIcon('error'),
        title: '执行失败',
        badges: [{ text: agent.toUpperCase(), class: 'badge-' + agent.toLowerCase() }],
        time: m.time || '',
        content: contentHtml,
        dataAttrs: { 'msg-idx': idx }
      });
    }


// ============================================
// 计划和问题
// ============================================

export function renderPlanPreviewCard(m, idx) {
      const reviewStatus = m.review?.status || 'approved';
      const statusText = reviewStatus === 'rejected' ? '需修订' : '待执行';
      const badgeClass = reviewStatus === 'rejected' ? 'badge-error' : 'badge-info';
      const footerHtml = reviewStatus === 'rejected'
        ? ''
        : '<button class="plan-confirm-btn confirm plan-start-btn" data-plan-id="' + escapeHtml(m.planId || '') + '">' +
          '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M5.5 3.5a.5.5 0 0 1 .8-.4l5 4a.5.5 0 0 1 0 .8l-5 4a.5.5 0 0 1-.8-.4v-8z"/></svg>' +
          '开始执行</button>';

      const reviewHtml = m.review?.summary
        ? '<div class="plan-review"><strong>评审结论</strong><br>' + escapeHtml(m.review.summary) + '</div>'
        : '';

      return renderUnifiedCard({
        type: 'plan',
        variant: 'info',
        icon: getRoleIcon('plan'),
        title: '执行计划',  // 恢复标题，但不设置 collapsed，所以标题无点击事件
        badges: [{ text: statusText, class: badgeClass }],
        time: m.time || '',
        content: '<div class="plan-content">' + formatPlanHtml(m.content || '') + '</div>' + reviewHtml,
        footer: footerHtml,
        collapsed: false,  // 🔧 不可折叠，标题无点击事件
        className: 'plan-confirmation-card ' + (reviewStatus === 'rejected' ? 'cancelled' : 'ready'),
        dataAttrs: { 'msg-idx': idx }
      });
    }

export function renderPlanConfirmationCard(m, idx) {
      const isPending = m.isPending;
      const statusClass = isPending ? 'pending' : (m.confirmed ? 'confirmed' : 'cancelled');
      const statusText = isPending ? '等待确认' : (m.confirmed ? '已确认' : '已取消');
      const badgeClass = isPending ? 'badge-warning' : (m.confirmed ? 'badge-success' : 'badge-error');

      let footerHtml = '';
      if (isPending) {
        footerHtml = '<button class="plan-confirm-btn cancel" data-action="cancel">' +
          '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/></svg>' +
          '取消</button>' +
          '<button class="plan-confirm-btn confirm" data-action="confirm">' +
          '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M13.854 3.646a.5.5 0 0 1 0 .708l-7 7a.5.5 0 0 1-.708 0l-3.5-3.5a.5.5 0 1 1 .708-.708L6.5 10.293l6.646-6.647a.5.5 0 0 1 .708 0z"/></svg>' +
          '确认执行</button>';
      }

      return renderUnifiedCard({
        type: 'plan',
        variant: isPending ? 'warning' : (m.confirmed ? 'success' : 'error'),
        icon: getRoleIcon('plan'),
        title: '执行计划确认',  // 恢复标题，但不设置 collapsed，所以标题无点击事件
        badges: [{ text: statusText, class: badgeClass }],
        time: m.time || '',
        content: '<div class="plan-content">' + formatPlanHtml(m.content || '') + '</div>',
        footer: footerHtml,
        collapsed: false,  // 🔧 不可折叠，标题无点击事件
        className: 'plan-confirmation-card ' + statusClass,
        dataAttrs: { 'msg-idx': idx }
      });
    }

export function renderQuestionCard(m, idx) {
      const isPending = m.isPending;
      const answered = !!m.answered;
      const statusClass = isPending ? 'pending' : (answered ? 'answered' : 'cancelled');
      const statusText = isPending ? '等待回答' : (answered ? '已回答' : '已取消');
      const badgeClass = isPending ? 'badge-warning' : (answered ? 'badge-success' : 'badge-error');
      const questions = Array.isArray(m.questions) ? m.questions : [];

      // 构建内容
      let contentHtml = '<ol class="question-list">';
      questions.forEach(q => {
        contentHtml += '<li>' + escapeHtml(String(q)) + '</li>';
      });
      contentHtml += '</ol>';
      if (isPending) {
        contentHtml += '<div class="question-hint">请在下方输入框回答问题，然后点击发送。</div>';
      } else if (answered && m.answer) {
        contentHtml += '<div class="question-answer-display">' + escapeHtml(m.answer) + '</div>';
      }

      // 构建底部
      let footerHtml = '';
      if (isPending) {
        footerHtml = '<button class="question-btn cancel" data-action="cancel">取消</button>';
      }

      return renderUnifiedCard({
        type: 'question',
        variant: isPending ? 'warning' : (answered ? 'success' : 'error'),
        icon: getRoleIcon('question'),
        title: '需要补充信息',  // 恢复标题，但不设置 collapsed，所以标题无点击事件
        badges: [{ text: statusText, class: badgeClass }],
        time: m.time || '',
        content: contentHtml,
        footer: footerHtml,
        collapsed: false,  // 🔧 不可折叠，标题无点击事件
        className: 'question-card ' + statusClass,
        dataAttrs: { 'msg-idx': idx }
      });
    }

export function renderWorkerQuestionCard(m, idx) {
      const isPending = m.isPending;
      const answered = !!m.answered;
      const timedOut = !!m.timedOut;
      const hasError = !!m.error;

      let statusClass, statusText, badgeClass;
      if (isPending) {
        statusClass = 'pending';
        statusText = '等待回答';
        badgeClass = 'badge-warning';
      } else if (timedOut) {
        statusClass = 'timeout';
        statusText = '已超时';
        badgeClass = 'badge-error';
      } else if (hasError) {
        statusClass = 'error';
        statusText = '发送失败';
        badgeClass = 'badge-error';
      } else if (answered) {
        statusClass = 'answered';
        statusText = '已回答';
        badgeClass = 'badge-success';
      } else {
        statusClass = 'cancelled';
        statusText = '已取消';
        badgeClass = 'badge-error';
      }

      // 🔧 智能解析内容
      const parsed = parseWorkerQuestionContent(m.content);
      let contentHtml = '<div class="agent-question-content">';

      if (parsed.type === 'tool_calls') {
        // 工具调用：显示友好的工具信息
        parsed.tools.forEach((tool, i) => {
          contentHtml += '<div class="agent-tool-call" style="margin-bottom: 8px;">';
          contentHtml += '<div style="font-weight: 500; color: var(--vscode-textLink-foreground);">';
          contentHtml += '<span style="opacity: 0.7;">工具: </span>' + escapeHtml(tool.name);
          if (tool.model) {
            contentHtml += ' <span style="opacity: 0.6; font-size: 0.9em;">(' + escapeHtml(tool.model) + ')</span>';
          }
          contentHtml += '</div>';
          if (tool.description) {
            contentHtml += '<div style="margin-top: 4px; color: var(--vscode-foreground); opacity: 0.9;">';
            contentHtml += escapeHtml(tool.description.slice(0, 300));
            if (tool.description.length > 300) contentHtml += '...';
            contentHtml += '</div>';
          }
          contentHtml += '</div>';
        });
      } else if (parsed.type === 'structured') {
        // 结构化数据：显示描述
        contentHtml += '<div class="agent-question-text" style="white-space: pre-wrap;">';
        contentHtml += escapeHtml(parsed.description || '');
        contentHtml += '</div>';
      } else {
        // 普通文本或权限请求
        contentHtml += '<pre class="agent-question-text">' + escapeHtml(parsed.display || '') + '</pre>';
      }

      contentHtml += '</div>';

      if (!isPending && m.answer) {
        contentHtml += '<div class="agent-question-answer">';
        contentHtml += '<strong>回答:</strong> ' + escapeHtml(m.answer);
        contentHtml += '</div>';
      }

      if (hasError) {
        contentHtml += '<div class="agent-question-error">';
        contentHtml += '<strong>错误:</strong> ' + escapeHtml(m.error);
        contentHtml += '</div>';
      }

      // 构建底部（快捷回答按钮）
      let footerHtml = '';
      if (isPending) {
        footerHtml = '<div class="question-hint">请在底部输入框中回复该问题后发送。</div>';
      }

      return renderUnifiedCard({
        type: 'worker_question',
        variant: isPending ? 'warning' : (answered ? 'success' : 'error'),
        icon: '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14zm0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16z"/><path d="M5.255 5.786a.237.237 0 0 0 .241.247h.825c.138 0 .248-.113.266-.25.09-.656.54-1.134 1.342-1.134.686 0 1.314.343 1.314 1.168 0 .635-.374.927-.965 1.371-.673.489-1.206 1.06-1.168 1.987l.003.217a.25.25 0 0 0 .25.246h.811a.25.25 0 0 0 .25-.25v-.105c0-.718.273-.927 1.01-1.486.609-.463 1.244-.977 1.244-2.056 0-1.511-1.276-2.241-2.673-2.241-1.267 0-2.655.59-2.75 2.286zm1.557 5.763c0 .533.425.927 1.01.927.609 0 1.028-.394 1.028-.927 0-.552-.42-.94-1.029-.94-.584 0-1.009.388-1.009.94z"/></svg>',
        title: 'Worker 询问',  // 恢复标题，但不设置 collapsed，所以标题无点击事件
        badges: [{ text: statusText, class: badgeClass }],
        time: m.time || '',
        content: contentHtml,
        footer: footerHtml,
        collapsed: false,  // 🔧 不可折叠，标题无点击事件
        className: 'agent-question-card ' + statusClass,
        dataAttrs: { 'msg-idx': idx, 'question-id': m.questionId || '' }
      });
    }


// ============================================
// 视图
// ============================================

export function renderTasksView() {
      const container = document.getElementById('tasks-content');
      if (!container) return;

      // 获取所有任务，过滤掉没有子任务的任务（编排者直接回复的不需要显示）
      // 只有需要 Worker 协助的任务才显示在任务面板
      const allTasks = state?.tasks || [];
      const tasks = allTasks
        .filter(t => t.subTasks && t.subTasks.length > 0)  // 只显示有子任务的
        .slice()
        .reverse();  // 按时间倒序

      if (tasks.length === 0) {
        container.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M2 2a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v13.5a.5.5 0 0 1-.777.416L8 13.101l-5.223 2.815A.5.5 0 0 1 2 15.5V2zm2-1a1 1 0 0 0-1 1v12.566l4.723-2.482a.5.5 0 0 1 .554 0L13 14.566V2a1 1 0 0 0-1-1H4z"/></svg></div><span class="empty-state-text">暂无任务</span><span class="empty-state-hint">需要 Worker 协助的任务将在此显示</span></div>';
        return;
      }

      // 任务列表头部（带清理按钮）
      let html = '<div class="tasks-header">';
      html += '<span class="tasks-count">' + tasks.length + ' 个任务</span>';
      html += '<button class="btn-small btn-secondary" onclick="clearAllTasks()" title="清理所有任务">清理</button>';
      html += '</div>';
      html += '<div class="task-list">';

      // 渲染每个任务
      tasks.forEach((task, taskIndex) => {
        const isRunning = task.status === 'running';
        const isExpanded = isRunning || taskIndex === 0; // 运行中或最新的任务默认展开
        const statusMap = { pending: '等待中', running: '执行中', paused: '已暂停', retrying: '重试中', completed: '已完成', failed: '失败', cancelled: '已取消' };
        const statusClass = task.status || 'pending';
        const subTasks = task.subTasks || [];
        const completedCount = subTasks.filter(st => st.status === 'completed').length;
        const taskTime = task.createdAt ? new Date(task.createdAt).toLocaleTimeString().slice(0, 5) : '';

        html += '<div class="task-group ' + (isExpanded ? 'expanded' : 'collapsed') + '" data-task-id="' + task.id + '">';

        // 任务头部（可点击展开/折叠）
        html += '<div class="task-group-header" onclick="toggleTaskGroup(\'' + task.id + '\')">';
        html += '<div class="task-group-toggle"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/></svg></div>';
        html += '<div class="task-status ' + statusClass + '"></div>';
        html += '<div class="task-group-info">';
        html += '<span class="task-group-title">' + escapeHtml((task.prompt || '任务').slice(0, 50)) + (task.prompt && task.prompt.length > 50 ? '...' : '') + '</span>';
        html += '<span class="task-group-meta">' + (statusMap[task.status] || task.status);
        if (subTasks.length > 0) html += ' · ' + completedCount + '/' + subTasks.length + ' 子任务';
        if (taskTime) html += ' · ' + taskTime;
        html += '</span></div>';
        if (isRunning) {
          html += '<button class="btn-icon btn-danger" onclick="event.stopPropagation();interruptTask(\'' + task.id + '\')" title="中断"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M5 3.5h6A1.5 1.5 0 0 1 12.5 5v6a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 11V5A1.5 1.5 0 0 1 5 3.5z"/></svg></button>';
        }
        html += '</div>';

        // 子任务列表（可折叠）
        if (subTasks.length > 0) {
          html += '<div class="task-group-content">';
          subTasks.forEach((st, index) => {
            const stStatusCls = st.status === 'running' ? 'running' : st.status === 'completed' ? 'completed' : st.status === 'failed' ? 'failed' : st.status === 'skipped' ? 'skipped' : st.status === 'paused' ? 'paused' : st.status === 'retrying' ? 'retrying' : 'pending';
            const workerName = (st.assignedWorker || st.assignedCli || 'auto').toLowerCase();
            const description = st.description || st.title || '子任务 ' + (index + 1);
            const agentClass = workerName.includes('claude') ? 'claude' : workerName.includes('codex') ? 'codex' : workerName.includes('gemini') ? 'gemini' : '';

            html += '<div class="task-item" data-subtask-id="' + st.id + '">';
            html += '<div class="task-status ' + stStatusCls + '"></div>';
            html += '<div class="task-info">';
            html += '<span class="task-name">' + escapeHtml(description.slice(0, 50)) + (description.length > 50 ? '...' : '') + '</span>';
            html += '<div class="task-meta">';
            html += '<span class="task-worker ' + agentClass + '">' + workerName + '</span>';
            const fileCount = (st.modifiedFiles && st.modifiedFiles.length > 0)
              ? st.modifiedFiles.length
              : (st.targetFiles ? st.targetFiles.length : 0);
            if (fileCount > 0) {
              html += '<span class="task-files">' + fileCount + '文件</span>';
            }
            html += '</div></div></div>';
          });
          html += '</div>';
        }

        html += '</div>';
      });

      html += '</div>';
      container.innerHTML = html;
    }

export function renderEditsView() {
      const container = document.getElementById('edits-content');
      if (!container) return;
      if (pendingChanges.length === 0) {
        container.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10z"/></svg></div><span class="empty-state-text">暂无待处理变更</span><span class="empty-state-hint">模型修改文件后将在此显示</span></div>';
        return;
      }
      let html = '<div class="edits-header"><span class="edits-count">' + pendingChanges.length + ' 个变更</span>';
      html += '<div class="edits-batch-actions"><button class="btn-small btn-success" onclick="approveAllChanges()">全部批准</button>';
      html += '<button class="btn-small btn-danger" onclick="revertAllChanges()">全部还原</button></div></div>';
      html += '<div class="edits-list">';
      pendingChanges.forEach(edit => {
        const fileName = edit.filePath.split('/').pop();
        const dirPath = edit.filePath.split('/').slice(0, -1).join('/');
        const agentName = (edit.lastModifiedBy || '').toLowerCase();
        const agentClass = agentName.includes('claude') ? 'claude' : agentName.includes('codex') ? 'codex' : agentName.includes('gemini') ? 'gemini' : '';

        html += '<div class="edit-item" data-file="' + escapeHtml(edit.filePath) + '" onclick="viewDiff(\'' + escapeHtml(edit.filePath) + '\')">';
        html += '<svg class="edit-file-icon" viewBox="0 0 16 16" fill="currentColor"><path d="M4 0a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V4.414A2 2 0 0 0 13.414 3L11 .586A2 2 0 0 0 9.586 0H4z"/></svg>';
        html += '<div class="edit-info">';
        html += '<div class="edit-file-path"><span class="edit-file-name">' + escapeHtml(fileName) + '</span>';
        if (dirPath) html += '<span class="edit-file-dir">' + escapeHtml(dirPath) + '</span>';
        html += '</div></div>';
        html += '<div class="edit-stats">';
        html += '<span class="edit-stat-add">+' + (edit.additions || 0) + '</span>';
        html += '<span class="edit-stat-del">-' + (edit.deletions || 0) + '</span>';
        if (agentName) html += '<span class="badge badge--xs badge--' + agentClass + '">' + agentName + '</span>';
        html += '</div>';
        html += '<div class="edit-actions" onclick="event.stopPropagation()">';
        html += '<button class="btn-icon" onclick="approveChange(\'' + escapeHtml(edit.filePath) + '\')" title="批准"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M13.854 3.646a.5.5 0 0 1 0 .708l-7 7a.5.5 0 0 1-.708 0l-3.5-3.5a.5.5 0 1 1 .708-.708L6.5 10.293l6.646-6.647a.5.5 0 0 1 .708 0z"/></svg></button>';
        html += '<button class="btn-icon" onclick="revertChange(\'' + escapeHtml(edit.filePath) + '\')" title="还原"><svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M8 3a5 5 0 1 1-4.546 2.914.5.5 0 0 0-.908-.417A6 6 0 1 0 8 2v1z"/><path d="M8 4.466V.534a.25.25 0 0 0-.41-.192L5.23 2.308a.25.25 0 0 0 0 .384l2.36 1.966A.25.25 0 0 0 8 4.466z"/></svg></button>';
        html += '</div></div>';
      });
      html += '</div>';
      container.innerHTML = html;
    }


// ============================================
// 设置和配置
// ============================================

export function renderProfileTags(containerId, tags, type) {
      const container = document.getElementById(containerId);
      container.innerHTML = tags.map((tag, idx) => `
        <span class="profile-tag">
          ${tag}
          <button class="profile-tag-remove" data-type="${type}" data-index="${idx}">×</button>
        </span>
      `).join('');
    }

export function renderMCPServerList() {
      const listEl = document.getElementById('mcp-server-list');
      if (!listEl) return;

      if (mcpServers.length === 0) {
        listEl.innerHTML = `
          <div class="empty-state">
            <svg width="48" height="48" viewBox="0 0 16 16" fill="currentColor" opacity="0.3">
              <path d="M2 2.5a.5.5 0 0 1 .5-.5h3a.5.5 0 0 1 0 1H3v2.5a.5.5 0 0 1-1 0v-3zm12 0a.5.5 0 0 0-.5-.5h-3a.5.5 0 0 0 0 1H13v2.5a.5.5 0 0 0 1 0v-3zm-12 9a.5.5 0 0 0 .5.5h3a.5.5 0 0 0 0-1H3v-2.5a.5.5 0 0 0-1 0v3zm12 0a.5.5 0 0 1-.5.5h-3a.5.5 0 0 1 0-1H13v-2.5a.5.5 0 0 1 1 0v3z"/>
              <path d="M8 3a5 5 0 1 0 0 10A5 5 0 0 0 8 3zM4 8a4 4 0 1 1 8 0 4 4 0 0 1-8 0z"/>
            </svg>
            <p>暂无 MCP 服务器</p>
            <p style="font-size: 11px; opacity: 0.6;">点击"添加服务器"开始配置</p>
          </div>
        `;
        return;
      }

      listEl.innerHTML = mcpServers.map(server => `
        <div class="mcp-server-item" data-server-id="${server.id}">
          <div class="mcp-server-header" data-server-id="${server.id}">
            <div class="mcp-server-info">
              <div class="mcp-server-name">${server.name}</div>
              <div class="mcp-server-command">${server.command || ''}</div>
            </div>
            <div class="mcp-server-actions">
              <button class="mcp-action-btn" data-action="refresh" data-server-id="${server.id}" title="刷新工具列表">
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M11.534 7h3.932a.25.25 0 0 1 .192.41l-1.966 2.36a.25.25 0 0 1-.384 0l-1.966-2.36a.25.25 0 0 1 .192-.41zm-11 2h3.932a.25.25 0 0 0 .192-.41L2.692 6.23a.25.25 0 0 0-.384 0L.342 8.59A.25.25 0 0 0 .534 9z"/>
                  <path d="M8 3c-1.552 0-2.94.707-3.857 1.818a.5.5 0 1 1-.771-.636A6.002 6.002 0 0 1 13.917 7H12.9A5.002 5.002 0 0 0 8 3zM3.1 9a5.002 5.002 0 0 0 8.757 2.182.5.5 0 1 1 .771.636A6.002 6.002 0 0 1 2.083 9H3.1z"/>
                </svg>
              </button>
              <button class="mcp-action-btn" data-action="edit" data-server-id="${server.id}" title="编辑服务器">
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10zM11.207 2.5 13.5 4.793 14.793 3.5 12.5 1.207 11.207 2.5zm1.586 3L10.5 3.207 4 9.707V10h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.293l6.5-6.5zm-9.761 5.175-.106.106-1.528 3.821 3.821-1.528.106-.106A.5.5 0 0 1 5 12.5V12h-.5a.5.5 0 0 1-.5-.5V11h-.5a.5.5 0 0 1-.468-.325z"/>
                </svg>
              </button>
              <button class="mcp-action-btn danger" data-action="delete" data-server-id="${server.id}" title="删除服务器">
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M5.5 5.5A.5.5 0 0 1 6 6v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm2.5 0a.5.5 0 0 1 .5.5v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm3 .5a.5.5 0 0 0-1 0v6a.5.5 0 0 0 1 0V6z"/>
                  <path d="M14.5 3a1 1 0 0 1-1 1H13v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4h-.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1H6a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1h3.5a1 1 0 0 1 1 1v1zM4.118 4 4 4.059V13a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V4.059L11.882 4H4.118zM2.5 3V2h11v1h-11z"/>
                </svg>
              </button>
            </div>
          </div>
          <div class="mcp-server-tools" id="mcp-tools-${server.id}" style="display:none;"></div>
        </div>
      `).join('');

      // 绑定事件
      document.querySelectorAll('.mcp-server-header').forEach(header => {
        header.addEventListener('click', () => {
          const serverId = header.dataset.serverId;
          toggleMCPTools(serverId);
        });
      });

      document.querySelectorAll('.mcp-action-btn').forEach(btn => {
        btn.addEventListener('click', (e) => {
          e.stopPropagation();
          const action = btn.dataset.action;
          const serverId = btn.dataset.serverId;
          handleMCPAction(action, serverId);
        });
      });
    }

export function handleMCPAction(action, serverId) {
      switch (action) {
        case 'refresh':
          expandMCPTools(serverId, true);
          break;
        case 'edit':
          editMCPServer(serverId);
          break;
        case 'delete':
          if (confirm('确定要删除此 MCP 服务器吗？')) {
            postMessage({ type: 'deleteMCPServer', serverId });
          }
          break;
      }
    }

export function toggleMCPTools(serverId) {
      const toolsEl = document.getElementById(`mcp-tools-${serverId}`);
      if (!toolsEl) return;

      if (toolsEl.style.display === 'none') {
        expandMCPTools(serverId, true);
      } else {
        toolsEl.style.display = 'none';
      }
    }

export function expandMCPTools(serverId, forceRefresh) {
      const toolsEl = document.getElementById(`mcp-tools-${serverId}`);
      if (!toolsEl) return;

      toolsEl.style.display = 'block';
      toolsEl.innerHTML = '<div class="mcp-tools-empty">加载中...</div>';

      if (forceRefresh) {
        postMessage({ type: 'refreshMCPTools', serverId });
      } else {
        postMessage({ type: 'getMCPServerTools', serverId });
      }
    }

export function editMCPServer(serverId) {
      const server = mcpServers.find(s => s.id === serverId);
      if (!server) return;
      currentEditingMCPServer = server;
      showMCPDialog(server);
    }

export function showMCPDialog(server = null) {
      const isEdit = server !== null;
      const title = isEdit ? '编辑 MCP 服务器' : '添加 MCP 服务器';
      const defaultJSON = `{
  "mcpServers": {
    "${server?.name || "mcp-server"}": {
      "command": "${server?.command || "npx"}",
      "args": ${server?.args ? JSON.stringify(server.args, null, 2) : `[
        "@modelcontextprotocol/server-filesystem",
        "/path/to/allowed/files"
      ]`},
      "env": ${server?.env ? JSON.stringify(server.env, null, 2) : `{}`}
    }
  }
}`;

      const dialogHTML = `
        <div class="modal-overlay" id="mcp-dialog-overlay">
          <div class="modal-dialog">
            <div class="modal-header">
              <h3>${title}</h3>
              <button class="modal-close" id="mcp-dialog-close">×</button>
            </div>
            <div class="modal-body">
              <div class="form-field">
                <label>MCP 服务器 JSON</label>
                <textarea id="mcp-json" rows="12" placeholder="粘贴 MCP JSON 配置">${defaultJSON}</textarea>
                <div class="form-help" style="font-size: 11px; opacity: 0.7; margin-top: 6px;">
                  支持格式：{ "mcpServers": { "name": { "command": "...", "args": [...], "env": {...} } } }
                </div>
              </div>
            </div>
            <div class="modal-footer">
              <button class="settings-btn" id="mcp-dialog-cancel">取消</button>
              <button class="settings-btn primary" id="mcp-dialog-save">保存</button>
            </div>
          </div>
        </div>
      `;

      const oldDialog = document.getElementById('mcp-dialog-overlay');
      if (oldDialog) oldDialog.remove();

      document.body.insertAdjacentHTML('beforeend', dialogHTML);

      document.getElementById('mcp-dialog-close')?.addEventListener('click', closeMCPDialog);
      document.getElementById('mcp-dialog-cancel')?.addEventListener('click', closeMCPDialog);
      document.getElementById('mcp-dialog-save')?.addEventListener('click', () => saveMCPServer(isEdit));
      document.getElementById('mcp-dialog-overlay')?.addEventListener('click', (e) => {
        if (e.target.id === 'mcp-dialog-overlay') closeMCPDialog();
      });
    }

export function closeMCPDialog() {
      const dialog = document.getElementById('mcp-dialog-overlay');
      if (dialog) dialog.remove();
      currentEditingMCPServer = null;
    }

export function saveMCPServer(isEdit) {
      const jsonText = document.getElementById('mcp-json')?.value.trim();
      if (!jsonText) {
        alert('请输入 MCP JSON 配置');
        return;
      }

      let parsed;
      try {
        parsed = JSON.parse(jsonText);
      } catch (error) {
        alert('JSON 格式错误：' + error.message);
        return;
      }

      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        alert('JSON 必须是对象');
        return;
      }

      const servers = parsed.mcpServers;
      if (!servers || typeof servers !== 'object' || Array.isArray(servers)) {
        alert('缺少 mcpServers 对象');
        return;
      }

      const serverNames = Object.keys(servers);
      if (serverNames.length === 0) {
        alert('mcpServers 不能为空');
        return;
      }

      if (serverNames.length > 1 && isEdit) {
        alert('编辑模式仅支持一个服务器');
        return;
      }

      const saveServer = (name, cfg, isUpdate) => {
        if (!cfg || typeof cfg !== 'object') {
          alert(`服务器 ${name} 配置无效`);
          return false;
        }
        const command = String(cfg.command || '').trim();
        if (!command) {
          alert(`服务器 ${name} 缺少 command`);
          return false;
        }

        const args = cfg.args ?? [];
        if (!Array.isArray(args)) {
          alert(`服务器 ${name} 的 args 必须是数组`);
          return false;
        }

        const env = cfg.env ?? {};
        if (typeof env !== 'object' || Array.isArray(env)) {
          alert(`服务器 ${name} 的 env 必须是对象`);
          return false;
        }

        const serverData = {
          name,
          command,
          args,
          env,
          enabled: cfg.enabled !== false,
          type: 'stdio'
        };

        if (isUpdate && currentEditingMCPServer) {
          serverData.id = currentEditingMCPServer.id;
          postMessage({
            type: 'updateMCPServer',
            serverId: currentEditingMCPServer.id,
            updates: serverData
          });
        } else {
          postMessage({
            type: 'addMCPServer',
            server: serverData
          });
        }

        return true;
      };

      let savedCount = 0;
      if (isEdit && currentEditingMCPServer) {
        const name = serverNames[0];
        if (saveServer(name, servers[name], true)) savedCount += 1;
      } else {
        serverNames.forEach((name) => {
          if (saveServer(name, servers[name], false)) savedCount += 1;
        });
      }

      if (savedCount > 0) {
        postMessage({ type: 'loadMCPServers' });
        closeMCPDialog();
      }
    }

export function renderMCPTools(serverId, tools) {
      const toolsEl = document.getElementById(`mcp-tools-${serverId}`);
      if (!toolsEl) return;

      if (tools.length === 0) {
        toolsEl.innerHTML = '<div class="mcp-tools-empty">暂无工具</div>';
        return;
      }

      toolsEl.innerHTML = `
        <div class="mcp-tools-header">
          <span>工具列表 (${tools.length})</span>
        </div>
        <div class="mcp-tools-list">
          ${tools.map(tool => `
            <div class="mcp-tool-item">
              <div class="mcp-tool-row">
                <div class="mcp-tool-name" title="${tool.name}">${tool.name}</div>
                <button class="mcp-tool-desc-btn" type="button" title="查看描述" data-tool-desc="${(tool.description || '无描述').replace(/"/g, '&quot;')}">
                  <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor">
                    <path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm-.5-6.5h1v4h-1v-4zm0-3h1v1h-1v-1z"/>
                  </svg>
                </button>
              </div>
              <div class="mcp-tool-desc-pop">${tool.description || '无描述'}</div>
            </div>
          `).join('')}
        </div>
      `;

      toolsEl.querySelectorAll('.mcp-tool-desc-btn').forEach((btn) => {
        btn.addEventListener('click', (e) => {
          e.stopPropagation();
          const item = btn.closest('.mcp-tool-item');
          if (!item) return;
          toolsEl.querySelectorAll('.mcp-tool-item.show-desc').forEach((openItem) => {
            if (openItem !== item) openItem.classList.remove('show-desc');
          });
          item.classList.toggle('show-desc');
        });
      });

      toolsEl.addEventListener('click', (e) => {
        const target = e.target;
        if (target && target.closest && target.closest('.mcp-tool-desc-pop')) {
          return;
        }
        toolsEl.querySelectorAll('.mcp-tool-item.show-desc').forEach((openItem) => {
          openItem.classList.remove('show-desc');
        });
      });
    }

export function renderRepositoryManagementList() {
      const listEl = document.getElementById('repo-manage-list');
      if (!listEl) return;

      if (!repositories || repositories.length === 0) {
        listEl.innerHTML = `
          <div class="empty-state">
            <svg width="48" height="48" viewBox="0 0 16 16" fill="currentColor">
              <path d="M2.5 3.5a.5.5 0 0 1 0-1h11a.5.5 0 0 1 0 1h-11zm0 3a.5.5 0 0 1 0-1h11a.5.5 0 0 1 0 1h-11zm0 3a.5.5 0 0 1 0-1h11a.5.5 0 0 1 0 1h-11zm0 3a.5.5 0 0 1 0-1h11a.5.5 0 0 1 0 1h-11z"/>
            </svg>
            <p>暂无仓库</p>
            <p class="empty-state-hint">点击上方"添加"按钮添加仓库</p>
          </div>
        `;
        return;
      }

      listEl.innerHTML = repositories.map(repo => {
        const repoName = repo.name || (repo.id === 'builtin' ? 'Claude 官方技能' : repo.url);
        const isBuiltin = repo.id === 'builtin';

        return `
          <div class="repo-manage-item">
            <div class="repo-manage-info">
              <div class="repo-manage-header">
                <span class="repo-manage-name">${escapeHtml(repoName)}</span>
                ${isBuiltin ? '<span class="badge badge--xs badge--pill badge--primary">内置</span>' : ''}
              </div>
              <div class="repo-manage-url">${escapeHtml(repo.url)}</div>
            </div>
            <div class="repo-manage-actions">
              <button class="btn-icon btn-icon--sm" id="refresh-btn-${repo.id}" onclick="refreshRepositoryInDialog('${repo.id}')" title="刷新仓库">
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M11.534 7h3.932a.25.25 0 0 1 .192.41l-1.966 2.36a.25.25 0 0 1-.384 0l-1.966-2.36a.25.25 0 0 1 .192-.41zm-11 2h3.932a.25.25 0 0 0 .192-.41L2.692 6.23a.25.25 0 0 0-.384 0L.342 8.59A.25.25 0 0 0 .534 9z"/>
                  <path fill-rule="evenodd" d="M8 3c-1.552 0-2.94.707-3.857 1.818a.5.5 0 1 1-.771-.636A6.002 6.002 0 0 1 13.917 7H12.9A5.002 5.002 0 0 0 8 3zM3.1 9a5.002 5.002 0 0 0 8.757 2.182.5.5 0 1 1 .771.636A6.002 6.002 0 0 1 2.083 9H3.1z"/>
                </svg>
              </button>
              ${!isBuiltin ? `
                <button class="btn-icon btn-icon--sm btn-icon--danger" onclick="deleteRepositoryFromDialog('${repo.id}')" title="删除仓库">
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                    <path d="M5.5 5.5A.5.5 0 0 1 6 6v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm2.5 0a.5.5 0 0 1 .5.5v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm3 .5a.5.5 0 0 0-1 0v6a.5.5 0 0 0 1 0V6z"/>
                    <path fill-rule="evenodd" d="M14.5 3a1 1 0 0 1-1 1H13v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4h-.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1H6a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1h3.5a1 1 0 0 1 1 1v1zM4.118 4 4 4.059V13a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V4.059L11.882 4H4.118zM2.5 3V2h11v1h-11z"/>
                  </svg>
                </button>
              ` : ''}
            </div>
          </div>
        `;
      }).join('');
    }

export function getSkillsConfig() {
      return skillsConfig;
    }

export function renderSkillsToolList() {
      const listEl = document.getElementById('skills-tool-list');
      if (!listEl) return;

      // 检查是否有已安装的 Skills
      if (!skillsConfig || !skillsConfig.builtInTools) {
        listEl.innerHTML = `
          <div class="empty-state">
            <svg width="48" height="48" viewBox="0 0 16 16" fill="currentColor" opacity="0.3">
              <path d="M5 2V0H0v5h2v6H0v5h5v-2h6v2h5v-5h-2V5h2V0h-5v2H5zm6 1v2h2v6h-2v2H5v-2H3V5h2V3h6z"/>
            </svg>
            <p>暂无已安装的 Skill</p>
            <p style="font-size: 11px; opacity: 0.6;">点击"安装 Skill"从库中安装</p>
          </div>
        `;
        return;
      }

      // 获取已启用的 Skills
      const enabledSkills = [];
      for (const [toolName, toolConfig] of Object.entries(skillsConfig.builtInTools)) {
        if (toolConfig.enabled) {
          enabledSkills.push({
            name: toolName,
            description: toolConfig.description || '',
            enabled: true,
            source: 'builtin'
          });
        }
      }
      if (Array.isArray(skillsConfig.customTools)) {
        skillsConfig.customTools.forEach(tool => {
          enabledSkills.push({
            name: tool.name,
            description: tool.description || '',
            enabled: true,
            source: 'custom'
          });
        });
      }
      if (Array.isArray(skillsConfig.instructionSkills)) {
        skillsConfig.instructionSkills.forEach(skill => {
          enabledSkills.push({
            name: skill.name,
            description: skill.description || '',
            enabled: true,
            source: 'instruction'
          });
        });
      }

      // 如果没有启用的 Skills，显示空状态
      if (enabledSkills.length === 0) {
        listEl.innerHTML = `
          <div class="empty-state">
            <svg width="48" height="48" viewBox="0 0 16 16" fill="currentColor" opacity="0.3">
              <path d="M5 2V0H0v5h2v6H0v5h5v-2h6v2h5v-5h-2V5h2V0h-5v2H5zm6 1v2h2v6h-2v2H5v-2H3V5h2V3h6z"/>
            </svg>
            <p>暂无已安装的 Skill</p>
            <p style="font-size: 11px; opacity: 0.6;">点击"安装 Skill"从库中安装</p>
          </div>
        `;
        return;
      }

      // 渲染已安装的 Skills
      let html = '<div class="skills-tool-list">';

      for (const skill of enabledSkills) {
        // 判断是服务器端还是客户端工具
        const isServerSide = skill.name.includes('web_search') || skill.name.includes('web_fetch');
        const isCustom = skill.source === 'custom';
        const isInstruction = skill.source === 'instruction';
        const typeLabel = isInstruction ? 'Instruction' : (isCustom ? 'Custom' : (isServerSide ? 'Server' : 'Client'));
        const typeClass = isInstruction ? 'instruction' : (isCustom ? 'custom' : (isServerSide ? '' : 'client'));
        const iconClass = skill.name.includes('web_search') ? 'web-search' :
                         skill.name.includes('web_fetch') ? 'web-fetch' :
                         skill.name.includes('text_editor') ? 'text-editor' :
                         skill.name.includes('computer_use') ? 'computer-use' :
                         isInstruction ? 'instruction-skill' : 'custom-skill';

        const safeDesc = escapeHtml(skill.description || '');
        const descAttr = safeDesc.replace(/"/g, '&quot;');
        html += `
          <div class="skills-tool-item">
            <div class="skills-tool-icon ${iconClass}">
              <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                <path d="M5 2V0H0v5h2v6H0v5h5v-2h6v2h5v-5h-2V5h2V0h-5v2H5zm6 1v2h2v6h-2v2H5v-2H3V5h2V3h6z"/>
              </svg>
            </div>
            <div class="skills-tool-info">
              <div class="skills-tool-header">
                <span class="skills-tool-name">${escapeHtml(skill.name)}</span>
                <span class="skills-tool-type ${typeClass}">${typeLabel}</span>
              </div>
              <div class="skills-tool-row">
                <div class="skills-tool-desc" title="${descAttr}">${safeDesc || '-'}</div>
                <button class="skills-tool-desc-btn" type="button" title="查看描述" data-skill-desc="${descAttr}">
                  <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor">
                    <path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm-.5-6.5h1v4h-1v-4zm0-3h1v1h-1v-1z"/>
                  </svg>
                </button>
              </div>
            </div>
            <div class="skills-tool-toggle active" title="已启用"></div>
            <div class="skills-tool-desc-pop">${safeDesc || '无描述'}</div>
          </div>
        `;
      }

      html += '</div>';
      listEl.innerHTML = html;

      listEl.querySelectorAll('.skills-tool-desc-btn').forEach((btn) => {
        btn.addEventListener('click', (e) => {
          e.stopPropagation();
          const item = btn.closest('.skills-tool-item');
          if (!item) return;
          listEl.querySelectorAll('.skills-tool-item.show-desc').forEach((openItem) => {
            if (openItem !== item) openItem.classList.remove('show-desc');
          });
          item.classList.toggle('show-desc');
        });
      });

      listEl.addEventListener('click', (e) => {
        const target = e.target;
        if (target && target.closest && target.closest('.skills-tool-desc-pop')) {
          return;
        }
        listEl.querySelectorAll('.skills-tool-item.show-desc').forEach((openItem) => {
          openItem.classList.remove('show-desc');
        });
      });
    }

export function renderSkillLibrary(skills) {
      console.log('[Skill Library] Rendering skills:', skills);
      const listEl = document.getElementById('skill-library-list');
      if (!listEl) {
        console.error('[Skill Library] List element not found');
        return;
      }

      if (!skills || skills.length === 0) {
        console.warn('[Skill Library] No skills to display');
        listEl.innerHTML = `
          <div class="empty-state">
            <svg width="48" height="48" viewBox="0 0 16 16" fill="currentColor">
              <path d="M5 2V0H0v5h2v6H0v5h5v-2h6v2h5v-5h-2V5h2V0h-5v2H5zm6 1v2h2v6h-2v2H5v-2H3V5h2V3h6z"/>
            </svg>
            <p>暂无可用的 Skill</p>
            <p class="empty-state-hint">请先添加 Skill 仓库</p>
          </div>
        `;
        return;
      }

      // 按仓库分组
      const skillsByRepo = {};
      for (const skill of skills) {
        const repoId = skill.repositoryId || 'unknown';
        if (!skillsByRepo[repoId]) {
          skillsByRepo[repoId] = {
            name: skill.repositoryName || '未知仓库',
            skills: []
          };
        }
        skillsByRepo[repoId].skills.push(skill);
      }

      console.log('[Skill Library] Skills grouped by repository:', skillsByRepo);

      // 渲染分组后的 Skills
      let html = '';
      for (const [repoId, repoData] of Object.entries(skillsByRepo)) {
        html += `
          <div class="skill-repo-group">
            <div class="skill-repo-title">${escapeHtml(repoData.name)} (${repoData.skills.length} 个技能)</div>
            ${repoData.skills.map(skill => {
              // 只允许SVG图标，过滤掉emoji
              let iconSvg = '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M5 2V0H0v5h2v6H0v5h5v-2h6v2h5v-5h-2V5h2V0h-5v2H5zm6 1v2h2v6h-2v2H5v-2H3V5h2V3h6z"/></svg>';
              if (skill.icon && skill.icon.trim().startsWith('<svg')) {
                iconSvg = skill.icon;
              }
              const metaParts = [];
              if (skill.author) metaParts.push(`<span class="skill-library-meta-item">作者: ${escapeHtml(skill.author)}</span>`);
              if (skill.version) metaParts.push(`<span class="skill-library-meta-item">版本: ${escapeHtml(skill.version)}</span>`);
              if (skill.category) metaParts.push(`<span class="skill-library-meta-item">分类: ${escapeHtml(skill.category)}</span>`);
              if (skill.skillType) {
                const typeLabel = skill.skillType === 'instruction' ? 'Instruction' : 'Tool';
                metaParts.push(`<span class="skill-library-meta-item">类型: ${escapeHtml(typeLabel)}</span>`);
              }
              const metaHtml = metaParts.length > 0 ? metaParts.join('<span class="skill-library-meta-separator">|</span>') : '';

              return `
                <div class="skill-library-item" data-skill-name="${escapeHtml(skill.name)}" data-skill-desc="${escapeHtml(skill.description || '')}">
                  <div class="skill-library-icon">${iconSvg}</div>
                  <div class="skill-library-info">
                    <div class="skill-library-name">${escapeHtml(skill.name)}</div>
                    <div class="skill-library-desc">${escapeHtml(skill.description || '')}</div>
                    ${metaHtml ? `<div class="skill-library-meta">${metaHtml}</div>` : ''}
                  </div>
                  <div class="skill-library-actions">
                    <button class="settings-btn ${skill.installed ? '' : 'primary'}"
                      onclick="installSkill('${escapeHtml(skill.fullName)}')" ${skill.installed ? 'disabled' : ''}>
                      ${skill.installed ? '已安装' : '安装'}
                    </button>
                  </div>
                </div>
              `;
            }).join('')}
          </div>
        `;
      }

      listEl.innerHTML = html;
      console.log('[Skill Library] Rendered successfully');
    }


// ============================================
// 辅助函数
// ============================================



export function renderSummarySections(sections) {
      if (!sections || sections.length === 0) return '';
      return sections.map(section => {
        const title = section.title ? `<div class="tool-section-title">${escapeHtml(section.title)}</div>` : '';
        const items = Array.isArray(section.items) ? section.items : [];
        const list = items.length > 0
          ? '<ul class="summary-list">' + items.map(item => `<li>${escapeHtml(item)}</li>`).join('') + '</ul>'
          : '';
        return `<div class="tool-section">${title}${list}</div>`;
      }).join('');
    }

export function formatPlanHtml(formattedPlan) {
      // 将 Markdown 格式转换为简单 HTML
      return formattedPlan
        .replace(/## (.*)/g, '<h2>$1</h2>')
        .replace(/### (.*)/g, '<h3>$1</h3>')
        .replace(/\*\*(.*?)\*\*/g, '<strong>$1</strong>')
        .replace(/`([^`]+)`/g, '<code>$1</code>')
        .replace(/\n/g, '<br>')
        .replace(/---/g, '<hr>');
    }


// ============================================
// 辅助函数
// ============================================
// 注意：parseCodeBlockMeta, extractSingleCodeFence, shouldCollapseMessage, toggleMessageExpand
// 这些函数已从 utils.js 导入，不需要在此重复定义

// 收集 Worker 状态条目
export function collectWorkerStatusEntries() {
  const entries = [];
  // TODO: 需要从 state 中获取 tasks
  // 暂时返回空数组，后续集成时修复
  return entries;
}

// 注意：getRoleIcon, getRoleInfo, getMessageGroupKey, formatTime, cleanInternalProtocolData
// 这些函数已从 render-utils.js 导入，不需要在此重复定义
// 旧的 getToolIcon 函数已删除，使用 renderers/tool-call-renderer.js 中的实现

// 解析 Worker 询问内容，提取有意义的信息
export function parseWorkerQuestionContent(content) {
  if (!content || typeof content !== 'string') {
    return { type: 'text', display: content || '' };
  }

  // 先清理内部协议元数据
  const cleanedContent = cleanInternalProtocolData(content);
  const trimmed = cleanedContent.trim();

  // 清理后为空，说明全是内部协议数据
  if (!trimmed) {
    return { type: 'text', display: '等待输入...' };
  }

  // 检测是否是 JSON 格式的工具调用
  if (trimmed.startsWith('{') && trimmed.includes('\"type\"')) {
    try {
      const parsed = JSON.parse(trimmed);

      const internalEventTypes = [
        'stream_event', 'content_block_start', 'content_block_delta',
        'content_block_stop', 'message_start', 'message_delta',
        'message_stop', 'input_json_delta', 'ping'
      ];
      if (internalEventTypes.includes(parsed.type)) {
        return { type: 'text', display: '等待输入...' };
      }

      // Claude 工具调用格式: {"type":"assistant","message":{"content":[...]}}
      if (parsed.type === 'assistant' && parsed.message?.content) {
        const toolCalls = parsed.message.content.filter(c => c.type === 'tool_use');
        if (toolCalls.length > 0) {
          return {
            type: 'tool_calls',
            tools: toolCalls.map(t => ({
              name: t.name || '未知工具',
              description: t.input?.description || t.input?.prompt || '',
              model: t.input?.model,
              input: t.input
            }))
          };
        }
      }

      // tool_result 格式: {"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}
      if (parsed.type === 'user' && parsed.message?.content) {
        const toolResults = parsed.message.content.filter(c => c.type === 'tool_result');
        if (toolResults.length > 0) {
          const resultContent = toolResults.map(r => r.content).join('\n\n');
          const lines = resultContent.split('\n');
          const preview = lines.slice(0, 10).join('\n');
          const truncated = lines.length > 10 ? '\n... (更多内容)' : '';
          return { type: 'text', display: preview + truncated };
        }
      }

      // 结构化内容
      if (parsed.description || parsed.prompt) {
        return {
          type: 'structured',
          description: parsed.description || parsed.prompt,
          data: parsed
        };
      }

      // 尝试提取可读内容
      if (parsed.message?.content) {
        if (typeof parsed.message.content === 'string') {
          return { type: 'text', display: parsed.message.content };
        }
        if (Array.isArray(parsed.message.content)) {
          const textContent = parsed.message.content
            .map(item => {
              if (typeof item === 'string') return item;
              if (item.text) return item.text;
              if (item.content) return typeof item.content === 'string' ? item.content : JSON.stringify(item.content, null, 2);
              return '';
            })
            .filter(Boolean)
            .join('\n\n');
          if (textContent) {
            return { type: 'text', display: textContent };
          }
        }
      }

      // 兜底展示格式化 JSON
      return { type: 'text', display: JSON.stringify(parsed, null, 2) };
    } catch {
      // JSON 解析失败，作为普通文本处理
    }
  }

  // 检测常见权限请求模式
  if (/\[Y\/n\]/i.test(trimmed) || /\[yes\/no\]/i.test(trimmed)) {
    return { type: 'permission', display: trimmed };
  }

  return { type: 'text', display: trimmed };
}

// 全局函数（供 onclick 使用）
window.togglePanel = function(panelId) {
  // 查找包含该 panelId 的元素
  const panel = document.querySelector('[data-panel-id="' + panelId + '"]');
  if (!panel) return;

  // 查找折叠内容和图标
  const collapsibleContent = panel.querySelector('.collapsible-content');
  const collapsibleIcon = panel.querySelector('.c-collapsible-icon, .collapsible-icon');

  if (collapsibleContent) {
    collapsibleContent.classList.toggle('expanded');
  }
  if (collapsibleIcon) {
    collapsibleIcon.classList.toggle('expanded');
  }
};

window.toggleCodeBlock = function(codeId) {
  const block = document.querySelector('[data-code-id="' + codeId + '"]');
  if (!block) return;

  // 新版 c-codeblock 组件
  const truncated = block.querySelector('.c-codeblock__truncated');
  const expandSurface = block.querySelector('.c-codeblock__truncated-surface');
  const collapseBtn = block.querySelector('.c-codeblock__collapse-btn');

  if (truncated) {
    const isExpanded = truncated.classList.toggle('expanded');
    if (expandSurface) {
      expandSurface.style.display = isExpanded ? 'none' : '';
    }
    if (collapseBtn) {
      collapseBtn.classList.toggle('expanded', isExpanded);
    }
  }
};

window.copyCodeBlock = function(codeId) {
  const codeElement = document.getElementById(codeId);
  if (!codeElement) return;
  const text = codeElement.innerText || codeElement.textContent;
  navigator.clipboard.writeText(text).then(() => {
    const btn = event.currentTarget;
    const originalHtml = btn.innerHTML;
    btn.innerHTML = '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M13.854 3.646a.5.5 0 0 1 0 .708l-7 7a.5.5 0 0 1-.708 0l-3.5-3.5a.5.5 0 1 1 .708-.708L6.5 10.293l6.646-6.647a.5.5 0 0 1 .708 0z"/></svg>';
    btn.classList.add('copied');
    setTimeout(() => {
      btn.innerHTML = originalHtml;
      btn.classList.remove('copied');
    }, 1500);
  }).catch(err => {
    console.error('[copyCodeBlock] 复制失败:', err);
  });
};

window.expandCodeBlock = function(codeId) {
  const block = document.querySelector('[data-code-id="' + codeId + '"]');
  if (!block) return;
  const preview = block.querySelector('.markdown-code-preview');
  const expandBar = block.querySelector('.code-expand-bar');
  const full = block.querySelector('.markdown-code-full');
  if (preview) preview.style.display = 'none';
  if (expandBar) expandBar.style.display = 'none';
  if (full) full.style.display = 'flex';
  block.classList.add('expanded');
};

window.openFileInEditor = function(filepath) {
  if (!filepath) return;
  // 通过 vscode-api 发送消息
  if (window.vscode) {
    window.vscode.postMessage({ type: 'openFile', filepath: filepath });
  }
};

// 调度渲染（防抖）
let renderScheduled = false;
export function scheduleRenderMainContent() {
  if (renderScheduled) return;
  renderScheduled = true;
  requestAnimationFrame(() => {
    renderScheduled = false;
    renderMainContent();
  });
}

// 初始化 StreamingManager 的渲染回调
setRenderCallback(scheduleRenderMainContent);

// 导出 streamingManager 供其他模块使用
export { streamingManager };

// ============================================
// 内容块提取函数
// ============================================

/**
 * 从内容块中提取文本
 */
export function extractTextFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'text')
    .map(b => b.content)
    .join('\n');
}

/**
 * 从内容块中提取代码块
 */
export function extractCodeBlocksFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'code')
    .map(b => ({
      language: b.language || 'text',
      content: b.content,
      filename: b.filename,
      isEmbedded: b.isEmbedded,  // 嵌入式代码块标记
      highlightLines: b.highlightLines,  // 高亮行
    }));
}

/**
 * 从内容块中提取思考内容
 */
export function extractThinkingFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'thinking')
    .map(b => ({
      content: b.content,
      summary: b.summary,
    }));
}

/**
 * 从内容块中提取工具调用
 */
export function extractToolCallsFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'tool_call')
    .map(b => ({
      id: b.toolId,
      name: b.toolName,
      status: b.status,
      input: b.input,
      output: b.output,
      error: b.error,
      duration: b.duration,  // 持续时间（毫秒）
    }));
}

// ============================================
// 会话列表渲染
// ============================================

/**
 * 渲染会话列表
 */
export function renderSessionList() {
  const sessionList = document.getElementById('session-list');
  const sessionEmpty = document.getElementById('session-empty');
  const currentSessionName = document.getElementById('current-session-name');

  if (!sessionList || !sessionEmpty) return;

  // 更新当前会话名称
  if (currentSessionName) {
    const currentSession = sessions.find(s => s.id === currentSessionId);
    if (currentSession) {
      currentSessionName.textContent = currentSession.name || '新会话';
    }
  }

  // 如果没有会话，显示空状态
  if (sessions.length === 0) {
    sessionList.style.display = 'none';
    sessionEmpty.style.display = 'flex';
    return;
  }

  sessionList.style.display = 'block';
  sessionEmpty.style.display = 'none';

  // 渲染会话列表项
  sessionList.innerHTML = sessions.map(session => {
    const isActive = session.id === currentSessionId;
    const timeStr = formatRelativeTime(session.updatedAt);
    const msgCount = session.messageCount || 0;

    // 生成会话名称首字母作为头像
    const sessionName = session.name || '未命名会话';
    const initial = sessionName.charAt(0).toUpperCase();

    return `
      <div class="session-item ${isActive ? 'active' : ''}"
           onclick="handleSessionSelect('${session.id}')">
        <div class="session-item-avatar">${initial}</div>
        <div class="session-item-content">
          <div class="session-item-header">
            <span class="session-item-name">${escapeHtml(sessionName)}</span>
            <span class="session-item-time">${timeStr}</span>
          </div>
          <div class="session-item-preview">
            <span class="session-item-count">${msgCount} 条消息</span>
          </div>
        </div>
        <div class="session-item-actions" onclick="event.stopPropagation()">
          <button class="session-action-btn" onclick="handleRenameSession('${session.id}')" title="重命名">
            <svg viewBox="0 0 16 16" width="14" height="14">
              <path fill="currentColor" d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10zM11.207 2.5 13.5 4.793 14.793 3.5 12.5 1.207 11.207 2.5zm1.586 3L10.5 3.207 4 9.707V10h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.293l6.5-6.5zm-9.761 5.175-.106.106-1.528 3.821 3.821-1.528.106-.106A.5.5 0 0 1 5 12.5V12h-.5a.5.5 0 0 1-.5-.5V11h-.5a.5.5 0 0 1-.468-.325z"/>
            </svg>
          </button>
          <button class="session-action-btn session-action-btn--danger" onclick="handleDeleteSession('${session.id}')" title="删除">
            <svg viewBox="0 0 16 16" width="14" height="14">
              <path fill="currentColor" d="M5.5 5.5A.5.5 0 0 1 6 6v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm2.5 0a.5.5 0 0 1 .5.5v6a.5.5 0 0 1-1 0V6a.5.5 0 0 1 .5-.5zm3 .5a.5.5 0 0 0-1 0v6a.5.5 0 0 0 1 0V6z"/>
              <path fill="currentColor" fill-rule="evenodd" d="M14.5 3a1 1 0 0 1-1 1H13v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4h-.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1H6a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1h3.5a1 1 0 0 1 1 1v1zM4.118 4 4 4.059V13a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V4.059L11.882 4H4.118zM2.5 3V2h11v1h-11z"/>
            </svg>
          </button>
        </div>
      </div>
    `;
  }).join('');
}

/**
 * 初始化会话选择器事件
 */
export function initSessionSelector() {
  const sessionSelectorBtn = document.getElementById('session-selector-btn');
  const sessionDropdown = document.getElementById('session-dropdown');
  const newSessionDropdownBtn = document.getElementById('new-session-dropdown-btn');

  if (sessionSelectorBtn && sessionDropdown) {
    // 点击按钮切换下拉菜单
    sessionSelectorBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      const isVisible = sessionDropdown.style.display === 'block';
      sessionDropdown.style.display = isVisible ? 'none' : 'block';

      // 如果打开下拉菜单，渲染会话列表
      if (!isVisible) {
        renderSessionList();
      }
    });

    // 点击页面其他地方关闭下拉菜单
    document.addEventListener('click', () => {
      sessionDropdown.style.display = 'none';
    });

    // 阻止下拉菜单内部点击事件冒泡
    sessionDropdown.addEventListener('click', (e) => {
      e.stopPropagation();
    });
  }

  // 下拉菜单中的新建会话按钮
  if (newSessionDropdownBtn) {
    newSessionDropdownBtn.addEventListener('click', () => {
      postMessage({ type: 'newSession' });
      if (sessionDropdown) {
        sessionDropdown.style.display = 'none';
      }
    });
  }
}

// 将函数挂载到 window 对象供 HTML onclick 使用
window.handleSessionSelect = function(sessionId) {
  if (sessionId === currentSessionId) return;

  postMessage({
    type: 'switchSession',
    sessionId
  });

  // 关闭下拉菜单
  const dropdown = document.getElementById('session-dropdown');
  if (dropdown) {
    dropdown.style.display = 'none';
  }
};

window.handleRenameSession = function(sessionId) {
  const session = sessions.find(s => s.id === sessionId);
  if (!session) return;

  const newName = prompt('请输入新的会话名称:', session.name || '未命名会话');
  if (newName && newName.trim()) {
    postMessage({
      type: 'renameSession',
      sessionId: sessionId,
      name: newName.trim()
    });
  }
};

window.handleDeleteSession = function(sessionId) {
  const session = sessions.find(s => s.id === sessionId);
  if (!session) return;

  if (confirm(`确定要删除会话"${session.name || '未命名会话'}"吗？`)) {
    postMessage({
      type: 'closeSession',
      sessionId: sessionId
    });
  }
};
