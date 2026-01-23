/**
 * Codex 配置诊断工具
 * 用于测试 codex worker 的连接问题
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

// 加载配置
const configPath = path.join(os.homedir(), '.multicli', 'llm.json');
const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));

console.log('='.repeat(80));
console.log('Codex 配置诊断工具');
console.log('='.repeat(80));
console.log();

// 显示当前配置
console.log('📋 当前 Codex 配置：');
console.log(JSON.stringify(config.workers.codex, null, 2));
console.log();

// 检查配置问题
console.log('🔍 配置检查：');
const issues = [];

// 1. 检查 provider
if (config.workers.codex.provider !== 'openai') {
  issues.push(`❌ Provider 错误: "${config.workers.codex.provider}" (应该是 "openai")`);
} else {
  console.log('✅ Provider: openai');
}

// 2. 检查 baseUrl
const baseUrl = config.workers.codex.baseUrl;
console.log(`✅ Base URL: ${baseUrl}`);
if (!baseUrl.endsWith('/v1')) {
  console.log(`   ℹ️  注意: URL 会自动添加 /v1 后缀 -> ${baseUrl}/v1`);
}

// 3. 检查 apiKey
if (!config.workers.codex.apiKey) {
  issues.push('❌ API Key 缺失');
} else {
  console.log(`✅ API Key: ${config.workers.codex.apiKey.substring(0, 10)}...`);
}

// 4. 检查模型名称
const model = config.workers.codex.model;
console.log(`⚠️  Model: ${model}`);

// 检查是否是标准的 OpenAI 模型名
const standardModels = [
  'gpt-4',
  'gpt-4-turbo',
  'gpt-4-turbo-preview',
  'gpt-4-0125-preview',
  'gpt-4-1106-preview',
  'gpt-3.5-turbo',
  'gpt-3.5-turbo-16k',
];

if (!standardModels.includes(model) && !model.startsWith('gpt-')) {
  issues.push(`⚠️  模型名称 "${model}" 不是标准的 OpenAI 模型名`);
  console.log('   ℹ️  这可能是第三方 API 代理的自定义模型名');
  console.log('   ℹ️  请确认该模型在 API 代理上可用');
}

console.log();

// 5. 检查 enabled 状态
if (!config.workers.codex.enabled) {
  issues.push('❌ Codex 已禁用');
} else {
  console.log('✅ Codex 已启用');
}

console.log();

// 显示问题总结
if (issues.length > 0) {
  console.log('⚠️  发现以下问题：');
  issues.forEach(issue => console.log(`   ${issue}`));
  console.log();
}

// 测试 API 连接
console.log('🔌 测试 API 连接...');
console.log();

async function testConnection() {
  try {
    const OpenAI = require('openai');
    
    // 构建正确的 baseURL
    let baseURL = baseUrl;
    if (!baseURL.endsWith('/v1')) {
      baseURL = baseURL.replace(/\/$/, '') + '/v1';
    }
    
    console.log(`   连接到: ${baseURL}`);
    console.log(`   使用模型: ${model}`);
    console.log();
    
    const client = new OpenAI({
      apiKey: config.workers.codex.apiKey,
      baseURL: baseURL,
    });
    
    console.log('   发送测试请求...');
    const response = await client.chat.completions.create({
      model: model,
      messages: [{ role: 'user', content: 'Hello, this is a test message. Please respond with "OK".' }],
      max_tokens: 10,
    });
    
    console.log();
    console.log('✅ 连接成功！');
    console.log();
    console.log('📨 API 响应：');
    console.log(`   Choices 数量: ${response.choices?.length || 0}`);
    
    if (response.choices && response.choices.length > 0) {
      console.log(`   响应内容: ${response.choices[0].message.content}`);
      console.log(`   Finish Reason: ${response.choices[0].finish_reason}`);
      console.log(`   Token 使用: ${response.usage?.total_tokens || 0} tokens`);
    } else {
      console.log('   ❌ 响应中没有 choices 数组！');
      console.log('   完整响应:', JSON.stringify(response, null, 2));
    }
    
  } catch (error) {
    console.log();
    console.log('❌ 连接失败！');
    console.log();
    console.log('错误信息:');
    console.log(`   ${error.message}`);
    console.log();
    
    if (error.response) {
      console.log('API 响应:');
      console.log(`   状态码: ${error.response.status}`);
      console.log(`   状态文本: ${error.response.statusText}`);
      if (error.response.data) {
        console.log(`   错误详情: ${JSON.stringify(error.response.data, null, 2)}`);
      }
    }
    
    console.log();
    console.log('💡 可能的原因：');
    console.log('   1. 模型名称不正确或在该 API 上不可用');
    console.log('   2. API Key 无效或已过期');
    console.log('   3. API 代理服务不可用');
    console.log('   4. 网络连接问题');
  }
}

testConnection().then(() => {
  console.log();
  console.log('='.repeat(80));
  console.log('诊断完成');
  console.log('='.repeat(80));
  console.log();
  
  // 提供修复建议
  if (issues.length > 0 || model === 'gpt-5.2-codex') {
    console.log('💡 修复建议：');
    console.log();
    console.log('如果使用第三方 API 代理，请确认：');
    console.log('   1. 模型名称是否正确（联系 API 提供商获取支持的模型列表）');
    console.log('   2. API Key 是否有权限访问该模型');
    console.log();
    console.log('如果使用标准 OpenAI API，建议使用以下模型名：');
    console.log('   - gpt-4-turbo-preview (最新 GPT-4)');
    console.log('   - gpt-4 (标准 GPT-4)');
    console.log('   - gpt-3.5-turbo (GPT-3.5)');
    console.log();
    console.log('修改配置：');
    console.log(`   编辑文件: ${configPath}`);
    console.log('   或在 MultiCLI UI 的"画像"标签页中修改');
    console.log();
  }
}).catch(err => {
  console.error('诊断脚本执行失败:', err);
  process.exit(1);
});

