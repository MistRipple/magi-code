#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

console.log('开始提取 JavaScript...\n');

// 找到主 <script> 标签的位置（第二个 <script>，包含主要代码）
let scriptStart = -1;
let scriptEnd = -1;
let scriptCount = 0;

for (let i = 0; i < lines.length; i++) {
  if (lines[i].trim() === '<script>') {
    scriptCount++;
    if (scriptCount === 2) {
      scriptStart = i;
    }
  }
  if (lines[i].trim() === '</script>' && scriptStart !== -1 && scriptEnd === -1) {
    scriptEnd = i;
    break;
  }
}

console.log(`JavaScript 位置: 第 ${scriptStart + 1} 行 到 第 ${scriptEnd + 1} 行`);
console.log(`总行数: ${scriptEnd - scriptStart - 1} 行\n`);

// 提取 JavaScript 部分
const jsLines = lines.slice(scriptStart + 1, scriptEnd);
const jsContent = jsLines.join('\n');

// 创建目录
const dirs = [
  'src/ui/webview/js',
  'src/ui/webview/js/core',
  'src/ui/webview/js/ui'
];

dirs.forEach(dir => {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
});

// ============================================
// Phase 2.1: 提取状态管理 (state.js)
// ============================================

console.log('Phase 2.1: 提取状态管理...');

const stateJs = `// 全局状态管理
// 此文件包含所有全局状态变量和状态持久化逻辑

// VSCode API
export const vscode = typeof acquireVsCodeApi === 'function'
  ? acquireVsCodeApi()
  : {
      postMessage: () => {},
      getState: () => ({}),
      setState: () => {}
    };

// 从 VSCode 状态恢复
const previousState = vscode.getState() || {};

// Tab 状态
export let currentTopTab = 'thread';
export let currentBottomTab = 'thread';

// 消息状态
export let threadMessages = previousState.threadMessages || [];
export let cliOutputs = previousState.cliOutputs || { claude: [], codex: [], gemini: [] };

// 会话状态
export let sessions = previousState.sessions || [];
const injectedSessionId = '{{initialSessionId}}';
export let currentSessionId = previousState.currentSessionId || (injectedSessionId || null);

// 变更和任务状态
export let pendingChanges = previousState.pendingChanges || [];

// 处理状态
export let isProcessing = previousState.isProcessing || false;
export let thinkingStartAt = previousState.thinkingStartAt || null;
export let localProcessingUntil = 0;
export let streamingHintTimer = null;
export let processingActor = previousState.processingActor || { source: 'orchestrator', cli: 'claude' };

// 依赖分析状态
export let currentDependencyAnalysis = null;
export let isDependencyPanelExpanded = false;

// 滚动状态
export let scrollPositions = previousState.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
export let autoScrollEnabled = previousState.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
export let hasInitialRender = false;

// 消息列表限制
const MAX_THREAD_MESSAGES = 500;
const MAX_CLI_MESSAGES = 200;

// 裁剪消息列表
export function trimMessageLists() {
  if (threadMessages.length > MAX_THREAD_MESSAGES) {
    threadMessages = threadMessages.slice(-MAX_THREAD_MESSAGES);
  }
  ['claude', 'codex', 'gemini'].forEach(cli => {
    if (cliOutputs[cli] && cliOutputs[cli].length > MAX_CLI_MESSAGES) {
      cliOutputs[cli] = cliOutputs[cli].slice(-MAX_CLI_MESSAGES);
    }
  });
}

// 保存状态到 VSCode
export function saveWebviewState() {
  trimMessageLists();
  vscode.setState({
    currentTopTab,
    currentBottomTab,
    threadMessages,
    cliOutputs,
    sessions,
    currentSessionId,
    pendingChanges,
    isProcessing,
    thinkingStartAt,
    processingActor,
    scrollPositions,
    autoScrollEnabled
  });
}

// 状态更新函数
export function setCurrentTopTab(tab) {
  currentTopTab = tab;
  saveWebviewState();
}

export function setCurrentBottomTab(tab) {
  currentBottomTab = tab;
  saveWebviewState();
}

export function setCurrentSessionId(id) {
  currentSessionId = id;
  saveWebviewState();
}

export function setIsProcessing(value) {
  isProcessing = value;
  saveWebviewState();
}

export function setThinkingStartAt(value) {
  thinkingStartAt = value;
  saveWebviewState();
}

export function setProcessingActor(actor) {
  processingActor = actor;
  saveWebviewState();
}

export function addThreadMessage(message) {
  threadMessages.push(message);
  saveWebviewState();
}

export function addCliOutput(cli, message) {
  if (!cliOutputs[cli]) {
    cliOutputs[cli] = [];
  }
  cliOutputs[cli].push(message);
  saveWebviewState();
}

export function clearThreadMessages() {
  threadMessages = [];
  saveWebviewState();
}

export function clearCliOutputs() {
  cliOutputs = { claude: [], codex: [], gemini: [] };
  saveWebviewState();
}

// 处理宽限期
export function setLocalProcessingGrace(ms) {
  localProcessingUntil = Date.now() + ms;
}

export function hasLocalProcessingGrace() {
  return localProcessingUntil > 0 && Date.now() < localProcessingUntil;
}

// 滚动位置
export function saveScrollPosition() {
  const mainContent = document.getElementById('main-content');
  if (mainContent) {
    scrollPositions[currentBottomTab] = mainContent.scrollTop;
  }
}

export function setScrollPosition(tab, position) {
  scrollPositions[tab] = position;
  saveWebviewState();
}

export function setAutoScrollEnabled(tab, enabled) {
  autoScrollEnabled[tab] = enabled;
  saveWebviewState();
}
`;

fs.writeFileSync('src/ui/webview/js/core/state.js', stateJs, 'utf-8');
console.log('✅ js/core/state.js (状态管理)');
console.log(`   ${stateJs.split('\n').length} 行\n`);

// ============================================
// Phase 2.2: 提取工具函数 (utils.js)
// ============================================

console.log('Phase 2.2: 提取工具函数...');

const utilsJs = `// 工具函数集合
// 此文件包含所有通用工具函数

/**
 * HTML 转义
 */
export function escapeHtml(text) {
  if (text == null) return '';
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

/**
 * 格式化时间戳
 */
export function formatTimestamp(timestamp) {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

/**
 * 格式化经过时间
 */
export function formatElapsed(ms) {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const minutes = String(Math.floor(totalSec / 60)).padStart(2, '0');
  const seconds = String(totalSec % 60).padStart(2, '0');
  return \`\${minutes}:\${seconds}\`;
}

/**
 * 格式化相对时间（如"刚刚"、"5分钟前"）
 */
export function formatRelativeTime(timestamp) {
  if (!timestamp) return '';
  const now = Date.now();
  const diff = now - timestamp;

  if (diff < 60000) return '刚刚';
  if (diff < 3600000) {
    const mins = Math.floor(diff / 60000);
    return mins + ' 分钟前';
  }
  if (diff < 86400000) {
    const hours = Math.floor(diff / 3600000);
    return hours + ' 小时前';
  }
  if (diff < 604800000) {
    const days = Math.floor(diff / 86400000);
    return days + ' 天前';
  }
  // 超过一周显示具体日期
  return new Date(timestamp).toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' });
}

/**
 * 生成唯一 ID
 */
export function generateId() {
  return 'id-' + Math.random().toString(36).substr(2, 9) + '-' + Date.now();
}

/**
 * 平滑滚动到底部
 */
export function smoothScrollToBottom() {
  const mainContent = document.getElementById('main-content');
  if (mainContent) {
    mainContent.scrollTo({
      top: mainContent.scrollHeight,
      behavior: 'smooth'
    });
  }
}

/**
 * 检查消息是否需要折叠
 */
export function shouldCollapseMessage(content) {
  if (!content) return false;
  const lineCount = (content.match(/\\n/g) || []).length + 1;
  const charCount = content.length;
  return lineCount > 15 && charCount > 500;
}

/**
 * 切换消息展开/折叠状态
 */
export function toggleMessageExpand(btn) {
  const wrapper = btn.closest('.message-collapsible-wrapper');
  if (!wrapper) return;

  const contentEl = wrapper.querySelector('.message-content');
  if (!contentEl) return;

  const isCollapsed = contentEl.classList.contains('collapsed');
  if (isCollapsed) {
    contentEl.classList.remove('collapsed');
    contentEl.classList.add('expandable');
    btn.textContent = '收起';
  } else {
    contentEl.classList.add('collapsed');
    contentEl.classList.remove('expandable');
    btn.textContent = '展开更多';
  }
}

/**
 * 解析代码块语言标签
 */
export function parseCodeBlockMeta(langLine) {
  if (!langLine) return { lang: 'text', filepath: null };

  const colonMatch = langLine.match(/^(\\w+):(.+)$/);
  if (colonMatch) {
    return { lang: colonMatch[1], filepath: colonMatch[2].trim() };
  }

  const spaceMatch = langLine.match(/^(\\w+)\\s+(.+)$/);
  if (spaceMatch) {
    return { lang: spaceMatch[1], filepath: spaceMatch[2].trim() };
  }

  return { lang: langLine || 'text', filepath: null };
}

/**
 * 检查是否应该渲染为代码块
 */
export function shouldRenderAsCodeBlock(content) {
  if (!content) return false;
  const trimmed = content.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith('\`\`\`')) return false;
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return true;
  if (!content.includes('\\n')) return false;

  // 特殊行号格式
  if (/^\\s*\\d+→/m.test(content)) return true;
  if (/^\\s*\\d+\\s*[:>]/m.test(content)) return true;

  // 检测缩进代码
  const lines = content.split('\\n');
  const indentedLines = lines.filter(l => /^\\s{2,}|^\\t/.test(l) && l.trim());
  return indentedLines.length >= 3;
}

/**
 * 提取单个代码块
 */
export function extractSingleCodeFence(content) {
  if (!content) return null;
  const trimmed = content.trim();
  const match = trimmed.match(/^\`\`\`(\\w*)(?::([^\\s\\n]+)|\\s+([^\\n]+))?\\s*\\n([\\s\\S]*?)\\n?\`\`\`\\s*$/);
  if (!match) return null;
  const lang = match[1] || '';
  const filepath = match[2] || match[3] || undefined;
  const body = match[4] || '';
  return { lang: lang, body: body, filepath: filepath };
}

/**
 * 显示 Toast 通知
 */
export function showToast(message, type = 'info') {
  const container = document.getElementById('toast-container');
  if (!container) return;

  const toast = document.createElement('div');
  toast.className = \`toast toast-\${type}\`;
  toast.textContent = message;

  container.appendChild(toast);

  setTimeout(() => {
    toast.classList.add('fade-out');
    setTimeout(() => toast.remove(), 300);
  }, 3000);
}
`;

fs.writeFileSync('src/ui/webview/js/core/utils.js', utilsJs, 'utf-8');
console.log('✅ js/core/utils.js (工具函数)');
console.log(`   ${utilsJs.split('\n').length} 行\n`);

// ============================================
// Phase 2.3: 提取 VSCode API 封装 (vscode-api.js)
// ============================================

console.log('Phase 2.3: 提取 VSCode API 封装...');

const vscodeApiJs = `// VSCode API 通信封装
// 此文件封装所有与 VSCode Extension 的通信逻辑

import { vscode } from './state.js';

/**
 * 发送消息到 Extension
 */
export function postMessage(message) {
  vscode.postMessage(message);
}

/**
 * 执行任务
 */
export function executeTask(prompt, images = null, mode = 'agent', cli = null) {
  postMessage({
    type: 'executeTask',
    prompt,
    images,
    mode,
    cli
  });
}

/**
 * 中断任务
 */
export function interruptTask() {
  postMessage({ type: 'interrupt' });
}

/**
 * 新建会话
 */
export function createNewSession() {
  postMessage({ type: 'newSession' });
}

/**
 * 切换会话
 */
export function switchSession(sessionId) {
  postMessage({ type: 'switchSession', sessionId });
}

/**
 * 删除会话
 */
export function deleteSession(sessionId) {
  postMessage({ type: 'deleteSession', sessionId });
}

/**
 * 重命名会话
 */
export function renameSession(sessionId, newName) {
  postMessage({ type: 'renameSession', sessionId, newName });
}

/**
 * 确认计划
 */
export function confirmPlan(confirmed) {
  postMessage({ type: 'confirmPlan', confirmed });
}

/**
 * 回答问题
 */
export function answerQuestions(answer) {
  postMessage({ type: 'answerQuestions', answer });
}

/**
 * 回答澄清问题
 */
export function answerClarification(answers, additionalInfo) {
  postMessage({ type: 'answerClarification', answers, additionalInfo });
}

/**
 * 回答 Worker 问题
 */
export function answerWorkerQuestion(answer) {
  postMessage({ type: 'answerWorkerQuestion', answer });
}

/**
 * 打开文件
 */
export function openFile(filepath) {
  postMessage({ type: 'openFile', filepath });
}

/**
 * 应用变更
 */
export function applyChange(changeId) {
  postMessage({ type: 'applyChange', changeId });
}

/**
 * 拒绝变更
 */
export function rejectChange(changeId) {
  postMessage({ type: 'rejectChange', changeId });
}

/**
 * 获取 Profile 配置
 */
export function getProfileConfig() {
  postMessage({ type: 'getProfileConfig' });
}

/**
 * 保存 Profile 配置
 */
export function saveProfileConfig(config) {
  postMessage({ type: 'saveProfileConfig', config });
}

/**
 * 重置 Profile 配置
 */
export function resetProfileConfig() {
  postMessage({ type: 'resetProfileConfig' });
}

/**
 * 增强提示词
 */
export function enhancePrompt(prompt) {
  postMessage({ type: 'enhancePrompt', prompt });
}

/**
 * 刷新 CLI 连接状态
 */
export function refreshCliConnections() {
  postMessage({ type: 'refreshCliConnections' });
}

/**
 * 重置执行统计
 */
export function resetExecutionStats() {
  postMessage({ type: 'resetExecutionStats' });
}
`;

fs.writeFileSync('src/ui/webview/js/core/vscode-api.js', vscodeApiJs, 'utf-8');
console.log('✅ js/core/vscode-api.js (VSCode 通信)');
console.log(`   ${vscodeApiJs.split('\n').length} 行\n`);

console.log('Phase 2 完成！\n');
console.log('已创建文件:');
console.log('  - js/core/state.js (状态管理)');
console.log('  - js/core/utils.js (工具函数)');
console.log('  - js/core/vscode-api.js (VSCode 通信)');
console.log();
console.log('下一步: Phase 3 - 提取 UI 模块');
