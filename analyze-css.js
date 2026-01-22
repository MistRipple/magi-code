#!/usr/bin/env node

const fs = require('fs');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');

// 提取 CSS 部分（第 11 行到 3326 行）
const lines = content.split('\n');
const cssLines = lines.slice(10, 3326);
const css = cssLines.join('\n');

// CSS 分类规则
const categories = {
  base: {
    file: 'src/ui/webview/styles/base.css',
    patterns: [
      /\/\* 设计系统变量 \*\//,
      /^\s*:root\s*{/,
      /^\s*\*\s*{/,
      /^\s*body\s*{/,
    ],
    endPattern: /^\s*\.header-bar\s*{/
  },
  layout: {
    file: 'src/ui/webview/styles/layout.css',
    patterns: [
      /\/\* 顶部标题栏 \*\//,
      /\/\* 会话选择器 \*\//,
      /\/\* 会话下拉菜单 \*\//,
      /\/\* 顶部 Tab 栏 \*\//,
      /\/\* Tab 内容容器 \*\//,
      /\/\* 底部 Tab 栏 \*\//,
      /\.header-bar/,
      /\.session-/,
      /\.top-tab/,
      /\.bottom-tab/,
      /\.tab-/,
    ]
  },
  components: {
    file: 'src/ui/webview/styles/components.css',
    patterns: [
      /\.icon-btn/,
      /\.badge/,
      /\.dot/,
      /\.btn/,
      /\.input/,
      /\.select/,
      /\.checkbox/,
      /\.radio/,
      /\.toggle/,
      /\.spinner/,
      /\.loading/,
    ]
  },
  messages: {
    file: 'src/ui/webview/styles/messages.css',
    patterns: [
      /\/\* 消息/,
      /\/\* 卡片/,
      /\.message/,
      /\.card/,
      /\.thread/,
      /\.cli-output/,
      /\.thinking/,
      /\.tool-call/,
      /\.plan/,
      /\.task/,
    ]
  },
  settings: {
    file: 'src/ui/webview/styles/settings.css',
    patterns: [
      /\/\* 设置/,
      /\.settings/,
      /\.profile/,
      /\.config/,
      /\.llm-config/,
      /\.mcp-/,
      /\.skill/,
      /\.repo/,
    ]
  },
  modals: {
    file: 'src/ui/webview/styles/modals.css',
    patterns: [
      /\/\* 弹窗/,
      /\/\* 对话框/,
      /\.modal/,
      /\.dialog/,
      /\.overlay/,
      /\.popup/,
    ]
  }
};

console.log('开始分析 CSS...');
console.log(`总行数: ${cssLines.length}`);
console.log();

// 简单的分类：按注释分段
const sections = [];
let currentSection = { comment: '', lines: [] };

cssLines.forEach((line, index) => {
  if (line.trim().startsWith('/* ') && line.trim().endsWith(' */')) {
    // 新的注释段落
    if (currentSection.lines.length > 0) {
      sections.push(currentSection);
    }
    currentSection = { comment: line.trim(), lines: [line], startLine: index };
  } else {
    currentSection.lines.push(line);
  }
});

if (currentSection.lines.length > 0) {
  sections.push(currentSection);
}

console.log(`找到 ${sections.length} 个 CSS 段落`);
console.log();

// 显示前 20 个段落
sections.slice(0, 20).forEach((section, index) => {
  console.log(`${index + 1}. ${section.comment} (${section.lines.length} 行)`);
});

console.log();
console.log('建议的分类方案：');
console.log();
console.log('base.css:');
console.log('  - 设计系统变量');
console.log('  - CSS 重置');
console.log('  - body 样式');
console.log();
console.log('layout.css:');
console.log('  - 顶部标题栏');
console.log('  - 会话选择器');
console.log('  - Tab 栏');
console.log('  - 主容器布局');
console.log();
console.log('components.css:');
console.log('  - 按钮');
console.log('  - 输入框');
console.log('  - 图标');
console.log('  - 徽章');
console.log();
console.log('messages.css:');
console.log('  - 消息卡片');
console.log('  - 消息列表');
console.log('  - 工具调用');
console.log('  - 思考块');
console.log();
console.log('settings.css:');
console.log('  - 设置面板');
console.log('  - 配置表单');
console.log('  - Profile 配置');
console.log();
console.log('modals.css:');
console.log('  - 弹窗容器');
console.log('  - 对话框');
console.log('  - 遮罩层');
