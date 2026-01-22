#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

// 提取 CSS 部分（第 11 行到 3326 行，索引从 0 开始所以是 10-3325）
const cssLines = lines.slice(10, 3326);

console.log('开始提取 CSS...');
console.log(`总行数: ${cssLines.length}`);
console.log();

// 定义分类规则（按注释关键词）
const categories = {
  base: {
    file: 'src/ui/webview/styles/base.css',
    keywords: ['设计系统变量', '统一颜色系统', '统一卡片样式'],
    lines: [],
    description: 'CSS 变量、颜色系统、重置样式'
  },
  layout: {
    file: 'src/ui/webview/styles/layout.css',
    keywords: [
      '顶部标题栏', '会话选择器', '会话下拉菜单', '会话列表项',
      '顶部 Tab 栏', 'Tab 内容容器', '底部 Tab 栏',
      'CLI 能力标识', '交互模式选择器', '模式指示器', '阶段指示器',
      '主容器', '侧边栏', '布局'
    ],
    lines: [],
    description: '页面布局、容器、Tab 栏'
  },
  components: {
    file: 'src/ui/webview/styles/components.css',
    keywords: [
      '按钮', '输入框', '图标', '徽章', '开关', '下拉框',
      '复选框', '单选框', '加载', '旋转', '动画'
    ],
    lines: [],
    description: '通用组件样式'
  },
  messages: {
    file: 'src/ui/webview/styles/messages.css',
    keywords: [
      '消息', '卡片', '线程', '输出', '思考', '工具调用',
      '计划', '任务', '子任务', '问题', '验证', '恢复',
      '执行计划', '结构化规划'
    ],
    lines: [],
    description: '消息列表、卡片样式'
  },
  settings: {
    file: 'src/ui/webview/styles/settings.css',
    keywords: [
      '设置', '配置', 'Profile', 'LLM', 'MCP', 'Skill',
      '仓库', '工具', '表单', '字段'
    ],
    lines: [],
    description: '设置面板样式'
  },
  modals: {
    file: 'src/ui/webview/styles/modals.css',
    keywords: [
      '弹窗', '对话框', '遮罩', 'Modal', 'Dialog', 'Overlay',
      '确认', '提示'
    ],
    lines: [],
    description: '弹窗和对话框样式'
  }
};

// 添加基础样式（CSS 重置和 body）
categories.base.lines.push('/* CSS 变量和基础样式 */');
categories.base.lines.push('');

// 分类 CSS
let currentCategory = null;
let inComment = false;
let commentText = '';

cssLines.forEach((line, index) => {
  const trimmed = line.trim();

  // 检测注释
  if (trimmed.startsWith('/*')) {
    inComment = true;
    commentText = trimmed;

    // 根据注释内容分类
    let matched = false;
    for (const [catName, cat] of Object.entries(categories)) {
      if (cat.keywords.some(keyword => commentText.includes(keyword))) {
        currentCategory = catName;
        matched = true;
        break;
      }
    }

    // 如果没有匹配，保持当前分类
    if (!matched && currentCategory) {
      // 继续使用当前分类
    }
  }

  if (trimmed.endsWith('*/')) {
    inComment = false;
  }

  // 特殊处理：CSS 重置和 body 样式归入 base
  if (trimmed.startsWith('* {') || trimmed.startsWith('body {')) {
    currentCategory = 'base';
  }

  // 添加到对应分类
  if (currentCategory && categories[currentCategory]) {
    categories[currentCategory].lines.push(line);
  } else {
    // 未分类的放入 components（默认）
    if (!categories.components.lines.includes('/* 其他组件样式 */')) {
      categories.components.lines.push('');
      categories.components.lines.push('/* 其他组件样式 */');
    }
    categories.components.lines.push(line);
  }
});

// 写入文件
console.log('写入 CSS 文件...');
console.log();

for (const [catName, cat] of Object.entries(categories)) {
  if (cat.lines.length > 0) {
    const dir = path.dirname(cat.file);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }

    const content = cat.lines.join('\n');
    fs.writeFileSync(cat.file, content, 'utf-8');

    console.log(`✅ ${cat.file}`);
    console.log(`   ${cat.description}`);
    console.log(`   ${cat.lines.length} 行`);
    console.log();
  }
}

console.log('CSS 提取完成！');
console.log();
console.log('下一步：');
console.log('1. 检查生成的 CSS 文件');
console.log('2. 手动调整分类（如有需要）');
console.log('3. 在 index.html 中引入这些 CSS 文件');
