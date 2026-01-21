/**
 * 验证 extractCodeBlocks 修复
 * 测试 match[3] 非路径内容被正确添加回代码
 */

console.log('='.repeat(60));
console.log('extractCodeBlocks 修复验证测试');
console.log('='.repeat(60));

// 修复后的 extractCodeBlocks 逻辑
function isValidFilepath(candidate) {
  if (!candidate || candidate.length > 200) return false;
  if (candidate.includes('/') || candidate.includes('\\')) return true;
  if (/\.\w{1,10}$/.test(candidate)) return true;
  if (/[\s(){}[\]<>=;,]/.test(candidate)) return false;
  return false;
}

function extractCodeBlocks(content) {
  const blocks = [];
  const regex = /```(\w*)(?::([^\s\n]+)|[^\S\n]+([^\n]*))?\n([\s\S]*?)```/g;
  let match;

  while ((match = regex.exec(content)) !== null) {
    let candidateFilepath = match[2] || match[3]?.trim() || undefined;
    let codeContent = match[4] || '';

    // 🔧 关键修复：如果捕获的内容不是有效路径，需要将其添加回代码内容
    if (candidateFilepath && !isValidFilepath(candidateFilepath)) {
      if (match[3]?.trim()) {
        codeContent = match[3].trim() + '\n' + codeContent;
      }
      candidateFilepath = undefined;
    }

    blocks.push({
      lang: match[1] || 'text',
      filepath: candidateFilepath,
      code: codeContent,
      startIndex: match.index,
      endIndex: match.index + match[0].length,
    });
  }

  return blocks;
}

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (e) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${e.message}`);
    failed++;
  }
}

// 测试 1: 正常格式（应该不受影响）
test('正常格式 - 代码块以 { 开头', () => {
  const content = '```json\n{\n  "goal": "test"\n}\n```';
  const blocks = extractCodeBlocks(content);
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  if (!blocks[0].code.trim().startsWith('{')) {
    throw new Error(`代码应该以 { 开头，实际: ${blocks[0].code.substring(0, 20)}`);
  }
});

// 测试 2: 带空格和描述（描述不是路径）
test('带空格和描述 - { 作为描述被捕获', () => {
  // 这是问题场景：```json {\n"goal":...
  const content = '```json {\n  "goal": "test"\n}\n```';
  const blocks = extractCodeBlocks(content);
  
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  console.log('   捕获的代码:', JSON.stringify(blocks[0].code.substring(0, 50)));
  
  // 修复后，{ 应该被添加回代码开头
  if (!blocks[0].code.trim().startsWith('{')) {
    throw new Error(`代码应该以 { 开头，实际以 "${blocks[0].code.trim()[0]}" 开头`);
  }
});

// 测试 3: 复杂描述作为 match[3]
test('复杂描述作为 match[3] - 被错误当作路径', () => {
  // ```json some description\n{\n...
  const content = '```json some description\n{\n  "goal": "test"\n}\n```';
  const blocks = extractCodeBlocks(content);
  
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  // 描述包含空格，所以不是有效路径，应该被添加回代码（但这里原始代码没有问题）
  // 因为原始代码第一行就是 {
  console.log('   代码首行:', JSON.stringify(blocks[0].code.split('\n')[0]));
});

// 测试 4: 有效文件路径（应该不受影响）
test('有效文件路径 - 正确保留 filepath', () => {
  const content = '```json:config.json\n{\n  "key": "value"\n}\n```';
  const blocks = extractCodeBlocks(content);
  
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  if (blocks[0].filepath !== 'config.json') {
    throw new Error(`filepath 应该是 config.json，实际: ${blocks[0].filepath}`);
  }
  if (!blocks[0].code.trim().startsWith('{')) {
    throw new Error('代码应该以 { 开头');
  }
});

// 测试 5: 用户实际场景模拟
test('用户实际场景 - 25行 JSON 规划输出', () => {
  // 模拟可能的 CLI 输出格式
  const content = `\`\`\`json {
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务",
  "constraints": [
    "技术栈: Python FastAPI + Vue 3"
  ]
}
\`\`\``;
  
  const blocks = extractCodeBlocks(content);
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  
  const firstLine = blocks[0].code.split('\n')[0];
  console.log('   代码首行:', JSON.stringify(firstLine));
  
  if (!blocks[0].code.trim().startsWith('{')) {
    throw new Error(`代码应该以 { 开头，首行实际是: "${firstLine}"`);
  }
});

// 测试 6: 只有 { 作为描述
test('只有 { 作为描述', () => {
  // 这是关键场景：```json {\n 后直接是 "goal"
  const content = '```json {\n"goal": "test"\n}\n```';
  const blocks = extractCodeBlocks(content);
  
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');
  
  const firstLine = blocks[0].code.split('\n')[0].trim();
  console.log('   代码首行:', JSON.stringify(firstLine));
  
  // 修复后，{ 应该被添加回代码开头
  if (firstLine !== '{') {
    throw new Error(`首行应该是 {，实际是: "${firstLine}"`);
  }
});

console.log('\n' + '='.repeat(60));
console.log(`测试结果: ${passed} 通过, ${failed} 失败`);
console.log('='.repeat(60));

process.exit(failed > 0 ? 1 : 0);
