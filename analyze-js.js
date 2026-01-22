#!/usr/bin/env node

const fs = require('fs');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

console.log('分析 JavaScript 结构...\n');

// 找到 <script> 标签的位置
let scriptStart = -1;
let scriptEnd = -1;

for (let i = 0; i < lines.length; i++) {
  if (lines[i].trim().startsWith('<script>')) {
    scriptStart = i;
  }
  if (lines[i].trim().startsWith('</script>')) {
    scriptEnd = i;
    break;
  }
}

console.log(`<script> 标签位置: 第 ${scriptStart + 1} 行`);
console.log(`</script> 标签位置: 第 ${scriptEnd + 1} 行`);
console.log(`JavaScript 总行数: ${scriptEnd - scriptStart - 1} 行\n`);

// 提取 JavaScript 部分
const jsLines = lines.slice(scriptStart + 1, scriptEnd);

// 分析结构
console.log('分析代码结构...\n');

// 1. 全局变量声明
const globalVars = [];
jsLines.forEach((line, idx) => {
  const trimmed = line.trim();
  if (trimmed.startsWith('let ') || trimmed.startsWith('const ') || trimmed.startsWith('var ')) {
    // 检查是否在函数内部（简单判断：前面有 function 关键字）
    const prevLines = jsLines.slice(Math.max(0, idx - 20), idx).join('\n');
    if (!prevLines.includes('function ') && !prevLines.includes('=>')) {
      globalVars.push({
        line: scriptStart + idx + 2,
        code: trimmed.substring(0, 80)
      });
    }
  }
});

console.log(`全局变量声明: ${globalVars.length} 个`);
globalVars.slice(0, 10).forEach(v => {
  console.log(`  第 ${v.line} 行: ${v.code}`);
});
if (globalVars.length > 10) {
  console.log(`  ... 还有 ${globalVars.length - 10} 个`);
}
console.log();

// 2. 函数定义
const functions = [];
jsLines.forEach((line, idx) => {
  const trimmed = line.trim();
  if (trimmed.startsWith('function ')) {
    const match = trimmed.match(/function\s+(\w+)\s*\(/);
    if (match) {
      functions.push({
        line: scriptStart + idx + 2,
        name: match[1],
        code: trimmed.substring(0, 80)
      });
    }
  }
});

console.log(`函数定义: ${functions.length} 个`);
functions.slice(0, 15).forEach(f => {
  console.log(`  第 ${f.line} 行: ${f.name}()`);
});
if (functions.length > 15) {
  console.log(`  ... 还有 ${functions.length - 15} 个`);
}
console.log();

// 3. 事件监听器
const eventListeners = [];
jsLines.forEach((line, idx) => {
  const trimmed = line.trim();
  if (trimmed.includes('addEventListener') || trimmed.includes('.onclick')) {
    eventListeners.push({
      line: scriptStart + idx + 2,
      code: trimmed.substring(0, 80)
    });
  }
});

console.log(`事件监听器: ${eventListeners.length} 个`);
eventListeners.slice(0, 10).forEach(e => {
  console.log(`  第 ${e.line} 行: ${e.code}`);
});
if (eventListeners.length > 10) {
  console.log(`  ... 还有 ${eventListeners.length - 10} 个`);
}
console.log();

// 4. VSCode API 调用
const vscodeAPICalls = [];
jsLines.forEach((line, idx) => {
  const trimmed = line.trim();
  if (trimmed.includes('vscode.postMessage') || trimmed.includes('window.addEventListener(\'message\'')) {
    vscodeAPICalls.push({
      line: scriptStart + idx + 2,
      code: trimmed.substring(0, 80)
    });
  }
});

console.log(`VSCode API 调用: ${vscodeAPICalls.length} 个`);
vscodeAPICalls.slice(0, 10).forEach(v => {
  console.log(`  第 ${v.line} 行: ${v.code}`);
});
if (vscodeAPICalls.length > 10) {
  console.log(`  ... 还有 ${vscodeAPICalls.length - 10} 个`);
}
console.log();

// 5. 建议的模块划分
console.log('建议的模块划分:\n');

console.log('1. js/core/state.js - 状态管理');
console.log('   - sessions, currentSessionId, threadMessages');
console.log('   - repositories, skillsConfig');
console.log('   - cliOutputs, executionStats');
console.log('   - localStorage 持久化函数');
console.log();

console.log('2. js/core/vscode-api.js - VSCode 通信');
console.log('   - vscode.postMessage 封装');
console.log('   - window.addEventListener(\'message\') 处理');
console.log('   - 消息分发器');
console.log();

console.log('3. js/core/utils.js - 工具函数');
console.log('   - escapeHtml, formatTimestamp, generateId');
console.log('   - renderMarkdown, renderCodeBlock');
console.log('   - copyCodeBlock, openFileInEditor');
console.log();

console.log('4. js/ui/message-renderer.js - 消息渲染');
console.log('   - renderMainContent, renderMessage');
console.log('   - renderToolCallCard, renderTaskCard');
console.log('   - renderPlanCard, renderQuestionCard');
console.log();

console.log('5. js/ui/settings-panel.js - 设置面板');
console.log('   - Tab 切换逻辑');
console.log('   - Profile 配置管理');
console.log('   - LLM 配置管理');
console.log('   - 统计数据显示');
console.log();

console.log('6. js/ui/modal-*.js - 各种弹窗');
console.log('   - modal-mcp.js - MCP 服务器管理');
console.log('   - modal-repository.js - 仓库管理');
console.log('   - modal-skill.js - 技能库');
console.log();

console.log('7. js/main.js - 主入口');
console.log('   - 初始化所有模块');
console.log('   - 绑定全局事件');
console.log('   - 启动应用');
