/**
 * Markdown 格式处理完整性测试
 * 检查所有常见 MD 格式是否被正确处理
 */

// 模拟前端的 escapeHtml 函数
function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// 当前实现的 renderMarkdown 简化版（用于测试）
function renderMarkdown(content) {
  if (!content) return '';
  content = content.replace(/\r\n/g, '\n');

  // 代码块占位
  const codeBlockPlaceholders = [];
  content = content.replace(/```([^\n]*)\n?([\s\S]*?)```/g, (match, lang, code) => {
    const idx = codeBlockPlaceholders.length;
    codeBlockPlaceholders.push(`<pre><code class="language-${lang}">${escapeHtml(code)}</code></pre>`);
    return `__CODEBLOCK_${idx}__`;
  });

  // 行内代码
  const inlineCodePlaceholders = [];
  content = content.replace(/`([^`]+)`/g, (match, code) => {
    const idx = inlineCodePlaceholders.length;
    inlineCodePlaceholders.push('<code>' + escapeHtml(code) + '</code>');
    return `__INLINECODE_${idx}__`;
  });

  // 标题 H1-H6
  content = content.replace(/^###### (.+)$/gm, '<h6>$1</h6>');
  content = content.replace(/^##### (.+)$/gm, '<h5>$1</h5>');
  content = content.replace(/^#### (.+)$/gm, '<h4>$1</h4>');
  content = content.replace(/^### (.+)$/gm, '<h3>$1</h3>');
  content = content.replace(/^## (.+)$/gm, '<h2>$1</h2>');
  content = content.replace(/^# (.+)$/gm, '<h1>$1</h1>');

  // 图片
  content = content.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, '<img src="$2" alt="$1">');

  // 粗体和斜体 (支持 * 和 _ 两种语法)
  content = content.replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>');
  content = content.replace(/___(.+?)___/g, '<strong><em>$1</em></strong>');
  content = content.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  content = content.replace(/__(.+?)__/g, '<strong>$1</strong>');
  content = content.replace(/\*([^*\n]+)\*/g, '<em>$1</em>');
  content = content.replace(/_([^_\n]+)_/g, '<em>$1</em>');

  // 删除线
  content = content.replace(/~~(.+?)~~/g, '<del>$1</del>');

  // 高亮
  content = content.replace(/==(.+?)==/g, '<mark>$1</mark>');

  // 链接
  content = content.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank">$1</a>');

  // 任务列表
  content = content.replace(/^[\-\*] \[x\] (.+)$/gm, '<li class="task-item"><input type="checkbox" checked disabled> $1</li>');
  content = content.replace(/^[\-\*] \[ \] (.+)$/gm, '<li class="task-item"><input type="checkbox" disabled> $1</li>');

  // 无序列表
  content = content.replace(/^[\-\*] (.+)$/gm, '<li>$1</li>');

  // 有序列表
  content = content.replace(/^\d+\. (.+)$/gm, '<li>$1</li>');

  // 引用块
  content = content.replace(/^> (.+)$/gm, '<blockquote>$1</blockquote>');

  // 分隔线
  content = content.replace(/^(---|\*\*\*|___)$/gm, '<hr>');

  // 还原占位符
  content = content.replace(/__INLINECODE_(\d+)__/g, (_, idx) => inlineCodePlaceholders[parseInt(idx)]);
  content = content.replace(/__CODEBLOCK_(\d+)__/g, (_, idx) => codeBlockPlaceholders[parseInt(idx)]);

  return content;
}

// 测试用例
const testCases = [
  // 已支持的格式
  { name: '标题 H1', input: '# 标题', expected: '<h1>', supported: true },
  { name: '标题 H2', input: '## 标题', expected: '<h2>', supported: true },
  { name: '标题 H3', input: '### 标题', expected: '<h3>', supported: true },
  { name: '粗体 **', input: '**粗体**', expected: '<strong>', supported: true },
  { name: '斜体 *', input: '*斜体*', expected: '<em>', supported: true },
  { name: '粗斜体 ***', input: '***粗斜体***', expected: '<strong><em>', supported: true },
  { name: '行内代码', input: '`code`', expected: '<code>', supported: true },
  { name: '代码块', input: '```js\ncode\n```', expected: '<pre>', supported: true },
  { name: '链接', input: '[text](url)', expected: '<a href', supported: true },
  { name: '无序列表 -', input: '- item', expected: '<li>', supported: true },
  { name: '无序列表 *', input: '* item', expected: '<li>', supported: true },
  { name: '有序列表', input: '1. item', expected: '<li>', supported: true },
  { name: '引用块', input: '> quote', expected: '<blockquote>', supported: true },
  { name: '分隔线', input: '---', expected: '<hr>', supported: true },
  
  // 可能缺失的格式
  { name: '标题 H4', input: '#### 标题', expected: '<h4>', supported: false },
  { name: '标题 H5', input: '##### 标题', expected: '<h5>', supported: false },
  { name: '标题 H6', input: '###### 标题', expected: '<h6>', supported: false },
  { name: '粗体 __', input: '__粗体__', expected: '<strong>', supported: false },
  { name: '斜体 _', input: '_斜体_', expected: '<em>', supported: false },
  { name: '删除线', input: '~~删除~~', expected: '<del>', supported: false },
  { name: '图片', input: '![alt](url)', expected: '<img', supported: false },
  { name: '表格', input: '| a | b |\n|---|---|\n| 1 | 2 |', expected: '<table>', supported: false },
  { name: '任务列表', input: '- [ ] todo', expected: 'checkbox', supported: false },
  { name: '脚注', input: '[^1]', expected: 'footnote', supported: false },
  { name: '高亮', input: '==高亮==', expected: '<mark>', supported: false },
  { name: '上标', input: 'x^2^', expected: '<sup>', supported: false },
  { name: '下标', input: 'H~2~O', expected: '<sub>', supported: false },
  { name: '嵌套列表', input: '- a\n  - b', expected: 'nested', supported: false },
  { name: '分隔线 ***', input: '***', expected: '<hr>', supported: false },
  { name: '分隔线 ___', input: '___', expected: '<hr>', supported: false },
];

console.log('='.repeat(70));
console.log('  Markdown 格式处理完整性测试');
console.log('='.repeat(70));

let supportedCount = 0;
let missingCount = 0;

console.log('\n✅ 已支持的格式:');
testCases.filter(tc => tc.supported).forEach(tc => {
  const result = renderMarkdown(tc.input);
  const passed = result.includes(tc.expected);
  console.log(`  ${passed ? '✓' : '✗'} ${tc.name}: ${passed ? '正常' : '异常'}`);
  if (passed) supportedCount++;
});

console.log('\n⚠️  未支持的格式 (可能需要添加):');
testCases.filter(tc => !tc.supported).forEach(tc => {
  const result = renderMarkdown(tc.input);
  const actuallySupported = result.includes(tc.expected);
  if (actuallySupported) {
    console.log(`  ✓ ${tc.name}: 实际已支持!`);
    supportedCount++;
  } else {
    console.log(`  ✗ ${tc.name}: 未处理`);
    missingCount++;
  }
});

console.log('\n' + '='.repeat(70));
console.log(`  统计: ${supportedCount} 已支持, ${missingCount} 未支持`);
console.log('='.repeat(70));

// 优先级建议
console.log('\n📋 建议优先添加的格式 (按重要性排序):');
console.log('  1. 删除线 ~~text~~ - CLI 输出常用');
console.log('  2. 图片 ![alt](url) - 文档常用');
console.log('  3. 表格 - 数据展示常用');
console.log('  4. 任务列表 - [ ] - 任务管理常用');
console.log('  5. 下划线粗体/斜体 __bold__ _italic_');

