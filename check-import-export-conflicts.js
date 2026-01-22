#!/usr/bin/env node

/**
 * 检查导入和导出冲突
 * 如果一个模块从其他模块导入了某个函数，又在自己内部导出同名函数，就会冲突
 */

const fs = require('fs');

console.log('🔍 检查导入/导出冲突...\n');

const jsFiles = [
  'src/ui/webview/js/core/state.js',
  'src/ui/webview/js/core/utils.js',
  'src/ui/webview/js/core/vscode-api.js',
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',
  'src/ui/webview/js/main.js'
];

let hasConflicts = false;

jsFiles.forEach(file => {
  const content = fs.readFileSync(file, 'utf-8');
  
  // 提取导入的名称
  const imports = new Set();
  const importRegex = /import\s+\{([^}]+)\}\s+from/g;
  let match;
  
  while ((match = importRegex.exec(content)) !== null) {
    const names = match[1].split(',').map(n => n.trim());
    names.forEach(name => imports.add(name));
  }
  
  // 提取导出的名称
  const exports = new Set();
  const exportRegex = /export\s+(function|const|let|class)\s+(\w+)/g;
  
  while ((match = exportRegex.exec(content)) !== null) {
    exports.add(match[2]);
  }
  
  // 检查冲突
  const conflicts = [];
  imports.forEach(name => {
    if (exports.has(name)) {
      conflicts.push(name);
    }
  });
  
  if (conflicts.length > 0) {
    console.log(`❌ ${file}:`);
    console.log(`   导入又导出: ${conflicts.join(', ')}`);
    console.log(`   这些函数从其他模块导入，但又在本模块中重新定义并导出`);
    hasConflicts = true;
  } else {
    console.log(`✅ ${file}`);
    if (imports.size > 0) {
      console.log(`   导入: ${imports.size} 个, 导出: ${exports.size} 个, 无冲突`);
    }
  }
});

console.log('\n' + '='.repeat(60));

if (hasConflicts) {
  console.log('❌ 发现导入/导出冲突！');
  process.exit(1);
} else {
  console.log('✅ 无导入/导出冲突');
}

console.log('='.repeat(60));
