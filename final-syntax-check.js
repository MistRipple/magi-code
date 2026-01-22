#!/usr/bin/env node

/**
 * 最终语法检查 - 验证所有 JavaScript 模块
 */

const { execSync } = require('child_process');
const fs = require('fs');

console.log('🔍 最终语法检查...\n');

const jsFiles = [
  'src/ui/webview/js/core/state.js',
  'src/ui/webview/js/core/utils.js',
  'src/ui/webview/js/core/vscode-api.js',
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',
  'src/ui/webview/js/main.js'
];

let allOk = true;

jsFiles.forEach(file => {
  try {
    execSync(`node -c "${file}"`, { stdio: 'pipe' });
    const stats = fs.statSync(file);
    const lines = fs.readFileSync(file, 'utf-8').split('\n').length;
    console.log(`✅ ${file} (${lines} 行, ${(stats.size / 1024).toFixed(1)} KB)`);
  } catch (error) {
    console.log(`❌ ${file}:`);
    console.log(`   ${error.stderr.toString().trim()}`);
    allOk = false;
  }
});

console.log('\n' + '='.repeat(60));

if (allOk) {
  console.log('✅ 所有文件语法正确！');
  console.log('\n准备进行运行时测试：');
  console.log('  1. 按 F5 启动 VSCode 扩展开发主机');
  console.log('  2. 打开 MultiCLI 面板');
  console.log('  3. 检查样式和功能');
} else {
  console.log('❌ 发现语法错误！');
  process.exit(1);
}

console.log('='.repeat(60));
