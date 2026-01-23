// 主入口文件
// 整合所有模块并初始化应用

// ============================================
// 导入核心模块
// ============================================

import {
  vscode,
  threadMessages,
  agentOutputs,
  currentSessionId,
  currentTopTab,
  currentBottomTab,
  isProcessing,
  thinkingStartAt,
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
  setCurrentSessionId,
  setAppState,
  setProcessingActor,
  setThinkingStartAt,
  hasLocalProcessingGrace,
  clearLocalProcessingGrace,
  stopStreamingHintTimer
} from './core/state.js';

import {
  escapeHtml,
  formatTimestamp,
  formatElapsed,
  formatRelativeTime
} from './core/utils.js';

import { postMessage } from './core/vscode-api.js';

// 增量更新引擎
import { resetIncrementalState } from './core/incremental-update.js';

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
  renderEditsView,
  updateEditsBadge,
  updateTasksBadge,
  showDependencyAnalysis,
  renderRepositoryManagementList,
  renderMCPServerList,
  renderMCPTools,
  setMcpServers,
  setRepositories,
  setSkillsConfig,
  renderSkillsToolList
} from './ui/message-renderer.js';

import {
  handleStandardMessage,
  handleStandardUpdate,
  handleStandardComplete,
  handleInteractionMessage,
  updateStreamingMessage,
  showPlanPreview,
  showQuestionRequest,
  showWorkerQuestion,
  showPlanConfirmation,
  showClarificationAsMessage,
  updateWorkerDots,
  updatePhaseIndicator,
  setProcessingState,
  clearAllStreamingStates,
  showRecoveryDialog,
  showToolAuthorizationDialog,
  loadSessionMessages,
  showToast,
  addSystemMessage,
  handlePromptEnhanced,
  updatePromptEnhanceStatus
} from './ui/message-handler.js';

import {
  initializeEventListeners,
  updateInteractionModeUI,
  getModeDisplayName,
  showSkillLibraryDialog,
  setWorkerConfigs
} from './ui/event-handlers.js';

import {
  updateModelConnectionStatus,
  updateExecutionStats,
  updateProfileConfig,
  initializeSettingsPanel
} from './ui/settings-handler.js';

import {
  handleProjectKnowledgeLoaded,
  handleADRsLoaded,
  handleFAQsLoaded,
  handleFAQSearchResults,
  handleADRDeleted,
  handleFAQDeleted,
  initializeKnowledgeEventListeners
} from './ui/knowledge-handler.js';

let hasInitialRender = false;
let currentInteractionMode = 'auto';
let mcpServers = [];
let repositories = [];
let workerConfigs = {
  claude: null,
  codex: null,
  gemini: null
};

// ============================================
// 应用初始化
// ============================================

function initializeApp() {
  console.log('[Main] 初始化应用...');

  // 1. 恢复状态
  restoreWebviewState();

  // 2. 初始化事件监听器
  initializeEventListeners();

  // 2.1 初始化知识 Tab 事件监听器
  initializeKnowledgeEventListeners();

  // 3. 设置 window.addEventListener('message') 处理
  window.addEventListener('message', (event) => {
    const message = event.data;

    // 根据消息类型分发到对应的处理函数
    switch (message.type) {
      case 'standardMessage':
        handleStandardMessage(message.message || message);
        break;

      case 'standardUpdate':
        if (message.update) {
          handleStandardUpdate(message.update);
        } else {
          handleStandardUpdate(message);
        }
        break;

      case 'standardComplete':
        handleStandardComplete(message.message || message);
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

          // 保存完整状态供其他模块使用
          setAppState(message.state);

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

          // 同步处理状态（避免时序问题）
          if (message.state.isRunning !== undefined && message.state.isRunning !== isProcessing) {
            if (message.state.isRunning || !hasLocalProcessingGrace()) {
              setProcessingState(message.state.isRunning);
              if (message.state.isRunning) {
                clearLocalProcessingGrace();
              }
            }
          }

          // 更新阶段指示器
          if (message.state.orchestratorPhase) {
            updatePhaseIndicator(message.state.orchestratorPhase, message.state.isRunning);
          }

          const needsSessionLoad = currentSessionId
            && (currentSessionId !== prevSessionId || threadMessages.length === 0);
          if (needsSessionLoad) {
            loadSessionMessages(currentSessionId);
            renderMainContent();
            hasInitialRender = true;
          } else if (!hasInitialRender) {
            renderMainContent();
            hasInitialRender = true;
          } else {
            renderMainContent();
          }

          if (message.state.activePlan) {
            const hasPlanConfirmation = threadMessages.some(m => m.type === 'plan_confirmation' && m.isPending);
            const hasPlanPreview = threadMessages.some(m => m.type === 'plan_ready' && m.planId === message.state.activePlan.planId);
            const isWaitingConfirmation = message.state.orchestratorPhase === 'waiting_confirmation';
            if (!hasPlanPreview && !hasPlanConfirmation && !isWaitingConfirmation) {
              showPlanPreview(
                message.state.activePlan.formattedPlan,
                message.state.activePlan.planId,
                message.state.activePlan.updatedAt,
                message.state.activePlan.review
              );
            }
            const relatedTask = (message.state.tasks || []).find(t => t.planId === message.state.activePlan.planId);
            const reviewRejected = message.state.activePlan.review?.status === 'rejected';
            const shouldResume = !reviewRejected && relatedTask && relatedTask.status !== 'completed' && !message.state.isRunning;
            const hasResumeNotice = threadMessages.some(m => m.type === 'system_notice' && String(m.content || '').includes('未完成计划'));
            if (shouldResume && !hasResumeNotice) {
              addSystemMessage('检测到未完成计划，可输入 /start-work 继续执行。', 'warning');
            }
          }

          updateWorkerDots();
          updateEditsBadge();
          updateTasksBadge();
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
          agentOutputs.claude = [];
          agentOutputs.codex = [];
          agentOutputs.gemini = [];
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

      case 'sessionSwitched':
        if (message.sessionId) {
          setCurrentSessionId(message.sessionId);
          resetIncrementalState(); // 切换会话时重置增量更新状态
          loadSessionMessages(message.sessionId);
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

      case 'workerStatusUpdate':
        updateModelConnectionStatus(message.statuses);
        break;

      case 'workerStatusChanged':
        addSystemMessage(message.worker + ' 状态已更新', 'info');
        updateWorkerDots();
        break;

      case 'processingStateChanged': {
        const state = message.state;
        if (state) {
          setProcessingState(state.isProcessing);
          if (state.isProcessing && state.source && state.agent) {
            setProcessingActor(state.source, state.agent);
          }
          if (state.startedAt && !thinkingStartAt) {
            setThinkingStartAt(state.startedAt);
          }
          if (!state.isProcessing) {
            stopStreamingHintTimer();
          }
        }
        break;
      }

      case 'taskInterrupted': {
        stopStreamingHintTimer();
        setProcessingState(false);

        let hasUpdates = false;
        threadMessages.forEach(m => {
          if (m.streaming) {
            m.streaming = false;
            m.interrupted = true;
            hasUpdates = true;
          }
        });

        ['claude', 'codex', 'gemini'].forEach(agent => {
          const messages = agentOutputs[agent] || [];
          messages.forEach(m => {
            if (m.streaming) {
              m.streaming = false;
              m.interrupted = true;
              hasUpdates = true;
            }
          });
        });

        if (hasUpdates) {
          saveWebviewState();
          renderMainContent();
        }
        break;
      }

      case 'promptEnhanceResult':
        updatePromptEnhanceStatus(message.success, message.message);
        break;

      case 'promptEnhanced':
        handlePromptEnhanced(message.enhancedPrompt, message.error);
        break;

      case 'promptEnhanceConfig': {
        const urlInput = document.getElementById('prompt-enhance-url');
        const keyInput = document.getElementById('prompt-enhance-key');
        if (urlInput) urlInput.value = message.baseUrl || '';
        if (keyInput) keyInput.value = message.apiKey || '';
        break;
      }

      case 'workerError':
        stopStreamingHintTimer();
        clearAllStreamingStates();
        addSystemMessage(message.worker + ': ' + message.error, 'error');
        break;

      case 'allWorkerConfigsLoaded':
        setWorkerConfigs(message.configs || { claude: null, codex: null, gemini: null });
        break;

      case 'workerConfigSaved':
        break;

      case 'workerConnectionTestResult': {
        const workerTestBtn = document.getElementById('worker-test-btn');
        if (workerTestBtn) {
          workerTestBtn.classList.remove('loading');
          workerTestBtn.disabled = false;

          if (message.success) {
            workerTestBtn.classList.add('success');
            workerTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>连接成功';
          } else {
            workerTestBtn.classList.add('error');
            workerTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm.75-8.25a.75.75 0 0 0-1.5 0v3.5a.75.75 0 0 0 1.5 0v-3.5zM8 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2z"/></svg>连接失败';
          }

          setTimeout(() => {
            workerTestBtn.classList.remove('success', 'error');
            workerTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>测试连接';
          }, 2000);
        }
        break;
      }

      case 'orchestratorConfigLoaded': {
        const config = message.config || {};
        const baseUrlInput = document.getElementById('orch-base-url');
        const apiKeyInput = document.getElementById('orch-api-key');
        const modelInput = document.getElementById('orch-model');
        const providerSelect = document.getElementById('orch-provider');

        if (baseUrlInput) baseUrlInput.value = config.baseUrl || '';
        if (apiKeyInput) apiKeyInput.value = config.apiKey || '';
        if (modelInput) modelInput.value = config.model || '';
        if (providerSelect) providerSelect.value = config.provider || 'anthropic';
        break;
      }

      case 'orchestratorConfigSaved':
        break;

      case 'orchestratorConnectionTestResult': {
        const orchTestBtn = document.getElementById('orch-test-btn');
        if (orchTestBtn) {
          orchTestBtn.classList.remove('loading');
          orchTestBtn.disabled = false;

          if (message.success) {
            orchTestBtn.classList.add('success');
            orchTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>连接成功';
          } else {
            orchTestBtn.classList.add('error');
            orchTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm.75-8.25a.75.75 0 0 0-1.5 0v3.5a.75.75 0 0 0 1.5 0v-3.5zM8 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2z"/></svg>连接失败';
          }

          setTimeout(() => {
            orchTestBtn.classList.remove('success', 'error');
            orchTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>测试连接';
          }, 2000);
        }
        break;
      }

      case 'compressorConnectionTestResult': {
        const compTestBtn = document.getElementById('comp-test-btn');
        if (compTestBtn) {
          compTestBtn.classList.remove('loading');
          compTestBtn.disabled = false;

          if (message.success) {
            compTestBtn.classList.add('success');
            compTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>连接成功';
          } else {
            compTestBtn.classList.add('error');
            compTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm.75-8.25a.75.75 0 0 0-1.5 0v3.5a.75.75 0 0 0 1.5 0v-3.5zM8 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2z"/></svg>连接失败';
          }

          setTimeout(() => {
            compTestBtn.classList.remove('success', 'error');
            compTestBtn.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>测试连接';
          }, 2000);
        }
        break;
      }

      case 'compressorConfigLoaded': {
        const config = message.config || {};
        const baseUrlInput = document.getElementById('comp-base-url');
        const apiKeyInput = document.getElementById('comp-api-key');
        const modelInput = document.getElementById('comp-model');
        const providerSelect = document.getElementById('comp-provider');

        if (baseUrlInput) baseUrlInput.value = config.baseUrl || '';
        if (apiKeyInput) apiKeyInput.value = config.apiKey || '';
        if (modelInput) modelInput.value = config.model || '';
        if (providerSelect) providerSelect.value = config.provider || 'anthropic';
        break;
      }

      case 'compressorConfigSaved':
        break;

      case 'mcpServersLoaded':
        mcpServers = message.servers || [];
        setMcpServers(mcpServers);
        renderMCPServerList();
        break;

      case 'mcpServerAdded':
        mcpServers.push(message.server);
        setMcpServers(mcpServers);
        renderMCPServerList();
        break;

      case 'mcpServerUpdated':
        postMessage({ type: 'loadMCPServers' });
        break;

      case 'mcpServerDeleted':
        mcpServers = mcpServers.filter(s => s.id !== message.serverId);
        setMcpServers(mcpServers);
        renderMCPServerList();
        break;

      case 'mcpServerConnected':
      case 'mcpServerDisconnected':
      case 'mcpServerConnectionFailed':
        break;

      case 'mcpToolsRefreshed':
      case 'mcpServerTools':
        renderMCPTools(message.serverId, message.tools);
        break;

      case 'skillsConfigLoaded':
        setSkillsConfig(message.config);
        renderSkillsToolList();
        break;

      case 'skillInstalled':
        break;

      case 'repositoriesLoaded':
        repositories = message.repositories || [];
        setRepositories(repositories);
        if (document.getElementById('repo-manage-overlay')) {
          renderRepositoryManagementList();
        }
        break;

      case 'repositoryAdded':
        repositories.push(message.repository);
        setRepositories(repositories);
        if (document.getElementById('repo-manage-overlay')) {
          renderRepositoryManagementList();
        }
        break;

      case 'repositoryUpdated':
        postMessage({ type: 'loadRepositories' });
        break;

      case 'repositoryDeleted':
        repositories = repositories.filter(r => r.id !== message.repositoryId);
        setRepositories(repositories);
        if (document.getElementById('repo-manage-overlay')) {
          renderRepositoryManagementList();
        }
        break;

      case 'repositoryRefreshed':
        break;

      case 'skillLibraryLoaded':
        showSkillLibraryDialog(message.skills);
        break;

      case 'projectKnowledgeLoaded':
        // 项目知识加载完成
        handleProjectKnowledgeLoaded(message.codeIndex, message.adrs, message.faqs);
        break;

      case 'adrsLoaded':
        // ADR 列表加载完成
        handleADRsLoaded(message.adrs);
        break;

      case 'faqsLoaded':
        // FAQ 列表加载完成
        handleFAQsLoaded(message.faqs);
        break;

      case 'faqSearchResults':
        // FAQ 搜索结果
        handleFAQSearchResults(message.results);
        break;

      case 'adrDeleted':
        // ADR 删除成功
        handleADRDeleted(message.id);
        break;

      case 'faqDeleted':
        // FAQ 删除成功
        handleFAQDeleted(message.id);
        break;

      case 'interactionModeChanged':
        currentInteractionMode = message.mode;
        updateInteractionModeUI(message.mode);
        addSystemMessage('已切换到 ' + getModeDisplayName(message.mode) + ' 模式', 'info');
        break;

      case 'phaseChanged':
        updatePhaseIndicator(message.phase, message.isRunning);
        break;

      case 'verificationResult':
        if (message.success) {
          addSystemMessage('验证通过: ' + message.summary, 'success');
        } else {
          addSystemMessage('验证失败: ' + message.summary, 'error');
        }
        break;

      case 'recoveryRequest':
        showRecoveryDialog(message.taskId, message.error, message.canRetry, message.canRollback);
        break;

      case 'recoveryResult':
        addSystemMessage(message.message, message.success ? 'success' : 'error');
        break;

      case 'toolAuthorizationRequest':
        showToolAuthorizationDialog(message.toolName, message.toolArgs);
        break;

      case 'workerFallbackNotice':
        addSystemMessage(`${message.originalWorker} 降级到 ${message.fallbackWorker}: ${message.reason}`, 'warning');
        break;

      case 'dependencyAnalysis':
        if (message.data) {
          showDependencyAnalysis(message.data);
        }
        break;

      case 'workerTaskCard': {
        if (currentSessionId && message.sessionId && message.sessionId !== currentSessionId) return;
        const worker = message.worker;
        if (!worker) return;

        const agentMessages = agentOutputs[worker] || [];
        const existingTaskCard = agentMessages.find(m => m.type === 'task_card' && m.taskId === message.taskId && m.subTaskId === message.subTaskId);
        if (existingTaskCard) {
          Object.assign(existingTaskCard, {
            description: message.description,
            targetFiles: message.targetFiles || existingTaskCard.targetFiles || [],
            reason: message.reason || existingTaskCard.reason || '',
            status: message.status || existingTaskCard.status || 'started',
            dispatchId: message.dispatchId || existingTaskCard.dispatchId,
            timestamp: Date.now()
          });
        } else {
          agentMessages.push({
            type: 'task_card',
            role: 'system',
            taskId: message.taskId,
            subTaskId: message.subTaskId,
            description: message.description,
            targetFiles: message.targetFiles || [],
            reason: message.reason || '',
            status: message.status || 'started',
            time: new Date().toLocaleTimeString().slice(0, 5),
            timestamp: Date.now(),
            dispatchId: message.dispatchId,
            agent: worker
          });
        }
        agentOutputs[worker] = agentMessages;

        if (currentBottomTab === worker) {
          scheduleRenderMainContent();
        }
        saveWebviewState();
        break;
      }

      case 'questionRequest':
        if (currentSessionId && message.sessionId && message.sessionId !== currentSessionId) {
          return;
        }
        showQuestionRequest(message.questions || []);
        break;

      case 'clarificationRequest':
        if (currentSessionId && message.sessionId && message.sessionId !== currentSessionId) {
          return;
        }
        showClarificationAsMessage(message.questions, message.context, message.ambiguityScore, message.originalPrompt);
        break;

      case 'workerQuestionRequest':
        if (currentSessionId && message.sessionId && message.sessionId !== currentSessionId) {
          return;
        }
        showWorkerQuestion(message.workerId, message.question, message.context, message.options);
        break;

      case 'confirmationRequest':
        if (currentSessionId && message.sessionId && message.sessionId !== currentSessionId) {
          return;
        }
        if (currentInteractionMode === 'ask') {
          return;
        }
        showPlanConfirmation(message.plan, message.formattedPlan);
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
  agentOutputs,
  sessions,
  pendingChanges,
  renderMainContent,
  showToast,
  postMessage
};

console.log('[Main] 主模块加载完成');
