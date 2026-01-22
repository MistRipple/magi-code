#!/usr/bin/env node

/**
 * 检查所有 JavaScript 文件中可能未定义的变量引用
 * 通过静态分析查找可能的 ReferenceError
 */

const fs = require('fs');
const path = require('path');

const jsFiles = [
  'src/ui/webview/js/core/state.js',
  'src/ui/webview/js/core/utils.js',
  'src/ui/webview/js/core/vscode-api.js',
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',
  'src/ui/webview/js/main.js'
];

console.log('🔍 检查可能未定义的变量引用...\n');

let hasIssues = false;

jsFiles.forEach(file => {
  const content = fs.readFileSync(file, 'utf-8');
  const lines = content.split('\n');

  // 提取所有导入的名称
  const imports = new Set();
  const importRegex = /import\s*{([^}]+)}\s*from/g;
  let match;
  while ((match = importRegex.exec(content)) !== null) {
    const names = match[1].split(',').map(n => n.trim());
    names.forEach(name => imports.add(name));
  }

  // 提取所有导出的函数和变量
  const exports = new Set();
  const exportRegex = /export\s+(function|const|let|var)\s+(\w+)/g;
  while ((match = exportRegex.exec(content)) !== null) {
    exports.add(match[2]);
  }

  // 提取所有局部定义的变量和函数
  const locals = new Set();
  const localRegex = /(?:^|\s)(function|const|let|var)\s+(\w+)/gm;
  while ((match = localRegex.exec(content)) !== null) {
    locals.add(match[2]);
  }

  // 检查常见的全局变量使用
  const globalVars = [
    'document', 'window', 'console', 'setTimeout', 'setInterval',
    'clearTimeout', 'clearInterval', 'Date', 'Math', 'JSON',
    'acquireVsCodeApi', 'vscode', 'Array', 'Object', 'String',
    'Number', 'Boolean', 'RegExp', 'Error', 'Promise'
  ];

  // 合并所有已知的标识符
  const knownIdentifiers = new Set([...imports, ...exports, ...locals, ...globalVars]);

  // 检查可能未定义的引用（简单启发式）
  const issues = [];

  // 检查常见的状态变量
  const stateVars = [
    'threadMessages', 'cliOutputs', 'sessions', 'currentSessionId',
    'isProcessing', 'thinkingStartAt', 'processingActor',
    'pendingChanges', 'attachedImages', 'currentTopTab', 'currentBottomTab',
    'scrollPositions', 'autoScrollEnabled', 'streamingHintTimer',
    'currentDependencyAnalysis', 'isDependencyPanelExpanded'
  ];

  stateVars.forEach(varName => {
    const regex = new RegExp(`\\b${varName}\\b`, 'g');
    if (regex.test(content) && !knownIdentifiers.has(varName)) {
      const lineNumbers = [];
      lines.forEach((line, idx) => {
        if (new RegExp(`\\b${varName}\\b`).test(line)) {
          lineNumbers.push(idx + 1);
        }
      });
      issues.push({
        variable: varName,
        lines: lineNumbers.slice(0, 3) // 只显示前3个
      });
    }
  });

  if (issues.length > 0) {
    hasIssues = true;
    console.log(`❌ ${file}`);
    issues.forEach(issue => {
      console.log(`   - ${issue.variable} 可能未导入 (行: ${issue.lines.join(', ')})`);
    });
    console.log('');
  } else {
    console.log(`✅ ${file}`);
  }
});

console.log('\n============================================================');
if (hasIssues) {
  console.log('⚠️  发现可能未导入的变量');
  console.log('请检查上述文件的导入语句');
} else {
  console.log('✅ 未发现明显的未导入变量');
}
console.log('============================================================');

process.exit(hasIssues ? 1 : 0);
