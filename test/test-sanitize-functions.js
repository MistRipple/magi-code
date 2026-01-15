/**
 * 特殊字符处理函数测试
 */

// 模拟前端的处理函数
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

console.log('=== 特殊字符处理函数测试 ===\n');

// 测试 1: ANSI 转义序列移除
const ansiText = '\x1b[31mRed\x1b[0m \x1b[32mGreen\x1b[0m \x1b[1;34mBold Blue\x1b[0m';
const ansiResult = stripAnsi(ansiText);
console.log('测试 1: ANSI 转义序列');
console.log('  输入长度:', ansiText.length);
console.log('  输出:', ansiResult);
console.log('  预期: Red Green Bold Blue');
console.log('  结果:', ansiResult === 'Red Green Bold Blue' ? '✅ 通过' : '❌ 失败');

console.log('');

// 测试 2: 零宽字符移除
const zeroWidthText = 'Hello\u200BWorld\u200C\u200D\uFEFF';
const zeroWidthResult = stripZeroWidth(zeroWidthText);
console.log('测试 2: 零宽字符');
console.log('  输入长度:', zeroWidthText.length);
console.log('  输出:', zeroWidthResult);
console.log('  预期: HelloWorld');
console.log('  结果:', zeroWidthResult === 'HelloWorld' ? '✅ 通过' : '❌ 失败');

console.log('');

// 测试 3: 混合内容预处理
const mixedText = '\x1b[31mError:\x1b[0m Hello\u200BWorld\r\nLine2\rLine3';
const mixedResult = sanitizeCliOutput(mixedText);
console.log('测试 3: 混合内容');
console.log('  输出:', JSON.stringify(mixedResult));
console.log('  预期:', JSON.stringify('Error: HelloWorld\nLine2\nLine3'));
console.log('  结果:', mixedResult === 'Error: HelloWorld\nLine2\nLine3' ? '✅ 通过' : '❌ 失败');

console.log('');

// 测试 4: 空值处理
console.log('测试 4: 空值处理');
console.log('  null:', sanitizeCliOutput(null) === '' ? '✅' : '❌');
console.log('  undefined:', sanitizeCliOutput(undefined) === '' ? '✅' : '❌');
console.log('  空字符串:', sanitizeCliOutput('') === '' ? '✅' : '❌');

console.log('\n=== 测试完成 ===');

