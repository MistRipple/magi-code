/**
 * CLI 输出解析测试脚本
 * 启动真实 CLI 进程，捕获输出并验证解析逻辑
 */

const { spawn } = require('child_process');
const path = require('path');

// 测试用的 prompt，让 CLI 输出各种格式
const TEST_PROMPTS = {
  codeBlock: '请用 TypeScript 写一个简单的 debounce 函数，包含完整的类型定义',
  diffBlock: '请展示如何修改一个 package.json 文件，将版本从 1.0.0 改为 2.0.0',
  longMessage: '请详细解释 JavaScript 的事件循环机制，包括宏任务和微任务的区别'
};

/**
 * 解析代码块（模拟前端解析逻辑）
 */
function parseCodeBlocks(content) {
  const codeBlockRegex = /```(\w+)?(?::([^\n]+))?\n([\s\S]*?)```/g;
  const blocks = [];
  let match;
  
  while ((match = codeBlockRegex.exec(content)) !== null) {
    blocks.push({
      language: match[1] || 'text',
      filepath: match[2] || null,
      code: match[3],
      hasDiff: /^[+-]/m.test(match[3]),
      lineCount: match[3].split('\n').length
    });
  }
  
  return blocks;
}

/**
 * 检测差异行
 */
function parseDiffLines(code) {
  const lines = code.split('\n');
  return lines.map((line, idx) => ({
    lineNumber: idx + 1,
    content: line,
    type: line.startsWith('+') ? 'added' : line.startsWith('-') ? 'removed' : 'normal'
  }));
}

/**
 * 检测是否需要折叠（超过15行）
 */
function shouldCollapse(content) {
  const lineCount = (content.match(/\n/g) || []).length + 1;
  return { shouldCollapse: lineCount > 15, lineCount };
}

/**
 * 格式化相对时间
 */
function formatRelativeTime(timestamp) {
  const diff = Date.now() - timestamp;
  if (diff < 60000) return '刚刚';
  if (diff < 3600000) return Math.floor(diff / 60000) + ' 分钟前';
  if (diff < 86400000) return Math.floor(diff / 3600000) + ' 小时前';
  return Math.floor(diff / 86400000) + ' 天前';
}

/**
 * 运行 CLI 并捕获输出
 */
async function runCLI(cliName, prompt) {
  return new Promise((resolve, reject) => {
    console.log(`\n🚀 启动 ${cliName} CLI...`);
    console.log(`📝 Prompt: ${prompt.substring(0, 50)}...`);

    const startTime = Date.now();
    let output = '';

    // 使用管道方式传入 prompt
    const proc = spawn(cliName, ['-p'], {
      stdio: ['pipe', 'pipe', 'pipe']
    });

    // 写入 prompt
    proc.stdin.write(prompt);
    proc.stdin.end();

    proc.stdout.on('data', (data) => {
      output += data.toString();
    });

    proc.stderr.on('data', (data) => {
      // 忽略 stderr 的调试信息
    });

    proc.on('close', (code) => {
      const elapsed = Date.now() - startTime;
      resolve({ output, elapsed, exitCode: code });
    });

    proc.on('error', (err) => {
      reject(err);
    });

    // 30秒超时
    setTimeout(() => {
      proc.kill();
      resolve({ output, elapsed: 30000, exitCode: -1, timeout: true });
    }, 30000);
  });
}

/**
 * 分析并展示解析结果
 */
function analyzeOutput(output, elapsed) {
  console.log('\n' + '='.repeat(60));
  console.log('📊 解析结果分析');
  console.log('='.repeat(60));
  
  // 1. 代码块解析
  const codeBlocks = parseCodeBlocks(output);
  console.log(`\n✅ 代码块数量: ${codeBlocks.length}`);
  codeBlocks.forEach((block, idx) => {
    console.log(`   [${idx + 1}] 语言: ${block.language}, 文件: ${block.filepath || '无'}, 行数: ${block.lineCount}, 差异: ${block.hasDiff ? '是' : '否'}`);
    if (block.hasDiff) {
      const diffLines = parseDiffLines(block.code);
      const added = diffLines.filter(l => l.type === 'added').length;
      const removed = diffLines.filter(l => l.type === 'removed').length;
      console.log(`       +${added} 行新增, -${removed} 行删除`);
    }
  });
  
  // 2. 折叠检测
  const collapseInfo = shouldCollapse(output);
  console.log(`\n✅ 消息折叠: ${collapseInfo.shouldCollapse ? '需要折叠' : '无需折叠'} (${collapseInfo.lineCount} 行)`);
  
  // 3. 相对时间
  const timestamp = Date.now() - elapsed;
  console.log(`\n✅ 相对时间: ${formatRelativeTime(timestamp)}`);
  
  // 4. 输出预览
  console.log('\n📄 输出预览 (前500字符):');
  console.log('-'.repeat(60));
  console.log(output.substring(0, 500) + (output.length > 500 ? '...' : ''));
  console.log('-'.repeat(60));
}

// 主函数
async function main() {
  console.log('='.repeat(60));
  console.log('  CLI 输出解析测试');
  console.log('='.repeat(60));
  
  // 检测可用的 CLI
  const { execSync } = require('child_process');
  const availableCLIs = [];
  
  ['claude', 'codex', 'gemini'].forEach(cli => {
    try {
      execSync(`which ${cli}`, { stdio: 'ignore' });
      availableCLIs.push(cli);
    } catch {}
  });
  
  console.log(`\n可用 CLI: ${availableCLIs.join(', ') || '无'}`);
  
  if (availableCLIs.length === 0) {
    console.log('\n⚠️  没有可用的 CLI，使用模拟数据测试解析逻辑...\n');
    testWithMockData();
    return;
  }
  
  // 使用第一个可用的 CLI 进行测试
  const cli = availableCLIs[0];
  try {
    const result = await runCLI(cli, TEST_PROMPTS.codeBlock);
    analyzeOutput(result.output, result.elapsed);
  } catch (err) {
    console.error('CLI 执行失败:', err.message);
    testWithMockData();
  }
}

// 使用模拟数据测试
function testWithMockData() {
  const mockOutput = `好的，这是一个 TypeScript 的 debounce 函数实现：

\`\`\`typescript:src/utils/debounce.ts
/**
 * 防抖函数
 * @param fn 要防抖的函数
 * @param delay 延迟时间（毫秒）
 */
export function debounce<T extends (...args: any[]) => any>(
  fn: T,
  delay: number
): (...args: Parameters<T>) => void {
  let timer: NodeJS.Timeout | null = null;
  
  return (...args: Parameters<T>) => {
    if (timer) {
      clearTimeout(timer);
    }
    timer = setTimeout(() => {
      fn(...args);
      timer = null;
    }, delay);
  };
}
\`\`\`

使用示例：

\`\`\`typescript
const debouncedSearch = debounce((query: string) => {
  console.log('搜索:', query);
}, 300);
\`\`\`
`;

  console.log('使用模拟数据进行测试...');
  analyzeOutput(mockOutput, 1500);
}

main().catch(console.error);

