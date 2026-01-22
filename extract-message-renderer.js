#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

console.log('开始提取 message-renderer.js...\n');

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

// 渲染相关函数列表（从分析中得出）
const renderFunctions = [
  'renderMainContent',
  'renderThreadView',
  'renderCliOutputView',
  'renderMessageList',
  'renderMessageBlock',
  'renderUnifiedCard',
  'renderSpecialMessage',
  'renderMarkdown',
  'renderCodeBlock',
  'renderParsedBlocks',
  'renderMessageContentSmart',
  'renderSubTaskSummaryCard',
  'renderSummaryCard',
  'renderToolCallItem',
  'renderToolTrack',
  'renderStructuredPlanContent',
  'renderToolPanelContent',
  'renderSummarySections',
  'renderSessionList',
  'renderImagePreviews',
  'renderDependencyPanel',
  'renderStreamingAnimation',
  'renderStreamingAnimationForCli',
  'renderWorkerStatusCard',
  'renderTaskCard',
  'renderTaskProgress',
  'renderSubtaskStatusList',
  'renderSystemNotice',
  'renderErrorMessage',
  'renderPlanPreviewCard',
  'renderPlanConfirmationCard',
  'renderQuestionCard',
  'renderCliQuestionCard',
  'renderTasksView',
  'renderEditsView',
  'renderProfileTags',
  'renderMCPServerList',
  'renderMCPTools',
  'renderRepositoryManagementList',
  'renderSkillsToolList',
  'renderSkillLibrary',
  // 辅助渲染函数
  'parseCodeBlockMeta',
  'shouldRenderAsCodeBlock',
  'extractSingleCodeFence',
  'formatPlanHtml',
  'shouldCollapseMessage',
  'toggleMessageExpand'
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

console.log('提取渲染函数...\n');

const extractedFunctions = [];
let totalLines = 0;

renderFunctions.forEach(funcName => {
  const func = extractFunction(funcName);
  if (func) {
    extractedFunctions.push(func);
    totalLines += func.lines;
    console.log(`✅ ${funcName}() - ${func.lines} 行`);
  }
});

console.log(`\n总计: ${extractedFunctions.length} 个函数, ${totalLines} 行\n`);

// 生成 message-renderer.js
console.log('生成 message-renderer.js...\n');

let rendererCode = `// 消息渲染模块
// 此文件包含所有消息和 UI 渲染相关的函数

import {
  threadMessages,
  cliOutputs,
  currentBottomTab,
  currentSessionId,
  isProcessing,
  thinkingStartAt,
  processingActor,
  scrollPositions,
  autoScrollEnabled,
  pendingChanges,
  currentDependencyAnalysis,
  isDependencyPanelExpanded,
  saveScrollPosition
} from '../core/state.js';

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
// 渲染函数
// ============================================

`;

// 按功能分组添加函数
const groups = {
  '主渲染函数': ['renderMainContent', 'renderThreadView', 'renderCliOutputView'],
  '消息渲染': ['renderMessageList', 'renderMessageBlock', 'renderUnifiedCard', 'renderSpecialMessage', 'renderMessageContentSmart'],
  'Markdown和代码': ['renderMarkdown', 'renderCodeBlock', 'renderParsedBlocks'],
  '卡片渲染': ['renderSubTaskSummaryCard', 'renderSummaryCard', 'renderToolCallItem', 'renderToolTrack', 'renderStructuredPlanContent'],
  '特殊视图': ['renderSessionList', 'renderImagePreviews', 'renderDependencyPanel', 'renderStreamingAnimation', 'renderStreamingAnimationForCli'],
  '任务和状态': ['renderWorkerStatusCard', 'renderTaskCard', 'renderTaskProgress', 'renderSubtaskStatusList', 'renderSystemNotice', 'renderErrorMessage'],
  '计划和问题': ['renderPlanPreviewCard', 'renderPlanConfirmationCard', 'renderQuestionCard', 'renderCliQuestionCard'],
  '视图': ['renderTasksView', 'renderEditsView'],
  '设置和配置': ['renderProfileTags', 'renderMCPServerList', 'renderMCPTools', 'renderRepositoryManagementList', 'renderSkillsToolList', 'renderSkillLibrary'],
  '辅助函数': ['renderToolPanelContent', 'renderSummarySections', 'formatPlanHtml']
};

Object.keys(groups).forEach(groupName => {
  rendererCode += `\n// ============================================\n`;
  rendererCode += `// ${groupName}\n`;
  rendererCode += `// ============================================\n\n`;

  groups[groupName].forEach(funcName => {
    const func = extractedFunctions.find(f => f.name === funcName);
    if (func) {
      // 转换为 export function
      let code = func.code.replace(/^function\s+/, 'export function ');
      rendererCode += code + '\n\n';
    }
  });
});

// 添加未分组的函数
const groupedFuncs = Object.values(groups).flat();
const ungroupedFuncs = extractedFunctions.filter(f => !groupedFuncs.includes(f.name));

if (ungroupedFuncs.length > 0) {
  rendererCode += `\n// ============================================\n`;
  rendererCode += `// 其他渲染函数\n`;
  rendererCode += `// ============================================\n\n`;

  ungroupedFuncs.forEach(func => {
    let code = func.code.replace(/^function\s+/, 'export function ');
    rendererCode += code + '\n\n';
  });
}

// 写入文件
const outputPath = 'src/ui/webview/js/ui/message-renderer.js';
const dir = path.dirname(outputPath);
if (!fs.existsSync(dir)) {
  fs.mkdirSync(dir, { recursive: true });
}

fs.writeFileSync(outputPath, rendererCode, 'utf-8');

const finalLines = rendererCode.split('\n').length;
console.log(`✅ 已创建: ${outputPath}`);
console.log(`   ${finalLines} 行代码\n`);

console.log('message-renderer.js 提取完成！');
