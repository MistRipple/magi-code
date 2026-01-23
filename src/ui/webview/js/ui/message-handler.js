// 消息处理模块
// 此文件包含所有消息处理、转换和生命周期管理相关的函数

import {
  threadMessages,
  agentOutputs,
  currentSessionId,
  isProcessing,
  thinkingStartAt,
  processingActor,
  pendingChanges,
  sessions,
  currentTopTab,
  currentBottomTab,
  appState,
  saveWebviewState,
  stopStreamingHintTimer,
  setIsProcessing,
  setThinkingStartAt,
  setProcessingActor,
  setLocalProcessingGrace,
  hasLocalProcessingGrace,
  clearLocalProcessingGrace
} from '../core/state.js';

import {
  escapeHtml,
  formatTimestamp,
  formatElapsed,
  formatRelativeTime,
  smoothScrollToBottom
} from '../core/utils.js';

import {
  postMessage
} from '../core/vscode-api.js';

import {
  renderMainContent,
  scheduleRenderMainContent,
  getRoleIcon,
  getRoleInfo,
  getMessageGroupKey,
  renderMessageContentSmart,
  renderSessionList,
  extractTextFromBlocks,
  extractCodeBlocksFromBlocks,
  extractThinkingFromBlocks,
  extractToolCallsFromBlocks
} from './message-renderer.js';

import { resetIncrementalState } from '../core/incremental-update.js';

// 标准消息存储 - 按 messageId 索引
const standardMessages = new Map();
// 早到的流式更新（避免 update 先于 message 抵达被丢弃）
const pendingStandardUpdates = new Map();

// 消息列表限制
const MAX_THREAD_MESSAGES = 500;
const MAX_AGENT_MESSAGES = 200;

// 交互状态
let interactionState = 'idle'; // 'idle' | 'processing' | 'awaiting'
let awaitingContext = null;

// 流式消息 key 生成
let streamKeyCounter = 0;

// ============================================
// 消息处理函数
// ============================================


// ============================================
// 核心消息处理
// ============================================

export function handleStandardMessage(message) {
      if (!message || !message.id) {
        console.warn('[Webview] 收到无效的标准消息:', message);
        return;
      }

      console.log('[Webview] 收到标准消息:', message.id, message.type, message.lifecycle);

      // 处理交互消息（Worker 询问）
      if (message.type === 'interaction' && message.interaction) {
        handleInteractionMessage(message);
        return;
      }

      // 过滤编排者内部 JSON 分析输出（如模糊度评估、计划评审）
      if (message.source === 'orchestrator') {
        const textContent = extractTextFromBlocks(message.blocks || []);
        if (isInternalJsonMessage(textContent) && message.type !== 'plan') {
          return;
        }
      }

      // 存储消息
      standardMessages.set(message.id, message);
      // 应用可能早到的流式更新
      applyPendingUpdates(message);

      // 获取目标信息
      const isOrchestrator = message.source === 'orchestrator';
      const agent = message.agent || 'claude';

      // 处理特殊消息类型
      if (message.type === 'plan') {
        const content = extractTextFromBlocks(message.blocks || []);
        const planId = message.metadata?.taskId || message.id;
        showPlanPreview(content, planId, message.timestamp, message.metadata?.extra?.review);
        return;
      }

      // 检查是否已存在该消息
      const messages = isOrchestrator ? threadMessages : (agentOutputs[agent] || []);
      const existingMsg = messages.find(m => m.standardMessageId === message.id);

      if (existingMsg) {
        // 更新现有消息
        const updatedMsg = standardToWebviewMessage(message);
        if (existingMsg.startedAt && !updatedMsg.startedAt) {
          updatedMsg.startedAt = existingMsg.startedAt;
        }
        if (existingMsg.streamKey) {
          updatedMsg.streamKey = existingMsg.streamKey;
        }
        Object.assign(existingMsg, updatedMsg);
      } else {
        // 🔧 简化：直接添加消息，后端已去重
        const webviewMsg = standardToWebviewMessage(message);

        // 🔧 新增：内容去重 - 检查是否存在内容完全相同的消息
        const targetMessages = isOrchestrator ? threadMessages : (agentOutputs[agent] || []);
        const duplicateByContent = findEquivalentMessage(targetMessages, webviewMsg, 30000);
        if (duplicateByContent) {
          console.log('[Webview] 跳过内容重复的消息:', message.id, 'duplicate of:', duplicateByContent.standardMessageId);
          // 更新已存在消息的元数据而不是添加新消息
          if (webviewMsg.standardMessageId) {
            duplicateByContent.standardMessageId = webviewMsg.standardMessageId;
          }
          return;
        }

        if (isOrchestrator) {
          threadMessages.push(webviewMsg);
        } else {
          if (!agentOutputs[agent]) {
            agentOutputs[agent] = [];
          }
          agentOutputs[agent].push(webviewMsg);
        }
      }

      // Worker 输出镜像到 Thread 面板，确保主对话可见流式输出
      if (!isOrchestrator) {
        upsertThreadMirrorFromWorker(message);
      }

      // 设置处理状态
      if (message.lifecycle === 'streaming' || message.lifecycle === 'started') {
        setProcessingState(true);
        setProcessingActor(message.source, agent);
      }

      saveWebviewState();
      scheduleRenderMainContent();
      smoothScrollToBottom();
      updateWorkerDots();
    }

export function bufferStandardUpdate(update) {
      if (!update || !update.messageId) return;
      const list = pendingStandardUpdates.get(update.messageId) || [];
      list.push(update);
      pendingStandardUpdates.set(update.messageId, list);
    }

export function handleStandardUpdate(update) {
      if (!update || !update.messageId) {
        return;
      }

      const message = standardMessages.get(update.messageId);
      if (!message) {
        console.warn('[Webview] 未找到消息，暂存更新:', update.messageId);
        bufferStandardUpdate(update);
        return;
      }

      // 应用更新到标准消息
      applyUpdateToStandardMessage(message, update);

      // 查找并更新对应的 Webview 消息
      const isOrchestrator = message.source === 'orchestrator';
      const agent = message.agent || 'claude';
      const messages = isOrchestrator ? threadMessages : (agentOutputs[agent] || []);

      const webviewMsg = messages.find(m => m.standardMessageId === update.messageId);
      if (webviewMsg) {
        const updatedMsg = standardToWebviewMessage(message);
        if (webviewMsg.startedAt && !updatedMsg.startedAt) {
          updatedMsg.startedAt = webviewMsg.startedAt;
        }
        if (webviewMsg.streamKey) {
          updatedMsg.streamKey = webviewMsg.streamKey;
        }
        Object.assign(webviewMsg, updatedMsg);

        // 尝试增量更新 DOM（基于 blocks 渲染）
        if (update.updateType === 'append' || update.updateType === 'replace') {
          const updateSuccess = isOrchestrator
            ? updateStreamingMessage(webviewMsg.streamKey, webviewMsg.content)
            : updateAgentStreamingMessage(agent, webviewMsg.content);

          if (!updateSuccess) {
            scheduleRenderMainContent();
          }
        } else {
          scheduleRenderMainContent();
        }
      }

      // 同步更新 Thread 面板中的 Worker 镜像
      if (message.source !== 'orchestrator') {
        const mirrorMsg = upsertThreadMirrorFromWorker(message);
        if (mirrorMsg && (update.updateType === 'append' || update.updateType === 'replace')) {
          updateStreamingMessage(mirrorMsg.streamKey, mirrorMsg.content);
        }
      }

      throttledSaveState();
      smoothScrollToBottom();
      updateWorkerDots();
    }

export function handleStandardComplete(message) {
      if (!message || !message.id) {
        return;
      }

      console.log('[Webview] 标准消息完成:', message.id, message.lifecycle);

      // 更新存储的消息
      standardMessages.set(message.id, message);
      // 应用可能早到的流式更新
      applyPendingUpdates(message);

      // 查找并更新对应的 Webview 消息
      const isOrchestrator = message.source === 'orchestrator';
      const agent = message.agent || 'claude';
      const messages = isOrchestrator ? threadMessages : (agentOutputs[agent] || []);

      const webviewMsg = messages.find(m => m.standardMessageId === message.id);
      if (webviewMsg) {
        // 更新最终状态
        const finalMsg = standardToWebviewMessage(message);
        // 保留已有内容（如果完成消息内容为空）
        if (!finalMsg.content && webviewMsg.content) {
          finalMsg.content = webviewMsg.content;
          finalMsg.parsedBlocks = webviewMsg.parsedBlocks;
        }
        if (webviewMsg.startedAt && !finalMsg.startedAt) {
          finalMsg.startedAt = webviewMsg.startedAt;
        }
        if (webviewMsg.streamKey) {
          finalMsg.streamKey = webviewMsg.streamKey;
        }
        Object.assign(webviewMsg, finalMsg);
        webviewMsg.streaming = false;

        if (message.lifecycle === 'interrupted' || message.lifecycle === 'cancelled') {
          webviewMsg.interrupted = true;
        }
        if (message.lifecycle === 'failed') {
          webviewMsg.error = message.metadata?.error;
        }
      }

      // 同步更新 Thread 面板中的 Worker 镜像
      if (message.source !== 'orchestrator') {
        upsertThreadMirrorFromWorker(message);
      }

      // 检查是否还有活跃的流式消息，用于停止提示计时器
      // 注意：处理状态由后端 processingStateChanged 事件控制，前端不再自行判断
      const hasActiveStreaming = threadMessages.some(m => m.streaming) ||
        ['claude', 'codex', 'gemini'].some(c => (agentOutputs[c] || []).some(m => m.streaming));

      if (!hasActiveStreaming) {
        stopStreamingHintTimer();
        // 不再在此处调用 setProcessingState(false)
        // 处理状态完全由后端 UnifiedMessageBus 的 processingStateChanged 事件控制
      }

      saveWebviewState();
      scheduleRenderMainContent();
      smoothScrollToBottom();
    }

// ============================================
// 交互状态管理
// ============================================

export function setProcessingState(next, forceResetTimer = false) {
      if (interactionState === 'awaiting' && next === true) {
        console.log('[Webview] 忽略处理状态变更，当前正在等待用户输入');
        return;
      }

      const changed = isProcessing !== next;
      setIsProcessing(next);
      if (next) {
        if (!thinkingStartAt || forceResetTimer) {
          setThinkingStartAt(Date.now());
        }
        interactionState = 'processing';
      } else {
        setThinkingStartAt(null);
        if (interactionState !== 'awaiting') {
          interactionState = 'idle';
        }
      }

      if (changed) {
        updateSendButtonState();
        saveWebviewState();
      }
    }

export function enterAwaitingState(context) {
      interactionState = 'awaiting';
      awaitingContext = context || {};
      setIsProcessing(false);
      setThinkingStartAt(null);

      const inputBox = document.getElementById('prompt-input');
      if (inputBox) {
        inputBox.disabled = false;
        if (context && context.prompt) {
          const promptPreview = context.prompt.length > 40
            ? context.prompt.substring(0, 40) + '...'
            : context.prompt;
          inputBox.placeholder = '回复: ' + promptPreview;
        } else {
          inputBox.placeholder = '输入您的回复...';
        }
        inputBox.focus();
      }
      updateSendButtonState();
      saveWebviewState();
    }

export function exitAwaitingState() {
      interactionState = 'idle';
      awaitingContext = null;
      window._pendingClarification = null;
      window._pendingWorkerQuestion = null;

      const inputBox = document.getElementById('prompt-input');
      if (inputBox) {
        inputBox.placeholder = '描述你的任务... (可粘贴图片)';
      }
      updateSendButtonState();
    }

export function updateSendButtonState() {
      const btn = document.getElementById('execute-btn');
      const inputBox = document.getElementById('prompt-input');
      if (!btn) return;

      if (interactionState === 'processing') {
        btn.classList.add('processing');
        btn.disabled = false;
        btn.title = '停止 (Esc)';
        if (inputBox) inputBox.disabled = true;
      } else if (interactionState === 'awaiting') {
        btn.classList.remove('processing');
        btn.disabled = false;
        btn.title = '发送回复 (Cmd+Enter)';
        if (inputBox) inputBox.disabled = false;
      } else {
        btn.classList.remove('processing');
        btn.disabled = false;
        btn.title = '发送 (Cmd+Enter)';
        if (inputBox) inputBox.disabled = false;
      }
    }

// ============================================
// 辅助函数
// ============================================

export function generateStreamKey(source, agent) {
      streamKeyCounter += 1;
      return (source || 'orchestrator') + ':' + (agent || 'claude') + ':' + Date.now() + '-' + streamKeyCounter;
    }

export function detectSpecialContent(content) {
      if (!content) return { hasSpecial: false };
      const lines = content.split('\n');

      let pathCount = 0;
      for (const line of lines) {
        if (/^\/[^\s]+/.test(line.trim())) {
          pathCount++;
        }
      }
      if (pathCount >= 5) {
        return { hasSpecial: true, type: 'file-list', count: pathCount };
      }

      let numberedLineCount = 0;
      for (const line of lines) {
        if (/^\s*\d+→/.test(line)) {
          numberedLineCount++;
        }
      }
      if (numberedLineCount >= 5) {
        return { hasSpecial: true, type: 'numbered-code', count: numberedLineCount };
      }

      return { hasSpecial: false };
    }

export function updateWorkerDots() {
      const statuses = appState?.workerStatuses || [];
      const tasks = appState?.tasks || [];
      ['claude', 'codex', 'gemini'].forEach(worker => {
        const s = statuses.find(x => x.worker === worker);
        const dotEl = document.getElementById('dot-' + worker);
        if (!dotEl) return;
        const isRunning = tasks.some(t =>
          Array.isArray(t.subTasks) && t.subTasks.some(st => st.status === 'running' && st.assignedWorker === worker)
        );
        dotEl.className = 'dot ' + (isRunning ? 'running' : s?.available ? 'available' : 'unavailable');
      });
    }

export function clearAllStreamingStates() {
      // 清除 Thread 消息的 streaming 状态
      threadMessages.forEach(m => {
        if (m.streaming) {
          m.streaming = false;
        }
      });

      // 清除所有 Worker 消息的 streaming 状态
      ['claude', 'codex', 'gemini'].forEach(agent => {
        const msgs = agentOutputs[agent] || [];
        msgs.forEach(m => {
          if (m.streaming) {
            m.streaming = false;
          }
        });
      });

      // 移除 DOM 中的 streaming 类和动画元素
      document.querySelectorAll('.message.streaming').forEach(el => {
        el.classList.remove('streaming');
      });
      document.querySelectorAll('.message-streaming-hint, .message-streaming-footer').forEach(el => {
        el.remove();
      });

      // 注意：不再在此处调用 setProcessingState(false)
      // 处理状态完全由后端 processingStateChanged 事件控制
      // 此函数仅负责清理流式消息的 UI 状态
    }

let saveStateTimeout = null;
export function throttledSaveState() {
      if (saveStateTimeout) return;
      saveStateTimeout = setTimeout(() => {
        saveWebviewState();
        saveStateTimeout = null;
      }, 500);
    }

// ============================================
// 计划与提问
// ============================================

export function showPlanPreview(formattedPlan, planId, timestamp, review) {
      const existingIdx = threadMessages.findIndex(m =>
        m.type === 'plan_ready' && m.planId === planId
      );
      if (existingIdx !== -1) {
        threadMessages[existingIdx].content = formattedPlan;
        threadMessages[existingIdx].timestamp = timestamp || Date.now();
        threadMessages[existingIdx].review = review;
        saveWebviewState();
        renderMainContent();
        smoothScrollToBottom();
        return;
      }
      threadMessages.push({
        role: 'system',
        type: 'plan_ready',
        content: formattedPlan,
        planId,
        time: new Date().toLocaleTimeString().slice(0, 5),
        timestamp: timestamp || Date.now(),
        review
      });
      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function showQuestionRequest(questions) {
      const normalized = Array.isArray(questions) ? questions.filter(q => q && String(q).trim()) : [];
      if (normalized.length === 0) return;

      const existingIdx = threadMessages.findIndex(m => m.type === 'question_request' && m.isPending);
      if (existingIdx !== -1) {
        threadMessages[existingIdx].questions = normalized;
      } else {
        threadMessages.push({
          role: 'system',
          type: 'question_request',
          questions: normalized,
          time: new Date().toLocaleTimeString().slice(0, 5),
          timestamp: Date.now(),
          isPending: true
        });
      }

      clearLocalProcessingGrace();
      enterAwaitingState({
        type: 'question_request',
        prompt: '请回答补充问题后发送...'
      });

      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function showWorkerQuestion(workerId, question, context, options) {
      let content = `**${workerId}** 需要您的帮助：\n\n`;
      content += question;
      if (context) {
        content += `\n\n> ${context}`;
      }
      content += '\n\n请在下方输入框中回复。';

      threadMessages.push({
        role: 'assistant',
        content,
        time: new Date().toLocaleTimeString().slice(0, 5),
        timestamp: Date.now(),
        source: 'worker',
        agent: workerId,
        isWorkerQuestion: true,
        workerQuestionData: {
          workerId,
          question,
          context,
          options
        }
      });

      window._pendingWorkerQuestion = {
        workerId,
        question,
        context,
        options,
        timestamp: Date.now()
      };

      clearLocalProcessingGrace();
      enterAwaitingState({
        type: 'worker_question',
        agent: workerId,
        prompt: `回复 ${workerId} 的问题...`
      });

      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function showPlanConfirmation(plan, formattedPlan) {
      const planReadyIdx = threadMessages.findIndex(m => m.type === 'plan_ready');
      if (planReadyIdx !== -1) {
        const existingMsg = threadMessages[planReadyIdx];
        existingMsg.type = 'plan_confirmation';
        existingMsg.content = formattedPlan;
        existingMsg.plan = plan;
        existingMsg.isPending = true;
      } else {
        threadMessages.push({
          role: 'system',
          type: 'plan_confirmation',
          content: formattedPlan,
          plan,
          isPending: true,
          time: new Date().toLocaleTimeString().slice(0, 5),
          timestamp: Date.now()
        });
      }

      clearLocalProcessingGrace();
      enterAwaitingState({
        type: 'plan_confirmation',
        prompt: '请确认或拒绝执行计划...'
      });

      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function updatePhaseIndicator(phase, isRunningFromBackend) {
      const indicator = document.getElementById('phase-indicator');
      if (!indicator) return;

      const phaseMap = {
        idle: 0,
        started: 1,
        phase1_analysis: 1,
        analyzing: 1,
        phase2_waiting_confirmation: 2,
        waiting_confirmation: 2,
        waiting_questions: 2,
        phase3_execution: 3,
        dispatching: 3,
        monitoring: 3,
        integrating: 4,
        phase4_verification: 5,
        verifying: 5,
        phase5_recovery: 6,
        recovering: 6,
        phase6_summary: 7,
        summarizing: 7,
        completed: 8,
        failed: -1,
        interrupted: -2
      };

      const currentPhaseNum = phaseMap[phase] || 0;

      let nextProcessing = false;
      if (isRunningFromBackend !== undefined) {
        nextProcessing = isRunningFromBackend;
      } else {
        nextProcessing = currentPhaseNum > 0 && currentPhaseNum < 7;
      }
      if (phase === 'waiting_questions') {
        nextProcessing = false;
      }
      if (!nextProcessing && hasLocalProcessingGrace()) {
        nextProcessing = true;
      }

      if (!nextProcessing && (phase === 'completed' || phase === 'failed' || phase === 'interrupted' || phase === 'idle')) {
        let hasUpdates = false;
        threadMessages.forEach(m => {
          if (m.streaming) {
            m.streaming = false;
            hasUpdates = true;
          }
          if (m.isPending && (m.type === 'plan_confirmation' || m.type === 'question_request')) {
            m.isPending = false;
            hasUpdates = true;
          }
        });
        ['claude', 'codex', 'gemini'].forEach(agent => {
          const msgs = agentOutputs[agent] || [];
          msgs.forEach(m => {
            if (m.streaming) {
              m.streaming = false;
              hasUpdates = true;
            }
          });
        });
        if (hasUpdates) {
          saveWebviewState();
          renderMainContent();
        }
      }

      setProcessingState(nextProcessing);

      if (currentPhaseNum > 0 && currentPhaseNum < 8) {
        indicator.classList.add('visible');
      } else {
        indicator.classList.remove('visible');
      }

      const steps = indicator.querySelectorAll('.phase-step');
      const connectors = indicator.querySelectorAll('.phase-step-connector');

      steps.forEach((step, idx) => {
        const stepNum = idx + 1;
        step.classList.remove('active', 'completed');
        if (stepNum === currentPhaseNum) {
          step.classList.add('active');
        } else if (stepNum < currentPhaseNum) {
          step.classList.add('completed');
        }
      });

      connectors.forEach((conn, idx) => {
        conn.classList.toggle('completed', idx + 1 < currentPhaseNum);
      });
    }

export function showRecoveryDialog(taskId, error, canRetry, canRollback) {
      stopStreamingHintTimer();
      clearAllStreamingStates();

      const dialog = document.createElement('div');
      dialog.className = 'recovery-dialog visible';
      dialog.id = 'recovery-dialog';
      dialog.innerHTML = `
        <div class="recovery-dialog-title">
          <svg viewBox="0 0 16 16"><path d="M8.982 1.566a1.13 1.13 0 0 0-1.96 0L.165 13.233c-.457.778.091 1.767.98 1.767h13.713c.889 0 1.438-.99.98-1.767L8.982 1.566zM8 5c.535 0 .954.462.9.995l-.35 3.507a.552.552 0 0 1-1.1 0L7.1 5.995A.905.905 0 0 1 8 5zm.002 6a1 1 0 1 1 0 2 1 1 0 0 1 0-2z"/></svg>
          任务执行失败
        </div>
        <div class="recovery-dialog-error">${escapeHtml(error || '')}</div>
        <div class="recovery-dialog-actions">
          ${canRetry ? '<button class="recovery-btn retry" data-decision="retry">重试</button>' : ''}
          ${canRollback ? '<button class="recovery-btn rollback" data-decision="rollback">回滚</button>' : ''}
          <button class="recovery-btn continue" data-decision="continue">忽略继续</button>
        </div>
      `;

      const mainContent = document.getElementById('main-content');
      if (mainContent) {
        mainContent.appendChild(dialog);
      }

      dialog.querySelectorAll('.recovery-btn').forEach(btn => {
        btn.addEventListener('click', () => {
          const decision = btn.dataset.decision;
          postMessage({ type: 'confirmRecovery', decision: decision, taskId: taskId });
          dialog.remove();
        });
      });
    }

export function showToolAuthorizationDialog(toolName, toolArgs) {
      // 移除已存在的授权对话框
      const existingDialog = document.getElementById('tool-auth-dialog');
      if (existingDialog) {
        existingDialog.remove();
      }

      const dialog = document.createElement('div');
      dialog.className = 'tool-auth-dialog visible';
      dialog.id = 'tool-auth-dialog';

      // 格式化工具参数
      let argsDisplay = '';
      try {
        argsDisplay = JSON.stringify(toolArgs, null, 2);
      } catch (e) {
        argsDisplay = String(toolArgs || '');
      }

      dialog.innerHTML = `
        <div class="tool-auth-dialog-title">
          <svg viewBox="0 0 16 16"><path d="M8 1a2 2 0 0 1 2 2v4H6V3a2 2 0 0 1 2-2zm3 6V3a3 3 0 0 0-6 0v4a2 2 0 0 0-2 2v5a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2z"/></svg>
          工具授权请求
        </div>
        <div class="tool-auth-dialog-content">
          <div class="tool-auth-tool-name">
            <span class="tool-auth-label">工具:</span>
            <span class="tool-auth-value">${escapeHtml(toolName)}</span>
          </div>
          <div class="tool-auth-tool-args">
            <span class="tool-auth-label">参数:</span>
            <pre class="tool-auth-args-pre">${escapeHtml(argsDisplay)}</pre>
          </div>
        </div>
        <div class="tool-auth-dialog-actions">
          <button class="tool-auth-btn deny" data-allowed="false">拒绝</button>
          <button class="tool-auth-btn allow" data-allowed="true">允许</button>
        </div>
      `;

      const mainContent = document.getElementById('main-content');
      if (mainContent) {
        mainContent.appendChild(dialog);
      }

      dialog.querySelectorAll('.tool-auth-btn').forEach(btn => {
        btn.addEventListener('click', () => {
          const allowed = btn.dataset.allowed === 'true';
          postMessage({ type: 'toolAuthorizationResponse', allowed: allowed });
          dialog.remove();
        });
      });
    }

export function handleInteractionMessage(message) {
      const interaction = message.interaction;
      const worker = message.agent || 'claude';

      console.log('[Webview] 收到交互消息:', interaction.type, interaction.requestId);

      // 只处理 QUESTION 类型的交互
      if (interaction.type !== 'question') {
        console.warn('[Webview] 不支持的交互类型:', interaction.type);
        return;
      }

      // 创建 Worker 问题消息
      const questionMsg = {
        role: 'worker_question',
        type: 'worker_question',
        worker: worker,
        questionId: interaction.requestId,
        content: interaction.prompt,
        pattern: message.metadata?.questionPattern || 'interaction',
        time: new Date(message.timestamp).toLocaleTimeString().slice(0, 5),
        timestamp: message.timestamp,
        isPending: true,
        isWorkerQuestion: true,
        adapterRole: message.metadata?.adapterRole,
        standardMessageId: message.id,
        traceId: message.traceId
      };

      // 添加到 Thread 消息
      const existingThreadIdx = threadMessages.findIndex(m =>
        m.type === 'worker_question' && m.questionId === interaction.requestId
      );

      if (existingThreadIdx !== -1) {
        threadMessages[existingThreadIdx] = { ...threadMessages[existingThreadIdx], ...questionMsg };
      } else {
        threadMessages.push(questionMsg);
      }

      // 设置全局变量（使用 Worker 问题机制）
      window._pendingWorkerQuestion = {
        workerId: worker,
        question: interaction.prompt,
        questionId: interaction.requestId
      };

      // 清除处理宽限期并进入等待状态
      clearLocalProcessingGrace();
      enterAwaitingState({
        type: 'worker_question',
        worker: worker,
        requestId: interaction.requestId,
        prompt: interaction.prompt
      });

      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function applyUpdateToStandardMessage(message, update) {
      if (update.updateType === 'append' && update.appendText) {
        // 找到或创建文本块
        let textBlock = message.blocks.find(b => b.type === 'text');
        if (!textBlock) {
          textBlock = { type: 'text', content: '', isMarkdown: true };
          message.blocks.push(textBlock);
        }
        textBlock.content += update.appendText;
      } else if (update.updateType === 'replace' && update.replaceText !== undefined) {
        let textBlock = message.blocks.find(b => b.type === 'text');
        if (!textBlock) {
          textBlock = { type: 'text', content: '', isMarkdown: true };
          message.blocks.push(textBlock);
        }
        textBlock.content = update.replaceText;
      } else if (update.updateType === 'block_update' && update.blocks) {
        // 合并新的块
        for (const newBlock of update.blocks) {
          if (newBlock.type === 'tool_call') {
            // 工具调用按 ID 更新
            const existing = message.blocks.find(b => b.type === 'tool_call' && b.toolId === newBlock.toolId);
            if (existing) {
              Object.assign(existing, newBlock);
            } else {
              message.blocks.push(newBlock);
            }
          } else if (newBlock.type === 'thinking' && newBlock.blockId) {
            const existing = message.blocks.find(b => b.type === 'thinking' && b.blockId === newBlock.blockId);
            if (existing) {
              Object.assign(existing, newBlock);
            } else {
              message.blocks.push(newBlock);
            }
          } else {
            message.blocks.push(newBlock);
          }
        }
      }
    }

export function standardToWebviewMessage(message) {
      // 🆕 直接使用后端已解析的 blocks，不再重新解析
      const blocks = message.blocks || [];
      const textContent = extractTextFromBlocks(blocks);
      const thinking = extractThinkingFromBlocks(blocks);
      const toolCalls = extractToolCallsFromBlocks(blocks);
      const codeBlocks = extractCodeBlocksFromBlocks(blocks);

      const timestamp = typeof message.timestamp === 'number' ? message.timestamp : Date.now();
      const timeDisplay = message.timestamp ? new Date(message.timestamp).toLocaleTimeString().slice(0, 5) : '';

        const isStreaming = message.lifecycle === 'streaming' || message.lifecycle === 'started';
        return {
          role: message.source === 'user' ? 'user' : 'assistant',
          content: textContent,
          time: timeDisplay,
          timestamp: timestamp,
          streaming: isStreaming,
          startedAt: isStreaming ? Date.now() : undefined,
          source: message.source,
          agent: message.agent,
          streamKey: generateStreamKey(message.source, message.agent),
          thinking: thinking,
          toolCalls: toolCalls,
          codeBlocks: codeBlocks,  // 🆕 新增：已解析的代码块
          parsedBlocks: blocks,     // 🆕 新增：保留原始 blocks 供渲染使用
          metadata: message.metadata || {},
          // 标准消息扩展字段
          standardMessageId: message.id,
          traceId: message.traceId,
          lifecycle: message.lifecycle,
          messageType: message.type,
        };
      }


// ============================================
// 流式消息管理
// ============================================

export function updateStreamingMessage(streamKey, content) {
      const container = document.getElementById('main-content');
      if (!container || currentBottomTab !== 'thread') return false;

      // 查找带有 streaming 类的消息元素
      const streamingMessage = container.querySelector('.message.streaming[data-stream-key="' + streamKey + '"]');
      if (!streamingMessage) return false;

      const contentEl = streamingMessage.querySelector('.message-content');
      if (!contentEl) return false;

      const streamingMsg = threadMessages.find(m => m.streaming && m.streamKey === streamKey);
      const agent = streamingMsg?.agent || 'claude';
      // 🆕 使用智能渲染：优先使用后端已解析的 blocks
      const rendered = streamingMsg ? renderMessageContentSmart(streamingMsg, agent) : { html: escapeHtml(content || ''), isMarkdown: false };
      contentEl.innerHTML = rendered.html;
      if (rendered.isMarkdown) {
        contentEl.classList.add('markdown-rendered');
      } else {
        contentEl.classList.remove('markdown-rendered');
      }

      // 🔧 检测特殊内容并添加溢出标记
      const special = detectSpecialContent(content);
      if (special.hasSpecial) {
        contentEl.classList.add('has-special-content');
      }
      // 检测内容是否溢出
      if (contentEl.scrollHeight > contentEl.clientHeight + 20) {
        contentEl.classList.add('content-overflow');
      }

      return true;
    }

export function findActiveStreamMessage(source, agent) {
      const prefix = (source || 'orchestrator') + ':' + (agent || 'claude') + ':';
      for (let i = threadMessages.length - 1; i >= 0; i -= 1) {
        const msg = threadMessages[i];
        if (msg.streaming && msg.streamKey && msg.streamKey.startsWith(prefix)) {
          return { idx: i, msg };
        }
      }
      return null;
    }

export function ensureThreadStreamMessage(source, agent, initialContent) {
      // 查找当前来源的活跃流式消息
      const active = findActiveStreamMessage(source, agent);
      if (active) {
        return active;
      }
      // 创建新的流式消息，使用唯一的 streamKey
      const streamKey = generateStreamKey(source, agent);
      const msg = {
        role: 'assistant',
        content: initialContent || '',
        time: new Date().toLocaleTimeString().slice(0,5),
        timestamp: Date.now(),
        streaming: true,
        startedAt: Date.now(),
        source: source,
        agent: agent,
        streamKey: streamKey
      };
      threadMessages.push(msg);
      const idx = threadMessages.length - 1;
      return { idx, msg: threadMessages[idx] };
    }

export function updateAgentStreamingMessage(agent, content) {
      const container = document.getElementById('main-content');
      if (!container || currentBottomTab !== agent) return false;

      // 查找带有 streaming 类的消息元素
      const streamingMessage = container.querySelector('.message.streaming[data-agent="' + agent + '"]');
      if (!streamingMessage) return false;

      const contentEl = streamingMessage.querySelector('.message-content');
      if (!contentEl) return false;

      const messages = agentOutputs[agent] || [];
      const lastMsg = messages.find(m => m.streaming);
      if (lastMsg) {
        // 🆕 使用智能渲染：优先使用后端已解析的 blocks
        const rendered = renderMessageContentSmart(lastMsg, agent);
        contentEl.innerHTML = rendered.html;
        if (rendered.isMarkdown) {
          contentEl.classList.add('markdown-rendered');
        } else {
          contentEl.classList.remove('markdown-rendered');
        }

        // 🔧 检测特殊内容并添加溢出标记
        const special = detectSpecialContent(content || lastMsg.content || '');
        if (special.hasSpecial) {
          contentEl.classList.add('has-special-content');
        }
        // 检测内容是否溢出
        if (contentEl.scrollHeight > contentEl.clientHeight + 20) {
          contentEl.classList.add('content-overflow');
        }
      } else {
        contentEl.textContent = content;
      }
      return true;
    }


// ============================================
// 消息转换和规范化
// ============================================

export function upsertThreadMirrorFromWorker(message) {
      if (!message || message.source === 'orchestrator') return null;
      const existingIdx = threadMessages.findIndex(m => m.standardMessageId === message.id && m.source === 'worker');
      const mirrorMsg = standardToWebviewMessage(message);
      mirrorMsg.mirroredFromCli = true;
      if (existingIdx !== -1) {
        const existing = threadMessages[existingIdx];
        if (existing.startedAt && !mirrorMsg.startedAt) {
          mirrorMsg.startedAt = existing.startedAt;
        }
        if (existing.streamKey) {
          mirrorMsg.streamKey = existing.streamKey;
        }
        Object.assign(existing, mirrorMsg);
        return existing;
      }

      // 🔧 新增：内容去重 - 检查是否存在内容完全相同的消息
      const duplicateByContent = findEquivalentMessage(threadMessages, mirrorMsg, 30000);
      if (duplicateByContent) {
        console.log('[Webview] Worker 镜像跳过内容重复的消息:', message.id);
        return duplicateByContent;
      }

      threadMessages.push(mirrorMsg);
      return mirrorMsg;
    }

export function applyPendingUpdates(message) {
      const pending = pendingStandardUpdates.get(message.id);
      if (!pending || pending.length === 0) return;
      pending.forEach(update => applyUpdateToStandardMessage(message, update));
      pendingStandardUpdates.delete(message.id);
    }

export function normalizeMessageContentForDedup(content) {
      if (!content) return '';
      return String(content)
        .replace(/^#{1,6}\s*/gm, '')
        .replace(/\s+/g, ' ')
        .trim()
        .toLowerCase();
    }

export function findEquivalentMessage(messages, webviewMsg, windowMs) {
      const normalized = normalizeMessageContentForDedup(webviewMsg.content || '');
      if (!normalized) return null;
      const ts = webviewMsg.timestamp || Date.now();
      const targetType = webviewMsg.messageType || '';
      const maxWindow = typeof windowMs === 'number' ? windowMs : 5000;
      for (let i = messages.length - 1; i >= 0; i -= 1) {
        const m = messages[i];
        if (!m) continue;
        if (m.source !== webviewMsg.source || m.role !== webviewMsg.role) continue;
        const existingType = m.messageType || '';
        if (targetType && existingType && targetType !== existingType) continue;
        const delta = Math.abs((m.timestamp || 0) - ts);
        if (delta > maxWindow && i < messages.length - 5) {
          break;
        }
        const existingNormalized = normalizeMessageContentForDedup(m.content || '');
        if (existingNormalized && existingNormalized === normalized) {
          return m;
        }
      }
      return null;
    }

export function isInternalJsonMessage(content) {
      if (!content || typeof content !== 'string') return false;
      const trimmed = content.trim();
      if (!trimmed.startsWith('{') || !trimmed.endsWith('}')) return false;
      try {
        const parsed = JSON.parse(trimmed);
        if (!parsed || typeof parsed !== 'object') return false;
        const hasAmbiguity = Object.prototype.hasOwnProperty.call(parsed, 'score')
          && Object.prototype.hasOwnProperty.call(parsed, 'isAmbiguous');
        const hasReview = Object.prototype.hasOwnProperty.call(parsed, 'status')
          && Object.prototype.hasOwnProperty.call(parsed, 'summary');
        const hasMessageEnvelope = typeof parsed.type === 'string'
          && parsed.message && (parsed.message.role || parsed.message.content);
        const hasToolTrace = Array.isArray(parsed.message?.content)
          && parsed.message.content.some(item =>
            item && typeof item === 'object' &&
            (item.type === 'tool_result' || item.type === 'tool_use' || item.tool_use_id)
          );
        const hasToolUseRefs = Object.prototype.hasOwnProperty.call(parsed, 'tool_use_id')
          || Object.prototype.hasOwnProperty.call(parsed, 'parent_tool_use_id');
        return hasAmbiguity || hasReview || hasMessageEnvelope || hasToolTrace || hasToolUseRefs;
      } catch {
        return false;
      }
    }


// ============================================
// 交互处理
// ============================================

export function handleClarificationAnswer(answerText, cancelled) {
      if (!window._pendingClarification) return;

      const clarificationData = window._pendingClarification;

      // 清除待处理状态
      window._pendingClarification = null;

      // 发送澄清回答到后端
      postMessage({
        type: 'answerClarification',
        answers: cancelled ? null : { _userResponse: answerText },
        additionalInfo: answerText
      });

      const input = document.getElementById('prompt-input');
      if (input) {
        input.placeholder = '描述你的任务... (可粘贴图片)';
      }

      if (!cancelled) {
        setLocalProcessingGrace(15000);
        setProcessingState(true);
      }
      saveWebviewState();
      renderMainContent();
    }

export function handleWorkerQuestionAnswer(answer, cancelled) {
      if (!window._pendingWorkerQuestion) return;

      // 清除待处理状态
      window._pendingWorkerQuestion = null;

      postMessage({
        type: 'answerWorkerQuestion',
        answer: cancelled ? null : (answer || '')
      });

      const input = document.getElementById('prompt-input');
      if (input) {
        input.placeholder = '描述你的任务... (可粘贴图片)';
      }

      if (!cancelled) {
        setProcessingState(true);
      }
      saveWebviewState();
      renderMainContent();
    }

export function handleQuestionAnswer(answer, cancelled) {
      const idx = threadMessages.findIndex(m => m.type === 'question_request' && m.isPending);
      if (idx === -1) return;
      threadMessages[idx].isPending = false;
      threadMessages[idx].answered = !cancelled;
      threadMessages[idx].answer = cancelled ? '' : (answer || '');
      threadMessages[idx].cancelled = !!cancelled;
      postMessage({ type: 'answerQuestions', answer: cancelled ? null : (answer || '') });
      const input = document.getElementById('prompt-input');
      if (input) {
        input.placeholder = '描述你的任务... (可粘贴图片)';
      }
      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }

export function handlePlanConfirmation(confirmed) {
      // 发送确认消息到后端
      postMessage({ type: 'confirmPlan', confirmed: confirmed });

      // 🔧 风险1修复：确认后立即设置处理状态
      if (confirmed) {
        setProcessingState(true);
      }

      // 更新消息状态
      const confirmIdx = threadMessages.findIndex(m => m.type === 'plan_confirmation' && m.isPending);
      if (confirmIdx !== -1) {
        threadMessages[confirmIdx].isPending = false;
        threadMessages[confirmIdx].confirmed = confirmed;
      }

      saveWebviewState();
      renderMainContent();
    }

export function showClarificationAsMessage(questions, context, ambiguityScore, originalPrompt) {
      const normalized = Array.isArray(questions) ? questions.filter(q => q && String(q).trim()) : [];
      if (normalized.length === 0) return;

      // 构建澄清消息内容（Markdown 格式）
      let content = '';
      if (context) {
        content += context + '\n\n';
      }
      content += '为了更好地理解您的需求，请帮我澄清以下问题：\n\n';
      normalized.forEach((q, i) => {
        content += `${i + 1}. ${q}\n`;
      });
      content += '\n请在下方输入框中回复您的补充信息。';

      // 作为普通 assistant 消息添加
      threadMessages.push({
        role: 'assistant',
        content: content,
        time: new Date().toLocaleTimeString().slice(0, 5),
        timestamp: Date.now(),
        source: 'orchestrator',
        agent: 'claude',
        // 标记这是澄清消息，用于后续识别
        isClarification: true,
        clarificationData: {
          questions: normalized,
          context: context,
          ambiguityScore: ambiguityScore,
          originalPrompt: originalPrompt
        }
      });

      // 设置等待澄清状态
      window._pendingClarification = {
        questions: normalized,
        context: context,
        ambiguityScore: ambiguityScore,
        originalPrompt: originalPrompt,
        timestamp: Date.now()
      };

      // 🔧 修复：清除处理宽限期并进入等待状态
      clearLocalProcessingGrace();
      enterAwaitingState({
        type: 'clarification',
        prompt: '请输入补充信息...'
      });

      saveWebviewState();
      renderMainContent();
      smoothScrollToBottom();
    }


// ============================================
// 会话管理
// ============================================

export function loadSessionMessages(sessionId) {
      const session = sessions.find(s => s.id === sessionId);
      if (!session) return;
      const sessionMessages = Array.isArray(session.messages) ? session.messages : [];

      // 🔧 重要：切换会话时必须重置增量更新状态，否则 UI 不会刷新
      resetIncrementalState();

      // 转换消息格式：后端存储的是 SessionMessage，前端需要简化格式
      // 注意：必须保留所有特殊字段（toolCalls、parsedBlocks 等）以正确渲染
      const convertedMessages = sessionMessages.map(m => ({
        role: m.role,
        content: m.content,
        time: m.timestamp ? new Date(m.timestamp).toLocaleTimeString().slice(0,5) : '',
        timestamp: m.timestamp,
        agent: m.agent,
        source: m.source,
        images: m.images,
        // 保留特殊内容字段
        toolCalls: m.toolCalls,
        parsedBlocks: m.parsedBlocks,
        thinking: m.thinking,
        thinkingContent: m.thinkingContent
      }));

      // 清空并重新填充 threadMessages
      threadMessages.length = 0;
      threadMessages.push(...convertedMessages);

      // 清空 agentOutputs
      agentOutputs.claude = [];
      agentOutputs.codex = [];
      agentOutputs.gemini = [];

      // 恢复 agentOutputs
      if (session.agentOutputs) {
        ['claude', 'codex', 'gemini'].forEach(agent => {
          if (Array.isArray(session.agentOutputs[agent])) {
            agentOutputs[agent] = session.agentOutputs[agent];
          }
        });
      }

      saveWebviewState();
      renderMainContent();

      // 更新会话选择器显示
      renderSessionList();
    }

export function trimMessageLists() {
      // 裁剪 threadMessages，保留最新的消息
      if (threadMessages.length > MAX_THREAD_MESSAGES) {
        const trimmed = threadMessages.slice(-MAX_THREAD_MESSAGES);
        threadMessages.length = 0;
        threadMessages.push(...trimmed);
      }
      // 裁剪 agentOutputs
      ['claude', 'codex', 'gemini'].forEach(agent => {
        if (agentOutputs[agent] && agentOutputs[agent].length > MAX_AGENT_MESSAGES) {
          agentOutputs[agent] = agentOutputs[agent].slice(-MAX_AGENT_MESSAGES);
        }
      });
    }


// ============================================
// 系统消息
// ============================================

export function addSystemMessage(message, type = 'info') {
      const icons = {
        success: '<svg viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zm-3.97-3.03a.75.75 0 0 0-1.08.022L7.477 9.417 5.384 7.323a.75.75 0 0 0-1.06 1.06L6.97 11.03a.75.75 0 0 0 1.079-.02l3.992-4.99a.75.75 0 0 0-.01-1.05z"/></svg>',
        error: '<svg viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zM5.354 4.646a.5.5 0 1 0-.708.708L7.293 8l-2.647 2.646a.5.5 0 0 0 .708.708L8 8.707l2.646 2.647a.5.5 0 0 0 .708-.708L8.707 8l2.647-2.646a.5.5 0 0 0-.708-.708L8 7.293 5.354 4.646z"/></svg>',
        warning: '<svg viewBox="0 0 16 16"><path d="M8.982 1.566a1.13 1.13 0 0 0-1.96 0L.165 13.233c-.457.778.091 1.767.98 1.767h13.713c.889 0 1.438-.99.98-1.767L8.982 1.566zM8 5c.535 0 .954.462.9.995l-.35 3.507a.552.552 0 0 1-1.1 0L7.1 5.995A.905.905 0 0 1 8 5zm.002 6a1 1 0 1 1 0 2 1 1 0 0 1 0-2z"/></svg>',
        info: '<svg viewBox="0 0 16 16"><path d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16zm.93-9.412-1 4.705c-.07.34.029.533.304.533.194 0 .487-.07.686-.246l-.088.416c-.287.346-.92.598-1.465.598-.703 0-1.002-.422-.808-1.319l.738-3.468c.064-.293.006-.399-.287-.47l-.451-.081.082-.381 2.29-.287zM8 5.5a1 1 0 1 1 0-2 1 1 0 0 1 0 2z"/></svg>'
      };
      const now = new Date();
      const time = now.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });

      const last = threadMessages.length > 0 ? threadMessages[threadMessages.length - 1] : null;
      if (last && last.type === 'system_notice' && String(last.content || '') === String(message || '')) {
        return;
      }

      // 🔧 修复：使用 threadMessages 而不是 messages
      threadMessages.push({
        type: 'system_notice',
        noticeType: type,
        content: message,
        time: time,
        timestamp: Date.now()
      });

      // 渲染并滚动
      renderMainContent();
      smoothScrollToBottom();
      throttledSaveState();
    }

export function showToast(message, type = 'info', duration = 3000) {
      const container = document.getElementById('toast-container');
      const toast = document.createElement('div');
      toast.className = 'toast';
      const icons = {
        success: '<svg class="toast-icon success" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zm-3.97-3.03a.75.75 0 0 0-1.08.022L7.477 9.417 5.384 7.323a.75.75 0 0 0-1.06 1.06L6.97 11.03a.75.75 0 0 0 1.079-.02l3.992-4.99a.75.75 0 0 0-.01-1.05z"/></svg>',
        error: '<svg class="toast-icon error" viewBox="0 0 16 16"><path d="M16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0zM5.354 4.646a.5.5 0 1 0-.708.708L7.293 8l-2.647 2.646a.5.5 0 0 0 .708.708L8 8.707l2.646 2.647a.5.5 0 0 0 .708-.708L8.707 8l2.647-2.646a.5.5 0 0 0-.708-.708L8 7.293 5.354 4.646z"/></svg>',
        warning: '<svg class="toast-icon warning" viewBox="0 0 16 16"><path d="M8.982 1.566a1.13 1.13 0 0 0-1.96 0L.165 13.233c-.457.778.091 1.767.98 1.767h13.713c.889 0 1.438-.99.98-1.767L8.982 1.566zM8 5c.535 0 .954.462.9.995l-.35 3.507a.552.552 0 0 1-1.1 0L7.1 5.995A.905.905 0 0 1 8 5zm.002 6a1 1 0 1 1 0 2 1 1 0 0 1 0-2z"/></svg>',
        info: '<svg class="toast-icon info" viewBox="0 0 16 16"><path d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16zm.93-9.412-1 4.705c-.07.34.029.533.304.533.194 0 .487-.07.686-.246l-.088.416c-.287.346-.92.598-1.465.598-.703 0-1.002-.422-.808-1.319l.738-3.468c.064-.293.006-.399-.287-.47l-.451-.081.082-.381 2.29-.287zM8 5.5a1 1 0 1 1 0-2 1 1 0 0 1 0 2z"/></svg>'
      };
      toast.innerHTML = icons[type] + '<span class="toast-message">' + escapeHtml(message) + '</span><button class="toast-close" onclick="this.parentElement.remove()">×</button>';
      container.appendChild(toast);
      setTimeout(() => { toast.classList.add('hiding'); setTimeout(() => toast.remove(), 200); }, duration);
    }


// ============================================
// Prompt 增强
// ============================================

export function handlePromptEnhanced(enhancedPrompt, error) {
      const btn = document.getElementById('enhance-btn');
      const textSpan = btn.querySelector('.enhance-text');
      // 恢复按钮状态
      btn.classList.remove('loading');
      btn.disabled = false;
      if (textSpan) textSpan.textContent = '增强';

      if (error) {
        addSystemMessage('增强失败: ' + error, 'error');
        return;
      }

      if (enhancedPrompt) {
        const input = document.getElementById('prompt-input');
        input.value = enhancedPrompt;
        input.style.height = 'auto';
        input.style.height = Math.min(input.scrollHeight, 200) + 'px';
        addSystemMessage('Prompt 已增强', 'success');
      }
    }

export function updatePromptEnhanceStatus(success, message) {
      const promptEnhanceStatus = document.getElementById('prompt-enhance-status');
      const promptEnhanceTest = document.getElementById('prompt-enhance-test');

      if (promptEnhanceTest) {
        promptEnhanceTest.classList.remove('loading');
        promptEnhanceTest.disabled = false;

        if (success) {
          promptEnhanceTest.classList.add('success');
          promptEnhanceTest.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>连接成功';
        } else {
          promptEnhanceTest.classList.add('error');
          promptEnhanceTest.innerHTML = '<svg viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 13A6 6 0 1 1 8 2a6 6 0 0 1 0 12zm.75-8.25a.75.75 0 0 0-1.5 0v3.5a.75.75 0 0 0 1.5 0v-3.5zM8 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2z"/></svg>连接失败';
        }

        setTimeout(() => {
          promptEnhanceTest.classList.remove('success', 'error');
          promptEnhanceTest.innerHTML = '<svg viewBox="0 0 16 16"><path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425a.267.267 0 0 1 .02-.022z"/></svg>测试连接';
        }, 2000);
      }

      if (promptEnhanceStatus) {
        const hasMessage = !!message && message.trim().length > 0;
        const normalized = hasMessage ? message.trim() : '';
        const showDetail = hasMessage && normalized !== '连接成功' && normalized !== '连接失败';
        promptEnhanceStatus.style.display = showDetail ? 'block' : 'none';
        if (showDetail) {
          promptEnhanceStatus.className = 'prompt-enhance-status ' + (success ? 'success' : 'error');
          promptEnhanceStatus.textContent = normalized;
        } else {
          promptEnhanceStatus.textContent = '';
        }
      }
    }
