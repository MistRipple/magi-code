
import { spawn } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';

const scriptsDir = __dirname;

const tests = [
  { name: 'ux', file: 'test-verify-ux.ts', desc: '验证 UX/UI 数据契约 (MessageHub)' },
  { name: 'scenario', file: 'test-verify-scenario.ts', desc: '验证多 Worker 编排场景 (Mock LLM)' },
  { name: 'skills', file: 'test-verify-skills.js', desc: '验证技能集成 (Skill Integration)' },
  { name: 'mcp', file: 'test-verify-mcp.ts', desc: '验证 MCP 连接 (Mock Connection)' },
];

function printUsage() {
  console.log('MultiCLI 验证脚本工具');
  console.log('用法: npx ts-node scripts/run-verify.ts [test-name | all]\n');
  console.log('可用测试:');
  tests.forEach(t => {
    console.log(`  ${t.name.padEnd(10)} - ${t.desc}`);
  });
}

async function runTest(testName: string) {
  const test = tests.find(t => t.name === testName);
  if (!test) {
    console.error(`未知测试: ${testName}`);
    printUsage();
    process.exit(1);
  }

  console.log(`\n🚀 正在运行测试: ${test.name} (${test.desc})...`);
  const filePath = path.join(scriptsDir, test.file);
  
  // 使用 ts-node -T (transpile only) 运行，跳过类型检查以加快速度
  const cmd = 'npx';
  const args = ['ts-node', '-T', filePath];

  return new Promise<void>((resolve, reject) => {
    const p = spawn(cmd, args, { stdio: 'inherit', shell: true });
    p.on('close', (code) => {
      if (code === 0) {
        console.log(`✅ 测试 ${test.name} 通过`);
        resolve();
      } else {
        console.error(`❌ 测试 ${test.name} 失败 (Exit Code: ${code})`);
        reject(new Error(`Test ${test.name} failed`));
      }
    });
  });
}

async function main() {
  const target = process.argv[2];

  if (!target) {
    printUsage();
    process.exit(0);
  }

  if (target === 'all') {
    console.log('📦 运行所有验证测试...');
    for (const t of tests) {
      try {
        await runTest(t.name);
      } catch (e) {
        console.error('⚠️ 遇到错误，继续执行剩余测试...');
      }
    }
  } else {
    await runTest(target);
  }
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
