#!/usr/bin/env node

/**
 * 检查 JavaScript 模块中的重复导出
 */

const fs = require('fs');
const path = require('path');

console.log('🔍 检查重复导出...\n');

const jsFiles = [
  'src/ui/webview/js/core/state.js',
  'src/ui/webview/js/core/utils.js',
  'src/ui/webview/js/core/vscode-api.js',
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',
  'src/ui/webview/js/main.js'
];

let hasIssues = false;

jsFiles.forEach(file => {
  const content = fs.readFileSync(file, 'utf-8');
  const exports = [];
  const duplicates = [];
  
  // 匹配 export function/const/let/class
  const regex = /export\s+(function|const|let|class)\s+(\w+)/g;
  let match;
  
  while ((match = regex.exec(content)) !== null) {
    const name = match[2];
    if (exports.includes(name)) {
      duplicates.push(name);
    } else {
      exports.push(name);
    }
  }
  
  if (duplicates.length > 0) {
    console.log(`❌ ${file}:`);
    console.log(`   重复导出: ${duplicates.join(', ')}`);
    hasIssues = true;
  } else {
    console.log(`✅ ${file} (${exports.length} 个导出)`);
  }
});

console.log('\n' + '='.repeat(60));

if (hasIssues) {
  console.log('❌ 发现重复导出！');
  process.exit(1);
} else {
  console.log('✅ 无重复导出问题');
}

console.log('='.repeat(60));
