#!/usr/bin/env node
/**
 * Codex 配置自动修复脚本
 * 
 * 使用方法：
 *   node fix-codex-config.js [model-name]
 * 
 * 示例：
 *   node fix-codex-config.js gpt-4-turbo-preview
 *   node fix-codex-config.js gpt-3.5-turbo
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

const configPath = path.join(os.homedir(), '.multicli', 'llm.json');

// 标准 OpenAI 模型列表
const STANDARD_MODELS = [
  'gpt-4-turbo-preview',
  'gpt-4-turbo',
  'gpt-4',
  'gpt-4-0125-preview',
  'gpt-4-1106-preview',
  'gpt-3.5-turbo',
  'gpt-3.5-turbo-16k',
];

console.log('='.repeat(80));
console.log('Codex 配置自动修复脚本');
console.log('='.repeat(80));
console.log();

// 读取当前配置
if (!fs.existsSync(configPath)) {
  console.error('❌ 配置文件不存在:', configPath);
  process.exit(1);
}

const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));

console.log('📋 当前 Codex 配置：');
console.log(JSON.stringify(config.workers.codex, null, 2));
console.log();

// 获取命令行参数
const newModel = process.argv[2];

if (!newModel) {
  console.log('💡 可用的修复选项：');
  console.log();
  console.log('1. 使用 GPT-4 Turbo (推荐):');
  console.log('   node fix-codex-config.js gpt-4-turbo-preview');
  console.log();
  console.log('2. 使用 GPT-3.5 Turbo (更快、更便宜):');
  console.log('   node fix-codex-config.js gpt-3.5-turbo');
  console.log();
  console.log('3. 使用标准 GPT-4:');
  console.log('   node fix-codex-config.js gpt-4');
  console.log();
  console.log('4. 使用自定义模型名:');
  console.log('   node fix-codex-config.js <your-model-name>');
  console.log();
  console.log('标准 OpenAI 模型列表：');
  STANDARD_MODELS.forEach(model => console.log(`   - ${model}`));
  console.log();
  process.exit(0);
}

// 检查是否是标准模型
if (!STANDARD_MODELS.includes(newModel)) {
  console.log(`⚠️  警告: "${newModel}" 不是标准的 OpenAI 模型名`);
  console.log('   如果你使用的是第三方 API 代理，请确认该模型名是否正确');
  console.log();
  
  // 询问是否继续
  const readline = require('readline').createInterface({
    input: process.stdin,
    output: process.stdout
  });
  
  readline.question('是否继续修改配置？(y/n): ', (answer) => {
    readline.close();
    if (answer.toLowerCase() !== 'y') {
      console.log('已取消');
      process.exit(0);
    }
    applyFix();
  });
} else {
  applyFix();
}

function applyFix() {
  console.log('🔧 正在修复配置...');
  console.log();
  
  // 备份原配置
  const backupPath = configPath + '.backup.' + Date.now();
  fs.writeFileSync(backupPath, JSON.stringify(config, null, 2));
  console.log(`✅ 已备份原配置到: ${backupPath}`);
  
  // 修复 codex 配置
  const oldModel = config.workers.codex.model;
  config.workers.codex.model = newModel;
  
  console.log(`✅ 已将 codex 模型从 "${oldModel}" 改为 "${newModel}"`);
  
  // 检查并修复 gemini 配置
  if (config.workers.gemini.provider === 'anthropic' && 
      config.workers.gemini.model.startsWith('gemini')) {
    console.log();
    console.log('⚠️  发现 gemini 配置问题：');
    console.log(`   当前: provider="anthropic", model="${config.workers.gemini.model}"`);
    console.log('   修复: 将 provider 改为 "openai"');
    
    config.workers.gemini.provider = 'openai';
    console.log('✅ 已修复 gemini 配置');
  }
  
  // 保存配置
  fs.writeFileSync(configPath, JSON.stringify(config, null, 2));
  console.log();
  console.log('✅ 配置已保存');
  console.log();
  
  console.log('📋 新的 Codex 配置：');
  console.log(JSON.stringify(config.workers.codex, null, 2));
  console.log();
  
  console.log('='.repeat(80));
  console.log('✅ 修复完成！');
  console.log('='.repeat(80));
  console.log();
  console.log('💡 下一步：');
  console.log('   1. 重启 VS Code 或重新加载 MultiCLI 扩展');
  console.log('   2. 测试 codex worker 是否能正常工作');
  console.log('   3. 如果还有问题，请检查 API Key 和网络连接');
  console.log();
  console.log('如需恢复原配置：');
  console.log(`   cp ${backupPath} ${configPath}`);
  console.log();
}

