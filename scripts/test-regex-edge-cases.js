/**
 * 测试代码块正则在各种边界情况下的行为
 */

console.log('='.repeat(60));
console.log('代码块正则边界测试');
console.log('='.repeat(60));

// 使用与 content-parser.ts 相同的正则
const regex = /```(\w*)(?::([^\s\n]+)|[^\S\n]+([^\n]*))?\n([\s\S]*?)```/g;

function testRegex(name, content) {
  console.log(`\n${name}:`);
  console.log('输入:', JSON.stringify(content.substring(0, 100)) + (content.length > 100 ? '...' : ''));

  regex.lastIndex = 0;  // 重置正则状态
  const match = regex.exec(content);

  if (match) {
    console.log('✅ 匹配成功');
    console.log('  语言:', JSON.stringify(match[1]));
    console.log('  代码首行:', JSON.stringify(match[4].split('\n')[0]));
    console.log('  代码首字符:', JSON.stringify(match[4].trim()[0]));

    if (match[4].trim().startsWith('{')) {
      console.log('  ✅ 代码正确以 { 开头');
    } else {
      console.log('  ❌ 代码没有以 { 开头');
    }
  } else {
    console.log('❌ 未匹配');
  }
}

// 测试用例
testRegex('正常格式', `\`\`\`json
{
  "goal": "test"
}
\`\`\``);

testRegex('没有语言标记', `\`\`\`
{
  "goal": "test"
}
\`\`\``);

testRegex('语言后无换行（紧跟 {）', '```json{\n  "goal": "test"\n}\n```');

testRegex('带文件路径', `\`\`\`json:config.json
{
  "goal": "test"
}
\`\`\``);

testRegex('带空格分隔的描述', `\`\`\`json config file
{
  "goal": "test"
}
\`\`\``);

testRegex('代码块前有空行', `\`\`\`json

{
  "goal": "test"
}
\`\`\``);

testRegex('Windows 换行', '```json\r\n{\r\n  "goal": "test"\r\n}\r\n```');

testRegex('只有换行符', '```json\n\n```');

testRegex('内容首行是 "goal"（无 {）', `\`\`\`json
  "goal": "test",
  "analysis": "desc"
\`\`\``);

// 模拟可能的问题场景
console.log('\n' + '='.repeat(60));
console.log('问题场景模拟');
console.log('='.repeat(60));

// 场景：代码块内容在提取后被截断
const originalContent = `\`\`\`json
{
  "goal": "创建一个单人坦克大战小游戏"
}
\`\`\``;

regex.lastIndex = 0;
const m = regex.exec(originalContent);
if (m) {
  const code = m[4];
  const lines = code.split('\n');

  console.log('\n提取的代码行数:', lines.length);
  console.log('各行内容:');
  lines.forEach((line, i) => {
    console.log(`  第${i + 1}行: ${JSON.stringify(line)}`);
  });

  // 模拟 renderCodeBlock 的处理
  const trimmedCode = code.replace(/^\n+/, '').replace(/\n+$/, '');
  const trimmedLines = trimmedCode.split('\n');

  console.log('\n经过 trim 处理后:');
  console.log('处理后行数:', trimmedLines.length);
  trimmedLines.forEach((line, i) => {
    console.log(`  第${i + 1}行: ${JSON.stringify(line)}`);
  });
}

console.log('\n' + '='.repeat(60));
console.log('测试完成');
console.log('='.repeat(60));
