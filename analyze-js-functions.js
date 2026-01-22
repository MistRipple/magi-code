#!/usr/bin/env node

const fs = require('fs');

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

console.log('分析 JavaScript 函数结构...\n');

// 提取所有函数定义
const functions = [];
jsLines.forEach((line, idx) => {
  const trimmed = line.trim();
  const match = trimmed.match(/^function\s+(\w+)\s*\(/);
  if (match) {
    functions.push({
      line: scriptStart + idx + 2,
      name: match[1]
    });
  }
});

console.log(`找到 ${functions.length} 个函数\n`);

// 按功能分类
const categories = {
  render: [],
  message: [],
  ui: [],
  settings: [],
  modal: [],
  event: [],
  state: [],
  util: [],
  other: []
};

functions.forEach(f => {
  const name = f.name.toLowerCase();

  if (name.includes('render')) {
    categories.render.push(f);
  } else if (name.includes('message') || name.includes('thread') || name.includes('cli')) {
    categories.message.push(f);
  } else if (name.includes('show') || name.includes('hide') || name.includes('toggle') || name.includes('update')) {
    categories.ui.push(f);
  } else if (name.includes('settings') || name.includes('profile') || name.includes('config')) {
    categories.settings.push(f);
  } else if (name.includes('modal') || name.includes('dialog') || name.includes('popup')) {
    categories.modal.push(f);
  } else if (name.includes('handle') || name.includes('on')) {
    categories.event.push(f);
  } else if (name.includes('save') || name.includes('load') || name.includes('get') || name.includes('set')) {
    categories.state.push(f);
  } else if (name.includes('format') || name.includes('parse') || name.includes('escape') || name.includes('validate')) {
    categories.util.push(f);
  } else {
    categories.other.push(f);
  }
});

// 输出分类结果
console.log('=== 渲染相关函数 (render) ===');
categories.render.forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
console.log();

console.log('=== 消息处理函数 (message) ===');
categories.message.slice(0, 20).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.message.length > 20) console.log(`  ... 还有 ${categories.message.length - 20} 个`);
console.log();

console.log('=== UI 交互函数 (ui) ===');
categories.ui.slice(0, 20).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.ui.length > 20) console.log(`  ... 还有 ${categories.ui.length - 20} 个`);
console.log();

console.log('=== 设置面板函数 (settings) ===');
categories.settings.forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
console.log();

console.log('=== 弹窗相关函数 (modal) ===');
categories.modal.forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
console.log();

console.log('=== 事件处理函数 (event) ===');
categories.event.slice(0, 15).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.event.length > 15) console.log(`  ... 还有 ${categories.event.length - 15} 个`);
console.log();

console.log('=== 状态管理函数 (state) ===');
categories.state.slice(0, 15).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.state.length > 15) console.log(`  ... 还有 ${categories.state.length - 15} 个`);
console.log();

console.log('=== 工具函数 (util) ===');
categories.util.slice(0, 15).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.util.length > 15) console.log(`  ... 还有 ${categories.util.length - 15} 个`);
console.log();

console.log('=== 其他函数 (other) ===');
categories.other.slice(0, 15).forEach(f => console.log(`  ${f.name}() - 第 ${f.line} 行`));
if (categories.other.length > 15) console.log(`  ... 还有 ${categories.other.length - 15} 个`);
console.log();

// 统计
console.log('=== 统计 ===');
Object.keys(categories).forEach(cat => {
  console.log(`${cat}: ${categories[cat].length} 个函数`);
});
