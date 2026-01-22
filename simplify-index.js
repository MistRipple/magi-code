#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

console.log('开始简化 index.html...\n');

// 读取原始 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

// 找到各个部分的位置
let styleStart = -1;
let styleEnd = -1;
let scriptStart = -1;
let scriptEnd = -1;
let scriptCount = 0;

for (let i = 0; i < lines.length; i++) {
  const line = lines[i].trim();

  // 找到 <style> 标签
  if (line === '<style>' && styleStart === -1) {
    styleStart = i;
  }
  if (line === '</style>' && styleStart !== -1 && styleEnd === -1) {
    styleEnd = i;
  }

  // 找到第二个 <script> 标签（主要的 JavaScript）
  if (line === '<script>') {
    scriptCount++;
    if (scriptCount === 2) {
      scriptStart = i;
    }
  }
  if (line === '</script>' && scriptStart !== -1 && scriptEnd === -1) {
    scriptEnd = i;
  }
}

console.log(`找到内联 CSS: 第 ${styleStart + 1} 行 到 第 ${styleEnd + 1} 行 (${styleEnd - styleStart + 1} 行)`);
console.log(`找到内联 JS: 第 ${scriptStart + 1} 行 到 第 ${scriptEnd + 1} 行 (${scriptEnd - scriptStart + 1} 行)\n`);

// 提取 HTML 部分（移除内联 CSS 和 JS）
const htmlParts = [];

// 1. 头部（到 <style> 之前）
htmlParts.push(lines.slice(0, styleStart).join('\n'));

// 2. 替换 <style> 为 CSS 引入
htmlParts.push(`  <!-- 引入模块化 CSS -->
  <link rel="stylesheet" href="styles/base.css">
  <link rel="stylesheet" href="styles/layout.css">
  <link rel="stylesheet" href="styles/components.css">
  <link rel="stylesheet" href="styles/messages.css">
  <link rel="stylesheet" href="styles/settings.css">
  <link rel="stylesheet" href="styles/modals.css">`);

// 3. 中间部分（<style> 到 <script> 之间）
htmlParts.push(lines.slice(styleEnd + 1, scriptStart).join('\n'));

// 4. 替换 <script> 为 JS 引入
htmlParts.push(`  <!-- 引入模块化 JavaScript -->
  <script type="module" src="js/main.js"></script>`);

// 5. 尾部（</script> 之后）
htmlParts.push(lines.slice(scriptEnd + 1).join('\n'));

// 合并并清理
let simplifiedHtml = htmlParts.join('\n');

// 清理多余的空行（连续超过 2 个空行压缩为 2 个）
simplifiedHtml = simplifiedHtml.replace(/\n{3,}/g, '\n\n');

// 写入新文件
const outputPath = 'src/ui/webview/index-new.html';
fs.writeFileSync(outputPath, simplifiedHtml, 'utf-8');

const newLines = simplifiedHtml.split('\n').length;
const originalLines = lines.length;
const removedLines = originalLines - newLines;
const reductionPercent = ((removedLines / originalLines) * 100).toFixed(1);

console.log(`✅ 已创建简化版: ${outputPath}`);
console.log(`   原始行数: ${originalLines} 行`);
console.log(`   简化后: ${newLines} 行`);
console.log(`   减少: ${removedLines} 行 (${reductionPercent}%)\n`);

console.log('📊 简化内容：');
console.log(`   - 移除内联 CSS: ${styleEnd - styleStart + 1} 行`);
console.log(`   - 移除内联 JS: ${scriptEnd - scriptStart + 1} 行`);
console.log(`   - 添加 CSS 引入: 6 个 <link> 标签`);
console.log(`   - 添加 JS 引入: 1 个 <script type="module">\n`);

console.log('⚠️  注意：');
console.log('   1. 新文件保存为 index-new.html');
console.log('   2. 请检查新文件是否正常工作');
console.log('   3. 确认无误后，可以替换原文件：');
console.log('      mv src/ui/webview/index-new.html src/ui/webview/index.html\n');

console.log('index.html 简化完成！');
