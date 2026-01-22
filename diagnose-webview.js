// 在浏览器 Console 中运行此脚本来诊断问题

console.log('=== MultiCLI 诊断报告 ===\n');

// 1. 检查全局状态
console.log('1. 全局状态检查:');
console.log('  - __DEBUG__ 存在:', typeof window.__DEBUG__ !== 'undefined');
if (window.__DEBUG__) {
  console.log('  - threadMessages 数量:', window.__DEBUG__.threadMessages?.length || 0);
  console.log('  - sessions 数量:', window.__DEBUG__.sessions?.length || 0);
  console.log('  - pendingChanges 数量:', window.__DEBUG__.pendingChanges?.length || 0);
}

// 2. 检查 DOM 元素
console.log('\n2. DOM 元素检查:');
console.log('  - main-content:', !!document.getElementById('main-content'));
console.log('  - prompt-input:', !!document.getElementById('prompt-input'));
console.log('  - execute-btn:', !!document.getElementById('execute-btn'));
console.log('  - new-session-btn:', !!document.getElementById('new-session-btn'));
console.log('  - settings-btn:', !!document.getElementById('settings-btn'));

// 3. 检查 Tab 状态
console.log('\n3. Tab 状态检查:');
const activeTopTab = document.querySelector('.top-tab.active');
const activeBottomTab = document.querySelector('.bottom-tab.active');
console.log('  - 活动 Top Tab:', activeTopTab?.dataset.tab || '未找到');
console.log('  - 活动 Bottom Tab:', activeBottomTab?.dataset.bottomTab || '未找到');

// 4. 检查事件监听器
console.log('\n4. 事件监听器检查:');
const executeBtn = document.getElementById('execute-btn');
const newSessionBtn = document.getElementById('new-session-btn');
const settingsBtn = document.getElementById('settings-btn');
console.log('  - execute-btn 有监听器:', executeBtn && getEventListeners(executeBtn).click?.length > 0);
console.log('  - new-session-btn 有监听器:', newSessionBtn && getEventListeners(newSessionBtn).click?.length > 0);
console.log('  - settings-btn 有监听器:', settingsBtn && getEventListeners(settingsBtn).click?.length > 0);

// 5. 检查 main-content 内容
console.log('\n5. main-content 内容检查:');
const mainContent = document.getElementById('main-content');
if (mainContent) {
  console.log('  - innerHTML 长度:', mainContent.innerHTML.length);
  console.log('  - 前 200 字符:', mainContent.innerHTML.substring(0, 200));
  console.log('  - 子元素数量:', mainContent.children.length);
}

// 6. 检查 CSS 加载
console.log('\n6. CSS 文件加载检查:');
const cssFiles = ['base.css', 'layout.css', 'components.css', 'messages.css', 'settings.css', 'modals.css'];
cssFiles.forEach(file => {
  const link = document.querySelector(`link[href*="${file}"]`);
  console.log(`  - ${file}:`, link ? '✅ 已加载' : '❌ 未找到');
});

// 7. 检查 JS 模块加载
console.log('\n7. JavaScript 模块检查:');
const scripts = document.querySelectorAll('script[src]');
console.log('  - 脚本数量:', scripts.length);
scripts.forEach(script => {
  console.log('  -', script.src.split('/').pop());
});

// 8. 检查 Import Map
console.log('\n8. Import Map 检查:');
const importMap = document.querySelector('script[type="importmap"]');
if (importMap) {
  console.log('  - Import Map 存在: ✅');
  try {
    const map = JSON.parse(importMap.textContent);
    console.log('  - 导入数量:', Object.keys(map.imports || {}).length);
  } catch (e) {
    console.log('  - 解析错误:', e.message);
  }
} else {
  console.log('  - Import Map 存在: ❌');
}

// 9. 检查设置面板
console.log('\n9. 设置面板检查:');
const settingsOverlay = document.getElementById('settings-overlay');
console.log('  - settings-overlay 存在:', !!settingsOverlay);
console.log('  - 显示状态:', settingsOverlay?.style.display || 'default');

// 10. 检查 Console 日志
console.log('\n10. 请检查 Console 中是否有以下日志:');
console.log('  - [Main] 主模块加载完成');
console.log('  - [Main] 初始化应用...');
console.log('  - [EventHandlers] 事件监听器初始化完成');
console.log('  - [Main] 应用初始化完成');

console.log('\n=== 诊断完成 ===');
