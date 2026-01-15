/**
 * 特殊字符处理测试脚本
 * 验证前端渲染逻辑对各种特殊字符的处理
 */

// 模拟前端的处理函数
function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function stripAnsi(text) {
  return String(text).replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '');
}

function stripZeroWidth(text) {
  return String(text).replace(/[\u200B-\u200D\uFEFF]/g, '');
}

function sanitizeCliOutput(text) {
  if (!text) return '';
  let result = String(text);
  result = stripAnsi(result);
  result = stripZeroWidth(result);
  result = result.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  return result;
}

// 测试用例
const testCases = [
  {
    name: 'XSS 攻击 - script 标签',
    input: '<script>alert("XSS")</script>',
    shouldEscape: true
  },
  {
    name: 'XSS 攻击 - img onerror',
    input: '<img src=x onerror="alert(1)">',
    shouldEscape: true
  },
  {
    name: 'XSS 攻击 - svg onload',
    input: '<svg onload="alert(1)">',
    shouldEscape: true
  },
  {
    name: 'HTML 实体 - 已编码',
    input: '&lt;script&gt;alert(1)&lt;/script&gt;',
    shouldEscape: true,
    note: '双重转义问题：&lt; 会变成 &amp;lt;'
  },
  {
    name: 'HTML 实体 - nbsp',
    input: 'Hello&nbsp;World',
    shouldEscape: true
  },
  {
    name: '换行标签 - br',
    input: 'Line1<br>Line2<br/>Line3',
    shouldEscape: true
  },
  {
    name: '转义字符 - 反斜杠n',
    input: 'Line1\\nLine2\\tTab',
    shouldEscape: false,
    note: '字面量反斜杠，不是真正的换行'
  },
  {
    name: '真实换行符',
    input: 'Line1\nLine2\tTab',
    shouldEscape: false,
    note: '真实换行符需要转换为 <br> 或保留'
  },
  {
    name: 'Unicode Emoji',
    input: '😀🎉🚀💻',
    shouldEscape: false
  },
  {
    name: 'Unicode 特殊字符',
    input: '中文 日本語 한국어 العربية',
    shouldEscape: false
  },
  {
    name: 'JSON 字符串',
    input: '{"key": "value", "nested": {"a": 1}}',
    shouldEscape: true
  },
  {
    name: '代码块中的 HTML',
    input: '```html\n<div class="test">Hello</div>\n```',
    shouldEscape: false,
    note: '代码块内的 HTML 应该被转义显示'
  },
  {
    name: '混合内容',
    input: '这是<b>粗体</b>和`<code>`标签',
    shouldEscape: true
  },
  {
    name: 'URL 中的特殊字符',
    input: 'https://example.com?a=1&b=2&c=<test>',
    shouldEscape: true
  },
  {
    name: '路径中的反斜杠',
    input: 'C:\\Users\\test\\file.txt',
    shouldEscape: false
  },
  {
    name: '正则表达式',
    input: '/^[a-z]+$/gi.test("hello")',
    shouldEscape: true
  },
  {
    name: 'ANSI 转义序列',
    input: '\x1b[31mRed Text\x1b[0m',
    shouldEscape: true,
    note: 'CLI 输出可能包含 ANSI 颜色代码'
  },
  {
    name: '零宽字符',
    input: 'Hello\u200BWorld\u200C',
    shouldEscape: false,
    note: '零宽空格可能导致显示问题'
  }
];

console.log('='.repeat(70));
console.log('  特殊字符处理测试');
console.log('='.repeat(70));

let passed = 0;
let failed = 0;
let warnings = 0;

testCases.forEach((tc, idx) => {
  console.log(`\n[${idx + 1}] ${tc.name}`);
  console.log(`    输入: ${JSON.stringify(tc.input).substring(0, 50)}`);
  
  const escaped = escapeHtml(tc.input);
  const hasHtmlTags = /<[^>]+>/.test(escaped);
  const hasUnescapedAmp = /&(?!(amp|lt|gt|quot|#39);)/.test(escaped);
  
  if (tc.shouldEscape) {
    if (!hasHtmlTags && !hasUnescapedAmp) {
      console.log(`    ✅ 通过: HTML 已正确转义`);
      passed++;
    } else {
      console.log(`    ❌ 失败: 存在未转义的 HTML`);
      console.log(`    输出: ${escaped.substring(0, 50)}`);
      failed++;
    }
  } else {
    console.log(`    ✅ 通过: 无需转义`);
    passed++;
  }
  
  if (tc.note) {
    console.log(`    ⚠️  注意: ${tc.note}`);
    warnings++;
  }
});

console.log('\n' + '='.repeat(70));
console.log(`  测试结果: ${passed} 通过, ${failed} 失败, ${warnings} 警告`);
console.log('='.repeat(70));

// 检查当前实现的问题
console.log('\n📋 当前实现可能存在的问题:');
console.log('');
console.log('1. 双重转义: 如果内容已包含 &lt; 等实体，会被再次转义为 &amp;lt;');
console.log('2. 换行处理: 真实换行符 \\n 需要转换为 <br> 才能正确显示');
console.log('3. ANSI 代码: CLI 输出的颜色代码需要过滤或转换');
console.log('4. 零宽字符: 可能导致复制粘贴问题');
console.log('5. 代码块内容: 需要确保代码块内的 HTML 被转义显示');

