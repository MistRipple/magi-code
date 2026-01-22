#!/usr/bin/env node

/**
 * 验证 index.html 中的资源引用是否正确
 */

const fs = require('fs');
const path = require('path');

console.log('🔍 验证 index.html 资源引用...\n');

const htmlPath = 'src/ui/webview/index.html';
const html = fs.readFileSync(htmlPath, 'utf-8');

let allOk = true;

// 1. 检查 CSS 引用
console.log('📋 检查 CSS 引用:');
const cssFiles = ['base.css', 'layout.css', 'components.css', 'messages.css', 'settings.css', 'modals.css'];
cssFiles.forEach(cssFile => {
  const pattern = `href="styles/${cssFile}"`;
  if (html.includes(pattern)) {
    console.log(`  ✅ ${cssFile} - 引用正确`);

    // 检查文件是否存在
    const filePath = path.join('src/ui/webview/styles', cssFile);
    if (fs.existsSync(filePath)) {
      console.log(`     ✅ 文件存在: ${filePath}`);
    } else {
      console.log(`     ❌ 文件不存在: ${filePath}`);
      allOk = false;
    }
  } else {
    console.log(`  ❌ ${cssFile} - 引用缺失或格式错误`);
    allOk = false;
  }
});

// 2. 检查 JavaScript 引用
console.log('\n📋 检查 JavaScript 引用:');
const jsPattern = 'src="js/main.js"';
if (html.includes(jsPattern)) {
  console.log(`  ✅ main.js - 引用正确`);

  const filePath = 'src/ui/webview/js/main.js';
  if (fs.existsSync(filePath)) {
    console.log(`     ✅ 文件存在: ${filePath}`);
  } else {
    console.log(`     ❌ 文件不存在: ${filePath}`);
    allOk = false;
  }
} else {
  console.log(`  ❌ main.js - 引用缺失或格式错误`);
  allOk = false;
}

// 3. 检查 script type="module"
console.log('\n📋 检查模块类型:');
if (html.includes('type="module"')) {
  console.log(`  ✅ script 标签包含 type="module"`);
} else {
  console.log(`  ❌ script 标签缺少 type="module"`);
  allOk = false;
}

// 4. 检查是否有旧的引用
console.log('\n📋 检查旧引用:');
const oldReferences = [
  { pattern: 'href="styles.css"', name: 'styles.css (旧单文件)' },
  { pattern: 'src="login.js"', name: 'login.js (旧文件)' },
];

let hasOldRef = false;
oldReferences.forEach(({ pattern, name }) => {
  if (html.includes(pattern)) {
    console.log(`  ⚠️  发现旧引用: ${name}`);
    hasOldRef = true;
  }
});

if (!hasOldRef) {
  console.log(`  ✅ 无旧引用`);
}

// 5. 统计内联代码
console.log('\n📋 检查内联代码:');
const inlineStyleCount = (html.match(/<style[^>]*>/g) || []).length;
const inlineScriptCount = (html.match(/<script(?![^>]*src=)[^>]*>/g) || []).length;

console.log(`  内联 <style> 标签: ${inlineStyleCount}`);
console.log(`  内联 <script> 标签: ${inlineScriptCount}`);

if (inlineStyleCount === 0) {
  console.log(`  ✅ 无内联 CSS`);
} else {
  console.log(`  ⚠️  存在 ${inlineStyleCount} 个内联 <style> 标签`);
}

if (inlineScriptCount <= 1) {
  console.log(`  ✅ 内联 JavaScript 正常（仅库加载脚本）`);
} else {
  console.log(`  ⚠️  存在 ${inlineScriptCount} 个内联 <script> 标签`);
}

// 6. 检查文件大小
console.log('\n📋 文件大小:');
const stats = fs.statSync(htmlPath);
const sizeKB = (stats.size / 1024).toFixed(1);
console.log(`  index.html: ${sizeKB} KB`);

if (stats.size < 100 * 1024) {
  console.log(`  ✅ 文件大小合理 (< 100 KB)`);
} else {
  console.log(`  ⚠️  文件较大 (> 100 KB)`);
}

// 总结
console.log('\n' + '='.repeat(60));
if (allOk && !hasOldRef) {
  console.log('✅ 所有检查通过！HTML 引用正确。');
  console.log('\n下一步: 运行 `npm run compile` 然后按 F5 测试 Webview');
} else {
  console.log('❌ 发现问题，请检查上述错误。');
  process.exit(1);
}
console.log('='.repeat(60));
