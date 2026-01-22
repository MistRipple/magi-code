#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

console.log('开始提取 event-handlers.js...\n');

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

// 查找所有独立的事件处理函数（不是内联的）
// 这些函数通常在 addEventListener 之前定义
const eventHandlerFunctions = [
  // Tab 切换
  'switchTopTab',
  'switchBottomTab',
  'switchSettingsTab',

  // 输入处理
  'handlePromptSubmit',
  'handlePromptKeydown',
  'handleImagePaste',
  'handleImageUpload',
  'handleDragOver',
  'handleDragLeave',
  'handleDrop',

  // 按钮点击
  'handleExecuteClick',
  'handleEnhanceClick',
  'handleAttachImageClick',
  'handleInterruptClick',
  'handleClearClick',

  // 会话管理
  'handleSessionSelect',
  'handleNewSession',
  'handleRenameSession',
  'handleDeleteSession',
  'handleExportSession',

  // 设置面板
  'handleProfileSave',
  'handleProfileReset',
  'handleLLMConfigSave',
  'handleLLMConfigTest',

  // MCP 管理
  'handleMCPAdd',
  'handleMCPEdit',
  'handleMCPDelete',
  'handleMCPToggle',

  // Skill 管理
  'handleSkillAdd',
  'handleSkillEdit',
  'handleSkillDelete',
  'handleSkillToggle',

  // 变更管理
  'handleApproveChange',
  'handleRevertChange',
  'handleApproveAll',
  'handleRevertAll',
  'handleViewDiff',

  // 其他
  'handleVisibilityChange',
  'handleWindowMessage',
  'handleUnhandledRejection',
  'initializeEventListeners'
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

console.log('提取事件处理函数...\n');

const extractedFunctions = [];
let totalLines = 0;

eventHandlerFunctions.forEach(funcName => {
  const func = extractFunction(funcName);
  if (func) {
    extractedFunctions.push(func);
    totalLines += func.lines;
    console.log(`✅ ${funcName}() - ${func.lines} 行`);
  }
});

console.log(`\n总计: ${extractedFunctions.length} 个函数, ${totalLines} 行\n`);

// 生成 event-handlers.js
console.log('生成 event-handlers.js...\n');

let handlerCode = `// 事件处理模块
// 此文件包含所有用户交互事件的处理函数

import {
  threadMessages,
  cliOutputs,
  currentSessionId,
  currentTopTab,
  currentBottomTab,
  isProcessing,
  sessions,
  pendingChanges,
  saveWebviewState
} from '../core/state.js';

import {
  escapeHtml,
  formatTimestamp
} from '../core/utils.js';

import {
  postMessage,
  executeTask,
  interruptTask,
  confirmPlan,
  answerQuestion
} from '../core/vscode-api.js';

import {
  renderMainContent,
  scheduleRenderMainContent
} from './message-renderer.js';

import {
  handleStandardMessage,
  handleStandardUpdate,
  handleStandardComplete,
  loadSessionMessages,
  showToast
} from './message-handler.js';

// ============================================
// 事件处理函数
// ============================================

`;

// 按功能分组添加函数
const groups = {
  'Tab 切换': [
    'switchTopTab',
    'switchBottomTab',
    'switchSettingsTab'
  ],
  '输入处理': [
    'handlePromptSubmit',
    'handlePromptKeydown',
    'handleImagePaste',
    'handleImageUpload',
    'handleDragOver',
    'handleDragLeave',
    'handleDrop'
  ],
  '按钮点击': [
    'handleExecuteClick',
    'handleEnhanceClick',
    'handleAttachImageClick',
    'handleInterruptClick',
    'handleClearClick'
  ],
  '会话管理': [
    'handleSessionSelect',
    'handleNewSession',
    'handleRenameSession',
    'handleDeleteSession',
    'handleExportSession'
  ],
  '设置面板': [
    'handleProfileSave',
    'handleProfileReset',
    'handleLLMConfigSave',
    'handleLLMConfigTest'
  ],
  'MCP 管理': [
    'handleMCPAdd',
    'handleMCPEdit',
    'handleMCPDelete',
    'handleMCPToggle'
  ],
  'Skill 管理': [
    'handleSkillAdd',
    'handleSkillEdit',
    'handleSkillDelete',
    'handleSkillToggle'
  ],
  '变更管理': [
    'handleApproveChange',
    'handleRevertChange',
    'handleApproveAll',
    'handleRevertAll',
    'handleViewDiff'
  ],
  '系统事件': [
    'handleVisibilityChange',
    'handleWindowMessage',
    'handleUnhandledRejection'
  ],
  '初始化': [
    'initializeEventListeners'
  ]
};

Object.keys(groups).forEach(groupName => {
  handlerCode += `\n// ============================================\n`;
  handlerCode += `// ${groupName}\n`;
  handlerCode += `// ============================================\n\n`;

  groups[groupName].forEach(funcName => {
    const func = extractedFunctions.find(f => f.name === funcName);
    if (func) {
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
  handlerCode += `// 其他事件处理函数\n`;
  handlerCode += `// ============================================\n\n`;

  ungroupedFuncs.forEach(func => {
    let code = func.code.replace(/^function\s+/, 'export function ');
    handlerCode += code + '\n\n';
  });
}

// 写入文件
const outputPath = 'src/ui/webview/js/ui/event-handlers.js';
const dir = path.dirname(outputPath);
if (!fs.existsSync(dir)) {
  fs.mkdirSync(dir, { recursive: true });
}

fs.writeFileSync(outputPath, handlerCode, 'utf-8');

const finalLines = handlerCode.split('\n').length;
console.log(`✅ 已创建: ${outputPath}`);
console.log(`   ${finalLines} 行代码\n`);

console.log('event-handlers.js 提取完成！');
console.log('\n⚠️  注意: 大部分事件处理是内联匿名函数，需要手动重构');
console.log('建议: 将内联事件处理逻辑提取为独立函数，然后在 addEventListener 中调用');
