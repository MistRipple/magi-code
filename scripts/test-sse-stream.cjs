const http = require('http');

const BASE_URL = 'http://127.0.0.1:46231';
const WORKSPACE_ID = 'L1VzZXJzL3hpZS9jb2RlL21hZ2k';
const WORKSPACE_PATH = __dirname;
const SESSION_ID = 'test-session';

function postBoundJson(pathname, payload) {
  return new Promise((resolve, reject) => {
    const data = JSON.stringify({
      workspaceId: WORKSPACE_ID,
      workspacePath: WORKSPACE_PATH,
      sessionId: SESSION_ID,
      ...payload
    });
    
    const req = http.request(BASE_URL + pathname, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(data)
      }
    }, (res) => {
      let body = '';
      res.on('data', chunk => body += chunk);
      res.on('end', () => {
        if (res.statusCode >= 200 && res.statusCode < 300) {
          try { resolve(JSON.parse(body)); } catch(e) { resolve(body); }
        } else {
          reject(new Error(`API Error ${res.statusCode}: ${body}`));
        }
      });
    });
    
    req.on('error', reject);
    req.write(data);
    req.end();
  });
}

function listenSSE() {
  const url = `${BASE_URL}/api/events?workspaceId=${WORKSPACE_ID}&sessionId=${SESSION_ID}&workspacePath=${WORKSPACE_PATH}`;
  let buffer = '';
  
  return new Promise((resolve, reject) => {
    http.get(url, (res) => {
      if (res.statusCode !== 200) {
        return reject(new Error('SSE failed ' + res.statusCode));
      }
      
      console.log('[SSE] 已连接到事件流');
      let start_time = Date.now();
      let last_chunk_time = start_time;
      let chunks_received = 0;
      
      res.on('data', (chunk) => {
        buffer += chunk.toString();
        
        let lines = buffer.split('\n');
        buffer = lines.pop(); // 最后一行可能不完整
        
        for (let line of lines) {
          if (line.startsWith('data: ')) {
            const dataStr = line.slice(6);
            if (dataStr === '[DONE]') continue;
            
            try {
              const event = JSON.parse(dataStr);
              const now = Date.now();
              const delay = now - last_chunk_time;
              last_chunk_time = now;
              
              // 过滤掉太频繁的细碎消息，只在关键事件或者每 10 个 chunk 打印一次
              if (event.type === 'WorkerChunk') {
                chunks_received++;
                if (chunks_received % 10 === 0) {
                  // 打印打字机延迟分析
                  console.log(`[SSE:WorkerChunk] +${delay}ms -> 内容追加: ${JSON.stringify(event.payload.chunk)}`);
                }
              } else if (event.type === 'ActionChunk' || event.type === 'ActionStarted' || event.type === 'ActionFinished') {
                console.log(`[SSE:${event.type}] +${delay}ms`);
              } else if (event.type === 'StatusChanged' || event.type === 'InteractionRequested') {
                console.log(`[SSE:State] \x1b[36m${event.type}\x1b[0m -> ${JSON.stringify(event.payload)}`);
              } else if (event.type === 'SyncExecutionStats' || event.type === 'TaskStatusUpdated' || event.type === 'MissionCreated' || event.type === 'MissionUpdated') {
                // 忽略一些不影响观感的后台同步事件
              } else {
                 console.log(`[SSE:${event.type}] +${delay}ms`);
              }
            } catch (e) {
              console.error('SSE JSON 解析错误:', dataStr);
            }
          }
        }
      });
      
      res.on('end', () => resolve());
    }).on('error', reject);
  });
}

async function runTest(mode, prompt) {
  console.log(`\n===========================================`);
  console.log(`  开始测试模式: ${mode}`);
  console.log(`  Prompt: ${prompt}`);
  console.log(`===========================================`);
  
  // 1. 设置交互模式
  await postBoundJson('/api/interaction-mode', { mode: mode === 'deep' ? 'auto' : mode });
  
  // 2. 设置 DeepTask 状态
  await postBoundJson('/api/settings/update', { key: 'deepTask', value: mode === 'deep' });
  
  // 3. 启动任务
  const taskResult = await postBoundJson('/api/task/execute', { prompt });
  console.log(`[API] 任务已下发:`, taskResult);
  
  // 4. 等待 30 秒看看输出
  await new Promise(r => setTimeout(r, 20000));
}

async function main() {
  // 0. 注册 Workspace
  await new Promise((resolve, reject) => {
    const data = JSON.stringify({ rootPath: WORKSPACE_PATH, name: 'TEST' });
    const req = http.request(BASE_URL + '/api/workspaces/register', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(data) }
    }, (res) => resolve());
    req.on('error', reject);
    req.write(data);
    req.end();
  });

  listenSSE();
  
  await new Promise(r => setTimeout(r, 1000));
  
  // 测试一：Ask 模式，观察是否在得到结果前抛出 InteractionRequested 阻止流
  await runTest('ask', '列出 1 到 5，然后停止');
  
  // 测试二：Auto 模式，观察打字机流是否顺滑且直接出结果
  await runTest('auto', '列出 A 到 E，不问我，直接输出完毕。');
  
  // 测试三：Deep 模式，观察多任务和推理输出的流表现
  await runTest('deep', '分析当前系统的架构，列出优缺点。计划分两步，先列优点，再列缺点。');
  
  process.exit(0);
}

main().catch(console.error);

