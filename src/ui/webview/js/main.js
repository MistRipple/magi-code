// 主入口文件
// 整合所有模块并初始化应用

// ============================================
// 导入核心模块
// ============================================

import {
  vscode,
  threadMessages,
  cliOutputs,
  currentSessionId,
  currentTopTab,
  currentBottomTab,
  isProcessing,
  sessions,
  pendingChanges,
  tasks,
  attachedImages,
  state,
  saveWebviewState,
  restoreWebviewState,
  updateSessions,
  updatePendingChanges,
  updateTasks,
  setCurrentSessionId
} from './core/state.js';

import {
  escapeHtml,
  formatTimestamp,
  formatElapsed,
  formatRelativeTime
} from './core/utils.js';

import {
  postMessage,
  executeTask,
  interruptTask,
  confirmPlan,
  answerQuestions
} from './core/vscode-api.js';

// ============================================
// 导入 UI 模块
// ============================================

import {
  renderMainContent,
  scheduleRenderMainContent,
  renderSessionList,
  initSessionSelector,
  renderImagePreviews,
  renderTasksView,
  renderEditsView
} from './ui/message-renderer.js';

import {
  handleStandardMessage,
  handleStandardUpdate,
  handleStandardComplete,
  handleInteractionMessage,
  updateStreamingMessage,
  handleClarificationAnswer,
  handleWorkerQuestionAnswer,
  handleQuestionAnswer,
  handlePlanConfirmation,
  loadSessionMessages,
  showToast,
  addSystemMessage
} from './ui/message-handler.js';

import {
  initializeEventListeners,
  handleWindowMessage
} from './ui/event-handlers.js';

import {
  updateCliConnectionStatus,
  updateExecutionStats,
  updateProfileConfig,
  initializeSettingsPanel
} from './ui/settings-handler.js';

// ============================================
// 应用初始化
// ============================================

function initializeApp() {
  console.log('[Main] 初始化应用...');

  // 1. 恢复状态
  restoreWebviewState();

  // 2. 初始化事件监听器
  initializeEventListeners();

  // 3. 设置 window.addEventListener('message') 处理
  window.addEventListener('message', (event) => {
    const message = event.data;

    // 根据消息类型分发到对应的处理函数
    switch (message.type) {
      case 'standardMessage':
        handleStandardMessage(message);
        break;

      case 'standardUpdate':
        handleStandardUpdate(message);
        break;

      case 'standardComplete':
        handleStandardComplete(message);
        break;

      case 'interactionMessage':
        handleInteractionMessage(message);
        break;

      case 'stream':
        updateStreamingMessage(message.key, message.content);
        break;

      case 'sessionLoaded':
        // 会话加载完成
        if (message.session) {
          const session = message.session;
          setCurrentSessionId(session.id);
          threadMessages.length = 0;
          threadMessages.push(...(session.messages || []));
          renderMainContent();
          saveWebviewState();
        }
        break;

      case 'sessionsList':
        // 会话列表更新
        if (message.sessions) {
          updateSessions(message.sessions);
          renderSessionList();
        }
        break;

      case 'pendingChanges':
        // 待处理变更更新
        if (message.changes) {
          updatePendingChanges(message.changes);
          renderMainContent();
        }
        break;

      case 'toast':
        // 显示提示消息
        showToast(message.message, message.toastType || 'info', message.duration);
        break;

      case 'error':
        // 显示错误
        showToast(message.message || '发生错误', 'error');
        addSystemMessage(message.message || '发生错误', 'error');
        break;

      case 'cliStatus':
        // CLI 连接状态更新
        updateCliConnectionStatus(message.statuses);
        break;

      case 'executionStats':
        // 执行统计更新
        updateExecutionStats(message.stats, message.orchestratorStats);
        break;

      case 'profileConfig':
        // Profile 配置更新
        updateProfileConfig(message.config);
        break;

      case 'stateUpdate':
        // 状态更新 - 最重要的消息
        if (message.state) {
          const prevSessionId = currentSessionId;

          // 更新 sessions
          if (message.state.sessions) {
            updateSessions(message.state.sessions);
          }

          // 更新 currentSessionId
          if (message.state.currentSessionId) {
            setCurrentSessionId(message.state.currentSessionId);
          }

          // 更新 pendingChanges
          if (message.state.pendingChanges) {
            updatePendingChanges(message.state.pendingChanges);
          }

          // 更新 tasks
          if (message.state.tasks) {
            updateTasks(message.state.tasks);
          }

          // 如果会话切换了，需要加载消息
          const needsSessionLoad = currentSessionId && currentSessionId !== prevSessionId;
          if (needsSessionLoad) {
            loadSessionMessages(currentSessionId);
          }

          renderMainContent();
          renderSessionList();
          renderTasksView();
          renderEditsView();
        }
        break;

      case 'sessionCreated':
        // 新会话创建
        if (message.session) {
          sessions.push(message.session);
          setCurrentSessionId(message.session.id);
          threadMessages.length = 0;
          cliOutputs.claude = [];
          cliOutputs.codex = [];
          cliOutputs.gemini = [];
          saveWebviewState();
          renderMainContent();
          renderSessionList();
          showToast('新会话已创建', 'success');
        }
        break;

      case 'sessionsUpdated':
        // 会话列表更新
        if (message.sessions) {
          updateSessions(message.sessions);
          renderSessionList();
        }
        break;

      case 'sessionSummaryLoaded':
        // 会话总结加载（切换会话时）
        if (message.summary) {
          console.log('[Main] 会话总结已加载:', message.summary);
          // 显示会话总结提示
          const summaryText = `
📋 会话总结: ${message.summary.title}
🎯 目标: ${message.summary.objective}
💬 消息数: ${message.summary.messageCount} 条

${message.summary.completedTasks.length > 0 ? `✅ 已完成任务:\n${message.summary.completedTasks.map((t, i) => `  ${i + 1}. ${t}`).join('\n')}` : ''}
${message.summary.inProgressTasks.length > 0 ? `\n⏳ 进行中任务:\n${message.summary.inProgressTasks.map((t, i) => `  ${i + 1}. ${t}`).join('\n')}` : ''}
${message.summary.codeChanges.length > 0 ? `\n📝 代码变更: ${message.summary.codeChanges.length} 个文件` : ''}
          `.trim();

          addSystemMessage(summaryText, 'info');
          showToast('会话已切换', 'success');
        }
        break;

      case 'executionStatsUpdate':
        // 执行统计更新（注意是 executionStatsUpdate 不是 executionStats）
        updateExecutionStats(message.stats, message.orchestratorStats);
        break;

      case 'cliStatusUpdate':
        // CLI 状态更新
        updateCliConnectionStatus(message.statuses);
        break;

      default:
        console.log('[Main] 未处理的消息类型:', message.type);
    }
  });

  // 4. 初始化会话选择器
  initSessionSelector();

  // 5. 初始渲染
  renderMainContent();
  renderSessionList();

  // 6. 请求初始状态
  postMessage({ type: 'requestState' });

  // 7. 设置定时器更新相对时间
  setInterval(() => {
    // 更新消息时间显示
    const timeSpans = document.querySelectorAll('.message-time[data-timestamp]');
    timeSpans.forEach(span => {
      const timestamp = Number(span.dataset.timestamp || '');
      if (timestamp) {
        span.textContent = formatRelativeTime(timestamp);
      }
    });

    // 更新流式消息的用时显示
    const streamingHints = document.querySelectorAll('.message-streaming-hint[data-start-at], .message-streaming-footer[data-start-at]');
    streamingHints.forEach(hint => {
      const startAt = Number(hint.dataset.startAt || '');
      if (!startAt) return;
      const elapsedText = formatElapsed(Date.now() - startAt);
      const elapsedSpan = hint.querySelector('.thinking-elapsed');
      if (elapsedSpan) {
        elapsedSpan.textContent = `用时 ${elapsedText}`;
      }
    });
  }, 1000);

  console.log('[Main] 应用初始化完成');
}

// ============================================
// 启动应用
// ============================================

// 等待 DOM 加载完成后初始化
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', initializeApp);
} else {
  initializeApp();
}

// ============================================
// 导出供调试使用
// ============================================

window.__DEBUG__ = {
  state,
  threadMessages,
  cliOutputs,
  sessions,
  pendingChanges,
  renderMainContent,
  showToast,
  postMessage
};

console.log('[Main] 主模块加载完成');
