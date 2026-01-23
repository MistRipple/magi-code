#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

console.log('开始提取 message-handler.js...\n');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

// 找到主 <script> 标签
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

const jsLines = lines.slice(scriptStart + 1, scriptEnd);
const jsContent = jsLines.join('\n');

// 消息处理相关函数列表（基于实际存在的函数）
const messageHandlerFunctions = [
  // 核心消息处理
  'handleStandardMessage',
  'handleStandardUpdate',
  'handleStandardComplete',
  'handleInteractionMessage',
  'updateStreamingMessage',
  'applyUpdateToStandardMessage',
  'standardToWebviewMessage',

  // 流式消息管理
  'findActiveStreamMessage',
  'ensureThreadStreamMessage',
  'updateAgentStreamingMessage',

  // 消息转换和规范化
  'upsertThreadMirrorFromWorker',
  'applyPendingUpdates',
  'normalizeMessageContentForDedup',
  'findEquivalentMessage',
  'isInternalJsonMessage',

  // 交互处理
  'handleClarificationAnswer',
  'handleWorkerQuestionAnswer',
  'handleQuestionAnswer',
  'handlePlanConfirmation',
  'showClarificationAsMessage',

  // 会话管理
  'loadSessionMessages',
  'trimMessageLists',

  // 系统消息
  'addSystemMessage',
  'showToast',

  // Prompt 增强
  'handlePromptEnhanced',
  'updatePromptEnhanceStatus'
];

// 提取函数代码
function extractFunction(funcName) {
  const regex = new RegExp(`function\\s+${funcName}\\s*\\([^)]*\\)\\s*\\{`, 'g');
  const match = regex.exec(jsContent);

  if (!match) {
    console.log(`⚠️  未找到函数: ${funcName}`);
    return null;
  }

  const startPos = match.index;
  let braceCount = 0;
  let inFunction = false;
  let endPos = startPos;

  // 找到函数结束位置（匹配大括号）
  for (let i = startPos; i < jsContent.length; i++) {
    const char = jsContent[i];
    if (char === '{') {
      braceCount++;
      inFunction = true;
    } else if (char === '}') {
      braceCount--;
      if (inFunction && braceCount === 0) {
        endPos = i + 1;
        break;
      }
    }
  }

  const funcCode = jsContent.substring(startPos, endPos);
  const lineCount = (funcCode.match(/\n/g) || []).length + 1;

  return {
    name: funcName,
    code: funcCode,
    lines: lineCount,
    startPos,
    endPos
  };
}

console.log('提取消息处理函数...\n');

const extractedFunctions = [];
let totalLines = 0;

messageHandlerFunctions.forEach(funcName => {
  const func = extractFunction(funcName);
  if (func) {
    extractedFunctions.push(func);
    totalLines += func.lines;
    console.log(`✅ ${funcName}() - ${func.lines} 行`);
  }
});

console.log(`\n总计: ${extractedFunctions.length} 个函数, ${totalLines} 行\n`);

// 生成 message-handler.js
console.log('生成 message-handler.js...\n');

let handlerCode = `// 消息处理模块
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
  saveWebviewState
} from '../core/state.js';

import {
  escapeHtml,
  formatTimestamp,
  formatElapsed,
  formatRelativeTime
} from '../core/utils.js';

import {
  postMessage
} from '../core/vscode-api.js';

import {
  renderMainContent,
  scheduleRenderMainContent,
  getRoleIcon,
  getRoleInfo,
  getMessageGroupKey
} from './message-renderer.js';

// ============================================
// 消息处理函数
// ============================================

`;

// 按功能分组添加函数
const groups = {
  '核心消息处理': [
    'handleStandardMessage',
    'handleStandardUpdate',
    'handleStandardComplete',
    'handleInteractionMessage',
    'applyUpdateToStandardMessage',
    'standardToWebviewMessage'
  ],
  '流式消息管理': [
    'updateStreamingMessage',
    'findActiveStreamMessage',
    'ensureThreadStreamMessage',
    'updateAgentStreamingMessage'
  ],
  '消息转换和规范化': [
    'upsertThreadMirrorFromWorker',
    'applyPendingUpdates',
    'normalizeMessageContentForDedup',
    'findEquivalentMessage',
    'isInternalJsonMessage'
  ],
  '交互处理': [
    'handleClarificationAnswer',
    'handleWorkerQuestionAnswer',
    'handleQuestionAnswer',
    'handlePlanConfirmation',
    'showClarificationAsMessage'
  ],
  '会话管理': [
    'loadSessionMessages',
    'trimMessageLists'
  ],
  '系统消息': [
    'addSystemMessage',
    'showToast'
  ],
  'Prompt 增强': [
    'handlePromptEnhanced',
    'updatePromptEnhanceStatus'
  ]
};

Object.keys(groups).forEach(groupName => {
  handlerCode += `\n// ============================================\n`;
  handlerCode += `// ${groupName}\n`;
  handlerCode += `// ============================================\n\n`;

  groups[groupName].forEach(funcName => {
    const func = extractedFunctions.find(f => f.name === funcName);
    if (func) {
      // 转换为 export function
      let code = func.code.replace(/^function\s+/, 'export function ');
      handlerCode += code + '\n\n';
    }
  });
});

// 添加未分组的函数
const groupedFuncs = Object.values(groups).flat();
const ungroupedFuncs = extractedFunctions.filter(f => !groupedFuncs.includes(f.name));

if (ungroupedFuncs.length > 0) {
  handlerCode += `\n// ============================================\n`;
  handlerCode += `// 其他消息处理函数\n`;
  handlerCode += `// ============================================\n\n`;

  ungroupedFuncs.forEach(func => {
    let code = func.code.replace(/^function\s+/, 'export function ');
    handlerCode += code + '\n\n';
  });
}

// 写入文件
const outputPath = 'src/ui/webview/js/ui/message-handler.js';
const dir = path.dirname(outputPath);
if (!fs.existsSync(dir)) {
  fs.mkdirSync(dir, { recursive: true });
}

fs.writeFileSync(outputPath, handlerCode, 'utf-8');

const finalLines = handlerCode.split('\n').length;
console.log(`✅ 已创建: ${outputPath}`);
console.log(`   ${finalLines} 行代码\n`);

console.log('message-handler.js 提取完成！');
