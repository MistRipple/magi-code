/**
 * 消息流追踪诊断
 * 逐层追踪消息事件链，找到断点
 */

const path = require('path');

async function run() {
  // 动态加载编译后的模块
  const outDir = path.join(__dirname, '../out');

  const { CLIAdapterFactory } = require(path.join(outDir, 'cli/adapter-factory'));
  const { SessionManager } = require(path.join(outDir, 'cli/session/session-manager'));
  const { globalEventBus } = require(path.join(outDir, 'events'));

  const cwd = process.cwd();
  const prompt = process.argv.slice(2).join(' ') || '你好';

  console.log('=== 消息流追踪诊断 ===');
  console.log(`工作目录: ${cwd}`);
  console.log(`测试消息: ${prompt}`);
  console.log('');

  // 计数器
  const counts = {
    sessionManagerOutput: 0,
    adapterOutput: 0,
    normalizerMessage: 0,
    normalizerComplete: 0,
    standardMessage: 0,
    standardComplete: 0,
  };

  // 日志
  const logs = [];
  const addLog = (source, msg) => {
    const ts = Date.now();
    logs.push({ ts, source, msg });
    console.log(`[${source}] ${msg}`);
  };

  // 创建 CLIAdapterFactory
  const factory = new CLIAdapterFactory({ cwd });

  // 监听 CLIAdapterFactory 事件
  factory.on('standardMessage', (msg) => {
    counts.standardMessage++;
    const blocksInfo = msg.blocks?.map(b => `${b.type}:${(b.content || '').slice(0, 30)}`).join(', ');
    addLog('CLIAdapterFactory', `standardMessage: id=${msg.id}, blocks=[${blocksInfo}]`);
  });

  factory.on('standardComplete', (msg) => {
    counts.standardComplete++;
    const textBlocks = msg.blocks?.filter(b => b.type === 'text') || [];
    const totalChars = textBlocks.reduce((s, b) => s + (b.content?.length || 0), 0);
    addLog('CLIAdapterFactory', `standardComplete: id=${msg.id}, textChars=${totalChars}`);
  });

  factory.on('standardUpdate', (update) => {
    addLog('CLIAdapterFactory', `standardUpdate: type=${update.updateType}`);
  });

  // 访问内部 SessionManager
  const sessionManager = factory['sessionManager'];
  if (sessionManager) {
    sessionManager.on('output', ({ cli, role, chunk }) => {
      counts.sessionManagerOutput++;
      const preview = chunk.slice(0, 80).replace(/\n/g, '\\n');
      addLog('SessionManager', `output: cli=${cli}, role=${role}, preview="${preview}"`);
    });

    sessionManager.on('log', (msg) => {
      if (msg.includes('output') || msg.includes('消息') || msg.includes('stream')) {
        addLog('SessionManager', `log: ${msg}`);
      }
    });

    sessionManager.on('question', ({ cli, role, question }) => {
      addLog('SessionManager', `question: cli=${cli}, questionId=${question.questionId}`);
    });
  }

  // 检查 CLI 可用性
  console.log('\n--- 检查 CLI 可用性 ---');
  const availability = await factory.checkAllAvailability();
  console.log('可用性:', availability);

  const availableCLIs = Object.entries(availability).filter(([, ok]) => ok).map(([k]) => k);
  if (availableCLIs.length === 0) {
    console.error('没有可用的 CLI');
    process.exit(1);
  }

  const testCLI = 'claude';
  if (!availability[testCLI]) {
    console.error(`${testCLI} 不可用`);
    process.exit(1);
  }

  // 创建并连接适配器
  console.log(`\n--- 创建 ${testCLI} 适配器 ---`);
  const adapter = factory.create(testCLI);

  // 监听适配器事件
  adapter.on('output', (chunk) => {
    counts.adapterOutput++;
    const preview = chunk.slice(0, 80).replace(/\n/g, '\\n');
    addLog('Adapter', `output: "${preview}"`);
  });

  adapter.on('response', (response) => {
    const contentPreview = (response.content || '').slice(0, 80);
    addLog('Adapter', `response: content="${contentPreview}", error=${response.error}`);
  });

  adapter.on('stateChange', (state) => {
    addLog('Adapter', `stateChange: ${state}`);
  });

  console.log('\n--- 连接适配器 ---');
  await adapter.connect();
  console.log('适配器已连接');

  console.log('\n--- 发送消息 ---');
  const start = Date.now();

  try {
    const response = await adapter.sendMessage(prompt);
    const duration = Date.now() - start;

    console.log('\n--- 响应结果 ---');
    console.log(`耗时: ${duration}ms`);
    console.log(`响应长度: ${(response.content || '').length}`);
    console.log(`响应预览: ${(response.content || '').slice(0, 200)}`);

  } catch (err) {
    console.error('发送失败:', err.message);
  }

  // 等待异步事件
  await new Promise(r => setTimeout(r, 500));

  console.log('\n=== 消息流统计 ===');
  console.log(`SessionManager output 事件: ${counts.sessionManagerOutput}`);
  console.log(`Adapter output 事件: ${counts.adapterOutput}`);
  console.log(`StandardMessage 事件: ${counts.standardMessage}`);
  console.log(`StandardComplete 事件: ${counts.standardComplete}`);

  console.log('\n=== 诊断结论 ===');
  if (counts.sessionManagerOutput === 0) {
    console.log('❌ SessionManager 未收到 output 事件');
    console.log('   -> InteractiveSession 可能未正确发射 output 事件');
  } else if (counts.adapterOutput === 0) {
    console.log('❌ Adapter 未收到 output 事件');
    console.log('   -> PersistentSessionAdapter 事件转发可能有问题');
  } else if (counts.standardMessage === 0) {
    console.log('❌ CLIAdapterFactory 未发射 standardMessage');
    console.log('   -> Normalizer 处理可能有问题');
  } else {
    console.log('✅ 消息流完整');
  }

  // 清理
  await factory.disconnectAll().catch(() => {});
  process.exit(0);
}

run().catch(err => {
  console.error('诊断失败:', err);
  process.exit(1);
});
