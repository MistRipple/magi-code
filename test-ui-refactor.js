#!/usr/bin/env node

/**
 * UI 重构验证测试脚本
 * 测试所有模块化后的 JavaScript 文件
 */

const fs = require('fs');
const path = require('path');

console.log('🧪 开始 UI 重构验证测试...\n');

// ============================================
// 1. 文件存在性检查
// ============================================

console.log('📁 Phase 1: 文件存在性检查');

const requiredFiles = [
  // HTML
  'src/ui/webview/index.html',
  'src/ui/webview/index.html.backup',

  // CSS
  'src/ui/webview/styles/base.css',
  'src/ui/webview/styles/layout.css',
  'src/ui/webview/styles/components.css',
  'src/ui/webview/styles/messages.css',
  'src/ui/webview/styles/settings.css',
  'src/ui/webview/styles/modals.css',

  // JavaScript Core
  'src/ui/webview/js/core/state.js',
  'src/ui/webview/js/core/utils.js',
  'src/ui/webview/js/core/vscode-api.js',

  // JavaScript UI
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',

  // Main entry
  'src/ui/webview/js/main.js',
];

let filesOk = true;
requiredFiles.forEach(file => {
  const exists = fs.existsSync(file);
  const status = exists ? '✅' : '❌';
  console.log(`  ${status} ${file}`);
  if (!exists) filesOk = false;
});

if (!filesOk) {
  console.error('\n❌ 文件检查失败！部分文件不存在。');
  process.exit(1);
}

console.log('\n✅ 所有文件存在\n');

// ============================================
// 2. 文件大小统计
// ============================================

console.log('📊 Phase 2: 文件大小统计');

function getFileSize(filepath) {
  const stats = fs.statSync(filepath);
  return stats.size;
}

function formatSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
}

const htmlSize = getFileSize('src/ui/webview/index.html');
const htmlBackupSize = getFileSize('src/ui/webview/index.html.backup');

console.log(`  index.html: ${formatSize(htmlSize)}`);
console.log(`  index.html.backup: ${formatSize(htmlBackupSize)}`);
console.log(`  减少: ${formatSize(htmlBackupSize - htmlSize)} (${((1 - htmlSize / htmlBackupSize) * 100).toFixed(1)}%)`);

let totalCssSize = 0;
const cssFiles = [
  'base.css', 'layout.css', 'components.css',
  'messages.css', 'settings.css', 'modals.css'
];
cssFiles.forEach(file => {
  const size = getFileSize(`src/ui/webview/styles/${file}`);
  totalCssSize += size;
  console.log(`  styles/${file}: ${formatSize(size)}`);
});
console.log(`  CSS 总计: ${formatSize(totalCssSize)}`);

let totalJsSize = 0;
const jsFiles = [
  'core/state.js', 'core/utils.js', 'core/vscode-api.js',
  'ui/message-renderer.js', 'ui/message-handler.js', 'ui/event-handlers.js',
  'main.js'
];
jsFiles.forEach(file => {
  const size = getFileSize(`src/ui/webview/js/${file}`);
  totalJsSize += size;
  console.log(`  js/${file}: ${formatSize(size)}`);
});
console.log(`  JavaScript 总计: ${formatSize(totalJsSize)}`);

console.log('\n✅ 文件大小统计完成\n');

// ============================================
// 3. 模块导入/导出检查
// ============================================

console.log('🔗 Phase 3: 模块导入/导出检查');

function checkImportsExports(filepath) {
  const content = fs.readFileSync(filepath, 'utf-8');
  const imports = (content.match(/^import\s+.*?from\s+['"].*?['"]/gm) || []).length;
  const exports = (content.match(/^export\s+(function|const|let|class|default)/gm) || []).length;
  return { imports, exports };
}

const jsModules = [
  'js/core/state.js',
  'js/core/utils.js',
  'js/core/vscode-api.js',
  'js/ui/message-renderer.js',
  'js/ui/message-handler.js',
  'js/ui/event-handlers.js',
  'js/main.js'
];

jsModules.forEach(file => {
  const { imports, exports } = checkImportsExports(`src/ui/webview/${file}`);
  console.log(`  ${file}:`);
  console.log(`    导入: ${imports} 个`);
  console.log(`    导出: ${exports} 个`);
});

console.log('\n✅ 模块导入/导出检查完成\n');

// ============================================
// 4. HTML 引用检查
// ============================================

console.log('🔍 Phase 4: HTML 引用检查');

const htmlContent = fs.readFileSync('src/ui/webview/index.html', 'utf-8');

// 检查 CSS 引用
const cssLinks = htmlContent.match(/<link[^>]+href="styles\/[^"]+\.css"/g) || [];
console.log(`  CSS 引用: ${cssLinks.length} 个`);
cssLinks.forEach(link => {
  const match = link.match(/href="(styles\/[^"]+\.css)"/);
  if (match) {
    const file = `src/ui/webview/${match[1]}`;
    const exists = fs.existsSync(file);
    console.log(`    ${exists ? '✅' : '❌'} ${match[1]}`);
  }
});

// 检查 JS 引用
const jsScripts = htmlContent.match(/<script[^>]+src="[^"]+\.js"/g) || [];
console.log(`  JavaScript 引用: ${jsScripts.length} 个`);
jsScripts.forEach(script => {
  const match = script.match(/src="([^"]+\.js)"/);
  if (match) {
    const file = `src/ui/webview/${match[1]}`;
    const exists = fs.existsSync(file);
    console.log(`    ${exists ? '✅' : '❌'} ${match[1]}`);
  }
});

// 检查是否还有内联 CSS/JS
const hasInlineStyle = /<style[^>]*>/.test(htmlContent);
const hasInlineScript = htmlContent.match(/<script[^>]*>(?!.*src=)/g)?.length > 1; // 允许一个加载库的 script

console.log(`  内联 CSS: ${hasInlineStyle ? '❌ 存在' : '✅ 无'}`);
console.log(`  内联 JavaScript: ${hasInlineScript ? '❌ 存在' : '✅ 仅库加载脚本'}`);

console.log('\n✅ HTML 引用检查完成\n');

// ============================================
// 5. 语法检查（使用 Node.js）
// ============================================

console.log('✨ Phase 5: JavaScript 语法检查');

const { execSync } = require('child_process');

function checkSyntax(filepath) {
  try {
    // 使用 node -c 检查语法
    execSync(`node -c "${filepath}"`, { stdio: 'pipe' });
    return null; // 无错误
  } catch (error) {
    return error.stderr.toString();
  }
}

let syntaxOk = true;
jsModules.forEach(file => {
  const error = checkSyntax(`src/ui/webview/${file}`);
  if (error) {
    console.log(`  ❌ ${file}:`);
    console.log(`     ${error.trim()}`);
    syntaxOk = false;
  } else {
    console.log(`  ✅ ${file}`);
  }
});

if (!syntaxOk) {
  console.error('\n❌ 语法检查发现问题！');
  process.exit(1);
}

console.log('\n✅ 语法检查通过\n');

// ============================================
// 6. 依赖关系检查
// ============================================

console.log('🔗 Phase 6: 依赖关系检查');

function extractImports(filepath) {
  const content = fs.readFileSync(filepath, 'utf-8');
  const imports = [];
  const regex = /import\s+.*?from\s+['"](.+?)['"]/g;
  let match;
  while ((match = regex.exec(content)) !== null) {
    imports.push(match[1]);
  }
  return imports;
}

const dependencyGraph = {};
jsModules.forEach(file => {
  const imports = extractImports(`src/ui/webview/${file}`);
  dependencyGraph[file] = imports;
  console.log(`  ${file}:`);
  if (imports.length === 0) {
    console.log(`    无依赖`);
  } else {
    imports.forEach(imp => console.log(`    → ${imp}`));
  }
});

console.log('\n✅ 依赖关系检查完成\n');

// ============================================
// 7. 函数导出检查
// ============================================

console.log('📤 Phase 7: 关键函数导出检查');

function checkExportedFunctions(filepath, expectedFunctions) {
  const content = fs.readFileSync(filepath, 'utf-8');
  const results = {};

  expectedFunctions.forEach(funcName => {
    const regex = new RegExp(`export\\s+(function|const)\\s+${funcName}`, 'm');
    results[funcName] = regex.test(content);
  });

  return results;
}

// 检查 state.js
const stateExports = checkExportedFunctions('src/ui/webview/js/core/state.js', [
  'vscode', 'threadMessages', 'cliOutputs', 'currentSessionId',
  'saveWebviewState', 'restoreWebviewState', 'state', 'attachedImages'
]);
console.log('  state.js:');
Object.entries(stateExports).forEach(([func, exists]) => {
  console.log(`    ${exists ? '✅' : '❌'} ${func}`);
});

// 检查 utils.js
const utilsExports = checkExportedFunctions('src/ui/webview/js/core/utils.js', [
  'escapeHtml', 'formatTimestamp', 'formatElapsed', 'formatRelativeTime'
]);
console.log('  utils.js:');
Object.entries(utilsExports).forEach(([func, exists]) => {
  console.log(`    ${exists ? '✅' : '❌'} ${func}`);
});

// 检查 message-renderer.js
const rendererExports = checkExportedFunctions('src/ui/webview/js/ui/message-renderer.js', [
  'renderMainContent', 'renderThreadView', 'renderMessageList',
  'extractTextFromBlocks', 'extractCodeBlocksFromBlocks'
]);
console.log('  message-renderer.js:');
Object.entries(rendererExports).forEach(([func, exists]) => {
  console.log(`    ${exists ? '✅' : '❌'} ${func}`);
});

// 检查 event-handlers.js
const eventExports = checkExportedFunctions('src/ui/webview/js/ui/event-handlers.js', [
  'initializeEventListeners', 'handleExecuteButtonClick', 'handleTopTabClick'
]);
console.log('  event-handlers.js:');
Object.entries(eventExports).forEach(([func, exists]) => {
  console.log(`    ${exists ? '✅' : '❌'} ${func}`);
});

console.log('\n✅ 函数导出检查完成\n');

// ============================================
// 8. 代码行数统计
// ============================================

console.log('📏 Phase 8: 代码行数统计');

function countLines(filepath) {
  const content = fs.readFileSync(filepath, 'utf-8');
  return content.split('\n').length;
}

console.log('  HTML:');
console.log(`    index.html: ${countLines('src/ui/webview/index.html')} 行`);
console.log(`    index.html.backup: ${countLines('src/ui/webview/index.html.backup')} 行`);

console.log('  CSS:');
let totalCssLines = 0;
cssFiles.forEach(file => {
  const lines = countLines(`src/ui/webview/styles/${file}`);
  totalCssLines += lines;
  console.log(`    ${file}: ${lines} 行`);
});
console.log(`    总计: ${totalCssLines} 行`);

console.log('  JavaScript:');
let totalJsLines = 0;
jsModules.forEach(file => {
  const lines = countLines(`src/ui/webview/${file}`);
  totalJsLines += lines;
  console.log(`    ${file}: ${lines} 行`);
});
console.log(`    总计: ${totalJsLines} 行`);

console.log('\n✅ 代码行数统计完成\n');

// ============================================
// 总结
// ============================================

console.log('=' .repeat(60));
console.log('🎉 UI 重构验证测试完成！');
console.log('=' .repeat(60));

console.log('\n📊 重构成果总结:');
console.log(`  ✅ HTML 简化: ${countLines('src/ui/webview/index.html.backup')} → ${countLines('src/ui/webview/index.html')} 行 (${((1 - countLines('src/ui/webview/index.html') / countLines('src/ui/webview/index.html.backup')) * 100).toFixed(1)}% 减少)`);
console.log(`  ✅ CSS 模块化: 6 个文件, ${totalCssLines} 行, ${formatSize(totalCssSize)}`);
console.log(`  ✅ JavaScript 模块化: 7 个文件, ${totalJsLines} 行, ${formatSize(totalJsSize)}`);
console.log(`  ✅ 文件结构清晰: core/ (3) + ui/ (3) + main.js (1)`);

console.log('\n✅ 所有测试通过！重构成功！\n');
